//! systemd readiness + watchdog integration.
//!
//! With `Type=notify` and `WatchdogSec=` set on the service (see
//! `deploy/systemd/sandboxed-sh-watchdog.conf`), systemd expects:
//!
//! - `READY=1` once the server is actually listening ([`notify_ready`]);
//! - `WATCHDOG=1` at least once per `WatchdogSec`, or it restarts the unit.
//!
//! The watchdog tick is a **real liveness probe of the tokio runtime**, not a
//! timer on a side thread: a dedicated OS thread sends a probe message to a
//! responder task running on the main runtime and only pets the watchdog when
//! the round-trip completes in time. If the runtime is wedged or starved, the
//! probe times out, no `WATCHDOG=1` is sent, and systemd restarts the service.
//!
//! Everything no-ops cleanly when `NOTIFY_SOCKET` is unset (non-systemd runs,
//! Docker, tests). Setting `SANDBOXED_SH_SIMULATE_WEDGE=1` stops ticking (the
//! probes still run) so the restart path can be exercised end to end.

use std::time::Duration;

/// Env var that, when truthy, suppresses `WATCHDOG=1` ticks for testing.
pub const SIMULATE_WEDGE_ENV: &str = "SANDBOXED_SH_SIMULATE_WEDGE";

/// Tick interval derived from systemd's `WATCHDOG_USEC`: a third of the
/// budget, so two consecutive ticks can be missed before a restart. `None`
/// when the watchdog is not armed (unset, unparsable, or zero).
pub fn tick_interval_from_watchdog_usec(watchdog_usec: Option<&str>) -> Option<Duration> {
    let usec: u64 = watchdog_usec?.trim().parse().ok()?;
    if usec == 0 {
        return None;
    }
    Some(Duration::from_micros(usec / 3).max(Duration::from_millis(100)))
}

/// Whether a completed (or failed) probe should result in a `WATCHDOG=1`.
/// Pure so the tick policy is testable without systemd.
pub fn should_pet_watchdog(probe_ok: bool, wedge_simulated: bool) -> bool {
    probe_ok && !wedge_simulated
}

fn wedge_simulated() -> bool {
    std::env::var(SIMULATE_WEDGE_ENV)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// Send `READY=1`. Call once, after the listener is bound. No-op without
/// `NOTIFY_SOCKET`.
pub fn notify_ready() {
    if let Err(err) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
        tracing::debug!(%err, "sd_notify READY failed (not running under systemd?)");
    } else {
        tracing::info!("sd_notify: READY=1 sent");
    }
}

/// Probe the tokio runtime through `probe_tx` and wait up to `timeout` for
/// the responder task's reply. Returns `true` only when the runtime actually
/// scheduled the responder in time.
pub fn probe_runtime(
    probe_tx: &tokio::sync::mpsc::Sender<std::sync::mpsc::Sender<()>>,
    timeout: Duration,
) -> bool {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if probe_tx.blocking_send(reply_tx).is_err() {
        return false;
    }
    reply_rx.recv_timeout(timeout).is_ok()
}

/// Spawn the responder task on the current runtime plus the watchdog thread.
/// Must be called from within the main tokio runtime. No-op when systemd has
/// not armed a watchdog (`WATCHDOG_USEC` unset).
pub fn spawn() {
    let Some(interval) =
        tick_interval_from_watchdog_usec(std::env::var("WATCHDOG_USEC").ok().as_deref())
    else {
        tracing::debug!("systemd watchdog not armed (WATCHDOG_USEC unset); watchdog loop skipped");
        return;
    };

    // Responder: lives on the main runtime. If the runtime can't schedule
    // this trivial task, the probe times out — which is exactly the signal
    // we want to stop petting the watchdog on.
    let (probe_tx, mut probe_rx) = tokio::sync::mpsc::channel::<std::sync::mpsc::Sender<()>>(4);
    tokio::spawn(async move {
        while let Some(reply) = probe_rx.recv().await {
            let _ = reply.send(());
        }
    });

    tracing::info!(
        interval_ms = interval.as_millis() as u64,
        "systemd watchdog armed; starting liveness tick thread"
    );

    // Dedicated OS thread: independent of the runtime it is probing.
    std::thread::Builder::new()
        .name("sd-watchdog".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval);
            let probe_ok = probe_runtime(&probe_tx, interval);
            let wedged = wedge_simulated();
            if should_pet_watchdog(probe_ok, wedged) {
                if let Err(err) = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]) {
                    tracing::warn!(%err, "sd_notify WATCHDOG failed");
                }
            } else if wedged {
                tracing::warn!("watchdog tick suppressed ({SIMULATE_WEDGE_ENV} set)");
            } else {
                tracing::error!(
                    "runtime liveness probe timed out; withholding WATCHDOG=1 (systemd will restart the service)"
                );
            }
        })
        .expect("failed to spawn sd-watchdog thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_is_a_third_of_watchdog_usec() {
        assert_eq!(
            tick_interval_from_watchdog_usec(Some("90000000")),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn interval_absent_or_invalid_disables_watchdog() {
        assert_eq!(tick_interval_from_watchdog_usec(None), None);
        assert_eq!(tick_interval_from_watchdog_usec(Some("")), None);
        assert_eq!(tick_interval_from_watchdog_usec(Some("abc")), None);
        assert_eq!(tick_interval_from_watchdog_usec(Some("0")), None);
    }

    #[test]
    fn tiny_watchdog_budget_is_clamped_to_a_sane_floor() {
        assert_eq!(
            tick_interval_from_watchdog_usec(Some("3")),
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn pet_policy_requires_probe_success_and_no_simulated_wedge() {
        assert!(should_pet_watchdog(true, false));
        assert!(!should_pet_watchdog(false, false));
        assert!(!should_pet_watchdog(true, true));
        assert!(!should_pet_watchdog(false, true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_round_trip_succeeds_on_a_live_runtime() {
        let (probe_tx, mut probe_rx) = tokio::sync::mpsc::channel::<std::sync::mpsc::Sender<()>>(4);
        tokio::spawn(async move {
            while let Some(reply) = probe_rx.recv().await {
                let _ = reply.send(());
            }
        });
        let ok =
            tokio::task::spawn_blocking(move || probe_runtime(&probe_tx, Duration::from_secs(5)))
                .await
                .unwrap();
        assert!(ok);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_fails_when_responder_is_gone() {
        let (probe_tx, probe_rx) = tokio::sync::mpsc::channel::<std::sync::mpsc::Sender<()>>(4);
        drop(probe_rx); // responder never ran / runtime "dead"
        let ok = tokio::task::spawn_blocking(move || {
            probe_runtime(&probe_tx, Duration::from_millis(200))
        })
        .await
        .unwrap();
        assert!(!ok);
    }
}
