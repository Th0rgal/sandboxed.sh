//! Mission supervision: recovery, watchdogs, and stale-mission cleanup.
//!
//! Moved verbatim from `control.rs` (Phase 5 of the decomposition). The three
//! overlapping liveness mechanisms now live side by side:
//!
//! - [`recover_server_shutdown_missions`] — boot-time recovery of missions a
//!   previous process left active/interrupted.
//! - [`stuck_mission_watchdog_loop`] + [`ack_promotion_loop`] — in-process
//!   detection of silent/orphaned runners (incl. OOM-kill reporting).
//! - [`stale_mission_cleanup_loop`] — hour-scale cleanup of abandoned
//!   missions.
//! - [`background_task_autoresume_loop`] — wakes missions parked in
//!   `AwaitingUser` when Claude Code background shell tasks finish.
//!
//! TODO(Phase 5b): replace their three independent "is this mission alive?"
//! heuristics with one per-mission LivenessState fed by the event stream —
//! the dual notions of "stalled" are what produced past watchdog
//! false-positives.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::workspace;
use uuid::Uuid;

use super::control::MissionStatus;
#[allow(unused_imports)]
use super::control::*;
use super::mission_store::MissionStore;

mod bg_autoresume;

pub(crate) use bg_autoresume::background_task_autoresume_loop;

pub(crate) async fn recover_server_shutdown_missions(
    mission_store: Arc<dyn MissionStore>,
    events_tx: broadcast::Sender<AgentEvent>,
    cmd_tx: mpsc::Sender<ControlCommand>,
) {
    let mut to_resume = Vec::new();
    let mut seen = HashSet::new();

    match mission_store.get_all_active_missions().await {
        Ok(active_missions) => {
            for mission in active_missions {
                if mission.mission_mode == super::mission_store::MissionMode::Assistant {
                    tracing::debug!(
                        mission_id = %mission.id,
                        "Startup recovery: leaving assistant-mode active mission idle"
                    );
                    continue;
                }

                tracing::warn!(
                    mission_id = %mission.id,
                    title = %mission.title.as_deref().unwrap_or("Untitled"),
                    updated_at = %mission.updated_at,
                    "Startup recovery: active task mission survived restart; marking server_shutdown and auto-resuming"
                );
                if let Err(e) = mission_store
                    .update_mission_status_with_reason(
                        mission.id,
                        MissionStatus::Interrupted,
                        Some("server_shutdown"),
                    )
                    .await
                {
                    tracing::warn!(
                        mission_id = %mission.id,
                        "Startup recovery: failed to mark active mission interrupted: {}",
                        e
                    );
                    continue;
                }

                maybe_schedule_mission_metadata_refresh_for_status(
                    &mission_store,
                    &events_tx,
                    mission.id,
                    MissionStatus::Interrupted,
                );
                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                    mission_id: mission.id,
                    status: MissionStatus::Interrupted,
                    summary: Some(
                        "Interrupted: server restarted while mission was active".to_string(),
                    ),
                });

                if seen.insert(mission.id) {
                    to_resume.push(mission.id);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Startup recovery: failed to check for active missions: {}",
                e
            );
        }
    }

    match mission_store
        .get_recent_server_shutdown_mission_ids(SERVER_SHUTDOWN_AUTO_RESUME_MAX_AGE_HOURS)
        .await
    {
        Ok(mission_ids) => {
            for mission_id in mission_ids {
                if seen.insert(mission_id) {
                    to_resume.push(mission_id);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Startup recovery: failed to check for server-shutdown missions: {}",
                e
            );
        }
    }

    if to_resume.is_empty() {
        tracing::debug!("Startup recovery: no server-shutdown missions to auto-resume");
        return;
    }

    tracing::warn!(
        count = to_resume.len(),
        "Startup recovery: auto-resuming server-shutdown mission(s)"
    );

    for mission_id in to_resume {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = cmd_tx
            .send(ControlCommand::ResumeMission {
                mission_id,
                clean_workspace: false,
                skip_message: false,
                respond: tx,
            })
            .await
        {
            tracing::warn!(
                mission_id = %mission_id,
                "Startup recovery: failed to enqueue auto-resume: {}",
                e
            );
            continue;
        }

        match rx.await {
            Ok(Ok(_)) => {
                tracing::info!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume queued"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume failed: {}",
                    e
                );
            }
            Err(e) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume response dropped: {}",
                    e
                );
            }
        }
    }
}

/// Apply the stale-mission safety net once.
///
/// We intentionally do not infer "orphaned" from `MissionStatus::Active` alone here.
/// Missions remain `active` between turns while waiting for the next user message or
/// queued automation, so the periodic cleanup task cannot safely treat "not currently
/// running" as an interruption without spuriously flipping healthy Claude missions to
/// `interrupted`.
pub(crate) async fn cleanup_stale_active_missions_once(
    mission_store: &Arc<dyn MissionStore>,
    stale_hours: u64,
    events_tx: &broadcast::Sender<AgentEvent>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
) {
    match mission_store.get_stale_active_missions(stale_hours).await {
        Ok(stale_missions) => {
            for mission in stale_missions {
                tracing::info!(
                    "Auto-closing stale mission {}: '{}' (inactive since {})",
                    mission.id,
                    mission.title.as_deref().unwrap_or("Untitled"),
                    mission.updated_at
                );

                // Ask the control actor to cancel any in-memory runner
                // for this mission before we overwrite DB status. Without
                // this, a frozen runner (e.g. stuck in `child.wait()` on
                // an orphaned tool subprocess) would keep
                // `running_mission_id` pinned and /api/control/running
                // would keep reporting the mission as "running, stalled"
                // until the daemon restarts. CancelMission is idempotent
                // — it returns "not found" when there is no live runner,
                // which is the common case for stale missions, and we
                // ignore that error.
                let (tx, rx) = oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::CancelMission {
                        mission_id: mission.id,
                        min_idle: Some(std::time::Duration::from_secs(STUCK_SECONDS)),
                        respond: tx,
                    })
                    .await
                    .is_ok()
                {
                    let _ = rx.await;
                }

                if let Err(e) = mission_store
                    .update_mission_status(mission.id, MissionStatus::Completed)
                    .await
                {
                    tracing::warn!("Failed to auto-close stale mission {}: {}", mission.id, e);
                } else {
                    maybe_schedule_mission_metadata_refresh_for_status(
                        mission_store,
                        events_tx,
                        mission.id,
                        MissionStatus::Completed,
                    );
                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                        mission_id: mission.id,
                        status: MissionStatus::Completed,
                        summary: Some(format!(
                            "Auto-closed after {} hours of inactivity",
                            stale_hours
                        )),
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to check for stale missions: {}", e);
        }
    }
}

/// Background task that periodically cleans up stale missions.
/// Periodic watchdog: marks missions interrupted when the runner has
/// stalled for too long, even if the mission row is still `Active`.
///
/// Two cases this catches that the boot-time orphan recovery and the
/// daily stale-mission cleanup miss:
/// 1. mission_runner task died mid-flight (e.g. codex stdio EOF after
///    one of our reconnect attempts). The mission row stays Active
///    forever because nothing emits a terminal status; the codex
///    process can survive in its container namespace.
/// 2. codex itself hung — process alive but `futex_wait_queue` with no
///    events. Observed live on prod after a deploy mid-mission: 70+
///    minutes of silence, dashboard correctly flagged "may be stuck"
///    but no path was forcing termination.
///
/// Threshold is intentionally generous (15 min) so a model in the
/// middle of a slow API turn or a long shell command isn't false-killed.
/// Periodic ack-promotion: scans `AwaitingUser` missions whose
/// `first_viewed_at` is older than `ACK_GRACE_SECONDS` and flips them to
/// `Acknowledged`. Broadcasts `MissionStatusChanged` so dashboard/iOS clients
/// move the row from "Needs You" to "Finished" without a refresh.
pub(crate) async fn ack_promotion_loop(
    mission_store: Arc<dyn MissionStore>,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    tracing::info!(
        "Ack-promotion loop started: grace {}s, tick {}s",
        ACK_GRACE_SECONDS,
        ACK_PROMOTION_TICK_INTERVAL.as_secs()
    );
    loop {
        tokio::time::sleep(ACK_PROMOTION_TICK_INTERVAL).await;
        match mission_store
            .acknowledge_stale_awaiting_user_missions(ACK_GRACE_SECONDS)
            .await
        {
            Ok(promoted) => {
                for mission_id in promoted {
                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                        mission_id,
                        status: MissionStatus::Acknowledged,
                        summary: None,
                    });
                }
            }
            Err(e) => {
                tracing::warn!("Ack-promotion tick failed: {}", e);
            }
        }
    }
}

pub(crate) async fn stuck_mission_watchdog_loop(
    mission_store: Arc<dyn MissionStore>,
    cmd_tx: mpsc::Sender<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    workspaces: workspace::SharedWorkspaceStore,
) {
    use std::collections::HashSet;

    const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

    tracing::info!(
        "Stuck-mission watchdog started: threshold {}s, poll every {}s",
        STUCK_SECONDS,
        CHECK_INTERVAL.as_secs()
    );

    // Last seen `oom_kill` counter per scope unit; an increase means the
    // kernel killed something inside a mission's memory cgroup since the
    // previous tick. Entries for dead scopes are pruned each pass.
    let mut oom_seen: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    // Worker missions we have already auto-resumed once this process lifetime.
    // Auto-resume is a single supervised retry for workers whose runner died
    // (orphan / deploy SIGTERM) — an environmental interruption, not the
    // worker's own fault. If the resumed worker dies again it stays
    // interrupted and the boss handles it, so this can never resume-storm.
    let mut auto_resumed_workers: HashSet<Uuid> = HashSet::new();

    loop {
        tokio::time::sleep(CHECK_INTERVAL).await;

        // Pull the in-memory running list from the actor — same source
        // /api/control/running serves, includes seconds_since_activity.
        let (resp_tx, resp_rx) = oneshot::channel();
        if cmd_tx
            .send(ControlCommand::ListRunning { respond: resp_tx })
            .await
            .is_err()
        {
            tracing::debug!("Stuck-mission watchdog: actor channel closed; exiting");
            return;
        }
        let running_list = match resp_rx.await {
            Ok(list) => list,
            Err(_) => continue,
        };

        // Cross-check against DB: any mission Active in the store but
        // not in `running_list` is an orphan from a runner death.
        let active_missions = match mission_store.get_all_active_missions().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Stuck-mission watchdog: list active failed: {}", e);
                continue;
            }
        };

        let running_ids: HashSet<Uuid> = running_list.iter().map(|info| info.mission_id).collect();

        // OOM surveillance: surface kernel `oom_kill` events from mission
        // memory cgroups as mission activity. Without this, a build killed
        // by its memory cap looks like a silent tool failure and agents
        // retry it in a loop instead of adapting (lower parallelism) or
        // requesting a cap boost.
        check_mission_oom_kills(
            &workspaces,
            &active_missions,
            &running_ids,
            &mut oom_seen,
            &events_tx,
        )
        .await;

        // Case 1 — actor reports the mission running but stalled past
        // threshold. Cancel via the actor (clean shutdown) and mark
        // the row Interrupted.
        for info in &running_list {
            if info.seconds_since_activity >= STUCK_SECONDS {
                // A mission parked on a frontend tool (e.g. AskUserQuestion) is
                // intentionally silent: its harness is killed while it awaits a
                // human answer, so it emits no activity. Do not count that as a
                // stall — humans routinely take longer than the threshold to
                // reply. The wait is cleared the moment the answer arrives (or
                // the mission is cancelled), so this can't pin a dead mission.
                if tool_hub.is_waiting_for_input(info.mission_id) {
                    tracing::debug!(
                        mission_id = %info.mission_id,
                        seconds_since_activity = info.seconds_since_activity,
                        "Stuck-mission watchdog: skipping mission blocked on user input"
                    );
                    continue;
                }
                tracing::warn!(
                    "Stuck-mission watchdog: cancelling {} after {}s of inactivity",
                    info.mission_id,
                    info.seconds_since_activity
                );
                let (cancel_tx, cancel_rx) = oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::CancelMission {
                        mission_id: info.mission_id,
                        min_idle: Some(std::time::Duration::from_secs(STUCK_SECONDS)),
                        respond: cancel_tx,
                    })
                    .await
                    .is_ok()
                {
                    let _ = cancel_rx.await;
                }
                if let Err(e) = mission_store
                    .update_mission_status_with_reason(
                        info.mission_id,
                        MissionStatus::Interrupted,
                        Some("watchdog_stalled"),
                    )
                    .await
                {
                    tracing::warn!(
                        "Stuck-mission watchdog: status update failed for {}: {}",
                        info.mission_id,
                        e
                    );
                    continue;
                }
                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                    mission_id: info.mission_id,
                    status: MissionStatus::Interrupted,
                    summary: Some(format!(
                        "Interrupted: no agent activity for {}s (>{}s threshold)",
                        info.seconds_since_activity, STUCK_SECONDS
                    )),
                });
            }
        }

        // Case 2 — Active in DB, not in actor's running list at all.
        // This is the "mission_runner died, row never finalized" path.
        for mission in &active_missions {
            if running_ids.contains(&mission.id) {
                continue;
            }
            if mission.mission_mode == super::mission_store::MissionMode::Assistant {
                tracing::debug!(
                    mission_id = %mission.id,
                    "Stuck-mission watchdog: leaving idle assistant-mode mission active"
                );
                continue;
            }
            tracing::warn!(
                "Stuck-mission watchdog: orphan {} (no live runner); marking interrupted",
                mission.id
            );
            if let Err(e) = mission_store
                .update_mission_status_with_reason(
                    mission.id,
                    MissionStatus::Interrupted,
                    Some("orphan_no_runner"),
                )
                .await
            {
                tracing::warn!(
                    "Stuck-mission watchdog: status update failed for {}: {}",
                    mission.id,
                    e
                );
                continue;
            }
            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                mission_id: mission.id,
                status: MissionStatus::Interrupted,
                summary: Some(
                    "Interrupted: mission runner exited without reporting a terminal status"
                        .to_string(),
                ),
            });

            // One supervised auto-resume for orphaned WORKER missions. The
            // boss used to babysit this by hand (10 manual resume_worker
            // calls in one campaign); a runner death is environmental, so a
            // single retry is safe. Once-only per process: a worker that dies
            // again stays interrupted for the boss to triage.
            if mission.parent_mission_id.is_some() && auto_resumed_workers.insert(mission.id) {
                tracing::info!(
                    mission_id = %mission.id,
                    parent = ?mission.parent_mission_id,
                    "Stuck-mission watchdog: auto-resuming orphaned worker once"
                );
                let (resume_tx, resume_rx) = oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::ResumeMission {
                        mission_id: mission.id,
                        clean_workspace: false,
                        skip_message: false,
                        respond: resume_tx,
                    })
                    .await
                    .is_ok()
                {
                    match resume_rx.await {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => tracing::warn!(
                            mission_id = %mission.id,
                            "Auto-resume failed: {}; leaving interrupted for the boss", e
                        ),
                        Err(_) => {}
                    }
                }
            }
        }
    }
}

/// Read the kernel `oom_kill` counter from a scope unit's `memory.events`.
/// Returns `None` when the unit/cgroup is gone or unreadable.
pub(crate) async fn read_scope_oom_kills(unit: &str) -> Option<u64> {
    let output = tokio::process::Command::new("systemctl")
        .args(["show", unit, "-p", "ControlGroup", "--value"])
        .output()
        .await
        .ok()?;
    let cgroup = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cgroup.is_empty() {
        return None;
    }
    let path = format!("/sys/fs/cgroup{cgroup}/memory.events");
    let content = tokio::fs::read_to_string(path).await.ok()?;
    content.lines().find_map(|line| {
        line.strip_prefix("oom_kill ")
            .and_then(|v| v.trim().parse::<u64>().ok())
    })
}

/// Detect `oom_kill` increases in running missions' memory cgroups and
/// surface them on the mission event stream. One pass per watchdog tick.
pub(crate) async fn check_mission_oom_kills(
    workspaces: &workspace::SharedWorkspaceStore,
    active_missions: &[crate::api::mission_store::Mission],
    running_ids: &std::collections::HashSet<Uuid>,
    oom_seen: &mut std::collections::HashMap<String, u64>,
    events_tx: &broadcast::Sender<AgentEvent>,
) {
    let mut live_units: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Whether we got a complete picture of live scopes this tick. A failed
    // workspace lookup or `list-units` means some scopes couldn't be
    // enumerated, so we must not prune `oom_seen` (see the retain below).
    let mut enumeration_complete = true;

    // Scopes are workspace-level and shared by every mission running in the
    // same container, while `oom_seen` is keyed by unit. Group missions by
    // workspace so each unit's OOM delta is consumed once per tick and the
    // alert fans out to *all* missions on that workspace — otherwise the
    // first mission would absorb the delta and its siblings would never see
    // the OOM signal.
    let mut missions_by_workspace: std::collections::HashMap<Uuid, Vec<Uuid>> =
        std::collections::HashMap::new();
    for mission in active_missions
        .iter()
        .filter(|m| running_ids.contains(&m.id))
    {
        missions_by_workspace
            .entry(mission.workspace_id)
            .or_default()
            .push(mission.id);
    }

    for (workspace_id, mission_ids) in &missions_by_workspace {
        let Some(workspace) = workspaces.get(*workspace_id).await else {
            enumeration_complete = false;
            continue;
        };
        let units = match crate::api::workspaces::list_workspace_scope_units(&workspace).await {
            Ok(units) => units,
            Err(e) => {
                tracing::warn!(
                    "OOM watchdog: could not list scopes for {}: {}",
                    workspace.name,
                    e
                );
                enumeration_complete = false;
                continue;
            }
        };
        for unit in units {
            // The unit was listed, so it's alive: record it now so a transient
            // `memory.events` read failure below doesn't drop its baseline and
            // cause the cumulative oom_kill total to be re-reported as new.
            live_units.insert(unit.clone());
            let Some(count) = read_scope_oom_kills(&unit).await else {
                continue;
            };
            // Treat a never-seen scope as a baseline of 0 so the first kernel
            // OOM in a freshly-discovered cgroup is reported rather than
            // silently absorbed into the baseline (e.g. when the watchdog
            // starts after a scope already accumulated kills).
            let prev = oom_seen.get(&unit).copied().unwrap_or(0);
            if count > prev {
                let killed = count - prev;
                tracing::warn!(
                    "Memory watchdog: {} OOM kill(s) in {} (workspace {}, {} mission(s))",
                    killed,
                    unit,
                    workspace.name,
                    mission_ids.len()
                );
                for mission_id in mission_ids {
                    let _ = events_tx.send(AgentEvent::MissionActivity {
                        label: format!(
                            "⚠ Memory limit hit: kernel OOM-killed {killed} process(es) in this \
                             mission's cgroup. Builds should lower parallelism, or raise the \
                             workspace memory cap (Resources panel / MISSION_MEMORY_MAX)."
                        ),
                        tool_name: "memory_watchdog".to_string(),
                        mission_id: Some(*mission_id),
                    });
                }
            }
            oom_seen.insert(unit, count);
        }
    }

    // Drop counters for scopes that no longer exist so the map can't grow
    // unboundedly across weeks of uptime — but only when we fully enumerated
    // live scopes this tick. If any workspace failed to enumerate, pruning
    // would drop a still-live scope's baseline and re-emit its cumulative
    // oom_kill total as new kills on the next successful read. Skip pruning
    // for this tick; it self-heals on the next clean pass.
    if enumeration_complete {
        oom_seen.retain(|unit, _| live_units.contains(unit));
    }
}

pub(crate) async fn stale_mission_cleanup_loop(
    mission_store: Arc<dyn MissionStore>,
    stale_hours: u64,
    cmd_tx: mpsc::Sender<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    // Check every 5 minutes; the stale timeout remains a safety net for missions that
    // never receive an explicit terminal status.
    let check_interval = std::time::Duration::from_secs(300);

    tracing::info!(
        "Mission cleanup task started: stale timeout {} hours",
        stale_hours
    );

    loop {
        tokio::time::sleep(check_interval).await;
        cleanup_stale_active_missions_once(&mission_store, stale_hours, &events_tx, &cmd_tx).await;
    }
}
