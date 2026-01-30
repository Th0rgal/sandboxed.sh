//! WebSocket-based real-time system monitoring.
//!
//! Provides CPU, memory, and network usage metrics streamed
//! to connected clients via WebSocket. Maintains a history buffer
//! so new clients receive recent data immediately.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sysinfo::{Networks, System};
use tokio::sync::{broadcast, RwLock};

use super::auth;
use super::routes::AppState;

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

/// Initial snapshot message sent to new clients
#[derive(Debug, Clone, Serialize)]
pub struct HistorySnapshot {
    /// Type marker for the client to identify this message
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    /// Historical metrics (oldest first)
    pub history: Vec<SystemMetrics>,
}

/// Shared monitoring state that persists across connections
pub struct MonitoringState {
    /// Historical metrics buffer (oldest first)
    history: RwLock<VecDeque<SystemMetrics>>,
    /// Broadcast channel for real-time updates
    broadcast_tx: broadcast::Sender<SystemMetrics>,
}

impl MonitoringState {
    pub fn new() -> Arc<Self> {
        let (broadcast_tx, _) = broadcast::channel(64);
        let state = Arc::new(Self {
            history: RwLock::new(VecDeque::with_capacity(HISTORY_SIZE)),
            broadcast_tx,
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

    /// Background task that continuously collects metrics
    async fn run_collector(self: Arc<Self>) {
        let mut sys = System::new_all();
        let mut networks = Networks::new_with_refreshed_list();

        // Track previous network stats for calculating rates
        let mut prev_rx_bytes: u64 = 0;
        let mut prev_tx_bytes: u64 = 0;
        let mut prev_time = std::time::Instant::now();

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

            // Broadcast to all connected clients (ignore if no receivers)
            let _ = self.broadcast_tx.send(metrics);
        }
    }

    /// Get a snapshot of the current history
    pub async fn get_history(&self) -> Vec<SystemMetrics> {
        let history = self.history.read().await;
        history.iter().cloned().collect()
    }

    /// Subscribe to real-time updates
    pub fn subscribe(&self) -> broadcast::Receiver<SystemMetrics> {
        self.broadcast_tx.subscribe()
    }
}

/// Global monitoring state - lazily initialized
static MONITORING_STATE: std::sync::OnceLock<Arc<MonitoringState>> = std::sync::OnceLock::new();

fn get_monitoring_state() -> Arc<MonitoringState> {
    MONITORING_STATE.get_or_init(MonitoringState::new).clone()
}

/// Initialize the monitoring background collector at server startup.
/// This ensures history is populated before the first client connects.
pub fn init_monitoring() {
    // Calling get_monitoring_state() will initialize the state if not already done,
    // which spawns the background collector task.
    let _ = get_monitoring_state();
    tracing::info!("Monitoring background collector started");
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
    Query(_params): Query<MonitoringParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
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

    // Send historical data first
    let history = monitoring.get_history().await;
    if !history.is_empty() {
        let snapshot = HistorySnapshot {
            msg_type: "history",
            history,
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
                Ok(metrics) => {
                    if paused {
                        continue;
                    }

                    let json = match serde_json::to_string(&metrics) {
                        Ok(j) => j,
                        Err(_) => continue,
                    };

                    if ws_sender.send(Message::Text(json)).await.is_err() {
                        tracing::debug!("Client disconnected from monitoring stream");
                        break;
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
