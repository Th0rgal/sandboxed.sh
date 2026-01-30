//! WebSocket-backed console (PTY) for the dashboard.
//!
//! Features session pooling to allow fast reconnection - sessions are kept alive
//! for a configurable timeout after disconnect.
//!
//! Also provides workspace shell support - PTY sessions that run directly in
//! workspace directories (using systemd-nspawn for isolated workspaces).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, path::PathBuf};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use uuid::Uuid;

use super::auth;
use super::routes::AppState;
use crate::nspawn;
use crate::workspace::{use_nspawn_for_workspace, WorkspaceType};

/// How long to keep a session alive after disconnect before cleanup.
const SESSION_POOL_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to run the cleanup task.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientMsg {
    #[serde(rename = "i")]
    Input { d: String },
    #[serde(rename = "r")]
    Resize { c: u16, r: u16 },
}

/// A pooled console session that can be reused across WebSocket reconnections.
struct PooledSession {
    /// Channel to send input/resize commands to the PTY.
    to_pty_tx: mpsc::UnboundedSender<ClientMsg>,
    /// When this session was last disconnected (None if currently in use).
    disconnected_at: Option<Instant>,
    /// Active WebSocket connections attached to this session.
    connection_count: usize,
    /// Handle to kill the child process on cleanup.
    child_killer: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    /// Broadcast channel for PTY output (fan-out to all websocket clients).
    from_pty_tx: broadcast::Sender<String>,
}

/// Global session pool, keyed by a session identifier.
/// For simplicity, we use a single global session per authenticated user.
pub struct SessionPool {
    sessions: RwLock<HashMap<String, Arc<Mutex<PooledSession>>>>,
}

impl SessionPool {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Start the background cleanup task.
    pub fn start_cleanup_task(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(CLEANUP_INTERVAL).await;
                self.cleanup_expired_sessions().await;
            }
        });
    }

    async fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let now = Instant::now();

        let expired: Vec<String> = sessions
            .iter()
            .filter_map(|(key, session)| {
                // Try to lock without blocking
                if let Ok(s) = session.try_lock() {
                    if s.connection_count == 0 {
                        if let Some(disconnected_at) = s.disconnected_at {
                            if now.duration_since(disconnected_at) > SESSION_POOL_TIMEOUT {
                                return Some(key.clone());
                            }
                        }
                    }
                }
                None
            })
            .collect();

        for key in expired {
            if let Some(session) = sessions.remove(&key) {
                // Kill the session
                if let Ok(s) = session.try_lock() {
                    if let Ok(mut child_guard) = s.child_killer.try_lock() {
                        if let Some(mut child) = child_guard.take() {
                            let _ = child.kill();
                        }
                    }
                }
                tracing::debug!("Cleaned up expired console session: {}", key);
            }
        }
    }
}

impl Default for SessionPool {
    fn default() -> Self {
        Self::new()
    }
}

fn extract_jwt_from_protocols(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;
    // Client sends: ["sandboxed", "jwt.<token>"]
    for part in raw.split(',').map(|s| s.trim()) {
        if let Some(rest) = part.strip_prefix("jwt.") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

pub async fn console_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Enforce auth in non-dev mode by taking JWT from Sec-WebSocket-Protocol.
    let session_key = if state.config.auth.auth_required(state.config.dev_mode) {
        let token = match extract_jwt_from_protocols(&headers) {
            Some(t) => t,
            None => return (StatusCode::UNAUTHORIZED, "Missing websocket JWT").into_response(),
        };
        if !auth::verify_token_for_config(&token, &state.config) {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
        }
        // Use token hash as session key for authenticated users
        format!("auth:{:x}", md5::compute(&token))
    } else {
        // In dev mode, use a simple key
        "dev:default".to_string()
    };

    tracing::info!(session_key = %session_key, "Console websocket upgrade requested");
    // Select a stable subprotocol if client offered it.
    ws.protocols(["sandboxed"])
        .on_upgrade(move |socket| handle_console(socket, state, session_key))
}

async fn handle_console(socket: WebSocket, state: Arc<AppState>, session_key: String) {
    tracing::info!(session_key = %session_key, "Console websocket connected");
    // Try to reuse an existing session from the pool
    let existing_session = {
        let sessions = state.console_pool.sessions.read().await;
        sessions.get(&session_key).cloned()
    };

    if let Some(session) = existing_session {
        let (can_reuse, child_killer) = {
            let s = session.lock().await;
            (!s.to_pty_tx.is_closed(), s.child_killer.clone())
        };

        if can_reuse {
            if child_has_exited(&child_killer).await {
                let mut sessions = state.console_pool.sessions.write().await;
                sessions.remove(&session_key);
            } else {
                tracing::debug!("Reusing pooled console session: {}", session_key);
                handle_existing_session(socket, session, state, session_key).await;
                return;
            }
        }
    }

    // No reusable session, create a new one
    tracing::debug!("Creating new console session: {}", session_key);
    handle_new_session(socket, state, session_key).await;
}

async fn handle_existing_session(
    socket: WebSocket,
    session: Arc<Mutex<PooledSession>>,
    _state: Arc<AppState>,
    session_key: String,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Get channels from the session
    let (to_pty_tx, from_pty_tx) = {
        let s = session.lock().await;
        (s.to_pty_tx.clone(), s.from_pty_tx.clone())
    };

    {
        let mut s = session.lock().await;
        s.connection_count += 1;
        s.disconnected_at = None;
    }

    // Pump PTY output to WS
    let send_task = {
        let mut from_pty_rx = from_pty_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match from_pty_rx.recv().await {
                    Ok(data) => {
                        if ws_sender.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    // WS -> PTY
    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(t) => {
                if let Ok(parsed) = serde_json::from_str::<ClientMsg>(&t) {
                    let _ = to_pty_tx.send(parsed);
                }
            }
            Message::Binary(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();

    // Mark session as disconnected but keep it in the pool
    {
        let mut s = session.lock().await;
        if s.connection_count > 0 {
            s.connection_count -= 1;
        }
        if s.connection_count == 0 {
            s.disconnected_at = Some(Instant::now());
        }
    }
    tracing::info!(session_key = %session_key, "Console websocket disconnected (pooled session)");
    tracing::debug!("Console session returned to pool: {}", session_key);
}

async fn handle_new_session(mut socket: WebSocket, state: Arc<AppState>, session_key: String) {
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to open PTY: {}", e)))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    tracing::info!(
        "Spawning console shell (working_dir={})",
        state.config.working_dir.to_string_lossy()
    );
    let bash_path = std::path::Path::new("/bin/bash");
    let mut cmd = if bash_path.exists() {
        let mut cmd = CommandBuilder::new("/bin/bash");
        cmd.arg("--login");
        cmd.arg("-i");
        cmd
    } else {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-i");
        cmd
    };
    cmd.cwd(&state.config.working_dir);
    cmd.env("TERM", "xterm-256color");

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to spawn shell: {}", e)))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    if let Ok(Some(status)) = child.try_wait() {
        tracing::warn!("Console session exited immediately: {:?}", status);
        let _ = socket
            .send(Message::Text(format!(
                "Console session exited immediately: {:?}. Check shell availability and permissions.",
                status
            )))
            .await;
        let _ = socket.close().await;
        return;
    }
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = socket.close().await;
            return;
        }
    };

    let (to_pty_tx, mut to_pty_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (from_pty_tx, _from_pty_rx) = broadcast::channel::<String>(1024);

    // Writer/resizer thread.
    let master_for_writer = pair.master;
    let mut writer = match master_for_writer.take_writer() {
        Ok(w) => w,
        Err(_) => {
            let _ = child.kill();
            let _ = socket.close().await;
            return;
        }
    };

    let child_killer: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>> =
        Arc::new(Mutex::new(Some(child)));

    let writer_task = {
        let master = master_for_writer;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            while let Some(msg) = to_pty_rx.blocking_recv() {
                match msg {
                    ClientMsg::Input { d } => {
                        let _ = writer.write_all(d.as_bytes());
                        let _ = writer.flush();
                    }
                    ClientMsg::Resize { c, r } => {
                        let _ = master.resize(PtySize {
                            rows: r,
                            cols: c,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
        })
    };

    // Reader thread.
    let from_pty_tx_reader = from_pty_tx.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = from_pty_tx_reader.send(s);
                }
                Err(_) => break,
            }
        }
    });

    // Create the pooled session
    let session = Arc::new(Mutex::new(PooledSession {
        to_pty_tx: to_pty_tx.clone(),
        from_pty_tx: from_pty_tx.clone(),
        disconnected_at: None,
        connection_count: 1,
        child_killer: child_killer.clone(),
    }));

    // Store in pool
    {
        let mut sessions = state.console_pool.sessions.write().await;
        // Check if there's an existing session with the same key that is currently active
        let existing_connections = if let Some(old_session) = sessions.get(&session_key) {
            old_session
                .try_lock()
                .map(|s| s.connection_count)
                .unwrap_or(0)
        } else {
            0
        };

        if existing_connections > 0 {
            // Replace the existing active session (rare race when two connects create sessions)
            tracing::warn!(
                "Session {} has {} active connection(s); replacing with new console session",
                session_key,
                existing_connections
            );
        }

        // Remove and kill the old session (if any) before inserting the new one.
        if let Some(old_session) = sessions.remove(&session_key) {
            if let Ok(s) = old_session.try_lock() {
                if let Ok(mut child_guard) = s.child_killer.try_lock() {
                    if let Some(mut child) = child_guard.take() {
                        let _ = child.kill();
                    }
                }
            }
        }
        sessions.insert(session_key.clone(), session.clone());
    }

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Pump PTY output to WS.
    let send_task = {
        let mut from_pty_rx = from_pty_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match from_pty_rx.recv().await {
                    Ok(data) => {
                        if ws_sender.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    // WS -> PTY
    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(t) => {
                if let Ok(parsed) = serde_json::from_str::<ClientMsg>(&t) {
                    let _ = to_pty_tx.send(parsed);
                }
            }
            Message::Binary(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();

    // Mark session as disconnected but keep it in the pool for potential reuse
    {
        let mut s = session.lock().await;
        if s.connection_count > 0 {
            s.connection_count -= 1;
        }
        if s.connection_count == 0 {
            s.disconnected_at = Some(Instant::now());
        }
    }

    tracing::info!(session_key = %session_key, "Console websocket disconnected (new session)");
    tracing::debug!("Console session returned to pool: {}", session_key);

    // Note: We don't kill the child or clean up tasks here anymore.
    // The cleanup task will handle expired sessions.
    // Writer and reader tasks will continue running in the background.
    let _ = writer_task;
    let _ = reader_task;
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Shell WebSocket
// ─────────────────────────────────────────────────────────────────────────────

/// WebSocket endpoint for workspace shell sessions.
/// This spawns a PTY directly in the workspace (using systemd-nspawn for isolated workspaces).
pub async fn workspace_shell_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    AxumPath(workspace_id): AxumPath<Uuid>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Enforce auth in non-dev mode
    let session_key = if state.config.auth.auth_required(state.config.dev_mode) {
        let token = match extract_jwt_from_protocols(&headers) {
            Some(t) => t,
            None => return (StatusCode::UNAUTHORIZED, "Missing websocket JWT").into_response(),
        };
        if !auth::verify_token_for_config(&token, &state.config) {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
        }
        format!("workspace:{}:{:x}", workspace_id, md5::compute(&token))
    } else {
        format!("workspace:{}:dev", workspace_id)
    };

    tracing::info!(
        session_key = %session_key,
        workspace_id = %workspace_id,
        "Workspace shell websocket upgrade requested"
    );
    // Verify workspace exists
    let workspace = match state.workspaces.get(workspace_id).await {
        Some(ws) => ws,
        None => {
            return (
                StatusCode::NOT_FOUND,
                format!("Workspace {} not found", workspace_id),
            )
                .into_response()
        }
    };

    // For container workspaces, verify it's ready
    if workspace.workspace_type == WorkspaceType::Container
        && workspace.status != crate::workspace::WorkspaceStatus::Ready
    {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Workspace {} is not ready (status: {:?})",
                workspace_id, workspace.status
            ),
        )
            .into_response();
    }

    ws.protocols(["sandboxed"])
        .on_upgrade(move |socket| handle_workspace_shell(socket, state, workspace_id, session_key))
}

fn runtime_display_path() -> Option<PathBuf> {
    if let Ok(path) = env::var("SANDBOXED_SH_RUNTIME_DISPLAY_FILE") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    let candidates = [
        env::var("WORKING_DIR").ok(),
        env::var("SANDBOXED_SH_WORKSPACE_ROOT").ok(),
        env::var("HOME").ok(),
    ];

    for base in candidates.into_iter().flatten() {
        let path = PathBuf::from(base)
            .join(".sandboxed-sh")
            .join("runtime")
            .join("current_display.json");
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn read_runtime_display() -> Option<String> {
    if let Ok(display) = env::var("DESKTOP_DISPLAY") {
        if !display.trim().is_empty() {
            return Some(display);
        }
    }

    let path = runtime_display_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    if let Ok(json) = serde_json::from_str::<JsonValue>(&contents) {
        return json
            .get("display")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn child_has_exited(
    child_killer: &Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
) -> bool {
    let mut guard = child_killer.lock().await;
    match guard.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(Some(_status)) => {
                *guard = None;
                true
            }
            Ok(None) => false,
            Err(_) => {
                *guard = None;
                true
            }
        },
        None => true,
    }
}

/// Terminate any existing systemd-nspawn container for the given machine name.
/// This ensures we don't get "Directory tree is currently busy" errors when
/// spawning a new container session.
async fn terminate_stale_container(machine_name: &str) {
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("machinectl")
            .args(["show", machine_name, "--property=State"])
            .output(),
    )
    .await;

    let output = match status {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return,
        Err(_) => {
            tracing::warn!(
                "Timed out while checking machinectl state for '{}'",
                machine_name
            );
            return;
        }
    };

    if !output.status.success() {
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("State=") {
        tracing::info!(
            "Terminating stale container '{}' before spawning new session",
            machine_name
        );
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::process::Command::new("machinectl")
                .args(["terminate", machine_name])
                .output(),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn handle_workspace_shell(
    socket: WebSocket,
    state: Arc<AppState>,
    workspace_id: Uuid,
    session_key: String,
) {
    tracing::info!(
        session_key = %session_key,
        workspace_id = %workspace_id,
        "Workspace shell websocket connected"
    );
    // Try to reuse an existing session from the pool
    let existing_session = {
        let sessions = state.console_pool.sessions.read().await;
        sessions.get(&session_key).cloned()
    };

    if let Some(session) = existing_session {
        let (can_reuse, child_killer) = {
            let s = session.lock().await;
            (!s.to_pty_tx.is_closed(), s.child_killer.clone())
        };

        if can_reuse {
            if child_has_exited(&child_killer).await {
                let mut sessions = state.console_pool.sessions.write().await;
                sessions.remove(&session_key);
            } else {
                tracing::debug!("Reusing pooled workspace shell session: {}", session_key);
                handle_existing_session(socket, session, state, session_key.clone()).await;
                tracing::info!(
                    session_key = %session_key,
                    workspace_id = %workspace_id,
                    "Workspace shell websocket disconnected (pooled session)"
                );
                return;
            }
        }
    }

    tracing::debug!("Creating new workspace shell session: {}", session_key);
    handle_new_workspace_shell(socket, state, workspace_id, session_key).await;
}

async fn handle_new_workspace_shell(
    mut socket: WebSocket,
    state: Arc<AppState>,
    workspace_id: Uuid,
    session_key: String,
) {
    // Get workspace info
    let workspace = match state.workspaces.get(workspace_id).await {
        Some(ws) => ws,
        None => {
            let _ = socket
                .send(Message::Text(format!(
                    "Workspace {} not found",
                    workspace_id
                )))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to open PTY: {}", e)))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    // Build command based on workspace type
    let mut cmd = match workspace.workspace_type {
        WorkspaceType::Container if use_nspawn_for_workspace(&workspace) => {
            // For container workspaces, use systemd-nspawn to enter the isolated environment
            // First, terminate any stale container that might be holding the directory lock
            terminate_stale_container(&workspace.name).await;

            let mut cmd = CommandBuilder::new("systemd-nspawn");
            cmd.arg("-D");
            cmd.arg(workspace.path.to_string_lossy().to_string());
            // Register with a consistent machine name so we can detect/terminate it later
            cmd.arg(format!("--machine={}", workspace.name));
            cmd.arg("--quiet");
            cmd.arg("--timezone=off");
            for arg in nspawn::tailscale_nspawn_extra_args(&workspace.env_vars) {
                cmd.arg(arg);
            }

            if let Some(display) = read_runtime_display() {
                if std::path::Path::new("/tmp/.X11-unix").exists() {
                    cmd.arg("--bind=/tmp/.X11-unix");
                    cmd.arg(format!("--setenv=DISPLAY={}", display));
                }
            }

            cmd.arg("--setenv=TERM=xterm-256color");
            cmd.arg(format!("--setenv=WORKSPACE_ID={}", workspace_id));
            cmd.arg(format!("--setenv=WORKSPACE_NAME={}", workspace.name));
            for (key, value) in &workspace.env_vars {
                if key.trim().is_empty() {
                    continue;
                }
                cmd.arg(format!("--setenv={}={}", key, value));
            }

            // Try to use bash if available, fallback to sh
            let bash_path = workspace.path.join("bin/bash");
            if bash_path.exists() {
                cmd.arg("/bin/bash");
                cmd.arg("--login");
                cmd.arg("-i");
            } else {
                cmd.arg("/bin/sh");
                cmd.arg("-i");
            }
            cmd
        }
        _ => {
            // For host workspaces, just spawn a shell in the workspace directory
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let mut cmd = CommandBuilder::new(&shell);
            cmd.arg("--login");
            cmd.cwd(&workspace.path);
            cmd
        }
    };

    cmd.env("TERM", "xterm-256color");
    cmd.env("WORKSPACE_ID", workspace_id.to_string());
    cmd.env("WORKSPACE_NAME", &workspace.name);
    for (key, value) in &workspace.env_vars {
        if key.trim().is_empty() {
            continue;
        }
        cmd.env(key, value);
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to spawn shell: {}", e)))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    if let Ok(Some(status)) = child.try_wait() {
        tracing::warn!("Workspace shell exited immediately: {:?}", status);
        let _ = socket
            .send(Message::Text(format!(
                "Workspace shell exited immediately: {:?}",
                status
            )))
            .await;
        let _ = socket.close().await;
        return;
    }
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = socket.close().await;
            return;
        }
    };

    let (to_pty_tx, mut to_pty_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (from_pty_tx, _from_pty_rx) = broadcast::channel::<String>(1024);

    let master_for_writer = pair.master;
    let mut writer = match master_for_writer.take_writer() {
        Ok(w) => w,
        Err(_) => {
            let _ = child.kill();
            let _ = socket.close().await;
            return;
        }
    };

    let child_killer: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>> =
        Arc::new(Mutex::new(Some(child)));

    let writer_task = {
        let master = master_for_writer;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            while let Some(msg) = to_pty_rx.blocking_recv() {
                match msg {
                    ClientMsg::Input { d } => {
                        let _ = writer.write_all(d.as_bytes());
                        let _ = writer.flush();
                    }
                    ClientMsg::Resize { c, r } => {
                        let _ = master.resize(PtySize {
                            rows: r,
                            cols: c,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
        })
    };

    let from_pty_tx_reader = from_pty_tx.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = from_pty_tx_reader.send(s);
                }
                Err(_) => break,
            }
        }
    });

    // Create pooled session
    let session = Arc::new(Mutex::new(PooledSession {
        to_pty_tx: to_pty_tx.clone(),
        from_pty_tx: from_pty_tx.clone(),
        disconnected_at: None,
        connection_count: 1,
        child_killer: child_killer.clone(),
    }));

    // Store in pool
    {
        let mut sessions = state.console_pool.sessions.write().await;
        let existing_connections = if let Some(old_session) = sessions.get(&session_key) {
            old_session
                .try_lock()
                .map(|s| s.connection_count)
                .unwrap_or(0)
        } else {
            0
        };

        if existing_connections > 0 {
            tracing::warn!(
                "Session {} has {} active connection(s); replacing with new workspace shell session",
                session_key,
                existing_connections
            );
        }

        if let Some(old_session) = sessions.remove(&session_key) {
            if let Ok(s) = old_session.try_lock() {
                if let Ok(mut child_guard) = s.child_killer.try_lock() {
                    if let Some(mut child) = child_guard.take() {
                        let _ = child.kill();
                    }
                }
            }
        }
        sessions.insert(session_key.clone(), session.clone());
    }

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Pump PTY output to WS
    let send_task = {
        let mut from_pty_rx = from_pty_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match from_pty_rx.recv().await {
                    Ok(data) => {
                        if ws_sender.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    // WS -> PTY
    while let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Text(t) => {
                if let Ok(parsed) = serde_json::from_str::<ClientMsg>(&t) {
                    let _ = to_pty_tx.send(parsed);
                }
            }
            Message::Binary(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();

    // Mark session as disconnected but keep in pool
    {
        let mut s = session.lock().await;
        if s.connection_count > 0 {
            s.connection_count -= 1;
        }
        if s.connection_count == 0 {
            s.disconnected_at = Some(Instant::now());
        }
    }

    tracing::info!(
        session_key = %session_key,
        workspace_id = %workspace_id,
        "Workspace shell websocket disconnected (new session)"
    );
    tracing::debug!("Workspace shell session returned to pool: {}", session_key);

    let _ = writer_task;
    let _ = reader_task;
}
