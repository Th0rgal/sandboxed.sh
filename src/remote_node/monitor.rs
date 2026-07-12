//! Background fleet monitor: periodically polls every configured remote
//! runner node's `/heartbeat` and caches per-node statuses plus a bounded
//! history of recent dispatch outcomes.
//!
//! Status semantics:
//! - `Online`: latest probe returned a fresh heartbeat.
//! - `Degraded`: 1-2 consecutive missed probes.
//! - `Offline`: 3 or more consecutive missed probes.
//! - `Unknown`: never probed (monitor disabled or not yet run).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::client::RemoteNodeClient;
use super::protocol::NodeHeartbeat;
use super::{RemoteNodeConfig, RemoteNodeSettings, RemoteNodeStatus};

/// Consecutive missed probes after which a node is considered `Offline`
/// (fewer misses report `Degraded`).
const OFFLINE_MISS_THRESHOLD: u32 = 3;

/// Bounded length of the recent dispatch-outcome history.
const RECENT_OUTCOMES_CAP: usize = 50;

/// Cached status for one configured node.
#[derive(Debug, Clone, Serialize)]
pub struct CachedNodeStatus {
    pub node_id: String,
    pub status: RemoteNodeStatus,
    /// Consecutive heartbeat misses (0 while online).
    pub consecutive_misses: u32,
    /// Last successfully parsed heartbeat payload.
    pub last_heartbeat: Option<NodeHeartbeat>,
    /// When the last successful heartbeat was received.
    pub last_seen: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// Outcome record for a remote dispatch (sync `/execute` or async job).
#[derive(Debug, Clone, Serialize)]
pub struct DispatchOutcome {
    pub mission_id: Uuid,
    pub node_id: String,
    /// Async job id; `None` for the synchronous `/execute` MVP path.
    pub job_id: Option<Uuid>,
    /// queued | running | succeeded | failed | cancelled | lost | executed
    pub state: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Pure status transition applied after one heartbeat probe.
///
/// Returns the new `(status, consecutive_misses)` pair given the previous
/// miss count and whether the probe succeeded.
pub fn status_after_probe(previous_misses: u32, probe_ok: bool) -> (RemoteNodeStatus, u32) {
    if probe_ok {
        return (RemoteNodeStatus::Online, 0);
    }
    let misses = previous_misses.saturating_add(1);
    let status = if misses >= OFFLINE_MISS_THRESHOLD {
        RemoteNodeStatus::Offline
    } else {
        RemoteNodeStatus::Degraded
    };
    (status, misses)
}

/// Shared cache of node statuses and recent dispatch outcomes.
///
/// Uses `std::sync::RwLock` (never held across `.await`) so synchronous
/// callers like `RemoteNodeOverview::from_settings` can read it too.
pub struct FleetMonitor {
    statuses: RwLock<HashMap<String, CachedNodeStatus>>,
    recent: RwLock<VecDeque<DispatchOutcome>>,
}

impl Default for FleetMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetMonitor {
    pub fn new() -> Self {
        Self {
            statuses: RwLock::new(HashMap::new()),
            recent: RwLock::new(VecDeque::new()),
        }
    }

    /// Record a successful heartbeat probe.
    pub fn record_heartbeat(&self, node_id: &str, heartbeat: NodeHeartbeat) {
        let (status, misses) = status_after_probe(0, true);
        let mut statuses = self.statuses.write().unwrap_or_else(|e| e.into_inner());
        statuses.insert(
            node_id.to_string(),
            CachedNodeStatus {
                node_id: node_id.to_string(),
                status,
                consecutive_misses: misses,
                last_heartbeat: Some(heartbeat),
                last_seen: Some(Utc::now()),
                last_error: None,
            },
        );
    }

    /// Record a failed heartbeat probe (network error, auth failure, missing
    /// token env, ...). Keeps the last known heartbeat payload for display.
    pub fn record_miss(&self, node_id: &str, error: String) {
        let mut statuses = self.statuses.write().unwrap_or_else(|e| e.into_inner());
        let entry = statuses
            .entry(node_id.to_string())
            .or_insert_with(|| CachedNodeStatus {
                node_id: node_id.to_string(),
                status: RemoteNodeStatus::Unknown,
                consecutive_misses: 0,
                last_heartbeat: None,
                last_seen: None,
                last_error: None,
            });
        let (status, misses) = status_after_probe(entry.consecutive_misses, false);
        entry.status = status;
        entry.consecutive_misses = misses;
        entry.last_error = Some(error);
    }

    pub fn get(&self, node_id: &str) -> Option<CachedNodeStatus> {
        self.statuses
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(node_id)
            .cloned()
    }

    /// Aggregate fleet status across all probed nodes; `None` when no node
    /// has been probed yet.
    pub fn aggregate_status(&self) -> Option<RemoteNodeStatus> {
        let statuses = self.statuses.read().unwrap_or_else(|e| e.into_inner());
        if statuses.is_empty() {
            return None;
        }
        let online = statuses
            .values()
            .filter(|s| s.status == RemoteNodeStatus::Online)
            .count();
        Some(if online == statuses.len() {
            RemoteNodeStatus::Online
        } else if online > 0
            || statuses
                .values()
                .any(|s| s.status == RemoteNodeStatus::Degraded)
        {
            RemoteNodeStatus::Degraded
        } else {
            RemoteNodeStatus::Offline
        })
    }

    /// Record (or update, keyed by `job_id`) a dispatch outcome.
    pub fn record_outcome(&self, outcome: DispatchOutcome) {
        let mut recent = self.recent.write().unwrap_or_else(|e| e.into_inner());
        if let Some(job_id) = outcome.job_id {
            if let Some(existing) = recent.iter_mut().find(|entry| entry.job_id == Some(job_id)) {
                *existing = outcome;
                return;
            }
        }
        recent.push_front(outcome);
        recent.truncate(RECENT_OUTCOMES_CAP);
    }

    /// Most recent dispatch outcomes, newest first.
    pub fn recent_outcomes(&self, limit: usize) -> Vec<DispatchOutcome> {
        self.recent
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }
}

static GLOBAL_FLEET: OnceLock<Arc<FleetMonitor>> = OnceLock::new();

/// Register the process-wide fleet monitor so synchronous status readers
/// (e.g. `RemoteNodeOverview`) can consult the cache without `AppState`.
pub fn register_global_fleet(fleet: &Arc<FleetMonitor>) {
    let _ = GLOBAL_FLEET.set(Arc::clone(fleet));
}

pub fn global_fleet() -> Option<Arc<FleetMonitor>> {
    GLOBAL_FLEET.get().cloned()
}

/// Per-node view merged from static config and the monitor cache, as served
/// by `GET /api/remote-nodes`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteNodeView {
    pub id: String,
    pub base_url: String,
    pub token_env: String,
    pub status: RemoteNodeStatus,
    pub labels: Vec<String>,
    pub version: Option<String>,
    pub protocol_version: Option<u32>,
    pub capacity_total: Option<u32>,
    pub capacity_available: Option<u32>,
    pub active_leases: Option<u32>,
    pub active_jobs: Option<u32>,
    pub queued_jobs: Option<u32>,
    pub cpu_total: Option<u32>,
    pub mem_total_bytes: Option<u64>,
    pub mem_available_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub disk_available_bytes: Option<u64>,
    pub cached_toolchains: Vec<String>,
    pub last_seen: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

impl RemoteNodeView {
    pub fn from_cache(config: &RemoteNodeConfig, cached: Option<&CachedNodeStatus>) -> Self {
        let heartbeat = cached.and_then(|c| c.last_heartbeat.as_ref());
        let mut labels = heartbeat.map(|h| h.labels.clone()).unwrap_or_default();
        if labels.is_empty() {
            labels = config.labels.clone().unwrap_or_default();
        }
        Self {
            id: config.id.clone(),
            base_url: config.base_url.clone(),
            token_env: config.token_env.clone(),
            status: cached
                .map(|c| c.status.clone())
                .unwrap_or(RemoteNodeStatus::Unknown),
            labels,
            version: heartbeat.map(|h| h.version.clone()),
            protocol_version: heartbeat.map(|h| h.protocol_version),
            capacity_total: heartbeat.map(|h| h.capacity_total),
            capacity_available: heartbeat.map(|h| h.capacity_available),
            active_leases: heartbeat.map(|h| h.active_leases),
            active_jobs: heartbeat.map(|h| h.active_jobs),
            queued_jobs: heartbeat.map(|h| h.queued_jobs),
            cpu_total: heartbeat.map(|h| h.cpu_total),
            mem_total_bytes: heartbeat.map(|h| h.mem_total_bytes),
            mem_available_bytes: heartbeat.map(|h| h.mem_available_bytes),
            disk_total_bytes: heartbeat.map(|h| h.disk_total_bytes),
            disk_available_bytes: heartbeat.map(|h| h.disk_available_bytes),
            cached_toolchains: heartbeat
                .map(|h| h.cached_toolchains.clone())
                .unwrap_or_default(),
            last_seen: cached.and_then(|c| c.last_seen),
            error: cached.and_then(|c| c.last_error.clone()),
        }
    }
}

/// Response body for `GET /api/remote-nodes`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteNodesResponse {
    pub enabled: bool,
    pub nodes: Vec<RemoteNodeView>,
    /// Last dispatch outcomes across the fleet, newest first (max 10).
    pub recent_jobs: Vec<DispatchOutcome>,
}

/// Probe one node and record the result into the monitor cache.
pub async fn probe_node(fleet: &FleetMonitor, client: &RemoteNodeClient, node: &RemoteNodeConfig) {
    let token = std::env::var(&node.token_env)
        .ok()
        .filter(|token| !token.trim().is_empty());
    match token {
        Some(token) => match client.heartbeat(node, &token).await {
            Ok(heartbeat) => fleet.record_heartbeat(&node.id, heartbeat),
            Err(err) => fleet.record_miss(&node.id, err.to_string()),
        },
        None => fleet.record_miss(&node.id, format!("missing token env {}", node.token_env)),
    }
}

/// Spawn the background fleet monitor loop.
///
/// Poll interval comes from `REMOTE_NODE_MONITOR_SECS` (default 15). Setting
/// it to `0` disables the loop; the cache is then only fed on demand by
/// `GET /api/remote-nodes`.
pub fn spawn_fleet_monitor(fleet: Arc<FleetMonitor>, settings: RemoteNodeSettings) {
    // Register unconditionally so on-demand probes still feed the same
    // globally visible cache when the periodic loop is disabled.
    register_global_fleet(&fleet);
    let interval_secs = std::env::var("REMOTE_NODE_MONITOR_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(15);
    if interval_secs == 0 {
        tracing::info!("Fleet monitor disabled (REMOTE_NODE_MONITOR_SECS=0)");
        return;
    }
    if !settings.enabled || settings.nodes.is_empty() {
        return;
    }
    tracing::info!(
        nodes = settings.nodes.len(),
        interval_secs,
        "Starting remote-node fleet monitor"
    );
    tokio::spawn(async move {
        let client = RemoteNodeClient::default();
        loop {
            let probes = settings
                .nodes
                .iter()
                .map(|node| probe_node(&fleet, &client, node));
            futures::future::join_all(probes).await;
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat(node_id: &str) -> NodeHeartbeat {
        serde_json::from_value(serde_json::json!({
            "node_id": node_id,
            "online": true,
            "capacity_total": 1,
            "capacity_available": 1,
            "active_leases": 0,
            "version": "test",
        }))
        .unwrap()
    }

    #[test]
    fn probe_transitions_follow_miss_thresholds() {
        // Fresh heartbeat is always Online with the miss counter reset.
        assert_eq!(status_after_probe(0, true), (RemoteNodeStatus::Online, 0));
        assert_eq!(status_after_probe(7, true), (RemoteNodeStatus::Online, 0));
        // 1-2 consecutive misses degrade, 3+ go offline.
        assert_eq!(
            status_after_probe(0, false),
            (RemoteNodeStatus::Degraded, 1)
        );
        assert_eq!(
            status_after_probe(1, false),
            (RemoteNodeStatus::Degraded, 2)
        );
        assert_eq!(status_after_probe(2, false), (RemoteNodeStatus::Offline, 3));
        assert_eq!(
            status_after_probe(9, false),
            (RemoteNodeStatus::Offline, 10)
        );
    }

    #[test]
    fn monitor_tracks_status_and_recovery() {
        let fleet = FleetMonitor::new();
        assert!(fleet.aggregate_status().is_none());

        fleet.record_heartbeat("babylon", heartbeat("babylon"));
        assert_eq!(
            fleet.get("babylon").unwrap().status,
            RemoteNodeStatus::Online
        );
        assert_eq!(fleet.aggregate_status(), Some(RemoteNodeStatus::Online));

        fleet.record_miss("babylon", "timeout".to_string());
        fleet.record_miss("babylon", "timeout".to_string());
        let cached = fleet.get("babylon").unwrap();
        assert_eq!(cached.status, RemoteNodeStatus::Degraded);
        assert_eq!(cached.consecutive_misses, 2);
        // Last heartbeat payload is preserved through misses.
        assert!(cached.last_heartbeat.is_some());

        fleet.record_miss("babylon", "timeout".to_string());
        assert_eq!(
            fleet.get("babylon").unwrap().status,
            RemoteNodeStatus::Offline
        );
        assert_eq!(fleet.aggregate_status(), Some(RemoteNodeStatus::Offline));

        // Recovery resets both status and the miss counter.
        fleet.record_heartbeat("babylon", heartbeat("babylon"));
        let cached = fleet.get("babylon").unwrap();
        assert_eq!(cached.status, RemoteNodeStatus::Online);
        assert_eq!(cached.consecutive_misses, 0);
    }

    #[test]
    fn recent_outcomes_dedup_by_job_id_and_stay_bounded() {
        let fleet = FleetMonitor::new();
        let job_id = Uuid::new_v4();
        let outcome = |state: &str, job: Option<Uuid>| DispatchOutcome {
            mission_id: Uuid::new_v4(),
            node_id: "babylon".to_string(),
            job_id: job,
            state: state.to_string(),
            exit_code: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        };
        fleet.record_outcome(outcome("queued", Some(job_id)));
        fleet.record_outcome(outcome("succeeded", Some(job_id)));
        let recent = fleet.recent_outcomes(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].state, "succeeded");

        for _ in 0..(RECENT_OUTCOMES_CAP + 10) {
            fleet.record_outcome(outcome("executed", None));
        }
        assert_eq!(fleet.recent_outcomes(usize::MAX).len(), RECENT_OUTCOMES_CAP);
        assert_eq!(fleet.recent_outcomes(10).len(), 10);
    }
}
