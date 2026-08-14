//! Boot-time + on-demand reconciliation of systemd scopes vs mission truth.
//!
//! A service restart leaves three classes of drift behind:
//!
//! 1. **Leaked scopes** — `sandboxed-exec-*` / `sandboxed-durable-*` transient
//!    scopes whose owning mission/job is terminal or unknown (~80 observed on
//!    prod). The periodic [`super::scope_reaper`] eventually catches exec
//!    scopes; this pass sweeps both prefixes immediately at boot and on demand.
//! 2. **Ghost-active missions** — rows still `active` in a store although no
//!    live runner, scope, or durable run backs them. They are flipped to
//!    `interrupted` with reason `service_restart` and auto-resumed through the
//!    existing `ControlCommand::ResumeMission` path.
//! 3. **Orphaned awaiting_user missions** — the mission row survives (GET
//!    works) but its harness/session state is gone: no persisted events and no
//!    active run, so `/events` replay and `/resume` have nothing to work from.
//!    These are tagged `orphaned` in the mission's project tags (cheapest
//!    durable representation — a status-enum addition would need store
//!    migrations and touch every exhaustive status match) and announced on the
//!    control event stream so controllers can triage them.
//!
//! Runs once at boot (delayed, so per-session startup recovery lands first)
//! and from `POST /api/system/reconcile`. Hosts without systemd (Docker
//! installs) skip the scope sweep gracefully: `systemctl` failing to run is
//! treated as "no scopes", never as an error.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::control::{AgentEvent, ControlCommand, MissionStatus};
use super::mission_store::{
    MissionExecutionState, MissionFilter, MissionMode, MissionProjectPatch,
};
use super::mission_workspace_gc::{build_mission_index, entry_for_workspace};
use super::routes::AppState;
use super::scope_reaper::stop_unit;
use crate::workspace_exec::{
    machine_name_for_path, machine_name_from_exec_unit, mission_short_id_from_exec_unit,
};

/// `end_reason` recorded on missions interrupted by this reconciler.
pub const SERVICE_RESTART_REASON: &str = "service_restart";
/// Graceful SIGTERM / SIGINT drain (see `routes::shutdown_signal`).
pub const SERVER_SHUTDOWN_REASON: &str = "server_shutdown";
/// Cancel token fired but the JoinHandle never resolved. During a deploy
/// this overwrites the `server_shutdown` the drain just wrote — treat it
/// as the same class when a deploy marker is fresh.
pub const FORCE_KILLED_CANCEL_TIMEOUT_REASON: &str = "force_killed_after_cancel_timeout";
/// How recent a `/api/system/deploy` marker must be for a
/// `force_killed_after_cancel_timeout` row to count as a deploy interrupt.
const DEPLOY_FORCE_KILL_WINDOW_SECS: u64 = 30 * 60;

/// Reasons a restart/deploy may leave on a still-resumable mission.
pub fn is_recoverable_restart_reason(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some(SERVICE_RESTART_REASON) | Some(SERVER_SHUTDOWN_REASON)
    )
}

/// A cancel-timeout kill that landed inside a just-finished deploy is the
/// drain race (SIGTERM → cancel → JoinHandle hung → force-kill overwrites
/// `server_shutdown`). User-cancelled stuck runners outside that window
/// stay interrupted.
pub fn is_recent_deploy_force_kill(reason: Option<&str>, deploy_age_secs: Option<u64>) -> bool {
    reason == Some(FORCE_KILLED_CANCEL_TIMEOUT_REASON)
        && deploy_age_secs.is_some_and(|age| age < DEPLOY_FORCE_KILL_WINDOW_SECS)
}
/// Tag appended to a mission's project tags when its harness session state is
/// gone (see module docs, class 3).
pub const ORPHANED_TAG: &str = "orphaned";

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

fn boot_reconcile_enabled() -> bool {
    env_flag("BOOT_RECONCILE_ENABLED", true)
}

/// Delay before the boot pass so per-session startup recovery
/// (`recover_server_shutdown_missions`) and the eager control-session boot
/// finish first — reconcile is idempotent, this just avoids duplicate work.
fn boot_delay() -> Duration {
    let secs = std::env::var("BOOT_RECONCILE_DELAY_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(45);
    Duration::from_secs(secs)
}

/// Missions whose row was touched more recently than this are left alone by
/// the ghost-active check: a turn may be starting right now and the actor's
/// running list can lag the store write by a moment.
fn active_grace() -> chrono::Duration {
    let secs = std::env::var("BOOT_RECONCILE_ACTIVE_GRACE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(180);
    chrono::Duration::seconds(secs)
}

/// How far back a `interrupted(service_restart)` mission may have been last
/// touched and still be re-resumed. Guards against resurrecting ancient rows
/// left interrupted long ago. Default 6h.
fn interrupted_horizon() -> chrono::Duration {
    let secs = std::env::var("RECONCILE_INTERRUPTED_HORIZON_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(6 * 60 * 60);
    chrono::Duration::seconds(secs)
}

/// Cap on how many `interrupted(service_restart)` missions a single pass will
/// re-resume, so a restart storm can't stampede the actor. Default 20.
fn interrupted_resume_cap() -> usize {
    std::env::var("RECONCILE_INTERRUPTED_RESUME_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(20)
}

// ==================== scope-name parsing ====================

/// A sandboxed scope unit name, decomposed. Non-sandboxed scopes parse to
/// `None` and are skipped entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedScope {
    Exec {
        /// Workspace machine token (`sandboxed-<name>-<8hex>`), when the unit
        /// name is well-formed.
        machine: Option<String>,
        /// 8-hex mission short id embedded via the `-m<hex>-` tag. `None` for
        /// legacy or task-tagged units.
        mission_short: Option<String>,
    },
    Durable {
        /// Workspace machine token.
        machine: Option<String>,
        /// Durable job id (32-hex simple uuid trailing segment).
        job_id: Option<Uuid>,
    },
}

/// Parse a systemd scope unit name into a [`ParsedScope`]. Tolerant: units
/// that are not `sandboxed-exec-*`/`sandboxed-durable-*` scopes return `None`;
/// malformed sandboxed names still return the variant with `None` fields so
/// the caller can decide to keep them (fail closed).
pub fn parse_scope_unit(unit: &str) -> Option<ParsedScope> {
    if !unit.ends_with(".scope") {
        return None;
    }
    if unit.starts_with("sandboxed-exec-") {
        Some(ParsedScope::Exec {
            machine: machine_name_from_exec_unit(unit),
            mission_short: mission_short_id_from_exec_unit(unit),
        })
    } else if unit.starts_with("sandboxed-durable-") {
        let (machine, job_id) = parse_durable_unit(unit);
        Some(ParsedScope::Durable { machine, job_id })
    } else {
        None
    }
}

/// Split `sandboxed-durable-<machine>-<32hex>` into machine token and job id.
/// (See `workspace_exec::durable_scope_unit` for the producer.)
fn parse_durable_unit(unit: &str) -> (Option<String>, Option<Uuid>) {
    let Some(name) = unit
        .strip_suffix(".scope")
        .unwrap_or(unit)
        .strip_prefix("sandboxed-durable-")
    else {
        return (None, None);
    };
    let Some((machine, id)) = name.rsplit_once('-') else {
        return (None, None);
    };
    if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        (Some(machine.to_string()), Uuid::parse_str(id).ok())
    } else {
        (None, None)
    }
}

// ==================== classification (pure) ====================

/// What to do with one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    Stop,
    Keep,
}

/// True while a mission status still legitimately owns live processes.
fn status_needs_scopes(status: &MissionStatus) -> bool {
    // Parked-but-resumable statuses (AwaitingUser, Paused, WaitingBackground)
    // are kept here: resume semantics for background jobs depend on the scope
    // and the event-driven teardown in scope_reaper already handles the
    // configurable AwaitingUser/Paused cases with its own grace. Reconcile
    // only stops what is unambiguously dead: terminal or acknowledged.
    !(status.is_terminal() || *status == MissionStatus::Acknowledged)
}

/// Classify an exec scope given what the mission index knows about its owner.
/// `mission_status = None` means the mission is unknown; that only justifies a
/// stop when `index_complete` proves every store was actually scanned.
pub fn classify_exec_scope(
    mission_status: Option<&MissionStatus>,
    index_complete: bool,
) -> ScopeVerdict {
    match mission_status {
        Some(status) if status_needs_scopes(status) => ScopeVerdict::Keep,
        Some(_) => ScopeVerdict::Stop,
        None if index_complete => ScopeVerdict::Stop,
        None => ScopeVerdict::Keep,
    }
}

/// Classify a durable scope given the job registry entry. `None` (missing
/// job.json) or a terminal/unknown status means the scope is leaked.
pub fn classify_durable_scope(
    job_status: Option<&super::durable_jobs::DurableJobStatus>,
) -> ScopeVerdict {
    use super::durable_jobs::DurableJobStatus as S;
    match job_status {
        Some(S::Running) => ScopeVerdict::Keep,
        Some(S::Completed) | Some(S::Failed) | Some(S::Cancelled) | Some(S::Unknown) | None => {
            ScopeVerdict::Stop
        }
    }
}

/// What to do with one mission row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionVerdict {
    /// Flip to `interrupted` (reason `service_restart`) and auto-resume.
    InterruptAndResume,
    /// Tag `orphaned` + announce on the control stream.
    TagOrphaned,
    Keep,
}

/// Inputs for classifying one mission. Gathered by the async sweep, decided
/// here so the policy is unit-testable without systemd or a live actor.
#[derive(Debug, Clone, Copy)]
pub struct MissionFacts {
    pub status: MissionStatus,
    pub is_assistant_mode: bool,
    /// The control actor currently lists this mission as running.
    pub running_in_actor: bool,
    /// A live `sandboxed-exec-*` scope carries this mission's short id.
    pub has_live_scope: bool,
    /// A detached durable run (remote job) owns liveness.
    pub has_durable_run: bool,
    /// The mission row was updated within the grace window.
    pub recently_updated: bool,
    /// The event log has at least one persisted event for this mission.
    pub has_events: bool,
    /// The mission already carries the `orphaned` tag.
    pub already_tagged_orphaned: bool,
}

pub fn classify_mission(facts: &MissionFacts) -> MissionVerdict {
    match facts.status {
        MissionStatus::Active => {
            if facts.is_assistant_mode
                || facts.running_in_actor
                || facts.has_live_scope
                || facts.has_durable_run
                || facts.recently_updated
            {
                MissionVerdict::Keep
            } else {
                MissionVerdict::InterruptAndResume
            }
        }
        // Pending missions never started a harness — the dispatcher owns
        // them; interrupting would only add churn.
        MissionStatus::AwaitingUser => {
            if !facts.has_events
                && !facts.has_durable_run
                && !facts.running_in_actor
                && !facts.already_tagged_orphaned
            {
                MissionVerdict::TagOrphaned
            } else {
                MissionVerdict::Keep
            }
        }
        _ => MissionVerdict::Keep,
    }
}

/// Inputs for deciding whether an already-`interrupted` mission (from a PRIOR
/// restart) should be re-resumed. A service restart that interrupts a mission,
/// followed by a second restart before the auto-resume lands, otherwise leaves
/// the mission stuck `interrupted` forever (needing a manual resume).
#[derive(Debug, Clone, Copy)]
pub struct InterruptedFacts {
    pub status: MissionStatus,
    /// The mission's `terminal_reason` is a restart/deploy interrupt
    /// (`service_restart`, `server_shutdown`, or a cancel-timeout kill
    /// inside the deploy window). User-cancelled and genuinely-failed
    /// missions stay false.
    pub is_service_restart: bool,
    /// The control actor currently lists this mission as running.
    pub running_in_actor: bool,
    /// A live `sandboxed-exec-*` scope carries this mission's short id.
    pub has_live_scope: bool,
    /// A detached durable run (remote job) owns liveness.
    pub has_durable_run: bool,
    /// The mission row was updated within `RECONCILE_INTERRUPTED_HORIZON_SECS`.
    pub within_horizon: bool,
    /// A live mission with the SAME project+track already exists — the
    /// controller has re-dispatched, so resuming would duplicate it.
    pub live_retry_exists: bool,
}

/// True iff an `interrupted(service_restart)` mission with no live backing,
/// recently touched, and no competing live retry should be auto-resumed.
pub fn should_reresume_interrupted(facts: &InterruptedFacts) -> bool {
    facts.status == MissionStatus::Interrupted
        && facts.is_service_restart
        && !facts.running_in_actor
        && !facts.has_live_scope
        && !facts.has_durable_run
        && facts.within_horizon
        && !facts.live_retry_exists
}

// ==================== systemd enumeration ====================

/// List all sandboxed scope units. `Err` means systemd is unavailable or the
/// listing failed — callers must skip scope work, not treat it as "no scopes
/// exist". Docker installs without systemd PID 1 land here.
pub(crate) async fn try_list_sandboxed_scope_units() -> Result<Vec<String>, String> {
    let output = Command::new("systemctl")
        .args([
            "list-units",
            "--type=scope",
            "--all",
            "--no-legend",
            "--plain",
        ])
        .output()
        .await
        .map_err(|err| format!("systemctl unavailable: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "systemctl list-units exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| {
            unit.ends_with(".scope")
                && (unit.starts_with("sandboxed-exec-") || unit.starts_with("sandboxed-durable-"))
        })
        .map(|s| s.to_string())
        .collect())
}

// ==================== the sweep ====================

#[derive(Debug, Default, Serialize)]
pub struct ReconcileReport {
    /// systemd reachable and scopes enumerated this pass.
    pub systemd_available: bool,
    pub scopes_scanned: usize,
    pub scopes_stopped: usize,
    pub scopes_kept: usize,
    pub missions_scanned: usize,
    pub missions_interrupted: usize,
    pub missions_resumed: usize,
    /// Missions found already `interrupted(service_restart)` from a prior
    /// restart (no live backing, within horizon) and re-resumed this pass.
    pub missions_reresumed: usize,
    pub missions_tagged_orphaned: usize,
    pub errors: usize,
}

/// One full reconcile pass. Safe to run concurrently with normal traffic and
/// repeatedly (idempotent: already-handled rows classify as `Keep`).
pub async fn run(state: &Arc<AppState>) -> ReconcileReport {
    let mut report = ReconcileReport::default();

    // ---- Phase 1: scopes ----
    let units = match try_list_sandboxed_scope_units().await {
        Ok(units) => {
            report.systemd_available = true;
            units
        }
        Err(err) => {
            tracing::info!(%err, "reconcile: systemd scope listing unavailable; skipping scope sweep");
            Vec::new()
        }
    };

    // Live-mission short ids, used both to keep scopes and (inverted) to spot
    // ghost-active missions with no scope.
    let mut live_short_ids: HashSet<String> = HashSet::new();

    if !units.is_empty() {
        let index_full = build_mission_index(state).await;
        let index = &index_full.by_short;

        // Ownership: systemd units are host-global. Only act on scopes whose
        // machine token maps to one of THIS instance's workspaces — prod and
        // dev instances share hosts.
        let mut workspace_ids_by_token: std::collections::HashMap<String, Uuid> =
            std::collections::HashMap::new();
        for ws in state.workspaces.list().await {
            if let Some(token) = machine_name_for_path(&ws.path) {
                workspace_ids_by_token.insert(token, ws.id);
            }
        }

        for unit in &units {
            let Some(parsed) = parse_scope_unit(unit) else {
                continue;
            };
            report.scopes_scanned += 1;
            let verdict = match &parsed {
                ParsedScope::Exec {
                    machine,
                    mission_short,
                } => {
                    let owned_ws = machine
                        .as_ref()
                        .and_then(|m| workspace_ids_by_token.get(m))
                        .copied();
                    let Some(ws_id) = owned_ws else {
                        // Not ours (other instance) or unparseable: keep.
                        report.scopes_kept += 1;
                        continue;
                    };
                    match mission_short {
                        Some(short) => {
                            let entry = index
                                .get(short)
                                .and_then(|entries| entry_for_workspace(entries, ws_id));
                            let verdict =
                                classify_exec_scope(entry.map(|e| &e.status), index_full.complete);
                            if verdict == ScopeVerdict::Keep {
                                live_short_ids.insert(short.clone());
                            }
                            verdict
                        }
                        // Legacy / task-tagged units: the periodic reaper owns
                        // those (age-based policy); do not stop them here.
                        None => ScopeVerdict::Keep,
                    }
                }
                ParsedScope::Durable { machine, job_id } => {
                    if machine
                        .as_ref()
                        .map(|m| !workspace_ids_by_token.contains_key(m))
                        .unwrap_or(true)
                    {
                        report.scopes_kept += 1;
                        continue;
                    }
                    match job_id {
                        Some(job_id) => {
                            let status =
                                super::durable_jobs::job_status_for_reconcile(state, *job_id).await;
                            classify_durable_scope(status.as_ref())
                        }
                        None => ScopeVerdict::Keep,
                    }
                }
            };
            match verdict {
                ScopeVerdict::Stop => {
                    if stop_unit(unit, "reconcile: owner terminal or unknown").await {
                        report.scopes_stopped += 1;
                    } else {
                        report.errors += 1;
                    }
                }
                ScopeVerdict::Keep => report.scopes_kept += 1,
            }
        }
    }

    // ---- Phase 2: missions ----
    let grace = active_grace();
    let horizon = interrupted_horizon();
    // Pass-level budget: cap total re-resumes across ALL sessions so a restart
    // storm can't stampede the actor.
    let mut reresume_budget = interrupted_resume_cap();
    let now = chrono::Utc::now();
    for session in state.control.all_sessions().await {
        let store = &session.mission_store;
        let running_ids: HashSet<Uuid> = session
            .running_missions
            .read()
            .await
            .iter()
            .map(|info| info.mission_id)
            .collect();

        // 2a. Ghost-active missions → interrupted(service_restart) + resume.
        let active = match store.get_all_active_missions().await {
            Ok(missions) => missions,
            Err(err) => {
                tracing::warn!(%err, "reconcile: could not list active missions");
                report.errors += 1;
                continue;
            }
        };
        // Snapshot of live (active) project+track pairs, used by phase 2c to
        // avoid re-resuming an interrupted mission when the controller has
        // already re-dispatched a fresh worker for the same track.
        let live_project_tracks: HashSet<(String, Option<String>)> = active
            .iter()
            .filter_map(|m| {
                m.project
                    .project
                    .clone()
                    .map(|p| (p, m.project.track.clone()))
            })
            .collect();

        for mission in active {
            report.missions_scanned += 1;
            let has_durable_run = match store.get_active_mission_run(mission.id).await {
                Ok(run) => run.is_some_and(|run| {
                    run.execution_state == MissionExecutionState::WaitingRemoteJob
                }),
                Err(err) => {
                    // Execution truth unavailable: fail closed, keep.
                    tracing::warn!(mission_id = %mission.id, %err, "reconcile: run lookup failed");
                    report.errors += 1;
                    continue;
                }
            };
            let recently_updated = chrono::DateTime::parse_from_rfc3339(&mission.updated_at)
                .map(|t| now - t.with_timezone(&chrono::Utc) < grace)
                .unwrap_or(false);
            let short = mission.id.simple().to_string()[..8].to_string();
            let facts = MissionFacts {
                status: mission.status,
                is_assistant_mode: mission.mission_mode == MissionMode::Assistant,
                running_in_actor: running_ids.contains(&mission.id),
                has_live_scope: report.systemd_available && live_short_ids.contains(&short),
                has_durable_run,
                recently_updated,
                has_events: true, // irrelevant for the Active arm
                already_tagged_orphaned: false,
            };
            if classify_mission(&facts) != MissionVerdict::InterruptAndResume {
                continue;
            }
            tracing::warn!(
                mission_id = %mission.id,
                "reconcile: active mission has no live runner/scope; interrupting (service_restart) and resuming"
            );
            if let Err(err) = store
                .update_mission_status_with_reason(
                    mission.id,
                    MissionStatus::Interrupted,
                    Some(SERVICE_RESTART_REASON),
                )
                .await
            {
                tracing::warn!(mission_id = %mission.id, %err, "reconcile: interrupt failed");
                report.errors += 1;
                continue;
            }
            report.missions_interrupted += 1;
            let _ = session.events_tx.send(AgentEvent::MissionStatusChanged {
                mission_id: mission.id,
                status: MissionStatus::Interrupted,
                summary: Some(
                    "Interrupted: no live runner or scope after a service restart".to_string(),
                ),
            });
            // Auto-resume through the EXISTING resume path (same internal
            // command POST /api/control/missions/:id/resume uses).
            let (tx, rx) = oneshot::channel();
            let sent = session
                .cmd_tx
                .send(ControlCommand::ResumeMission {
                    mission_id: mission.id,
                    clean_workspace: false,
                    skip_message: false,
                    respond: tx,
                })
                .await
                .is_ok();
            match (sent, if sent { rx.await.ok() } else { None }) {
                (true, Some(Ok(_))) => report.missions_resumed += 1,
                (true, Some(Err(err))) => {
                    tracing::warn!(mission_id = %mission.id, %err, "reconcile: auto-resume failed");
                    report.errors += 1;
                }
                _ => {
                    tracing::warn!(mission_id = %mission.id, "reconcile: auto-resume not acknowledged");
                    report.errors += 1;
                }
            }
        }

        // 2b. AwaitingUser missions whose harness session state is gone.
        let recent = match store.list_missions(500, 0).await {
            Ok(missions) => missions,
            Err(err) => {
                tracing::warn!(%err, "reconcile: could not list missions for orphan scan");
                report.errors += 1;
                continue;
            }
        };
        for mission in recent
            .into_iter()
            .filter(|m| m.status == MissionStatus::AwaitingUser)
        {
            report.missions_scanned += 1;
            let has_events = store
                .max_event_sequence(mission.id)
                .await
                .map(|seq| seq > 0)
                .unwrap_or(true); // unreadable event log: fail closed, keep
            let has_durable_run = store
                .get_active_mission_run(mission.id)
                .await
                .ok()
                .flatten()
                .is_some();
            let facts = MissionFacts {
                status: mission.status,
                is_assistant_mode: mission.mission_mode == MissionMode::Assistant,
                running_in_actor: running_ids.contains(&mission.id),
                has_live_scope: false,
                has_durable_run,
                recently_updated: false,
                has_events,
                already_tagged_orphaned: mission.project.tags.iter().any(|t| t == ORPHANED_TAG),
            };
            if classify_mission(&facts) != MissionVerdict::TagOrphaned {
                continue;
            }
            let mut tags = mission.project.tags.clone();
            tags.push(ORPHANED_TAG.to_string());
            if let Err(err) = store
                .update_mission_project(
                    mission.id,
                    MissionProjectPatch {
                        tags: Some(tags),
                        ..Default::default()
                    },
                )
                .await
            {
                tracing::warn!(mission_id = %mission.id, %err, "reconcile: orphan tag failed");
                report.errors += 1;
                continue;
            }
            report.missions_tagged_orphaned += 1;
            let event = AgentEvent::MissionActivity {
                label: format!(
                    "Mission {} marked orphaned: awaiting_user but its harness session \
state (event log / live session) is gone. Replay and resume need a fresh turn; \
send a new message or re-create the mission.",
                    mission.id
                ),
                tool_name: "boot_reconcile".to_string(),
                mission_id: Some(mission.id),
            };
            let _ = store.log_event(mission.id, &event).await;
            let _ = session.events_tx.send(event);
            tracing::warn!(mission_id = %mission.id, "reconcile: tagged awaiting_user mission as orphaned");
        }

        // 2c. Missions ALREADY `interrupted(service_restart)` from a prior
        // restart, with no live backing, recently touched, and no competing
        // live retry → re-resume (bounded by `reresume_budget`). Without this,
        // a restart that interrupts a mission followed by a second restart
        // before the auto-resume lands strands it `interrupted` forever.
        if reresume_budget == 0 {
            continue;
        }
        let interrupted = match store
            .list_missions_filtered(
                &MissionFilter {
                    status: Some(MissionStatus::Interrupted.to_string()),
                    ..Default::default()
                },
                500,
                0,
            )
            .await
        {
            Ok(missions) => missions,
            Err(err) => {
                tracing::warn!(%err, "reconcile: could not list interrupted missions");
                report.errors += 1;
                continue;
            }
        };
        for mission in interrupted {
            if reresume_budget == 0 {
                break;
            }
            report.missions_scanned += 1;
            // (a) ONLY restart/deploy interrupts — never user-cancelled
            // or genuinely-failed missions. `force_killed_after_cancel_timeout`
            // qualifies only while the deploy marker is still fresh: that
            // is the SIGTERM drain race that used to strand writers.
            let is_service_restart =
                is_recoverable_restart_reason(mission.terminal_reason.as_deref())
                    || is_recent_deploy_force_kill(
                        mission.terminal_reason.as_deref(),
                        super::system::last_deploy_age_secs(),
                    );
            if !is_service_restart {
                continue;
            }
            let has_durable_run = match store.get_active_mission_run(mission.id).await {
                Ok(run) => run.is_some_and(|run| {
                    run.execution_state == MissionExecutionState::WaitingRemoteJob
                }),
                Err(err) => {
                    tracing::warn!(mission_id = %mission.id, %err, "reconcile: run lookup failed (interrupted)");
                    report.errors += 1;
                    continue;
                }
            };
            let within_horizon = chrono::DateTime::parse_from_rfc3339(&mission.updated_at)
                .map(|t| now - t.with_timezone(&chrono::Utc) < horizon)
                .unwrap_or(false);
            let short = mission.id.simple().to_string()[..8].to_string();
            // (b) A live mission with the same project+track means the
            // controller already re-dispatched; don't duplicate it.
            let live_retry_exists = mission
                .project
                .project
                .as_ref()
                .map(|p| live_project_tracks.contains(&(p.clone(), mission.project.track.clone())))
                .unwrap_or(false);
            let facts = InterruptedFacts {
                status: mission.status,
                is_service_restart,
                running_in_actor: running_ids.contains(&mission.id),
                has_live_scope: report.systemd_available && live_short_ids.contains(&short),
                has_durable_run,
                within_horizon,
                live_retry_exists,
            };
            if !should_reresume_interrupted(&facts) {
                continue;
            }
            tracing::warn!(
                mission_id = %mission.id,
                project = ?mission.project.project,
                track = ?mission.project.track,
                "reconcile: re-resuming mission stranded interrupted(service_restart) by a prior restart"
            );
            let (tx, rx) = oneshot::channel();
            let sent = session
                .cmd_tx
                .send(ControlCommand::ResumeMission {
                    mission_id: mission.id,
                    clean_workspace: false,
                    skip_message: false,
                    respond: tx,
                })
                .await
                .is_ok();
            match (sent, if sent { rx.await.ok() } else { None }) {
                (true, Some(Ok(_))) => {
                    report.missions_reresumed += 1;
                    reresume_budget -= 1;
                }
                (true, Some(Err(err))) => {
                    tracing::warn!(mission_id = %mission.id, %err, "reconcile: interrupted re-resume failed");
                    report.errors += 1;
                }
                _ => {
                    tracing::warn!(mission_id = %mission.id, "reconcile: interrupted re-resume not acknowledged");
                    report.errors += 1;
                }
            }
        }
    }

    report
}

/// Spawn the boot-time reconcile pass. Call once after AppState is built.
pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        if !boot_reconcile_enabled() {
            tracing::info!("boot reconcile disabled (BOOT_RECONCILE_ENABLED=0)");
            return;
        }
        tokio::time::sleep(boot_delay()).await;
        let report = run(&state).await;
        tracing::info!(
            systemd = report.systemd_available,
            scopes_scanned = report.scopes_scanned,
            scopes_stopped = report.scopes_stopped,
            missions_interrupted = report.missions_interrupted,
            missions_resumed = report.missions_resumed,
            missions_reresumed = report.missions_reresumed,
            orphaned = report.missions_tagged_orphaned,
            errors = report.errors,
            "boot reconcile finished"
        );
    });
}

/// `POST /api/system/reconcile` — run a full pass on demand.
pub async fn reconcile_endpoint(State(state): State<Arc<AppState>>) -> Json<ReconcileReport> {
    Json(run(&state).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::durable_jobs::DurableJobStatus;

    // ---------- scope-name parsing ----------

    #[test]
    fn parses_mission_tagged_exec_scope() {
        let unit = "sandboxed-exec-sandboxed-dumbcontracts-634e6d35-m4efda364-0a1b2c3d.scope";
        assert_eq!(
            parse_scope_unit(unit),
            Some(ParsedScope::Exec {
                machine: Some("sandboxed-dumbcontracts-634e6d35".to_string()),
                mission_short: Some("4efda364".to_string()),
            })
        );
    }

    #[test]
    fn parses_full_uuid_tagged_exec_scope_to_short_id() {
        let unit =
            "sandboxed-exec-sandboxed-ws-deadbeef-m4efda364aabbccddeeff001122334455-0a1b2c3d.scope";
        match parse_scope_unit(unit) {
            Some(ParsedScope::Exec { mission_short, .. }) => {
                assert_eq!(mission_short.as_deref(), Some("4efda364"));
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn parses_legacy_exec_scope_without_mission_tag() {
        let unit = "sandboxed-exec-sandboxed-misc-deadbeef-0a1b2c3d.scope";
        assert_eq!(
            parse_scope_unit(unit),
            Some(ParsedScope::Exec {
                machine: Some("sandboxed-misc-deadbeef".to_string()),
                mission_short: None,
            })
        );
    }

    #[test]
    fn parses_durable_scope() {
        let job = Uuid::new_v4();
        let unit = format!(
            "sandboxed-durable-sandboxed-dumbcontracts-634e6d35-{}.scope",
            job.simple()
        );
        assert_eq!(
            parse_scope_unit(&unit),
            Some(ParsedScope::Durable {
                machine: Some("sandboxed-dumbcontracts-634e6d35".to_string()),
                job_id: Some(job),
            })
        );
    }

    #[test]
    fn skips_non_sandboxed_scopes_and_non_scopes() {
        assert_eq!(parse_scope_unit("session-1.scope"), None);
        assert_eq!(parse_scope_unit("sandboxed-mission-x.scope"), None);
        assert_eq!(
            parse_scope_unit("sandboxed-exec-foo-0a1b2c3d.service"),
            None
        );
        assert_eq!(parse_scope_unit("cron.service"), None);
    }

    #[test]
    fn malformed_durable_scope_parses_with_none_job_id() {
        assert_eq!(
            parse_scope_unit("sandboxed-durable-notahex.scope"),
            Some(ParsedScope::Durable {
                machine: None,
                job_id: None
            })
        );
    }

    // ---------- exec scope classification ----------

    #[test]
    fn exec_scope_kept_for_live_statuses() {
        for status in [
            MissionStatus::Active,
            MissionStatus::Pending,
            MissionStatus::WaitingBackground,
            MissionStatus::AwaitingUser,
            MissionStatus::Paused,
        ] {
            assert_eq!(
                classify_exec_scope(Some(&status), true),
                ScopeVerdict::Keep,
                "{status} scope must be kept"
            );
        }
    }

    #[test]
    fn exec_scope_stopped_for_terminal_and_acknowledged() {
        for status in [
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Interrupted,
            MissionStatus::Blocked,
            MissionStatus::NotFeasible,
            MissionStatus::Acknowledged,
        ] {
            assert_eq!(
                classify_exec_scope(Some(&status), true),
                ScopeVerdict::Stop,
                "{status} scope must be stopped"
            );
        }
    }

    #[test]
    fn unknown_mission_scope_stops_only_with_complete_index() {
        assert_eq!(classify_exec_scope(None, true), ScopeVerdict::Stop);
        assert_eq!(classify_exec_scope(None, false), ScopeVerdict::Keep);
    }

    // ---------- durable scope classification ----------

    #[test]
    fn durable_scope_kept_only_while_running() {
        assert_eq!(
            classify_durable_scope(Some(&DurableJobStatus::Running)),
            ScopeVerdict::Keep
        );
        for status in [
            DurableJobStatus::Completed,
            DurableJobStatus::Failed,
            DurableJobStatus::Cancelled,
            DurableJobStatus::Unknown,
        ] {
            assert_eq!(classify_durable_scope(Some(&status)), ScopeVerdict::Stop);
        }
        assert_eq!(classify_durable_scope(None), ScopeVerdict::Stop);
    }

    // ---------- mission classification ----------

    fn base_active_facts() -> MissionFacts {
        MissionFacts {
            status: MissionStatus::Active,
            is_assistant_mode: false,
            running_in_actor: false,
            has_live_scope: false,
            has_durable_run: false,
            recently_updated: false,
            has_events: true,
            already_tagged_orphaned: false,
        }
    }

    #[test]
    fn ghost_active_mission_is_interrupted_and_resumed() {
        assert_eq!(
            classify_mission(&base_active_facts()),
            MissionVerdict::InterruptAndResume
        );
    }

    #[test]
    fn active_mission_with_any_liveness_signal_is_kept() {
        for mutate in [
            |f: &mut MissionFacts| f.running_in_actor = true,
            |f: &mut MissionFacts| f.has_live_scope = true,
            |f: &mut MissionFacts| f.has_durable_run = true,
            |f: &mut MissionFacts| f.recently_updated = true,
            |f: &mut MissionFacts| f.is_assistant_mode = true,
        ] {
            let mut facts = base_active_facts();
            mutate(&mut facts);
            assert_eq!(classify_mission(&facts), MissionVerdict::Keep);
        }
    }

    #[test]
    fn awaiting_user_without_session_state_is_tagged_orphaned_once() {
        let mut facts = base_active_facts();
        facts.status = MissionStatus::AwaitingUser;
        facts.has_events = false;
        assert_eq!(classify_mission(&facts), MissionVerdict::TagOrphaned);
        facts.already_tagged_orphaned = true;
        assert_eq!(classify_mission(&facts), MissionVerdict::Keep);
    }

    #[test]
    fn awaiting_user_with_events_is_kept() {
        let mut facts = base_active_facts();
        facts.status = MissionStatus::AwaitingUser;
        facts.has_events = true;
        assert_eq!(classify_mission(&facts), MissionVerdict::Keep);
    }

    #[test]
    fn pending_and_terminal_missions_are_never_touched() {
        for status in [
            MissionStatus::Pending,
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Interrupted,
            MissionStatus::Paused,
            MissionStatus::WaitingBackground,
            MissionStatus::Acknowledged,
        ] {
            let mut facts = base_active_facts();
            facts.status = status;
            facts.has_events = false;
            assert_eq!(
                classify_mission(&facts),
                MissionVerdict::Keep,
                "{status} must be kept"
            );
        }
    }

    // ---------- interrupted(service_restart) re-resume ----------

    fn base_interrupted_facts() -> InterruptedFacts {
        InterruptedFacts {
            status: MissionStatus::Interrupted,
            is_service_restart: true,
            running_in_actor: false,
            has_live_scope: false,
            has_durable_run: false,
            within_horizon: true,
            live_retry_exists: false,
        }
    }

    #[test]
    fn interrupted_service_restart_no_scope_recent_is_reresumed() {
        assert!(should_reresume_interrupted(&base_interrupted_facts()));
    }

    #[test]
    fn server_shutdown_and_service_restart_are_recoverable() {
        assert!(is_recoverable_restart_reason(Some(SERVICE_RESTART_REASON)));
        assert!(is_recoverable_restart_reason(Some(SERVER_SHUTDOWN_REASON)));
        assert!(!is_recoverable_restart_reason(Some(
            FORCE_KILLED_CANCEL_TIMEOUT_REASON
        )));
        assert!(!is_recoverable_restart_reason(Some("cancelled")));
        assert!(!is_recoverable_restart_reason(None));
    }

    #[test]
    fn force_kill_is_recoverable_only_inside_the_deploy_window() {
        assert!(is_recent_deploy_force_kill(
            Some(FORCE_KILLED_CANCEL_TIMEOUT_REASON),
            Some(60)
        ));
        assert!(!is_recent_deploy_force_kill(
            Some(FORCE_KILLED_CANCEL_TIMEOUT_REASON),
            Some(31 * 60)
        ));
        assert!(!is_recent_deploy_force_kill(
            Some(FORCE_KILLED_CANCEL_TIMEOUT_REASON),
            None
        ));
        assert!(!is_recent_deploy_force_kill(Some("cancelled"), Some(10)));
    }

    #[test]
    fn interrupted_user_cancel_is_not_reresumed() {
        // Only restart/deploy interrupts qualify — a user-cancelled or
        // genuinely failed mission has `is_service_restart == false`.
        let mut facts = base_interrupted_facts();
        facts.is_service_restart = false;
        assert!(!should_reresume_interrupted(&facts));
    }

    #[test]
    fn interrupted_service_restart_too_old_is_not_reresumed() {
        let mut facts = base_interrupted_facts();
        facts.within_horizon = false;
        assert!(!should_reresume_interrupted(&facts));
    }

    #[test]
    fn interrupted_with_live_retry_is_not_reresumed() {
        let mut facts = base_interrupted_facts();
        facts.live_retry_exists = true;
        assert!(!should_reresume_interrupted(&facts));
    }

    #[test]
    fn interrupted_with_any_live_backing_is_not_reresumed() {
        for mutate in [
            |f: &mut InterruptedFacts| f.running_in_actor = true,
            |f: &mut InterruptedFacts| f.has_live_scope = true,
            |f: &mut InterruptedFacts| f.has_durable_run = true,
        ] {
            let mut facts = base_interrupted_facts();
            mutate(&mut facts);
            assert!(!should_reresume_interrupted(&facts));
        }
    }

    #[test]
    fn non_interrupted_status_is_never_reresumed() {
        for status in [
            MissionStatus::Active,
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::AwaitingUser,
            MissionStatus::Acknowledged,
        ] {
            let mut facts = base_interrupted_facts();
            facts.status = status;
            assert!(
                !should_reresume_interrupted(&facts),
                "{status} must not be re-resumed"
            );
        }
    }

    #[test]
    fn reresume_cap_is_respected() {
        // The per-pass budget starts at the cap and decrements per resume;
        // once exhausted, no further missions are resumed. Model the loop's
        // budget arithmetic directly against the pure predicate.
        let cap = 3usize;
        let mut budget = cap;
        let candidates = vec![base_interrupted_facts(); 10];
        let mut resumed = 0usize;
        for facts in &candidates {
            if budget == 0 {
                break;
            }
            if should_reresume_interrupted(facts) {
                resumed += 1;
                budget -= 1;
            }
        }
        assert_eq!(resumed, cap);
        assert_eq!(budget, 0);
    }
}
