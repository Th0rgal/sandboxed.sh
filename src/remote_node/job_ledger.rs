//! Durable ledger of in-flight async remote jobs.
//!
//! `dispatch_remote_job` records a handle after the node accepts a job and
//! removes it once the poll loop finalizes the mission. If the API process
//! restarts in between, the startup reconciler reads this file and either
//! re-attaches a poll loop to the still-Active mission or converges its
//! state — without it, the only copy of (node_id, job_id) died with the
//! in-memory task and the mission stayed Active forever.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHandle {
    pub mission_id: Uuid,
    pub node_id: String,
    pub job_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

fn ledger_path(working_dir: &Path) -> PathBuf {
    working_dir.join(".sandboxed-sh").join("remote-jobs.json")
}

/// Serializes read-modify-write cycles within this process. Cross-process
/// concurrency is not a concern: one API instance owns its working dir.
fn lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub async fn load(working_dir: &Path) -> Vec<JobHandle> {
    let path = ledger_path(working_dir);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), ?err, "remote job ledger unreadable; ignoring");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

async fn store(working_dir: &Path, handles: &[JobHandle]) {
    let path = ledger_path(working_dir);
    let Ok(bytes) = serde_json::to_vec_pretty(handles) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = tokio::fs::write(&tmp, &bytes).await {
        tracing::warn!(path = %tmp.display(), ?err, "remote job ledger write failed");
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp, &path).await {
        tracing::warn!(path = %path.display(), ?err, "remote job ledger rename failed");
    }
}

/// Record a job handle (idempotent on job_id).
pub async fn record(working_dir: &Path, handle: JobHandle) {
    let _guard = lock().lock().await;
    let mut handles = load(working_dir).await;
    handles.retain(|h| h.job_id != handle.job_id);
    handles.push(handle);
    store(working_dir, &handles).await;
}

/// Remove a job handle once its mission is finalized.
pub async fn remove(working_dir: &Path, job_id: Uuid) {
    let _guard = lock().lock().await;
    let mut handles = load(working_dir).await;
    let before = handles.len();
    handles.retain(|h| h.job_id != job_id);
    if handles.len() != before {
        store(working_dir, &handles).await;
    }
}
