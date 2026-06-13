//! Task board scheduler: server-owned orchestration of worker missions.
//!
//! The boss agent registers a task DAG once (via the orchestrator MCP's
//! `plan_tasks`, which lands in `MissionStore::upsert_board_tasks`). From that
//! point the control loop owns the schedule:
//!
//! - `scheduler_pass` (throttled inside the actor's 100ms tick) spawns a
//!   worker mission for every dependency-satisfied `pending` task while
//!   capacity allows, sweeps zombies (workers lost to a restart), and — this
//!   is the control-plane part — sends a generic, content-free WAKE to a boss
//!   when its board has tasks needing a decision and the boss is idle.
//! - `on_worker_settled` (called when a parallel runner parks) classifies the
//!   outcome, retries failures once, and persists the result. It does NOT push
//!   any message to the boss.
//!
//! Pull model (why): the boss reacts to its OWN board state, not to a pushed
//! per-task digest. The wake carries no task/board specifics, so even if it
//! were misdelivered, the receiving mission would just read its own (empty)
//! board and end its turn — one board's work can never leak into another
//! mission. All control-plane sends are STRICT (`UserMessage { strict: true }`):
//! delivered only to the exact target, never `/goal`-rewritten, never routed to
//! the main session. They self-send into the actor's command channel via
//! `try_send` (never awaited — the scheduler runs on the consuming task).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agents::TerminalReason;
use crate::api::mission_store::{BoardTask, BoardTaskOutcome, BoardTaskStatus, MissionStore};

use super::{ControlCommand, MissionStatus};

/// One slot is always reserved for the boss itself so digest delivery can
/// never be starved by board workers occupying every parallel slot.
const RESERVED_BOSS_SLOTS: usize = 1;

/// Max attempts per task (1 original + 1 automatic retry).
const MAX_ATTEMPTS: u32 = 2;

/// How long a `running` task may sit with its worker mission still `pending`
/// (spawn message lost, e.g. dropped on a capacity race) before the scheduler
/// re-kicks it.
const STUCK_PENDING_SECS: i64 = 90;

/// Digest truncation: keep the head and tail of the worker's final message.
const DIGEST_HEAD_CHARS: usize = 400;
const DIGEST_TAIL_CHARS: usize = 1200;

/// Snapshot of runner occupancy, computed by the actor loop each pass.
pub struct RunnerSnapshot {
    /// Mission ids present in `parallel_runners` (running or parked).
    pub present: HashSet<Uuid>,
    /// Mission ids currently executing a turn (main + parallel). Used to decide
    /// whether a boss is busy before sending it a board wake.
    pub running_ids: HashSet<Uuid>,
    /// Count of runners actively executing a turn.
    pub running_count: usize,
    /// Whether the main (non-parallel) session is executing a turn.
    pub main_running: bool,
}

/// Tasks whose dependencies are satisfied and that are ready to spawn.
/// A dependency is satisfied when the dep task settled successfully or was
/// accepted by the boss. Unknown dep keys block forever (visible in the UI)
/// rather than silently passing.
pub fn ready_tasks(tasks: &[BoardTask]) -> Vec<&BoardTask> {
    tasks
        .iter()
        .filter(|t| t.status == BoardTaskStatus::Pending)
        .filter(|t| {
            t.depends_on.iter().all(|dep_key| {
                tasks.iter().any(|d| {
                    d.task_key == *dep_key
                        && (d.status == BoardTaskStatus::Accepted
                            || (d.status == BoardTaskStatus::Settled
                                && d.outcome == Some(BoardTaskOutcome::Success)))
                })
            })
        })
        .collect()
}

/// Classify how a worker turn ended.
pub fn classify_outcome(
    terminal_reason: Option<TerminalReason>,
    success: bool,
    output: &str,
) -> BoardTaskOutcome {
    let failed = matches!(
        terminal_reason,
        Some(TerminalReason::Cancelled)
            | Some(TerminalReason::ServerShutdown)
            | Some(TerminalReason::LlmError)
            | Some(TerminalReason::Stalled)
            | Some(TerminalReason::InfiniteLoop)
            | Some(TerminalReason::MaxIterations)
            | Some(TerminalReason::RateLimited)
            | Some(TerminalReason::CapacityLimited)
            | Some(TerminalReason::AuthError)
    ) || (terminal_reason.is_none() && !success);
    if failed {
        return BoardTaskOutcome::Failed;
    }
    // Harness-level failures can surface as a "successful" turn whose entire
    // output is an error banner (e.g. opencode session errors arrive via
    // stderr text, not terminal_reason — observed in the dev smoke test).
    // Only match banners at the very start so a legit summary that merely
    // mentions errors isn't misclassified.
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return BoardTaskOutcome::Failed;
    }
    const ERROR_BANNERS: [&str; 4] = [
        "Error:",
        "[session.error]",
        "[MAIN] SESSION.ERROR",
        "Session ended with error",
    ];
    if ERROR_BANNERS.iter().any(|b| trimmed.starts_with(b)) {
        return BoardTaskOutcome::Failed;
    }
    // Worker contract: a stuck worker ends its turn with a line starting
    // "BLOCKED". Look near the start of the final message.
    let head: String = output.trim_start().chars().take(600).collect();
    if head.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("BLOCKED") || l.starts_with("**BLOCKED")
    }) {
        return BoardTaskOutcome::Blocked;
    }
    BoardTaskOutcome::Success
}

/// Head+tail truncation that keeps the final summary (workers put their
/// conclusion at the end of the turn).
pub fn digest_excerpt(output: &str) -> String {
    let chars: Vec<char> = output.trim().chars().collect();
    if chars.len() <= DIGEST_HEAD_CHARS + DIGEST_TAIL_CHARS {
        return chars.into_iter().collect();
    }
    let head: String = chars[..DIGEST_HEAD_CHARS].iter().collect();
    let tail: String = chars[chars.len() - DIGEST_TAIL_CHARS..].iter().collect();
    format!("{head}\n[… truncated …]\n{tail}")
}

/// The standing contract appended to every worker prompt.
fn worker_contract(task: &BoardTask) -> String {
    format!(
        "\n\n---\n[task-board contract] You are the worker for task `{key}` (\"{title}\") \
         of boss mission {boss}.\n\
         - Work autonomously until the success condition in the task is met and verified.\n\
         - Do NOT end your turn to report progress; partial updates are wasted.\n\
         - End your turn ONLY when: (a) the task is done and verified — finish with a short \
         summary of what changed and how you verified it; or (b) you are genuinely stuck — \
         finish with a line starting `BLOCKED:` plus the obstacle, what you tried, and ONE \
         specific question.\n\
         - Never widen scope beyond the task.",
        key = task.task_key,
        title = task.title,
        boss = task.boss_mission_id,
    )
}

/// True when a board has at least one task needing a boss decision — a
/// settled task awaiting a verdict, or a task that exhausted its retries and
/// failed. This is the wake trigger.
fn board_needs_attention(tasks: &[BoardTask]) -> bool {
    tasks
        .iter()
        .any(|t| matches!(t.status, BoardTaskStatus::Settled | BoardTaskStatus::Failed))
}

/// Generic, content-free wake delivered to a boss when its board changes.
/// Deliberately mentions NO specific task or other board — the boss reacts to
/// its OWN board state. If this ever reaches the wrong mission, that mission
/// finds nothing to act on and simply ends its turn, so a misroute can't leak
/// one board's work into another mission.
const BOARD_WAKE_PROMPT: &str = "[task-board] Your task board changed — one or more tasks \
    settled, failed, or need a decision. Call board_status now and act on YOUR board only: \
    judge each settled task with accept_task / reject_task (review_task for detail), \
    merge_branch finished worktree branches, and plan_tasks for newly-unblocked or follow-up \
    work. Scheduling, retries, and worker dispatch are automatic — never wait or poll. If \
    board_status shows nothing needing action, just end your turn.";

/// Fire-and-forget control-plane send into the actor's own command channel.
/// Always strict: delivered only to `target_mission_id`, never re-routed to
/// the main session or rewritten as a `/goal`. Never awaits (the scheduler
/// runs on the task that consumes this channel).
fn self_send_message(
    cmd_tx: &mpsc::Sender<ControlCommand>,
    target_mission_id: Uuid,
    content: String,
) -> bool {
    let (respond, _rx) = oneshot::channel();
    match cmd_tx.try_send(ControlCommand::UserMessage {
        id: Uuid::new_v4(),
        content,
        agent: None,
        target_mission_id: Some(target_mission_id),
        strict: true,
        respond,
    }) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(target = %target_mission_id, "board: self-send failed: {}", e);
            false
        }
    }
}

fn seconds_since(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds())
        .unwrap_or(i64::MAX)
}

/// Spawn workers for ready tasks while capacity allows, and sweep zombies.
/// Called from the control actor's tick, throttled by the caller (~2s).
pub async fn scheduler_pass(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    snapshot: &RunnerSnapshot,
    max_parallel: usize,
    // Per-boss "a wake is outstanding" flag, owned by the actor loop. Coalesces
    // wakes: at most one pending wake per boss until it next runs (consuming it).
    wake_state: &mut HashMap<Uuid, bool>,
) {
    let boards = match mission_store.list_active_board_missions().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("board: failed to list active boards: {}", e);
            return;
        }
    };
    if boards.is_empty() {
        return;
    }

    let total_running = snapshot.running_count + usize::from(snapshot.main_running);
    let spawnable_cap = max_parallel.saturating_sub(RESERVED_BOSS_SLOTS);
    let mut available = spawnable_cap.saturating_sub(total_running);

    for boss_id in boards {
        let tasks = match mission_store.list_board_tasks(boss_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(boss = %boss_id, "board: failed to list tasks: {}", e);
                continue;
            }
        };

        // --- Zombie sweep: running tasks whose worker is not actually running.
        for task in tasks
            .iter()
            .filter(|t| t.status == BoardTaskStatus::Running)
        {
            let Some(worker_id) = task.worker_mission_id else {
                continue;
            };
            if snapshot.present.contains(&worker_id) {
                continue; // runner alive (running or about to be reaped normally)
            }
            let Ok(Some(worker)) = mission_store.get_mission(worker_id).await else {
                continue;
            };
            match worker.status {
                // Spawn message lost (e.g. capacity race) — re-kick after a grace period.
                MissionStatus::Pending => {
                    if seconds_since(&task.updated_at) > STUCK_PENDING_SECS {
                        tracing::info!(task = %task.task_key, worker = %worker_id,
                            "board: re-kicking stuck pending worker");
                        let prompt = format!("{}{}", task.prompt, worker_contract(task));
                        if self_send_message(cmd_tx, worker_id, prompt) {
                            let mut t = task.clone();
                            t.notes = append_note(&t.notes, "re-kicked stuck pending worker");
                            let _ = mission_store.save_board_task(&t).await;
                        }
                    }
                }
                // Worker settled while we weren't looking (server restart).
                MissionStatus::AwaitingUser
                | MissionStatus::Completed
                | MissionStatus::Acknowledged => {
                    let last = worker
                        .history
                        .iter()
                        .rev()
                        .find(|h| h.role == "assistant")
                        .map(|h| h.content.clone())
                        .unwrap_or_default();
                    settle_task(
                        mission_store,
                        task.clone(),
                        classify_outcome(None, true, &last),
                        &last,
                    )
                    .await;
                }
                MissionStatus::Failed
                | MissionStatus::Interrupted
                | MissionStatus::Blocked
                | MissionStatus::NotFeasible => {
                    let last = worker
                        .history
                        .iter()
                        .rev()
                        .find(|h| h.role == "assistant")
                        .map(|h| h.content.clone())
                        .unwrap_or_default();
                    settle_task(mission_store, task.clone(), BoardTaskOutcome::Failed, &last).await;
                }
                MissionStatus::Active => {
                    // Runner may exist in another control session or be mid-start;
                    // leave it alone.
                }
            }
        }

        // --- Spawn ready tasks while capacity allows.
        if available > 0 {
            if let Ok(Some(boss)) = mission_store.get_mission(boss_id).await {
                let ready: Vec<BoardTask> = ready_tasks(&tasks).into_iter().cloned().collect();
                for task in ready {
                    if available == 0 {
                        break;
                    }
                    match spawn_task_worker(mission_store, cmd_tx, &task, boss.workspace_id).await {
                        Ok(worker_id) => {
                            available -= 1;
                            tracing::info!(task = %task.task_key, worker = %worker_id, boss = %boss_id,
                                "board: spawned worker for ready task");
                        }
                        Err(e) => {
                            tracing::warn!(task = %task.task_key, boss = %boss_id,
                                "board: failed to spawn worker: {}", e);
                        }
                    }
                }
            }
        }

        // --- Wake decision (pull model): if the boss isn't currently running a
        // turn and its board has tasks needing a decision, send ONE generic,
        // content-free wake. Coalesced via wake_state so we don't re-wake every
        // pass; cleared once the boss is observed running (it consumed the wake)
        // and re-armed when it goes idle with work still pending.
        let boss_running = snapshot.running_ids.contains(&boss_id);
        if boss_running {
            wake_state.insert(boss_id, false);
            continue;
        }
        // Re-read post-sweep/spawn state for an accurate decision.
        let fresh = mission_store
            .list_board_tasks(boss_id)
            .await
            .unwrap_or(tasks);
        let needs = board_needs_attention(&fresh);
        if !needs {
            wake_state.insert(boss_id, false);
        } else if !wake_state.get(&boss_id).copied().unwrap_or(false)
            && self_send_message(cmd_tx, boss_id, BOARD_WAKE_PROMPT.to_string())
        {
            wake_state.insert(boss_id, true);
            tracing::info!(boss = %boss_id, "board: sent wake (tasks awaiting decision)");
        }
    }
}

fn append_note(notes: &Option<String>, line: &str) -> Option<String> {
    let stamp = chrono::Utc::now().to_rfc3339();
    match notes {
        Some(n) => Some(format!("{n}\n[{stamp}] {line}")),
        None => Some(format!("[{stamp}] {line}")),
    }
}

async fn spawn_task_worker(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    task: &BoardTask,
    workspace_id: Uuid,
) -> Result<Uuid, String> {
    let mission = mission_store
        .create_mission_with_parent(
            Some(&format!("[{}] {}", task.task_key, task.title)),
            Some(workspace_id),
            None,
            task.model_override.as_deref(),
            task.model_effort.as_deref(),
            Some(&task.backend),
            None,
            Some(task.boss_mission_id),
            task.working_directory.as_deref(),
        )
        .await?;

    let mut t = task.clone();
    t.worker_mission_id = Some(mission.id);
    t.status = BoardTaskStatus::Running;
    t.attempts += 1;
    if t.attempts > 1 {
        t.notes = append_note(&t.notes, &format!("retry: attempt {}", t.attempts));
    }
    mission_store.save_board_task(&t).await?;

    let prompt = format!("{}{}", t.prompt, worker_contract(&t));
    if !self_send_message(cmd_tx, mission.id, prompt) {
        // Channel full: leave the task running; the zombie sweep re-kicks the
        // pending worker mission after the grace period.
        tracing::warn!(task = %t.task_key, "board: spawn message deferred (channel full)");
    }
    Ok(mission.id)
}

/// Settle a task: persist outcome + result digest, and retry failures once.
/// Does NOT notify the boss — the scheduler pass wakes the boss from board
/// state (pull model), so a settle never pushes per-task content into any
/// mission. Shared by the live settle hook and the zombie sweep.
async fn settle_task(
    mission_store: &Arc<dyn MissionStore>,
    mut task: BoardTask,
    outcome: BoardTaskOutcome,
    output: &str,
) {
    if outcome == BoardTaskOutcome::Failed && task.attempts < MAX_ATTEMPTS {
        // Silent automatic retry: back to pending, next pass respawns fresh.
        task.status = BoardTaskStatus::Pending;
        task.notes = append_note(
            &task.notes,
            &format!(
                "attempt {} failed (worker {}); auto-retrying",
                task.attempts,
                task.worker_mission_id
                    .map(|id| id.to_string())
                    .unwrap_or_default()
            ),
        );
        task.worker_mission_id = None;
        if let Err(e) = mission_store.save_board_task(&task).await {
            tracing::warn!(task = %task.task_key, "board: failed to persist retry: {}", e);
        }
        return;
    }

    task.status = if outcome == BoardTaskOutcome::Failed {
        BoardTaskStatus::Failed
    } else {
        BoardTaskStatus::Settled
    };
    task.outcome = Some(outcome);
    // result_digest is stored for the UI / review_task, not pushed anywhere.
    task.result_digest = Some(digest_excerpt(output));
    if let Err(e) = mission_store.save_board_task(&task).await {
        tracing::warn!(task = %task.task_key, "board: failed to persist settle: {}", e);
    }
}

/// Live settle hook: called from the control actor's tick when a parallel
/// runner parks with no queued follow-up. No-op for missions that are not
/// board workers.
pub async fn on_worker_settled(
    mission_store: &Arc<dyn MissionStore>,
    worker_mission_id: Uuid,
    output: &str,
    terminal_reason: Option<TerminalReason>,
    success: bool,
) {
    let task = match mission_store
        .get_board_task_by_worker(worker_mission_id)
        .await
    {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(worker = %worker_mission_id, "board: task lookup failed: {}", e);
            return;
        }
    };
    if task.status != BoardTaskStatus::Running {
        return; // already settled (sweep) or cancelled meanwhile
    }
    let outcome = classify_outcome(terminal_reason, success, output);
    settle_task(mission_store, task, outcome, output).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::NewBoardTask;

    fn mk(
        key: &str,
        deps: &[&str],
        status: BoardTaskStatus,
        outcome: Option<BoardTaskOutcome>,
    ) -> BoardTask {
        BoardTask {
            id: Uuid::new_v4(),
            boss_mission_id: Uuid::nil(),
            task_key: key.to_string(),
            title: key.to_string(),
            prompt: "p".into(),
            backend: "codex".into(),
            model_override: None,
            model_effort: None,
            working_directory: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            status,
            outcome,
            worker_mission_id: None,
            attempts: 0,
            result_digest: None,
            notes: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn ready_respects_dependencies() {
        let tasks = vec![
            mk(
                "a",
                &[],
                BoardTaskStatus::Settled,
                Some(BoardTaskOutcome::Success),
            ),
            mk("b", &["a"], BoardTaskStatus::Pending, None),
            mk("c", &["b"], BoardTaskStatus::Pending, None),
            mk("d", &["missing"], BoardTaskStatus::Pending, None),
            mk("e", &[], BoardTaskStatus::Pending, None),
        ];
        let ready: Vec<&str> = ready_tasks(&tasks)
            .iter()
            .map(|t| t.task_key.as_str())
            .collect();
        assert_eq!(ready, vec!["b", "e"]);
    }

    #[test]
    fn ready_blocks_on_failed_or_blocked_dep() {
        let tasks = vec![
            mk(
                "a",
                &[],
                BoardTaskStatus::Settled,
                Some(BoardTaskOutcome::Blocked),
            ),
            mk("b", &["a"], BoardTaskStatus::Pending, None),
            mk(
                "c",
                &[],
                BoardTaskStatus::Failed,
                Some(BoardTaskOutcome::Failed),
            ),
            mk("d", &["c"], BoardTaskStatus::Pending, None),
        ];
        assert!(ready_tasks(&tasks).is_empty());
    }

    #[test]
    fn accepted_dep_unblocks() {
        let tasks = vec![
            mk(
                "a",
                &[],
                BoardTaskStatus::Accepted,
                Some(BoardTaskOutcome::Success),
            ),
            mk("b", &["a"], BoardTaskStatus::Pending, None),
        ];
        let ready: Vec<&str> = ready_tasks(&tasks)
            .iter()
            .map(|t| t.task_key.as_str())
            .collect();
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn classify_blocked_and_failed() {
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "All done, verified."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "BLOCKED: cannot find the artifact.\nTried X and Y."
            ),
            BoardTaskOutcome::Blocked
        );
        assert_eq!(
            classify_outcome(Some(TerminalReason::Stalled), false, "whatever"),
            BoardTaskOutcome::Failed
        );
        // Harness error banners masquerading as a successful turn.
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "Error: Unexpected error, check log file at /root/.local/share/opencode/log"
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(Some(TerminalReason::TurnComplete), true, "   "),
            BoardTaskOutcome::Failed
        );
        // A summary that merely mentions an error is still a success.
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "Done. Fixed the Error: handling path and verified with cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(classify_outcome(None, false, "x"), BoardTaskOutcome::Failed);
        assert_eq!(
            classify_outcome(Some(TerminalReason::Completed), true, "done"),
            BoardTaskOutcome::Success
        );
    }

    #[test]
    fn digest_excerpt_truncates_keeping_tail() {
        let long = "a".repeat(5000);
        let d = digest_excerpt(&long);
        assert!(d.len() < 2000);
        assert!(d.contains("[… truncated …]"));
        let short = "short output";
        assert_eq!(digest_excerpt(short), short);
    }

    #[test]
    fn needs_attention_detection() {
        // Settled (awaiting verdict) or Failed → boss is needed.
        assert!(board_needs_attention(&[mk(
            "a",
            &[],
            BoardTaskStatus::Settled,
            Some(BoardTaskOutcome::Success)
        )]));
        assert!(board_needs_attention(&[mk(
            "a",
            &[],
            BoardTaskStatus::Settled,
            Some(BoardTaskOutcome::Blocked)
        )]));
        assert!(board_needs_attention(&[mk(
            "a",
            &[],
            BoardTaskStatus::Failed,
            Some(BoardTaskOutcome::Failed)
        )]));
        // Only running/pending/accepted/cancelled → nothing for the boss to do.
        assert!(!board_needs_attention(&[
            mk("a", &[], BoardTaskStatus::Running, None),
            mk("b", &[], BoardTaskStatus::Pending, None),
        ]));
        assert!(!board_needs_attention(&[mk(
            "a",
            &[],
            BoardTaskStatus::Accepted,
            Some(BoardTaskOutcome::Success)
        )]));
        assert!(!board_needs_attention(&[]));
    }

    #[tokio::test]
    async fn upsert_and_dep_flow_in_memory_store() {
        use crate::api::mission_store::InMemoryMissionStore;
        let store = InMemoryMissionStore::new();
        let boss = Uuid::new_v4();
        let tasks = store
            .upsert_board_tasks(
                boss,
                vec![
                    NewBoardTask {
                        task_key: "t1".into(),
                        title: "first".into(),
                        prompt: "do x".into(),
                        backend: "codex".into(),
                        model_override: Some("gpt-5.5".into()),
                        model_effort: None,
                        working_directory: None,
                        depends_on: vec![],
                    },
                    NewBoardTask {
                        task_key: "t2".into(),
                        title: "second".into(),
                        prompt: "do y".into(),
                        backend: "opencode".into(),
                        model_override: None,
                        model_effort: None,
                        working_directory: None,
                        depends_on: vec!["t1".into()],
                    },
                ],
            )
            .await
            .expect("upsert");
        assert_eq!(tasks.len(), 2);

        let listed = store.list_board_tasks(boss).await.expect("list");
        let ready: Vec<&str> = ready_tasks(&listed)
            .iter()
            .map(|t| t.task_key.as_str())
            .collect();
        assert_eq!(ready, vec!["t1"]);

        // Settle t1 successfully → t2 becomes ready.
        let mut t1 = listed.iter().find(|t| t.task_key == "t1").unwrap().clone();
        t1.status = BoardTaskStatus::Settled;
        t1.outcome = Some(BoardTaskOutcome::Success);
        store.save_board_task(&t1).await.expect("save");
        let listed = store.list_board_tasks(boss).await.expect("list");
        let ready: Vec<&str> = ready_tasks(&listed)
            .iter()
            .map(|t| t.task_key.as_str())
            .collect();
        assert_eq!(ready, vec!["t2"]);

        // Upsert with same key on a pending task updates it; settled untouched.
        let again = store
            .upsert_board_tasks(
                boss,
                vec![NewBoardTask {
                    task_key: "t1".into(),
                    title: "changed".into(),
                    prompt: "p".into(),
                    backend: "codex".into(),
                    model_override: None,
                    model_effort: None,
                    working_directory: None,
                    depends_on: vec![],
                }],
            )
            .await
            .expect("upsert again");
        assert_eq!(
            again[0].title, "first",
            "settled task must not be clobbered"
        );

        assert_eq!(
            store.list_active_board_missions().await.expect("active"),
            vec![boss]
        );
    }
}
