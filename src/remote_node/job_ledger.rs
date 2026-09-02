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

use super::ArtifactEntry;

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
    /// Identity schema version. `0` = legacy (commit-keyed, no tree); such
    /// receipts are never replayed as success for a newer identity.
    #[serde(default)]
    pub version: u32,
    pub repository: String,
    pub commit: String,
    /// Root tree of `commit`. When present it, not the commit, is the content
    /// identity: the same tree under a different commit message is the same
    /// build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_tree_sha: Option<String>,
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
    /// Builder image / toolchain container digest, when the node runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder_image_digest: Option<String>,
    /// Wire/build protocol revision of the submitting wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_protocol_version: Option<String>,
    /// Digest of the allowlisted, behaviour-affecting, non-secret environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_env_digest: Option<String>,
}

/// Current identity schema version written by core.
pub const IDENTITY_VERSION: u32 = 1;

impl RemoteJobIdentity {
    /// Canonical, byte-stable hash of the identity. `base_tree_sha` replaces
    /// the commit when present; artifacts are sorted and de-duplicated; the
    /// version is part of the hash so a schema change can never alias.
    pub fn identity_hash(&self) -> String {
        use sha2::Digest;
        let mut artifacts = self.artifacts.clone();
        artifacts.sort();
        artifacts.dedup();
        let content = match self.base_tree_sha.as_deref().map(str::trim) {
            Some(tree) if !tree.is_empty() => format!("tree:{}", tree.to_ascii_lowercase()),
            _ => format!("commit:{}", self.commit.to_ascii_lowercase()),
        };
        let canonical = serde_json::json!({
            "v": self.version,
            "repository": self.repository,
            "content": content,
            "cwd_rel": self.cwd_rel,
            "cwd_rel_known": self.cwd_rel_known,
            "argv": self.command,
            "artifacts": artifacts,
            "toolchain": self.toolchain,
            "bundle": self.source_bundle_digest,
            "image": self.builder_image_digest,
            "protocol": self.build_protocol_version,
            "env": self.behavior_env_digest,
        });
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"sandboxed-remote-job-identity\0");
        hasher.update(canonical.to_string().as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Same build for exclusion purposes. Two identities of the same version
    /// compare by hash. A legacy (v0) identity on either side falls back to
    /// the legacy field equality so an in-flight pre-upgrade job still
    /// blocks a duplicate — conservatively, never the other way round.
    pub fn excludes(&self, other: &RemoteJobIdentity) -> bool {
        if self.version == other.version {
            return self.identity_hash() == other.identity_hash();
        }
        self.repository == other.repository
            && self.commit.eq_ignore_ascii_case(&other.commit)
            && self.cwd_rel == other.cwd_rel
            && self.command == other.command
            && self.artifacts == other.artifacts
            && self.toolchain == other.toolchain
            && self.source_bundle_digest == other.source_bundle_digest
    }

    /// Whether a successful receipt with this identity may be replayed for a
    /// request with `other`: same version (never v0) and same hash.
    pub fn reusable_for(&self, other: &RemoteJobIdentity) -> bool {
        self.version >= 1
            && self.version == other.version
            && self.identity_hash() == other.identity_hash()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHandle {
    pub mission_id: Uuid,
    pub node_id: String,
    pub job_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Durable monotonic submission order assigned by the core ledger. This
    /// is authoritative when wall-clock timestamps collide. Legacy handles
    /// deserialize as zero and are treated conservatively when ambiguous.
    #[serde(default)]
    pub submission_sequence: u64,
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
    /// Whether the build owns a mission continuation. This is independent of
    /// the HTTP response mode: the bundled wrapper requests a job id
    /// asynchronously but keeps polling inside the harness tool. `None`
    /// denotes a legacy ledger entry; recovery treats it conservatively so an
    /// upgrade cannot discard an in-flight continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for_completion: Option<bool>,
    /// The submitting harness no longer waits on the HTTP response, so the
    /// terminal receipt must wake the owning mission. Recovered remote builds
    /// are promoted to this mode because a restart severed any synchronous
    /// waiter that may have existed.
    #[serde(default)]
    pub wake_on_terminal: bool,
}

impl JobHandle {
    /// Request-level continuation intent, also available while a submission
    /// is still tentative and its node acceptance is ambiguous.
    pub fn continuation_requested(&self) -> bool {
        self.wake_on_terminal || self.wait_for_completion != Some(false)
    }

    /// True when this remote build owns a mission continuation. Newly written
    /// fire-and-forget handles carry `Some(false)` and are excluded. Legacy
    /// remote-build handles remain recoverable because their request mode was
    /// not recorded.
    pub fn expects_mission_continuation(&self) -> bool {
        self.kind == JobHandleKind::RemoteBuild && self.continuation_requested()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteJobReceipt {
    pub mission_id: Uuid,
    pub node_id: String,
    pub job_id: Uuid,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub submission_sequence: u64,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    #[serde(default)]
    pub exit_status: Option<i32>,
    pub identity: RemoteJobIdentity,
    /// Content digests produced by the exact terminal execution. Older
    /// receipts deserialize empty and are not reused for artifact-bearing
    /// validations.
    #[serde(default)]
    pub artifacts: Vec<ArtifactEntry>,
    /// The accepted request owned a mission continuation even if its terminal
    /// wake had not yet been armed. This lets startup recover the narrow
    /// crash window after finalization but before a synchronous result reaches
    /// the harness. Legacy receipts default to false.
    #[serde(default)]
    pub continuation_expected: bool,
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
    /// A newer ambiguous submission may have reached a node but has not yet
    /// produced an accepted validation handle. The terminal callback must
    /// wait until that job id is reconciled.
    Deferred,
    /// A later validation has taken ownership of the mission's continuation.
    SupersededBy(Uuid),
}

pub(crate) fn validation_order(
    sequence: u64,
    started_at: chrono::DateTime<chrono::Utc>,
) -> (u64, i64) {
    (sequence, started_at.timestamp_micros())
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
                && handle.expects_mission_continuation()
        })
        .max_by_key(|handle| validation_order(handle.submission_sequence, handle.started_at));
    let Some(current_handle) = current_handle else {
        return Ok(None);
    };

    let current_order = validation_order(
        current_handle.submission_sequence,
        current_handle.started_at,
    );
    let newer_terminal_exists = receipts.iter().any(|receipt| {
        if receipt.mission_id != mission_id
            || (!receipt.wake_required && !receipt.continuation_expected)
        {
            return false;
        }
        let receipt_order = validation_order(receipt.submission_sequence, receipt.started_at);
        receipt_order > current_order
            || (receipt_order == current_order
                && (receipt.submission_sequence == 0 || current_handle.submission_sequence == 0))
    });
    let newer_ambiguous_submission_exists = handles.iter().any(|handle| {
        handle.mission_id == mission_id
            && handle.kind == JobHandleKind::Tentative
            && handle.continuation_requested()
            && validation_order(handle.submission_sequence, handle.started_at) > current_order
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
            receipt.identity.reusable_for(identity)
                && (identity.artifacts.is_empty() || !receipt.artifacts.is_empty())
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
            handle
                .identity
                .as_ref()
                .is_some_and(|existing| existing.excludes(identity))
                && matches!(
                    handle.kind,
                    JobHandleKind::RemoteBuild | JobHandleKind::Tentative
                )
        })
        .min_by_key(|handle| handle.started_at)
        .map(EquivalentRemoteValidation::Active))
}

/// Unresolved (accepted or ambiguously submitted) job handle with the same
/// immutable validation identity, ignoring terminal receipts. In the combined
/// lookup above a successful receipt takes precedence over an active handle,
/// so forced runs that bypass receipt replay must use this to keep failing
/// closed on a concurrent equivalent submission.
pub async fn active_equivalent_remote_validation(
    working_dir: &Path,
    identity: &RemoteJobIdentity,
) -> anyhow::Result<Option<JobHandle>> {
    let _guard = lock().lock().await;
    Ok(load_result(working_dir)
        .await?
        .into_iter()
        .filter(|handle| {
            handle
                .identity
                .as_ref()
                .is_some_and(|existing| existing.excludes(identity))
                && matches!(
                    handle.kind,
                    JobHandleKind::RemoteBuild | JobHandleKind::Tentative
                )
        })
        .min_by_key(|handle| handle.started_at))
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

/// Terminal receipts that can recover an inherited `waiting_remote_job`
/// lease. Unlike `pending_terminal_wakes`, this includes a synchronous result
/// finalized immediately before a core crash, while its normal HTTP/tool
/// delivery was still in flight.
pub async fn recoverable_terminal_continuations(
    working_dir: &Path,
    mission_id: Uuid,
) -> anyhow::Result<Vec<RemoteJobReceipt>> {
    Ok(load_receipts_result(working_dir)
        .await?
        .into_iter()
        .filter(|receipt| {
            receipt.mission_id == mission_id
                && receipt.wake_delivered_at.is_none()
                && (receipt.wake_required || receipt.continuation_expected)
        })
        .collect())
}

/// Promote a terminal continuation into the durable wake outbox after startup
/// proved that its mission still had an inherited remote-wait lease.
pub async fn require_terminal_receipt_wake(
    working_dir: &Path,
    job_id: Uuid,
) -> anyhow::Result<bool> {
    let _guard = lock().lock().await;
    let mut receipts = load_receipts_result(working_dir).await?;
    let Some(receipt) = receipts.iter_mut().find(|receipt| receipt.job_id == job_id) else {
        return Ok(false);
    };
    if receipt.wake_delivered_at.is_some()
        || (!receipt.wake_required && !receipt.continuation_expected)
    {
        return Ok(false);
    }
    if !receipt.wake_required {
        receipt.wake_required = true;
        store_receipts(working_dir, &receipts).await?;
    }
    Ok(true)
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
    let receipt_order = validation_order(receipt.submission_sequence, receipt.started_at);

    if let Some(newer) = handles
        .iter()
        .filter(|handle| {
            handle.kind == JobHandleKind::RemoteBuild
                && handle.expects_mission_continuation()
                && handle.mission_id == receipt.mission_id
                && handle.job_id != receipt.job_id
                && validation_order(handle.submission_sequence, handle.started_at) > receipt_order
        })
        .max_by_key(|handle| validation_order(handle.submission_sequence, handle.started_at))
    {
        return Ok(TerminalWakeDisposition::SupersededBy(newer.job_id));
    }
    if let Some(newer) = receipts
        .iter()
        .filter(|candidate| {
            candidate.mission_id == receipt.mission_id
                && (candidate.wake_required || candidate.continuation_expected)
                && candidate.job_id != receipt.job_id
                && validation_order(candidate.submission_sequence, candidate.started_at)
                    > receipt_order
        })
        .max_by_key(|candidate| {
            validation_order(candidate.submission_sequence, candidate.started_at)
        })
    {
        return Ok(TerminalWakeDisposition::SupersededBy(newer.job_id));
    }
    if handles.iter().any(|handle| {
        handle.kind == JobHandleKind::Tentative
            && handle.continuation_requested()
            && handle.mission_id == receipt.mission_id
            && handle.job_id != receipt.job_id
            && validation_order(handle.submission_sequence, handle.started_at) >= receipt_order
    }) {
        return Ok(TerminalWakeDisposition::Deferred);
    }
    if handles.iter().any(|handle| {
        handle.kind == JobHandleKind::RemoteBuild
            && handle.expects_mission_continuation()
            && handle.mission_id == receipt.mission_id
            && handle.job_id != receipt.job_id
            && validation_order(handle.submission_sequence, handle.started_at) == receipt_order
            && (handle.submission_sequence == 0 || receipt.submission_sequence == 0)
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
        sql::mirror_wake(working_dir, job_id, "suppressed", Some(superseding_job_id));
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
        sql::mirror_wake(working_dir, job_id, "delivered", None);
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
    finalize_with_artifacts(working_dir, job_id, state, exit_status, Vec::new()).await
}

/// Finalize a remote build while retaining its resolved artifact evidence.
pub async fn finalize_with_artifacts(
    working_dir: &Path,
    job_id: Uuid,
    state: &str,
    exit_status: Option<i32>,
    artifacts: Vec<ArtifactEntry>,
) -> anyhow::Result<bool> {
    const MAX_RECEIPTS: usize = 2_000;

    let _guard = lock().lock().await;
    let mut handles = load_result(working_dir).await?;
    let Some(index) = handles.iter().position(|handle| handle.job_id == job_id) else {
        return Ok(false);
    };
    let handle = handles.remove(index);
    if handle.kind != JobHandleKind::RemoteBuild {
        sql::mirror_terminal_without_receipt(working_dir, job_id, state);
    }
    if handle.kind == JobHandleKind::RemoteBuild {
        let continuation_expected = handle.expects_mission_continuation();
        let Some(identity) = handle.identity else {
            sql::mirror_terminal_without_receipt(working_dir, job_id, state);
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
            submission_sequence: handle.submission_sequence,
            finished_at: chrono::Utc::now(),
            state: state.to_string(),
            exit_status,
            identity,
            artifacts,
            continuation_expected,
            wake_required: handle.kind == JobHandleKind::RemoteBuild && handle.wake_on_terminal,
            wake_delivered_at: previous_wake_delivered_at,
            wake_suppressed_by: None,
        });
        if let Some(receipt) = receipts.last() {
            sql::mirror_receipt(working_dir, receipt);
        }
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
    let mut handle = handle;
    let previous_sequence = handles
        .iter()
        .find(|existing| existing.job_id == handle.job_id)
        .map(|existing| existing.submission_sequence)
        .unwrap_or(0);
    if handle.submission_sequence == 0 {
        handle.submission_sequence = if previous_sequence > 0 {
            previous_sequence
        } else {
            let receipt_max = load_receipts_result(working_dir)
                .await?
                .into_iter()
                .map(|receipt| receipt.submission_sequence)
                .max()
                .unwrap_or(0);
            handles
                .iter()
                .map(|existing| existing.submission_sequence)
                .max()
                .unwrap_or(0)
                .max(receipt_max)
                .saturating_add(1)
        };
    }
    handles.retain(|existing| existing.job_id != handle.job_id);
    sql::mirror_handle(working_dir, &handle);
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
        sql::mirror_remove(working_dir, job_id);
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
    sql::mirror_handle(working_dir, handle);
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

/// SQL mirror of the JSON ledger (`projects.db`: `remote_jobs`,
/// `remote_job_subscribers`, `receipts` kind=`build`).
///
/// Dual-write window (plan step 6): the JSON files stay authoritative and
/// every mutation is mirrored here best-effort; startup runs
/// [`sql_parity_backfill`] so a mirror that fell behind converges. Reads move
/// to SQL in step 7, once a production restart has shown zero drift.
pub mod sql {
    use std::path::{Path, PathBuf};

    use rusqlite::{params, Connection, OptionalExtension};
    use serde::Serialize;
    use uuid::Uuid;

    use super::{JobHandle, JobHandleKind, RemoteJobReceipt};

    fn db_path(working_dir: &Path) -> PathBuf {
        working_dir.join(".sandboxed-sh").join("projects.db")
    }

    fn open(working_dir: &Path) -> Option<Connection> {
        let path = db_path(working_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let connection = Connection::open(&path)
            .map_err(|error| tracing::warn!(%error, "remote job SQL mirror: open failed"))
            .ok()?;
        let _ = connection.busy_timeout(std::time::Duration::from_secs(5));
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
        // Idempotent: the store normally created these already; a fresh
        // working dir (tests, first boot before the store opened) gets them
        // here so a mirror write can never race the store's initialize.
        for schema in [
            crate::api::projects_store::SCHEMA,
            crate::api::projects_store::REMOTE_JOBS_SCHEMA,
        ] {
            if let Err(error) = connection.execute_batch(schema) {
                tracing::warn!(%error, "remote job SQL mirror: schema failed");
                return None;
            }
        }
        Some(connection)
    }

    fn kind_label(kind: JobHandleKind) -> &'static str {
        match kind {
            JobHandleKind::Mission => "mission",
            JobHandleKind::RemoteBuild => "remote_build",
            JobHandleKind::Tentative => "tentative",
        }
    }

    fn handle_state(handle: &JobHandle) -> &'static str {
        if handle.kind == JobHandleKind::Tentative || handle.accepted_at.is_none() {
            "submitting"
        } else if handle.heartbeat_at.is_some() {
            "running"
        } else {
            "accepted"
        }
    }

    fn upsert_handle(connection: &Connection, handle: &JobHandle) -> rusqlite::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let identity_json = handle
            .identity
            .as_ref()
            .and_then(|identity| serde_json::to_string(identity).ok());
        let identity_hash = handle
            .identity
            .as_ref()
            .map(|identity| identity.identity_hash());
        let identity_version = handle
            .identity
            .as_ref()
            .map(|identity| identity.version)
            .unwrap_or(0);
        connection.execute(
            "INSERT INTO remote_jobs \
               (job_id, mission_id, node_id, kind, state, identity_version, identity_hash, identity_json, \
                submission_sequence, started_at, accepted_at, heartbeat_at, wake_required, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
             ON CONFLICT(job_id) DO UPDATE SET \
               node_id = excluded.node_id, kind = excluded.kind, \
               state = CASE WHEN remote_jobs.state IN ('succeeded','failed','cancelled','lost') \
                            THEN remote_jobs.state ELSE excluded.state END, \
               identity_version = excluded.identity_version, identity_hash = excluded.identity_hash, \
               identity_json = excluded.identity_json, \
               submission_sequence = CASE WHEN excluded.submission_sequence > 0 \
                                          THEN excluded.submission_sequence ELSE remote_jobs.submission_sequence END, \
               accepted_at = COALESCE(excluded.accepted_at, remote_jobs.accepted_at), \
               heartbeat_at = COALESCE(excluded.heartbeat_at, remote_jobs.heartbeat_at), \
               wake_required = MAX(remote_jobs.wake_required, excluded.wake_required), \
               updated_at = excluded.updated_at",
            params![
                handle.job_id.to_string(),
                handle.mission_id.to_string(),
                handle.node_id,
                kind_label(handle.kind),
                handle_state(handle),
                identity_version,
                identity_hash,
                identity_json,
                handle.submission_sequence as i64,
                handle.started_at.to_rfc3339(),
                handle.accepted_at.map(|at| at.to_rfc3339()),
                handle.heartbeat_at.map(|at| at.to_rfc3339()),
                handle.wake_on_terminal as i64,
                now,
            ],
        )?;
        Ok(())
    }

    /// Mirror a live handle. A unique-index violation means a second live
    /// job for the same identity slipped past the JSON check: logged loudly
    /// (the JSON path is still the authority in the dual-write window).
    pub fn mirror_handle(working_dir: &Path, handle: &JobHandle) {
        let Some(connection) = open(working_dir) else {
            return;
        };
        if let Err(error) = upsert_handle(&connection, handle) {
            tracing::warn!(
                job_id = %handle.job_id,
                mission_id = %handle.mission_id,
                %error,
                "remote job SQL mirror: handle upsert failed (duplicate live identity?)"
            );
        }
    }

    fn upsert_receipt(connection: &Connection, receipt: &RemoteJobReceipt) -> rusqlite::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let state = match receipt.state.as_str() {
            "succeeded" | "failed" | "cancelled" | "lost" => receipt.state.as_str(),
            _ => "failed",
        };
        let artifacts = serde_json::to_string(&receipt.artifacts).unwrap_or_else(|_| "[]".into());
        let identity_json = serde_json::to_string(&receipt.identity).ok();
        connection.execute(
            "INSERT INTO remote_jobs \
               (job_id, mission_id, node_id, kind, state, identity_version, identity_hash, identity_json, \
                submission_sequence, started_at, finished_at, exit_status, artifacts_json, wake_required, \
                wake_delivered_at, wake_suppressed_by, updated_at) \
             VALUES (?1, ?2, ?3, 'remote_build', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
             ON CONFLICT(job_id) DO UPDATE SET \
               state = excluded.state, finished_at = excluded.finished_at, exit_status = excluded.exit_status, \
               artifacts_json = excluded.artifacts_json, identity_version = excluded.identity_version, \
               identity_hash = excluded.identity_hash, identity_json = excluded.identity_json, \
               wake_required = excluded.wake_required, wake_delivered_at = excluded.wake_delivered_at, \
               wake_suppressed_by = excluded.wake_suppressed_by, updated_at = excluded.updated_at",
            params![
                receipt.job_id.to_string(),
                receipt.mission_id.to_string(),
                receipt.node_id,
                state,
                receipt.identity.version,
                receipt.identity.identity_hash(),
                identity_json,
                receipt.submission_sequence as i64,
                receipt.started_at.to_rfc3339(),
                receipt.finished_at.to_rfc3339(),
                receipt.exit_status,
                artifacts,
                receipt.wake_required as i64,
                receipt.wake_delivered_at.map(|at| at.to_rfc3339()),
                receipt.wake_suppressed_by.map(|id| id.to_string()),
                now,
            ],
        )?;
        // Immutable evidence: one build receipt per job (idempotent).
        let outcome = match state {
            "succeeded" => "succeeded",
            "cancelled" => "cancelled",
            _ => "failed",
        };
        let payload = serde_json::json!({
            "identity": receipt.identity,
            "identity_hash": receipt.identity.identity_hash(),
            "node_id": receipt.node_id,
            "mission_id": receipt.mission_id,
            "exit_status": receipt.exit_status,
            "artifacts": receipt.artifacts,
            "started_at": receipt.started_at,
            "finished_at": receipt.finished_at,
        });
        let request_hash = {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(payload.to_string().as_bytes());
            hex::encode(hasher.finalize())
        };
        connection.execute(
            "INSERT OR IGNORE INTO receipts \
               (id, idempotency_key, request_hash, kind, project_slug, track_id, criterion_id, subject_type, \
                subject_id, outcome, actor_type, actor_id, verifier, supersedes_receipt_id, observed_at, \
                payload, created_at) \
             VALUES (?1, ?2, ?3, 'build', NULL, NULL, NULL, 'build', ?4, ?5, 'system', ?6, ?7, NULL, ?8, ?9, ?10)",
            params![
                Uuid::new_v4().to_string(),
                format!("build:{}", receipt.job_id),
                request_hash,
                receipt.job_id.to_string(),
                outcome,
                format!("node:{}", receipt.node_id),
                receipt.identity.toolchain,
                receipt.finished_at.to_rfc3339(),
                payload.to_string(),
                now,
            ],
        )?;
        Ok(())
    }

    pub fn mirror_receipt(working_dir: &Path, receipt: &RemoteJobReceipt) {
        let Some(connection) = open(working_dir) else {
            return;
        };
        if let Err(error) = upsert_receipt(&connection, receipt) {
            tracing::warn!(job_id = %receipt.job_id, %error, "remote job SQL mirror: receipt upsert failed");
        }
    }

    /// A mission / tentative handle finalized without a receipt.
    pub fn mirror_terminal_without_receipt(working_dir: &Path, job_id: Uuid, state: &str) {
        let Some(connection) = open(working_dir) else {
            return;
        };
        let state = match state {
            "succeeded" | "failed" | "cancelled" | "lost" => state,
            _ => "failed",
        };
        if let Err(error) = connection.execute(
            "UPDATE remote_jobs SET state = ?2, finished_at = ?3, updated_at = ?3 WHERE job_id = ?1",
            params![job_id.to_string(), state, chrono::Utc::now().to_rfc3339()],
        ) {
            tracing::warn!(%job_id, %error, "remote job SQL mirror: terminal update failed");
        }
    }

    pub fn mirror_remove(working_dir: &Path, job_id: Uuid) {
        let Some(connection) = open(working_dir) else {
            return;
        };
        // Only a still-live row is removed; terminal rows are history.
        if let Err(error) = connection.execute(
            "DELETE FROM remote_jobs WHERE job_id = ?1 AND state IN ('submitting','accepted','running')",
            params![job_id.to_string()],
        ) {
            tracing::warn!(%job_id, %error, "remote job SQL mirror: remove failed");
        }
    }

    pub fn mirror_wake(
        working_dir: &Path,
        job_id: Uuid,
        disposition: &str,
        suppressed_by: Option<Uuid>,
    ) {
        let Some(connection) = open(working_dir) else {
            return;
        };
        let now = chrono::Utc::now().to_rfc3339();
        let result = match disposition {
            "delivered" => connection.execute(
                "UPDATE remote_jobs SET wake_delivered_at = ?2, updated_at = ?2 WHERE job_id = ?1",
                params![job_id.to_string(), now],
            ),
            _ => connection.execute(
                "UPDATE remote_jobs SET wake_suppressed_by = ?2, updated_at = ?3 WHERE job_id = ?1",
                params![
                    job_id.to_string(),
                    suppressed_by.map(|id| id.to_string()),
                    now
                ],
            ),
        };
        if let Err(error) = result {
            tracing::warn!(%job_id, %error, "remote job SQL mirror: wake update failed");
        }
    }

    #[derive(Debug, Default, Clone, Serialize)]
    pub struct ParityReport {
        pub json_handles: usize,
        pub json_receipts: usize,
        pub sql_live: usize,
        pub sql_terminal: usize,
        pub backfilled_handles: usize,
        pub backfilled_receipts: usize,
        pub sql_live_not_in_json: usize,
    }

    /// Compare the JSON ledger with the mirror and backfill what the mirror
    /// lacks. Live SQL rows with no JSON handle are reported, not deleted:
    /// in the dual-write window the JSON file is the authority and a
    /// disagreement is the signal the removal gate waits on.
    pub fn parity_backfill(
        working_dir: &Path,
        handles: &[JobHandle],
        receipts: &[RemoteJobReceipt],
    ) -> ParityReport {
        let mut report = ParityReport {
            json_handles: handles.len(),
            json_receipts: receipts.len(),
            ..ParityReport::default()
        };
        let Some(connection) = open(working_dir) else {
            return report;
        };
        let exists = |job_id: &Uuid| -> bool {
            connection
                .query_row(
                    "SELECT 1 FROM remote_jobs WHERE job_id = ?1",
                    params![job_id.to_string()],
                    |_| Ok(()),
                )
                .optional()
                .ok()
                .flatten()
                .is_some()
        };
        for handle in handles {
            if !exists(&handle.job_id) && upsert_handle(&connection, handle).is_ok() {
                report.backfilled_handles += 1;
            }
        }
        for receipt in receipts {
            let terminal: Option<String> = connection
                .query_row(
                    "SELECT state FROM remote_jobs WHERE job_id = ?1",
                    params![receipt.job_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            let needs = !matches!(
                terminal.as_deref(),
                Some("succeeded" | "failed" | "cancelled" | "lost")
            );
            if needs && upsert_receipt(&connection, receipt).is_ok() {
                report.backfilled_receipts += 1;
            }
        }
        report.sql_live = connection
            .query_row(
                "SELECT count(*) FROM remote_jobs WHERE state IN ('submitting','accepted','running')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        report.sql_terminal = connection
            .query_row(
                "SELECT count(*) FROM remote_jobs WHERE state IN ('succeeded','failed','cancelled','lost')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as usize;
        let live_ids: Vec<String> = connection
            .prepare(
                "SELECT job_id FROM remote_jobs WHERE state IN ('submitting','accepted','running')",
            )
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .map(|rows| rows.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        report.sql_live_not_in_json = live_ids
            .iter()
            .filter(|id| {
                !handles
                    .iter()
                    .any(|handle| handle.job_id.to_string() == **id)
            })
            .count();
        report
    }
}

/// Boot-time parity pass: mirror anything the JSON ledger has that SQL lacks
/// and report drift. Never deletes.
pub async fn sql_parity_backfill(working_dir: &Path) -> anyhow::Result<sql::ParityReport> {
    let _guard = lock().lock().await;
    let handles = load_result(working_dir).await?;
    let receipts = load_receipts_result(working_dir).await?;
    Ok(sql::parity_backfill(working_dir, &handles, &receipts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_build_continuation_mode_is_explicit_and_legacy_safe() {
        let base = JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id: Uuid::new_v4(),
            started_at: chrono::Utc::now(),
            submission_sequence: 1,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: Some(chrono::Utc::now()),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: None,
            wait_for_completion: Some(false),
            wake_on_terminal: false,
        };

        assert!(!base.expects_mission_continuation());
        assert!(JobHandle {
            wait_for_completion: Some(true),
            ..base.clone()
        }
        .expects_mission_continuation());
        assert!(JobHandle {
            wait_for_completion: None,
            ..base.clone()
        }
        .expects_mission_continuation());
        assert!(JobHandle {
            wake_on_terminal: true,
            ..base
        }
        .expects_mission_continuation());
    }

    #[tokio::test]
    async fn record_is_durable_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let handle = JobHandle {
            mission_id: Uuid::new_v4(),
            node_id: "node-a".to_string(),
            job_id,
            started_at: chrono::Utc::now(),
            submission_sequence: 0,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: Some(chrono::Utc::now()),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Mission,
            identity: None,
            wait_for_completion: None,
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
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Mission,
                identity: None,
                wait_for_completion: None,
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
        assert_eq!(handle.submission_sequence, 0);
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
            submission_sequence: 0,
            accepted_at: None,
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::Tentative,
            identity: None,
            wait_for_completion: None,
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
        assert_eq!(handles[0].submission_sequence, 1);
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
            submission_sequence: 0,
            accepted_at: Some(chrono::Utc::now()),
            heartbeat_at: None,
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: Some(RemoteJobIdentity {
                version: 0,
                base_tree_sha: None,
                builder_image_digest: None,
                build_protocol_version: None,
                behavior_env_digest: None,
                repository: "https://example.invalid/repo.git".to_string(),
                commit: "a".repeat(40),
                cwd_rel_known: true,
                cwd_rel: None,
                command: vec!["lake".to_string(), "build".to_string()],
                artifacts: Vec::new(),
                toolchain: None,
                source_bundle_digest: None,
            }),
            wait_for_completion: None,
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
            version: 0,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
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
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: None,
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
            version: IDENTITY_VERSION,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
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
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: None,
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
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(artifact_identity.clone()),
                wait_for_completion: None,
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();
        finalize_with_artifacts(
            dir.path(),
            artifact_job_id,
            "succeeded",
            Some(0),
            vec![ArtifactEntry {
                path: "build/report.json".to_string(),
                sha256: "c".repeat(64),
                size_bytes: 42,
            }],
        )
        .await
        .unwrap();
        assert!(matches!(
            equivalent_remote_validation(dir.path(), &artifact_identity)
                .await
                .unwrap(),
            Some(EquivalentRemoteValidation::Succeeded(receipt))
                if receipt.job_id == artifact_job_id && receipt.artifacts.len() == 1
        ));

        let changed_overlay = RemoteJobIdentity {
            source_bundle_digest: Some("b".repeat(64)),
            ..identity.clone()
        };
        assert!(equivalent_remote_validation(dir.path(), &changed_overlay)
            .await
            .unwrap()
            .is_none());

        // A forced run bypasses the succeeded receipt, so it must be able to
        // see a concurrent unresolved handle that the combined lookup hides
        // behind receipt precedence.
        let forced_job_id = Uuid::new_v4();
        record(
            dir.path(),
            JobHandle {
                mission_id: Uuid::new_v4(),
                node_id: "node-c".to_string(),
                job_id: forced_job_id,
                started_at: chrono::Utc::now(),
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: Some(chrono::Utc::now()),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: None,
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            equivalent_remote_validation(dir.path(), &identity)
                .await
                .unwrap(),
            Some(EquivalentRemoteValidation::Succeeded(receipt)) if receipt.job_id == job_id
        ));
        assert!(matches!(
            active_equivalent_remote_validation(dir.path(), &identity)
                .await
                .unwrap(),
            Some(handle) if handle.job_id == forced_job_id
        ));
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
            version: 0,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
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
                submission_sequence: 0,
                accepted_at: Some(old_started_at),
                heartbeat_at: Some(old_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('a')),
                wait_for_completion: None,
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
                submission_sequence: 0,
                accepted_at: Some(new_started_at),
                heartbeat_at: Some(new_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('b')),
                wait_for_completion: None,
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
            submission_sequence: 0,
            accepted_at: Some(started_at),
            heartbeat_at: Some(started_at),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: Some(RemoteJobIdentity {
                version: 0,
                base_tree_sha: None,
                builder_image_digest: None,
                build_protocol_version: None,
                behavior_env_digest: None,
                repository: "https://example.invalid/verity.git".to_string(),
                commit: job_id.to_string(),
                cwd_rel_known: true,
                cwd_rel: None,
                command: vec!["lake".to_string(), "build".to_string()],
                artifacts: Vec::new(),
                toolchain: None,
                source_bundle_digest: None,
            }),
            wait_for_completion: None,
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
    async fn fire_and_forget_validation_never_supersedes_a_continuation_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let continuation_job_id = Uuid::new_v4();
        let detached_job_id = Uuid::new_v4();
        let identity = |commit: char| RemoteJobIdentity {
            version: 0,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
            repository: "https://example.invalid/verity.git".to_string(),
            commit: commit.to_string().repeat(40),
            cwd_rel_known: true,
            cwd_rel: None,
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: Vec::new(),
            toolchain: None,
            source_bundle_digest: None,
        };
        let continuation_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-a".to_string(),
                job_id: continuation_job_id,
                started_at: continuation_started_at,
                submission_sequence: 0,
                accepted_at: Some(continuation_started_at),
                heartbeat_at: Some(continuation_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('a')),
                wait_for_completion: Some(true),
                wake_on_terminal: true,
            },
        )
        .await
        .unwrap();
        let detached_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "node-b".to_string(),
                job_id: detached_job_id,
                started_at: detached_started_at,
                submission_sequence: 0,
                accepted_at: Some(detached_started_at),
                heartbeat_at: Some(detached_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity('b')),
                wait_for_completion: Some(false),
                wake_on_terminal: false,
            },
        )
        .await
        .unwrap();
        finalize(dir.path(), detached_job_id, "succeeded", Some(0))
            .await
            .unwrap();

        assert_eq!(
            current_remote_build_wait_handle(dir.path(), mission_id)
                .await
                .unwrap()
                .map(|handle| handle.job_id),
            Some(continuation_job_id),
            "detached terminal evidence must not hide the live continuation owner"
        );

        finalize(dir.path(), continuation_job_id, "succeeded", Some(0))
            .await
            .unwrap();
        let continuation_receipt = load_receipts_result(dir.path())
            .await
            .unwrap()
            .into_iter()
            .find(|receipt| receipt.job_id == continuation_job_id)
            .unwrap();
        assert_eq!(
            terminal_wake_disposition(dir.path(), &continuation_receipt)
                .await
                .unwrap(),
            TerminalWakeDisposition::Ready,
            "a newer detached receipt must not suppress the owning terminal wake"
        );
    }

    #[tokio::test]
    async fn submission_sequence_breaks_equal_timestamp_ties_without_using_uuid_order() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let old_job_id = Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
        let new_job_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let started_at = chrono::Utc::now();
        let handle = |job_id| JobHandle {
            mission_id,
            node_id: "node-a".to_string(),
            job_id,
            started_at,
            submission_sequence: 0,
            accepted_at: Some(started_at),
            heartbeat_at: Some(started_at),
            disk_reservation_bytes: 0,
            kind: JobHandleKind::RemoteBuild,
            identity: None,
            wait_for_completion: None,
            wake_on_terminal: true,
        };

        record(dir.path(), handle(old_job_id)).await.unwrap();
        record(dir.path(), handle(new_job_id)).await.unwrap();

        let handles = load(dir.path()).await.unwrap();
        let old_sequence = handles
            .iter()
            .find(|handle| handle.job_id == old_job_id)
            .unwrap()
            .submission_sequence;
        let new_sequence = handles
            .iter()
            .find(|handle| handle.job_id == new_job_id)
            .unwrap()
            .submission_sequence;
        assert!(new_sequence > old_sequence);
        assert_eq!(
            current_remote_build_wait_handle(dir.path(), mission_id)
                .await
                .unwrap()
                .map(|handle| handle.job_id),
            Some(new_job_id)
        );
    }

    #[tokio::test]
    async fn newer_terminal_wake_is_not_blocked_by_an_older_active_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let active_job_id = Uuid::new_v4();
        let receipt_job_id = Uuid::new_v4();
        let active_started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let receipt_started_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        let identity = RemoteJobIdentity {
            version: 0,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
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
                    submission_sequence: 0,
                    accepted_at: Some(started_at),
                    heartbeat_at: Some(started_at),
                    disk_reservation_bytes: 0,
                    kind: JobHandleKind::RemoteBuild,
                    identity: Some(identity.clone()),
                    wait_for_completion: None,
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
            TerminalWakeDisposition::Ready
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
            version: 0,
            base_tree_sha: None,
            builder_image_digest: None,
            build_protocol_version: None,
            behavior_env_digest: None,
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
                submission_sequence: 0,
                accepted_at: Some(old_started_at),
                heartbeat_at: Some(old_started_at),
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: None,
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
                submission_sequence: 0,
                accepted_at: None,
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Tentative,
                identity: Some(identity),
                wait_for_completion: None,
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
                submission_sequence: 0,
                accepted_at: None,
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::Tentative,
                identity: Some(RemoteJobIdentity {
                    version: 0,
                    base_tree_sha: None,
                    builder_image_digest: None,
                    build_protocol_version: None,
                    behavior_env_digest: None,
                    repository: "https://example.invalid/verity.git".to_string(),
                    commit: "a".repeat(40),
                    cwd_rel_known: true,
                    cwd_rel: None,
                    command: vec!["lake".to_string(), "build".to_string()],
                    artifacts: Vec::new(),
                    toolchain: None,
                    source_bundle_digest: None,
                }),
                wait_for_completion: None,
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

    fn identity_v1(commit: &str, tree: Option<&str>) -> RemoteJobIdentity {
        RemoteJobIdentity {
            version: IDENTITY_VERSION,
            repository: "github.com/lfglabs-dev/verity".to_string(),
            commit: commit.to_string(),
            base_tree_sha: tree.map(str::to_string),
            cwd_rel_known: true,
            cwd_rel: Some("verity".to_string()),
            command: vec!["lake".to_string(), "build".to_string()],
            artifacts: vec!["b.olean".to_string(), "a.olean".to_string()],
            toolchain: Some("leanprover/lean4:v4.19.0".to_string()),
            source_bundle_digest: None,
            builder_image_digest: None,
            build_protocol_version: Some("1".to_string()),
            behavior_env_digest: None,
        }
    }

    #[test]
    fn identity_hash_is_content_not_commit_and_every_input_matters() {
        let tree = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let a = identity_v1("a".repeat(40).as_str(), Some(tree));
        let b = identity_v1("b".repeat(40).as_str(), Some(tree));
        assert_eq!(
            a.identity_hash(),
            b.identity_hash(),
            "same tree, different commit: same build"
        );
        assert!(a.reusable_for(&b));
        assert!(a.excludes(&b));

        let other_tree = identity_v1("a".repeat(40).as_str(), Some(&"f".repeat(64)));
        assert_ne!(a.identity_hash(), other_tree.identity_hash());

        let mut env = a.clone();
        env.behavior_env_digest = Some("abc".into());
        assert_ne!(a.identity_hash(), env.identity_hash());
        let mut image = a.clone();
        image.builder_image_digest = Some("sha256:1".into());
        assert_ne!(a.identity_hash(), image.identity_hash());
        let mut argv = a.clone();
        argv.command = vec!["lake".into(), "build".into(), "-K".into()];
        assert_ne!(a.identity_hash(), argv.identity_hash());
        let mut sorted = a.clone();
        sorted.artifacts = vec!["a.olean".into(), "b.olean".into()];
        assert_eq!(
            a.identity_hash(),
            sorted.identity_hash(),
            "artifact order is not identity"
        );

        // No tree: the commit is the content.
        let c1 = identity_v1("c".repeat(40).as_str(), None);
        let c2 = identity_v1("d".repeat(40).as_str(), None);
        assert_ne!(c1.identity_hash(), c2.identity_hash());
    }

    #[test]
    fn legacy_v0_receipts_never_replay_but_still_exclude() {
        let mut legacy = identity_v1("a".repeat(40).as_str(), None);
        legacy.version = 0;
        legacy.base_tree_sha = None;
        legacy.build_protocol_version = None;
        let current = identity_v1("a".repeat(40).as_str(), Some(&"e".repeat(64)));
        assert!(
            !legacy.reusable_for(&current),
            "a pre-upgrade success is not evidence for v1"
        );
        assert!(
            !legacy.reusable_for(&legacy),
            "v0 never replays, even against itself"
        );
        let mut current_same_fields = current.clone();
        current_same_fields.build_protocol_version = None;
        assert!(
            legacy.excludes(&current_same_fields),
            "an in-flight v0 job still blocks a v1 duplicate"
        );
        let mut other_cwd = current_same_fields.clone();
        other_cwd.cwd_rel = Some("elsewhere".into());
        assert!(!legacy.excludes(&other_cwd));
    }

    #[tokio::test]
    async fn sql_mirror_tracks_handles_receipts_and_backfills() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = Uuid::new_v4();
        let mission_id = Uuid::new_v4();
        let identity = identity_v1("a".repeat(40).as_str(), Some(&"e".repeat(64)));
        record(
            dir.path(),
            JobHandle {
                mission_id,
                node_id: "spark".to_string(),
                job_id,
                started_at: chrono::Utc::now(),
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: Some(true),
                wake_on_terminal: true,
            },
        )
        .await
        .unwrap();
        let db = dir.path().join(".sandboxed-sh/projects.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        let (state, hash): (String, String) = connection
            .query_row(
                "SELECT state, identity_hash FROM remote_jobs WHERE job_id = ?1",
                rusqlite::params![job_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "accepted");
        assert_eq!(hash, identity.identity_hash());

        // A second live job with the same identity trips the unique index in
        // SQL (logged, not fatal in the dual-write window).
        let duplicate = Uuid::new_v4();
        sql::mirror_handle(
            dir.path(),
            &JobHandle {
                mission_id: Uuid::new_v4(),
                node_id: "spark".to_string(),
                job_id: duplicate,
                started_at: chrono::Utc::now(),
                submission_sequence: 0,
                accepted_at: Some(chrono::Utc::now()),
                heartbeat_at: None,
                disk_reservation_bytes: 0,
                kind: JobHandleKind::RemoteBuild,
                identity: Some(identity.clone()),
                wait_for_completion: None,
                wake_on_terminal: false,
            },
        );
        let live: i64 = connection
            .query_row(
                "SELECT count(*) FROM remote_jobs WHERE state IN ('submitting','accepted','running')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live, 1, "the unique index keeps one live job per identity");

        finalize(dir.path(), job_id, "succeeded", Some(0))
            .await
            .unwrap();
        let (state, outcome): (String, String) = connection
            .query_row(
                "SELECT j.state, r.outcome FROM remote_jobs j \
                 JOIN receipts r ON r.subject_type = 'build' AND r.subject_id = j.job_id \
                 WHERE j.job_id = ?1",
                rusqlite::params![job_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (state.as_str(), outcome.as_str()),
            ("succeeded", "succeeded")
        );

        // Wipe the mirror; the boot parity pass rebuilds it from JSON.
        connection.execute("DELETE FROM remote_jobs", []).unwrap();
        let report = sql_parity_backfill(dir.path()).await.unwrap();
        assert_eq!(report.json_receipts, 1);
        assert_eq!(report.backfilled_receipts, 1);
        assert_eq!(report.sql_terminal, 1);
        assert_eq!(report.sql_live_not_in_json, 0);
        let again = sql_parity_backfill(dir.path()).await.unwrap();
        assert_eq!(again.backfilled_receipts, 0, "parity is idempotent");
    }
}
