//! Durable, privacy-safe ledger for long ChatGPT UI (GPT Pro) turns.
//!
//! A multi-hour Pro reasoning run keeps generating server-side at OpenAI even
//! if the local browser, driver, or sandboxed.sh process dies. This ledger
//! remembers *that* a prompt was submitted — never *what* was said — so a
//! restarted or reconnected turn can reattach to the existing conversation
//! instead of resubmitting the prompt (idempotent submission).
//!
//! Privacy invariants, enforced here and checked in tests:
//! - Records store only pointers and digests: a SHA-256 prompt fingerprint,
//!   an opaque conversation route (`/c/<id>`), a profile basename, states and
//!   timestamps. Never prompt text, response text, or any reasoning content.
//! - Hidden chain-of-thought is never fetched by the driver, so it can never
//!   reach this ledger or the telemetry snapshot.
//! - The telemetry snapshot exposes even less: no conversation route and no
//!   fingerprint, only states, counters, and the profile basename.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const JOB_SCHEMA_VERSION: u32 = 1;

/// Serialize record replacements within the backend process. A mission retry,
/// dashboard reconciliation, and shutdown handling can otherwise all perform
/// read/modify/write cycles against the same record and lose the newest state.
static JOB_LEDGER_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// A submitted prompt stays resumable for this long; afterwards reconciliation
/// abandons it. Generous on purpose: Pro runs are budgeted in hours.
pub const RESUME_MAX_AGE: Duration = Duration::from_secs(48 * 60 * 60);

/// Terminal records are kept briefly for dashboard forensics, then purged.
pub const TERMINAL_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Prompt is live in a ChatGPT conversation; completion not yet observed.
    Submitted,
    /// The turn observed a completed response for this prompt.
    Completed,
    /// The record can no longer be resumed (superseded, stale, or invalid).
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub schema_version: u32,
    pub job_id: Uuid,
    pub mission_id: Uuid,
    /// SHA-256 over model + prompt. The text itself is never stored.
    pub prompt_sha256: String,
    pub model: Option<String>,
    /// Basename of the browser profile that owns the conversation. Resume
    /// must reuse this profile: conversations are account-scoped.
    pub profile: String,
    /// Opaque ChatGPT conversation route such as `/c/<id>` — a pointer, not
    /// content. Never exposed through telemetry or logs.
    pub conversation_path: String,
    pub state: JobState,
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Allowlisted driver/runner failure code from the latest attempt.
    pub last_error_code: Option<String>,
}

/// Privacy-reduced view for the dashboard endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct JobStatusSummary {
    pub mission_id: Uuid,
    pub state: JobState,
    pub attempts: u32,
    pub profile: String,
    pub model: Option<String>,
    pub age_secs: u64,
    pub updated_secs_ago: u64,
    pub resumable: bool,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub abandoned: usize,
    pub purged: usize,
}

pub fn jobs_dir(app_working_dir: &Path) -> PathBuf {
    app_working_dir
        .join(".sandboxed-sh")
        .join("chatgpt-ui-jobs")
}

fn job_path(app_working_dir: &Path, mission_id: Uuid) -> PathBuf {
    jobs_dir(app_working_dir).join(format!("{mission_id}.json"))
}

/// Deterministic digest of everything that makes a prompt submission unique.
/// A retried turn with the same digest is the same logical submission and
/// must reattach instead of resubmitting.
pub fn prompt_fingerprint(message: &str, model: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(message.as_bytes());
    hex::encode(hasher.finalize())
}

/// Conversation routes are strictly shaped opaque ids. Anything else is
/// rejected before it can be persisted or navigated to.
pub fn valid_conversation_path(path: &str) -> bool {
    let Some(id) = path.strip_prefix("/c/") else {
        return false;
    };
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// Keep persisted failure codes on a fixed vocabulary so a record can never
/// smuggle free-form driver output.
pub fn allowlisted_error_code(code: &str) -> &'static str {
    match code {
        "auth_required" => "auth_required",
        "rate_limited" => "rate_limited",
        "browser_launch" => "browser_launch",
        "compatibility" => "compatibility",
        "timeout" => "timeout",
        "dependency_missing" => "dependency_missing",
        "invalid_request" => "invalid_request",
        "invalid_config" => "invalid_config",
        "resume_not_found" => "resume_not_found",
        "resume_mismatch" => "resume_mismatch",
        "continuation_not_found" => "continuation_not_found",
        "driver_protocol" => "driver_protocol",
        "driver_exit" => "driver_exit",
        "stream_error" => "stream_error",
        "profile_unavailable" => "profile_unavailable",
        "superseded" => "superseded",
        "stale" => "stale",
        "cancelled" => "cancelled",
        "server_shutdown" => "server_shutdown",
        _ => "other",
    }
}

pub fn load_job(app_working_dir: &Path, mission_id: Uuid) -> Option<JobRecord> {
    let raw = std::fs::read_to_string(job_path(app_working_dir, mission_id)).ok()?;
    let record: JobRecord = serde_json::from_str(&raw).ok()?;
    if record.schema_version != JOB_SCHEMA_VERSION || record.mission_id != mission_id {
        return None;
    }
    Some(record)
}

fn save_job_unlocked(app_working_dir: &Path, record: &JobRecord) -> Result<(), String> {
    let dir = jobs_dir(app_working_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create ChatGPT UI job ledger directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure ChatGPT UI job ledger directory: {error}"))?;
    }
    let path = job_path(app_working_dir, record.mission_id);
    // A unique temporary name avoids collisions if an old process is still
    // unwinding while its replacement reconciles the same mission.
    let tmp = dir.join(format!(".{}.{}.tmp", record.mission_id, Uuid::new_v4()));
    let payload = serde_json::to_vec_pretty(record)
        .map_err(|error| format!("cannot serialize ChatGPT UI job record: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("cannot create ChatGPT UI job record: {error}"))?;
        file.write_all(&payload)
            .map_err(|error| format!("cannot write ChatGPT UI job record: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync ChatGPT UI job record: {error}"))?;
        drop(file);
        std::fs::rename(&tmp, &path)
            .map_err(|error| format!("cannot commit ChatGPT UI job record: {error}"))?;
        // Persist the rename itself across a host crash on platforms where a
        // directory can be opened and synced.
        #[cfg(unix)]
        std::fs::File::open(&dir)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync ChatGPT UI job ledger directory: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

pub fn save_job(app_working_dir: &Path, record: &JobRecord) -> Result<(), String> {
    let _guard = JOB_LEDGER_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    save_job_unlocked(app_working_dir, record)
}

fn update_job<F: FnOnce(&mut JobRecord)>(app_working_dir: &Path, mission_id: Uuid, apply: F) {
    let _guard = JOB_LEDGER_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(mut record) = load_job(app_working_dir, mission_id) {
        apply(&mut record);
        record.updated_at = Utc::now();
        if let Err(error) = save_job_unlocked(app_working_dir, &record) {
            tracing::warn!(mission_id = %mission_id, error = %error, "failed to update ChatGPT UI job record");
        }
    }
}

#[cfg(test)]
fn ledger_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

#[cfg(test)]
fn assert_private_ledger_permissions(app_working_dir: &Path, mission_id: Uuid) {
    #[cfg(unix)]
    {
        assert_eq!(ledger_mode(&jobs_dir(app_working_dir)), 0o700);
        assert_eq!(ledger_mode(&job_path(app_working_dir, mission_id)), 0o600);
    }
    #[cfg(not(unix))]
    {
        let _ = (app_working_dir, mission_id);
    }
}

/// Persist the moment a prompt provably lives in a conversation. This is the
/// only place a durability record is born.
pub fn record_submitted(
    app_working_dir: &Path,
    mission_id: Uuid,
    prompt_sha256: &str,
    model: Option<&str>,
    profile: &str,
    conversation_path: &str,
    prior_attempts: u32,
) -> Result<JobRecord, String> {
    if !valid_conversation_path(conversation_path) {
        return Err("refusing to persist a malformed ChatGPT conversation route".to_string());
    }
    let now = Utc::now();
    let record = JobRecord {
        schema_version: JOB_SCHEMA_VERSION,
        job_id: Uuid::new_v4(),
        mission_id,
        prompt_sha256: prompt_sha256.to_string(),
        model: model.map(str::to_string),
        profile: profile.to_string(),
        conversation_path: conversation_path.to_string(),
        state: JobState::Submitted,
        attempts: prior_attempts.saturating_add(1),
        created_at: now,
        updated_at: now,
        last_error_code: None,
    };
    save_job(app_working_dir, &record)?;
    Ok(record)
}

pub fn mark_completed(app_working_dir: &Path, mission_id: Uuid) {
    update_job(app_working_dir, mission_id, |record| {
        record.state = JobState::Completed;
        record.last_error_code = None;
    });
}

pub fn mark_abandoned(app_working_dir: &Path, mission_id: Uuid, code: &str) {
    update_job(app_working_dir, mission_id, |record| {
        record.state = JobState::Abandoned;
        record.last_error_code = Some(allowlisted_error_code(code).to_string());
    });
}

/// A post-submission failure does not kill durability: the conversation keeps
/// generating server-side. Note the code, bump the attempt counter, and leave
/// the record resumable.
pub fn note_attempt_error(app_working_dir: &Path, mission_id: Uuid, code: &str) {
    update_job(app_working_dir, mission_id, |record| {
        if record.state == JobState::Submitted {
            record.last_error_code = Some(allowlisted_error_code(code).to_string());
        }
    });
}

pub fn touch_resume_attempt(app_working_dir: &Path, mission_id: Uuid) {
    update_job(app_working_dir, mission_id, |record| {
        record.attempts = record.attempts.saturating_add(1);
    });
}

fn age_of(record: &JobRecord, now: DateTime<Utc>) -> Duration {
    (now - record.created_at).to_std().unwrap_or_default()
}

pub fn resumable_job(record: &JobRecord, prompt_sha256: &str, now: DateTime<Utc>) -> bool {
    record.state == JobState::Submitted
        && record.prompt_sha256 == prompt_sha256
        && valid_conversation_path(&record.conversation_path)
        && age_of(record, now) <= RESUME_MAX_AGE
}

pub fn continuable_conversation(record: &JobRecord) -> bool {
    record.state == JobState::Completed && valid_conversation_path(&record.conversation_path)
}

fn read_all_records(app_working_dir: &Path) -> Vec<JobRecord> {
    let Ok(entries) = std::fs::read_dir(jobs_dir(app_working_dir)) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(mission_id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| Uuid::parse_str(value).ok())
        else {
            continue;
        };
        if let Some(record) = load_job(app_working_dir, mission_id) {
            records.push(record);
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    records
}

/// Restart/disconnect reconciliation: expire records that can no longer be
/// resumed and purge old terminal records. Safe to call from any attempt or
/// from the telemetry endpoint — it only ever narrows state.
pub fn reconcile_jobs(app_working_dir: &Path) -> ReconcileSummary {
    let now = Utc::now();
    let mut summary = ReconcileSummary::default();
    for record in read_all_records(app_working_dir) {
        match record.state {
            JobState::Submitted => {
                if age_of(&record, now) > RESUME_MAX_AGE
                    || !valid_conversation_path(&record.conversation_path)
                {
                    mark_abandoned(app_working_dir, record.mission_id, "stale");
                    summary.abandoned += 1;
                }
            }
            JobState::Completed | JobState::Abandoned => {
                let idle = (now - record.updated_at).to_std().unwrap_or_default();
                if idle > TERMINAL_RETENTION {
                    let _ = std::fs::remove_file(job_path(app_working_dir, record.mission_id));
                    summary.purged += 1;
                }
            }
        }
    }
    summary
}

/// Dashboard snapshot. Deliberately omits the conversation route and prompt
/// fingerprint: operators see health, never pointers into an account.
pub fn jobs_snapshot(app_working_dir: &Path) -> Vec<JobStatusSummary> {
    let now = Utc::now();
    read_all_records(app_working_dir)
        .into_iter()
        .map(|record| JobStatusSummary {
            mission_id: record.mission_id,
            state: record.state,
            attempts: record.attempts,
            profile: record.profile.clone(),
            model: record.model.clone(),
            age_secs: age_of(&record, now).as_secs(),
            updated_secs_ago: (now - record.updated_at)
                .to_std()
                .unwrap_or_default()
                .as_secs(),
            resumable: resumable_job(&record, &record.prompt_sha256, now),
            last_error_code: record.last_error_code.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit(dir: &Path, mission_id: Uuid, fingerprint: &str) -> JobRecord {
        record_submitted(
            dir,
            mission_id,
            fingerprint,
            Some("gpt-5.6-pro"),
            "profile-1",
            "/c/0123456789abcdef",
            0,
        )
        .unwrap()
    }

    #[test]
    fn fingerprint_binds_model_and_message_without_storing_text() {
        let a = prompt_fingerprint("build the report", Some("gpt-5.6-pro"));
        let b = prompt_fingerprint("build the report", Some("gpt-5.6-pro"));
        let c = prompt_fingerprint("build the report", None);
        let d = prompt_fingerprint("build the reports", Some("gpt-5.6-pro"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(a.len(), 64);
        assert!(!a.contains("report"));
    }

    #[test]
    fn conversation_routes_are_strictly_validated() {
        assert!(valid_conversation_path("/c/0123456789abcdef"));
        assert!(valid_conversation_path(
            "/c/019969c9-2c6a-7325-a2b1-9e01b4d3ce17"
        ));
        assert!(!valid_conversation_path("/c/short"));
        assert!(!valid_conversation_path("/settings"));
        assert!(!valid_conversation_path("/c/../../etc/passwd"));
        assert!(!valid_conversation_path("/c/id with spaces"));
        assert!(!valid_conversation_path(
            "https://chatgpt.com/c/0123456789abcdef"
        ));
    }

    #[test]
    fn submission_persists_and_reloads_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let fingerprint = prompt_fingerprint("prompt", Some("Pro"));
        let record = submit(root.path(), mission_id, &fingerprint);
        assert_eq!(record.attempts, 1);
        assert_private_ledger_permissions(root.path(), mission_id);

        let loaded = load_job(root.path(), mission_id).unwrap();
        assert_eq!(loaded.state, JobState::Submitted);
        assert_eq!(loaded.prompt_sha256, fingerprint);
        assert!(resumable_job(&loaded, &fingerprint, Utc::now()));
        assert!(!resumable_job(
            &loaded,
            &prompt_fingerprint("different prompt", Some("Pro")),
            Utc::now()
        ));
    }

    #[test]
    fn malformed_conversation_routes_are_never_persisted() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let error = record_submitted(
            root.path(),
            mission_id,
            "fingerprint",
            None,
            "profile-1",
            "/c/../../../steal",
            0,
        )
        .unwrap_err();
        assert!(error.contains("malformed"));
        assert!(load_job(root.path(), mission_id).is_none());
    }

    #[test]
    fn records_store_no_prompt_or_response_content() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let message = "extremely private mission prompt";
        let fingerprint = prompt_fingerprint(message, Some("gpt-5.6-pro"));
        submit(root.path(), mission_id, &fingerprint);

        let raw = std::fs::read_to_string(job_path(root.path(), mission_id)).unwrap();
        assert!(!raw.contains("private"));
        assert!(!raw.contains("prompt text"));
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let keys: Vec<&str> = parsed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in keys {
            assert!(
                [
                    "schema_version",
                    "job_id",
                    "mission_id",
                    "prompt_sha256",
                    "model",
                    "profile",
                    "conversation_path",
                    "state",
                    "attempts",
                    "created_at",
                    "updated_at",
                    "last_error_code",
                ]
                .contains(&key),
                "unexpected persisted field: {key}"
            );
        }
    }

    #[test]
    fn post_submission_errors_keep_the_job_resumable() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let fingerprint = prompt_fingerprint("prompt", None);
        submit(root.path(), mission_id, &fingerprint);

        note_attempt_error(root.path(), mission_id, "stream_error");
        let record = load_job(root.path(), mission_id).unwrap();
        assert_eq!(record.state, JobState::Submitted);
        assert_eq!(record.last_error_code.as_deref(), Some("stream_error"));
        assert!(resumable_job(&record, &fingerprint, Utc::now()));

        touch_resume_attempt(root.path(), mission_id);
        assert_eq!(load_job(root.path(), mission_id).unwrap().attempts, 2);

        mark_completed(root.path(), mission_id);
        let record = load_job(root.path(), mission_id).unwrap();
        assert_eq!(record.state, JobState::Completed);
        assert!(!resumable_job(&record, &fingerprint, Utc::now()));
        assert!(continuable_conversation(&record));
    }

    #[test]
    fn concurrent_resume_updates_do_not_lose_attempts() {
        let root = tempfile::tempdir().unwrap();
        let app_working_dir = root.path().to_path_buf();
        let mission_id = Uuid::new_v4();
        submit(&app_working_dir, mission_id, "fingerprint");

        let handles = (0..8)
            .map(|_| {
                let app_working_dir = app_working_dir.clone();
                std::thread::spawn(move || touch_resume_attempt(&app_working_dir, mission_id))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(load_job(&app_working_dir, mission_id).unwrap().attempts, 9);
    }

    #[test]
    fn unknown_error_codes_collapse_to_other() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        submit(root.path(), mission_id, "fingerprint");
        note_attempt_error(root.path(), mission_id, "free form driver text!");
        assert_eq!(
            load_job(root.path(), mission_id)
                .unwrap()
                .last_error_code
                .as_deref(),
            Some("other")
        );
    }

    #[test]
    fn reconciliation_abandons_stale_submissions_and_purges_old_terminals() {
        let root = tempfile::tempdir().unwrap();
        let stale_mission = Uuid::new_v4();
        let fresh_mission = Uuid::new_v4();
        let purged_mission = Uuid::new_v4();

        let mut stale = submit(root.path(), stale_mission, "fingerprint");
        stale.created_at = Utc::now() - chrono::Duration::hours(72);
        save_job(root.path(), &stale).unwrap();

        submit(root.path(), fresh_mission, "fingerprint");

        let mut purged = submit(root.path(), purged_mission, "fingerprint");
        purged.state = JobState::Completed;
        purged.updated_at = Utc::now() - chrono::Duration::days(30);
        save_job(root.path(), &purged).unwrap();

        let summary = reconcile_jobs(root.path());
        assert_eq!(summary.abandoned, 1);
        assert_eq!(summary.purged, 1);
        assert_eq!(
            load_job(root.path(), stale_mission).unwrap().state,
            JobState::Abandoned
        );
        assert_eq!(
            load_job(root.path(), fresh_mission).unwrap().state,
            JobState::Submitted
        );
        assert!(load_job(root.path(), purged_mission).is_none());
    }

    #[test]
    fn snapshot_exposes_health_but_never_pointers() {
        let root = tempfile::tempdir().unwrap();
        let mission_id = Uuid::new_v4();
        let fingerprint = prompt_fingerprint("secret prompt", Some("gpt-5.6-pro"));
        submit(root.path(), mission_id, &fingerprint);

        let snapshot = jobs_snapshot(root.path());
        assert_eq!(snapshot.len(), 1);
        let entry = &snapshot[0];
        assert_eq!(entry.mission_id, mission_id);
        assert_eq!(entry.state, JobState::Submitted);
        assert!(entry.resumable);
        assert_eq!(entry.profile, "profile-1");

        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("/c/"));
        assert!(!serialized.contains(&fingerprint));
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn reconciliation_tolerates_a_missing_ledger_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(reconcile_jobs(root.path()), ReconcileSummary::default());
        assert!(jobs_snapshot(root.path()).is_empty());
    }
}
