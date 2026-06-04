//! WebSocket-based real-time system monitoring.
//!
//! Provides CPU, memory, and network usage metrics streamed
//! to connected clients via WebSocket. Maintains a history buffer
//! so new clients receive recent data immediately.
//!
//! Also streams per-container (systemd-nspawn) CPU and memory metrics
//! for container workspaces, collected from cgroup stats.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sysinfo::{Networks, System};
use tokio::sync::{broadcast, RwLock};

use super::auth;
use super::routes::AppState;
use crate::workspace::{SharedWorkspaceStore, WorkspaceStatus, WorkspaceType};

/// How many historical samples to keep (at 1 sample/sec = 60 seconds of history)
const HISTORY_SIZE: usize = 60;

/// Query parameters for the monitoring stream endpoint
#[derive(Debug, Deserialize)]
pub struct MonitoringParams {
    /// Update interval in milliseconds (default: 1000, min: 500, max: 5000)
    pub interval_ms: Option<u64>,
}

/// System metrics snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    /// CPU usage percentage (0-100)
    pub cpu_percent: f32,
    /// Per-core CPU usage percentages
    pub cpu_cores: Vec<f32>,
    /// Memory used in bytes
    pub memory_used: u64,
    /// Total memory in bytes
    pub memory_total: u64,
    /// Memory usage percentage (0-100)
    pub memory_percent: f32,
    /// Network bytes received per second
    pub network_rx_bytes_per_sec: u64,
    /// Network bytes transmitted per second
    pub network_tx_bytes_per_sec: u64,
    /// Timestamp in milliseconds since epoch
    pub timestamp_ms: u64,
}

/// Per-container metrics snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerMetrics {
    pub workspace_id: String,
    pub workspace_name: String,
    pub cpu_percent: f64,
    pub memory_used: u64,
    pub memory_total: u64,
    pub memory_percent: f64,
}

/// Severity of host memory pressure, used by the dashboard banner and by
/// mission admission control. Thresholds: warn at >=80% RAM (or >=20% swap),
/// critical at >=90% RAM (or >=50% swap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryHealthLevel {
    Ok,
    Warn,
    Critical,
}

/// A single workspace/mission's memory footprint, for "who's the culprit"
/// diagnostics in the warning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConsumer {
    pub workspace_id: String,
    pub workspace_name: String,
    pub memory_used: u64,
    pub memory_percent: f64,
}

/// Host memory-health snapshot: overall level + top consuming workspaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHealth {
    pub level: MemoryHealthLevel,
    pub memory_used: u64,
    pub memory_total: u64,
    pub memory_percent: f32,
    pub swap_used: u64,
    pub swap_total: u64,
    pub swap_percent: f32,
    /// Top memory-consuming container workspaces, largest first (max 5).
    pub top_consumers: Vec<MemoryConsumer>,
    pub timestamp_ms: u64,
}

/// Warn when RAM or swap usage reaches these percentages.
const MEM_WARN_PCT: f32 = 80.0;
const SWAP_WARN_PCT: f32 = 20.0;
/// Escalate to critical at these percentages.
const MEM_CRITICAL_PCT: f32 = 90.0;
const SWAP_CRITICAL_PCT: f32 = 50.0;

impl MemoryHealthLevel {
    fn from_usage(memory_percent: f32, swap_percent: f32) -> Self {
        if memory_percent >= MEM_CRITICAL_PCT || swap_percent >= SWAP_CRITICAL_PCT {
            MemoryHealthLevel::Critical
        } else if memory_percent >= MEM_WARN_PCT || swap_percent >= SWAP_WARN_PCT {
            MemoryHealthLevel::Warn
        } else {
            MemoryHealthLevel::Ok
        }
    }
}

/// Percentage of `used` over `total` (0 when `total` is 0).
fn pct(used: u64, total: u64) -> f32 {
    if total > 0 {
        (used as f64 / total as f64 * 100.0) as f32
    } else {
        0.0
    }
}

/// Container metrics update sent over WebSocket
#[derive(Debug, Clone, Serialize)]
struct ContainerMetricsMessage {
    #[serde(rename = "type")]
    msg_type: &'static str,
    containers: Vec<ContainerMetrics>,
}

/// Initial snapshot message sent to new clients
#[derive(Debug, Clone, Serialize)]
pub struct HistorySnapshot {
    /// Type marker for the client to identify this message
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    /// Historical metrics (oldest first)
    pub history: Vec<SystemMetrics>,
    /// Per-container historical metrics (oldest first), keyed by workspace_id
    pub container_history: HashMap<String, Vec<ContainerMetrics>>,
}

/// Combined broadcast message containing both system and container metrics
#[derive(Debug, Clone)]
pub(crate) struct MonitoringBroadcast {
    system: SystemMetrics,
    containers: Vec<ContainerMetrics>,
}

/// Shared monitoring state that persists across connections
pub struct MonitoringState {
    /// Historical metrics buffer (oldest first)
    history: RwLock<VecDeque<SystemMetrics>>,
    /// Per-container historical metrics (oldest first), keyed by workspace_id
    container_history: RwLock<HashMap<String, VecDeque<ContainerMetrics>>>,
    /// Broadcast channel for real-time updates
    broadcast_tx: broadcast::Sender<MonitoringBroadcast>,
    /// Workspace store for querying container workspaces
    workspaces: RwLock<Option<SharedWorkspaceStore>>,
}

impl MonitoringState {
    pub fn new() -> Arc<Self> {
        let (broadcast_tx, _) = broadcast::channel(64);
        let state = Arc::new(Self {
            history: RwLock::new(VecDeque::with_capacity(HISTORY_SIZE)),
            container_history: RwLock::new(HashMap::new()),
            broadcast_tx,
            workspaces: RwLock::new(None),
        });

        // Start the background collector task
        let state_clone = Arc::clone(&state);
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(state_clone.run_collector())
                .catch_unwind()
                .await;
            if let Err(err) = result {
                tracing::error!("Monitoring collector panicked: {:?}", err);
            }
        });

        state
    }

    /// Set the workspace store for container metrics collection.
    /// Called after AppState is initialized.
    pub async fn set_workspaces(&self, ws: SharedWorkspaceStore) {
        let mut guard = self.workspaces.write().await;
        *guard = Some(ws);
    }

    /// Snapshot of host memory pressure + the top memory-consuming container
    /// workspaces. Backs the in-app memory-pressure banner and (later) mission
    /// admission control. Host figures come from a fresh `sysinfo` read; the
    /// per-container figures reuse the latest sample the background collector
    /// already keeps, so this is cheap to call on a poll.
    pub async fn memory_health(&self) -> MemoryHealth {
        let mut sys = System::new();
        sys.refresh_memory();
        let memory_total = sys.total_memory();
        let memory_used = sys.used_memory();
        let swap_total = sys.total_swap();
        let swap_used = sys.used_swap();
        let memory_percent = pct(memory_used, memory_total);
        let swap_percent = pct(swap_used, swap_total);

        let mut top_consumers: Vec<MemoryConsumer> = {
            let ch = self.container_history.read().await;
            ch.values()
                .filter_map(|h| h.back())
                .map(|cm| MemoryConsumer {
                    workspace_id: cm.workspace_id.clone(),
                    workspace_name: cm.workspace_name.clone(),
                    memory_used: cm.memory_used,
                    memory_percent: cm.memory_percent,
                })
                .collect()
        };
        top_consumers.sort_by_key(|c| std::cmp::Reverse(c.memory_used));
        top_consumers.truncate(5);

        MemoryHealth {
            level: MemoryHealthLevel::from_usage(memory_percent, swap_percent),
            memory_used,
            memory_total,
            memory_percent,
            swap_used,
            swap_total,
            swap_percent,
            top_consumers,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    /// Background task that continuously collects metrics
    async fn run_collector(self: Arc<Self>) {
        let mut sys = System::new_all();
        let mut networks = Networks::new_with_refreshed_list();

        // Track previous network stats for calculating rates
        let mut prev_rx_bytes: u64 = 0;
        let mut prev_tx_bytes: u64 = 0;
        let mut prev_time = std::time::Instant::now();

        // Track previous CPU nanoseconds per container for delta calculation
        let mut prev_cpu_ns: HashMap<String, u64> = HashMap::new();
        let mut prev_container_time = std::time::Instant::now();

        // Initial refresh
        sys.refresh_all();
        networks.refresh();

        // Get initial network totals
        for (_name, data) in networks.iter() {
            prev_rx_bytes += data.total_received();
            prev_tx_bytes += data.total_transmitted();
        }

        // Collection interval (1 second)
        let interval = Duration::from_secs(1);
        let num_cpus = sys.cpus().len().max(1) as f64;

        loop {
            tokio::time::sleep(interval).await;

            // Refresh system info
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            networks.refresh();

            // Calculate CPU usage
            let cpu_percent = sys.global_cpu_usage();
            let cpu_cores: Vec<f32> = sys.cpus().iter().map(|cpu| cpu.cpu_usage()).collect();

            // Calculate memory usage
            let memory_used = sys.used_memory();
            let memory_total = sys.total_memory();
            let memory_percent = if memory_total > 0 {
                (memory_used as f64 / memory_total as f64 * 100.0) as f32
            } else {
                0.0
            };

            // Calculate network rates
            let now = std::time::Instant::now();
            let elapsed_secs = now.duration_since(prev_time).as_secs_f64();

            let mut current_rx_bytes: u64 = 0;
            let mut current_tx_bytes: u64 = 0;
            for (_name, data) in networks.iter() {
                current_rx_bytes += data.total_received();
                current_tx_bytes += data.total_transmitted();
            }

            let rx_diff = current_rx_bytes.saturating_sub(prev_rx_bytes);
            let tx_diff = current_tx_bytes.saturating_sub(prev_tx_bytes);

            let network_rx_bytes_per_sec = if elapsed_secs > 0.0 {
                (rx_diff as f64 / elapsed_secs) as u64
            } else {
                0
            };
            let network_tx_bytes_per_sec = if elapsed_secs > 0.0 {
                (tx_diff as f64 / elapsed_secs) as u64
            } else {
                0
            };

            prev_rx_bytes = current_rx_bytes;
            prev_tx_bytes = current_tx_bytes;
            prev_time = now;

            let metrics = SystemMetrics {
                cpu_percent,
                cpu_cores,
                memory_used,
                memory_total,
                memory_percent,
                network_rx_bytes_per_sec,
                network_tx_bytes_per_sec,
                timestamp_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };

            // Add to history
            {
                let mut history = self.history.write().await;
                if history.len() >= HISTORY_SIZE {
                    history.pop_front();
                }
                history.push_back(metrics.clone());
            }

            // Collect per-container metrics
            let container_elapsed_secs = now.duration_since(prev_container_time).as_secs_f64();
            prev_container_time = now;
            let container_metrics = self
                .collect_container_metrics(
                    &mut prev_cpu_ns,
                    container_elapsed_secs,
                    num_cpus,
                    memory_total,
                )
                .await;

            // Add container metrics to history
            {
                let mut ch = self.container_history.write().await;
                // Track which workspace IDs are still active
                let active_ids: std::collections::HashSet<String> = container_metrics
                    .iter()
                    .map(|m| m.workspace_id.clone())
                    .collect();

                for cm in &container_metrics {
                    let history = ch
                        .entry(cm.workspace_id.clone())
                        .or_insert_with(|| VecDeque::with_capacity(HISTORY_SIZE));
                    if history.len() >= HISTORY_SIZE {
                        history.pop_front();
                    }
                    history.push_back(cm.clone());
                }

                // Remove history for containers that no longer exist
                ch.retain(|id, _| active_ids.contains(id));
            }

            // Broadcast to all connected clients (ignore if no receivers)
            let _ = self.broadcast_tx.send(MonitoringBroadcast {
                system: metrics,
                containers: container_metrics,
            });
        }
    }

    /// Collect CPU and memory metrics for all active container workspaces.
    async fn collect_container_metrics(
        &self,
        prev_cpu_ns: &mut HashMap<String, u64>,
        elapsed_secs: f64,
        num_cpus: f64,
        host_memory_total: u64,
    ) -> Vec<ContainerMetrics> {
        let workspaces = {
            let guard = self.workspaces.read().await;
            match guard.as_ref() {
                Some(ws) => ws.list().await,
                None => return Vec::new(),
            }
        };

        let mut results = Vec::new();
        let elapsed_ns = (elapsed_secs * 1_000_000_000.0) as u64;

        for ws in workspaces {
            if ws.workspace_type != WorkspaceType::Container || ws.status != WorkspaceStatus::Ready
            {
                continue;
            }

            let ws_id = ws.id.to_string();

            // Derive the machine name from the workspace path (last path component),
            // matching how systemd-nspawn auto-derives it when no --machine flag is used.
            let machine_name = ws
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&ws.name);

            // Escape the machine name for systemd unit naming. Hyphens within the
            // name part must be escaped as `\x2d` because `-` is a path separator
            // in systemd unit hierarchies. Without this, `systemctl show` and
            // `is-active` silently return empty/inactive results for containers
            // whose names contain hyphens (e.g., `dgx-spark`).
            let escaped_name = systemd_escape(machine_name);
            let scope_name = format!("machine-{}.scope", escaped_name);

            // Check if the scope is actually active before querying properties.
            // `systemctl show` returns exit 0 even for non-existent units (with
            // `[not set]` for all values), so we need an explicit liveness check.
            let is_active = tokio::process::Command::new("systemctl")
                .args(["is-active", "--quiet", &scope_name])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !is_active {
                continue;
            }

            let output = match tokio::process::Command::new("systemctl")
                .args(["show", &scope_name])
                .output()
                .await
            {
                Ok(o) if o.status.success() => o,
                _ => continue,
            };

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut memory_current: Option<u64> = None;
            let mut memory_max: Option<u64> = None;
            let mut cpu_usage_ns: Option<u64> = None;

            for line in stdout.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "MemoryCurrent" => memory_current = value.parse::<u64>().ok(),
                        "MemoryMax" => {
                            if value == "infinity" {
                                memory_max = Some(host_memory_total);
                            } else {
                                memory_max = value.parse::<u64>().ok();
                            }
                        }
                        "CPUUsageNSec" => cpu_usage_ns = value.parse::<u64>().ok(),
                        _ => {}
                    }
                }
            }

            let mem_used = memory_current.unwrap_or(0);
            // Use host total memory as fallback when cgroup limit is unlimited
            let mem_total = match memory_max {
                Some(v) if v < u64::MAX => v,
                _ => host_memory_total,
            };
            let mem_percent = if mem_total > 0 {
                (mem_used as f64 / mem_total as f64) * 100.0
            } else {
                0.0
            };

            // Calculate CPU % from delta of cumulative CPUUsageNSec
            let cpu_pct = if let Some(current_ns) = cpu_usage_ns {
                let prev = prev_cpu_ns.get(&ws_id).copied().unwrap_or(current_ns);
                let delta_ns = current_ns.saturating_sub(prev);
                prev_cpu_ns.insert(ws_id.clone(), current_ns);

                if elapsed_ns > 0 {
                    // CPU% relative to all cores: delta_ns / (elapsed_ns * num_cores) * 100
                    (delta_ns as f64 / (elapsed_ns as f64 * num_cpus)) * 100.0
                } else {
                    0.0
                }
            } else {
                0.0
            };

            results.push(ContainerMetrics {
                workspace_id: ws_id,
                workspace_name: ws.name.clone(),
                cpu_percent: cpu_pct,
                memory_used: mem_used,
                memory_total: mem_total,
                memory_percent: mem_percent,
            });
        }

        // Clean up prev_cpu_ns for containers that no longer exist
        let active_ids: std::collections::HashSet<&str> =
            results.iter().map(|m| m.workspace_id.as_str()).collect();
        prev_cpu_ns.retain(|id, _| active_ids.contains(id.as_str()));

        results
    }

    /// Get a snapshot of the current history
    pub async fn get_history(&self) -> Vec<SystemMetrics> {
        let history = self.history.read().await;
        history.iter().cloned().collect()
    }

    /// Get a snapshot of per-container history
    pub async fn get_container_history(&self) -> HashMap<String, Vec<ContainerMetrics>> {
        let ch = self.container_history.read().await;
        ch.iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect()
    }

    /// Subscribe to real-time updates
    pub fn subscribe(&self) -> broadcast::Receiver<MonitoringBroadcast> {
        self.broadcast_tx.subscribe()
    }
}

/// Global monitoring state - lazily initialized
static MONITORING_STATE: std::sync::OnceLock<Arc<MonitoringState>> = std::sync::OnceLock::new();

fn get_monitoring_state() -> Arc<MonitoringState> {
    MONITORING_STATE.get_or_init(MonitoringState::new).clone()
}

/// `GET /api/monitoring/memory-health` — current host memory pressure level
/// (ok / warn / critical) plus the top container memory consumers, for the
/// in-app memory-pressure banner. Auth is enforced by the protected-routes
/// middleware.
pub async fn memory_health_handler() -> Json<MemoryHealth> {
    Json(get_monitoring_state().memory_health().await)
}

/// Initialize the monitoring background collector at server startup.
/// This ensures history is populated before the first client connects.
pub fn init_monitoring() {
    // Calling get_monitoring_state() will initialize the state if not already done,
    // which spawns the background collector task.
    let _ = get_monitoring_state();
    tracing::info!("Monitoring background collector started");
}

/// Set the workspace store so the monitoring collector can query container metrics.
/// Should be called after AppState is initialized.
pub async fn init_monitoring_workspaces(workspaces: SharedWorkspaceStore) {
    let state = get_monitoring_state();
    state.set_workspaces(workspaces).await;
    tracing::info!("Monitoring container metrics collection enabled");
}

/// Extract JWT from WebSocket subprotocol header
fn extract_jwt_from_protocols(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;
    for part in raw.split(',').map(|s| s.trim()) {
        if let Some(rest) = part.strip_prefix("jwt.") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// WebSocket endpoint for streaming system metrics
pub async fn monitoring_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MonitoringParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let _ = params.interval_ms;
    // Enforce auth in non-dev mode
    if state.config.auth.auth_required(state.config.dev_mode) {
        let token = match extract_jwt_from_protocols(&headers) {
            Some(t) => t,
            None => return (StatusCode::UNAUTHORIZED, "Missing websocket JWT").into_response(),
        };
        if !auth::verify_token_for_config(&token, &state.config) {
            return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
        }
    }

    ws.protocols(["sandboxed"])
        .on_upgrade(handle_monitoring_stream)
}

/// Client command for controlling the monitoring stream
#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
enum ClientCommand {
    #[serde(rename = "pause")]
    Pause,
    #[serde(rename = "resume")]
    Resume,
}

/// Handle the WebSocket connection for system monitoring
async fn handle_monitoring_stream(socket: WebSocket) {
    tracing::info!("New monitoring stream client connected");

    let monitoring = get_monitoring_state();

    // Split the socket
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Send historical data first (system + container)
    let history = monitoring.get_history().await;
    let container_history = monitoring.get_container_history().await;
    if !history.is_empty() || !container_history.is_empty() {
        let snapshot = HistorySnapshot {
            msg_type: "history",
            history,
            container_history,
        };
        if let Ok(json) = serde_json::to_string(&snapshot) {
            if ws_sender.send(Message::Text(json)).await.is_err() {
                tracing::debug!("Client disconnected before receiving history");
                return;
            }
        }
    }

    // Subscribe to real-time updates
    let mut rx = monitoring.subscribe();

    // Channel for control commands
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<ClientCommand>();

    // Spawn task to handle incoming messages
    let cmd_tx_clone = cmd_tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(cmd) = serde_json::from_str::<ClientCommand>(&t) {
                        let _ = cmd_tx_clone.send(cmd);
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let mut paused = false;

    // Main streaming loop
    let mut stream_task = tokio::spawn(async move {
        loop {
            // Check for control commands (non-blocking)
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    ClientCommand::Pause => {
                        paused = true;
                    }
                    ClientCommand::Resume => {
                        paused = false;
                    }
                }
            }

            // Wait for next broadcast
            match rx.recv().await {
                Ok(broadcast) => {
                    if paused {
                        continue;
                    }

                    // Send system metrics
                    let json = match serde_json::to_string(&broadcast.system) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };

                    if ws_sender.send(Message::Text(json)).await.is_err() {
                        tracing::debug!("Client disconnected from monitoring stream");
                        break;
                    }

                    // Always send container metrics (even if empty) so the
                    // frontend can clean up when all containers stop.
                    let msg = ContainerMetricsMessage {
                        msg_type: "container_metrics",
                        containers: broadcast.containers,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if ws_sender.send(Message::Text(json)).await.is_err() {
                            tracing::debug!("Client disconnected during container metrics send");
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Monitoring client lagged by {} messages", n);
                    // Continue receiving
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!("Monitoring broadcast channel closed");
                    break;
                }
            }
        }

        tracing::info!("Monitoring stream client disconnected");
    });

    // Wait for either task to complete
    tokio::select! {
        _ = &mut recv_task => {
            stream_task.abort();
        }
        _ = &mut stream_task => {
            recv_task.abort();
        }
    }
}

/// Escape a string for use as a systemd unit name component.
///
/// Systemd uses `-` as a path separator in unit hierarchies, so hyphens
/// within a name must be escaped as `\x2d`. Other non-alphanumeric chars
/// (except `_` and `.`) are also hex-escaped.
fn systemd_escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '-' => out.push_str("\\x2d"),
            c if c.is_ascii_alphanumeric() || c == '_' || c == '.' => out.push(c),
            c => {
                // Hex-escape other characters
                for b in c.to_string().as_bytes() {
                    out.push_str(&format!("\\x{:02x}", b));
                }
            }
        }
    }
    out
}
