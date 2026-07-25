//! Exclusive browser-profile leases with per-slot health and quarantine.
//!
//! Every ChatGPT UI turn leases exactly one profile directory through an
//! advisory file lock. Slots that keep failing in ways that a retry cannot fix
//! (logged-out profile, profile-local Chromium singleton conflict, repeated UI incompatibility)
//! are quarantined for a cooldown so unhealthy slots are not retried blindly.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, TerminalReason};
use crate::api::control::AgentEvent;

const AUTH_QUARANTINE: Duration = Duration::from_secs(30 * 60);
const LAUNCH_QUARANTINE: Duration = Duration::from_secs(5 * 60);
const COMPATIBILITY_QUARANTINE: Duration = Duration::from_secs(10 * 60);
const COMPATIBILITY_QUARANTINE_THRESHOLD: u32 = 3;

pub struct ProfileLock(File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn try_lock_profile(profile_dir: &Path) -> Result<Option<ProfileLock>, String> {
    let name = profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("profile");
    let lock_path = profile_dir
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(format!(".{name}.sandboxed-chatgpt-ui.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("cannot create ChatGPT UI profile lock: {error}"))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(ProfileLock(file))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(format!("cannot lock ChatGPT UI profile: {error}")),
    }
}

/// Failure kinds that indicate a slot-local problem rather than a one-off
/// model or network hiccup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotFailureKind {
    /// The profile is logged out; retries cannot recover it.
    Auth,
    /// The profile is held by a live, foreign-host, or unrecognized Chromium
    /// singleton. Undifferentiated browser launch failures are host-global and
    /// must not be recorded against a slot.
    Launch,
    /// The driver failed a UI compatibility check on this profile.
    Compatibility,
}

impl SlotFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Launch => "launch",
            Self::Compatibility => "compatibility",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SlotHealth {
    consecutive_failures: u32,
    quarantined_until: Option<Instant>,
    last_failure: Option<SlotFailureKind>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, SlotHealth>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, SlotHealth>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn quarantine_for(kind: SlotFailureKind, consecutive_failures: u32) -> Option<Duration> {
    match kind {
        SlotFailureKind::Auth => Some(AUTH_QUARANTINE),
        SlotFailureKind::Launch => Some(LAUNCH_QUARANTINE),
        SlotFailureKind::Compatibility => (consecutive_failures
            >= COMPATIBILITY_QUARANTINE_THRESHOLD)
            .then_some(COMPATIBILITY_QUARANTINE),
    }
}

fn record_slot_failure_at(profile_dir: &Path, kind: SlotFailureKind, now: Instant) {
    let mut slots = registry().lock().expect("profile pool registry poisoned");
    let health = slots.entry(profile_dir.to_path_buf()).or_default();
    health.consecutive_failures = if health.last_failure == Some(kind) {
        health.consecutive_failures.saturating_add(1)
    } else {
        1
    };
    health.last_failure = Some(kind);
    if let Some(cooldown) = quarantine_for(kind, health.consecutive_failures) {
        let until = now + cooldown;
        health.quarantined_until = Some(
            health
                .quarantined_until
                .map_or(until, |existing| existing.max(until)),
        );
    }
}

pub fn record_slot_failure(profile_dir: &Path, kind: SlotFailureKind) {
    record_slot_failure_at(profile_dir, kind, Instant::now());
    tracing::warn!(
        failure_kind = kind.as_str(),
        "ChatGPT UI profile slot recorded a slot-local failure"
    );
}

pub fn record_slot_success(profile_dir: &Path) {
    let mut slots = registry().lock().expect("profile pool registry poisoned");
    slots.remove(profile_dir);
}

fn slot_health_at(profile_dir: &Path, now: Instant) -> SlotHealth {
    let mut slots = registry().lock().expect("profile pool registry poisoned");
    let Some(health) = slots.get_mut(profile_dir) else {
        return SlotHealth::default();
    };
    if health.quarantined_until.is_some_and(|until| until <= now) {
        health.quarantined_until = None;
    }
    *health
}

fn is_quarantined_at(profile_dir: &Path, now: Instant) -> bool {
    slot_health_at(profile_dir, now)
        .quarantined_until
        .is_some_and(|until| until > now)
}

/// Test-only: clear pool health so tests do not observe each other's state.
#[cfg(test)]
pub(crate) fn reset_registry_for_tests(profile_dirs: &[PathBuf]) {
    let mut slots = registry().lock().expect("profile pool registry poisoned");
    for profile_dir in profile_dirs {
        slots.remove(profile_dir);
    }
}

pub async fn acquire_profile(
    profile_dirs: &[PathBuf],
    mission_id: Uuid,
    events_tx: &broadcast::Sender<AgentEvent>,
    cancel: &CancellationToken,
) -> Result<(usize, PathBuf, ProfileLock), AgentResult> {
    let mut announced_wait = false;
    loop {
        let now = Instant::now();
        let mut candidates = profile_dirs.iter().enumerate().collect::<Vec<_>>();
        // A compatibility failure is not quarantined until it repeats, but a
        // clean alternative must still win the next lease. This lets a
        // controller perform its single different-slot retry without making
        // one transient selector miss sideline the profile for ten minutes.
        candidates.sort_by_key(|(_, profile_dir)| {
            slot_health_at(profile_dir, now).last_failure.is_some()
        });
        for (slot, profile_dir) in candidates {
            if is_quarantined_at(profile_dir, now) {
                continue;
            }
            match try_lock_profile(profile_dir) {
                Ok(Some(lock)) => return Ok((slot, profile_dir.clone(), lock)),
                Ok(None) => {}
                Err(error) => {
                    // A broken lock path is local to this profile. Keep the
                    // pool available when another configured slot is healthy;
                    // if every slot is unavailable, the ordinary wait/cancel
                    // path below fails closed without selecting one.
                    if !announced_wait {
                        tracing::warn!(
                            profile_name = profile_dir
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("profile"),
                            error = %error,
                            "ChatGPT UI profile slot is unavailable"
                        );
                    }
                }
            }
        }
        if !announced_wait {
            let _ = events_tx.send(AgentEvent::MissionActivity {
                label: "Waiting for a ChatGPT UI browser slot…".to_string(),
                tool_name: "chatgpt_ui_profile_pool".to_string(),
                mission_id: Some(mission_id),
            });
            announced_wait = true;
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                let shutdown = crate::api::routes::is_shutdown_initiated();
                return Err(AgentResult::failure(
                    if shutdown {
                        "Server restart — paused while waiting for a ChatGPT UI browser slot."
                    } else {
                        "Mission cancelled while waiting for a ChatGPT UI browser slot"
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSlotState {
    Available,
    InUse,
    Quarantined,
    Unavailable,
}

/// Privacy-safe snapshot of one pool slot: exposes only the directory
/// basename, never the full operator path or any profile contents.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSlotStatus {
    pub slot: usize,
    pub profile_name: String,
    pub state: ProfileSlotState,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantine_remaining_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<SlotFailureKind>,
}

pub fn pool_snapshot(profile_dirs: &[PathBuf]) -> Vec<ProfileSlotStatus> {
    let now = Instant::now();
    profile_dirs
        .iter()
        .enumerate()
        .map(|(index, profile_dir)| {
            let health = slot_health_at(profile_dir, now);
            let quarantine_remaining = health
                .quarantined_until
                .and_then(|until| until.checked_duration_since(now));
            // A momentary probe lock is released immediately and cannot
            // starve waiting missions, which retry every two seconds.
            let lock_state = try_lock_profile(profile_dir);
            let state = match lock_state {
                Ok(None) => ProfileSlotState::InUse,
                Ok(Some(_)) if quarantine_remaining.is_some() => ProfileSlotState::Quarantined,
                Ok(Some(_)) => ProfileSlotState::Available,
                Err(_) => ProfileSlotState::Unavailable,
            };
            ProfileSlotStatus {
                slot: index + 1,
                profile_name: profile_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("profile")
                    .to_string(),
                state,
                consecutive_failures: health.consecutive_failures,
                quarantine_remaining_secs: quarantine_remaining
                    .map(|remaining| remaining.as_secs()),
                last_failure: health.last_failure,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lock_is_exclusive_and_reusable() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let first = try_lock_profile(&profile).unwrap().unwrap();
        assert!(try_lock_profile(&profile).unwrap().is_none());
        drop(first);
        assert!(try_lock_profile(&profile).unwrap().is_some());
    }

    #[tokio::test]
    async fn profile_pool_uses_the_next_free_authenticated_slot() {
        let root = tempfile::tempdir().unwrap();
        let first_profile = root.path().join("profile-1");
        let second_profile = root.path().join("profile-2");
        std::fs::create_dir(&first_profile).unwrap();
        std::fs::create_dir(&second_profile).unwrap();
        reset_registry_for_tests(&[first_profile.clone(), second_profile.clone()]);
        let _first_lease = try_lock_profile(&first_profile).unwrap().unwrap();
        let (events_tx, _) = broadcast::channel(8);
        let cancel = CancellationToken::new();

        let (slot, selected, _lease) = acquire_profile(
            &[first_profile, second_profile.clone()],
            Uuid::new_v4(),
            &events_tx,
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(slot, 1);
        assert_eq!(selected, second_profile);
    }

    #[tokio::test]
    async fn unavailable_slot_does_not_block_a_healthy_alternative() {
        let root = tempfile::tempdir().unwrap();
        let unavailable = root.path().join("missing-parent").join("profile-1");
        let healthy = root.path().join("profile-2");
        std::fs::create_dir(&healthy).unwrap();
        reset_registry_for_tests(&[unavailable.clone(), healthy.clone()]);
        let (events_tx, _) = broadcast::channel(8);
        let cancel = CancellationToken::new();

        let (slot, selected, _lease) = acquire_profile(
            &[unavailable, healthy.clone()],
            Uuid::new_v4(),
            &events_tx,
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(slot, 1);
        assert_eq!(selected, healthy);
    }

    #[tokio::test]
    async fn quarantined_slot_is_skipped_while_a_healthy_slot_exists() {
        let root = tempfile::tempdir().unwrap();
        let first_profile = root.path().join("profile-1");
        let second_profile = root.path().join("profile-2");
        std::fs::create_dir(&first_profile).unwrap();
        std::fs::create_dir(&second_profile).unwrap();
        reset_registry_for_tests(&[first_profile.clone(), second_profile.clone()]);
        record_slot_failure(&first_profile, SlotFailureKind::Auth);
        let (events_tx, _) = broadcast::channel(8);
        let cancel = CancellationToken::new();

        let (slot, selected, _lease) = acquire_profile(
            &[first_profile.clone(), second_profile.clone()],
            Uuid::new_v4(),
            &events_tx,
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(slot, 1);
        assert_eq!(selected, second_profile);
        reset_registry_for_tests(&[first_profile]);
    }

    #[tokio::test]
    async fn compatibility_failure_prefers_a_different_clean_slot() {
        let root = tempfile::tempdir().unwrap();
        let first_profile = root.path().join("profile-1");
        let second_profile = root.path().join("profile-2");
        std::fs::create_dir(&first_profile).unwrap();
        std::fs::create_dir(&second_profile).unwrap();
        reset_registry_for_tests(&[first_profile.clone(), second_profile.clone()]);
        record_slot_failure(&first_profile, SlotFailureKind::Compatibility);
        let (events_tx, _) = broadcast::channel(8);
        let cancel = CancellationToken::new();

        let (slot, selected, _lease) = acquire_profile(
            &[first_profile.clone(), second_profile.clone()],
            Uuid::new_v4(),
            &events_tx,
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(slot, 1);
        assert_eq!(selected, second_profile);
        reset_registry_for_tests(&[first_profile]);
    }

    #[tokio::test]
    async fn fully_quarantined_pool_waits_until_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let only_profile = root.path().join("profile-1");
        std::fs::create_dir(&only_profile).unwrap();
        reset_registry_for_tests(std::slice::from_ref(&only_profile));
        record_slot_failure(&only_profile, SlotFailureKind::Launch);
        let (events_tx, _) = broadcast::channel(8);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = acquire_profile(
            std::slice::from_ref(&only_profile),
            Uuid::new_v4(),
            &events_tx,
            &cancel,
        )
        .await;

        assert!(result.is_err());
        reset_registry_for_tests(&[only_profile]);
    }

    #[test]
    fn auth_and_launch_failures_quarantine_immediately() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        reset_registry_for_tests(std::slice::from_ref(&profile));
        let now = Instant::now();

        record_slot_failure_at(&profile, SlotFailureKind::Auth, now);
        assert!(is_quarantined_at(&profile, now));
        assert!(!is_quarantined_at(&profile, now + AUTH_QUARANTINE));

        reset_registry_for_tests(std::slice::from_ref(&profile));
        record_slot_failure_at(&profile, SlotFailureKind::Launch, now);
        assert!(is_quarantined_at(&profile, now));
        assert!(!is_quarantined_at(&profile, now + LAUNCH_QUARANTINE));
        reset_registry_for_tests(&[profile]);
    }

    #[test]
    fn compatibility_failures_quarantine_only_after_repeats() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        reset_registry_for_tests(std::slice::from_ref(&profile));
        let now = Instant::now();

        record_slot_failure_at(&profile, SlotFailureKind::Compatibility, now);
        record_slot_failure_at(&profile, SlotFailureKind::Compatibility, now);
        assert!(!is_quarantined_at(&profile, now));
        record_slot_failure_at(&profile, SlotFailureKind::Compatibility, now);
        assert!(is_quarantined_at(&profile, now));

        record_slot_success(&profile);
        assert!(!is_quarantined_at(&profile, now));
        reset_registry_for_tests(&[profile]);
    }

    #[test]
    fn compatibility_threshold_counts_only_consecutive_compatibility_failures() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        reset_registry_for_tests(std::slice::from_ref(&profile));
        let now = Instant::now();

        record_slot_failure_at(&profile, SlotFailureKind::Compatibility, now);
        record_slot_failure_at(&profile, SlotFailureKind::Compatibility, now);
        record_slot_failure_at(&profile, SlotFailureKind::Launch, now);
        assert_eq!(slot_health_at(&profile, now).consecutive_failures, 1);

        reset_registry_for_tests(&[profile]);
    }

    #[test]
    fn success_resets_slot_health() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        reset_registry_for_tests(std::slice::from_ref(&profile));
        record_slot_failure(&profile, SlotFailureKind::Auth);
        record_slot_success(&profile);
        let status = pool_snapshot(std::slice::from_ref(&profile));
        assert_eq!(status[0].state, ProfileSlotState::Available);
        assert_eq!(status[0].consecutive_failures, 0);
        assert!(status[0].last_failure.is_none());
        reset_registry_for_tests(&[profile]);
    }

    #[test]
    fn pool_snapshot_reports_slot_states_without_full_paths() {
        let root = tempfile::tempdir().unwrap();
        let busy = root.path().join("busy-profile");
        let sick = root.path().join("sick-profile");
        let free = root.path().join("free-profile");
        for profile in [&busy, &sick, &free] {
            std::fs::create_dir(profile).unwrap();
        }
        reset_registry_for_tests(&[busy.clone(), sick.clone(), free.clone()]);
        let _lease = try_lock_profile(&busy).unwrap().unwrap();
        record_slot_failure(&sick, SlotFailureKind::Auth);

        let snapshot = pool_snapshot(&[busy.clone(), sick.clone(), free.clone()]);

        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].slot, 1);
        assert_eq!(snapshot[0].profile_name, "busy-profile");
        assert_eq!(snapshot[0].state, ProfileSlotState::InUse);
        assert_eq!(snapshot[1].state, ProfileSlotState::Quarantined);
        assert_eq!(snapshot[1].last_failure, Some(SlotFailureKind::Auth));
        assert!(snapshot[1].quarantine_remaining_secs.is_some());
        assert_eq!(snapshot[2].state, ProfileSlotState::Available);
        for status in &snapshot {
            assert!(!status.profile_name.contains('/'));
        }
        reset_registry_for_tests(&[busy, sick, free]);
    }

    #[test]
    fn pool_snapshot_never_reports_an_unlockable_slot_as_available() {
        let root = tempfile::tempdir().unwrap();
        let missing_parent = root.path().join("missing").join("profile");

        let snapshot = pool_snapshot(&[missing_parent]);

        assert_eq!(snapshot[0].state, ProfileSlotState::Unavailable);
    }

    #[tokio::test]
    async fn concurrent_missions_never_share_a_profile_slot() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let root = tempfile::tempdir().unwrap();
        let profiles: Vec<PathBuf> = (0..2)
            .map(|index| {
                let profile = root.path().join(format!("profile-{index}"));
                std::fs::create_dir(&profile).unwrap();
                profile
            })
            .collect();
        reset_registry_for_tests(&profiles);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (events_tx, _) = broadcast::channel(64);

        let mut handles = Vec::new();
        for _ in 0..6 {
            let profiles = profiles.clone();
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let events_tx = events_tx.clone();
            handles.push(tokio::spawn(async move {
                let cancel = CancellationToken::new();
                let (slot, profile, lease) =
                    acquire_profile(&profiles, Uuid::new_v4(), &events_tx, &cancel)
                        .await
                        .unwrap();
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now_active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(25)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                drop(lease);
                (slot, profile)
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert!(peak.load(Ordering::SeqCst) <= profiles.len());
    }
}
