//! Background disk-usage watcher.
//!
//! The 2026-07-12 incident: the root filesystem filled up silently until
//! mission workers, then the supervisor itself, started dying on ENOSPC.
//! This task samples root-disk usage every few minutes and pushes an alert
//! through the Paloma webhook (HMAC-signed, same channel as mission status
//! forwarding) when usage crosses the Warn/Critical thresholds, re-alerts
//! periodically while elevated, and sends a recovery notice once usage
//! drops back under the hysteresis band.
//!
//! Delivery is best-effort: `tracing::error!`/`warn!` is the baseline that
//! always fires; the webhook only when `PALOMA_WEBHOOK_FORWARD_URL` is set.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::monitoring::{self, DiskHealthLevel};
use super::routes::AppState;

/// How often usage is sampled.
const TICK_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Hysteresis: recovery is announced only once usage falls this many
/// percentage points below the warn threshold, so a value oscillating
/// around the threshold doesn't flap alert/recovery pairs.
const RECOVERY_MARGIN_PCT: f32 = 3.0;

fn repeat_interval() -> Duration {
    let hours = std::env::var("DISK_ALERT_REPEAT_HOURS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|h| *h >= 1)
        .unwrap_or(24);
    Duration::from_secs(hours * 3600)
}

fn rank(level: DiskHealthLevel) -> u8 {
    match level {
        DiskHealthLevel::Ok => 0,
        DiskHealthLevel::Warn => 1,
        DiskHealthLevel::Critical => 2,
    }
}

/// Notification state belongs to the physical root being sampled, never to
/// the highest sample in a tick.  Persisted roots can be on separate mounts;
/// an alert for one mount must not suppress the first alert for another.
#[derive(Clone, Copy, Debug)]
struct AlertState {
    level: DiskHealthLevel,
    last_alert_at: Option<tokio::time::Instant>,
}

impl Default for AlertState {
    fn default() -> Self {
        Self {
            level: DiskHealthLevel::Ok,
            last_alert_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlertAction {
    Notify(DiskHealthLevel),
    Recover,
    None,
}

/// Prefer an OS filesystem identity so two persisted roots on the same mount
/// share one alert cadence. The canonical path remains the portable fallback
/// (and also identifies mounts on platforms without a device number).
fn filesystem_identity(root: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(root) {
            return format!("device:{}", metadata.dev());
        }
    }
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

fn alert_action(
    state: &mut AlertState,
    level: DiskHealthLevel,
    percent: f32,
    now: tokio::time::Instant,
) -> AlertAction {
    let escalated = rank(level) > rank(state.level);
    let repeat_due = state
        .last_alert_at
        .map(|then| now.duration_since(then) >= repeat_interval())
        .unwrap_or(true);
    let recovered = state.level != DiskHealthLevel::Ok
        && percent < monitoring::disk_warn_pct() - RECOVERY_MARGIN_PCT;

    if level != DiskHealthLevel::Ok && (escalated || repeat_due) {
        state.level = level;
        state.last_alert_at = Some(now);
        AlertAction::Notify(level)
    } else if recovered {
        state.level = DiskHealthLevel::Ok;
        state.last_alert_at = None;
        AlertAction::Recover
    } else {
        AlertAction::None
    }
}

/// Spawn the watcher loop. Safe to call once at server start.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        run_loop(state).await;
    });
}

async fn run_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    // Skip the boot tick; the server is busy starting subsystems.
    interval.tick().await;
    // Filesystem identity is a stable key for the mounted location. State is
    // deliberately retained while a root is temporarily absent, preventing an
    // unmount/remount from producing alert spam.
    let mut alerts: HashMap<String, AlertState> = HashMap::new();
    loop {
        interval.tick().await;
        // Roots persisted for active missions can outlive a configuration
        // change, so sample every filesystem that can still host one rather
        // than only today's MISSION_WORKSPACE_ROOT.
        let workspace = crate::workspace::Workspace::default_host(state.config.working_dir.clone());
        let usages = crate::workspace::mission_workspace_roots_for_workspace(&workspace)
            .into_iter()
            .filter_map(|root| match monitoring::disk_usage_for_path(&root) {
                Ok(usage) => Some((root.canonicalize().unwrap_or(root), usage)),
                Err(error) => {
                    tracing::warn!(path = %root.display(), %error, "disk watcher: cannot measure mission workspace filesystem");
                    None
                }
            });
        for (mission_root, usage) in usages {
            let used = usage.used;
            let total = usage.total;
            let percent = if total == 0 {
                100.0
            } else {
                used as f32 / total as f32 * 100.0
            };
            let level = DiskHealthLevel::from_percent(percent);
            if level != DiskHealthLevel::Ok {
                let gc_state = Arc::clone(&state);
                tokio::spawn(async move {
                    super::mission_workspace_gc::run_pressure_sweep(&gc_state).await;
                });
            }
            match alert_action(
                alerts
                    .entry(filesystem_identity(&mission_root))
                    .or_default(),
                level,
                percent,
                tokio::time::Instant::now(),
            ) {
                AlertAction::Notify(level) => {
                    let free_gb = total.saturating_sub(used) / (1024 * 1024 * 1024);
                    let message = format!(
                "Disk {level:?}: mission filesystem {} at {percent:.1}% ({free_gb} GB free). \
                 Mission admission is refused at critical level.",
                mission_root.display()
            );
                    match level {
                        DiskHealthLevel::Critical => tracing::error!(percent, free_gb, "{message}"),
                        _ => tracing::warn!(percent, free_gb, "{message}"),
                    }
                    deliver_webhook(&state, level, used, total, percent, &message).await;
                }
                AlertAction::Recover => {
                    let message = format!(
                        "Disk recovered: mission filesystem {} back to {percent:.1}% used.",
                        mission_root.display()
                    );
                    tracing::info!(percent, "{message}");
                    deliver_webhook(&state, DiskHealthLevel::Ok, used, total, percent, &message)
                        .await;
                }
                AlertAction::None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_and_recoveries_are_independent_per_persisted_root() {
        let now = tokio::time::Instant::now();
        let mut roots = HashMap::<String, AlertState>::new();
        let root_a = PathBuf::from("/mounted/root-a");
        let root_b = PathBuf::from("/mounted/root-b");

        assert_eq!(
            alert_action(
                roots.entry(root_a.display().to_string()).or_default(),
                DiskHealthLevel::Critical,
                96.0,
                now
            ),
            AlertAction::Notify(DiskHealthLevel::Critical)
        );
        // A different filesystem at the same severity is its own first
        // notification, not suppressed by root A's outstanding alert.
        assert_eq!(
            alert_action(
                roots.entry(root_b.display().to_string()).or_default(),
                DiskHealthLevel::Critical,
                96.0,
                now
            ),
            AlertAction::Notify(DiskHealthLevel::Critical)
        );
        assert_eq!(
            alert_action(
                roots.entry(root_a.display().to_string()).or_default(),
                DiskHealthLevel::Ok,
                80.0,
                now
            ),
            AlertAction::Recover
        );
        // Root B stays elevated but does not spam a second alert before its
        // repeat interval merely because root A recovered.
        assert_eq!(
            alert_action(
                roots.entry(root_b.display().to_string()).or_default(),
                DiskHealthLevel::Critical,
                96.0,
                now
            ),
            AlertAction::None
        );
    }
}

/// POST the alert to the Paloma webhook, HMAC-signed over the raw body with
/// `PALOMA_WEBHOOK_SECRET` (GitHub-style `X-Hub-Signature-256`), matching the
/// mission-status forwarder so the receiving end verifies both identically.
async fn deliver_webhook(
    state: &Arc<AppState>,
    level: DiskHealthLevel,
    used: u64,
    total: u64,
    percent: f32,
    message: &str,
) {
    let Some(url) = state.config.paloma_webhook_forward_url.clone() else {
        return;
    };
    let body = serde_json::json!({
        "type": "disk_alert",
        "event_id": uuid::Uuid::new_v4(),
        "level": level,
        "disk_used": used,
        "disk_total": total,
        "disk_percent": percent,
        "message": message,
        "occurred_at": chrono::Utc::now().to_rfc3339(),
    });
    let payload = match serde_json::to_vec(&body) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(?err, "failed to serialize disk alert payload");
            return;
        }
    };
    let signature = state.config.paloma_webhook_secret.as_ref().and_then(|s| {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(s.as_bytes()).ok()?;
        mac.update(&payload);
        Some(format!(
            "sha256={}",
            hex::encode(mac.finalize().into_bytes())
        ))
    });
    let http = reqwest::Client::new();
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut request = http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.clone());
        if let Some(signature) = signature.as_deref() {
            request = request.header("X-Hub-Signature-256", signature);
        }
        match request.send().await {
            Ok(resp) if resp.status().is_success() => return,
            Ok(resp) => {
                tracing::warn!(attempt, status = %resp.status(), "disk alert webhook non-success");
            }
            Err(err) => {
                tracing::warn!(attempt, ?err, "disk alert webhook send failed");
            }
        }
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
        }
    }
}
