//! Async job runner for the `sandboxed-node` binary.
//!
//! Jobs are submitted to an mpsc queue and executed under a capacity
//! semaphore (`SANDBOXED_NODE_CAPACITY`). Each job runs `bash -lc <command>`
//! in `<workdir>/<mission-id>/` with combined stdout+stderr captured to
//! `<workdir>/logs/<job-id>.log`. Jobs are killed by process *group* on
//! cancel or timeout (SIGTERM, then SIGKILL after 10s).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    tx: mpsc::Sender<QueuedJob>,
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
        let max_queued = std::env::var("SANDBOXED_NODE_MAX_QUEUED")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or_else(|| (capacity.max(1) as usize).saturating_mul(4));
        let (tx, mut rx) = mpsc::channel::<QueuedJob>(max_queued);
        let runner = Arc::new(Self {
            store,
            work_root,
            max_job_secs: max_job_secs.max(1),
            tx,
            cancels: Mutex::new(HashMap::new()),
            queued: AtomicU32::new(0),
            active: AtomicU32::new(0),
        });
        let dispatcher = Arc::clone(&runner);
        tokio::spawn(async move {
            let semaphore = Arc::new(Semaphore::new(capacity.max(1) as usize));
            while let Some(job) = rx.recv().await {
                let permit = match Arc::clone(&semaphore).acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break, // semaphore closed: shutting down
                };
                let runner = Arc::clone(&dispatcher);
                tokio::spawn(async move {
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
        // Reserve bounded queue capacity before persisting the queued row. The
        // permit prevents concurrent submitters from all passing an advisory
        // heartbeat check and growing memory/jobs.db without limit.
        let permit = self.tx.try_reserve().map_err(|_| NodeQueueFull {
            max_queued: self.tx.max_capacity(),
        })?;
        let payload_json = serde_json::to_string(&payload)?;
        let log_path = self.log_path(job_id);
        self.store
            .create(
                job_id,
                mission_id,
                payload_json,
                log_path.display().to_string(),
            )
            .await?;
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id, CancellationToken::new());
        self.queued.fetch_add(1, Ordering::AcqRel);
        permit.send(QueuedJob {
            id: job_id,
            mission_id,
            payload,
        });
        Ok(())
    }

    /// Request cancellation of a queued or running job. Returns whether a
    /// live job received the request.
    pub fn cancel(&self, job_id: Uuid) -> bool {
        let cancels = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
        match cancels.get(&job_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
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
        if let Err(err) = self
            .store
            .finish_with_artifacts(job.id, state, exit_code, error, artifacts_json)
            .await
        {
            tracing::warn!(job_id = %job.id, "failed to persist job outcome: {err}");
        }
        self.drop_cancel_token(job.id);
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

                self.store.mark_running(job.id).await?;

                let cmd = crate::remote_node::raw_command(command, &mission_dir, env.as_ref());
                let limit_secs = clamp_timeout(*timeout_secs, self.max_job_secs);
                let outcome = run_logged_command(cmd, &log_path, limit_secs, token).await?;
                let (state, exit_code, error) = outcome.into_job_result();
                Ok((state, exit_code, error, None))
            }
            JobPayload::LeanBuild { .. } => {
                self.store.mark_running(job.id).await?;
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
/// cancel/timeout. Shared by raw-command jobs and the lean-build steps.
pub(crate) async fn run_logged_command(
    mut cmd: tokio::process::Command,
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
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        // New process group so cancel/timeout can kill the whole tree.
        .process_group(0);
    let mut child = cmd.spawn()?;
    let pid = child.id();

    let outcome = tokio::select! {
        _ = token.cancelled() => {
            kill_process_group(pid, &mut child).await;
            RunOutcome::Cancelled
        }
        waited = tokio::time::timeout(Duration::from_secs(limit_secs.max(1)), child.wait()) => {
            match waited {
                Ok(Ok(status)) => RunOutcome::Exited(status.code()),
                Ok(Err(err)) => return Err(err.into()),
                Err(_) => {
                    kill_process_group(pid, &mut child).await;
                    RunOutcome::TimedOut { limit_secs }
                }
            }
        }
    };
    Ok(outcome)
}

/// SIGTERM the job's process group, escalating to SIGKILL after a grace
/// period. Falls back to killing the direct child when the pid is unknown.
async fn kill_process_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(KILL_GRACE, child.wait())
            .await
            .is_err()
        {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.wait().await;
        }
    } else {
        let _ = child.kill().await;
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
        assert!(runner.cancel(job_id));

        let record = wait_for_terminal(&store, job_id).await;
        assert_eq!(record.state, JobState::Cancelled);
        // Cancelling an already-finished (deregistered) job reports false.
        assert!(!runner.cancel(job_id));
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
