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
use super::mission_runner::TOOL_CALL_STALL_GRACE_SECS;
use super::mission_store::{MissionExecutionState, MissionFilter, MissionRun, MissionStore};

mod bg_autoresume;

pub(crate) use bg_autoresume::{background_task_autoresume_loop, reset_waiting_background_on_boot};

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

                match mission_has_detached_durable_run(mission_store.as_ref(), mission.id).await {
                    Ok(true) => {
                        tracing::info!(
                            mission_id = %mission.id,
                            "Startup recovery: durable detached execution will be reattached"
                        );
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        // Execution truth is authoritative. Defer instead of
                        // overwriting it with a server-shutdown presentation
                        // status while the store is temporarily unavailable.
                        tracing::warn!(
                            mission_id = %mission.id,
                            %error,
                            "Startup recovery: could not inspect active run; deferring recovery"
                        );
                        continue;
                    }
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
                // Same ownership rule as the 15-minute watchdog (#840) and
                // registered-liveness interrupt (#841). Grok 08306fdb / Kimi
                // c91618dc / cert 203a49d5 were still in a long turn (CPU
                // 11–14%) when this 2-hour net marked them Completed with
                // "Auto-closed after 2 hours of inactivity" — a false
                // terminal that resume() then refuses.
                if watchdog_skips_control_owned_mission(
                    mission.origin.as_deref(),
                    mission.origin_session_id.as_deref(),
                    &mission.project.tags,
                ) {
                    tracing::warn!(
                        mission_id = %mission.id,
                        "Stale cleanup: control-owned mission idle — NOT auto-closing"
                    );
                    continue;
                }
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
                let mut cancellation_skipped = false;
                if cmd_tx
                    .send(ControlCommand::CancelMission {
                        mission_id: mission.id,
                        min_idle: Some(std::time::Duration::from_secs(stuck_seconds())),
                        respond: tx,
                    })
                    .await
                    .is_ok()
                {
                    cancellation_skipped = matches!(
                        rx.await,
                        Ok(Ok(CancelMissionOutcome::SkippedRecentlyActive))
                    );
                }
                if cancellation_skipped {
                    tracing::info!(
                        mission_id = %mission.id,
                        "Stale cleanup: mission resumed activity; leaving it active"
                    );
                    continue;
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

fn inactivity_is_cancellable(seconds_since_activity: u64, tool_call_in_flight: bool) -> bool {
    seconds_since_activity >= stuck_seconds()
        && (!tool_call_in_flight || seconds_since_activity >= TOOL_CALL_STALL_GRACE_SECS)
}

/// Control-owned writers must not be auto-killed for a quiet Lean/Grok turn.
/// The stored `origin` field is sometimes missing even when the MCP tagged
/// the mission `origin:hermes-assistant` — Verity's overnight flap
/// (cancel every ~15m) was that hole.
pub(crate) fn watchdog_skips_control_owned_mission(
    origin: Option<&str>,
    origin_session_id: Option<&str>,
    tags: &[String],
) -> bool {
    if origin == Some("hermes") && origin_session_id.is_some() {
        return true;
    }
    tags.iter()
        .any(|tag| tag == "origin:hermes-assistant" || tag == "pr-writer" || tag == "origin:hermes")
}

fn execution_state_proves_durable_liveness(state: &str) -> bool {
    state == "waiting_remote_job"
}

fn detached_run_proves_durable_liveness(state: MissionExecutionState) -> bool {
    state == MissionExecutionState::WaitingRemoteJob
}

fn chatgpt_ui_run_proves_liveness(run: &MissionRun, now: chrono::DateTime<chrono::Utc>) -> bool {
    if run.execution_state == MissionExecutionState::Stopping || run.execution_state.is_terminal() {
        return false;
    }
    chrono::DateTime::parse_from_rfc3339(&run.heartbeat_at)
        .ok()
        .map(|heartbeat| {
            (now - heartbeat.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0)
                <= 60
        })
        .unwrap_or(false)
}

async fn mission_has_detached_durable_run(
    mission_store: &dyn MissionStore,
    mission_id: Uuid,
) -> Result<bool, String> {
    Ok(mission_store
        .get_active_mission_run(mission_id)
        .await?
        .is_some_and(|run| detached_run_proves_durable_liveness(run.execution_state)))
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

    // Idle-child notification state: child id → (last_activity_at at the time
    // we notified, notifications sent). One notification per stall episode —
    // re-notify only if the child showed new activity and stalled again.
    let mut idle_child_notified: std::collections::HashMap<Uuid, (String, u32)> =
        std::collections::HashMap::new();
    let idle_child_threshold = idle_worker_notify_threshold_secs();

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
            if info.seconds_since_activity < stuck_seconds() {
                continue;
            }
            let is_chatgpt_ui = info.backend_id.as_deref() == Some("chatgpt_ui");
            if is_chatgpt_ui {
                match mission_store.get_active_mission_run(info.mission_id).await {
                    Ok(Some(run)) if chatgpt_ui_run_proves_liveness(&run, chrono::Utc::now()) => {
                        tracing::debug!(
                            mission_id = %info.mission_id,
                            seconds_since_activity = info.seconds_since_activity,
                            "Stuck-mission watchdog: fresh ChatGPT UI run heartbeat proves liveness"
                        );
                        continue;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            mission_id = %info.mission_id,
                            %error,
                            "Stuck-mission watchdog: cannot inspect ChatGPT UI run; deferring cancellation"
                        );
                        continue;
                    }
                }
            }
            if execution_state_proves_durable_liveness(&info.state) {
                tracing::debug!(
                    mission_id = %info.mission_id,
                    seconds_since_activity = info.seconds_since_activity,
                    "Stuck-mission watchdog: durable remote job proves liveness"
                );
                continue;
            }
            if !inactivity_is_cancellable(info.seconds_since_activity, info.tool_call_in_flight) {
                // Model events are quiet while a harness tool executes. An
                // unmatched ToolCall protects normal long builds, but only for
                // a bounded grace because some harnesses omit ToolResult.
                tracing::debug!(
                    mission_id = %info.mission_id,
                    seconds_since_activity = info.seconds_since_activity,
                    "Stuck-mission watchdog: applying bounded in-flight tool grace"
                );
                continue;
            }
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
            // Never silently auto-kill a mission an operator/controller launched
            // through a control session (`origin == "hermes"` with an
            // `origin_session_id`). A slow scan/build looks idle but is the
            // operator's work; killing it out from under a "launch these
            // missions" request is exactly the confusion we're removing. Skip
            // the kill and log — Phase 1 routes a rich alert to the owning
            // controller session so a human/controller can decide, and Phase 2b
            // will re-narrow this to operator-vs-cron. Dashboard/API-direct
            // missions (`origin == None`) keep the watchdog protection.
            if let Ok(Some(mission)) = mission_store.get_mission(info.mission_id).await {
                if watchdog_skips_control_owned_mission(
                    mission.origin.as_deref(),
                    mission.origin_session_id.as_deref(),
                    &mission.project.tags,
                ) {
                    tracing::warn!(
                        mission_id = %info.mission_id,
                        seconds_since_activity = info.seconds_since_activity,
                        "Stuck-mission watchdog: control-session-launched mission idle past threshold — NOT auto-killing (operator/controller owns it)"
                    );
                    continue;
                }
            }
            tracing::warn!(
                "Stuck-mission watchdog: cancelling {} after {}s of inactivity",
                info.mission_id,
                info.seconds_since_activity
            );
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let cancelled = if cmd_tx
                .send(ControlCommand::CancelMission {
                    mission_id: info.mission_id,
                    min_idle: Some(std::time::Duration::from_secs(stuck_seconds())),
                    respond: cancel_tx,
                })
                .await
                .is_ok()
            {
                matches!(cancel_rx.await, Ok(Ok(CancelMissionOutcome::Cancelled)))
            } else {
                false
            };
            if !cancelled {
                tracing::info!(
                    mission_id = %info.mission_id,
                    "Stuck-mission watchdog: cancellation was skipped or not acknowledged"
                );
                continue;
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
            // A remote build deliberately outlives the harness process. The
            // durable run lease is authoritative here: the remote-build
            // reconciler owns its heartbeat and terminal transition after the
            // conversational runner exits. Marking the presentation row
            // interrupted would make that reconciler flip it back to Active on
            // every tick, producing a false orphan/reconcile loop.
            match mission_has_detached_durable_run(mission_store.as_ref(), mission.id).await {
                Ok(true) => {
                    tracing::debug!(
                        mission_id = %mission.id,
                        "Stuck-mission watchdog: detached durable execution owns liveness"
                    );
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    // Fail closed: if execution truth is temporarily
                    // unavailable, do not write a conflicting terminal
                    // presentation status. The next watchdog tick retries.
                    tracing::warn!(
                        mission_id = %mission.id,
                        %error,
                        "Stuck-mission watchdog: could not inspect active run; deferring orphan reconciliation"
                    );
                    continue;
                }
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

        // Case 3 — a CHILD mission parked in a non-terminal status with no
        // activity for a long time. The boss learns about terminal workers via
        // wait_for_worker, but a worker that ends its turn without delivering
        // (awaiting_user → acknowledged) is silent: nothing tells the boss it
        // has been sitting there for 16 hours. Ping the parent so detection is
        // an event, not a discipline the boss must remember.
        if let Some(threshold) = idle_child_threshold {
            notify_parents_of_idle_children(
                mission_store.as_ref(),
                &cmd_tx,
                threshold,
                &mut idle_child_notified,
            )
            .await;
        }
    }
}

/// Idle threshold (seconds) before a parked child mission triggers a parent
/// notification. Env-tunable; `0` disables the sweep entirely.
fn idle_worker_notify_threshold_secs() -> Option<u64> {
    const DEFAULT_SECS: u64 = 3600;
    match std::env::var("SANDBOXED_SH_IDLE_WORKER_NOTIFY_SECS") {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(secs),
            Err(_) => {
                tracing::warn!(
                    value = %raw,
                    "SANDBOXED_SH_IDLE_WORKER_NOTIFY_SECS is not a number; using default"
                );
                Some(DEFAULT_SECS)
            }
        },
        Err(_) => Some(DEFAULT_SECS),
    }
}

/// Whether a child mission's (status, idle) pair warrants pinging its parent.
/// Only parked, non-terminal statuses count: Active stalls are the stuck-
/// mission watchdog's job, WaitingBackground has its own auto-resume watcher,
/// and Paused is an explicit operator decision.
fn idle_child_needs_parent_ping(status: MissionStatus, idle_seconds: u64, threshold: u64) -> bool {
    matches!(
        status,
        MissionStatus::Pending | MissionStatus::AwaitingUser | MissionStatus::Acknowledged
    ) && idle_seconds >= threshold
}

/// Sweep recent missions for parked children idle past `threshold` and inject
/// a strict control message into their parent mission. One notification per
/// stall episode (keyed by the child's `last_activity` at notify time), capped
/// per child so a permanently-ignored worker cannot spam its boss forever.
async fn notify_parents_of_idle_children(
    mission_store: &dyn MissionStore,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    threshold: u64,
    notified: &mut std::collections::HashMap<Uuid, (String, u32)>,
) {
    const MAX_NOTIFICATIONS_PER_CHILD: u32 = 3;
    const SCAN_LIMIT: usize = 200;

    let missions = match mission_store
        .list_missions_filtered(&MissionFilter::default(), SCAN_LIMIT, 0)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Idle-child watchdog: list missions failed: {}", e);
            return;
        }
    };

    let candidates: Vec<_> = missions
        .iter()
        .filter(|m| m.parent_mission_id.is_some())
        .filter(|m| {
            matches!(
                m.status,
                MissionStatus::Pending | MissionStatus::AwaitingUser | MissionStatus::Acknowledged
            )
        })
        .collect();

    // Drop state for children that left the candidate set (went terminal or
    // active again); if they re-park and stall, that's a fresh episode anyway.
    let candidate_ids: HashSet<Uuid> = candidates.iter().map(|m| m.id).collect();
    notified.retain(|id, _| candidate_ids.contains(id));

    if candidates.is_empty() {
        return;
    }

    let ids: Vec<Uuid> = candidates.iter().map(|m| m.id).collect();
    let activity = match mission_store.get_mission_activity(&ids).await {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!("Idle-child watchdog: load activity failed: {}", e);
            return;
        }
    };

    let now = chrono::Utc::now();
    for child in candidates {
        // Same staleness basis as `populate_activity`: the most recent of
        // updated_at and the newest persisted event.
        let last_event = activity.get(&child.id).and_then(|(e, _)| e.clone());
        let last_activity = [Some(child.updated_at.clone()), last_event]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or_else(|| child.updated_at.clone());
        let idle_seconds = match chrono::DateTime::parse_from_rfc3339(&last_activity) {
            Ok(ts) => (now - ts.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64,
            Err(_) => continue,
        };
        if !idle_child_needs_parent_ping(child.status, idle_seconds, threshold) {
            continue;
        }

        match notified.get(&child.id) {
            // Already notified for this exact stall episode.
            Some((at, _)) if *at == last_activity => continue,
            Some((_, count)) if *count >= MAX_NOTIFICATIONS_PER_CHILD => continue,
            _ => {}
        }

        let Some(parent_id) = child.parent_mission_id else {
            continue;
        };
        // Never resurrect a finished orchestration over a leftover child; a
        // terminal boss's stale workers are garbage-collection material, not
        // an emergency.
        match mission_store.get_mission(parent_id).await {
            Ok(Some(parent)) if !parent.status.is_terminal() => {}
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(
                    child = %child.id,
                    "Idle-child watchdog: parent lookup failed: {}",
                    e
                );
                continue;
            }
        }

        let idle_human = crate::api::control::humanize_idle(idle_seconds);
        let title = child.title.as_deref().unwrap_or("untitled");
        let content = format!(
            "[worker-watchdog] Worker mission {id} ('{title}') has been idle for {idle} \
in status {status}: no new events and no status change. It may be stalled, or parked \
without having delivered. Inspect it with get_worker_diagnostics(mission_id=\"{id}\"), \
then either send it corrective instructions (send_message_to_worker / retask_worker), \
cancel it (cancel_worker), or accept its current output if the work is actually done. \
This notification fires at most once per stall episode.",
            id = child.id,
            title = title,
            idle = idle_human,
            status = child.status,
        );

        let (ack_tx, ack_rx) = oneshot::channel();
        if cmd_tx
            .send(ControlCommand::UserMessage {
                id: Uuid::new_v4(),
                content,
                agent: None,
                target_mission_id: Some(parent_id),
                strict: true,
                source: Some("idle-worker-watchdog".to_string()),
                respond: ack_tx,
            })
            .await
            .is_err()
        {
            return; // actor gone; loop will exit on its own
        }
        let delivered = !matches!(ack_rx.await, Ok(UserMessageAck::Dropped));
        let entry = notified.entry(child.id).or_insert((String::new(), 0));
        entry.0 = last_activity;
        entry.1 += 1;
        tracing::info!(
            child = %child.id,
            parent = %parent_id,
            idle_seconds,
            delivered,
            "Idle-child watchdog: notified parent of idle worker"
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactivity_watchdog_cancels_idle_runner_at_threshold() {
        assert!(inactivity_is_cancellable(STUCK_SECONDS, false));
    }

    #[test]
    fn watchdog_skips_hermes_tag_even_when_origin_field_is_empty() {
        assert!(watchdog_skips_control_owned_mission(
            None,
            None,
            &["origin:hermes-assistant".to_string()]
        ));
        assert!(watchdog_skips_control_owned_mission(
            None,
            None,
            &["pr-writer".to_string()]
        ));
        assert!(watchdog_skips_control_owned_mission(
            Some("hermes"),
            Some("20260814_224352_2a62eb"),
            &[]
        ));
        assert!(!watchdog_skips_control_owned_mission(None, None, &[]));
        assert!(!watchdog_skips_control_owned_mission(
            Some("hermes"),
            None,
            &[]
        ));
    }

    #[test]
    fn inactivity_watchdog_requires_registered_liveness_past_threshold() {
        assert!(inactivity_is_cancellable(STUCK_SECONDS * 2, true));
    }

    #[test]
    fn inactivity_watchdog_eventually_cancels_stale_tool_hint() {
        assert!(inactivity_is_cancellable(STUCK_SECONDS, true));
    }

    #[test]
    fn durable_remote_wait_is_not_an_idle_runner() {
        assert!(execution_state_proves_durable_liveness(
            "waiting_remote_job"
        ));
        assert!(!execution_state_proves_durable_liveness("waiting_tool"));
    }

    #[test]
    fn detached_durable_run_is_not_an_orphan() {
        assert!(detached_run_proves_durable_liveness(
            MissionExecutionState::WaitingRemoteJob
        ));
        assert!(!detached_run_proves_durable_liveness(
            MissionExecutionState::WaitingBackground
        ));
        assert!(!detached_run_proves_durable_liveness(
            MissionExecutionState::Running
        ));
        assert!(!detached_run_proves_durable_liveness(
            MissionExecutionState::WaitingTool
        ));
    }

    #[test]
    fn fresh_chatgpt_ui_run_heartbeat_proves_liveness_until_driver_timeout() {
        let now = chrono::Utc::now();
        let mut run = MissionRun {
            run_id: Uuid::new_v4(),
            mission_id: Uuid::new_v4(),
            generation: 1,
            execution_state: MissionExecutionState::Running,
            owner_actor_id: "control:test".to_string(),
            scope_unit: None,
            started_at: now.to_rfc3339(),
            heartbeat_at: (now - chrono::Duration::seconds(30)).to_rfc3339(),
            stopping_at: None,
            ended_at: None,
            terminal_reason: None,
        };
        assert!(chatgpt_ui_run_proves_liveness(&run, now));

        run.heartbeat_at = (now - chrono::Duration::seconds(61)).to_rfc3339();
        assert!(!chatgpt_ui_run_proves_liveness(&run, now));

        run.heartbeat_at = now.to_rfc3339();
        run.execution_state = MissionExecutionState::Stopping;
        assert!(!chatgpt_ui_run_proves_liveness(&run, now));
    }

    #[test]
    fn idle_child_ping_covers_parked_statuses_past_threshold() {
        // The A.3 incident shape: acknowledged for 16h.
        assert!(idle_child_needs_parent_ping(
            MissionStatus::Acknowledged,
            16 * 3600,
            3600
        ));
        assert!(idle_child_needs_parent_ping(
            MissionStatus::AwaitingUser,
            3600,
            3600
        ));
        assert!(idle_child_needs_parent_ping(
            MissionStatus::Pending,
            7200,
            3600
        ));
        // Below threshold: parked is normal end-of-turn, not a stall.
        assert!(!idle_child_needs_parent_ping(
            MissionStatus::AwaitingUser,
            60,
            3600
        ));
    }

    #[test]
    fn idle_child_ping_never_fires_for_active_terminal_or_paused() {
        for status in [
            MissionStatus::Active,            // stuck-mission watchdog's job
            MissionStatus::WaitingBackground, // bg auto-resume watcher's job
            MissionStatus::Paused,            // operator decision
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Interrupted,
            MissionStatus::Blocked,
            MissionStatus::NotFeasible,
        ] {
            assert!(
                !idle_child_needs_parent_ping(status, 1_000_000, 3600),
                "{status} must not trigger a parent ping"
            );
        }
    }
}
