//! Persistent backend-wide ChatGPT UI availability gate.
//!
//! Profile health is process-local and slot-specific. Account rate limits and
//! backend-wide failures are different: they must survive a service restart
//! and must not become available merely because a timer elapsed. Expiry moves
//! the gate to `probing`; only a successful, non-submitting browser probe can
//! reopen traffic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, TerminalReason};
use crate::api::control::AgentEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    Available,
    Cooldown,
    Probing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityReason {
    Compatibility,
    Transport,
    RateLimited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityStatus {
    pub state: AvailabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AvailabilityReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_since: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    pub transition_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_at: Option<DateTime<Utc>>,
}

impl Default for AvailabilityStatus {
    fn default() -> Self {
        Self {
            state: AvailabilityState::Available,
            reason: None,
            unavailable_since: None,
            retry_at: None,
            retry_after_secs: None,
            transition_id: 0,
            last_probe_at: None,
            recovered_at: None,
        }
    }
}

#[derive(Debug)]
struct Entry {
    status: AvailabilityStatus,
    state_path: PathBuf,
    current_profile_dirs: Vec<PathBuf>,
    known_profile_dirs: Vec<PathBuf>,
    known_configurations: Vec<Vec<PathBuf>>,
    probe_claimed: bool,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn entry_for_profiles_mut<'a>(
    entries: &'a mut HashMap<PathBuf, Entry>,
    profile_dirs: &[PathBuf],
) -> Option<&'a mut Entry> {
    entries.values_mut().find(|entry| {
        entry.current_profile_dirs == profile_dirs
            || entry
                .known_configurations
                .iter()
                .any(|configuration| configuration == profile_dirs)
    })
}

fn state_path(app_working_dir: &Path) -> PathBuf {
    app_working_dir
        .join(".sandboxed-sh")
        .join("chatgpt_ui_availability.json")
}

fn persist(entry: &Entry) {
    let Some(parent) = entry.state_path.parent() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(error = %error, "Failed to create ChatGPT UI availability directory");
        return;
    }
    let temp_path = entry.state_path.with_extension("json.tmp");
    let bytes = match serde_json::to_vec_pretty(&entry.status) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(error = %error, "Failed to encode ChatGPT UI availability state");
            return;
        }
    };
    if let Err(error) = std::fs::write(&temp_path, bytes)
        .and_then(|_| std::fs::rename(&temp_path, &entry.state_path))
    {
        tracing::warn!(error = %error, "Failed to persist ChatGPT UI availability state");
        let _ = std::fs::remove_file(temp_path);
    }
}

fn reconcile(entry: &mut Entry, now: DateTime<Utc>) {
    if entry.status.state == AvailabilityState::Cooldown
        && entry
            .status
            .retry_at
            .is_some_and(|retry_at| retry_at <= now)
    {
        entry.status.state = AvailabilityState::Probing;
        entry.status.retry_at = None;
        entry.status.retry_after_secs = None;
        entry.status.transition_id = entry.status.transition_id.saturating_add(1);
        entry.probe_claimed = false;
        persist(entry);
    } else if entry.status.state == AvailabilityState::Cooldown {
        entry.status.retry_after_secs = entry
            .status
            .retry_at
            .map(|retry_at| retry_at.signed_duration_since(now).num_seconds().max(0) as u64);
    }
}

pub fn configure(profile_dirs: &[PathBuf], app_working_dir: &Path) {
    if profile_dirs.is_empty() {
        return;
    }
    let path = state_path(app_working_dir);
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    if let Some(entry) = entries.get_mut(&path) {
        // Retain prior members for the lifetime of this process. A settings
        // refresh can remove a profile while an already-running browser turn
        // still owns it; a later failure from that turn must continue to open
        // the account-wide gate. Process restart naturally clears mappings
        // once no old turns can still report.
        entry.current_profile_dirs = profile_dirs.to_vec();
        if !entry
            .known_configurations
            .iter()
            .any(|configuration| configuration == profile_dirs)
        {
            entry.known_configurations.push(profile_dirs.to_vec());
        }
        for profile_dir in profile_dirs {
            if !entry.known_profile_dirs.contains(profile_dir) {
                entry.known_profile_dirs.push(profile_dir.clone());
            }
        }
        return;
    }
    let status = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<AvailabilityStatus>(&bytes).ok())
        // A newly configured pool has no positive recovery evidence yet.
        // Fail closed and let the background worker perform the same bounded
        // probe used after a cooldown.
        .unwrap_or(AvailabilityStatus {
            state: AvailabilityState::Probing,
            transition_id: 1,
            ..AvailabilityStatus::default()
        });
    let mut entry = Entry {
        status,
        state_path: path.clone(),
        current_profile_dirs: profile_dirs.to_vec(),
        known_profile_dirs: profile_dirs.to_vec(),
        known_configurations: vec![profile_dirs.to_vec()],
        // A process restart cannot inherit ownership of a prior probe.
        probe_claimed: false,
    };
    reconcile(&mut entry, Utc::now());
    if entry.status.state == AvailabilityState::Probing {
        entry.probe_claimed = false;
    }
    entries.insert(path, entry);
}

pub fn status(profile_dirs: &[PathBuf]) -> AvailabilityStatus {
    if profile_dirs.is_empty() {
        return AvailabilityStatus::default();
    }
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    let Some(entry) = entry_for_profiles_mut(&mut entries, profile_dirs) else {
        return AvailabilityStatus::default();
    };
    reconcile(entry, Utc::now());
    entry.status.clone()
}

pub fn is_configured(profile_dirs: &[PathBuf]) -> bool {
    entry_for_profiles_mut(
        &mut registry()
            .lock()
            .expect("ChatGPT UI availability registry poisoned"),
        profile_dirs,
    )
    .is_some()
}

pub fn open_cooldown(profile_dir: &Path, reason: AvailabilityReason, cooldown: Duration) {
    let now = Utc::now();
    let retry_at = now
        + chrono::Duration::from_std(cooldown).unwrap_or_else(|_| chrono::Duration::minutes(10));
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    // A configured account pool may intentionally use profile directories
    // from different parents. Resolve the owning pool from the complete
    // configured membership instead of deriving an incompatible key from the
    // failing slot alone.
    let owner = entries
        .iter()
        .find(|(_, entry)| {
            entry
                .current_profile_dirs
                .iter()
                .any(|profile| profile == profile_dir)
        })
        .or_else(|| {
            entries.iter().find(|(_, entry)| {
                entry
                    .known_profile_dirs
                    .iter()
                    .any(|profile| profile == profile_dir)
            })
        })
        .map(|(key, _)| key.clone());
    let Some(entry) = owner.and_then(|key| entries.get_mut(&key)) else {
        return;
    };
    let was_available = entry.status.state == AvailabilityState::Available;
    entry.status.state = AvailabilityState::Cooldown;
    entry.status.reason = Some(reason);
    entry.status.unavailable_since = if was_available {
        Some(now)
    } else {
        entry.status.unavailable_since.or(Some(now))
    };
    entry.status.retry_at = Some(
        entry
            .status
            .retry_at
            .map_or(retry_at, |old| old.max(retry_at)),
    );
    entry.status.retry_after_secs = Some(cooldown.as_secs());
    entry.status.transition_id = entry.status.transition_id.saturating_add(1);
    entry.probe_claimed = false;
    persist(entry);
}

pub fn claim_probe(profile_dirs: &[PathBuf]) -> bool {
    if profile_dirs.is_empty() {
        return false;
    }
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    let Some(entry) = entry_for_profiles_mut(&mut entries, profile_dirs) else {
        return false;
    };
    reconcile(entry, Utc::now());
    if entry.status.state != AvailabilityState::Probing || entry.probe_claimed {
        return false;
    }
    entry.probe_claimed = true;
    entry.status.last_probe_at = Some(Utc::now());
    persist(entry);
    true
}

pub fn release_probe(profile_dirs: &[PathBuf]) {
    if profile_dirs.is_empty() {
        return;
    }
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    if let Some(entry) = entry_for_profiles_mut(&mut entries, profile_dirs) {
        entry.probe_claimed = false;
    }
}

pub fn mark_available(profile_dirs: &[PathBuf]) {
    if profile_dirs.is_empty() {
        return;
    }
    let mut entries = registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned");
    let Some(entry) = entry_for_profiles_mut(&mut entries, profile_dirs) else {
        return;
    };
    let now = Utc::now();
    entry.status.state = AvailabilityState::Available;
    entry.status.reason = None;
    entry.status.unavailable_since = None;
    entry.status.retry_at = None;
    entry.status.retry_after_secs = None;
    entry.status.recovered_at = Some(now);
    entry.status.transition_id = entry.status.transition_id.saturating_add(1);
    entry.probe_claimed = false;
    persist(entry);
}

#[allow(clippy::result_large_err)] // AgentResult carries the terminal mission record.
pub async fn wait_until_available(
    profile_dirs: &[PathBuf],
    mission_id: Uuid,
    events_tx: &broadcast::Sender<AgentEvent>,
    cancel: &CancellationToken,
) -> Result<(), AgentResult> {
    let mut announced = false;
    loop {
        let snapshot = status(profile_dirs);
        if snapshot.state == AvailabilityState::Available {
            return Ok(());
        }
        if !announced {
            let label = match snapshot.state {
                AvailabilityState::Cooldown => format!(
                    "Waiting for ChatGPT UI cooldown (about {}s)…",
                    snapshot.retry_after_secs.unwrap_or(1).max(1)
                ),
                AvailabilityState::Probing => {
                    "Waiting for the ChatGPT UI recovery probe…".to_string()
                }
                AvailabilityState::Available => unreachable!(),
            };
            let _ = events_tx.send(AgentEvent::MissionActivity {
                label,
                tool_name: "chatgpt_ui_availability".to_string(),
                mission_id: Some(mission_id),
            });
            announced = true;
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                let shutdown = crate::api::routes::is_shutdown_initiated();
                return Err(AgentResult::failure(
                    if shutdown {
                        "Server restart — paused while waiting for ChatGPT UI recovery."
                    } else {
                        "Mission cancelled while waiting for ChatGPT UI recovery"
                    },
                    0,
                )
                .with_terminal_reason(if shutdown {
                    TerminalReason::ServerShutdown
                } else {
                    TerminalReason::Cancelled
                }));
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests(profile_dirs: &[PathBuf]) {
    registry()
        .lock()
        .expect("ChatGPT UI availability registry poisoned")
        .retain(|_, entry| entry.current_profile_dirs != profile_dirs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_expiry_requires_probe_success() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profiles/one");
        std::fs::create_dir_all(&profile).unwrap();
        let profiles = vec![profile.clone()];
        configure(&profiles, root.path());
        open_cooldown(&profile, AvailabilityReason::RateLimited, Duration::ZERO);

        let snapshot = status(&profiles);
        assert_eq!(snapshot.state, AvailabilityState::Probing);
        assert!(claim_probe(&profiles));
        assert!(!claim_probe(&profiles));
        mark_available(&profiles);
        assert_eq!(status(&profiles).state, AvailabilityState::Available);
        reset_for_tests(&profiles);
    }

    #[test]
    fn cooldown_state_survives_registry_restart() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profiles/one");
        std::fs::create_dir_all(&profile).unwrap();
        let profiles = vec![profile.clone()];
        configure(&profiles, root.path());
        open_cooldown(
            &profile,
            AvailabilityReason::RateLimited,
            Duration::from_secs(600),
        );
        reset_for_tests(&profiles);
        configure(&profiles, root.path());

        let snapshot = status(&profiles);
        assert_eq!(snapshot.state, AvailabilityState::Cooldown);
        assert_eq!(snapshot.reason, Some(AvailabilityReason::RateLimited));
        reset_for_tests(&profiles);
    }

    #[test]
    fn newly_configured_pool_requires_initial_probe() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profiles/one");
        std::fs::create_dir_all(&profile).unwrap();
        let profiles = vec![profile];

        configure(&profiles, root.path());

        assert_eq!(status(&profiles).state, AvailabilityState::Probing);
        assert!(claim_probe(&profiles));
        reset_for_tests(&profiles);
    }

    #[test]
    fn failure_from_a_different_parent_opens_the_configured_pool() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("account-a/one");
        let second = root.path().join("account-b/two");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let profiles = vec![first, second.clone()];
        configure(&profiles, root.path());
        mark_available(&profiles);

        open_cooldown(
            &second,
            AvailabilityReason::RateLimited,
            Duration::from_secs(600),
        );

        let snapshot = status(&profiles);
        assert_eq!(snapshot.state, AvailabilityState::Cooldown);
        assert_eq!(snapshot.reason, Some(AvailabilityReason::RateLimited));
        reset_for_tests(&profiles);
    }

    #[test]
    fn settings_refresh_retains_profiles_owned_by_in_flight_turns() {
        let root = tempfile::tempdir().unwrap();
        let retained = root.path().join("profiles/retained");
        let removed = root.path().join("profiles/removed");
        std::fs::create_dir_all(&retained).unwrap();
        std::fs::create_dir_all(&removed).unwrap();
        // Remove the original first profile so a parent/first-profile-derived
        // registry identity would change during the refresh.
        let original_profiles = vec![removed.clone(), retained.clone()];
        configure(&original_profiles, root.path());
        mark_available(&original_profiles);

        configure(std::slice::from_ref(&retained), root.path());
        open_cooldown(
            &removed,
            AvailabilityReason::RateLimited,
            Duration::from_secs(600),
        );

        assert_eq!(
            status(std::slice::from_ref(&retained)).state,
            AvailabilityState::Cooldown
        );
        assert_eq!(
            status(&original_profiles).state,
            AvailabilityState::Cooldown
        );
        reset_for_tests(std::slice::from_ref(&retained));
    }
}
