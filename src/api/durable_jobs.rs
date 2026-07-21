//! Durable background jobs.
//!
//! Jobs are launched by the API server rather than by an ephemeral agent shell.
//! That places long-running commands under the server's lifecycle and gives
//! later turns an explicit registry for status, logs, and cancellation.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::process::{Child, Command};
use uuid::Uuid;

use super::{auth::AuthUser, control::control_for_user, routes::AppState};
use crate::api::mission_store::MissionStore;
use crate::workspace::WorkspaceType;
use crate::workspace_exec::WorkspaceExec;

const PIDLESS_START_GRACE_SECS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurableJobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableJob {
    pub id: Uuid,
    pub command: String,
    pub cwd: String,
    pub status: DurableJobStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Persisted liveness signal advanced by the server-owned watcher.
    #[serde(default)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Absolute runtime deadline. Long jobs default to two hours.
    #[serde(default)]
    pub deadline_at: Option<DateTime<Utc>>,
    pub started_by_mission_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    /// Authenticated API user that launched the job. Legacy entries fall back
    /// to their owning mission for authorization.
    #[serde(default)]
    pub owner_user_id: Option<String>,
    pub stdout_log: String,
    pub stderr_log: String,
    pub status_file: String,
    /// Transient systemd scope owned by this durable job. It intentionally
    /// does not carry the launching mission's tag, so mission teardown cannot
    /// terminate it.
    #[serde(default)]
    pub scope_unit: Option<String>,
    /// Caller-selected scheduling/resource hint (for example `lean_heavy`).
    #[serde(default)]
    pub resource_class: Option<String>,
    /// Retry key supplied by the caller. The job id is derived from this key.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Hash of every caller-controlled execution option. Environment values
    /// are hashed rather than persisted so job receipts never expose secrets.
    #[serde(default)]
    pub request_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartDurableJobRequest {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub started_by_mission_id: Option<Uuid>,
    /// Run through this workspace's execution layer. Container workspaces
    /// therefore use their persistent nspawn namespace and mission cgroup
    /// caps instead of leaking heavy work into the API service cgroup.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Maximum runtime in seconds. Defaults to two hours and is capped at one day.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub resource_class: Option<String>,
    /// Required by Hermes-facing callers so reconnect retries cannot duplicate work.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JobLogsQuery {
    #[serde(default = "default_tail_bytes")]
    pub tail_bytes: usize,
    #[serde(default)]
    pub stream: Option<String>,
}

fn default_tail_bytes() -> usize {
    16 * 1024
}

const DEFAULT_JOB_TIMEOUT_SECS: u64 = 2 * 60 * 60;
const MAX_JOB_TIMEOUT_SECS: u64 = 24 * 60 * 60;
const FORCE_CLEAR_SECS: u64 = 30;

fn validated_idempotency_key(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let key = raw.trim();
    if key.is_empty() {
        return Err("idempotency_key cannot be empty".to_string());
    }
    if key.len() > 200 {
        return Err("idempotency_key must be at most 200 bytes".to_string());
    }
    Ok(Some(key.to_string()))
}

fn durable_job_id(user_id: &str, mission_id: Uuid, key: Option<&str>) -> Uuid {
    match key {
        Some(key) => Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("sandboxed.sh/durable-job/{user_id}/{mission_id}/{key}").as_bytes(),
        ),
        None => Uuid::new_v4(),
    }
}

fn durable_job_request_fingerprint(
    command: &str,
    cwd: &Path,
    workspace_id: Uuid,
    env: &std::collections::HashMap<String, String>,
    timeout_secs: u64,
    resource_class: Option<&str>,
) -> String {
    let mut env = env.iter().collect::<Vec<_>>();
    env.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let payload = serde_json::to_vec(&(
        command,
        cwd.to_string_lossy(),
        workspace_id,
        env,
        timeout_secs,
        resource_class,
    ))
    .expect("durable job fingerprint payload is serializable");
    hex::encode(Sha256::digest(payload))
}

fn job_matches_request(job: &DurableJob, request_fingerprint: &str) -> bool {
    job.request_fingerprint.as_deref() == Some(request_fingerprint)
}

fn try_acquire_submission_claim(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

fn require_workspace_scope(workspace_id: Option<Uuid>) -> Result<Uuid, String> {
    workspace_id.ok_or_else(|| {
        "workspace_id is required; unscoped durable jobs cannot run on the API host".to_string()
    })
}

#[derive(Debug, Serialize)]
pub struct JobLogsResponse {
    pub job_id: Uuid,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExitRecord {
    exit_code: Option<i32>,
    signal: Option<i32>,
    finished_at: DateTime<Utc>,
}

fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}

async fn authorize_start(
    state: &Arc<AppState>,
    user: &AuthUser,
    workspace_id: Option<Uuid>,
    mission_id: Option<Uuid>,
) -> Result<(Uuid, Uuid), (StatusCode, Json<ErrorResponse>)> {
    let workspace_id = require_workspace_scope(workspace_id)
        .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    let mission_id = mission_id.ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "started_by_mission_id is required for durable jobs",
        )
    })?;
    let control = control_for_user(state, user).await;
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .ok_or_else(|| err(StatusCode::FORBIDDEN, "mission is not owned by the caller"))?;
    if mission.workspace_id != workspace_id {
        return Err(err(
            StatusCode::FORBIDDEN,
            "workspace does not belong to the caller mission",
        ));
    }
    Ok((workspace_id, mission_id))
}

fn explicit_owner_authorized(job: &DurableJob, user_id: &str) -> Option<bool> {
    job.owner_user_id.as_deref().map(|owner| owner == user_id)
}

fn durable_shell_wrapper(command: &str) -> String {
    // Isolate the caller's shell options and `exit` calls. In particular,
    // `set -e; false` must terminate only this subshell so the parent can
    // always persist the restart-safe terminal record.
    format!(
        "if [ -n \"${{REMOTE_BUILD_COMMAND:-}}\" ]; then\n  __oa_policy_bin=${{REMOTE_BUILD_COMMAND%/*}}\n  PATH=$__oa_policy_bin:$PATH\n  export PATH\n  unset __oa_policy_bin\nfi\n(\n{command}\n)\ncode=$?\nprintf '{{\"exit_code\":%s,\"signal\":null,\"finished_at\":\"%s\"}}\\n' \"$code\" \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" > \"$SANDBOXED_SH_DURABLE_STATUS\"\nexit \"$code\"\n"
    )
}

async fn authorize_job(
    state: &Arc<AppState>,
    user: &AuthUser,
    job: &DurableJob,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if let Some(allowed) = explicit_owner_authorized(job, &user.id) {
        return if allowed {
            Ok(())
        } else {
            Err(err(
                StatusCode::FORBIDDEN,
                "durable job belongs to another user",
            ))
        };
    }

    // Backward compatibility for jobs created before owner_user_id existed.
    let control = control_for_user(state, user).await;
    authorize_legacy_job(&control.mission_store, job).await
}

async fn authorize_legacy_job(
    mission_store: &Arc<dyn MissionStore>,
    job: &DurableJob,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let mission_id = job.started_by_mission_id.ok_or_else(|| {
        err(
            StatusCode::FORBIDDEN,
            "legacy durable job has no caller ownership metadata",
        )
    })?;
    let mission = mission_store
        .get_mission(mission_id)
        .await
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .ok_or_else(|| err(StatusCode::FORBIDDEN, "durable job belongs to another user"))?;
    if job
        .workspace_id
        .is_some_and(|workspace_id| mission.workspace_id != workspace_id)
    {
        return Err(err(
            StatusCode::FORBIDDEN,
            "durable job belongs to another user",
        ));
    }
    Ok(())
}

fn jobs_root(state: &AppState) -> PathBuf {
    state.config.working_dir.join(".sandboxed-sh/durable-jobs")
}

/// Observe whether a durable job owned by `mission_id` is terminal without
/// requiring an agent turn to call the durable-job API. The automation
/// scheduler uses this to provide event-like completion wakeups while keeping
/// the file-backed registry authoritative across API restarts.
pub(crate) async fn terminal_for_mission(
    working_dir: &Path,
    id: Uuid,
    mission_id: Uuid,
) -> Result<bool, String> {
    let path = working_dir
        .join(".sandboxed-sh/durable-jobs")
        .join(id.to_string())
        .join("job.json");
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| format!("durable job not found: {id}"))?;
    let job: DurableJob =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid durable job entry: {e}"))?;

    if job.started_by_mission_id != Some(mission_id) {
        return Err(format!(
            "durable job {id} is not owned by mission {mission_id}"
        ));
    }

    if matches!(
        job.status,
        DurableJobStatus::Completed
            | DurableJobStatus::Failed
            | DurableJobStatus::Cancelled
            | DurableJobStatus::Unknown
    ) {
        return Ok(true);
    }

    // The child watcher is process-local, so an API restart can leave job.json
    // at `running`. The wrapper's atomic exit record is the restart-safe source
    // of truth and should wake the owner even before a GET refreshes job.json.
    if let Ok(bytes) = tokio::fs::read(&job.status_file).await {
        if serde_json::from_slice::<ExitRecord>(&bytes).is_ok() {
            return Ok(true);
        }
    }

    // If neither watcher nor wrapper could persist a terminal record, a dead
    // supervisor still requires owner attention rather than infinite polling.
    // Keep a short grace for the normal start_job window between the initial
    // record and the post-spawn PID update. After a restart that update can no
    // longer arrive, so an older pidless record must wake its owner too.
    Ok(match job.pid {
        Some(pid) => !process_alive(pid),
        None => (Utc::now() - job.updated_at).num_seconds() >= PIDLESS_START_GRACE_SECS,
    })
}

fn job_dir(state: &AppState, id: Uuid) -> PathBuf {
    jobs_root(state).join(id.to_string())
}

fn job_file(state: &AppState, id: Uuid) -> PathBuf {
    job_dir(state, id).join("job.json")
}

fn job_lock_file(state: &AppState, id: Uuid) -> PathBuf {
    job_dir(state, id).join("job.lock")
}

fn resolve_cwd(base: &Path, raw: Option<&str>) -> Result<PathBuf, String> {
    let cwd = match raw {
        Some(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value.trim());
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        }
        _ => base.to_path_buf(),
    };

    if !cwd.exists() {
        return Err(format!("cwd does not exist: {}", cwd.display()));
    }
    if !cwd.is_dir() {
        return Err(format!("cwd is not a directory: {}", cwd.display()));
    }

    Ok(cwd)
}

fn resolve_workspace_cwd(
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    raw: Option<&str>,
) -> Result<PathBuf, String> {
    let cwd = match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => workspace_root.to_path_buf(),
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() && path.starts_with(workspace_root) {
                path
            } else if path.is_absolute() && workspace_type == WorkspaceType::Container {
                workspace_root.join(path.strip_prefix("/").unwrap_or(&path))
            } else if path.is_absolute() {
                return Err(format!(
                    "cwd must stay within workspace root {}",
                    workspace_root.display()
                ));
            } else {
                workspace_root.join(path)
            }
        }
    };

    if !cwd.is_dir() {
        return Err(format!("cwd does not exist: {}", cwd.display()));
    }
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|e| format!("failed to resolve workspace root: {e}"))?;
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|e| format!("failed to resolve cwd: {e}"))?;
    if !canonical_cwd.starts_with(&canonical_root) {
        return Err(format!(
            "cwd escapes workspace root {}",
            workspace_root.display()
        ));
    }
    // Keep the configured/logical workspace prefix after validating its
    // canonical target. WorkspaceExec strips that prefix to derive the path
    // inside a container. Returning the canonical target here breaks that
    // mapping when the configured root is a stable symlink (as on dev), and
    // silently turns every requested cwd into `/`.
    Ok(cwd)
}

fn merge_job_for_write(current: Option<DurableJob>, mut next: DurableJob) -> DurableJob {
    if let Some(current) = current {
        if matches!(
            current.status,
            DurableJobStatus::Completed | DurableJobStatus::Failed | DurableJobStatus::Cancelled
        ) && !matches!(
            next.status,
            DurableJobStatus::Completed | DurableJobStatus::Failed | DurableJobStatus::Cancelled
        ) {
            return current;
        }
        if current.status == DurableJobStatus::Cancelled
            && next.status != DurableJobStatus::Cancelled
        {
            next.status = DurableJobStatus::Cancelled;
        }
    }
    next
}

async fn write_job(state: &AppState, job: &DurableJob) -> Result<DurableJob, String> {
    let path = job_file(state, job.id);
    let parent = path
        .parent()
        .ok_or_else(|| "invalid durable job path".to_string())?;
    let lock_path = job_lock_file(state, job.id);
    std::fs::create_dir_all(parent).map_err(|e| format!("failed to create job dir: {}", e))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("failed to open job lock: {}", e))?;
    lock.lock_exclusive()
        .map_err(|e| format!("failed to lock job registry entry: {}", e))?;

    let current = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<DurableJob>(&bytes).ok(),
        Err(_) => None,
    };
    let job = merge_job_for_write(current, job.clone());
    let bytes = serde_json::to_vec_pretty(&job)
        .map_err(|e| format!("failed to serialize job registry entry: {}", e))?;
    std::fs::write(path, bytes)
        .map_err(|e| format!("failed to write job registry entry: {}", e))?;
    Ok(job)
}

async fn read_job(state: &AppState, id: Uuid) -> Result<DurableJob, String> {
    let bytes = tokio::fs::read(job_file(state, id))
        .await
        .map_err(|_| format!("durable job not found: {}", id))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid durable job entry: {}", e))
}

async fn write_terminal_job_state(
    state: &AppState,
    mut job: DurableJob,
    status: DurableJobStatus,
    exit_code: Option<i32>,
    signal: Option<i32>,
    updated_at: DateTime<Utc>,
) -> DurableJob {
    if let Ok(latest) = read_job(state, job.id).await {
        job = latest;
    }

    job.status = status;
    job.exit_code = exit_code;
    job.signal = signal;
    job.updated_at = updated_at;
    write_job(state, &job).await.unwrap_or(job)
}

async fn signal_job(job: &DurableJob, force: bool) {
    if let Some(unit) = job.scope_unit.as_deref() {
        if force {
            let unit = if unit.ends_with(".scope") {
                unit.to_string()
            } else {
                format!("{unit}.scope")
            };
            let _ = Command::new("systemctl")
                .args(["kill", "--kill-who=all", "--signal=KILL", unit.as_str()])
                .status()
                .await;
        } else if stop_scope(unit).await {
            return;
        }
    }
    if let Some(pid) = job.pid {
        if force {
            force_kill_process_group(pid);
        } else {
            terminate_process_group(pid);
        }
    }
}

fn spawn_job_watcher(state: Arc<AppState>, id: Uuid, mut child: Child) {
    tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let deadline = read_job(&state, id)
                .await
                .ok()
                .and_then(|job| job.deadline_at)
                .map(|deadline| {
                    (deadline - Utc::now())
                        .to_std()
                        .unwrap_or(std::time::Duration::from_millis(1))
                })
                .unwrap_or_default();
            tokio::select! {
                status = child.wait() => {
                    let Ok(status) = status else { break };
                    if let Ok(job) = read_job(&state, id).await {
                        let job_status = if status.success() {
                            DurableJobStatus::Completed
                        } else {
                            DurableJobStatus::Failed
                        };
                #[cfg(unix)]
                        let signal = {
                            use std::os::unix::process::ExitStatusExt;
                            status.signal()
                        };
                #[cfg(not(unix))]
                        let signal = None;
                        let _ = write_terminal_job_state(
                            &state, job, job_status, status.code(), signal, Utc::now(),
                        ).await;
                    }
                    break;
                }
                _ = heartbeat.tick() => {
                    if let Ok(mut job) = read_job(&state, id).await {
                        if job.status != DurableJobStatus::Running { break; }
                        let now = Utc::now();
                        job.heartbeat_at = Some(now);
                        job.updated_at = now;
                        let _ = write_job(&state, &job).await;
                    }
                }
                _ = tokio::time::sleep(deadline), if !deadline.is_zero() => {
                    if let Ok(mut job) = read_job(&state, id).await {
                        if job.status == DurableJobStatus::Running {
                            job.status = DurableJobStatus::Failed;
                            job.updated_at = Utc::now();
                            let _ = write_job(&state, &job).await;
                            signal_job(&job, false).await;
                            tokio::time::sleep(std::time::Duration::from_secs(FORCE_CLEAR_SECS)).await;
                            if job.pid.is_some_and(process_alive) {
                                signal_job(&job, true).await;
                            }
                        }
                    }
                    let _ = child.wait().await;
                    break;
                }
            }
        }
    });
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn cgroup_contains_scope(cgroup: &str, scope_unit: &str) -> bool {
    let expected = if scope_unit.ends_with(".scope") {
        scope_unit.to_string()
    } else {
        format!("{scope_unit}.scope")
    };
    cgroup
        .lines()
        .flat_map(|line| {
            line.rsplit_once(':')
                .map(|(_, path)| path)
                .unwrap_or(line)
                .split('/')
        })
        .any(|component| component == expected)
}

#[cfg(unix)]
fn process_owned_by_job(job: &DurableJob, pid: u32) -> bool {
    if !process_alive(pid) {
        return false;
    }
    let Some(scope_unit) = job.scope_unit.as_deref() else {
        return true;
    };
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .is_some_and(|cgroup| cgroup_contains_scope(&cgroup, scope_unit))
}

#[cfg(not(unix))]
fn process_owned_by_job(_job: &DurableJob, _pid: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn terminate_process_group(pid: u32) {
    unsafe {
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid > 0 {
            let _ = libc::kill(-pgid, libc::SIGTERM);
        } else {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

#[cfg(unix)]
fn force_kill_process_group(pid: u32) {
    unsafe {
        let pgid = libc::getpgid(pid as libc::pid_t);
        if pgid > 0 {
            let _ = libc::kill(-pgid, libc::SIGKILL);
        } else {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

async fn stop_scope(unit: &str) -> bool {
    let unit = if unit.ends_with(".scope") {
        unit.to_string()
    } else {
        format!("{unit}.scope")
    };
    Command::new("systemctl")
        .args(["stop", unit.as_str()])
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {}

#[cfg(not(unix))]
fn force_kill_process_group(_pid: u32) {}

async fn refresh_job(state: &AppState, mut job: DurableJob) -> DurableJob {
    if matches!(
        job.status,
        DurableJobStatus::Running | DurableJobStatus::Unknown
    ) {
        if let Ok(bytes) = tokio::fs::read(&job.status_file).await {
            if let Ok(exit) = serde_json::from_slice::<ExitRecord>(&bytes) {
                let status = if exit.exit_code == Some(0) {
                    DurableJobStatus::Completed
                } else {
                    DurableJobStatus::Failed
                };
                return write_terminal_job_state(
                    state,
                    job,
                    status,
                    exit.exit_code,
                    exit.signal,
                    exit.finished_at,
                )
                .await;
            }
        }

        if let Some(pid) = job.pid {
            if !process_owned_by_job(&job, pid) {
                // systemd-run may need a brief moment to attach the payload
                // to its transient scope, and very short commands can exit
                // just before their wrapper atomically publishes exit.json.
                // Keep the persisted start lease provisional during that
                // bounded window instead of returning a false `unknown`.
                let now = Utc::now();
                if (now - job.created_at).num_seconds() >= PIDLESS_START_GRACE_SECS {
                    job.status = DurableJobStatus::Unknown;
                    job.updated_at = now;
                    job = write_job(state, &job).await.unwrap_or(job);
                }
            } else {
                let now = Utc::now();
                if job.deadline_at.is_some_and(|deadline| deadline <= now) {
                    job.status = DurableJobStatus::Cancelled;
                    job.updated_at = now;
                    job = write_job(state, &job).await.unwrap_or(job);
                    let cancelling = job.clone();
                    tokio::spawn(async move {
                        signal_job(&cancelling, false).await;
                        tokio::time::sleep(std::time::Duration::from_secs(FORCE_CLEAR_SECS)).await;
                        if cancelling.pid.is_some_and(process_alive) {
                            signal_job(&cancelling, true).await;
                        }
                    });
                } else if job
                    .heartbeat_at
                    .is_none_or(|heartbeat| (now - heartbeat).num_seconds() >= 5)
                {
                    // The original child watcher is process-local. After an
                    // API restart, a verified live PID in the persisted scope
                    // is enough to reattach the lease and advance heartbeat
                    // without resubmitting the underlying remote job.
                    job.heartbeat_at = Some(now);
                    job.updated_at = now;
                    job = write_job(state, &job).await.unwrap_or(job);
                }
            }
        }
    }
    job
}

pub async fn start_job(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<StartDurableJobRequest>,
) -> Result<Json<DurableJob>, (StatusCode, Json<ErrorResponse>)> {
    let command = req.command.trim();
    if command.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "command is required"));
    }
    let (workspace_id, started_by_mission_id) =
        authorize_start(&state, &user, req.workspace_id, req.started_by_mission_id).await?;
    let idempotency_key = validated_idempotency_key(req.idempotency_key.as_deref())
        .map_err(|message| err(StatusCode::BAD_REQUEST, message))?;
    let id = durable_job_id(&user.id, started_by_mission_id, idempotency_key.as_deref());

    let workspace = Some(state.workspaces.get(workspace_id).await.ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            format!("workspace not found: {workspace_id}"),
        )
    })?);
    let caller_env = req.env;
    let mut job_env = caller_env.clone();
    if let Some(workspace) = workspace.as_ref() {
        // Durable jobs are the Hermes-facing path for long commands, so they
        // must receive the same compute-policy boundary as an agent runner.
        // In particular, a Beal/Verity `lake build` may never bypass the Lake
        // shim merely because it was submitted through assistant-MCP.
        crate::workspace::install_remote_build_wrapper(workspace, started_by_mission_id)
            .await
            .map_err(|error| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to install workspace build policy wrappers: {error}"),
                )
            })?;
        if let Some(remote_env) = workspace.remote_build_env(started_by_mission_id) {
            // Policy, endpoint and capability token are server-owned. Caller
            // env may still opt into the audited emergency local override,
            // but cannot downgrade `remote_required` itself.
            job_env.extend(remote_env);
        }
    }
    let cwd = match workspace.as_ref() {
        Some(workspace) => resolve_workspace_cwd(
            &workspace.path,
            workspace.workspace_type,
            req.cwd.as_deref(),
        ),
        None => resolve_cwd(&state.config.working_dir, req.cwd.as_deref()),
    }
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

    let timeout_secs = req
        .timeout_secs
        .unwrap_or(DEFAULT_JOB_TIMEOUT_SECS)
        .clamp(1, MAX_JOB_TIMEOUT_SECS);
    let request_fingerprint = durable_job_request_fingerprint(
        command,
        &cwd,
        workspace_id,
        &caller_env,
        timeout_secs,
        req.resource_class.as_deref(),
    );
    if idempotency_key.is_some() {
        if let Ok(existing) = read_job(&state, id).await {
            if !job_matches_request(&existing, &request_fingerprint) {
                return Err(err(
                    StatusCode::CONFLICT,
                    "idempotency_key was already used with different job parameters",
                ));
            }
            return Ok(Json(refresh_job(&state, existing).await));
        }
    }

    let dir = job_dir(&state, id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // The lock, rather than file existence, owns the submission lease. A
    // failed request drops the lock, so a later retry can reuse the same key;
    // a concurrent request waits for the first job receipt instead of spawning
    // a duplicate process.
    let _submission_claim = if idempotency_key.is_some() {
        let claim_path = dir.join("submission.claim");
        match try_acquire_submission_claim(&claim_path)
            .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        {
            Some(claim) => {
                // Close the race between the initial read and taking the lock.
                if let Ok(existing) = read_job(&state, id).await {
                    if !job_matches_request(&existing, &request_fingerprint) {
                        return Err(err(
                            StatusCode::CONFLICT,
                            "idempotency_key was already used with different job parameters",
                        ));
                    }
                    return Ok(Json(refresh_job(&state, existing).await));
                }
                Some(claim)
            }
            None => {
                for _ in 0..40 {
                    if let Ok(existing) = read_job(&state, id).await {
                        if !job_matches_request(&existing, &request_fingerprint) {
                            return Err(err(
                                StatusCode::CONFLICT,
                                "idempotency_key parameter mismatch",
                            ));
                        }
                        return Ok(Json(refresh_job(&state, existing).await));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                return Err(err(
                    StatusCode::CONFLICT,
                    "idempotent job submission is still being registered",
                ));
            }
        }
    } else {
        None
    };
    let runtime_dir = workspace
        .as_ref()
        .map(|workspace| {
            workspace
                .path
                .join(".sandboxed-sh/durable-jobs")
                .join(id.to_string())
        })
        .unwrap_or_else(|| dir.clone());
    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stdout_log = runtime_dir.join("stdout.log");
    let stderr_log = runtime_dir.join("stderr.log");
    let status_file = runtime_dir.join("exit.json");

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stdout_log)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_log)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let wrapper = durable_shell_wrapper(command);

    let now = Utc::now();
    let mut job = DurableJob {
        id,
        command: command.to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        status: DurableJobStatus::Running,
        pid: None,
        exit_code: None,
        signal: None,
        created_at: now,
        updated_at: now,
        heartbeat_at: Some(now),
        deadline_at: Some(now + chrono::Duration::seconds(timeout_secs as i64)),
        started_by_mission_id: Some(started_by_mission_id),
        workspace_id: req.workspace_id,
        owner_user_id: Some(user.id.clone()),
        stdout_log: stdout_log.to_string_lossy().to_string(),
        stderr_log: stderr_log.to_string_lossy().to_string(),
        status_file: status_file.to_string_lossy().to_string(),
        scope_unit: workspace.as_ref().and_then(|workspace| {
            WorkspaceExec::new(workspace.clone())
                .machine_name()
                .map(|machine| crate::workspace_exec::durable_scope_unit(&machine, id))
        }),
        resource_class: req.resource_class,
        idempotency_key,
        request_fingerprint: Some(request_fingerprint),
    };
    job = write_job(&state, &job)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let status_path_for_child = workspace
        .as_ref()
        .map(|workspace| {
            WorkspaceExec::new(workspace.clone()).translate_path_for_container(&status_file)
        })
        .unwrap_or_else(|| status_file.to_string_lossy().to_string());
    job_env.insert(
        "SANDBOXED_SH_DURABLE_STATUS".to_string(),
        status_path_for_child,
    );
    // WorkspaceExec already provides the container login-shell boundary and
    // changes to the translated cwd before it execs this shell. A second
    // login shell can reset PWD to HOME (observed as `/` in nspawn), causing
    // a requested repository build to run against the wrong tree.
    let shell_args = vec!["-c".to_string(), wrapper];
    let child_result = match workspace {
        Some(workspace) => {
            WorkspaceExec::new(workspace)
                .spawn_with_stdio(
                    &cwd,
                    "/bin/sh",
                    &shell_args,
                    job_env,
                    Stdio::null(),
                    Stdio::from(stdout),
                    Stdio::from(stderr),
                    id,
                )
                .await
        }
        None => {
            let mut child = Command::new("/bin/sh");
            child
                .args(&shell_args)
                .current_dir(&cwd)
                .envs(job_env)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            #[cfg(unix)]
            unsafe {
                child.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            child.spawn().map_err(anyhow::Error::from)
        }
    };
    let child = match child_result {
        Ok(child) => child,
        Err(e) => {
            job.status = DurableJobStatus::Failed;
            job.updated_at = Utc::now();
            let _ = write_job(&state, &job).await;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };
    job.pid = child.id();
    job.updated_at = Utc::now();
    job = match write_job(&state, &job).await {
        Ok(job) => job,
        Err(e) => {
            let stopped_scope = match job.scope_unit.as_deref() {
                Some(unit) => stop_scope(unit).await,
                None => false,
            };
            if !stopped_scope {
                if let Some(pid) = job.pid {
                    terminate_process_group(pid);
                }
            }
            job.status = DurableJobStatus::Failed;
            job.updated_at = Utc::now();
            let _ = write_job(&state, &job).await;
            spawn_job_watcher(Arc::clone(&state), id, child);
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e));
        }
    };
    if job.status == DurableJobStatus::Cancelled {
        let stopped_scope = match job.scope_unit.as_deref() {
            Some(unit) => stop_scope(unit).await,
            None => false,
        };
        if !stopped_scope {
            if let Some(pid) = job.pid {
                terminate_process_group(pid);
            }
        }
    }

    spawn_job_watcher(Arc::clone(&state), id, child);

    Ok(Json(job))
}

pub async fn list_jobs(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<Vec<DurableJob>> {
    let mut jobs = Vec::new();
    let root = jobs_root(&state);
    if let Ok(mut entries) = tokio::fs::read_dir(root).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path().join("job.json");
            if let Ok(bytes) = tokio::fs::read(path).await {
                if let Ok(job) = serde_json::from_slice::<DurableJob>(&bytes) {
                    if authorize_job(&state, &user, &job).await.is_ok() {
                        jobs.push(refresh_job(&state, job).await);
                    }
                }
            }
        }
    }
    jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
    Json(jobs)
}

pub async fn get_job(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<DurableJob>, (StatusCode, Json<ErrorResponse>)> {
    let job = read_job(&state, id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    authorize_job(&state, &user, &job).await?;
    Ok(Json(refresh_job(&state, job).await))
}

async fn tail_file(path: &str, max_bytes: usize) -> String {
    let keep = max_bytes.clamp(1, 256 * 1024);
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return String::new();
    };
    let Ok(metadata) = file.metadata().await else {
        return String::new();
    };
    let start = metadata.len().saturating_sub(keep as u64);
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return String::new();
    }

    let mut bytes = Vec::with_capacity(keep);
    if file.read_to_end(&mut bytes).await.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).to_string()
}

pub async fn job_logs(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    AxumPath(id): AxumPath<Uuid>,
    axum::extract::Query(query): axum::extract::Query<JobLogsQuery>,
) -> Result<Json<JobLogsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let job = read_job(&state, id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    authorize_job(&state, &user, &job).await?;

    let stdout = if query.stream.as_deref() == Some("stderr") {
        String::new()
    } else {
        tail_file(&job.stdout_log, query.tail_bytes).await
    };
    let stderr = if query.stream.as_deref() == Some("stdout") {
        String::new()
    } else {
        tail_file(&job.stderr_log, query.tail_bytes).await
    };

    Ok(Json(JobLogsResponse {
        job_id: id,
        stdout,
        stderr,
    }))
}

pub async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<DurableJob>, (StatusCode, Json<ErrorResponse>)> {
    let mut job = read_job(&state, id)
        .await
        .map_err(|e| err(StatusCode::NOT_FOUND, e))?;
    authorize_job(&state, &user, &job).await?;
    job = refresh_job(&state, job).await;
    if job.status == DurableJobStatus::Running {
        job.status = DurableJobStatus::Cancelled;
        job.updated_at = Utc::now();
        job = write_job(&state, &job)
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let cancelling = job.clone();
        tokio::spawn(async move {
            signal_job(&cancelling, false).await;
            tokio::time::sleep(std::time::Duration::from_secs(FORCE_CLEAR_SECS)).await;
            if cancelling.pid.is_some_and(process_alive) {
                signal_job(&cancelling, true).await;
            }
        });
    }
    Ok(Json(job))
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_jobs).post(start_job))
        .route("/:id", get(get_job))
        .route("/:id/logs", get(job_logs))
        .route("/:id/cancel", post(cancel_job))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::SqliteMissionStore;

    fn test_job(status: DurableJobStatus) -> DurableJob {
        let now = Utc::now();
        DurableJob {
            id: Uuid::new_v4(),
            command: "true".to_string(),
            cwd: "/tmp".to_string(),
            status,
            pid: Some(123),
            exit_code: None,
            signal: None,
            created_at: now,
            updated_at: now,
            heartbeat_at: Some(now),
            deadline_at: None,
            started_by_mission_id: None,
            workspace_id: None,
            owner_user_id: None,
            stdout_log: "/tmp/stdout.log".to_string(),
            stderr_log: "/tmp/stderr.log".to_string(),
            status_file: "/tmp/exit.json".to_string(),
            scope_unit: None,
            resource_class: None,
            idempotency_key: None,
            request_fingerprint: None,
        }
    }

    #[test]
    fn resolve_cwd_defaults_to_base() {
        let base = std::env::current_dir().unwrap();
        assert_eq!(resolve_cwd(&base, None).unwrap(), base);
    }

    #[test]
    fn durable_jobs_require_a_workspace_scope() {
        let id = Uuid::new_v4();
        assert_eq!(require_workspace_scope(Some(id)).unwrap(), id);
        assert!(require_workspace_scope(None)
            .unwrap_err()
            .contains("cannot run on the API host"));
    }

    #[test]
    fn idempotency_key_derives_one_stable_job_id() {
        let mission_id = Uuid::new_v4();
        let first = durable_job_id("user", mission_id, Some("logical-build-1"));
        let retry = durable_job_id("user", mission_id, Some("logical-build-1"));
        let distinct = durable_job_id("user", mission_id, Some("logical-build-2"));
        assert_eq!(first, retry);
        assert_ne!(first, distinct);
        assert!(validated_idempotency_key(Some(" ")).is_err());
    }

    #[test]
    fn idempotency_fingerprint_covers_every_execution_option() {
        let workspace_id = Uuid::new_v4();
        let mut env = std::collections::HashMap::from([("MODE".to_string(), "one".to_string())]);
        let base = durable_job_request_fingerprint(
            "lake build",
            Path::new("/workspaces/beal"),
            workspace_id,
            &env,
            7_200,
            Some("lean_heavy"),
        );
        assert_eq!(
            base,
            durable_job_request_fingerprint(
                "lake build",
                Path::new("/workspaces/beal"),
                workspace_id,
                &env,
                7_200,
                Some("lean_heavy"),
            )
        );
        env.insert("MODE".to_string(), "two".to_string());
        assert_ne!(
            base,
            durable_job_request_fingerprint(
                "lake build",
                Path::new("/workspaces/beal"),
                workspace_id,
                &env,
                7_200,
                Some("lean_heavy"),
            )
        );
        assert_ne!(
            base,
            durable_job_request_fingerprint(
                "lake build",
                Path::new("/workspaces/verity"),
                workspace_id,
                &std::collections::HashMap::from([("MODE".to_string(), "one".to_string(),)]),
                7_200,
                Some("lean_heavy"),
            )
        );
        assert_ne!(
            base,
            durable_job_request_fingerprint(
                "lake build",
                Path::new("/workspaces/beal"),
                workspace_id,
                &std::collections::HashMap::from([("MODE".to_string(), "one".to_string(),)]),
                3_600,
                Some("lean_heavy"),
            )
        );
        assert_ne!(
            base,
            durable_job_request_fingerprint(
                "lake build",
                Path::new("/workspaces/beal"),
                workspace_id,
                &std::collections::HashMap::from([("MODE".to_string(), "one".to_string(),)]),
                7_200,
                Some("diagnostic"),
            )
        );
    }

    #[test]
    fn failed_submission_claim_can_be_reacquired() {
        let dir = tempfile::tempdir().unwrap();
        let claim_path = dir.path().join("submission.claim");
        let first = try_acquire_submission_claim(&claim_path)
            .unwrap()
            .expect("first claim");
        assert!(try_acquire_submission_claim(&claim_path).unwrap().is_none());
        drop(first);
        assert!(try_acquire_submission_claim(&claim_path).unwrap().is_some());
    }

    #[test]
    fn cgroup_scope_membership_requires_the_exact_unit() {
        let cgroup = "1:name=systemd:/\n0::/missions.slice/sandboxed-durable-demo.scope\n";
        assert!(cgroup_contains_scope(cgroup, "sandboxed-durable-demo"));
        assert!(cgroup_contains_scope(
            cgroup,
            "sandboxed-durable-demo.scope"
        ));
        assert!(!cgroup_contains_scope(cgroup, "sandboxed-durable"));
        assert!(!cgroup_contains_scope(
            cgroup,
            "sandboxed-durable-demo-other"
        ));
    }

    #[test]
    fn explicit_job_owner_is_isolated_between_users() {
        let mut job = test_job(DurableJobStatus::Running);
        assert_eq!(explicit_owner_authorized(&job, "alice"), None);
        job.owner_user_id = Some("alice".to_string());
        assert_eq!(explicit_owner_authorized(&job, "alice"), Some(true));
        assert_eq!(explicit_owner_authorized(&job, "bob"), Some(false));
    }

    #[test]
    fn wrapper_records_failure_even_when_command_enables_errexit() {
        let dir = tempfile::tempdir().unwrap();
        let status_file = dir.path().join("exit.json");
        let wrapper = durable_shell_wrapper("set -eu; false");
        assert!(wrapper.contains("REMOTE_BUILD_COMMAND%/*"));
        let output = std::process::Command::new("/bin/sh")
            .args(["-lc", &wrapper])
            .env("SANDBOXED_SH_DURABLE_STATUS", &status_file)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let record: ExitRecord =
            serde_json::from_slice(&std::fs::read(status_file).unwrap()).unwrap();
        assert_eq!(record.exit_code, Some(1));
        assert_eq!(record.signal, None);
    }

    #[test]
    fn resolve_cwd_rejects_missing_path() {
        let base = std::env::current_dir().unwrap();
        let result = resolve_cwd(&base, Some("__definitely_missing_durable_job_cwd__"));
        assert!(result.is_err());
    }

    #[test]
    fn workspace_cwd_maps_container_absolute_path_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        let mission = dir.path().join("workspaces/mission-deadbeef");
        std::fs::create_dir_all(&mission).unwrap();

        let resolved = resolve_workspace_cwd(
            dir.path(),
            WorkspaceType::Container,
            Some("/workspaces/mission-deadbeef"),
        )
        .unwrap();

        assert_eq!(resolved, mission);
    }

    #[test]
    fn workspace_cwd_rejects_host_absolute_path_and_relative_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_cwd(dir.path(), WorkspaceType::Host, Some("/tmp")).is_err());
        assert!(resolve_workspace_cwd(dir.path(), WorkspaceType::Container, Some("../")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_cwd_preserves_symlink_root_after_escape_validation() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let canonical_root = parent.path().join("canonical");
        let logical_root = parent.path().join("logical");
        std::fs::create_dir_all(canonical_root.join("workspaces/repo")).unwrap();
        symlink(&canonical_root, &logical_root).unwrap();

        let cwd = resolve_workspace_cwd(
            &logical_root,
            WorkspaceType::Container,
            Some("/workspaces/repo"),
        )
        .unwrap();

        assert_eq!(cwd, logical_root.join("workspaces/repo"));
        assert_ne!(cwd, canonical_root.join("workspaces/repo"));
    }

    #[test]
    fn merge_job_for_write_preserves_cancelled_status() {
        let current = test_job(DurableJobStatus::Cancelled);
        let mut next = current.clone();
        next.status = DurableJobStatus::Completed;
        next.exit_code = Some(0);

        let merged = merge_job_for_write(Some(current), next);

        assert_eq!(merged.status, DurableJobStatus::Cancelled);
        assert_eq!(merged.exit_code, Some(0));
    }

    #[test]
    fn merge_job_for_write_allows_explicit_cancelled_update() {
        let current = test_job(DurableJobStatus::Running);
        let mut next = current.clone();
        next.status = DurableJobStatus::Cancelled;

        let merged = merge_job_for_write(Some(current), next);

        assert_eq!(merged.status, DurableJobStatus::Cancelled);
    }

    #[tokio::test]
    async fn terminal_observer_uses_restart_safe_exit_record_and_enforces_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let mut job = test_job(DurableJobStatus::Running);
        job.started_by_mission_id = Some(mission_id);
        job.pid = None;
        job.status_file = dir.path().join("exit.json").to_string_lossy().to_string();
        let registry_dir = dir
            .path()
            .join(".sandboxed-sh/durable-jobs")
            .join(job.id.to_string());
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("job.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();

        assert!(!terminal_for_mission(dir.path(), job.id, mission_id)
            .await
            .unwrap());
        assert!(terminal_for_mission(dir.path(), job.id, Uuid::new_v4())
            .await
            .unwrap_err()
            .contains("not owned"));

        job.updated_at = Utc::now() - chrono::Duration::seconds(PIDLESS_START_GRACE_SECS + 1);
        std::fs::write(
            registry_dir.join("job.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        assert!(terminal_for_mission(dir.path(), job.id, mission_id)
            .await
            .unwrap());

        let exit = ExitRecord {
            exit_code: Some(0),
            signal: None,
            finished_at: Utc::now(),
        };
        std::fs::write(&job.status_file, serde_json::to_vec(&exit).unwrap()).unwrap();
        assert!(terminal_for_mission(dir.path(), job.id, mission_id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn terminal_observer_accepts_persisted_terminal_status() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let mut job = test_job(DurableJobStatus::Failed);
        job.started_by_mission_id = Some(mission_id);
        job.pid = None;
        let registry_dir = dir
            .path()
            .join(".sandboxed-sh/durable-jobs")
            .join(job.id.to_string());
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("job.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();

        assert!(terminal_for_mission(dir.path(), job.id, mission_id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn legacy_job_without_workspace_uses_owning_mission() {
        let dir = tempfile::tempdir().unwrap();
        let owner_store: Arc<dyn MissionStore> = Arc::new(
            SqliteMissionStore::new(dir.path().to_path_buf(), "owner")
                .await
                .unwrap(),
        );
        let other_store: Arc<dyn MissionStore> = Arc::new(
            SqliteMissionStore::new(dir.path().to_path_buf(), "other")
                .await
                .unwrap(),
        );
        let mission = owner_store
            .create_mission(Some("legacy owner"), None, None, None, None, None, None)
            .await
            .unwrap();
        let mut job = test_job(DurableJobStatus::Running);
        job.started_by_mission_id = Some(mission.id);
        let registry_dir = dir
            .path()
            .join(".sandboxed-sh/durable-jobs")
            .join(job.id.to_string());
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("job.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let persisted: DurableJob =
            serde_json::from_slice(&std::fs::read(registry_dir.join("job.json")).unwrap()).unwrap();

        assert!(authorize_legacy_job(&owner_store, &persisted).await.is_ok());
        assert!(authorize_legacy_job(&other_store, &persisted)
            .await
            .is_err());
    }

    #[test]
    fn merge_job_for_write_preserves_terminal_status_over_unknown_refresh() {
        let current = test_job(DurableJobStatus::Completed);
        let mut next = current.clone();
        next.status = DurableJobStatus::Unknown;

        let merged = merge_job_for_write(Some(current), next);

        assert_eq!(merged.status, DurableJobStatus::Completed);
    }

    #[tokio::test]
    async fn tail_file_reads_only_requested_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stdout.log");
        std::fs::write(&path, "0123456789abcdef").unwrap();

        let tail = tail_file(path.to_str().unwrap(), 6).await;

        assert_eq!(tail, "abcdef");
    }
}
