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
    RemoteNodeClient, RemoteNodeConfig, RemoteNodeError, RemoteNodeStatus, SubmitJobRequest,
    SCOPE_JOB_SUBMIT,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_remote_build))
        .route("/:job_id", get(get_remote_build))
}

/// Client-side cap on a waited build: poll every 3s for at most 2 hours.
const WAIT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const WAIT_MAX_POLLS: u32 = 2 * 60 * 60 / 3;

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
    /// Build cwd relative to the checkout root.
    #[serde(default)]
    pub cwd_rel: Option<String>,
    /// Build argv (`["lake", "build"]` etc.); validated on the node.
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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

async fn core_side_reservations(state: &AppState) -> HashMap<String, u32> {
    match crate::remote_node::job_ledger::load(&state.config.working_dir).await {
        Ok(handles) => {
            let mut reservations = HashMap::new();
            for handle in handles {
                let cached = state.fleet.get(&handle.node_id);
                let heartbeat_started_at = cached
                    .as_ref()
                    .and_then(|status| status.last_probe_started_at);
                let heartbeat_has_job_counters = cached
                    .as_ref()
                    .and_then(|status| status.last_heartbeat.as_ref())
                    .is_some_and(|heartbeat| {
                        heartbeat.protocol_version
                            >= crate::remote_node::protocol::NODE_PROTOCOL_VERSION
                    });
                if handle_needs_reservation(
                    &handle,
                    heartbeat_started_at,
                    heartbeat_has_job_counters,
                ) {
                    *reservations.entry(handle.node_id).or_insert(0) += 1;
                }
            }
            reservations
        }
        Err(error) => {
            tracing::warn!(?error, "remote job reservations could not be loaded");
            HashMap::new()
        }
    }
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
) -> Result<RemoteNodeConfig, (StatusCode, String)> {
    let settings = &state.config.remote_nodes;
    if !settings.enabled || settings.nodes.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "remote build nodes are not configured".to_string(),
        ));
    }
    if node_id.eq_ignore_ascii_case("auto") {
        let reservations = core_side_reservations(state).await;
        let first = state
            .fleet
            .place_auto_with_reservations(settings, requirements, &reservations);
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
                let reservations = core_side_reservations(state).await;
                state
                    .fleet
                    .place_auto_with_reservations(settings, requirements, &reservations)
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
    if requirements.iter().any(|requirement| requirement == "lean")
        && state
            .fleet
            .get(node_id)
            .and_then(|cached| cached.last_heartbeat)
            .and_then(|heartbeat| heartbeat.lean_runtime_ready)
            == Some(false)
    {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("remote node '{node_id}' has no executable Lake runtime"),
        ));
    }
    Ok(node)
}

fn submit_error_status(err: &RemoteNodeError) -> StatusCode {
    match err {
        RemoteNodeError::Rejected { status, .. }
            if (400..500).contains(status) && !matches!(*status, 401 | 403 | 408 | 429) =>
        {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
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
                        crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id)
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
                Ok(status)
                    if matches!(
                        status.state.as_str(),
                        "succeeded" | "failed" | "cancelled" | "lost"
                    ) =>
                {
                    state.fleet.record_outcome(DispatchOutcome {
                        mission_id,
                        node_id: node.id.clone(),
                        job_id: Some(job_id),
                        state: status.state,
                        exit_code: status.exit_code,
                        error: status.error,
                        started_at,
                        finished_at: Some(chrono::Utc::now()),
                    });
                    crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id).await;
                    return;
                }
                Err(error) if cancel_requested && error.is_not_found() => {
                    crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id).await;
                    return;
                }
                Ok(_) | Err(_) => {}
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
    // Every endpoint payload is a declarative Lean build. Callers may add
    // placement labels, but may not remove the runtime readiness gate by
    // sending an empty or unrelated requirements list.
    let requirements = normalized_lean_requirements(&req.requirements);

    // Serialize placement through tentative-handle persistence. Otherwise a
    // burst can select the same idle node before its next heartbeat.
    let placement_guard = placement_lock().lock().await;
    let node = match resolve_node(&state, &req.node_id, &requirements).await {
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
            },
            cwd_rel: req.cwd_rel.clone(),
            command: req.command.clone(),
            timeout_secs: req.timeout_secs,
            cache_key: None,
            artifacts: req.artifacts.clone(),
            env: Default::default(),
        },
    };

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
            kind: crate::remote_node::job_ledger::JobHandleKind::Tentative,
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
            // Transport outages and queue saturation remain 503 so the wrapper
            // can fall back locally. Node-side caller validation stays 4xx and
            // must not be disguised as a fleet outage.
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
            kind: crate::remote_node::job_ledger::JobHandleKind::RemoteBuild,
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
        if matches!(
            status.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "lost"
        ) {
            state.fleet.record_outcome(outcome(
                &status.state,
                status.exit_code,
                status.error.clone(),
                true,
            ));
            crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id).await;
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
) -> Result<Json<NodeJobStatus>, (StatusCode, String)> {
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
    let settings = &state.config.remote_nodes;
    let node = settings.node(&query.node_id).cloned().ok_or((
        StatusCode::BAD_REQUEST,
        format!("remote node '{}' is not configured", query.node_id),
    ))?;
    let shared_token = node_shared_token(&node)?;
    let status = RemoteNodeClient::default()
        .get_job(&node, &shared_token, job_id)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    // The capability token is mission-scoped: never leak another mission's
    // job status through it.
    if status.mission_id != query.mission_id {
        return Err((
            StatusCode::FORBIDDEN,
            "job does not belong to this mission".to_string(),
        ));
    }
    if matches!(
        status.state.as_str(),
        "succeeded" | "failed" | "cancelled" | "lost"
    ) {
        crate::remote_node::job_ledger::remove(&state.config.working_dir, job_id).await;
    }
    Ok(Json(status))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn tentative_handle_stays_reserved_across_overlapping_probe() {
        let now = chrono::Utc::now();
        let mut handle = crate::remote_node::job_ledger::JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id: Uuid::new_v4(),
            started_at: now,
            accepted_at: None,
            kind: crate::remote_node::job_ledger::JobHandleKind::Tentative,
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
    fn run_remote_build_wrapper_with_http_status(status: u16) -> std::process::ExitStatus {
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
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[cfg(unix)]
    #[test]
    fn wrapper_treats_host_capability_auth_failures_as_temporary() {
        for status in [401, 403, 503] {
            assert_eq!(
                run_remote_build_wrapper_with_http_status(status).code(),
                Some(75),
                "HTTP {status} should request local fallback"
            );
        }
        for status in [400, 422] {
            assert_eq!(
                run_remote_build_wrapper_with_http_status(status).code(),
                Some(1),
                "HTTP {status} remains a caller error"
            );
        }
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
