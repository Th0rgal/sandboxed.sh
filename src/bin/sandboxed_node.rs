use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use sandboxed_sh::node::{
    read_log_tail, JobRecord, JobRunner, JobStore, NodeQueueFull, DEFAULT_MAX_JOB_SECS,
};
use sandboxed_sh::remote_node::{
    bearer_token, node_token_matches, parse_labels, run_lease_command, validate_lease_token,
    CancelJobResponse, ExecuteResponse, LeaseClaims, LeaseRequest, NodeHeartbeat, NodeJobStatus,
    RemoteNodeError, SubmitJobRequest, SubmitJobResponse, NODE_PROTOCOL_VERSION, SCOPE_JOB_SUBMIT,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

struct NodeState {
    node_id: String,
    shared_token: String,
    /// Optional previous token accepted during rotation
    /// (`SANDBOXED_NODE_TOKEN_PREVIOUS`).
    previous_token: Option<String>,
    labels: Vec<String>,
    work_root: PathBuf,
    capacity_total: u32,
    /// Shared permits for every process the node starts, whether it came from
    /// synchronous `/execute` or the async job API.
    admission: Arc<Semaphore>,
    active_leases: AtomicU32,
    jobs: JobStore,
    runner: Arc<JobRunner>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if sandboxed_sh::node::maybe_exec_cleared_scope_payload()? {
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sandboxed_sh=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    #[cfg(unix)]
    {
        let running_as_root = unsafe { libc::geteuid() == 0 };
        let allow_root = std::env::var("SANDBOXED_NODE_ALLOW_ROOT")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if running_as_root && !allow_root {
            anyhow::bail!(
                "sandboxed-node refuses to run as root; use the dedicated sandboxed-node service account (SANDBOXED_NODE_ALLOW_ROOT=1 is an emergency-only override)"
            );
        }
    }

    let node_id = std::env::var("SANDBOXED_NODE_ID").unwrap_or_else(|_| "local-node".to_string());
    let shared_token = std::env::var("SANDBOXED_NODE_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("SANDBOXED_NODE_TOKEN must be set"))?;
    let previous_token = std::env::var("SANDBOXED_NODE_TOKEN_PREVIOUS")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let labels = std::env::var("SANDBOXED_NODE_LABELS")
        .map(|raw| parse_labels(&raw))
        .unwrap_or_default();
    // Default to loopback: reaching the node over a network requires an
    // explicit SANDBOXED_NODE_BIND (e.g. a tailscale interface IP), so a
    // node is never exposed on all interfaces by accident.
    let bind = std::env::var("SANDBOXED_NODE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3088".to_string())
        .parse::<SocketAddr>()?;
    let work_root = std::env::var("SANDBOXED_NODE_WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/sandboxed-node/work"));
    let capacity_total = match std::env::var("SANDBOXED_NODE_CAPACITY") {
        Ok(raw) => raw
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|capacity| *capacity > 0)
            .ok_or_else(|| anyhow::anyhow!("SANDBOXED_NODE_CAPACITY must be a positive integer"))?,
        Err(_) => 1,
    };
    let max_job_secs = std::env::var("SANDBOXED_NODE_MAX_JOB_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_JOB_SECS);
    tokio::fs::create_dir_all(&work_root).await?;

    // Durable job store + runner. Jobs left in flight by a previous process
    // lifetime are flipped to `lost` before we accept new work.
    let jobs = JobStore::open(&work_root).await?;
    let recovered = jobs.recover_on_start().await?;
    if recovered > 0 {
        warn!("marked {recovered} in-flight job(s) from a previous run as lost");
    }
    let admission = Arc::new(Semaphore::new(capacity_total as usize));
    let runner = JobRunner::spawn_with_admission(
        jobs.clone(),
        work_root.clone(),
        capacity_total,
        max_job_secs,
        Arc::clone(&admission),
    );

    // Periodic disk GC for lean-build checkouts and lake cache slots
    // (SANDBOXED_NODE_MIN_FREE_GB, default 10).
    sandboxed_sh::node::spawn_cache_gc(work_root.clone());

    let state = Arc::new(NodeState {
        node_id,
        shared_token,
        previous_token,
        labels,
        work_root,
        capacity_total,
        admission,
        active_leases: AtomicU32::new(0),
        jobs,
        runner,
    });
    let app = Router::new()
        .route("/heartbeat", get(heartbeat))
        .route("/execute", post(execute))
        // Private-source jobs carry bounded payloads before base64 encoding,
        // so this route needs the same bounded wire allowance as the core
        // `/api/remote-build` endpoint.
        .route(
            "/jobs",
            post(submit_job)
                .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
                .get(list_jobs),
        )
        .route("/jobs/:id", get(get_job))
        .route("/jobs/:id/cancel", post(cancel_job))
        .with_state(state);

    info!("starting sandboxed-node on {bind}");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn check_auth(headers: &HeaderMap, state: &NodeState) -> Result<(), (StatusCode, String)> {
    match bearer_token(headers) {
        Some(token)
            if node_token_matches(token, &state.shared_token, state.previous_token.as_deref()) =>
        {
            Ok(())
        }
        _ => Err((StatusCode::UNAUTHORIZED, "invalid node token".to_string())),
    }
}

/// Host resource figures reported in heartbeat v2.
struct HostResources {
    cpu_total: u32,
    mem_total_bytes: u64,
    mem_available_bytes: u64,
    disk_total_bytes: u64,
    disk_available_bytes: u64,
}

/// Snapshot CPU/memory/disk of the host. Disk figures come from the
/// filesystem backing `work_root` (longest matching mount point), falling
/// back to `/` or the largest visible filesystem.
fn host_resources(work_root: &Path) -> HostResources {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let cpu_total = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(0);

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64, u64)> = None; // (mount depth, total, available)
    let mut largest: Option<(u64, u64)> = None;
    for disk in disks.list() {
        let total = disk.total_space();
        let available = disk.available_space();
        if work_root.starts_with(disk.mount_point()) {
            let depth = disk.mount_point().components().count();
            if best.map(|(d, _, _)| depth > d).unwrap_or(true) {
                best = Some((depth, total, available));
            }
        }
        if largest.map(|(t, _)| total > t).unwrap_or(true) {
            largest = Some((total, available));
        }
    }
    let (disk_total_bytes, disk_available_bytes) = best
        .map(|(_, total, available)| (total, available))
        .or(largest)
        .unwrap_or((0, 0));

    HostResources {
        cpu_total,
        mem_total_bytes: sys.total_memory(),
        mem_available_bytes: sys.available_memory(),
        disk_total_bytes,
        disk_available_bytes,
    }
}

async fn heartbeat(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
) -> Result<Json<NodeHeartbeat>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    let active_leases = state.active_leases.load(Ordering::Acquire);
    let resources = host_resources(&state.work_root);
    let lean_runtime_ready = sandboxed_sh::node::lean_runtime_ready(&state.work_root);
    let labels = advertised_labels(&state.labels, lean_runtime_ready);
    Ok(Json(NodeHeartbeat {
        node_id: state.node_id.clone(),
        online: true,
        capacity_total: state.capacity_total,
        // This is only a monitoring snapshot; the shared semaphore above is
        // the atomic enforcement point.
        capacity_available: state.admission.available_permits() as u32,
        active_leases,
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: NODE_PROTOCOL_VERSION,
        labels,
        cpu_total: resources.cpu_total,
        mem_total_bytes: resources.mem_total_bytes,
        mem_available_bytes: resources.mem_available_bytes,
        disk_total_bytes: resources.disk_total_bytes,
        disk_available_bytes: resources.disk_available_bytes,
        active_jobs: state.runner.active_count(),
        queued_jobs: state.runner.queued_count(),
        cached_toolchains: sandboxed_sh::node::cached_toolchains(&state.work_root),
        lean_runtime_ready: Some(lean_runtime_ready),
    }))
}

fn advertised_labels(configured: &[String], lean_runtime_ready: bool) -> Vec<String> {
    let mut labels = configured.to_vec();
    if !lean_runtime_ready {
        labels.retain(|label| label != "lean");
    }
    labels
}

/// Signing secrets accepted for lease validation: the current token plus,
/// during rotation, the previous one.
fn lease_secrets(state: &NodeState) -> Vec<&str> {
    let mut secrets = vec![state.shared_token.as_str()];
    if let Some(previous) = state.previous_token.as_deref() {
        secrets.push(previous);
    }
    secrets
}

/// Validate a lease token against any accepted signing secret, returning the
/// claims and the secret that validated it.
fn validate_lease_any<'a>(
    state: &'a NodeState,
    token: &str,
    scope: &str,
) -> Result<(LeaseClaims, &'a str), RemoteNodeError> {
    let now = chrono::Utc::now();
    let mut last_err = RemoteNodeError::InvalidLease("no signing secret".to_string());
    for secret in lease_secrets(state) {
        match validate_lease_token(token, secret, &state.node_id, scope, now) {
            Ok(claims) => return Ok((claims, secret)),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

async fn execute(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    // Leases may be signed with the previous token during rotation; pick the
    // secret that validates (run_lease_command re-validates internally).
    let signing_secret = validate_lease_any(
        &state,
        &request.lease_token,
        sandboxed_sh::remote_node::SCOPE_MISSION_EXECUTE,
    )
    .map(|(_, secret)| secret.to_string())
    .unwrap_or_else(|_| state.shared_token.clone());
    // Do not turn an overloaded node into an unbounded set of concurrent
    // child processes. `try_acquire_owned` is atomic and shares permits with
    // async jobs, so both APIs fail closed at the same capacity boundary.
    let _permit = state.admission.clone().try_acquire_owned().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "node capacity exhausted".to_string(),
        )
    })?;
    state.active_leases.fetch_add(1, Ordering::AcqRel);
    let result = run_lease_command(
        &state.node_id,
        &signing_secret,
        state.work_root.clone(),
        request,
    )
    .await;
    state.active_leases.fetch_sub(1, Ordering::AcqRel);
    match result {
        Ok(response) => Ok(Json(response)),
        Err(err) => {
            warn!("lease execution rejected: {err}");
            Err((StatusCode::BAD_REQUEST, err.to_string()))
        }
    }
}

fn job_status_from_record(record: &JobRecord, log_tail: Option<String>) -> NodeJobStatus {
    NodeJobStatus {
        job_id: record.id,
        mission_id: record.mission_id,
        state: record.state.as_str().to_string(),
        exit_code: record.exit_code,
        created_at: record.created_at.clone(),
        started_at: record.started_at.clone(),
        finished_at: record.finished_at.clone(),
        error: record.error.clone(),
        log_tail,
        artifacts: record
            .artifacts_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default(),
    }
}

/// `POST /jobs` — queue an async job. Requires the shared bearer token plus a
/// per-job lease scoped to `job:submit` and bound to the mission (and job id
/// when present in the claims).
async fn submit_job(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    Json(request): Json<SubmitJobRequest>,
) -> Result<(StatusCode, Json<SubmitJobResponse>), (StatusCode, String)> {
    check_auth(&headers, &state)?;
    let (claims, _) = validate_lease_any(&state, &request.lease_token, SCOPE_JOB_SUBMIT)
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;
    if claims.mission_id != request.mission_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "lease is scoped to a different mission".to_string(),
        ));
    }
    if claims.job_id.is_some_and(|job_id| job_id != request.job_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "lease is scoped to a different job".to_string(),
        ));
    }
    validate_job_payload(&request.payload, &state.work_root)
        .map_err(|err| (StatusCode::UNPROCESSABLE_ENTITY, err))?;
    if state
        .jobs
        .get(request.job_id)
        .await
        .map_err(internal_error)?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            format!("job {} already exists", request.job_id),
        ));
    }
    state
        .runner
        .submit(request.job_id, request.mission_id, request.payload)
        .await
        .map_err(|err| {
            if err.downcast_ref::<NodeQueueFull>().is_some() {
                (StatusCode::TOO_MANY_REQUESTS, err.to_string())
            } else {
                internal_error(err)
            }
        })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitJobResponse {
            job_id: request.job_id,
            state: "queued".to_string(),
        }),
    ))
}

fn validate_job_payload(
    payload: &sandboxed_sh::remote_node::JobPayload,
    work_root: &Path,
) -> Result<(), String> {
    if let sandboxed_sh::remote_node::JobPayload::LeanBuild {
        source,
        cwd_rel,
        command,
        estimated_disk_bytes,
        env,
        ..
    } = payload
    {
        sandboxed_sh::node::lean::validate_lean_build(
            source,
            cwd_rel.as_deref(),
            command,
            env,
            &sandboxed_sh::node::lean::env_allowlist_from_env(),
        )?;
        sandboxed_sh::node::lean::validate_disk_admission(
            work_root,
            estimated_disk_bytes.unwrap_or_default(),
        )?;
    }
    Ok(())
}

/// `GET /jobs/:id` — full job status including up to 64 KiB of log tail.
async fn get_job(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<NodeJobStatus>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    let record = state
        .jobs
        .get(id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("job {id} not found")))?;
    let log_tail = match record.log_path.as_deref() {
        Some(path) => read_log_tail(Path::new(path)).await,
        None => None,
    };
    Ok(Json(job_status_from_record(&record, log_tail)))
}

/// `POST /jobs/:id/cancel` — request cancellation of a queued/running job.
async fn cancel_job(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<CancelJobResponse>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    let cancel_requested = state.runner.cancel(id).await.map_err(internal_error)?;
    let record = state
        .jobs
        .get(id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("job {id} not found")))?;
    Ok(Json(CancelJobResponse {
        job_id: id,
        state: record.state.as_str().to_string(),
        cancel_requested,
    }))
}

#[derive(Debug, Deserialize)]
struct ListJobsQuery {
    limit: Option<usize>,
}

/// `GET /jobs?limit=N` — recent jobs, newest first (default 20, no log tails).
async fn list_jobs(
    State(state): State<Arc<NodeState>>,
    headers: HeaderMap,
    Query(query): Query<ListJobsQuery>,
) -> Result<Json<Vec<NodeJobStatus>>, (StatusCode, String)> {
    check_auth(&headers, &state)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 200);
    let records = state.jobs.recent(limit).await.map_err(internal_error)?;
    Ok(Json(
        records
            .iter()
            .map(|record| job_status_from_record(record, None))
            .collect(),
    ))
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::AUTHORIZATION;
    use sandboxed_sh::remote_node::{create_lease_token, LeaseClaims, SCOPE_MISSION_EXECUTE};
    use uuid::Uuid;

    fn auth_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}")
                .parse()
                .expect("valid auth header"),
        );
        headers
    }

    async fn test_state_with_capacity(work_root: PathBuf, capacity_total: u32) -> Arc<NodeState> {
        let jobs = JobStore::open(&work_root).await.expect("job store");
        let admission = Arc::new(Semaphore::new(capacity_total as usize));
        let runner = JobRunner::spawn_with_admission(
            jobs.clone(),
            work_root.clone(),
            capacity_total,
            DEFAULT_MAX_JOB_SECS,
            Arc::clone(&admission),
        );
        Arc::new(NodeState {
            node_id: "test-node".to_string(),
            shared_token: "node-secret".to_string(),
            previous_token: Some("node-secret-old".to_string()),
            labels: vec!["test".to_string()],
            work_root,
            capacity_total,
            admission,
            active_leases: AtomicU32::new(0),
            jobs,
            runner,
        })
    }

    async fn test_state(work_root: PathBuf) -> Arc<NodeState> {
        test_state_with_capacity(work_root, 2).await
    }

    #[tokio::test]
    async fn check_auth_accepts_current_and_previous_tokens() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = test_state(work_root.path().to_path_buf()).await;
        assert!(check_auth(&auth_headers("node-secret"), &state).is_ok());
        assert!(check_auth(&auth_headers("node-secret-old"), &state).is_ok());
        assert!(check_auth(&auth_headers("wrong"), &state).is_err());
        assert!(check_auth(&HeaderMap::new(), &state).is_err());
    }

    #[tokio::test]
    async fn heartbeat_reports_active_lease_capacity() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = test_state(work_root.path().to_path_buf()).await;
        let mission_id = Uuid::new_v4();
        let claims = LeaseClaims {
            mission_id,
            node_id: state.node_id.clone(),
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).timestamp(),
            job_id: None,
        };
        let request = LeaseRequest {
            mission_id,
            node_id: state.node_id.clone(),
            lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
            command: "sleep 0.2".to_string(),
        };
        let headers = auth_headers(&state.shared_token);

        let running = tokio::spawn(execute(
            State(state.clone()),
            headers.clone(),
            Json(request),
        ));

        let mut observed_busy = None;
        for _ in 0..20 {
            let snapshot = heartbeat(State(state.clone()), headers.clone())
                .await
                .expect("heartbeat")
                .0;
            if snapshot.active_leases == 1 {
                observed_busy = Some(snapshot);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let busy = observed_busy.expect("heartbeat should observe active execute");
        assert_eq!(busy.capacity_total, 2);
        assert_eq!(busy.capacity_available, 1);
        assert_eq!(busy.protocol_version, NODE_PROTOCOL_VERSION);
        assert_eq!(busy.labels, vec!["test".to_string()]);
        let _response = running
            .await
            .expect("execute task")
            .expect("execute response");

        let idle = heartbeat(State(state), headers).await.expect("heartbeat").0;
        assert_eq!(idle.active_leases, 0);
        assert_eq!(idle.capacity_available, 2);
    }

    #[test]
    fn heartbeat_withholds_lean_label_without_a_lake_proxy() {
        let labels = vec!["lean".to_string(), "high-memory".to_string()];
        assert_eq!(
            advertised_labels(&labels, false),
            vec!["high-memory".to_string()]
        );
        assert_eq!(advertised_labels(&labels, true), labels);
    }

    #[tokio::test]
    async fn execute_rejects_concurrent_work_when_shared_capacity_is_full() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = test_state_with_capacity(work_root.path().to_path_buf(), 1).await;
        let headers = auth_headers(&state.shared_token);
        let mission_id = Uuid::new_v4();
        let claims = LeaseClaims {
            mission_id,
            node_id: state.node_id.clone(),
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).timestamp(),
            job_id: None,
        };
        let request = || LeaseRequest {
            mission_id,
            node_id: state.node_id.clone(),
            lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
            command: "true".to_string(),
        };
        // Hold the shared permit directly. Spawning a shell and polling
        // active_leases made this admission test race with process startup and
        // teardown under a heavily parallel test runner; heartbeat coverage
        // separately verifies the execute bookkeeping around a real process.
        let _held_permit = Arc::clone(&state.admission)
            .acquire_owned()
            .await
            .expect("capacity permit");

        let rejected = execute(State(Arc::clone(&state)), headers, Json(request())).await;
        assert_eq!(rejected.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn execute_and_async_jobs_share_the_same_capacity_boundary() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = test_state_with_capacity(work_root.path().to_path_buf(), 1).await;
        let headers = auth_headers(&state.shared_token);
        let mission_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let mission_dir = work_root.path().join(mission_id.to_string());
        tokio::fs::create_dir_all(&mission_dir)
            .await
            .expect("mission dir");
        let job_claims = LeaseClaims {
            mission_id,
            node_id: state.node_id.clone(),
            scope: SCOPE_JOB_SUBMIT.to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).timestamp(),
            job_id: Some(job_id),
        };
        let _ = submit_job(
            State(Arc::clone(&state)),
            headers.clone(),
            Json(SubmitJobRequest {
                job_id,
                mission_id,
                lease_token: create_lease_token(&job_claims, &state.shared_token)
                    .expect("lease token"),
                payload: sandboxed_sh::remote_node::JobPayload::RawCommand {
                    command: "while [ ! -e release ]; do sleep 0.01; done".to_string(),
                    timeout_secs: Some(30),
                    env: None,
                },
            }),
        )
        .await
        .expect("job accepted");
        // Wait for the async job's process to actually register as active. A
        // 200ms budget raced with process startup under a heavily parallel test
        // runner (the scheduled CI run flaked here); match the 5s budget the
        // terminal-state wait below already uses. Breaks immediately once up.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.runner.active_count() != 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("async job should register as active");
        assert_eq!(state.runner.active_count(), 1);

        let execute_claims = LeaseClaims {
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            job_id: None,
            ..job_claims
        };
        let rejected = execute(
            State(Arc::clone(&state)),
            headers,
            Json(LeaseRequest {
                mission_id,
                node_id: state.node_id.clone(),
                lease_token: create_lease_token(&execute_claims, &state.shared_token)
                    .expect("lease token"),
                command: "true".to_string(),
            }),
        )
        .await;
        assert_eq!(rejected.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
        tokio::fs::write(mission_dir.join("release"), b"")
            .await
            .expect("release async job");
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(record) = state.jobs.get(job_id).await.expect("read job") {
                    if record.state.is_terminal() {
                        break record;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("async job should finish");
        assert_eq!(terminal.state, sandboxed_sh::node::JobState::Succeeded);
    }

    #[tokio::test]
    async fn job_submission_requires_job_scoped_lease_and_runs_async() {
        let work_root = tempfile::tempdir().expect("tempdir");
        let state = test_state(work_root.path().to_path_buf()).await;
        let headers = auth_headers(&state.shared_token);
        let mission_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let payload = sandboxed_sh::remote_node::JobPayload::RawCommand {
            command: "echo job-ok".to_string(),
            timeout_secs: Some(30),
            env: None,
        };

        // A mission:execute lease must be rejected for job submission.
        let wrong_scope = LeaseClaims {
            mission_id,
            node_id: state.node_id.clone(),
            scope: SCOPE_MISSION_EXECUTE.to_string(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(1)).timestamp(),
            job_id: Some(job_id),
        };
        let rejected = submit_job(
            State(state.clone()),
            headers.clone(),
            Json(SubmitJobRequest {
                job_id,
                mission_id,
                lease_token: create_lease_token(&wrong_scope, &state.shared_token)
                    .expect("lease token"),
                payload: payload.clone(),
            }),
        )
        .await;
        assert_eq!(rejected.unwrap_err().0, StatusCode::BAD_REQUEST);

        // A properly scoped lease is accepted with 202/queued.
        let claims = LeaseClaims {
            scope: SCOPE_JOB_SUBMIT.to_string(),
            ..wrong_scope
        };
        let invalid = submit_job(
            State(state.clone()),
            headers.clone(),
            Json(SubmitJobRequest {
                job_id,
                mission_id,
                lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
                payload: sandboxed_sh::remote_node::JobPayload::LeanBuild {
                    source: Box::new(sandboxed_sh::remote_node::JobSource {
                        base_tree_sha: None,
                        repo: "/node/local/repo".to_string(),
                        commit: "a".repeat(40),
                        archive: None,
                        bundle: None,
                    }),
                    cwd_rel: None,
                    command: vec!["lake".to_string(), "build".to_string()],
                    timeout_secs: None,
                    estimated_disk_bytes: None,
                    cache_key: None,
                    artifacts: vec![],
                    env: Default::default(),
                },
            }),
        )
        .await;
        assert_eq!(invalid.unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(state.jobs.get(job_id).await.unwrap().is_none());

        let (status, accepted) = submit_job(
            State(state.clone()),
            headers.clone(),
            Json(SubmitJobRequest {
                job_id,
                mission_id,
                lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
                payload: payload.clone(),
            }),
        )
        .await
        .expect("job accepted");
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(accepted.0.state, "queued");

        // Poll until terminal and check the captured log tail.
        let mut final_status = None;
        for _ in 0..200 {
            let snapshot = get_job(State(state.clone()), headers.clone(), AxumPath(job_id))
                .await
                .expect("job status")
                .0;
            if matches!(snapshot.state.as_str(), "succeeded" | "failed" | "lost") {
                final_status = Some(snapshot);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let final_status = final_status.expect("job should finish");
        assert_eq!(final_status.state, "succeeded");
        assert_eq!(final_status.exit_code, Some(0));
        assert!(final_status.log_tail.unwrap_or_default().contains("job-ok"));

        // Duplicate submissions conflict.
        let duplicate = submit_job(
            State(state.clone()),
            headers.clone(),
            Json(SubmitJobRequest {
                job_id,
                mission_id,
                lease_token: create_lease_token(&claims, &state.shared_token).expect("lease token"),
                payload,
            }),
        )
        .await;
        assert_eq!(duplicate.unwrap_err().0, StatusCode::CONFLICT);

        // The job shows up in the recent list.
        let listed = list_jobs(
            State(state),
            headers,
            Query(ListJobsQuery { limit: Some(5) }),
        )
        .await
        .expect("job list")
        .0;
        assert!(listed.iter().any(|job| job.job_id == job_id));
    }
}
