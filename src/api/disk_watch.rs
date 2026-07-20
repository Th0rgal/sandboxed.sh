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
    // The level we last notified about (Ok = nothing outstanding).
    let mut alerted_level = DiskHealthLevel::Ok;
    let mut last_alert_at: Option<tokio::time::Instant> = None;
    loop {
        interval.tick().await;
        let (used, total, percent) = monitoring::current_disk_usage();
        let level = DiskHealthLevel::from_percent(percent);

        let escalated = rank(level) > rank(alerted_level);
        let repeat_due = last_alert_at
            .map(|t| t.elapsed() >= repeat_interval())
            .unwrap_or(true);
        let recovered = alerted_level != DiskHealthLevel::Ok
            && percent < monitoring::disk_warn_pct() - RECOVERY_MARGIN_PCT;

        if level != DiskHealthLevel::Ok {
            let gc_state = Arc::clone(&state);
            tokio::spawn(async move {
                super::mission_workspace_gc::run_pressure_sweep(&gc_state).await;
            });
        }

        if level != DiskHealthLevel::Ok && (escalated || repeat_due) {
            let free_gb = total.saturating_sub(used) / (1024 * 1024 * 1024);
            let message = format!(
                "Disk {level:?}: root filesystem at {percent:.1}% ({free_gb} GB free). \
                 Mission admission is refused at critical level."
            );
            match level {
                DiskHealthLevel::Critical => tracing::error!(percent, free_gb, "{message}"),
                _ => tracing::warn!(percent, free_gb, "{message}"),
            }
            deliver_webhook(&state, level, used, total, percent, &message).await;
            alerted_level = level;
            last_alert_at = Some(tokio::time::Instant::now());
        } else if recovered {
            let message = format!(
                "Disk recovered: root filesystem back to {percent:.1}% used; \
                 mission admission restored."
            );
            tracing::info!(percent, "{message}");
            deliver_webhook(&state, DiskHealthLevel::Ok, used, total, percent, &message).await;
            alerted_level = DiskHealthLevel::Ok;
            last_alert_at = None;
        }
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
