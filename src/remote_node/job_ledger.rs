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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobHandleKind {
    /// A raw remote mission whose terminal result finalizes the mission.
    #[default]
    Mission,
    /// A waited `/api/remote-build` request. Recovery only observes the node
    /// job and cleans up its handle; it must not finalize the agent mission.
    RemoteBuild,
    /// A submit request failed ambiguously after the node may have accepted
    /// the generated job id. Recovery must cancel/poll this handle, but must
    /// never finalize the owning mission from its result.
    Tentative,
}

/// Immutable identity of a remote validation. This deliberately stores a
/// credential-free repository identifier so the durable ledger can prove
/// exactly what was validated without persisting clone credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteJobIdentity {
    pub repository: String,
    pub commit: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bundle_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHandle {
    pub mission_id: Uuid,
    pub node_id: String,
    pub job_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the node acceptance response was observed. Tentative and legacy
    /// handles keep this empty and must remain conservatively reserved.
    #[serde(default)]
    pub accepted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last successful observation of the durable node job. A stale value
    /// means reconciliation is needed; it does not authorize resubmission.
    #[serde(default)]
    pub heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Scratch capacity reserved for this job until it reaches terminal state.
    /// Older ledgers deserialize this as zero.
    #[serde(default)]
    pub disk_reservation_bytes: u64,
    #[serde(default)]
    pub kind: JobHandleKind,
    /// Present for immutable remote build/validation jobs. Legacy and raw
    /// command handles deserialize with no identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RemoteJobIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteJobReceipt {
    pub mission_id: Uuid,
    pub node_id: String,
    pub job_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    #[serde(default)]
    pub exit_status: Option<i32>,
    pub identity: RemoteJobIdentity,
}

fn ledger_path(working_dir: &Path) -> PathBuf {
    working_dir.join(".sandboxed-sh").join("remote-jobs.json")
}

fn receipts_path(working_dir: &Path) -> PathBuf {
    working_dir
        .join(".sandboxed-sh")
        .join("remote-job-receipts.json")
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

pub async fn load(working_dir: &Path) -> anyhow::Result<Vec<JobHandle>> {
    load_result(working_dir).await
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

async fn load_receipts_result(working_dir: &Path) -> anyhow::Result<Vec<RemoteJobReceipt>> {
    let path = receipts_path(working_dir);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| anyhow::anyhow!("parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(anyhow::anyhow!("read {}: {err}", path.display())),
    }
}

async fn store_receipts(working_dir: &Path, receipts: &[RemoteJobReceipt]) -> anyhow::Result<()> {
    let path = receipts_path(working_dir);
    let bytes = serde_json::to_vec_pretty(receipts)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

pub async fn terminal_receipt(
    working_dir: &Path,
    job_id: Uuid,
) -> anyhow::Result<Option<RemoteJobReceipt>> {
    Ok(load_receipts_result(working_dir)
        .await?
        .into_iter()
        .find(|receipt| receipt.job_id == job_id))
}

/// Close an active handle and retain immutable validation evidence. Raw
/// mission and tentative handles are removed without producing a receipt.
pub async fn finalize(
    working_dir: &Path,
    job_id: Uuid,
    state: &str,
    exit_status: Option<i32>,
) -> anyhow::Result<bool> {
    const MAX_RECEIPTS: usize = 2_000;

    let _guard = lock().lock().await;
    let mut handles = load_result(working_dir).await?;
    let Some(index) = handles.iter().position(|handle| handle.job_id == job_id) else {
        return Ok(false);
    };
    let handle = handles.remove(index);
    if let Some(identity) = handle.identity {
        let mut receipts = load_receipts_result(working_dir).await?;
        receipts.retain(|receipt| receipt.job_id != job_id);
        receipts.push(RemoteJobReceipt {
            mission_id: handle.mission_id,
            node_id: handle.node_id,
            job_id,
            started_at: handle.started_at,
            finished_at: chrono::Utc::now(),
            state: state.to_string(),
            exit_status,
            identity,
        });
        if receipts.len() > MAX_RECEIPTS {
            receipts.sort_by_key(|receipt| receipt.finished_at);
            receipts.drain(..receipts.len() - MAX_RECEIPTS);
        }
        store_receipts(working_dir, &receipts).await?;
    }
    store(working_dir, &handles).await?;
    Ok(true)
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
    let mut handles = match load_result(working_dir).await {
        Ok(handles) => handles,
        Err(err) => {
            tracing::warn!(?err, "remote job ledger removal deferred");
            return;
        }
    };
    let before = handles.len();
    handles.retain(|h| h.job_id != job_id);
    if handles.len() != before {
        if let Err(err) = store(working_dir, &handles).await {
            tracing::warn!(?err, "remote job ledger removal failed");
        }
    }
}

/// Advance the durable heartbeat after a successful node status observation.
pub async fn heartbeat(working_dir: &Path, job_id: Uuid) -> anyhow::Result<bool> {
    let _guard = lock().lock().await;
    let mut handles = load_result(working_dir).await?;
    let Some(handle) = handles.iter_mut().find(|handle| handle.job_id == job_id) else {
        return Ok(false);
    };
    let now = chrono::Utc::now();
    if handle
        .heartbeat_at
        .is_some_and(|previous| now - previous < chrono::Duration::seconds(15))
    {
        return Ok(true);
    }
    handle.heartbeat_at = Some(now);
    store(working_dir, &handles).await?;
    Ok(true)
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
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: Some(chrono::Utc::now()),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Mission,
            identity: None,
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
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Mission,
                identity: None,
            },
        )
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn legacy_handles_default_to_mission_kind() {
        let raw = serde_json::json!({
            "mission_id": Uuid::new_v4(),
            "node_id": "node-a",
            "job_id": Uuid::new_v4(),
            "started_at": chrono::Utc::now(),
        });

        let handle: JobHandle = serde_json::from_value(raw).unwrap();

        assert_eq!(handle.kind, JobHandleKind::Mission);
        assert_eq!(handle.accepted_at, None);
        assert_eq!(handle.heartbeat_at, None);
        assert_eq!(handle.identity, None);
    }

    #[test]
    fn tentative_handles_round_trip() {
        let raw = serde_json::json!({
            "mission_id": Uuid::new_v4(),
            "node_id": "node-a",
            "job_id": Uuid::new_v4(),
            "started_at": chrono::Utc::now(),
            "kind": "tentative",
        });

        let handle: JobHandle = serde_json::from_value(raw).unwrap();

        assert_eq!(handle.kind, JobHandleKind::Tentative);
    }

    #[tokio::test]
    async fn accepted_handle_replaces_pre_submit_tentative_handle() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let mission_id = Uuid::new_v4();
        let mut handle = JobHandle {
            mission_id,
            node_id: "node-a".to_string(),
            job_id,
            started_at: chrono::Utc::now(),
            accepted_at: None,
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Tentative,
            identity: None,
        };
        record(dir.path(), handle.clone()).await.unwrap();

        handle.kind = JobHandleKind::Mission;
        handle.accepted_at = Some(chrono::Utc::now());
        record(dir.path(), handle).await.unwrap();

        let handles = load_result(dir.path()).await.unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].job_id, job_id);
        assert_eq!(handles[0].kind, JobHandleKind::Mission);
    }

    #[tokio::test]
    async fn heartbeat_advances_only_an_existing_handle() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let handle = JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id,
            started_at: chrono::Utc::now(),
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: Some(RemoteJobIdentity {
                repository: "https://example.invalid/repo.git".to_string(),
                commit: "a".repeat(40),
                command: vec!["lake".to_string(), "build".to_string()],
                toolchain: None,
                source_bundle_digest: None,
            }),
        };
        record(dir.path(), handle).await.unwrap();

        assert!(heartbeat(dir.path(), job_id).await.unwrap());
        assert!(load_result(dir.path()).await.unwrap()[0]
            .heartbeat_at
            .is_some());
        assert!(!heartbeat(dir.path(), Uuid::new_v4()).await.unwrap());
    }

    #[tokio::test]
    async fn finalization_retains_immutable_remote_validation_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let identity = RemoteJobIdentity {
            repository: "https://example.invalid/repo.git".to_string(),
            commit: "a".repeat(40),
            command: vec!["lake".to_string(), "build".to_string()],
            toolchain: Some("leanprover/lean4:v4.19.0".to_string()),
            source_bundle_digest: Some("b".repeat(64)),
        };
        record(
            dir.path(),
            JobHandle {
                mission_id: Uuid::new_v4(),
                node_id: "node-a".to_string(),
                job_id,
                started_at: chrono::Utc::now(),
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
            },
        )
        .await
        .unwrap();

        assert!(finalize(dir.path(), job_id, "succeeded", Some(0))
            .await
            .unwrap());
        assert!(load_result(dir.path()).await.unwrap().is_empty());
        let receipt = terminal_receipt(dir.path(), job_id).await.unwrap().unwrap();
        assert_eq!(receipt.identity, identity);
        assert_eq!(receipt.state, "succeeded");
        assert_eq!(receipt.exit_status, Some(0));
    }
}
