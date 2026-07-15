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
    /// When the request producing the last successful heartbeat started.
    /// Reservations created after this instant cannot safely be assumed to
    /// be represented by that heartbeat payload.
    pub last_probe_started_at: Option<DateTime<Utc>>,
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
        self.record_heartbeat_for_probe(node_id, heartbeat, Utc::now());
    }

    pub fn record_heartbeat_for_probe(
        &self,
        node_id: &str,
        heartbeat: NodeHeartbeat,
        probe_started_at: DateTime<Utc>,
    ) {
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
                last_probe_started_at: Some(probe_started_at),
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
                last_probe_started_at: None,
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

/// Placement thresholds (env-overridable defaults).
const DEFAULT_MIN_DISK_GB: u64 = 20;
const DEFAULT_MIN_MEM_GB: u64 = 8;

fn env_gb_bytes(key: &str, default_gb: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default_gb)
        .saturating_mul(1 << 30)
}

/// Why auto placement found no eligible node. FAIL CLOSED: every configured
/// node is listed with its own exclusion reason so the caller (and the
/// wrapper's fallback logging) can see exactly what was ruled out and why.
#[derive(Debug, Clone, Serialize)]
pub struct PlacementError {
    /// `(node_id, reason)` per configured node.
    pub reasons: Vec<(String, String)>,
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.reasons.is_empty() {
            return write!(f, "no remote nodes are configured");
        }
        let detail = self
            .reasons
            .iter()
            .map(|(node, reason)| format!("{node}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        write!(f, "no eligible remote node ({detail})")
    }
}

impl std::error::Error for PlacementError {}

/// Pure capacity-aware auto placement over cached node statuses.
///
/// A node is eligible when it is `Online` with a heartbeat, its labels cover
/// every requirement, it has at least `min_disk_bytes` disk and
/// `min_mem_bytes` memory available, and its in-flight load
/// (`active_jobs + queued_jobs + active_leases`) is below
/// `2 * capacity_total`. Eligible
/// nodes are ranked least-loaded first, breaking ties on the most available
/// memory.
pub fn select_node_auto(
    nodes: &[RemoteNodeConfig],
    statuses: &HashMap<String, CachedNodeStatus>,
    requirements: &[String],
    min_disk_bytes: u64,
    min_mem_bytes: u64,
) -> Result<String, PlacementError> {
    select_node_auto_with_reservations(
        nodes,
        statuses,
        requirements,
        min_disk_bytes,
        min_mem_bytes,
        &HashMap::new(),
    )
}

/// Capacity-aware placement that also counts jobs accepted by the core but
/// not yet reflected in the nodes' periodic heartbeats.
pub fn select_node_auto_with_reservations(
    nodes: &[RemoteNodeConfig],
    statuses: &HashMap<String, CachedNodeStatus>,
    requirements: &[String],
    min_disk_bytes: u64,
    min_mem_bytes: u64,
    reservations: &HashMap<String, u32>,
) -> Result<String, PlacementError> {
    let mut reasons: Vec<(String, String)> = Vec::new();
    let mut eligible: Vec<(u32, u64, String)> = Vec::new(); // (load, mem_avail, id)
    for node in nodes {
        let cached = statuses.get(&node.id);
        let status = cached
            .map(|c| c.status.clone())
            .unwrap_or(RemoteNodeStatus::Unknown);
        if status != RemoteNodeStatus::Online {
            reasons.push((node.id.clone(), format!("not online (status: {status:?})")));
            continue;
        }
        let Some(heartbeat) = cached.and_then(|c| c.last_heartbeat.as_ref()) else {
            reasons.push((node.id.clone(), "no heartbeat data".to_string()));
            continue;
        };
        if requirements.iter().any(|requirement| requirement == "lean")
            && heartbeat.lean_runtime_ready == Some(false)
        {
            reasons.push((
                node.id.clone(),
                "Lean runtime unavailable (Lake proxy missing)".to_string(),
            ));
            continue;
        }
        // Labels come from the heartbeat, falling back to static config for
        // nodes that don't report any (mirrors RemoteNodeView).
        let labels: &[String] = if heartbeat.labels.is_empty() {
            node.labels.as_deref().unwrap_or(&[])
        } else {
            &heartbeat.labels
        };
        if let Some(missing) = requirements.iter().find(|req| !labels.contains(req)) {
            reasons.push((node.id.clone(), format!("missing label '{missing}'")));
            continue;
        }
        if heartbeat.disk_available_bytes < min_disk_bytes {
            reasons.push((
                node.id.clone(),
                format!(
                    "low disk ({} GiB available, {} GiB required)",
                    heartbeat.disk_available_bytes / (1 << 30),
                    min_disk_bytes / (1 << 30)
                ),
            ));
            continue;
        }
        if heartbeat.mem_available_bytes < min_mem_bytes {
            reasons.push((
                node.id.clone(),
                format!(
                    "low memory ({} GiB available, {} GiB required)",
                    heartbeat.mem_available_bytes / (1 << 30),
                    min_mem_bytes / (1 << 30)
                ),
            ));
            continue;
        }
        // Sync `/execute` dispatches surface as active_leases (not jobs) —
        // count them too, or a node saturated by synchronous work looks idle.
        let load = heartbeat
            .active_jobs
            .saturating_add(heartbeat.queued_jobs)
            .saturating_add(heartbeat.active_leases)
            .saturating_add(reservations.get(&node.id).copied().unwrap_or(0));
        if load >= heartbeat.capacity_total.saturating_mul(2) {
            reasons.push((
                node.id.clone(),
                format!(
                    "busy ({load} jobs/leases in flight >= 2x capacity {})",
                    heartbeat.capacity_total
                ),
            ));
            continue;
        }
        eligible.push((load, heartbeat.mem_available_bytes, node.id.clone()));
    }
    eligible.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    eligible
        .into_iter()
        .next()
        .map(|(_, _, id)| id)
        .ok_or(PlacementError { reasons })
}

impl FleetMonitor {
    /// Capacity-aware auto placement over the configured nodes, using cached
    /// heartbeats and env thresholds (`REMOTE_NODE_MIN_DISK_GB`, default 20;
    /// `REMOTE_NODE_MIN_MEM_GB`, default 8). Fails closed with a per-node
    /// exclusion report when no node qualifies.
    pub fn place_auto(
        &self,
        settings: &RemoteNodeSettings,
        requirements: &[String],
    ) -> Result<String, PlacementError> {
        self.place_auto_with_reservations(settings, requirements, &HashMap::new())
    }

    /// Like [`Self::place_auto`], with accepted jobs that may not have reached
    /// the latest node heartbeat yet.
    pub fn place_auto_with_reservations(
        &self,
        settings: &RemoteNodeSettings,
        requirements: &[String],
        reservations: &HashMap<String, u32>,
    ) -> Result<String, PlacementError> {
        let statuses = self.statuses.read().unwrap_or_else(|e| e.into_inner());
        select_node_auto_with_reservations(
            &settings.nodes,
            &statuses,
            requirements,
            env_gb_bytes("REMOTE_NODE_MIN_DISK_GB", DEFAULT_MIN_DISK_GB),
            env_gb_bytes("REMOTE_NODE_MIN_MEM_GB", DEFAULT_MIN_MEM_GB),
            reservations,
        )
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
    pub lean_runtime_ready: Option<bool>,
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
            lean_runtime_ready: heartbeat.and_then(|h| h.lean_runtime_ready),
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
        Some(token) => {
            let probe_started_at = Utc::now();
            match client.heartbeat(node, &token).await {
                Ok(heartbeat) => {
                    fleet.record_heartbeat_for_probe(&node.id, heartbeat, probe_started_at)
                }
                Err(err) => fleet.record_miss(&node.id, err.to_string()),
            }
        }
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

    fn node_config(id: &str) -> RemoteNodeConfig {
        RemoteNodeConfig {
            id: id.to_string(),
            base_url: format!("http://{id}:3088"),
            token_env: "TOKEN".to_string(),
            labels: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cached_online(
        id: &str,
        labels: &[&str],
        disk_gb: u64,
        mem_gb: u64,
        capacity: u32,
        active: u32,
        queued: u32,
    ) -> CachedNodeStatus {
        let heartbeat: NodeHeartbeat = serde_json::from_value(serde_json::json!({
            "node_id": id,
            "online": true,
            "capacity_total": capacity,
            "capacity_available": capacity.saturating_sub(active),
            "active_leases": 0,
            "version": "test",
            "protocol_version": 2,
            "labels": labels,
            "mem_available_bytes": mem_gb * (1u64 << 30),
            "disk_available_bytes": disk_gb * (1u64 << 30),
            "active_jobs": active,
            "queued_jobs": queued,
        }))
        .unwrap();
        CachedNodeStatus {
            node_id: id.to_string(),
            status: RemoteNodeStatus::Online,
            consecutive_misses: 0,
            last_heartbeat: Some(heartbeat),
            last_seen: Some(Utc::now()),
            last_probe_started_at: Some(Utc::now()),
            last_error: None,
        }
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn place_auto_filters_by_status_labels_disk_mem_and_load() {
        let nodes = vec![
            node_config("offline"),
            node_config("unlabeled"),
            node_config("lowdisk"),
            node_config("lowmem"),
            node_config("busy"),
            node_config("good"),
        ];
        let mut statuses = HashMap::new();
        let mut offline = cached_online("offline", &["lean"], 100, 32, 2, 0, 0);
        offline.status = RemoteNodeStatus::Offline;
        statuses.insert("offline".to_string(), offline);
        statuses.insert(
            "unlabeled".to_string(),
            cached_online("unlabeled", &["docker"], 100, 32, 2, 0, 0),
        );
        statuses.insert(
            "lowdisk".to_string(),
            cached_online("lowdisk", &["lean"], 5, 32, 2, 0, 0),
        );
        statuses.insert(
            "lowmem".to_string(),
            cached_online("lowmem", &["lean"], 100, 2, 2, 0, 0),
        );
        // capacity 2 -> load ceiling is 4; 3 active + 1 queued = at ceiling.
        statuses.insert(
            "busy".to_string(),
            cached_online("busy", &["lean"], 100, 32, 2, 3, 1),
        );
        statuses.insert(
            "good".to_string(),
            cached_online("good", &["lean", "docker"], 100, 32, 2, 1, 0),
        );

        let requirements = vec!["lean".to_string()];
        let picked = select_node_auto(&nodes, &statuses, &requirements, 20 * GIB, 8 * GIB).unwrap();
        assert_eq!(picked, "good");

        // Remove the only good node: fail closed with one reason per node.
        let nodes_without_good = &nodes[..5];
        let err = select_node_auto(
            nodes_without_good,
            &statuses,
            &requirements,
            20 * GIB,
            8 * GIB,
        )
        .unwrap_err();
        assert_eq!(err.reasons.len(), 5);
        let reason_for = |id: &str| {
            err.reasons
                .iter()
                .find(|(node, _)| node == id)
                .map(|(_, reason)| reason.clone())
                .unwrap()
        };
        assert!(reason_for("offline").contains("not online"));
        assert!(reason_for("unlabeled").contains("missing label 'lean'"));
        assert!(reason_for("lowdisk").contains("low disk"));
        assert!(reason_for("lowmem").contains("low memory"));
        assert!(reason_for("busy").contains("busy"));
        let message = err.to_string();
        assert!(message.contains("no eligible remote node"));
        assert!(message.contains("lowdisk"));
    }

    #[test]
    fn place_auto_prefers_least_loaded_then_most_free_memory() {
        let nodes = vec![node_config("a"), node_config("b"), node_config("c")];
        let mut statuses = HashMap::new();
        statuses.insert(
            "a".to_string(),
            cached_online("a", &["lean"], 100, 64, 4, 2, 1),
        );
        statuses.insert(
            "b".to_string(),
            cached_online("b", &["lean"], 100, 16, 4, 1, 0),
        );
        statuses.insert(
            "c".to_string(),
            cached_online("c", &["lean"], 100, 48, 4, 1, 0),
        );
        let requirements = vec!["lean".to_string()];
        // b and c tie on load (1); c wins on more available memory.
        let picked = select_node_auto(&nodes, &statuses, &requirements, 20 * GIB, 8 * GIB).unwrap();
        assert_eq!(picked, "c");

        // Never-probed nodes are excluded (status Unknown), not crashed on.
        let unknown_nodes = vec![node_config("ghost")];
        let err = select_node_auto(
            &unknown_nodes,
            &HashMap::new(),
            &requirements,
            20 * GIB,
            8 * GIB,
        )
        .unwrap_err();
        assert!(err.reasons[0].1.contains("not online"));

        // Config labels back a heartbeat that reports none.
        let mut labeled_config = node_config("d");
        labeled_config.labels = Some(vec!["lean".to_string()]);
        let mut statuses = HashMap::new();
        statuses.insert("d".to_string(), cached_online("d", &[], 100, 32, 2, 0, 0));
        let picked = select_node_auto(
            &[labeled_config],
            &statuses,
            &requirements,
            20 * GIB,
            8 * GIB,
        )
        .unwrap();
        assert_eq!(picked, "d");

        // No requirements: any healthy node qualifies, even without labels.
        let mut statuses = HashMap::new();
        statuses.insert("e".to_string(), cached_online("e", &[], 100, 32, 2, 0, 0));
        let picked =
            select_node_auto(&[node_config("e")], &statuses, &[], 20 * GIB, 8 * GIB).unwrap();
        assert_eq!(picked, "e");
    }

    #[test]
    fn place_auto_rejects_a_lean_label_when_runtime_is_not_ready() {
        let node = node_config("missing-lake");
        let mut status = cached_online("missing-lake", &["lean"], 100, 32, 2, 0, 0);
        status
            .last_heartbeat
            .as_mut()
            .expect("heartbeat")
            .lean_runtime_ready = Some(false);
        let statuses = HashMap::from([("missing-lake".to_string(), status)]);

        let error = select_node_auto(&[node], &statuses, &["lean".to_string()], 20 * GIB, 8 * GIB)
            .unwrap_err();
        assert!(error.reasons[0].1.contains("Lake proxy missing"));

        // Compatibility: the readiness field is irrelevant to jobs that do
        // not request the Lean capability.
        assert_eq!(
            select_node_auto(
                &[node_config("missing-lake")],
                &statuses,
                &[],
                20 * GIB,
                8 * GIB,
            )
            .unwrap(),
            "missing-lake"
        );
    }

    #[test]
    fn placement_counts_core_side_reservations_before_heartbeat_catches_up() {
        let nodes = vec![node_config("a"), node_config("b")];
        let mut statuses = HashMap::new();
        statuses.insert(
            "a".to_string(),
            cached_online("a", &["lean"], 100, 64, 4, 0, 0),
        );
        statuses.insert(
            "b".to_string(),
            cached_online("b", &["lean"], 100, 32, 4, 0, 0),
        );
        let reservations = HashMap::from([("a".to_string(), 1)]);
        let picked = select_node_auto_with_reservations(
            &nodes,
            &statuses,
            &["lean".to_string()],
            20 * GIB,
            8 * GIB,
            &reservations,
        )
        .unwrap();
        assert_eq!(picked, "b");
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
