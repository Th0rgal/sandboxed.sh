//! Remote lean-build dispatch endpoint.
//!
//! A harness inside a mission workspace calls `POST /api/remote-build` (over
//! the host veth link, like `POST /api/spark/offload`) to run a declarative
//! `lean_build` job on a remote runner node. The HOST holds the node bearer
//! tokens (`SANDBOXED_REMOTE_NODE_<ID>_TOKEN`), so workspaces never carry
//! them — the wrapper only holds a per-mission HMAC capability token.
//!
//! Flow: resolve a node (capacity-aware `place_auto` by default), mint a
//! `job:submit` lease, submit the `lean_build` job, and either poll it to
//! completion (`wait: true`, default) or return `{job_id, node_id}`
//! immediately. Responds `503` when remote nodes are unavailable or no node
//! qualifies, so the in-workspace `remote-lean-build` wrapper can fall back
//! to a local build (exit 75).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::routes::AppState;
use crate::remote_node::{
    ArtifactEntry, DispatchOutcome, JobPayload, JobSource, LeaseClaims, NodeJobStatus,
    RemoteNodeClient, RemoteNodeConfig, RemoteNodeError, RemoteNodeStatus, SourceBundle,
    SubmitJobRequest, SCOPE_JOB_SUBMIT,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_remote_build))
        .route("/:job_id", get(get_remote_build))
}

/// Client-side cap on a waited build: poll every 3s for at most 2 hours.
const WAIT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const WAIT_MAX_POLLS: u32 = 2 * 60 * 60 / 3;
const GIB: u64 = 1 << 30;
const DEFAULT_ESTIMATED_DISK_GB: u64 = 12;
const MAX_ESTIMATED_DISK_GB: u64 = 512;
const DEFAULT_NODE_MIN_DISK_GB: u64 = 20;
const DEFAULT_NODE_DISK_EMERGENCY_GB: u64 = 10;

fn env_gib(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .saturating_mul(GIB)
}

fn default_estimated_disk_bytes() -> u64 {
    env_gib("REMOTE_BUILD_ESTIMATED_DISK_GB", DEFAULT_ESTIMATED_DISK_GB)
}

fn required_node_disk_bytes(estimated_disk_bytes: u64) -> u64 {
    env_gib("REMOTE_NODE_MIN_DISK_GB", DEFAULT_NODE_MIN_DISK_GB).max(
        estimated_disk_bytes.saturating_add(env_gib(
            "REMOTE_NODE_DISK_EMERGENCY_GB",
            DEFAULT_NODE_DISK_EMERGENCY_GB,
        )),
    )
}

/// Secret used to sign the per-mission remote-build capability token. Same
/// source as the spark-offload token (`src/api/spark.rs`).
fn remote_build_secret() -> Option<String> {
    std::env::var("SANDBOXED_INTERNAL_ACTION_SECRET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("JWT_SECRET")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
}

#[derive(Debug, Serialize, Deserialize)]
struct RemoteBuildCapability {
    mission_id: Uuid,
    expires_at: i64,
}

fn remote_build_token_ttl_secs() -> i64 {
    std::env::var("REMOTE_BUILD_TOKEN_TTL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds >= 300)
        .unwrap_or(6 * 60 * 60)
}

/// Domain-separated, expiring HMAC capability (pure; unit-tested).
fn sign_remote_build_token(secret: &str, mission_id: Uuid, expires_at: i64) -> Option<String> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let claims = RemoteBuildCapability {
        mission_id,
        expires_at,
    };
    let payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).ok()?);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(b"remote-build:");
    mac.update(payload.as_bytes());
    Some(format!(
        "{payload}.{}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

/// Mint a per-mission, scope-bound capability token for remote builds. The
/// domain prefix (`remote-build:`) separates it from the spark-offload token
/// signed with the same secret, so neither can be replayed as the other.
/// Returns `None` when no signing secret is configured.
pub fn build_remote_build_token(mission_id: Uuid) -> Option<String> {
    sign_remote_build_token(
        &remote_build_secret()?,
        mission_id,
        chrono::Utc::now().timestamp() + remote_build_token_ttl_secs(),
    )
}

fn verify_remote_build_token_with_secret(
    secret: &str,
    mission_id: Uuid,
    token: &str,
    now: i64,
) -> bool {
    use base64::Engine;
    let Some((payload, signature)) = token.trim().split_once('.') else {
        return false;
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<RemoteBuildCapability>(&bytes) else {
        return false;
    };
    if claims.mission_id != mission_id || claims.expires_at < now {
        return false;
    }
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(b"remote-build:");
    mac.update(payload.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    super::auth::constant_time_eq(&expected, signature)
}

fn verify_remote_build_token(mission_id: Uuid, token: &str) -> bool {
    let Some(secret) = remote_build_secret() else {
        return false;
    };
    verify_remote_build_token_with_secret(
        &secret,
        mission_id,
        token,
        chrono::Utc::now().timestamp(),
    )
}

async fn mission_accepts_remote_build(state: &AppState, mission_id: Uuid) -> bool {
    for session in state.control.all_sessions().await {
        if let Ok(Some(mission)) = session.mission_store.get_mission(mission_id).await {
            return matches!(
                mission.status,
                super::control::MissionStatus::Active
                    | super::control::MissionStatus::Pending
                    | super::control::MissionStatus::WaitingBackground
            );
        }
    }
    match super::mission_workspace_gc::persisted_mission_status(state, mission_id).await {
        Ok(Some(status)) => matches!(
            status,
            super::control::MissionStatus::Active
                | super::control::MissionStatus::Pending
                | super::control::MissionStatus::WaitingBackground
        ),
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(mission_id = %mission_id, ?err, "remote-build persisted mission lookup failed");
            false
        }
    }
}

fn default_requirements() -> Vec<String> {
    vec!["lean".to_string()]
}

fn normalized_lean_requirements(requirements: &[String]) -> Vec<String> {
    let mut normalized = requirements.to_vec();
    if !normalized.iter().any(|requirement| requirement == "lean") {
        normalized.push("lean".to_string());
    }
    normalized
}

fn default_node_id() -> String {
    "auto".to_string()
}

async fn probe_explicit_lean_node(
    state: &AppState,
    node_id: &str,
    requirements: &[String],
) -> Result<(), (StatusCode, String)> {
    if node_id.eq_ignore_ascii_case("auto")
        || !requirements.iter().any(|requirement| requirement == "lean")
    {
        return Ok(());
    }
    let settings = &state.config.remote_nodes;
    if !settings.enabled || settings.nodes.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "remote build nodes are not configured".to_string(),
        ));
    }
    let node = settings.node(node_id).ok_or((
        StatusCode::BAD_REQUEST,
        format!("remote node '{node_id}' is not configured"),
    ))?;

    // This network I/O deliberately happens before the process-wide placement
    // mutex is acquired. A slow explicit node must not block unrelated auto
    // placement to healthy runners.
    let client = RemoteNodeClient::default();
    crate::remote_node::probe_node(&state.fleet, &client, node).await;
    let cached = state.fleet.get(node_id);
    if cached.as_ref().is_none_or(|cached| {
        cached.status != RemoteNodeStatus::Online || cached.last_heartbeat.is_none()
    }) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("remote node '{node_id}' readiness probe failed"),
        ));
    }
    if cached
        .and_then(|cached| cached.last_heartbeat)
        .and_then(|heartbeat| heartbeat.lean_runtime_ready)
        == Some(false)
    {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("remote node '{node_id}' has no executable Lake runtime"),
        ));
    }
    Ok(())
}

fn default_wait() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct RemoteBuildRequest {
    /// Mission this build belongs to; `token` must be the capability token
    /// minted for exactly this mission.
    pub mission_id: Uuid,
    pub token: String,
    /// Git clone/fetch URL; the node fetches it itself.
    pub repo: String,
    /// Full 40-char lowercase hex commit SHA.
    pub commit: String,
    /// Optional exact-head gate captured by the caller immediately before
    /// submission. A mismatch fails before placement, so an already-stale
    /// validation can never consume remote capacity.
    #[serde(default)]
    pub expected_head: Option<String>,
    /// Toolchain identity recorded in the durable receipt. The node still
    /// reads the pinned checkout's toolchain file as the execution authority.
    #[serde(default)]
    pub toolchain: Option<String>,
    /// Optional, bounded source overlay whose hashes are verified by the node
    /// before it is applied over the pinned commit.
    #[serde(default)]
    pub source_bundle: Option<SourceBundle>,
    /// Build cwd relative to the checkout root.
    #[serde(default)]
    pub cwd_rel: Option<String>,
    /// Build argv (`["lake", "build"]` etc.); validated on the node.
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Expected peak scratch use. Defaults to 12 GiB and is reserved during
    /// placement so simultaneous cold builds cannot overcommit one node.
    #[serde(default = "default_estimated_disk_bytes")]
    pub estimated_disk_bytes: u64,
    /// Node label requirements for auto placement.
    #[serde(default = "default_requirements")]
    pub requirements: Vec<String>,
    /// Explicit node id, or `"auto"` (default) for capacity-aware placement.
    #[serde(default = "default_node_id")]
    pub node_id: String,
    /// Wait for the build to finish (default). `false` returns
    /// `{job_id, node_id}` immediately; poll `GET /api/remote-build/:job_id`.
    #[serde(default = "default_wait")]
    pub wait: bool,
    /// Artifact patterns (relative to the checkout root) to digest after a
    /// successful build.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

#[derive(Serialize)]
struct RemoteBuildWaitResponse {
    exit_code: Option<i32>,
    state: String,
    duration_secs: u64,
    log_tail: String,
    node_id: String,
    job_id: Uuid,
    artifacts: Vec<ArtifactEntry>,
}

#[derive(Serialize)]
struct RemoteBuildAcceptedResponse {
    job_id: Uuid,
    node_id: String,
    repository: String,
    commit: String,
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    toolchain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_bundle_digest: Option<String>,
}

fn repository_identity(repo: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(repo) else {
        return repo.to_string();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

fn remote_job_identity(
    req: &RemoteBuildRequest,
) -> crate::remote_node::job_ledger::RemoteJobIdentity {
    crate::remote_node::job_ledger::RemoteJobIdentity {
        repository: repository_identity(&req.repo),
        commit: req.commit.clone(),
        command: req.command.clone(),
        toolchain: req.toolchain.clone(),
        source_bundle_digest: req
            .source_bundle
            .as_ref()
            .map(|bundle| bundle.manifest_sha256.clone()),
    }
}

fn validate_expected_head(commit: &str, expected_head: Option<&str>) -> Result<(), String> {
    match expected_head {
        Some(expected_head) if expected_head != commit => Err(format!(
            "STALE_VALIDATION_REQUEST: pinned commit {commit} does not match expected head {expected_head}"
        )),
        _ => Ok(()),
    }
}

fn should_reprobe_after_placement_failure(_status: Option<&RemoteNodeStatus>) -> bool {
    // A cached Online heartbeat can still contain stale load, disk, or memory
    // figures. Placement has already rejected every cached candidate, so one
    // fresh probe of every configured node is the authoritative retry.
    true
}

fn placement_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Default)]
struct CoreReservations {
    jobs: HashMap<String, u32>,
    disk_bytes: HashMap<String, u64>,
}

async fn core_side_reservations(state: &AppState) -> Result<CoreReservations, String> {
    match crate::remote_node::job_ledger::load(&state.config.working_dir).await {
        Ok(handles) => {
            let mut reservations = CoreReservations::default();
            for handle in handles {
                let disk = reservations
                    .disk_bytes
                    .entry(handle.node_id.clone())
                    .or_insert(0);
                *disk = disk.saturating_add(handle.disk_reservation_bytes);
                let cached = state.fleet.get(&handle.node_id);
                let heartbeat_started_at = cached
                    .as_ref()
                    .and_then(|status| status.last_probe_started_at);
                let heartbeat_has_job_counters = cached
                    .as_ref()
                    .and_then(|status| status.last_heartbeat.as_ref())
                    .is_some_and(|heartbeat| {
                        heartbeat_protocol_has_job_counters(heartbeat.protocol_version)
                    });
                if handle_needs_reservation(
                    &handle,
                    heartbeat_started_at,
                    heartbeat_has_job_counters,
                ) {
                    *reservations.jobs.entry(handle.node_id).or_insert(0) += 1;
                }
            }
            Ok(reservations)
        }
        Err(error) => Err(format!(
            "remote job reservations could not be loaded; disk-aware placement fails closed: {error}"
        )),
    }
}

fn heartbeat_protocol_has_job_counters(protocol_version: u32) -> bool {
    protocol_version >= crate::remote_node::protocol::NODE_JOB_COUNTER_PROTOCOL_VERSION
}

fn heartbeat_cannot_reflect(
    handle_started_at: chrono::DateTime<chrono::Utc>,
    heartbeat_started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    heartbeat_started_at.is_none_or(|started_at| handle_started_at > started_at)
}

fn handle_needs_reservation(
    handle: &crate::remote_node::job_ledger::JobHandle,
    heartbeat_started_at: Option<chrono::DateTime<chrono::Utc>>,
    heartbeat_has_job_counters: bool,
) -> bool {
    if !heartbeat_has_job_counters {
        return true;
    }
    handle
        .accepted_at
        .is_none_or(|accepted_at| heartbeat_cannot_reflect(accepted_at, heartbeat_started_at))
}

/// Resolve the target node: explicit id or capacity-aware auto placement.
/// All misses map to `503` so the wrapper can fall back to a local build —
/// except an explicitly named unknown node, which is a caller bug (`400`).
async fn resolve_node(
    state: &AppState,
    node_id: &str,
    requirements: &[String],
    min_disk_bytes: u64,
    min_protocol_version: u32,
) -> Result<RemoteNodeConfig, (StatusCode, String)> {
    let settings = &state.config.remote_nodes;
    if !settings.enabled || settings.nodes.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "remote build nodes are not configured".to_string(),
        ));
    }
    if node_id.eq_ignore_ascii_case("auto") {
        let reservations = core_side_reservations(state)
            .await
            .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?;
        let first = state
            .fleet
            .place_auto_with_protocol_and_resource_reservations(
                settings,
                requirements,
                min_disk_bytes,
                min_protocol_version,
                &reservations.jobs,
                &reservations.disk_bytes,
            );
        let picked = match first {
            Ok(picked) => picked,
            Err(initial_err) => {
                // The monitor can be disabled, its initial tick can race this
                // request, or a node can have recovered since a cached miss.
                // Re-probe every configured node, including cached Online
                // nodes whose load/resource figures may now be stale, then
                // retry placement once.
                let retry_nodes: Vec<_> = settings
                    .nodes
                    .iter()
                    .filter(|node| {
                        let cached = state.fleet.get(&node.id);
                        should_reprobe_after_placement_failure(
                            cached.as_ref().map(|cached| &cached.status),
                        )
                    })
                    .collect();
                if retry_nodes.is_empty() {
                    return Err((StatusCode::SERVICE_UNAVAILABLE, initial_err.to_string()));
                }
                let client = RemoteNodeClient::default();
                futures::future::join_all(
                    retry_nodes
                        .into_iter()
                        .map(|node| crate::remote_node::probe_node(&state.fleet, &client, node)),
                )
                .await;
                let reservations = core_side_reservations(state)
                    .await
                    .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?;
                state
                    .fleet
                    .place_auto_with_protocol_and_resource_reservations(
                        settings,
                        requirements,
                        min_disk_bytes,
                        min_protocol_version,
                        &reservations.jobs,
                        &reservations.disk_bytes,
                    )
                    .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?
            }
        };
        return settings.node(&picked).cloned().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("placed node '{picked}' vanished from configuration"),
        ));
    }
    let node = settings.node(node_id).cloned().ok_or((
        StatusCode::BAD_REQUEST,
        format!("remote node '{node_id}' is not configured"),
    ))?;
    let reservations = core_side_reservations(state)
        .await
        .map_err(|message| (StatusCode::SERVICE_UNAVAILABLE, message))?;
    let cached = state.fleet.get(&node.id).ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        format!("remote node '{}' has no heartbeat data", node.id),
    ))?;
    let heartbeat = cached.last_heartbeat.ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        format!("remote node '{}' has no heartbeat data", node.id),
    ))?;
    if heartbeat.protocol_version < min_protocol_version {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "remote node '{}' reports protocol v{}; v{} is required",
                node.id, heartbeat.protocol_version, min_protocol_version
            ),
        ));
    }
    let reserved = reservations.disk_bytes.get(&node.id).copied().unwrap_or(0);
    let effective = heartbeat.disk_available_bytes.saturating_sub(reserved);
    if effective < min_disk_bytes {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "remote node '{}' has {} GiB effective free after {} GiB reserved; {} GiB required",
                node.id,
                effective / GIB,
                reserved / GIB,
                min_disk_bytes / GIB
            ),
        ));
    }
    Ok(node)
}

fn submit_error_status(err: &RemoteNodeError) -> StatusCode {
    match err {
        // DNS/connect failures happen before an HTTP request reaches the node,
        // so no remote job can have been accepted and local fallback is safe.
        RemoteNodeError::Connect(_) => StatusCode::SERVICE_UNAVAILABLE,
        // A transport failure may happen after the node accepted the request.
        // The tentative ledger/observer will reconcile it, but the caller must
        // not start a duplicate local build in the meantime.
        RemoteNodeError::Request(_) => StatusCode::BAD_GATEWAY,
        RemoteNodeError::Rejected { status, .. }
            if (400..500).contains(status) && !matches!(*status, 401 | 403 | 408 | 429) =>
        {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn job_status_error_status(err: &RemoteNodeError) -> StatusCode {
    match err {
        // Preserve terminal lookup failures from the runner so a resumable
        // client can stop polling a handle that no longer exists. Keep node
        // authentication and transient/transport failures behind the core.
        RemoteNodeError::Rejected { status, .. } if matches!(*status, 400 | 404) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        _ => StatusCode::BAD_GATEWAY,
    }
}

fn node_shared_token(node: &RemoteNodeConfig) -> Result<String, (StatusCode, String)> {
    std::env::var(&node.token_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "remote node '{}' has no token in {}",
                node.id, node.token_env
            ),
        ))
}

fn remote_build_is_terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "lost")
}

/// Keep the fleet rollup aligned with the runner's latest state, not merely
/// the state returned by `POST /jobs`. Long jobs commonly transition from
/// `queued` to `running` before the next heartbeat, and the rollup is what
/// controllers use to explain current placement.
fn remote_build_dispatch_outcome(
    node_id: &str,
    status: &NodeJobStatus,
    started_at: chrono::DateTime<chrono::Utc>,
) -> DispatchOutcome {
    let terminal = remote_build_is_terminal(&status.state);
    DispatchOutcome {
        mission_id: status.mission_id,
        node_id: node_id.to_string(),
        job_id: Some(status.job_id),
        state: status.state.clone(),
        exit_code: status.exit_code,
        error: status.error.clone(),
        started_at,
        finished_at: terminal.then(chrono::Utc::now),
    }
}

fn record_remote_build_status(
    state: &AppState,
    node_id: &str,
    status: &NodeJobStatus,
    started_at: chrono::DateTime<chrono::Utc>,
) {
    state
        .fleet
        .record_outcome(remote_build_dispatch_outcome(node_id, status, started_at));
}

async fn finalize_remote_build_handle(
    working_dir: &std::path::Path,
    job_id: Uuid,
    state: &str,
    exit_status: Option<i32>,
) {
    if let Err(error) =
        crate::remote_node::job_ledger::finalize(working_dir, job_id, state, exit_status).await
    {
        tracing::warn!(%job_id, ?error, "remote build receipt finalization failed");
    }
}

async fn remote_build_started_at(state: &AppState, job_id: Uuid) -> chrono::DateTime<chrono::Utc> {
    if let Some(started_at) = state
        .fleet
        .recent_outcomes(usize::MAX)
        .into_iter()
        .find(|outcome| outcome.job_id == Some(job_id))
        .map(|outcome| outcome.started_at)
    {
        return started_at;
    }
    match crate::remote_node::job_ledger::load(&state.config.working_dir).await {
        Ok(handles) => handles
            .into_iter()
            .find(|handle| handle.job_id == job_id)
            .map(|handle| handle.started_at)
            .unwrap_or_else(chrono::Utc::now),
        Err(error) => {
            tracing::warn!(job_id = %job_id, ?error, "remote build start time could not be recovered");
            chrono::Utc::now()
        }
    }
}

fn spawn_remote_build_observer(
    state: Arc<AppState>,
    node: RemoteNodeConfig,
    shared_token: String,
    mission_id: Uuid,
    job_id: Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
    cancel_requested: bool,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
            let client = RemoteNodeClient::default();
            if cancel_requested {
                if let Err(error) = client.cancel_job(&node, &shared_token, job_id).await {
                    if error.is_not_found() {
                        finalize_remote_build_handle(
                            &state.config.working_dir,
                            job_id,
                            "lost",
                            None,
                        )
                        .await;
                        return;
                    }
                    tracing::warn!(
                        mission_id = %mission_id,
                        node_id = %node.id,
                        job_id = %job_id,
                        ?error,
                        "remote build cancellation failed; observer will retry"
                    );
                }
            }
            match client.get_job(&node, &shared_token, job_id).await {
                Ok(status) if remote_build_is_terminal(&status.state) => {
                    record_remote_build_status(&state, &node.id, &status, started_at);
                    finalize_remote_build_handle(
                        &state.config.working_dir,
                        job_id,
                        &status.state,
                        status.exit_code,
                    )
                    .await;
                    return;
                }
                Err(error) if cancel_requested && error.is_not_found() => {
                    finalize_remote_build_handle(&state.config.working_dir, job_id, "lost", None)
                        .await;
                    return;
                }
                Ok(status) => {
                    record_remote_build_status(&state, &node.id, &status, started_at);
                    if let Err(error) =
                        crate::remote_node::job_ledger::heartbeat(&state.config.working_dir, job_id)
                            .await
                    {
                        tracing::warn!(%job_id, ?error, "remote build heartbeat persistence failed");
                    }
                }
                Err(_) => {}
            }
        }
    });
}

async fn submit_remote_build(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoteBuildRequest>,
) -> axum::response::Response {
    // Auth: per-mission, scope-bound capability token (NOT the dashboard JWT
    // and NOT a node bearer token) — a leak only authorizes remote builds
    // for this one mission. Same trust model as the spark offload endpoint.
    if !verify_remote_build_token(req.mission_id, &req.token) {
        return (StatusCode::UNAUTHORIZED, "invalid remote build token").into_response();
    }
    if !mission_accepts_remote_build(&state, req.mission_id).await {
        return (
            StatusCode::FORBIDDEN,
            "remote builds require a live mission",
        )
            .into_response();
    }
    if req.command.is_empty() {
        return (StatusCode::BAD_REQUEST, "command argv required").into_response();
    }
    if let Err(message) = validate_expected_head(&req.commit, req.expected_head.as_deref()) {
        return (StatusCode::CONFLICT, message).into_response();
    }
    let max_estimated_disk_bytes =
        env_gib("REMOTE_BUILD_MAX_ESTIMATED_DISK_GB", MAX_ESTIMATED_DISK_GB);
    if req.estimated_disk_bytes == 0 || req.estimated_disk_bytes > max_estimated_disk_bytes {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "estimated_disk_bytes must be between 1 and {} GiB",
                max_estimated_disk_bytes / GIB
            ),
        )
            .into_response();
    }
    let min_disk_bytes = required_node_disk_bytes(req.estimated_disk_bytes);
    let min_protocol_version = if req.source_bundle.is_some() {
        crate::remote_node::protocol::NODE_PROTOCOL_VERSION
    } else {
        1
    };
    // Every endpoint payload is a declarative Lean build. Callers may add
    // placement labels, but may not remove the runtime readiness gate by
    // sending an empty or unrelated requirements list.
    let requirements = normalized_lean_requirements(&req.requirements);
    if let Err((status, message)) =
        probe_explicit_lean_node(&state, &req.node_id, &requirements).await
    {
        return (status, message).into_response();
    }

    // Serialize placement through tentative-handle persistence. Otherwise a
    // burst can select the same idle node before its next heartbeat.
    let placement_guard = placement_lock().lock().await;
    let node = match resolve_node(
        &state,
        &req.node_id,
        &requirements,
        min_disk_bytes,
        min_protocol_version,
    )
    .await
    {
        Ok(node) => node,
        Err((status, message)) => return (status, message).into_response(),
    };
    let shared_token = match node_shared_token(&node) {
        Ok(token) => token,
        Err((status, message)) => return (status, message).into_response(),
    };

    let job_id = Uuid::new_v4();
    let claims = LeaseClaims {
        mission_id: req.mission_id,
        node_id: node.id.clone(),
        scope: SCOPE_JOB_SUBMIT.to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(15)).timestamp(),
        job_id: Some(job_id),
    };
    let lease_token = match crate::remote_node::create_lease_token(&claims, &shared_token) {
        Ok(token) => token,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let submit = SubmitJobRequest {
        job_id,
        mission_id: req.mission_id,
        lease_token,
        payload: JobPayload::LeanBuild {
            source: JobSource {
                repo: req.repo.clone(),
                commit: req.commit.clone(),
                bundle: req.source_bundle.clone(),
            },
            cwd_rel: req.cwd_rel.clone(),
            command: req.command.clone(),
            timeout_secs: req.timeout_secs,
            estimated_disk_bytes: Some(req.estimated_disk_bytes),
            cache_key: None,
            artifacts: req.artifacts.clone(),
            env: Default::default(),
        },
    };

    let identity = remote_job_identity(&req);
    let client = RemoteNodeClient::default();
    let started_at = chrono::Utc::now();
    if let Err(error) = crate::remote_node::job_ledger::record(
        &state.config.working_dir,
        crate::remote_node::job_ledger::JobHandle {
            mission_id: req.mission_id,
            node_id: node.id.clone(),
            job_id,
            started_at,
            accepted_at: None,
            heartbeat_at: None,
            disk_reservation_bytes: req.estimated_disk_bytes,
            kind: crate::remote_node::job_ledger::JobHandleKind::Tentative,
            identity: Some(identity.clone()),
        },
    )
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("remote build recovery handle could not be prepared: {error}"),
        )
            .into_response();
    }
    drop(placement_guard);
    let accepted = match client.submit_job(&node, &shared_token, &submit).await {
        Ok(accepted) => accepted,
        Err(err) => {
            // Pre-connect outages and queue saturation remain 503 so the
            // wrapper can fall back locally. Response-side transport failures
            // are ambiguous and return 502 to prohibit duplicate local work.
            let status = submit_error_status(&err);
            if matches!(&err, RemoteNodeError::Request(_)) {
                spawn_remote_build_observer(
                    Arc::clone(&state),
                    node.clone(),
                    shared_token.clone(),
                    req.mission_id,
                    job_id,
                    started_at,
                    true,
                );
            } else {
                crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id).await;
            }
            return (
                status,
                format!("remote node '{}' did not accept the build: {err}", node.id),
            )
                .into_response();
        }
    };
    let outcome = |state: &str, exit_code: Option<i32>, error: Option<String>, terminal: bool| {
        DispatchOutcome {
            mission_id: req.mission_id,
            node_id: node.id.clone(),
            job_id: Some(job_id),
            state: state.to_string(),
            exit_code,
            error,
            started_at,
            finished_at: terminal.then(chrono::Utc::now),
        }
    };
    state
        .fleet
        .record_outcome(outcome(&accepted.state, None, None, false));
    tracing::info!(
        mission_id = %req.mission_id,
        node_id = %node.id,
        job_id = %job_id,
        wait = req.wait,
        "remote build dispatched"
    );

    // Persist every accepted build before either returning 202 or entering a
    // request-local wait. Startup recovery can therefore observe it through
    // terminal state even if the caller or API disappears.
    if let Err(err) = crate::remote_node::job_ledger::record(
        &state.config.working_dir,
        crate::remote_node::job_ledger::JobHandle {
            mission_id: req.mission_id,
            node_id: node.id.clone(),
            job_id,
            started_at,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: Some(chrono::Utc::now()),
            disk_reservation_bytes: req.estimated_disk_bytes,
            kind: crate::remote_node::job_ledger::JobHandleKind::RemoteBuild,
            identity: Some(identity.clone()),
        },
    )
    .await
    {
        // The tentative pre-submit entry remains the restart fallback, but it
        // cannot authorize normal observation of an accepted build. Cancel it
        // immediately and retry until the node confirms terminal state.
        spawn_remote_build_observer(
            Arc::clone(&state),
            node.clone(),
            shared_token.clone(),
            req.mission_id,
            job_id,
            started_at,
            true,
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("remote build accepted but recovery handle could not be persisted: {err}"),
        )
            .into_response();
    }

    if !req.wait {
        spawn_remote_build_observer(
            Arc::clone(&state),
            node.clone(),
            shared_token.clone(),
            req.mission_id,
            job_id,
            started_at,
            false,
        );
        return (
            StatusCode::ACCEPTED,
            Json(RemoteBuildAcceptedResponse {
                job_id,
                node_id: node.id.clone(),
                repository: identity.repository,
                commit: identity.commit,
                command: identity.command,
                toolchain: identity.toolchain,
                source_bundle_digest: identity.source_bundle_digest,
            }),
        )
            .into_response();
    }

    // Poll to completion (3s interval, capped at 2h client-side; the node
    // enforces its own SANDBOXED_NODE_MAX_JOB_SECS on the job itself).
    for _ in 0..WAIT_MAX_POLLS {
        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        let status = match client.get_job(&node, &shared_token, job_id).await {
            Ok(status) => status,
            Err(_) => continue, // transient poll failure; the job keeps running
        };
        record_remote_build_status(&state, &node.id, &status, started_at);
        if remote_build_is_terminal(&status.state) {
            finalize_remote_build_handle(
                &state.config.working_dir,
                job_id,
                &status.state,
                status.exit_code,
            )
            .await;
            let duration_secs = (chrono::Utc::now() - started_at).num_seconds().max(0) as u64;
            return Json(RemoteBuildWaitResponse {
                exit_code: status.exit_code,
                state: status.state,
                duration_secs,
                log_tail: status.log_tail.unwrap_or_default(),
                node_id: node.id.clone(),
                job_id,
                artifacts: status.artifacts,
            })
            .into_response();
        }
        if let Err(error) =
            crate::remote_node::job_ledger::heartbeat(&state.config.working_dir, job_id).await
        {
            tracing::warn!(%job_id, ?error, "remote build heartbeat persistence failed");
        }
    }
    state.fleet.record_outcome(outcome(
        "lost",
        None,
        Some("client-side wait cap (2h) exceeded".to_string()),
        true,
    ));
    spawn_remote_build_observer(
        Arc::clone(&state),
        node.clone(),
        shared_token.clone(),
        req.mission_id,
        job_id,
        started_at,
        false,
    );
    (
        StatusCode::GATEWAY_TIMEOUT,
        format!("remote build {job_id} on '{}' did not finish within the 2h wait cap; poll GET /api/remote-build/{job_id}?node_id={}", node.id, node.id),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct RemoteBuildStatusQuery {
    pub mission_id: Uuid,
    #[serde(default = "default_node_id")]
    pub node_id: String,
    /// Live head observed by a gate/controller. The node state remains
    /// unchanged, while `receipt_state` becomes `stale_success` when a green
    /// build validated a different immutable commit.
    #[serde(default)]
    pub expected_head: Option<String>,
}

#[derive(Serialize)]
struct RemoteBuildStatusResponse {
    #[serde(flatten)]
    status: NodeJobStatus,
    receipt_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_head_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation: Option<crate::remote_node::job_ledger::RemoteJobIdentity>,
}

fn remote_build_response_from_receipt(
    query: &RemoteBuildStatusQuery,
    receipt: crate::remote_node::job_ledger::RemoteJobReceipt,
) -> Result<Json<RemoteBuildStatusResponse>, (StatusCode, String)> {
    if receipt.mission_id != query.mission_id {
        return Err((
            StatusCode::FORBIDDEN,
            "job does not belong to this mission".to_string(),
        ));
    }
    let current_head_match = query
        .expected_head
        .as_deref()
        .map(|expected_head| receipt.identity.commit == expected_head);
    let receipt_state = if receipt.state == "succeeded" && current_head_match == Some(false) {
        "stale_success".to_string()
    } else {
        receipt.state.clone()
    };
    let status = NodeJobStatus {
        job_id: receipt.job_id,
        mission_id: receipt.mission_id,
        state: receipt.state,
        exit_code: receipt.exit_status,
        created_at: receipt.started_at.to_rfc3339(),
        started_at: Some(receipt.started_at.to_rfc3339()),
        finished_at: Some(receipt.finished_at.to_rfc3339()),
        error: None,
        log_tail: None,
        artifacts: Vec::new(),
    };
    Ok(Json(RemoteBuildStatusResponse {
        status,
        receipt_state,
        current_head_match,
        validation: Some(receipt.identity),
    }))
}

/// `GET /api/remote-build/:job_id?mission_id=...&node_id=...` —
/// job status for a build submitted with `wait: false`. Authenticated with
/// the same per-mission capability token in `Authorization: Bearer ...` so
/// credentials never enter request URLs or access logs.
async fn get_remote_build(
    State(state): State<Arc<AppState>>,
    Path(job_id): Path<Uuid>,
    Query(query): Query<RemoteBuildStatusQuery>,
    headers: HeaderMap,
) -> Result<Json<RemoteBuildStatusResponse>, (StatusCode, String)> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !verify_remote_build_token(query.mission_id, token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid remote build token".to_string(),
        ));
    }
    if query.node_id.eq_ignore_ascii_case("auto") {
        return Err((
            StatusCode::BAD_REQUEST,
            "node_id is required to poll a build".to_string(),
        ));
    }
    // Prefer the live node because it retains bounded logs and artifact
    // digests. Keep the durable receipt ready as the authority when the node
    // is offline, reconfigured, or has pruned its own job record.
    let persisted_terminal_receipt =
        crate::remote_node::job_ledger::terminal_receipt(&state.config.working_dir, job_id)
            .await
            .unwrap_or_default();
    let settings = &state.config.remote_nodes;
    let node = match settings.node(&query.node_id).cloned() {
        Some(node) => node,
        None => {
            if let Some(receipt) = persisted_terminal_receipt.clone() {
                return remote_build_response_from_receipt(&query, receipt);
            }
            return Err((
                StatusCode::BAD_REQUEST,
                format!("remote node '{}' is not configured", query.node_id),
            ));
        }
    };
    let shared_token = match node_shared_token(&node) {
        Ok(token) => token,
        Err(error) => {
            if let Some(receipt) = persisted_terminal_receipt.clone() {
                return remote_build_response_from_receipt(&query, receipt);
            }
            return Err(error);
        }
    };
    let status = match RemoteNodeClient::default()
        .get_job(&node, &shared_token, job_id)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            // Finalization can race this lookup. Recheck the durable receipt
            // before surfacing a transient/404 node failure.
            if let Some(receipt) = persisted_terminal_receipt.clone().or(
                crate::remote_node::job_ledger::terminal_receipt(&state.config.working_dir, job_id)
                    .await
                    .unwrap_or_default(),
            ) {
                return remote_build_response_from_receipt(&query, receipt);
            }
            return Err((job_status_error_status(&error), error.to_string()));
        }
    };
    // The capability token is mission-scoped: never leak another mission's
    // job status through it.
    if status.mission_id != query.mission_id {
        return Err((
            StatusCode::FORBIDDEN,
            "job does not belong to this mission".to_string(),
        ));
    }
    let handle = crate::remote_node::job_ledger::load(&state.config.working_dir)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|handle| handle.job_id == job_id);
    let terminal_receipt = if handle.is_none() {
        match persisted_terminal_receipt {
            Some(receipt) => Some(receipt),
            None => {
                crate::remote_node::job_ledger::terminal_receipt(&state.config.working_dir, job_id)
                    .await
                    .unwrap_or_default()
            }
        }
    } else {
        None
    };
    let validation = handle
        .as_ref()
        .and_then(|handle| handle.identity.clone())
        .or_else(|| {
            terminal_receipt
                .as_ref()
                .map(|receipt| receipt.identity.clone())
        });
    let current_head_match = query.expected_head.as_deref().and_then(|expected_head| {
        validation
            .as_ref()
            .map(|identity| identity.commit == expected_head)
    });
    let receipt_state = if status.state == "succeeded" && current_head_match == Some(false) {
        "stale_success".to_string()
    } else {
        status.state.clone()
    };
    let started_at = remote_build_started_at(&state, job_id).await;
    record_remote_build_status(&state, &node.id, &status, started_at);
    if remote_build_is_terminal(&status.state) {
        finalize_remote_build_handle(
            &state.config.working_dir,
            job_id,
            &status.state,
            status.exit_code,
        )
        .await;
    } else if let Err(error) =
        crate::remote_node::job_ledger::heartbeat(&state.config.working_dir, job_id).await
    {
        tracing::warn!(%job_id, ?error, "remote build heartbeat persistence failed");
    }
    Ok(Json(RemoteBuildStatusResponse {
        status,
        receipt_state,
        current_head_match,
        validation,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_job_status(state: &str) -> NodeJobStatus {
        NodeJobStatus {
            job_id: Uuid::new_v4(),
            mission_id: Uuid::new_v4(),
            state: state.to_string(),
            exit_code: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            started_at: None,
            finished_at: None,
            error: None,
            log_tail: None,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn polled_remote_build_state_is_visible_in_fleet_rollup() {
        let started_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        let running =
            remote_build_dispatch_outcome("federation", &node_job_status("running"), started_at);
        assert_eq!(running.state, "running");
        assert_eq!(running.node_id, "federation");
        assert_eq!(running.started_at, started_at);
        assert!(running.finished_at.is_none());

        let succeeded =
            remote_build_dispatch_outcome("federation", &node_job_status("succeeded"), started_at);
        assert_eq!(succeeded.state, "succeeded");
        assert!(succeeded.finished_at.is_some());
    }

    #[test]
    fn reservations_only_exclude_jobs_older_than_the_heartbeat_probe() {
        let heartbeat_started_at = chrono::Utc::now();

        assert!(!heartbeat_cannot_reflect(
            heartbeat_started_at - chrono::Duration::seconds(1),
            Some(heartbeat_started_at)
        ));
        assert!(heartbeat_cannot_reflect(
            heartbeat_started_at + chrono::Duration::seconds(1),
            Some(heartbeat_started_at)
        ));
        assert!(heartbeat_cannot_reflect(heartbeat_started_at, None));
    }

    #[test]
    fn v2_heartbeats_still_cover_job_count_reservations() {
        assert!(!heartbeat_protocol_has_job_counters(1));
        assert!(heartbeat_protocol_has_job_counters(2));
        assert!(heartbeat_protocol_has_job_counters(3));
    }

    #[test]
    fn tentative_handle_stays_reserved_across_overlapping_probe() {
        let now = chrono::Utc::now();
        let mut handle = crate::remote_node::job_ledger::JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id: Uuid::new_v4(),
            started_at: now,
            accepted_at: None,
            heartbeat_at: None,
            disk_reservation_bytes: 12 * GIB,
            kind: crate::remote_node::job_ledger::JobHandleKind::Tentative,
            identity: None,
        };

        assert!(handle_needs_reservation(
            &handle,
            Some(now + chrono::Duration::seconds(1)),
            true,
        ));

        handle.accepted_at = Some(now + chrono::Duration::seconds(2));
        assert!(handle_needs_reservation(
            &handle,
            Some(now + chrono::Duration::seconds(1)),
            true,
        ));
        assert!(!handle_needs_reservation(
            &handle,
            Some(now + chrono::Duration::seconds(3)),
            true,
        ));
        assert!(handle_needs_reservation(
            &handle,
            Some(now + chrono::Duration::seconds(3)),
            false,
        ));
    }

    #[cfg(unix)]
    fn run_remote_build_wrapper_with_submit_result(
        status: u16,
        curl_exit: u8,
    ) -> std::process::ExitStatus {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("create wrapper test directory");
        let repo = temp.path().join("repo");
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&repo).expect("create test repo");
        std::fs::create_dir_all(&bin).expect("create fake bin directory");

        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "Remote Build Test"]);
        git(&["config", "user.email", "remote-build@example.invalid"]);
        std::fs::write(repo.join("README.md"), "test\n").expect("write tracked file");
        git(&["add", "README.md"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "test",
        ]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ]);

        let fake_curl = bin.join("curl");
        std::fs::write(
            &fake_curl,
            r#"#!/bin/sh
output=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        output="$1"
    fi
    shift
done
printf '{}' > "$output"
[ "$REMOTE_BUILD_TEST_CURL_EXIT" = "0" ] || exit "$REMOTE_BUILD_TEST_CURL_EXIT"
printf '%s' "$REMOTE_BUILD_TEST_HTTP_STATUS"
"#,
        )
        .expect("write fake curl");
        let mut permissions = std::fs::metadata(&fake_curl)
            .expect("stat fake curl")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_curl, permissions).expect("make fake curl executable");

        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::process::Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/remote-lean-build"
            ))
            .current_dir(repo)
            .env("PATH", path)
            .env(
                "REMOTE_BUILD_URL",
                "http://example.invalid/api/remote-build",
            )
            .env("REMOTE_BUILD_TOKEN", "expired-test-token")
            .env("REMOTE_BUILD_MISSION_ID", Uuid::new_v4().to_string())
            .env("REMOTE_BUILD_TEST_HTTP_STATUS", status.to_string())
            .env("REMOTE_BUILD_TEST_CURL_EXIT", curl_exit.to_string())
            .status()
            .expect("run remote-build wrapper")
    }

    #[test]
    fn token_round_trips_and_is_mission_and_domain_scoped() {
        let mission = Uuid::new_v4();
        let other = Uuid::new_v4();
        let expires_at = chrono::Utc::now().timestamp() + 600;
        let token = sign_remote_build_token("secret", mission, expires_at).unwrap();
        assert_ne!(
            sign_remote_build_token("secret", other, expires_at).unwrap(),
            token
        );
        assert_ne!(
            sign_remote_build_token("other-secret", mission, expires_at).unwrap(),
            token
        );
        // Domain separation from the spark-offload token (same secret,
        // different prefix).
        // Spark: HMAC("spark-offload:" || mission); ours must differ.
        {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
            mac.update(b"spark-offload:");
            mac.update(mission.as_bytes());
            let spark_token = hex::encode(mac.finalize().into_bytes());
            assert_ne!(spark_token, token);
        }
    }

    #[test]
    fn token_expiry_is_enforced() {
        let mission = Uuid::new_v4();
        let now = chrono::Utc::now().timestamp();
        let token = sign_remote_build_token("secret", mission, now + 60).unwrap();
        let expired = sign_remote_build_token("secret", mission, now - 1).unwrap();
        assert!(verify_remote_build_token_with_secret(
            "secret", mission, &token, now
        ));
        assert!(!verify_remote_build_token_with_secret(
            "secret", mission, &expired, now
        ));
        assert!(!verify_remote_build_token_with_secret(
            "secret",
            Uuid::new_v4(),
            &token,
            now
        ));
        assert!(!verify_remote_build_token_with_secret(
            "wrong", mission, &token, now
        ));
    }

    #[test]
    fn immutable_identity_redacts_repository_credentials() {
        let req: RemoteBuildRequest = serde_json::from_value(serde_json::json!({
            "mission_id": Uuid::new_v4(),
            "token": "token",
            "repo": "https://secret-user:secret-token@example.invalid/org/repo.git?token=secret#fragment",
            "commit": "a".repeat(40),
            "command": ["lake", "build"],
            "toolchain": "leanprover/lean4:v4.19.0",
            "wait": false
        }))
        .unwrap();
        let identity = remote_job_identity(&req);

        assert_eq!(identity.repository, "https://example.invalid/org/repo.git");
        assert_eq!(identity.commit, "a".repeat(40));
        assert_eq!(identity.command, vec!["lake", "build"]);
        assert_eq!(
            identity.toolchain.as_deref(),
            Some("leanprover/lean4:v4.19.0")
        );
    }

    #[test]
    fn exact_head_gate_rejects_an_already_stale_request() {
        assert!(validate_expected_head(&"a".repeat(40), Some(&"a".repeat(40))).is_ok());
        let error = validate_expected_head(&"a".repeat(40), Some(&"b".repeat(40))).unwrap_err();
        assert!(error.starts_with("STALE_VALIDATION_REQUEST:"));
    }

    #[test]
    fn terminal_receipt_reports_stale_success_without_node_lookup() {
        let mission_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let started_at = chrono::Utc::now();
        let response = remote_build_response_from_receipt(
            &RemoteBuildStatusQuery {
                mission_id,
                node_id: "offline-node".to_string(),
                expected_head: Some("b".repeat(40)),
            },
            crate::remote_node::job_ledger::RemoteJobReceipt {
                mission_id,
                node_id: "offline-node".to_string(),
                job_id,
                started_at,
                finished_at: started_at,
                state: "succeeded".to_string(),
                exit_status: Some(0),
                identity: crate::remote_node::job_ledger::RemoteJobIdentity {
                    repository: "https://github.com/example/verity.git".to_string(),
                    commit: "a".repeat(40),
                    command: vec!["lake".to_string(), "build".to_string()],
                    toolchain: Some("leanprover/lean4:v4.19.0".to_string()),
                    source_bundle_digest: None,
                },
            },
        )
        .unwrap()
        .0;

        assert_eq!(response.status.job_id, job_id);
        assert_eq!(response.receipt_state, "stale_success");
        assert_eq!(response.current_head_match, Some(false));
        assert_eq!(
            response
                .validation
                .as_ref()
                .map(|identity| identity.commit.as_str()),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn request_deserialization_applies_defaults() {
        let mission = Uuid::new_v4();
        let req: RemoteBuildRequest = serde_json::from_value(serde_json::json!({
            "mission_id": mission,
            "token": "t",
            "repo": "https://github.com/example/verity.git",
            "commit": "a".repeat(40),
            "command": ["lake", "build"],
        }))
        .unwrap();
        assert_eq!(req.mission_id, mission);
        assert_eq!(req.node_id, "auto");
        assert!(req.wait);
        assert_eq!(req.requirements, vec!["lean".to_string()]);
        assert_eq!(req.cwd_rel, None);
        assert_eq!(req.timeout_secs, None);
        assert_eq!(req.estimated_disk_bytes, 12 * GIB);
        assert!(req.source_bundle.is_none());
        assert!(req.artifacts.is_empty());

        let req: RemoteBuildRequest = serde_json::from_value(serde_json::json!({
            "mission_id": mission,
            "token": "t",
            "repo": "https://x.git",
            "commit": "b".repeat(40),
            "command": ["lake", "build", "Verity"],
            "cwd_rel": "verity",
            "timeout_secs": 1200,
            "requirements": ["lean", "bigmem"],
            "node_id": "babylon",
            "wait": false,
            "artifacts": [".lake/build/lib/*"],
        }))
        .unwrap();
        assert_eq!(req.node_id, "babylon");
        assert!(!req.wait);
        assert_eq!(req.requirements, vec!["lean", "bigmem"]);
        assert_eq!(req.cwd_rel.as_deref(), Some("verity"));
        assert_eq!(req.timeout_secs, Some(1200));
        assert_eq!(req.artifacts, vec![".lake/build/lib/*"]);
    }

    #[test]
    fn lean_requirement_cannot_be_removed_by_request_overrides() {
        assert_eq!(normalized_lean_requirements(&[]), vec!["lean".to_string()]);
        assert_eq!(
            normalized_lean_requirements(&["high-memory".to_string()]),
            vec!["high-memory".to_string(), "lean".to_string()]
        );
        assert_eq!(
            normalized_lean_requirements(&["lean".to_string(), "gpu".to_string()]),
            vec!["lean".to_string(), "gpu".to_string()]
        );
    }

    #[test]
    fn submit_errors_preserve_caller_failures_only() {
        assert_eq!(
            submit_error_status(&RemoteNodeError::Rejected {
                status: 400,
                body: "invalid payload".to_string(),
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            submit_error_status(&RemoteNodeError::Rejected {
                status: 422,
                body: "invalid payload".to_string(),
            }),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            submit_error_status(&RemoteNodeError::Rejected {
                status: 429,
                body: "queue full".to_string(),
            }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        for status in [401, 403] {
            assert_eq!(
                submit_error_status(&RemoteNodeError::Rejected {
                    status,
                    body: "node token rejected".to_string(),
                }),
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
        assert_eq!(
            submit_error_status(&RemoteNodeError::Request("offline".to_string())),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            submit_error_status(&RemoteNodeError::Connect("offline".to_string())),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn job_status_errors_preserve_unrecoverable_lookup_failures_only() {
        for status in [400, 404] {
            assert_eq!(
                job_status_error_status(&RemoteNodeError::Rejected {
                    status,
                    body: "job not found".to_string(),
                }),
                StatusCode::from_u16(status).unwrap()
            );
        }
        for status in [401, 403, 429, 500, 503] {
            assert_eq!(
                job_status_error_status(&RemoteNodeError::Rejected {
                    status,
                    body: "runner failure".to_string(),
                }),
                StatusCode::BAD_GATEWAY
            );
        }
        assert_eq!(
            job_status_error_status(&RemoteNodeError::Connect("offline".to_string())),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            job_status_error_status(&RemoteNodeError::Request("offline".to_string())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_treats_host_capability_auth_failures_as_temporary() {
        for status in [401, 403, 503] {
            assert_eq!(
                run_remote_build_wrapper_with_submit_result(status, 0).code(),
                Some(75),
                "HTTP {status} should request local fallback"
            );
        }
        for status in [400, 422] {
            assert_eq!(
                run_remote_build_wrapper_with_submit_result(status, 0).code(),
                Some(1),
                "HTTP {status} remains a caller error"
            );
        }
        assert_eq!(
            run_remote_build_wrapper_with_submit_result(0, 28).code(),
            Some(1),
            "a submit timeout has an ambiguous outcome and must not request local fallback"
        );
        assert_eq!(
            run_remote_build_wrapper_with_submit_result(0, 56).code(),
            Some(1),
            "a response-side transport failure may follow acceptance and must not request fallback"
        );
        assert_eq!(
            run_remote_build_wrapper_with_submit_result(0, 7).code(),
            Some(75),
            "a pre-connect failure is safe for local fallback"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_sends_dirty_sources_as_a_hashed_bounded_bundle() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let bin = temp.path().join("bin");
        let capture = temp.path().join("request.json");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(repo.join("Theory")).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "Remote Build Test"]);
        git(&["config", "user.email", "remote-build@example.invalid"]);
        std::fs::write(repo.join("Theory/Proof.lean"), "old\n").unwrap();
        std::fs::write(repo.join("lean-toolchain"), "leanprover/lean4:v4.19.0\n").unwrap();
        git(&["add", "Theory/Proof.lean", "lean-toolchain"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ]);
        std::fs::write(repo.join("Theory/Proof.lean"), "new proof\n").unwrap();
        std::fs::write(repo.join("Theory/Witness.lean"), "new witness\n").unwrap();

        let fake_curl = bin.join("curl");
        std::fs::write(
            &fake_curl,
            r#"#!/bin/sh
output=""
data=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) shift; output="$1" ;;
        --data-binary) shift; data="$1" ;;
    esac
    shift
done
cp "${data#@}" "$REMOTE_BUILD_TEST_CAPTURE"
printf 'unavailable' > "$output"
printf '503'
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_curl).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_curl, permissions).unwrap();

        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = std::process::Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/remote-lean-build"
            ))
            .current_dir(repo.join("Theory"))
            .env("PATH", path)
            .env(
                "REMOTE_BUILD_URL",
                "http://example.invalid/api/remote-build",
            )
            .env("REMOTE_BUILD_TOKEN", "test-token")
            .env("REMOTE_BUILD_MISSION_ID", Uuid::new_v4().to_string())
            .env("REMOTE_BUILD_EXPECTED_HEAD", "a".repeat(40))
            .env("REMOTE_BUILD_TEST_CAPTURE", &capture)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(75));

        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(capture).unwrap()).unwrap();
        assert_eq!(
            request
                .get("estimated_disk_bytes")
                .and_then(serde_json::Value::as_u64),
            Some(12 * GIB)
        );
        assert_eq!(
            request.get("toolchain").and_then(serde_json::Value::as_str),
            Some("leanprover/lean4:v4.19.0")
        );
        assert_eq!(
            request
                .get("expected_head")
                .and_then(serde_json::Value::as_str),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let bundle = request.get("source_bundle").unwrap();
        let files = bundle.get("files").unwrap().as_array().unwrap();
        assert_eq!(
            files
                .iter()
                .map(|file| file.get("path").unwrap().as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["Theory/Proof.lean", "Theory/Witness.lean"]
        );
        let manifest = files
            .iter()
            .map(|file| {
                (
                    file.get("path").unwrap().as_str().unwrap().to_string(),
                    file.get("sha256").unwrap().as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bundle.get("manifest_sha256").unwrap().as_str().unwrap(),
            crate::node::lean::bundle_manifest_sha256(&manifest)
        );
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_resumes_an_interrupted_async_job_without_resubmitting() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("create wrapper test directory");
        let repo = temp.path().join("repo");
        let bin = temp.path().join("bin");
        let state = temp.path().join("state");
        let submit_count = temp.path().join("submit-count");
        std::fs::create_dir_all(&repo).expect("create test repo");
        std::fs::create_dir_all(&bin).expect("create fake bin directory");

        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "Remote Build Test"]);
        git(&["config", "user.email", "remote-build@example.invalid"]);
        std::fs::write(repo.join("README.md"), "test\n").expect("write tracked file");
        git(&["add", "README.md"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "test",
        ]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ]);

        let fake_curl = bin.join("curl");
        std::fs::write(
            &fake_curl,
            r#"#!/bin/sh
output=""
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) shift; output="$1" ;;
        http://*) url="$1" ;;
    esac
    shift
done
case "$url" in
    */api/remote-build/*)
        if [ "$REMOTE_BUILD_TEST_POLL_MODE" = "fail" ]; then
            exit 7
        fi
        if [ "$REMOTE_BUILD_TEST_POLL_MODE" = "stale" ]; then
            case "$url" in
                *expected_head=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb*) ;;
                *) exit 22 ;;
            esac
            printf '{"job_id":"11111111-1111-1111-1111-111111111111","mission_id":"%s","state":"succeeded","receipt_state":"stale_success","current_head_match":false,"exit_code":0,"created_at":"2026-07-15T20:00:00Z","started_at":"2026-07-15T20:00:01Z","finished_at":"2026-07-15T20:00:03Z","error":null,"log_tail":"remote ok\\n","artifacts":[]}' "$REMOTE_BUILD_TEST_MISSION_ID" > "$output"
        else
            printf '{"job_id":"11111111-1111-1111-1111-111111111111","mission_id":"%s","state":"succeeded","receipt_state":"succeeded","current_head_match":true,"exit_code":0,"created_at":"2026-07-15T20:00:00Z","started_at":"2026-07-15T20:00:01Z","finished_at":"2026-07-15T20:00:03Z","error":null,"log_tail":"remote ok\\n","artifacts":[]}' "$REMOTE_BUILD_TEST_MISSION_ID" > "$output"
        fi
        printf '200'
        ;;
    *)
        if [ "$REMOTE_BUILD_TEST_POLL_MODE" = "persist-fail" ]; then
            chmod 500 "$REMOTE_BUILD_TEST_STATE_DIR"
        fi
        count=0
        [ ! -f "$REMOTE_BUILD_TEST_SUBMIT_COUNT" ] || count=$(cat "$REMOTE_BUILD_TEST_SUBMIT_COUNT")
        count=$((count + 1))
        printf '%s' "$count" > "$REMOTE_BUILD_TEST_SUBMIT_COUNT"
        if [ "$REMOTE_BUILD_TEST_POLL_MODE" = "ambiguous" ]; then
            exit 28
        fi
        printf '{"job_id":"11111111-1111-1111-1111-111111111111","node_id":"lean:gpu"}' > "$output"
        printf '202'
        ;;
esac
"#,
        )
        .expect("write fake curl");
        let mut permissions = std::fs::metadata(&fake_curl)
            .expect("stat fake curl")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_curl, permissions).expect("make fake curl executable");

        let mission_id = Uuid::new_v4().to_string();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let command = |poll_mode: &str| {
            let mut command = std::process::Command::new("bash");
            command
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/scripts/remote-lean-build"
                ))
                .current_dir(&repo)
                .env("PATH", &path)
                .env(
                    "REMOTE_BUILD_URL",
                    "http://example.invalid/api/remote-build",
                )
                .env("REMOTE_BUILD_TOKEN", "test-token")
                .env("REMOTE_BUILD_MISSION_ID", &mission_id)
                .env("REMOTE_BUILD_STATE_DIR", &state)
                .env("REMOTE_BUILD_TEST_STATE_DIR", &state)
                .env("REMOTE_BUILD_POLL_SECS", "1")
                .env("REMOTE_BUILD_CLIENT_TIMEOUT_SECS", "1")
                .env("REMOTE_BUILD_TEST_POLL_MODE", poll_mode)
                .env("REMOTE_BUILD_TEST_MISSION_ID", &mission_id)
                .env("REMOTE_BUILD_TEST_SUBMIT_COUNT", &submit_count);
            command
        };
        let run = |poll_mode: &str| {
            command(poll_mode)
                .output()
                .expect("run remote-build wrapper")
        };

        let first = command("fail")
            .spawn()
            .expect("start first concurrent wrapper");
        let second = command("fail")
            .spawn()
            .expect("start second concurrent wrapper");
        for interrupted in [
            first.wait_with_output().expect("wait for first wrapper"),
            second.wait_with_output().expect("wait for second wrapper"),
        ] {
            assert_eq!(
                interrupted.status.code(),
                Some(1),
                "an accepted remote job must never request local fallback: stdout={} stderr={}",
                String::from_utf8_lossy(&interrupted.stdout),
                String::from_utf8_lossy(&interrupted.stderr)
            );
        }
        assert_eq!(std::fs::read_to_string(&submit_count).unwrap(), "1");
        let receipts = std::fs::read_dir(&state)
            .expect("read receipt directory")
            .filter(|entry| {
                entry
                    .as_ref()
                    .ok()
                    .and_then(|entry| entry.path().extension().map(|ext| ext == "json"))
                    .unwrap_or(false)
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("collect receipts");
        assert_eq!(receipts.len(), 1);
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(receipts[0].path()).expect("read persisted receipt"),
        )
        .expect("parse persisted receipt");
        assert!(receipt.get("token").is_none());
        assert!(receipt.get("repo").is_none());
        assert!(receipt.get("wait").is_none());
        assert_eq!(
            receipt.get("job_id").and_then(serde_json::Value::as_str),
            Some("11111111-1111-1111-1111-111111111111")
        );

        let resumed = run("success");
        assert!(
            resumed.status.success(),
            "resume failed: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert_eq!(std::fs::read_to_string(&submit_count).unwrap(), "1");
        assert!(String::from_utf8_lossy(&resumed.stderr).contains("resuming job"));
        assert_eq!(String::from_utf8_lossy(&resumed.stdout), "remote ok\n");

        let replayed = run("fail");
        assert!(
            replayed.status.success(),
            "terminal receipt replay failed: {}",
            String::from_utf8_lossy(&replayed.stderr)
        );
        assert_eq!(std::fs::read_to_string(&submit_count).unwrap(), "1");
        assert!(String::from_utf8_lossy(&replayed.stderr).contains("replaying terminal job"));
        assert_eq!(String::from_utf8_lossy(&replayed.stdout), "remote ok\n");

        let stale = command("stale")
            .env("REMOTE_BUILD_EXPECTED_HEAD", "b".repeat(40))
            .output()
            .expect("run stale exact-head validation");
        assert_eq!(stale.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&stale.stderr).contains("validation is stale"));
        assert_eq!(std::fs::read_to_string(&submit_count).unwrap(), "1");

        for receipt in std::fs::read_dir(&state).expect("read receipts before persistence failure")
        {
            std::fs::remove_file(receipt.expect("read receipt entry").path())
                .expect("remove prior receipt");
        }
        let persistence_failed = run("persist-fail");
        if state.is_file() {
            std::fs::remove_file(&state).expect("remove persistence-failure sentinel");
            std::fs::create_dir(&state).expect("restore state directory");
        }
        let mut state_permissions = std::fs::metadata(&state)
            .expect("stat state directory after persistence failure")
            .permissions();
        state_permissions.set_mode(0o700);
        std::fs::set_permissions(&state, state_permissions)
            .expect("restore state directory permissions");
        assert_eq!(
            persistence_failed.status.code(),
            Some(1),
            "unexpected persistence-failure result: stdout={} stderr={}",
            String::from_utf8_lossy(&persistence_failed.stdout),
            String::from_utf8_lossy(&persistence_failed.stderr)
        );
        assert!(String::from_utf8_lossy(&persistence_failed.stderr)
            .contains("failed to persist its receipt"));
        let persistence_retry = run("success");
        assert_eq!(persistence_retry.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&persistence_retry.stderr)
            .contains("previous submission stopped"));
        assert_eq!(
            std::fs::read_to_string(&submit_count).unwrap(),
            "2",
            "the accepted persistence-failure submission must block an identical retry"
        );
        let forced_after_reconciliation = command("success")
            .env("REMOTE_BUILD_FORCE_NEW", "1")
            .output()
            .expect("force a new build after explicit reconciliation");
        assert!(
            forced_after_reconciliation.status.success(),
            "explicitly forced build failed: {}",
            String::from_utf8_lossy(&forced_after_reconciliation.stderr)
        );
        assert_eq!(std::fs::read_to_string(&submit_count).unwrap(), "3");

        for entry in std::fs::read_dir(&state).expect("read state before ambiguous submission") {
            let path = entry.expect("read state entry").path();
            if path.is_dir() {
                std::fs::remove_dir_all(path).expect("remove stale test lock directory");
            } else {
                std::fs::remove_file(path).expect("remove prior test receipt");
            }
        }
        std::fs::write(&submit_count, "0").expect("reset submit counter");
        let ambiguous = run("ambiguous");
        assert_eq!(ambiguous.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("unknown outcome"));
        let blocked_retry = run("success");
        assert_eq!(blocked_retry.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&blocked_retry.stderr).contains("previous submission"));
        assert_eq!(
            std::fs::read_to_string(&submit_count).unwrap(),
            "1",
            "an ambiguous first submit must block an identical retry"
        );
        let ambiguous_receipt = std::fs::read_dir(&state)
            .expect("read ambiguous receipt directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .expect("ambiguous receipt");
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(ambiguous_receipt).expect("read ambiguous receipt"),
        )
        .expect("parse ambiguous receipt");
        assert_eq!(
            receipt.get("state").and_then(serde_json::Value::as_str),
            Some("ambiguous")
        );
        assert!(receipt.get("token").is_none());
        assert!(receipt.get("repo").is_none());
    }

    #[test]
    fn failed_auto_placement_reprobes_every_cache_state() {
        assert!(should_reprobe_after_placement_failure(None));
        for status in [
            RemoteNodeStatus::Unknown,
            RemoteNodeStatus::Degraded,
            RemoteNodeStatus::Offline,
            RemoteNodeStatus::Disabled,
        ] {
            assert!(should_reprobe_after_placement_failure(Some(&status)));
        }
        assert!(should_reprobe_after_placement_failure(Some(
            &RemoteNodeStatus::Online
        )));
    }
}
