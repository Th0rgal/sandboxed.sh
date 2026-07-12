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

async fn load_result(working_dir: &Path) -> anyhow::Result<Vec<JobHandle>> {
    let path = ledger_path(working_dir);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| anyhow::anyhow!("parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(anyhow::anyhow!("read {}: {err}", path.display())),
    }
}

pub async fn load(working_dir: &Path) -> Vec<JobHandle> {
    load_result(working_dir).await.unwrap_or_else(|err| {
        tracing::warn!(
            ?err,
            "remote job ledger unreadable; keeping missions unchanged"
        );
        Vec::new()
    })
}

async fn store(working_dir: &Path, handles: &[JobHandle]) -> anyhow::Result<()> {
    let path = ledger_path(working_dir);
    let bytes = serde_json::to_vec_pretty(handles)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Record a job handle (idempotent on job_id).
pub async fn record(working_dir: &Path, handle: JobHandle) -> anyhow::Result<()> {
    let _guard = lock().lock().await;
    let mut handles = load_result(working_dir).await?;
    handles.retain(|h| h.job_id != handle.job_id);
    handles.push(handle);
    store(working_dir, &handles).await
}

/// Remove a job handle once its mission is finalized.
pub async fn remove(working_dir: &Path, job_id: Uuid) {
    let _guard = lock().lock().await;
    let mut handles = load(working_dir).await;
    let before = handles.len();
    handles.retain(|h| h.job_id != job_id);
    if handles.len() != before {
        if let Err(err) = store(working_dir, &handles).await {
            tracing::warn!(?err, "remote job ledger removal failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_is_durable_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let handle = JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id,
            started_at: chrono::Utc::now(),
        };

        record(dir.path(), handle.clone()).await.unwrap();
        record(dir.path(), handle).await.unwrap();
        let handles = load_result(dir.path()).await.unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].job_id, job_id);
    }

    #[tokio::test]
    async fn record_surfaces_an_unwritable_ledger_path() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join(".sandboxed-sh"), b"not a directory")
            .await
            .unwrap();
        let result = record(
            dir.path(),
            JobHandle {
                mission_id: Uuid::new_v4(),
                node_id: "node-a".to_string(),
                job_id: Uuid::new_v4(),
                started_at: chrono::Utc::now(),
            },
        )
        .await;
        assert!(result.is_err());
    }
}
