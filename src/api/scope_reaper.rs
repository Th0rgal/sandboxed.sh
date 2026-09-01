//! Lifecycle for per-exec transient scopes (`sandboxed-exec-*.scope`).
//!
//! Harness processes (codex app-server, claude, …) are spawned into
//! transient scopes via `systemd-run --scope --collect` + `nsenter`. Killing
//! the local child only kills the nsenter wrapper — the payload inside the
//! container PID namespace survives, and `--collect` never collects a scope
//! that still has processes. On prod this leaked 312 zombie app-server
//! scopes pinning ~250 mission workspace dirs.
//!
//! Two mechanisms fix it:
//! - an event listener stops a mission's scopes the moment the mission
//!   reaches a stopping status (terminal always; AwaitingUser/Paused behind
//!   default-on toggles — resume re-spawns the harness, it never reattaches
//!   to a live process);
//! - a periodic reaper sweeps zombies the listener missed (crashes,
//!   restarts, legacy-named scopes from before the mission tag existed).
//!
//! `WaitingBackground` is always exempt: those missions park with live
//! background shell jobs that DO live in an exec scope.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::control::{AgentEvent, MissionStatus};
use super::mission_store::MissionStore;
use super::mission_workspace_gc::{build_mission_index, entry_for_workspace};
use super::routes::AppState;
use crate::workspace_exec::{
    machine_name_for_path, machine_name_from_exec_unit, mission_short_id_from_exec_unit,
};

fn env_flag(var: &str, default: bool) -> bool {
    match std::env::var(var) {
        Ok(v) => {
            let v = v.trim();
            if default {
                v != "0" && !v.eq_ignore_ascii_case("false")
            } else {
                v == "1" || v.eq_ignore_ascii_case("true")
            }
        }
        Err(_) => default,
    }
}

fn stop_on_awaiting_user() -> bool {
    env_flag("SCOPE_STOP_ON_AWAITING_USER", true)
}

fn stop_on_pause() -> bool {
    env_flag("SCOPE_STOP_ON_PAUSE", true)
}

fn reaper_enabled() -> bool {
    env_flag("SCOPE_REAPER_ENABLED", true)
}

fn reaper_interval() -> Duration {
    let minutes = std::env::var("SCOPE_REAPER_INTERVAL_MINUTES")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|m| *m >= 1)
        .unwrap_or(15);
    Duration::from_secs(minutes * 60)
}

/// Grace between a stopping status event and the actual scope stop. Must be
/// comfortably LONGER than the background watcher's 10s reconciliation poll
/// (`bg_autoresume::BG_POLL_INTERVAL`): a mission that parks AwaitingUser
/// right after launching a background job is only promoted to
/// WaitingBackground on the watcher's next tick, and the pre-stop status
/// recheck must observe that promotion.
fn teardown_grace() -> Duration {
    let secs = std::env::var("SCOPE_TEARDOWN_GRACE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s >= 1)
        .unwrap_or(30);
    Duration::from_secs(secs)
}

fn zombie_max_age() -> Duration {
    let hours = std::env::var("SCOPE_ZOMBIE_MAX_AGE_HOURS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(12);
    Duration::from_secs(hours * 3600)
}

/// Statuses whose scopes must never be touched: the mission is running or
/// parked with live background work.
fn status_keeps_scopes(status: &MissionStatus) -> bool {
    match status {
        MissionStatus::Active | MissionStatus::Pending | MissionStatus::WaitingBackground => true,
        MissionStatus::AwaitingUser => !stop_on_awaiting_user(),
        MissionStatus::Paused => !stop_on_pause(),
        _ => false,
    }
}

/// True when a status transition should trigger immediate scope teardown.
fn status_triggers_teardown(status: &MissionStatus) -> bool {
    match status {
        MissionStatus::Completed
        | MissionStatus::Failed
        | MissionStatus::Interrupted
        | MissionStatus::Blocked
        | MissionStatus::NotFeasible
        | MissionStatus::Acknowledged => true,
        MissionStatus::AwaitingUser => stop_on_awaiting_user(),
        MissionStatus::Paused => stop_on_pause(),
        MissionStatus::Active | MissionStatus::Pending | MissionStatus::WaitingBackground => false,
    }
}

/// Schedule the guarded teardown for one status transition.
///
/// Most transitions arrive through the broadcast listener below, but callers
/// that persist terminal status also invoke this directly. That makes scope
/// cleanup reliable even when the broadcast receiver is lagged, restarted, or
/// temporarily absent. The delayed store recheck preserves live background
/// work exactly as the listener path does.
pub(crate) fn schedule_mission_scope_teardown(
    mission_store: Arc<dyn MissionStore>,
    mission_id: Uuid,
    status: MissionStatus,
) {
    if !status_triggers_teardown(&status) {
        return;
    }
    tokio::spawn(async move {
        let reason = format!("mission status {status:?}");
        tokio::time::sleep(teardown_grace()).await;
        match mission_store.get_mission(mission_id).await {
            Ok(Some(m)) if status_keeps_scopes(&m.status) => {
                tracing::debug!(
                    %mission_id,
                    status = ?m.status,
                    "scope teardown skipped: status changed during grace"
                );
                return;
            }
            _ => {}
        }
        stop_mission_exec_scopes(mission_id, &reason).await;
    });
}

fn legacy_scope_is_reapable(
    mission_index_complete: bool,
    age: Option<Duration>,
    max_age: Duration,
    owned_by_live_workspace: bool,
) -> bool {
    mission_index_complete && age.is_some_and(|age| age >= max_age) && !owned_by_live_workspace
}

/// A mission-tagged scope missing from the cross-store index is not proof of
/// orphanhood. Index construction races session/store registration and has
/// briefly missed live missions in production. Require the same age evidence
/// as legacy scopes before treating an unknown tag as stale; status-driven
/// teardown remains the fast path for known terminal missions.
fn unknown_tagged_scope_is_reapable(
    mission_index_complete: bool,
    age: Option<Duration>,
    max_age: Duration,
) -> bool {
    mission_index_complete && age.is_some_and(|age| age >= max_age)
}

/// List all `sandboxed-exec-*.scope` unit names currently known to systemd.
pub(crate) async fn try_list_exec_scope_units() -> Result<Vec<String>, String> {
    let output = Command::new("systemctl")
        .args([
            "list-units",
            "--plain",
            "--no-legend",
            "--all",
            "sandboxed-exec-*.scope",
        ])
        .output()
        .await
        .map_err(|err| format!("systemctl list-units failed: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "systemctl list-units exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let units: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.starts_with("sandboxed-exec-") && unit.ends_with(".scope"))
        .map(|s| s.to_string())
        .collect();
    if !stdout.trim().is_empty() && units.is_empty() {
        return Err("systemctl list-units returned no parseable exec scopes".to_string());
    }
    Ok(units)
}

pub(crate) async fn list_exec_scope_units() -> Vec<String> {
    match try_list_exec_scope_units().await {
        Ok(units) => units,
        Err(err) => {
            tracing::warn!(%err, "could not list exec scopes");
            Vec::new()
        }
    }
}

pub(crate) async fn stop_unit(unit: &str, reason: &str) -> bool {
    match Command::new("systemctl")
        .args(["stop", unit])
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            tracing::info!(unit, reason, "stopped exec scope");
            true
        }
        Ok(o) => {
            tracing::warn!(
                unit,
                reason,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "failed to stop exec scope"
            );
            false
        }
        Err(err) => {
            tracing::warn!(unit, reason, ?err, "systemctl stop failed");
            false
        }
    }
}

/// Stop every exec scope tagged with `mission_id`'s short id. Returns the
/// number of scopes stopped. No-op when the host has no systemd scopes
/// (caps disabled) — the unit list is simply empty.
pub async fn stop_mission_exec_scopes(mission_id: Uuid, reason: &str) -> usize {
    let mut stopped = 0;
    for unit in list_exec_scope_units().await {
        if crate::workspace_exec::exec_unit_belongs_to_mission(&unit, mission_id)
            && stop_unit(&unit, reason).await
        {
            stopped += 1;
        }
    }
    stopped
}

/// Event-driven teardown: stop a mission's scopes as soon as its status
/// stops needing them. Spawned once per control session next to the other
/// event listeners (telegram alerts, paloma forwarder).
pub fn spawn_status_listener(
    mut events: broadcast::Receiver<AgentEvent>,
    mission_store: Arc<dyn MissionStore>,
) {
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(AgentEvent::MissionStatusChanged {
                    mission_id, status, ..
                }) => {
                    // Detached so the grace sleep never stalls this receiver
                    // into broadcast lag.
                    schedule_mission_scope_teardown(Arc::clone(&mission_store), mission_id, status);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    tracing::warn!(dropped, "scope teardown listener lagged; continuing");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Age of a unit, from systemd's monotonic activation timestamp compared to
/// the host uptime. Monotonic avoids parsing locale/timezone-formatted
/// wall-clock strings and is immune to clock adjustments.
async fn unit_age(unit: &str) -> Option<Duration> {
    let output = Command::new("systemctl")
        .args([
            "show",
            "-p",
            "ActiveEnterTimestampMonotonic",
            "--value",
            unit,
        ])
        .output()
        .await
        .ok()?;
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let entered_us: u64 = raw.parse().ok().filter(|v| *v > 0)?;
    let uptime = tokio::fs::read_to_string("/proc/uptime").await.ok()?;
    let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let uptime_us = (uptime_secs * 1_000_000.0) as u64;
    Some(Duration::from_micros(uptime_us.saturating_sub(entered_us)))
}

/// Spawn the periodic zombie reaper. Safe to call once at server start.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(reaper_interval());
        interval.tick().await; // skip boot tick
        loop {
            interval.tick().await;
            if !reaper_enabled() {
                continue;
            }
            let report = run_once(&state).await;
            if report.stopped > 0 || report.errors > 0 {
                tracing::info!(
                    scanned = report.scanned,
                    stopped = report.stopped,
                    kept = report.kept,
                    errors = report.errors,
                    "scope reaper sweep finished"
                );
            }
        }
    });
}

#[derive(Default)]
pub struct ReaperReport {
    pub scanned: usize,
    pub stopped: usize,
    pub kept: usize,
    pub errors: usize,
}

async fn run_once(state: &Arc<AppState>) -> ReaperReport {
    let mut report = ReaperReport::default();
    let units = list_exec_scope_units().await;
    if units.is_empty() {
        return report;
    }
    let index_full = build_mission_index(state).await;
    let index = &index_full.by_short;
    // Two token sets over this instance's workspaces (machine-name tokens,
    // cf. `WorkspaceExec::mission_scope_match_token`):
    // - `workspace_ids_by_token`: OWNERSHIP filter and collision-safe lookup.
    //   systemd units are
    //   host-global; when prod and dev instances share a host, each must
    //   only ever act on scopes of its own workspaces — a unit whose token
    //   we can't attribute to one of our workspaces is not ours to stop.
    // - `live_workspace_tokens`: workspaces with a scope-keeping mission,
    //   for the legacy-naming policy (no mission id in the unit name).
    let mut workspace_ids_by_token: std::collections::HashMap<String, Uuid> =
        std::collections::HashMap::new();
    let mut live_workspace_tokens: HashSet<String> = HashSet::new();
    let workspaces = state.workspaces.list().await;
    let live_workspace_ids: HashSet<Uuid> = index
        .values()
        .flatten()
        .filter(|e| status_keeps_scopes(&e.status))
        .map(|e| e.workspace_id)
        .collect();
    for ws in &workspaces {
        if let Some(token) = machine_name_for_path(&ws.path) {
            if live_workspace_ids.contains(&ws.id) {
                live_workspace_tokens.insert(token.clone());
            }
            workspace_ids_by_token.insert(token, ws.id);
        }
    }
    let max_age = zombie_max_age();

    for unit in units {
        report.scanned += 1;
        let unit_machine = machine_name_from_exec_unit(&unit);
        let unit_workspace_id = unit_machine
            .as_ref()
            .and_then(|machine| workspace_ids_by_token.get(machine))
            .copied();
        let Some(unit_workspace_id) = unit_workspace_id else {
            tracing::debug!(unit, "reaper: kept (scope not owned by this instance)");
            report.kept += 1;
            continue;
        };
        match mission_short_id_from_exec_unit(&unit) {
            Some(short) => match index
                .get(&short)
                .and_then(|entries| entry_for_workspace(entries, unit_workspace_id))
            {
                Some(entry) if status_keeps_scopes(&entry.status) => {
                    report.kept += 1;
                }
                Some(entry) => {
                    let reason = format!("reaper: mission {short} status {:?}", entry.status);
                    if stop_unit(&unit, &reason).await {
                        report.stopped += 1;
                    } else {
                        report.errors += 1;
                    }
                }
                None => {
                    let age = unit_age(&unit).await;
                    if !unknown_tagged_scope_is_reapable(index_full.complete, age, max_age) {
                        tracing::debug!(
                            unit,
                            ?age,
                            "reaper: kept (mission unknown without stale-age evidence)"
                        );
                        report.kept += 1;
                    } else if stop_unit(&unit, "reaper: mission unknown to any store").await {
                        report.stopped += 1;
                    } else {
                        report.errors += 1;
                    }
                }
            },
            None => {
                // Legacy naming (or task-tagged): stop only when old enough
                // AND the owning workspace has no scope-keeping mission. An
                // incomplete cross-store index cannot prove that absence, so
                // fail closed just like the mission-tagged branch above.
                let age = unit_age(&unit).await;
                let owned_by_live_ws = unit_machine
                    .as_ref()
                    .is_some_and(|machine| live_workspace_tokens.contains(machine));
                if !legacy_scope_is_reapable(index_full.complete, age, max_age, owned_by_live_ws) {
                    if !index_full.complete {
                        tracing::debug!(
                            unit,
                            "reaper: kept (legacy scope but mission index incomplete)"
                        );
                    }
                    report.kept += 1;
                    continue;
                }
                let reason = format!(
                    "reaper: legacy-named scope older than {}h in workspace with no live mission",
                    max_age.as_secs() / 3600
                );
                if stop_unit(&unit, &reason).await {
                    report.stopped += 1;
                } else {
                    report.errors += 1;
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_scopes_fail_closed_when_mission_index_is_incomplete() {
        let max_age = Duration::from_secs(12 * 3600);
        let old = Some(max_age + Duration::from_secs(1));

        assert!(!legacy_scope_is_reapable(false, old, max_age, false));
        assert!(legacy_scope_is_reapable(true, old, max_age, false));
        assert!(!legacy_scope_is_reapable(true, old, max_age, true));
        assert!(!legacy_scope_is_reapable(
            true,
            Some(max_age - Duration::from_secs(1)),
            max_age,
            false
        ));
        assert!(!legacy_scope_is_reapable(true, None, max_age, false));
    }

    #[test]
    fn unknown_tagged_scopes_require_complete_index_and_stale_age() {
        let max_age = Duration::from_secs(12 * 3600);
        let old = Some(max_age + Duration::from_secs(1));
        assert!(!unknown_tagged_scope_is_reapable(false, old, max_age));
        assert!(!unknown_tagged_scope_is_reapable(
            true,
            Some(max_age - Duration::from_secs(1)),
            max_age
        ));
        assert!(!unknown_tagged_scope_is_reapable(true, None, max_age));
        assert!(unknown_tagged_scope_is_reapable(true, old, max_age));
    }
}
