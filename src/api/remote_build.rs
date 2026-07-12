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

use std::sync::Arc;
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

fn should_reprobe_after_placement_failure(status: Option<&RemoteNodeStatus>) -> bool {
    !matches!(status, Some(RemoteNodeStatus::Online))
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
        let first = state.fleet.place_auto(settings, requirements);
        let picked = match first {
            Ok(picked) => picked,
            Err(initial_err) => {
                // The monitor can be disabled, its initial tick can race this
                // request, or a node can have recovered since a cached miss.
                // Re-probe missing and non-online nodes, then retry placement
                // once without masking capacity/label rejection from nodes
                // whose latest heartbeat still says they are online.
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
                state
                    .fleet
                    .place_auto(settings, requirements)
                    .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err.to_string()))?
            }
        };
        return settings.node(&picked).cloned().ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("placed node '{picked}' vanished from configuration"),
        ));
    }
    settings.node(node_id).cloned().ok_or((
        StatusCode::BAD_REQUEST,
        format!("remote node '{node_id}' is not configured"),
    ))
}

fn submit_error_status(err: &RemoteNodeError) -> StatusCode {
    match err {
        RemoteNodeError::Rejected { status, .. }
            if (400..500).contains(status) && !matches!(*status, 408 | 429) =>
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

    let node = match resolve_node(&state, &req.node_id, &req.requirements).await {
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
    let accepted = match client.submit_job(&node, &shared_token, &submit).await {
        Ok(accepted) => accepted,
        Err(err) => {
            // Transport outages and queue saturation remain 503 so the wrapper
            // can fall back locally. Node-side caller validation stays 4xx and
            // must not be disguised as a fleet outage.
            let status = submit_error_status(&err);
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

    if !req.wait {
        return (
            StatusCode::ACCEPTED,
            Json(RemoteBuildAcceptedResponse {
                job_id,
                node_id: node.id.clone(),
            }),
        )
            .into_response();
    }

    // Persist before entering the request-local poll loop. If the handler is
    // aborted or the API restarts, startup recovery can still observe the
    // accepted node job through to a terminal state instead of orphaning it.
    if let Err(err) = crate::remote_node::job_ledger::record(
        &state.config.working_dir,
        crate::remote_node::job_ledger::JobHandle {
            mission_id: req.mission_id,
            node_id: node.id.clone(),
            job_id,
            started_at,
            kind: crate::remote_node::job_ledger::JobHandleKind::RemoteBuild,
        },
    )
    .await
    {
        let _ = client.cancel_job(&node, &shared_token, job_id).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("remote build accepted but recovery handle could not be persisted: {err}"),
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
    let observer_state = Arc::clone(&state);
    let observer_node = node.clone();
    let observer_token = shared_token.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
            match RemoteNodeClient::default()
                .get_job(&observer_node, &observer_token, job_id)
                .await
            {
                Ok(status)
                    if matches!(
                        status.state.as_str(),
                        "succeeded" | "failed" | "cancelled" | "lost"
                    ) =>
                {
                    observer_state.fleet.record_outcome(DispatchOutcome {
                        mission_id: req.mission_id,
                        node_id: observer_node.id.clone(),
                        job_id: Some(job_id),
                        state: status.state,
                        exit_code: status.exit_code,
                        error: status.error,
                        started_at,
                        finished_at: Some(chrono::Utc::now()),
                    });
                    crate::remote_node::job_ledger::remove(
                        &observer_state.config.working_dir,
                        job_id,
                    )
                    .await;
                    return;
                }
                Ok(_) | Err(_) => {}
            }
        }
    });
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
        assert_eq!(
            submit_error_status(&RemoteNodeError::Request("offline".to_string())),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn failed_auto_placement_reprobes_every_non_online_cache_state() {
        assert!(should_reprobe_after_placement_failure(None));
        for status in [
            RemoteNodeStatus::Unknown,
            RemoteNodeStatus::Degraded,
            RemoteNodeStatus::Offline,
            RemoteNodeStatus::Disabled,
        ] {
            assert!(should_reprobe_after_placement_failure(Some(&status)));
        }
        assert!(!should_reprobe_after_placement_failure(Some(
            &RemoteNodeStatus::Online
        )));
    }
}
