//! Async job runner for the `sandboxed-node` binary.
//!
//! Jobs are submitted to an mpsc queue and executed under a capacity
//! semaphore (`SANDBOXED_NODE_CAPACITY`). Each job runs `bash -lc <command>`
//! in `<workdir>/<mission-id>/` with combined stdout+stderr captured to
//! `<workdir>/logs/<job-id>.log`. Every terminal path cleans up the process
//! cgroup when systemd scopes are available, with process-group cleanup as a
//! fallback, so daemonized children cannot escape queue accounting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::job_store::{JobState, JobStore};
use crate::remote_node::JobPayload;

/// Default hard ceiling for a single job (4 hours), overridable via
/// `SANDBOXED_NODE_MAX_JOB_SECS`.
pub const DEFAULT_MAX_JOB_SECS: u64 = 14_400;

/// Maximum log-tail bytes returned by job status endpoints (64 KiB).
pub const LOG_TAIL_MAX_BYTES: u64 = 64 * 1024;

/// Grace period between SIGTERM and SIGKILL when stopping a job.
const KILL_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum SystemdScopeMode {
    System,
    User,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SystemdScope {
    unit: String,
    mode: SystemdScopeMode,
}

struct QueuedJob {
    id: Uuid,
    mission_id: Uuid,
    payload: JobPayload,
}

/// Shared job runner; construct with [`JobRunner::spawn`].
pub struct JobRunner {
    store: JobStore,
    work_root: PathBuf,
    max_job_secs: u64,
    tx: mpsc::UnboundedSender<QueuedJob>,
    max_queued: u32,
    cancels: Mutex<HashMap<Uuid, CancellationToken>>,
    queued: AtomicU32,
    active: AtomicU32,
}

impl JobRunner {
    /// Start the dispatcher loop and return the shared runner handle.
    pub fn spawn(
        store: JobStore,
        work_root: PathBuf,
        capacity: u32,
        max_job_secs: u64,
    ) -> Arc<Self> {
        Self::spawn_with_admission(
            store,
            work_root,
            capacity,
            max_job_secs,
            Arc::new(Semaphore::new(capacity.max(1) as usize)),
        )
    }

    /// Start a runner that shares its execution permits with other node work
    /// (notably synchronous `/execute` requests). This is what makes the
    /// node-wide capacity limit an admission control boundary rather than a
    /// heartbeat-only metric.
    pub fn spawn_with_admission(
        store: JobStore,
        work_root: PathBuf,
        capacity: u32,
        max_job_secs: u64,
        admission: Arc<Semaphore>,
    ) -> Arc<Self> {
        let max_queued = std::env::var("SANDBOXED_NODE_MAX_QUEUED")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or_else(|| (capacity.max(1) as usize).saturating_mul(4));
        let (tx, mut rx) = mpsc::unbounded_channel::<QueuedJob>();
        let runner = Arc::new(Self {
            store,
            work_root,
            max_job_secs: max_job_secs.max(1),
            tx,
            max_queued: u32::try_from(max_queued).unwrap_or(u32::MAX),
            cancels: Mutex::new(HashMap::new()),
            queued: AtomicU32::new(0),
            active: AtomicU32::new(0),
        });
        let dispatcher = Arc::clone(&runner);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let runner = Arc::clone(&dispatcher);
                let admission = Arc::clone(&admission);
                tokio::spawn(async move {
                    let permit = match admission.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            runner.queued.fetch_sub(1, Ordering::AcqRel);
                            runner.drop_cancel_token(job.id);
                            return;
                        }
                    };
                    let claimed = match runner.store.mark_running_if_queued(job.id).await {
                        Ok(claimed) => claimed,
                        Err(err) => {
                            runner.queued.fetch_sub(1, Ordering::AcqRel);
                            runner.drop_cancel_token(job.id);
                            tracing::warn!(job_id = %job.id, "failed to claim queued job: {err}");
                            return;
                        }
                    };
                    if !claimed {
                        runner.drop_cancel_token(job.id);
                        return;
                    }
                    runner.queued.fetch_sub(1, Ordering::AcqRel);
                    runner.active.fetch_add(1, Ordering::AcqRel);
                    runner.run_one(job).await;
                    runner.active.fetch_sub(1, Ordering::AcqRel);
                    drop(permit);
                });
            }
        });
        runner
    }

    pub fn queued_count(&self) -> u32 {
        self.queued.load(Ordering::Acquire)
    }

    pub fn active_count(&self) -> u32 {
        self.active.load(Ordering::Acquire)
    }

    /// Log file path for a job id.
    pub fn log_path(&self, job_id: Uuid) -> PathBuf {
        self.work_root.join("logs").join(format!("{job_id}.log"))
    }

    /// Persist a new queued job and enqueue it for execution.
    pub async fn submit(
        &self,
        job_id: Uuid,
        mission_id: Uuid,
        payload: JobPayload,
    ) -> anyhow::Result<()> {
        let payload_json = serde_json::to_string(&payload)?;
        // The dispatcher drains the mpsc channel into semaphore waiters so a
        // cancelled queued job releases its channel slot immediately. Keep an
        // explicit atomic bound across both locations.
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                (queued < self.max_queued).then_some(queued + 1)
            })
            .map_err(|_| NodeQueueFull {
                max_queued: self.max_queued as usize,
            })?;
        let log_path = self.log_path(job_id);
        if let Err(error) = self
            .store
            .create(
                job_id,
                mission_id,
                payload_json,
                log_path.display().to_string(),
            )
            .await
        {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id, CancellationToken::new());
        if self
            .tx
            .send(QueuedJob {
                id: job_id,
                mission_id,
                payload,
            })
            .is_err()
        {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            self.drop_cancel_token(job_id);
            self.store
                .finish(
                    job_id,
                    JobState::Lost,
                    None,
                    Some("node dispatcher stopped before enqueue".to_string()),
                )
                .await?;
            anyhow::bail!("node dispatcher stopped before enqueue");
        }
        Ok(())
    }

    /// Request cancellation of a queued or running job. Returns whether a
    /// live job received the request.
    pub async fn cancel(&self, job_id: Uuid) -> anyhow::Result<bool> {
        let token = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job_id)
            .cloned();
        let Some(token) = token else {
            return Ok(false);
        };
        token.cancel();
        if self.store.cancel_if_queued(job_id).await? {
            self.queued.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(true)
    }

    fn take_cancel_token(&self, job_id: Uuid) -> CancellationToken {
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job_id)
            .cloned()
            .unwrap_or_default()
    }

    fn drop_cancel_token(&self, job_id: Uuid) {
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job_id);
    }

    async fn run_one(&self, job: QueuedJob) {
        let token = self.take_cancel_token(job.id);
        let (state, exit_code, error, artifacts_json) = if token.is_cancelled() {
            (
                JobState::Cancelled,
                None,
                Some("cancelled before start".to_string()),
                None,
            )
        } else {
            match self.execute(&job, &token).await {
                Ok(outcome) => outcome,
                Err(err) => (JobState::Failed, None, Some(err.to_string()), None),
            }
        };
        // Execution is over, so cancellation must no longer report that it
        // reached a live job even while the terminal row is being persisted.
        self.drop_cancel_token(job.id);
        if let Err(err) = self
            .store
            .finish_with_artifacts(job.id, state, exit_code, error, artifacts_json)
            .await
        {
            tracing::warn!(job_id = %job.id, "failed to persist job outcome: {err}");
        }
    }

    async fn execute(
        &self,
        job: &QueuedJob,
        token: &CancellationToken,
    ) -> anyhow::Result<(JobState, Option<i32>, Option<String>, Option<String>)> {
        let log_path = self.log_path(job.id);
        match &job.payload {
            JobPayload::RawCommand {
                command,
                timeout_secs,
                env,
            } => {
                let mission_dir = self.work_root.join(job.mission_id.to_string());
                tokio::fs::create_dir_all(&mission_dir).await?;

                let cmd = crate::remote_node::raw_command(command, &mission_dir, env.as_ref());
                let limit_secs = clamp_timeout(*timeout_secs, self.max_job_secs);
                let outcome = run_logged_command(
                    cmd,
                    CommandEnvironment::Clear,
                    &log_path,
                    limit_secs,
                    token,
                )
                .await?;
                let (state, exit_code, error) = outcome.into_job_result();
                Ok((state, exit_code, error, None))
            }
            JobPayload::LeanBuild { .. } => {
                let result = super::lean::execute_lean_build(
                    &self.work_root,
                    &log_path,
                    &job.payload,
                    self.max_job_secs,
                    token,
                )
                .await?;
                let artifacts_json = if result.artifacts.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&result.artifacts)?)
                };
                Ok((result.state, result.exit_code, result.error, artifacts_json))
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("node job queue is full (maximum {max_queued} queued jobs)")]
pub struct NodeQueueFull {
    pub max_queued: usize,
}

/// Clamp a client-requested timeout to the node ceiling (min 1s).
pub(crate) fn clamp_timeout(requested: Option<u64>, max_job_secs: u64) -> u64 {
    requested
        .map(|requested| requested.min(max_job_secs))
        .unwrap_or(max_job_secs)
        .max(1)
}

/// Outcome of one supervised child process run (see [`run_logged_command`]).
pub(crate) enum RunOutcome {
    Exited(Option<i32>),
    Cancelled,
    TimedOut { limit_secs: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandEnvironment {
    Inherit,
    Clear,
}

impl RunOutcome {
    /// Whether the process ran to completion with exit code 0.
    pub(crate) fn success(&self) -> bool {
        matches!(self, RunOutcome::Exited(Some(0)))
    }

    /// Map a run outcome to the `(state, exit_code, error)` triple persisted
    /// on the job record.
    pub(crate) fn into_job_result(self) -> (JobState, Option<i32>, Option<String>) {
        match self {
            RunOutcome::Exited(Some(0)) => (JobState::Succeeded, Some(0), None),
            RunOutcome::Exited(code) => (
                JobState::Failed,
                code,
                Some(format!("command exited with {code:?}")),
            ),
            RunOutcome::Cancelled => (JobState::Cancelled, None, Some("cancelled".to_string())),
            RunOutcome::TimedOut { limit_secs } => (
                JobState::Failed,
                None,
                Some(format!("timed out after {limit_secs}s")),
            ),
        }
    }
}

/// Spawn `cmd` in its own process group with combined stdout+stderr appended
/// to `log_path`, honoring cancellation and a hard timeout. The whole process
/// group is killed (SIGTERM, then SIGKILL after a grace period) on
/// cancel/timeout and after the leader exits normally. Shared by raw-command
/// jobs and the lean-build steps.
pub(crate) async fn run_logged_command(
    cmd: tokio::process::Command,
    environment: CommandEnvironment,
    log_path: &Path,
    limit_secs: u64,
    token: &CancellationToken,
) -> anyhow::Result<RunOutcome> {
    if let Some(parent) = log_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let stderr_file = stdout_file.try_clone()?;
    let (mut cmd, systemd_scope) = contain_command(cmd, environment)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        // New process group so cancel/timeout can kill the whole tree.
        .process_group(0);
    let mut child = cmd.spawn()?;
    let pid = child.id();

    let outcome = tokio::select! {
        _ = token.cancelled() => {
            kill_contained_process(systemd_scope.as_ref(), pid, &mut child).await;
            RunOutcome::Cancelled
        }
        waited = tokio::time::timeout(Duration::from_secs(limit_secs.max(1)), child.wait()) => {
            match waited {
                Ok(Ok(status)) => {
                    // The process-group leader may exit after daemonizing a
                    // child. A terminal job must not leave those descendants
                    // consuming node resources outside queue accounting.
                    kill_contained_process(systemd_scope.as_ref(), pid, &mut child).await;
                    RunOutcome::Exited(status.code())
                }
                Ok(Err(err)) => return Err(err.into()),
                Err(_) => {
                    kill_contained_process(systemd_scope.as_ref(), pid, &mut child).await;
                    RunOutcome::TimedOut { limit_secs }
                }
            }
        }
    };
    Ok(outcome)
}

/// Put node jobs in a transient systemd scope when the host has a running
/// system manager. Process groups do not contain descendants that call
/// `setsid`; a scope's cgroup does, so stopping the unit also reaps those
/// daemonized descendants. Commands that clear their environment are marked
/// explicitly because `std::process::Command` does not expose that setting.
fn contain_command(
    cmd: tokio::process::Command,
    environment: CommandEnvironment,
) -> std::io::Result<(tokio::process::Command, Option<SystemdScope>)> {
    // Cargo's lib and binary test harnesses do not run sandboxed-node's main,
    // so they cannot service the hidden trampoline entrypoint. Construction
    // is covered directly below; execution tests retain the process-group
    // fallback for clear-env commands.
    if environment == CommandEnvironment::Clear
        && std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.parent()
                    .and_then(Path::file_name)
                    .map(ToOwned::to_owned)
            })
            .as_deref()
            == Some(std::ffi::OsStr::new("deps"))
    {
        return Ok((cmd, None));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(mode) = systemd_scope_mode() {
            let scope = SystemdScope {
                unit: format!("sandboxed-node-job-{}.scope", Uuid::new_v4().simple()),
                mode,
            };
            return Ok((
                systemd_scope_command(cmd, environment, &scope)?,
                Some(scope),
            ));
        }
    }
    Ok((cmd, None))
}

#[cfg(target_os = "linux")]
fn systemd_scope_mode() -> Option<SystemdScopeMode> {
    // Merely seeing systemd's runtime directory is insufficient in containers
    // and CI runners. Root can use the system manager. A hardened non-root
    // node uses its lingering user manager, exposed through XDG_RUNTIME_DIR;
    // without either manager we retain the process-group fallback.
    if !Path::new("/run/systemd/system").is_dir() {
        return None;
    }
    // SAFETY: geteuid() is a side-effect-free syscall with no preconditions.
    if unsafe { libc::geteuid() == 0 } {
        return Some(SystemdScopeMode::System);
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    match user_systemd_scope_mode(Path::new(&runtime_dir)) {
        Ok(mode) => Some(mode),
        Err(error) => {
            static WARN_DEGRADED_CONTAINMENT: std::sync::Once = std::sync::Once::new();
            WARN_DEGRADED_CONTAINMENT.call_once(|| {
                tracing::warn!(
                    bus = %Path::new(&runtime_dir).join("bus").display(),
                    %error,
                    "transient user scopes are unavailable; containment degraded to process groups; ProtectHome=true may be hiding /run/user (bind the runner's /run/user/%U into the unit)"
                );
            });
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn user_systemd_scope_mode(runtime_dir: &Path) -> std::io::Result<SystemdScopeMode> {
    // Connecting verifies that the path is both visible and usable by this
    // process. A metadata/existence check can pass for an inaccessible,
    // stale, or non-socket path and make systemd-run fail later per job.
    std::os::unix::net::UnixStream::connect(runtime_dir.join("bus"))?;
    Ok(SystemdScopeMode::User)
}

#[cfg(target_os = "linux")]
fn systemd_scope_command(
    cmd: tokio::process::Command,
    environment: CommandEnvironment,
    scope: &SystemdScope,
) -> std::io::Result<tokio::process::Command> {
    let command = cmd.as_std();
    let program = command.get_program().to_os_string();
    let args: Vec<_> = command.get_args().map(ToOwned::to_owned).collect();
    let cwd = command.get_current_dir().map(ToOwned::to_owned);
    let env: Vec<_> = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(ToOwned::to_owned)))
        .collect();

    let mut scoped = tokio::process::Command::new("systemd-run");
    if scope.mode == SystemdScopeMode::User {
        scoped.arg("--user");
    }
    scoped
        .arg("--scope")
        .arg("--quiet")
        .arg("--collect")
        .arg(format!("--unit={}", scope.unit))
        .arg("--property=KillMode=control-group")
        .arg("--");
    if environment == CommandEnvironment::Clear {
        // With --scope, systemd-run executes the payload itself, so the
        // payload otherwise inherits the runner service's environment. Keep
        // systemd-run's own environment intact for the user bus, but reset
        // the environment at the payload boundary and add back only the
        // command's explicitly configured entries.
        scoped
            .arg(std::env::current_exe()?)
            .arg("--sandboxed-scope-exec-cleared-env");
        for (key, value) in &env {
            if let Some(value) = value {
                scoped.arg(key).env(key, value);
            }
        }
        scoped.arg("--");
    }
    scoped.arg(program).args(args);
    if let Some(cwd) = cwd {
        scoped.current_dir(cwd);
    }
    if environment == CommandEnvironment::Inherit {
        for (key, value) in env {
            match value {
                Some(value) => {
                    scoped.env(key, value);
                }
                None => {
                    scoped.env_remove(key);
                }
            }
        }
    }
    Ok(scoped)
}

/// Replace the node process with a scope payload whose environment contains
/// only the named variables. Values stay in the inherited environment until
/// `exec`, never in argv. Returns `Ok(false)` for a normal node invocation.
pub fn maybe_exec_cleared_scope_payload() -> std::io::Result<bool> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--sandboxed-scope-exec-cleared-env")) {
        return Ok(false);
    }

    let mut keys = Vec::new();
    loop {
        let arg = args.next().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing scope payload")
        })?;
        if arg == "--" {
            break;
        }
        keys.push(arg);
    }
    let program = args.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "missing scope payload program",
        )
    })?;
    let env = keys
        .into_iter()
        .filter_map(|key| std::env::var_os(&key).map(|value| (key, value)))
        .collect::<Vec<_>>();
    let mut command = std::process::Command::new(program);
    command.args(args).env_clear().envs(env);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    }
    #[cfg(not(unix))]
    Ok(true)
}

async fn kill_contained_process(
    _systemd_scope: Option<&SystemdScope>,
    pid: Option<u32>,
    child: &mut tokio::process::Child,
) {
    #[cfg(target_os = "linux")]
    if let Some(scope) = _systemd_scope {
        let mut stop_command = tokio::process::Command::new("systemctl");
        if scope.mode == SystemdScopeMode::User {
            stop_command.arg("--user");
        }
        let stop = stop_command
            .arg("stop")
            .arg(&scope.unit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match tokio::time::timeout(KILL_GRACE, stop).await {
            Ok(Ok(status)) if !status.success() => {
                tracing::warn!(scope = %scope.unit, %status, "failed to stop node job systemd scope");
            }
            Ok(Err(error)) => {
                tracing::warn!(scope = %scope.unit, %error, "could not stop node job systemd scope");
            }
            Err(_) => {
                tracing::warn!(scope = %scope.unit, "timed out stopping node job systemd scope");
            }
            Ok(Ok(_)) => {}
        }
    }
    kill_process_group(pid, child).await;
}

/// SIGTERM the job's process group, escalating to SIGKILL after a grace
/// period. Falls back to killing the direct child when the pid is unknown.
async fn kill_process_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if !wait_for_process_group_exit(pid, child, KILL_GRACE).await {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            if !wait_for_process_group_exit(pid, child, Duration::from_secs(1)).await {
                tracing::warn!(
                    process_group = pid,
                    "job process group still exists after SIGKILL"
                );
            }
        }
        let _ = child.wait().await;
    } else {
        let _ = child.kill().await;
    }
}

fn process_group_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(-(pid as i32), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

async fn wait_for_process_group_exit(
    pid: u32,
    child: &mut tokio::process::Child,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // Reap the leader promptly; an unreaped zombie keeps the process
        // group observable even after every live process has exited.
        let _ = child.try_wait();
        if !process_group_exists(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Read up to the last `LOG_TAIL_MAX_BYTES` of a job log file.
pub async fn read_log_tail(path: &Path) -> Option<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(&path).ok()?;
        let len = file.metadata().ok()?.len();
        let start = len.saturating_sub(LOG_TAIL_MAX_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_a_job_and_captures_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        let runner = JobRunner::spawn(
            store.clone(),
            dir.path().to_path_buf(),
            1,
            DEFAULT_MAX_JOB_SECS,
        );
        let job_id = Uuid::new_v4();
        let mission_id = Uuid::new_v4();
        runner
            .submit(
                job_id,
                mission_id,
                JobPayload::RawCommand {
                    command: "echo hello-from-job && pwd".to_string(),
                    timeout_secs: Some(30),
                    env: None,
                },
            )
            .await
            .unwrap();

        let record = wait_for_terminal(&store, job_id).await;
        assert_eq!(record.state, JobState::Succeeded);
        assert_eq!(record.exit_code, Some(0));
        let tail = read_log_tail(&runner.log_path(job_id)).await.unwrap();
        assert!(tail.contains("hello-from-job"));
        // The command ran inside the per-mission directory.
        assert!(tail.contains(&mission_id.to_string()));
        assert_eq!(runner.active_count(), 0);
        assert_eq!(runner.queued_count(), 0);
    }

    #[tokio::test]
    async fn cancel_kills_a_running_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        let runner = JobRunner::spawn(
            store.clone(),
            dir.path().to_path_buf(),
            1,
            DEFAULT_MAX_JOB_SECS,
        );
        let job_id = Uuid::new_v4();
        runner
            .submit(
                job_id,
                Uuid::new_v4(),
                JobPayload::RawCommand {
                    command: "sleep 30".to_string(),
                    timeout_secs: None,
                    env: None,
                },
            )
            .await
            .unwrap();

        // Wait for the job to actually start, then cancel it.
        for _ in 0..100 {
            if matches!(
                store.get(job_id).await.unwrap().map(|r| r.state),
                Some(JobState::Running)
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(runner.cancel(job_id).await.unwrap());

        let record = wait_for_terminal(&store, job_id).await;
        assert_eq!(record.state, JobState::Cancelled);
        // Cancelling an already-finished (deregistered) job reports false.
        assert!(!runner.cancel(job_id).await.unwrap());
    }

    #[tokio::test]
    async fn cancelling_queued_job_is_terminal_and_releases_capacity_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        let runner = JobRunner::spawn(
            store.clone(),
            dir.path().to_path_buf(),
            1,
            DEFAULT_MAX_JOB_SECS,
        );
        let running = Uuid::new_v4();
        let queued = Uuid::new_v4();
        for id in [running, queued] {
            runner
                .submit(
                    id,
                    Uuid::new_v4(),
                    JobPayload::RawCommand {
                        command: "sleep 30".to_string(),
                        timeout_secs: None,
                        env: None,
                    },
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(
                    store.get(running).await.unwrap().map(|record| record.state),
                    Some(JobState::Running)
                ) && matches!(
                    store.get(queued).await.unwrap().map(|record| record.state),
                    Some(JobState::Queued)
                ) && runner.active_count() == 1
                    && runner.queued_count() == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("first job should start while the second remains queued");

        assert_eq!(runner.queued_count(), 1);
        assert!(runner.cancel(queued).await.unwrap());
        let cancelled = store.get(queued).await.unwrap().unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(cancelled.finished_at.is_some());
        assert_eq!(runner.queued_count(), 0);

        assert!(runner.cancel(running).await.unwrap());
        assert_eq!(
            wait_for_terminal(&store, running).await.state,
            JobState::Cancelled
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runner.queued_count(), 0);
        assert_eq!(runner.active_count(), 0);
    }

    #[tokio::test]
    async fn timeout_fails_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).await.unwrap();
        // Node-level 1s ceiling clamps the requested timeout.
        let runner = JobRunner::spawn(store.clone(), dir.path().to_path_buf(), 1, 1);
        let job_id = Uuid::new_v4();
        runner
            .submit(
                job_id,
                Uuid::new_v4(),
                JobPayload::RawCommand {
                    command: "sleep 20".to_string(),
                    timeout_secs: Some(600),
                    env: None,
                },
            )
            .await
            .unwrap();
        let record = wait_for_terminal(&store, job_id).await;
        assert_eq!(record.state, JobState::Failed);
        assert!(record.error.as_deref().unwrap_or("").contains("timed out"));
    }

    #[tokio::test]
    async fn normal_exit_kills_background_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("background-survived");
        let log = dir.path().join("job.log");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "(sleep 1; echo survived > \"$SURVIVOR_MARKER\") &"])
            .env("SURVIVOR_MARKER", &marker);

        let outcome = run_logged_command(
            cmd,
            CommandEnvironment::Inherit,
            &log,
            30,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(outcome.success());
        tokio::time::sleep(Duration::from_millis(1_250)).await;
        assert!(
            !marker.exists(),
            "background descendant survived the terminal job"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_scope_wrapper_keeps_environment_out_of_argv() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "printf ok"])
            .current_dir("/")
            .env("NODE_JOB_SECRET", "not-in-argv");

        let scoped = systemd_scope_command(
            cmd,
            CommandEnvironment::Inherit,
            &SystemdScope {
                unit: "sandboxed-node-job-test.scope".to_string(),
                mode: SystemdScopeMode::User,
            },
        )
        .unwrap();
        let argv = scoped
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(argv.iter().any(|arg| arg == "--user"));
        assert!(argv.iter().any(|arg| arg == "/bin/sh"));
        assert!(argv.iter().any(|arg| arg == "printf ok"));
        assert!(!argv.iter().any(|arg| arg.contains("not-in-argv")));
        assert_eq!(scoped.as_std().get_current_dir(), Some(Path::new("/")));
        assert!(scoped
            .as_std()
            .get_envs()
            .any(|(key, value)| key == "NODE_JOB_SECRET" && value == Some("not-in-argv".as_ref())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_scope_wrapper_preserves_cleared_payload_environment() {
        let mut cmd = tokio::process::Command::new("/usr/bin/env");
        cmd.env_clear().env("EXPLICIT_JOB_ENV", "present");

        let scoped = systemd_scope_command(
            cmd,
            CommandEnvironment::Clear,
            &SystemdScope {
                unit: "sandboxed-node-job-test.scope".to_string(),
                mode: SystemdScopeMode::User,
            },
        )
        .unwrap();
        let argv = scoped
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        let payload = argv
            .iter()
            .position(|arg| arg == "--")
            .map(|index| &argv[index + 1..])
            .expect("systemd-run payload separator");
        assert!(payload
            .iter()
            .any(|arg| arg == "--sandboxed-scope-exec-cleared-env"));
        assert!(payload.iter().any(|arg| arg == "EXPLICIT_JOB_ENV"));
        assert!(!payload.iter().any(|arg| arg.contains("present")));
        assert!(!payload
            .iter()
            .any(|arg| arg.starts_with("SANDBOXED_NODE_TOKEN=")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_scope_probe_requires_an_accessible_bus_socket() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bus"), b"not a socket").unwrap();

        assert!(user_systemd_scope_mode(dir.path()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_scope_probe_selects_user_mode_for_accessible_bus() {
        let dir = tempfile::tempdir().unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(dir.path().join("bus")).unwrap();

        assert_eq!(
            user_systemd_scope_mode(dir.path()).unwrap(),
            SystemdScopeMode::User
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn systemd_scope_kills_setsid_descendants() {
        if systemd_scope_mode().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("setsid-survived");
        let log = dir.path().join("job.log");
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "setsid sh -c 'sleep 5; echo survived > \"$SURVIVOR_MARKER\"' &",
        ])
        .env("SURVIVOR_MARKER", &marker);

        let outcome = run_logged_command(
            cmd,
            CommandEnvironment::Inherit,
            &log,
            30,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(outcome.success());
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!marker.exists(), "setsid descendant escaped the job cgroup");
    }

    async fn wait_for_terminal(store: &JobStore, job_id: Uuid) -> super::super::JobRecord {
        for _ in 0..600 {
            if let Some(record) = store.get(job_id).await.unwrap() {
                if record.state.is_terminal() {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job {job_id} did not reach a terminal state in time");
    }
}
