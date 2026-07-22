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
    /// `false` means this identity predates cwd persistence. Such receipts are
    /// intentionally not equal to new root-cwd requests because their actual
    /// execution directory is unknowable after upgrade.
    #[serde(default)]
    pub cwd_rel_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_rel: Option<String>,
    pub command: Vec<String>,
    /// Requested artifact digests are part of execution identity. Terminal
    /// receipt reuse is disabled when this is non-empty until receipts retain
    /// the resulting artifact entries as well.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
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
    /// The submitting harness no longer waits on the HTTP response, so the
    /// terminal receipt must wake the owning mission. Recovered remote builds
    /// are promoted to this mode because a restart severed any synchronous
    /// waiter that may have existed.
    #[serde(default)]
    pub wake_on_terminal: bool,
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
    /// Remote-build receipts are also a durable wake-up outbox. Legacy
    /// receipts predate that contract and default to `false`, so upgrading
    /// never replays old validations into missions.
    #[serde(default)]
    pub wake_required: bool,
    /// Set only after the owning mission has durably accepted the terminal
    /// continuation (live control queue or persisted deferred goal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// A newer validation for the same mission made this terminal callback
    /// obsolete before it was delivered. The receipt remains immutable build
    /// evidence, but it must not close the newer wait lease or start a stale
    /// continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_suppressed_by: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalWakeDisposition {
    Ready,
    /// Another remote validation is still active for the same mission. The
    /// terminal callback must wait so it cannot close that job's run lease.
    Deferred,
    /// A later validation has taken ownership of the mission's continuation.
    SupersededBy(Uuid),
}

/// The one unresolved remote build, if any, that is still allowed to own a
/// mission's `waiting_remote_job` lease. A terminal receipt from a later
/// validation supersedes an older in-flight handle: the old node job must
/// still be observed to terminal, but it must not resurrect or park the
/// mission after the newer validation has already advanced the workflow.
pub async fn current_remote_build_wait_handle(
    working_dir: &Path,
    mission_id: Uuid,
) -> anyhow::Result<Option<JobHandle>> {
    let _guard = lock().lock().await;
    let handles = load_result(working_dir).await?;
    let receipts = load_receipts_result(working_dir).await?;

    let current_handle = handles
        .iter()
        .filter(|handle| {
            handle.mission_id == mission_id
                && handle.kind == JobHandleKind::RemoteBuild
                && handle.accepted_at.is_some()
        })
        .max_by_key(|handle| (handle.started_at, handle.job_id));
    let Some(current_handle) = current_handle else {
        return Ok(None);
    };

    let newer_terminal_exists = receipts.iter().any(|receipt| {
        receipt.mission_id == mission_id
            && (receipt.started_at, receipt.job_id)
                > (current_handle.started_at, current_handle.job_id)
    });
    let newer_ambiguous_submission_exists = handles.iter().any(|handle| {
        handle.mission_id == mission_id
            && handle.kind == JobHandleKind::Tentative
            && (handle.started_at, handle.job_id)
                > (current_handle.started_at, current_handle.job_id)
    });

    if newer_terminal_exists || newer_ambiguous_submission_exists {
        Ok(None)
    } else {
        Ok(Some(current_handle.clone()))
    }
}

#[derive(Debug, Clone)]
pub enum EquivalentRemoteValidation {
    /// An accepted or ambiguously submitted job with the same immutable
    /// validation identity is still unresolved. A new submission must fail
    /// closed until that canonical job is reconciled.
    Active(JobHandle),
    /// A successful terminal receipt already proves this exact validation.
    /// Callers may replay it without consuming remote capacity.
    Succeeded(RemoteJobReceipt),
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

pub async fn terminal_receipts_for_mission(
    working_dir: &Path,
    mission_id: Uuid,
) -> anyhow::Result<Vec<RemoteJobReceipt>> {
    Ok(load_receipts_result(working_dir)
        .await?
        .into_iter()
        .filter(|receipt| receipt.mission_id == mission_id)
        .collect())
}

/// Resolve an immutable validation identity across mission boundaries.
///
/// The ledger and receipt files are inspected under one lock so finalization
/// cannot move a matching job between them during the lookup. Successful
/// receipts take precedence over a redundant in-flight job left by an older
/// client: once exact evidence exists, no third build should be dispatched.
pub async fn equivalent_remote_validation(
    working_dir: &Path,
    identity: &RemoteJobIdentity,
) -> anyhow::Result<Option<EquivalentRemoteValidation>> {
    let _guard = lock().lock().await;
    let receipts = load_receipts_result(working_dir).await?;
    if let Some(receipt) = receipts
        .into_iter()
        .filter(|receipt| {
            receipt.identity == *identity
                && identity.artifacts.is_empty()
                && receipt.state == "succeeded"
                && receipt.exit_status == Some(0)
        })
        .max_by_key(|receipt| receipt.finished_at)
    {
        return Ok(Some(EquivalentRemoteValidation::Succeeded(receipt)));
    }
    Ok(load_result(working_dir)
        .await?
        .into_iter()
        .filter(|handle| {
            handle.identity.as_ref() == Some(identity)
                && matches!(
                    handle.kind,
                    JobHandleKind::RemoteBuild | JobHandleKind::Tentative
                )
        })
        .min_by_key(|handle| handle.started_at)
        .map(EquivalentRemoteValidation::Active))
}

/// Terminal remote-build receipts whose mission continuation has not yet
/// reached a durable queue. The startup reconciler retries these after a
/// process crash, making receipt finalization and mission wake-up effectively
/// an outbox operation rather than a best-effort callback.
pub async fn pending_terminal_wakes(working_dir: &Path) -> anyhow::Result<Vec<RemoteJobReceipt>> {
    Ok(load_receipts_result(working_dir)
        .await?
        .into_iter()
        .filter(|receipt| receipt.wake_required && receipt.wake_delivered_at.is_none())
        .collect())
}

/// Decide whether a durable terminal callback still owns the mission wake.
///
/// Missions may advance their immutable head while an older validation is in
/// flight. After a restart the older handle is armed for recovery, while the
/// resumed harness can submit the newer head in the same run. Delivering the
/// old callback would then finish `waiting_remote_job` underneath the newer
/// synchronous tool. Prefer the newest accepted validation and retain older
/// receipts as evidence only.
pub async fn terminal_wake_disposition(
    working_dir: &Path,
    receipt: &RemoteJobReceipt,
) -> anyhow::Result<TerminalWakeDisposition> {
    let _guard = lock().lock().await;
    let handles = load_result(working_dir).await?;
    let receipts = load_receipts_result(working_dir).await?;

    if let Some(newer) = handles
        .iter()
        .filter(|handle| {
            handle.kind == JobHandleKind::RemoteBuild
                && handle.mission_id == receipt.mission_id
                && handle.job_id != receipt.job_id
                && handle.started_at > receipt.started_at
        })
        .max_by_key(|handle| handle.started_at)
    {
        return Ok(TerminalWakeDisposition::SupersededBy(newer.job_id));
    }
    if let Some(newer) = receipts
        .iter()
        .filter(|candidate| {
            candidate.mission_id == receipt.mission_id
                && candidate.job_id != receipt.job_id
                && candidate.started_at > receipt.started_at
        })
        .max_by_key(|candidate| candidate.started_at)
    {
        return Ok(TerminalWakeDisposition::SupersededBy(newer.job_id));
    }
    if handles.iter().any(|handle| {
        matches!(
            handle.kind,
            JobHandleKind::RemoteBuild | JobHandleKind::Tentative
        ) && handle.mission_id == receipt.mission_id
            && handle.job_id != receipt.job_id
    }) {
        return Ok(TerminalWakeDisposition::Deferred);
    }
    Ok(TerminalWakeDisposition::Ready)
}

pub async fn mark_terminal_wake_suppressed(
    working_dir: &Path,
    job_id: Uuid,
    superseding_job_id: Uuid,
) -> anyhow::Result<bool> {
    let _guard = lock().lock().await;
    let mut receipts = load_receipts_result(working_dir).await?;
    let Some(receipt) = receipts.iter_mut().find(|receipt| receipt.job_id == job_id) else {
        return Ok(false);
    };
    if receipt.wake_delivered_at.is_none() {
        receipt.wake_delivered_at = Some(chrono::Utc::now());
        receipt.wake_suppressed_by = Some(superseding_job_id);
        store_receipts(working_dir, &receipts).await?;
    }
    Ok(true)
}

pub async fn mark_terminal_wake_delivered(
    working_dir: &Path,
    job_id: Uuid,
) -> anyhow::Result<bool> {
    let _guard = lock().lock().await;
    let mut receipts = load_receipts_result(working_dir).await?;
    let Some(receipt) = receipts.iter_mut().find(|receipt| receipt.job_id == job_id) else {
        return Ok(false);
    };
    if receipt.wake_delivered_at.is_none() {
        receipt.wake_delivered_at = Some(chrono::Utc::now());
        store_receipts(working_dir, &receipts).await?;
    }
    Ok(true)
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
    if handle.kind == JobHandleKind::RemoteBuild {
        let Some(identity) = handle.identity else {
            store(working_dir, &handles).await?;
            return Ok(true);
        };
        let mut receipts = load_receipts_result(working_dir).await?;
        let previous_wake_delivered_at = receipts
            .iter()
            .find(|receipt| receipt.job_id == job_id)
            .and_then(|receipt| receipt.wake_delivered_at);
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
            wake_required: handle.kind == JobHandleKind::RemoteBuild && handle.wake_on_terminal,
            wake_delivered_at: previous_wake_delivered_at,
            wake_suppressed_by: None,
        });
        if receipts.len() > MAX_RECEIPTS {
            receipts.sort_by_key(|receipt| receipt.finished_at);
            let mut excess = receipts.len() - MAX_RECEIPTS;
            receipts.retain(|receipt| {
                let pending_wake = receipt.wake_required && receipt.wake_delivered_at.is_none();
                if excess > 0 && !pending_wake {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
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

pub async fn require_terminal_wake(working_dir: &Path, job_id: Uuid) -> anyhow::Result<bool> {
    let _guard = lock().lock().await;
    let mut handles = load_result(working_dir).await?;
    let Some(handle) = handles.iter_mut().find(|handle| handle.job_id == job_id) else {
        return Ok(false);
    };
    if !handle.wake_on_terminal {
        handle.wake_on_terminal = true;
        store(working_dir, &handles).await?;
    }
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
            wake_on_terminal: false,
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
                wake_on_terminal: false,
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

    #[test]
    fn legacy_validation_identity_keeps_cwd_unknown() {
        let identity: RemoteJobIdentity = serde_json::from_value(serde_json::json!({
            "repository": "https://example.invalid/repo.git",
            "commit": "a".repeat(40),
            "command": ["lake", "build"]
        }))
        .unwrap();

        assert!(!identity.cwd_rel_known);
        assert_eq!(identity.cwd_rel, None);
        assert!(identity.artifacts.is_empty());
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
            wake_on_terminal: false,
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
                cwd_rel_known: true,
                cwd_rel: None,
                command: vec!["lake".to_string(), "build".to_string()],
                artifacts: Vec::new(),
                toolchain: None,
                source_bundle_digest: None,
            }),
            wake_on_terminal: false,
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
        let mission_id = Uuid::new_v4();
        let identity = RemoteJobIdentity {
            repository: "https://example.invalid/repo.git".to_string(),
            commit: "a".repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: Some("leanprover/lean4:v4.19.0".to_string()),
            source_bundle_digest: Some("b".repeat(64)),
        };
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-a".to_string(),
                job_id,
                started_at: chrono::Utc::now(),
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wake_on_terminal: true,
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
        assert!(receipt.wake_required);
        assert_eq!(receipt.wake_delivered_at, None);
        assert_eq!(pending_terminal_wakes(dir.path()).await.unwrap().len(), 1);
        assert!(mark_terminal_wake_delivered(dir.path(), job_id)
            .await
            .unwrap());
        assert!(pending_terminal_wakes(dir.path()).await.unwrap().is_empty());
        assert_eq!(
            terminal_receipts_for_mission(dir.path(), mission_id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(terminal_receipts_for_mission(dir.path(), Uuid::new_v4())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn immutable_validation_lookup_blocks_active_and_reuses_success() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let identity = RemoteJobIdentity {
            repository: "https://example.invalid/repo.git".to_string(),
            commit: "a".repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: Some("leanprover/lean4:v4.24.0".to_string()),
            source_bundle_digest: None,
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
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            equivalent_remote_validation(dir.path(), &identity)
                .await
                .unwrap(),
            Some(EquivalentRemoteValidation::Active(handle)) if handle.job_id == job_id
        ));

        finalize(dir.path(), job_id, "succeeded", Some(0))
            .await
            .unwrap();
        assert!(matches!(
            equivalent_remote_validation(dir.path(), &identity)
                .await
                .unwrap(),
            Some(EquivalentRemoteValidation::Succeeded(receipt)) if receipt.job_id == job_id
        ));

        let changed_cwd = RemoteJobIdentity {
            cwd_rel: Some("subproject".to_string()),
            ..identity.clone()
        };
        assert!(equivalent_remote_validation(dir.path(), &changed_cwd)
            .await
            .unwrap()
            .is_none());

        let artifact_identity = RemoteJobIdentity {
            artifacts: vec!["build/report.json".to_string()],
            ..identity.clone()
        };
        let artifact_job_id = Uuid::new_v4();
        record(
            dir.path(),
            JobHandle {
                mission_id: Uuid::new_v4(),
                node_id: "node-b".to_string(),
                job_id: artifact_job_id,
                started_at: chrono::Utc::now(),
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(artifact_identity.clone()),
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();
        finalize(dir.path(), artifact_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        assert!(
            equivalent_remote_validation(dir.path(), &artifact_identity)
                .await
                .unwrap()
                .is_none(),
            "artifact-producing validation cannot replay until receipts retain artifact digests"
        );

        let changed_overlay = RemoteJobIdentity {
            source_bundle_digest: Some("b".repeat(64)),
            ..identity
        };
        assert!(equivalent_remote_validation(dir.path(), &changed_overlay)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn newer_validation_suppresses_stale_terminal_wake() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let old_job_id = Uuid::new_v4();
        let new_job_id = Uuid::new_v4();
        let old_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let new_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let identity = |commit: char| RemoteJobIdentity {
            repository: "https://example.invalid/verity.git".to_string(),
            commit: commit.to_string().repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: Some("leanprover/lean4:v4.24.0".to_string()),
            source_bundle_digest: None,
        };

        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-old".to_string(),
                job_id: old_job_id,
                started_at: old_started_at,
                accepted_at: Some(old_started_at),
                heartbeat_at: Some(old_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('a')),
                wake_on_terminal: true,
            },
        )
        .await
        .unwrap();
        finalize(dir.path(), old_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        let old_receipt = terminal_receipt(dir.path(), old_job_id)
            .await
            .unwrap()
            .unwrap();

        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-new".to_string(),
                job_id: new_job_id,
                started_at: new_started_at,
                accepted_at: Some(new_started_at),
                heartbeat_at: Some(new_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('b')),
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            terminal_wake_disposition(dir.path(), &old_receipt)
                .await
                .unwrap(),
            TerminalWakeDisposition::SupersededBy(new_job_id)
        );
        assert!(
            mark_terminal_wake_suppressed(dir.path(), old_job_id, new_job_id)
                .await
                .unwrap()
        );
        assert!(pending_terminal_wakes(dir.path()).await.unwrap().is_empty());
        let old_receipt = terminal_receipt(dir.path(), old_job_id)
            .await
            .unwrap()
            .unwrap();
        assert!(old_receipt.wake_delivered_at.is_some());
        assert_eq!(old_receipt.wake_suppressed_by, Some(new_job_id));
    }

    #[tokio::test]
    async fn newer_terminal_validation_prevents_old_handle_from_owning_wait_lease() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let old_job_id = Uuid::new_v4();
        let new_job_id = Uuid::new_v4();
        let old_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let new_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let handle = |job_id, started_at| JobHandle {
            mission_id,
            node_id: "node-a".to_string(),
            job_id,
            started_at,
            accepted_at: Some(started_at),
            heartbeat_at: Some(started_at),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: Some(RemoteJobIdentity {
                repository: "https://example.invalid/verity.git".to_string(),
                commit: job_id.to_string(),
                cwd_rel_known: true,
                cwd_rel: None,
                command: vec!["lake".to_string(), "build".to_string()],
                artifacts: Vec::new(),
                toolchain: None,
                source_bundle_digest: None,
            }),
            wake_on_terminal: true,
        };

        record(dir.path(), handle(old_job_id, old_started_at))
            .await
            .unwrap();
        record(dir.path(), handle(new_job_id, new_started_at))
            .await
            .unwrap();
        assert_eq!(
            current_remote_build_wait_handle(dir.path(), mission_id)
                .await
                .unwrap()
                .map(|handle| handle.job_id),
            Some(new_job_id)
        );

        finalize(dir.path(), new_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        assert!(
            current_remote_build_wait_handle(dir.path(), mission_id)
                .await
                .unwrap()
                .is_none(),
            "an older live job is evidence only after a newer validation is terminal"
        );
    }

    #[tokio::test]
    async fn terminal_wake_defers_behind_an_older_active_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let active_job_id = Uuid::new_v4();
        let receipt_job_id = Uuid::new_v4();
        let active_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let receipt_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let identity = RemoteJobIdentity {
            repository: "https://example.invalid/verity.git".to_string(),
            commit: "a".repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: None,
            source_bundle_digest: None,
        };
        for (job_id, started_at, wake_on_terminal) in [
            (active_job_id, active_started_at, false),
            (receipt_job_id, receipt_started_at, true),
        ] {
            record(
                dir.path(),
                JobHandle {
                    mission_id,
                    node_id: "node-a".to_string(),
                    job_id,
                    started_at,
                    accepted_at: Some(started_at),
                    heartbeat_at: Some(started_at),
                    disk_reservation_bytes: 0,
                    kind: JobHandleKind::RemoteBuild,
                    identity: Some(identity.clone()),
                    wake_on_terminal,
                },
            )
            .await
            .unwrap();
        }
        finalize(dir.path(), receipt_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        let receipt = terminal_receipt(dir.path(), receipt_job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal_wake_disposition(dir.path(), &receipt)
                .await
                .unwrap(),
            TerminalWakeDisposition::Deferred
        );
        assert_eq!(pending_terminal_wakes(dir.path()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn newer_tentative_submission_defers_without_suppressing_wake() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let receipt_job_id = Uuid::new_v4();
        let tentative_job_id = Uuid::new_v4();
        let old_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let new_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let identity = RemoteJobIdentity {
            repository: "https://example.invalid/verity.git".to_string(),
            commit: "a".repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: None,
            source_bundle_digest: None,
        };
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-old".to_string(),
                job_id: receipt_job_id,
                started_at: old_started_at,
                accepted_at: Some(old_started_at),
                heartbeat_at: Some(old_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wake_on_terminal: true,
            },
        )
        .await
        .unwrap();
        finalize(dir.path(), receipt_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-new".to_string(),
                job_id: tentative_job_id,
                started_at: new_started_at,
                accepted_at: None,
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Tentative,
                identity: Some(identity),
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();

        let receipt = terminal_receipt(dir.path(), receipt_job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal_wake_disposition(dir.path(), &receipt)
                .await
                .unwrap(),
            TerminalWakeDisposition::Deferred
        );
        assert_eq!(pending_terminal_wakes(dir.path()).await.unwrap().len(), 1);
        assert_eq!(receipt.wake_suppressed_by, None);
    }

    #[tokio::test]
    async fn terminal_tentative_handle_never_becomes_validation_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-a".to_string(),
                job_id,
                started_at: now,
                accepted_at: None,
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Tentative,
                identity: Some(RemoteJobIdentity {
                    repository: "https://example.invalid/verity.git".to_string(),
                    commit: "a".repeat(40),
                    cwd_rel_known: true,
                    cwd_rel: None,
                    command: vec!["lake".to_string(), "build".to_string()],
                    artifacts: Vec::new(),
                    toolchain: None,
                    source_bundle_digest: None,
                }),
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();

        assert!(finalize(dir.path(), job_id, "lost", None).await.unwrap());
        assert!(load_result(dir.path()).await.unwrap().is_empty());
        assert!(terminal_receipt(dir.path(), job_id)
            .await
            .unwrap()
            .is_none());
        assert!(terminal_receipts_for_mission(dir.path(), mission_id)
            .await
            .unwrap()
            .is_empty());
    }
}
