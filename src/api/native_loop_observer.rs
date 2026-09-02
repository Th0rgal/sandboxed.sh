//! Translate harness goal-loop events into Automation / AutomationExecution
//! rows. Runs as a single background task that subscribes to the same
//! broadcast channel as the event logger.
//!
//! Behavior:
//!   1. First `GoalIteration` or `GoalStatus` event for a mission triggers
//!      lazy materialization of an `Automation { driver: HarnessLoop }` row
//!      (idempotent — reuses an existing row for the same harness/command).
//!   2. Each `Iteration` observation writes a `Success` AutomationExecution.
//!   3. A terminal `Completed` observation closes the most recent execution
//!      and (when status is final) **deletes** the automation row. The row is
//!      a UI artifact representing an in-progress goal loop, not a
//!      user-defined automation — keeping it as `active=0` pollutes the panel
//!      (one piled-up row per `/goal` cycle). Cascade FK drops the iteration
//!      executions; the mission event stream retains the full history.
//!
//! Phase 1 doesn't drive the harness — it observes. The user-typed `/goal X`
//! already triggers the harness path; this task ensures the panel sees it.
//!
//! Errors are swallowed with a warn log: this task must never crash the
//! event-broadcasting infrastructure.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::broadcast;
use uuid::Uuid;

use super::control::AgentEvent;
use super::mission_store::{
    Automation, AutomationDriver, AutomationExecution, CommandSource, ExecutionStatus,
    MissionStore, StopPolicy, TriggerType,
};
use crate::backend::native_loops::{self, LoopObservation};

/// Run the observer loop until the broadcast channel closes. Spawn from
/// `bootstrap_control_state` next to the event logger.
pub async fn run(store: Arc<dyn MissionStore>, events_tx: broadcast::Sender<AgentEvent>) {
    let mut rx = events_tx.subscribe();
    // mission_id -> automation_id materialized lazily on first goal event.
    let mut cache: HashMap<Uuid, Uuid> = HashMap::new();
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Err(e) = handle_event(&store, &event, &mut cache).await {
                    tracing::warn!(error = %e, "native_loop_observer event handling failed");
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("native_loop_observer lagged by {} events", n);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    tracing::info!("native_loop_observer task stopped");
}

/// Returns the canonical terminal statuses that should flip the automation
/// inactive. `paused` / `budget_limited` are *not* terminal: the loop may
/// resume.
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "complete" | "aborted" | "cleared")
}

async fn handle_event(
    store: &Arc<dyn MissionStore>,
    event: &AgentEvent,
    cache: &mut HashMap<Uuid, Uuid>,
) -> Result<(), String> {
    let mission_id = match event {
        AgentEvent::GoalIteration { mission_id, .. } => *mission_id,
        AgentEvent::GoalStatus { mission_id, .. } => *mission_id,
        _ => return Ok(()),
    };
    let Some(mission_id) = mission_id else {
        return Ok(());
    };
    let Some(mission) = store.get_mission(mission_id).await? else {
        return Ok(());
    };
    let harness = mission.backend.as_str();
    let Some(adapter) = native_loops::find_adapter(harness, "goal") else {
        return Ok(());
    };

    let observation = adapter.observe(event);
    if matches!(observation, LoopObservation::None) {
        return Ok(());
    }

    let objective = match event {
        AgentEvent::GoalIteration { objective, .. } => objective.clone(),
        AgentEvent::GoalStatus { objective, .. } => objective.clone(),
        _ => String::new(),
    };

    let automation_id =
        ensure_automation(store, mission_id, harness, "goal", &objective, cache).await?;

    match observation {
        LoopObservation::Iteration { index, summary } => {
            record_iteration_execution(store, automation_id, mission_id, index, summary).await?;
        }
        LoopObservation::Completed { status, summary } => {
            record_completion_execution(store, automation_id, mission_id, &status, summary).await?;
            if is_terminal_status(&status) {
                // Delete (not just deactivate) — see module doc. Cascade FK
                // drops the iteration executions; mission events keep history.
                if let Err(e) = store.delete_automation(automation_id).await {
                    tracing::warn!(
                        automation_id = %automation_id,
                        error = %e,
                        "Failed to delete completed native-loop automation"
                    );
                }
                // Evict from cache so a subsequent `/goal` on the same mission
                // creates a fresh automation row instead of reattaching to the
                // deleted one.
                cache.remove(&mission_id);
            }
        }
        LoopObservation::None => {}
    }

    Ok(())
}

/// Find an existing `NativeLoop` automation for this mission+harness+command,
/// or create one. Cached in-memory after first lookup.
async fn ensure_automation(
    store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    harness: &str,
    command: &str,
    objective: &str,
    cache: &mut HashMap<Uuid, Uuid>,
) -> Result<Uuid, String> {
    if let Some(&id) = cache.get(&mission_id) {
        return Ok(id);
    }
    // Only consider *active* native-loop rows: a prior `/goal` on this
    // mission may have completed and been deactivated, in which case
    // reusing it would append iterations to a row the panel hides.
    let existing = store
        .get_mission_automations(mission_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|a| {
            a.active
                && matches!(
                    &a.command_source,
                    CommandSource::NativeLoop { harness: h, command: c, .. }
                        if h == harness && c == command
                )
        });
    let id = if let Some(a) = existing {
        a.id
    } else {
        let new = Automation {
            id: Uuid::new_v4(),
            mission_id,
            command_source: CommandSource::NativeLoop {
                harness: harness.to_string(),
                command: command.to_string(),
                args: serde_json::json!({ "objective": objective }),
            },
            // No scheduler trigger applies; AgentFinished is the closest
            // semantic match ("fires when the harness signals iteration").
            trigger: TriggerType::AgentFinished,
            variables: Default::default(),
            active: true,
            created_at: now_iso(),
            last_triggered_at: None,
            retry_config: Default::default(),
            stop_policy: StopPolicy::WhenMissionTerminal,
            fresh_session: Default::default(),
            consecutive_failures: 0,
            driver: AutomationDriver::HarnessLoop,
        };
        store.create_automation(new.clone()).await?;
        new.id
    };
    cache.insert(mission_id, id);
    Ok(id)
}

async fn record_iteration_execution(
    store: &Arc<dyn MissionStore>,
    automation_id: Uuid,
    mission_id: Uuid,
    index: u32,
    summary: Option<String>,
) -> Result<(), String> {
    let exec = AutomationExecution {
        id: Uuid::new_v4(),
        automation_id,
        mission_id,
        triggered_at: now_iso(),
        trigger_source: format!("harness_loop:iteration:{}", index),
        status: ExecutionStatus::Success,
        webhook_payload: None,
        variables_used: Default::default(),
        completed_at: Some(now_iso()),
        error: summary,
        retry_count: 0,
    };
    store.create_automation_execution(exec).await.map(|_| ())
}

async fn record_completion_execution(
    store: &Arc<dyn MissionStore>,
    automation_id: Uuid,
    mission_id: Uuid,
    status: &str,
    summary: Option<String>,
) -> Result<(), String> {
    let exec_status = match status {
        "complete" => ExecutionStatus::Success,
        "aborted" | "cleared" => ExecutionStatus::Cancelled,
        _ => ExecutionStatus::Running, // paused / budget_limited
    };
    let exec = AutomationExecution {
        id: Uuid::new_v4(),
        automation_id,
        mission_id,
        triggered_at: now_iso(),
        trigger_source: format!("harness_loop:status:{}", status),
        status: exec_status,
        webhook_payload: None,
        variables_used: Default::default(),
        completed_at: Some(now_iso()),
        error: summary,
        retry_count: 0,
    };
    store.create_automation_execution(exec).await.map(|_| ())
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::SqliteMissionStore;

    async fn store_with_codex_mission() -> (Arc<dyn MissionStore>, Uuid) {
        let temp = tempfile::tempdir().expect("tempdir");
        let raw = SqliteMissionStore::new(temp.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let store: Arc<dyn MissionStore> = Arc::new(raw);
        let mission = store
            .create_mission(
                Some("goal-loop"),
                None,
                None,
                None,
                None,
                Some("codex"),
                None,
            )
            .await
            .expect("mission");
        std::mem::forget(temp); // keep dir alive for the test's lifetime
        (store, mission.id)
    }

    #[tokio::test]
    async fn iteration_event_materializes_active_native_loop_row() {
        let (store, mission_id) = store_with_codex_mission().await;
        let mut cache = HashMap::new();

        let event = AgentEvent::GoalIteration {
            iteration: 1,
            objective: "ship the feature".to_string(),
            mission_id: Some(mission_id),
        };
        handle_event(&store, &event, &mut cache)
            .await
            .expect("handle iteration");

        let automations = store
            .get_mission_automations(mission_id)
            .await
            .expect("list");
        assert_eq!(automations.len(), 1);
        assert!(automations[0].active);
        assert!(matches!(
            automations[0].command_source,
            CommandSource::NativeLoop { .. }
        ));
    }

    #[tokio::test]
    async fn terminal_complete_status_deletes_native_loop_row() {
        let (store, mission_id) = store_with_codex_mission().await;
        let mut cache = HashMap::new();

        handle_event(
            &store,
            &AgentEvent::GoalIteration {
                iteration: 1,
                objective: "ship".into(),
                mission_id: Some(mission_id),
            },
            &mut cache,
        )
        .await
        .expect("iter");

        handle_event(
            &store,
            &AgentEvent::GoalStatus {
                status: "complete".into(),
                objective: "ship".into(),
                mission_id: Some(mission_id),
            },
            &mut cache,
        )
        .await
        .expect("complete");

        let automations = store
            .get_mission_automations(mission_id)
            .await
            .expect("list");
        assert!(
            automations.is_empty(),
            "completed native_loop row should be deleted, found {} row(s)",
            automations.len()
        );
        assert!(
            !cache.contains_key(&mission_id),
            "cache should be evicted after terminal status"
        );
    }

    #[tokio::test]
    async fn terminal_aborted_and_cleared_also_delete_row() {
        for status in ["aborted", "cleared"] {
            let (store, mission_id) = store_with_codex_mission().await;
            let mut cache = HashMap::new();

            handle_event(
                &store,
                &AgentEvent::GoalIteration {
                    iteration: 1,
                    objective: "ship".into(),
                    mission_id: Some(mission_id),
                },
                &mut cache,
            )
            .await
            .expect("iter");

            handle_event(
                &store,
                &AgentEvent::GoalStatus {
                    status: status.into(),
                    objective: "ship".into(),
                    mission_id: Some(mission_id),
                },
                &mut cache,
            )
            .await
            .expect("terminal");

            let automations = store
                .get_mission_automations(mission_id)
                .await
                .expect("list");
            assert!(
                automations.is_empty(),
                "status '{}' should delete row, found {}",
                status,
                automations.len()
            );
        }
    }

    #[tokio::test]
    async fn nonterminal_status_keeps_row_active() {
        // `paused` / `budget_limited` are not terminal: the loop may resume.
        // The row must stay so subsequent iterations reattach to it.
        let (store, mission_id) = store_with_codex_mission().await;
        let mut cache = HashMap::new();

        handle_event(
            &store,
            &AgentEvent::GoalIteration {
                iteration: 1,
                objective: "ship".into(),
                mission_id: Some(mission_id),
            },
            &mut cache,
        )
        .await
        .expect("iter");

        handle_event(
            &store,
            &AgentEvent::GoalStatus {
                status: "paused".into(),
                objective: "ship".into(),
                mission_id: Some(mission_id),
            },
            &mut cache,
        )
        .await
        .expect("paused");

        let automations = store
            .get_mission_automations(mission_id)
            .await
            .expect("list");
        assert_eq!(automations.len(), 1);
        assert!(automations[0].active);
    }

    #[tokio::test]
    async fn second_goal_cycle_creates_a_fresh_row_after_completion() {
        // Regression guard for the original bug: the panel was accumulating
        // a new inactive row per `/goal` cycle. After the fix, each cycle
        // owns at most one row, deleted on completion.
        let (store, mission_id) = store_with_codex_mission().await;
        let mut cache = HashMap::new();

        for cycle in 0..3 {
            handle_event(
                &store,
                &AgentEvent::GoalIteration {
                    iteration: 1,
                    objective: format!("cycle {}", cycle),
                    mission_id: Some(mission_id),
                },
                &mut cache,
            )
            .await
            .expect("iter");

            handle_event(
                &store,
                &AgentEvent::GoalStatus {
                    status: "complete".into(),
                    objective: format!("cycle {}", cycle),
                    mission_id: Some(mission_id),
                },
                &mut cache,
            )
            .await
            .expect("complete");
        }

        let automations = store
            .get_mission_automations(mission_id)
            .await
            .expect("list");
        assert!(
            automations.is_empty(),
            "three completed cycles should leave zero rows, found {}",
            automations.len()
        );
    }
}
