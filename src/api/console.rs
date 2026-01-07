//! WebSocket-backed SSH console (PTY) for the dashboard.
//!
//! Features session pooling to allow fast reconnection - sessions are kept alive
//! for a configurable timeout after disconnect, allowing seamless reconnection
//! without re-establishing SSH connections.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex, RwLock};

use super::auth;
use super::routes::AppState;
use super::ssh_util::materialize_private_key;

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

/// A pooled SSH session that can be reused across WebSocket reconnections.
struct PooledSession {
    /// Channel to send input/resize commands to the PTY.
    to_pty_tx: mpsc::UnboundedSender<ClientMsg>,
    /// Channel to receive output from the PTY.
    from_pty_rx: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
    /// When this session was last disconnected (None if currently in use).
    disconnected_at: Option<Instant>,
    /// Whether this session is currently in use by a WebSocket connection.
    in_use: bool,
    /// Handle to kill the child process on cleanup.
    child_killer: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
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
                    if !s.in_use {
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
    // Client sends: ["openagent", "jwt.<token>"]
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

    // Select a stable subprotocol if client offered it.
    ws.protocols(["openagent"])
        .on_upgrade(move |socket| handle_console(socket, state, session_key))
}

async fn handle_console(socket: WebSocket, state: Arc<AppState>, session_key: String) {
    // Try to reuse an existing session from the pool
    let existing_session = {
        let sessions = state.console_pool.sessions.read().await;
        sessions.get(&session_key).cloned()
    };

    if let Some(session) = existing_session {
        let mut s = session.lock().await;
        if !s.in_use && s.to_pty_tx.is_closed() == false {
            // Reuse this session
            s.in_use = true;
            s.disconnected_at = None;
            tracing::debug!("Reusing pooled console session: {}", session_key);
            drop(s);
            handle_existing_session(socket, session, state, session_key).await;
            return;
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
    let (to_pty_tx, from_pty_rx) = {
        let s = session.lock().await;
        (s.to_pty_tx.clone(), s.from_pty_rx.clone())
    };

    // Pump PTY output to WS
    let send_task = {
        let from_pty_rx = from_pty_rx.clone();
        tokio::spawn(async move {
            loop {
                let chunk = {
                    let mut rx = from_pty_rx.lock().await;
                    rx.recv().await
                };
                match chunk {
                    Some(data) => {
                        if ws_sender.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
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
        s.in_use = false;
        s.disconnected_at = Some(Instant::now());
    }
    tracing::debug!("Console session returned to pool: {}", session_key);
}

async fn handle_new_session(mut socket: WebSocket, state: Arc<AppState>, session_key: String) {
    let cfg = state.config.console_ssh.clone();
    let key = match cfg.private_key.as_deref() {
        Some(k) if !k.trim().is_empty() => k,
        _ => {
            let _ = socket
                .send(Message::Text(
                    "Console SSH is not configured on the server.".into(),
                ))
                .await;
            let _ = socket.close().await;
            return;
        }
    };

    let key_file = match materialize_private_key(key).await {
        Ok(k) => k,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to load SSH key: {}", e)))
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

    let mut cmd = CommandBuilder::new("ssh");
    cmd.arg("-i");
    cmd.arg(key_file.path());
    cmd.arg("-p");
    cmd.arg(cfg.port.to_string());
    cmd.arg("-o");
    cmd.arg("BatchMode=yes");
    cmd.arg("-o");
    cmd.arg("StrictHostKeyChecking=accept-new");
    cmd.arg("-o");
    cmd.arg(format!(
        "UserKnownHostsFile={}",
        std::env::temp_dir()
            .join("open_agent_known_hosts")
            .to_string_lossy()
    ));
    // Allocate PTY on the remote side too.
    cmd.arg("-tt");
    cmd.arg(format!("{}@{}", cfg.user, cfg.host));
    cmd.env("TERM", "xterm-256color");

    let child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("Failed to spawn ssh: {}", e)))
                .await;
            let _ = socket.close().await;
            return;
        }
    };
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => {
            let _ = socket.close().await;
            return;
        }
    };

    let (to_pty_tx, mut to_pty_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (from_pty_tx, from_pty_rx) = mpsc::unbounded_channel::<String>();

    // Writer/resizer thread.
    let master_for_writer = pair.master;
    let mut writer = match master_for_writer.take_writer() {
        Ok(w) => w,
        Err(_) => {
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
    let reader_task = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).to_string();
                    if from_pty_tx.send(s).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Create the pooled session
    let from_pty_rx = Arc::new(Mutex::new(from_pty_rx));
    let session = Arc::new(Mutex::new(PooledSession {
        to_pty_tx: to_pty_tx.clone(),
        from_pty_rx: from_pty_rx.clone(),
        disconnected_at: None,
        in_use: true,
        child_killer: child_killer.clone(),
    }));

    // Store in pool
    {
        let mut sessions = state.console_pool.sessions.write().await;
        // Remove any old session with the same key
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
        let from_pty_rx = from_pty_rx.clone();
        tokio::spawn(async move {
            loop {
                let chunk = {
                    let mut rx = from_pty_rx.lock().await;
                    rx.recv().await
                };
                match chunk {
                    Some(data) => {
                        if ws_sender.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
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
        s.in_use = false;
        s.disconnected_at = Some(Instant::now());
    }

    tracing::debug!("Console session returned to pool: {}", session_key);

    // Note: We don't kill the child or clean up tasks here anymore.
    // The cleanup task will handle expired sessions.
    // Writer and reader tasks will continue running in the background.
    let _ = writer_task;
    let _ = reader_task;
}
