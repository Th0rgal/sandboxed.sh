//! Background-task auto-resume watcher for Claude Code `Bash run_in_background`.
//!
//! When a mission agent launches a background shell job and parks in
//! `AwaitingUser`, this module polls for completion inside the workspace and
//! wakes the agent with the output.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::workspace;
use crate::workspace_exec::WorkspaceExec;

use super::super::control::{AgentEvent, ControlCommand, MissionStatus, UserMessageAck};
use super::super::mission_runner::{BackgroundTask, BackgroundTaskRegistry};
use super::super::mission_store::MissionStore;

/// How often the watcher polls in-flight background tasks for completion.
const BG_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Once a probe reports the launched process gone (`busy == false`), it must
/// stay gone for at least this long before we treat the task as finished. This
/// absorbs a transient single-probe `pgrep` miss (e.g. the process briefly not
/// matching while it forks/execs a child).
const BG_IDLE_STABLE_SECS: u64 = 15;

/// Grace period after a task starts during which a not-busy probe is NOT
/// trusted on its own. A task that finished before our very first probe never
/// shows as busy, so we only trust "not busy" once either (a) we have seen it
/// busy at least once, or (b) it has existed at least this long (so a probe
/// that simply raced the launch can't immediately declare it finished).
const BG_START_GRACE_SECS: u64 = 20;

/// Hard cap on how long we track a single background task before treating it as
/// done regardless of the completion heuristic. Prevents a task whose process
/// we can't observe (exotic shell, missing pgrep) from being watched forever.
const BG_OVERALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Max number of trailing bytes of the output file to include in the resume
/// message, so we never blow up a turn's context with a huge log.
const BG_OUTPUT_TAIL_BYTES: usize = 4000;

/// Cap on completion-check commands per tick to bound work even if a misbehaving
/// agent spawns hundreds of background jobs.
const BG_MAX_CHECKS_PER_TICK: usize = 64;

/// Outcome of a single workspace completion probe.
///
/// The probe keys completion off the launched **process** (`busy`), not the
/// output file: Claude Code's background-task output file is ephemeral and is
/// usually gone by the time we probe (the turn has already ended). `exists`
/// and `mtime_epoch` are kept only so we can tail the output when it happens to
/// still be present.
#[derive(Debug, PartialEq, Eq)]
enum BgProbe {
    /// A probe result. `exists` reflects whether the output file is present
    /// (for tailing); `busy` is true while the launched command's process is
    /// still running; `mtime_epoch` is the output file mtime (0 when absent).
    /// `pgrep_available` is false when `pgrep` is missing from the workspace —
    /// the watcher must not trust `busy` in that case (fail closed).
    Observed {
        exists: bool,
        mtime_epoch: u64,
        busy: bool,
        pgrep_available: bool,
    },
    /// The probe could not be run / parsed (WorkspaceExec error, bad output).
    /// We skip this task this tick and retry next pass. `reason` carries the
    /// underlying WorkspaceExec error (when the probe failed to exec) purely for
    /// diagnostic logging; it is `None` for a parse failure.
    Unknown { reason: Option<String> },
}

/// Per-task bookkeeping the watcher keeps across ticks.
#[derive(Debug, Clone)]
struct BgWatch {
    /// True once any probe has reported the launched process running. Lets us
    /// trust a later "not busy" reading even if the task finished quickly.
    was_busy: bool,
    /// Wall-clock instant at which we *first* observed the task not busy in the
    /// current not-busy streak. Reset whenever a probe reports busy. Used to
    /// require the process stay gone for `BG_IDLE_STABLE_SECS`.
    idle_since: Option<Instant>,
}

/// Pure completion decision for one probe, factored out so it is unit-testable
/// without a real filesystem or process table.
fn bg_decide_finished(
    prev: Option<&BgWatch>,
    busy: bool,
    task_age_secs: u64,
    now: Instant,
) -> (bool, BgWatch) {
    let prev_was_busy = prev.map(|w| w.was_busy).unwrap_or(false);

    if busy {
        return (
            false,
            BgWatch {
                was_busy: true,
                idle_since: None,
            },
        );
    }

    let idle_since = prev.and_then(|w| w.idle_since).unwrap_or(now);
    let idle_secs = now.duration_since(idle_since).as_secs();

    let trust_not_busy = prev_was_busy || task_age_secs >= BG_START_GRACE_SECS;
    let finished = trust_not_busy && idle_secs >= BG_IDLE_STABLE_SECS;

    (
        finished,
        BgWatch {
            was_busy: prev_was_busy,
            idle_since: Some(idle_since),
        },
    )
}

/// Watcher that auto-resumes missions whose background shell tasks have finished.
///
/// Uses an in-memory [`BackgroundTaskRegistry`] shared with the control actor.
/// A persistent mission store is **not** required — only
/// [`MissionStore::get_mission`] must work (including in-memory dev stores).
pub(crate) async fn background_task_autoresume_loop(
    mission_store: Arc<dyn MissionStore>,
    cmd_tx: mpsc::Sender<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    workspaces: workspace::SharedWorkspaceStore,
    background_tasks: BackgroundTaskRegistry,
) {
    tracing::info!(
        "Background-task auto-resume watcher started: poll {}s, start-grace {}s, idle-stable {}s, \
         timeout {}s",
        BG_POLL_INTERVAL.as_secs(),
        BG_START_GRACE_SECS,
        BG_IDLE_STABLE_SECS,
        BG_OVERALL_TIMEOUT.as_secs(),
    );

    let mut watches: HashMap<(Uuid, String), BgWatch> = HashMap::new();

    loop {
        tokio::time::sleep(BG_POLL_INTERVAL).await;

        let snapshot: Vec<(Uuid, BackgroundTask)> = {
            let guard = background_tasks.read().await;
            guard
                .iter()
                .flat_map(|(mid, tasks)| tasks.values().map(move |t| (*mid, t.clone())))
                .collect()
        };

        if snapshot.is_empty() {
            watches.clear();
            continue;
        }

        {
            let live: HashSet<(Uuid, String)> =
                snapshot.iter().map(|(m, t)| (*m, t.id.clone())).collect();
            watches.retain(|k, _| live.contains(k));
        }

        let mut checks_done = 0usize;
        for (mission_id, task) in snapshot {
            if checks_done >= BG_MAX_CHECKS_PER_TICK {
                break;
            }

            let mission = match mission_store.get_mission(mission_id).await {
                Ok(Some(m)) => m,
                Ok(None) => {
                    remove_background_task(&background_tasks, mission_id, &task.id).await;
                    continue;
                }
                Err(e) => {
                    tracing::debug!(
                        mission_id = %mission_id,
                        "bg-autoresume: get_mission failed: {}; retrying next tick",
                        e
                    );
                    continue;
                }
            };
            if mission.status != MissionStatus::AwaitingUser {
                let terminal = matches!(
                    mission.status,
                    MissionStatus::Completed | MissionStatus::Failed | MissionStatus::NotFeasible
                );
                let age_secs = task.started_at.elapsed().as_secs();
                let prev = watches.get(&(mission_id, task.id.clone()));
                tracing::debug!(
                    mission_id = %mission_id,
                    task = %task.id,
                    mission_status = ?mission.status,
                    exists = tracing::field::Empty,
                    mtime_epoch = tracing::field::Empty,
                    busy = tracing::field::Empty,
                    was_busy = prev.map(|w| w.was_busy).unwrap_or(false),
                    age_secs,
                    idle_secs = tracing::field::Empty,
                    decision = "skipped_not_awaiting_user",
                    "bg-autoresume: probe",
                );
                if terminal || task.started_at.elapsed() >= BG_OVERALL_TIMEOUT {
                    remove_background_task(&background_tasks, mission_id, &task.id).await;
                }
                continue;
            }

            let timed_out = task.started_at.elapsed() >= BG_OVERALL_TIMEOUT;

            let workspace = match workspaces.get(mission.workspace_id).await {
                Some(ws) => ws,
                None => {
                    tracing::debug!(
                        mission_id = %mission_id,
                        workspace_id = %mission.workspace_id,
                        "bg-autoresume: workspace not found; skipping"
                    );
                    continue;
                }
            };
            let exec = WorkspaceExec::new(workspace.clone());

            let mut output_exists = timed_out;
            let mut finished = timed_out;
            let age_secs = task.started_at.elapsed().as_secs();
            if timed_out {
                let prev = watches.get(&(mission_id, task.id.clone()));
                tracing::debug!(
                    mission_id = %mission_id,
                    task = %task.id,
                    mission_status = ?mission.status,
                    exists = tracing::field::Empty,
                    mtime_epoch = tracing::field::Empty,
                    busy = tracing::field::Empty,
                    was_busy = prev.map(|w| w.was_busy).unwrap_or(false),
                    age_secs,
                    idle_secs = tracing::field::Empty,
                    decision = "timeout",
                    "bg-autoresume: probe",
                );
            } else {
                checks_done += 1;
                match probe_background_task(&exec, &workspace.path, &task).await {
                    BgProbe::Observed {
                        exists,
                        mtime_epoch,
                        busy,
                        pgrep_available,
                    } => {
                        if !pgrep_available {
                            let prev = watches.get(&(mission_id, task.id.clone()));
                            tracing::warn!(
                                mission_id = %mission_id,
                                task = %task.id,
                                "bg-autoresume: pgrep unavailable in workspace; \
                                 cannot detect completion (will retry until timeout)"
                            );
                            tracing::debug!(
                                mission_id = %mission_id,
                                task = %task.id,
                                mission_status = ?mission.status,
                                exists = tracing::field::Empty,
                                mtime_epoch = tracing::field::Empty,
                                busy = tracing::field::Empty,
                                was_busy = prev.map(|w| w.was_busy).unwrap_or(false),
                                age_secs,
                                idle_secs = tracing::field::Empty,
                                decision = "pgrep_unavailable",
                                "bg-autoresume: probe",
                            );
                            continue;
                        }

                        output_exists = exists;
                        let key = (mission_id, task.id.clone());
                        let now = Instant::now();
                        let was_busy = watches.get(&key).map(|w| w.was_busy).unwrap_or(false);
                        let (is_finished, next) =
                            bg_decide_finished(watches.get(&key), busy, age_secs, now);
                        let idle_secs = next
                            .idle_since
                            .map(|s| now.duration_since(s).as_secs())
                            .unwrap_or(0);
                        watches.insert(key, next);
                        finished = is_finished;
                        tracing::debug!(
                            mission_id = %mission_id,
                            task = %task.id,
                            mission_status = ?mission.status,
                            exists,
                            mtime_epoch,
                            busy,
                            was_busy,
                            age_secs,
                            idle_secs,
                            decision = if is_finished { "finished" } else { "waiting" },
                            "bg-autoresume: probe",
                        );
                    }
                    BgProbe::Unknown { reason } => {
                        let prev = watches.get(&(mission_id, task.id.clone()));
                        tracing::debug!(
                            mission_id = %mission_id,
                            task = %task.id,
                            mission_status = ?mission.status,
                            exists = tracing::field::Empty,
                            mtime_epoch = tracing::field::Empty,
                            busy = tracing::field::Empty,
                            was_busy = prev.map(|w| w.was_busy).unwrap_or(false),
                            age_secs,
                            idle_secs = tracing::field::Empty,
                            decision = "probe_unknown",
                            error = reason.as_deref().unwrap_or("(parse failure)"),
                            "bg-autoresume: probe",
                        );
                        continue;
                    }
                }
            }

            if !finished {
                continue;
            }

            let tail = if output_exists {
                read_output_tail(&exec, &workspace.path, &task.output_path)
                    .await
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let command_display = if task.command.trim().is_empty() {
                "(command unavailable)".to_string()
            } else {
                task.command.clone()
            };
            let content = if tail.is_empty() {
                let note = if timed_out {
                    " (reported finished after the 30-minute watcher timeout; it may \
                     still be running.)"
                } else {
                    ""
                };
                format!(
                    "Background task `{}` (`{}`) finished. (No captured output was \
                     available.){}\n\nContinue from here.",
                    task.id, command_display, note,
                )
            } else {
                let note = if timed_out {
                    "\n\n(Note: reported as finished after the 30-minute watcher timeout; \
                     it may still be running.)"
                } else {
                    ""
                };
                format!(
                    "Background task `{}` (`{}`) finished. Output:\n\n```\n{}\n```{}\n\nContinue from here.",
                    task.id, command_display, tail, note,
                )
            };

            let _ = events_tx.send(AgentEvent::MissionActivity {
                label: format!("Resuming: background task `{}` finished", task.id),
                tool_name: "background_task_autoresume".to_string(),
                mission_id: Some(mission_id),
            });

            let (ack_tx, ack_rx) = oneshot::channel();
            let send_res = cmd_tx
                .send(ControlCommand::UserMessage {
                    id: Uuid::new_v4(),
                    content,
                    agent: None,
                    target_mission_id: Some(mission_id),
                    strict: true,
                    respond: ack_tx,
                })
                .await;
            if let Err(e) = send_res {
                tracing::warn!(
                    mission_id = %mission_id,
                    task = %task.id,
                    "bg-autoresume: control channel closed; exiting watcher: {}",
                    e
                );
                return;
            }

            match ack_rx.await {
                Ok(UserMessageAck::Dropped) => {
                    tracing::info!(
                        mission_id = %mission_id,
                        task = %task.id,
                        "bg-autoresume: resume dropped (not deliverable this pass); will retry"
                    );
                    continue;
                }
                Ok(_) => {
                    tracing::info!(
                        mission_id = %mission_id,
                        task = %task.id,
                        timed_out = timed_out,
                        "bg-autoresume: resumed mission with background task output"
                    );
                }
                Err(_) => {
                    // Actor dropped the responder without sending an ack — the
                    // delivery outcome is unknown. Keep the task in the registry
                    // and retry next tick rather than dropping the wake.
                    tracing::warn!(
                        mission_id = %mission_id,
                        task = %task.id,
                        "bg-autoresume: resume ack channel closed before ack; will retry"
                    );
                    continue;
                }
            }

            let prev = watches.get(&(mission_id, task.id.clone()));
            tracing::debug!(
                mission_id = %mission_id,
                task = %task.id,
                mission_status = ?mission.status,
                exists = output_exists,
                mtime_epoch = tracing::field::Empty,
                busy = tracing::field::Empty,
                was_busy = prev.map(|w| w.was_busy).unwrap_or(false),
                age_secs = task.started_at.elapsed().as_secs(),
                idle_secs = tracing::field::Empty,
                decision = "resumed",
                "bg-autoresume: probe",
            );

            remove_background_task(&background_tasks, mission_id, &task.id).await;
            watches.remove(&(mission_id, task.id.clone()));
        }
    }
}

async fn remove_background_task(
    background_tasks: &BackgroundTaskRegistry,
    mission_id: Uuid,
    task_id: &str,
) {
    let mut guard = background_tasks.write().await;
    if let Some(tasks) = guard.get_mut(&mission_id) {
        tasks.remove(task_id);
        if tasks.is_empty() {
            guard.remove(&mission_id);
        }
    }
}

/// Choose a `pgrep -f` pattern for a background task.
///
/// Match on the launched **command**, not Claude's task id. The task id (and
/// the output-file path) do NOT appear in the launched process's command line:
/// Claude runs the job as `/bin/bash -c '… eval <command> …'` and redirects its
/// output to the task file via an fd from its parent, so the only
/// task-identifying text in the process table is the command itself. Verified
/// live against the deployed harness — grepping every process cmdline for the
/// task id / output path matched nothing. Matching the id would never match,
/// `busy` would stay false, and the watcher would resume prematurely (the
/// original footgun). The `/proc/$$/cmdline` self-match guard in the probe
/// keeps the command pattern from matching the probe shell itself.
///
/// The id / output-file stem are kept only as a last-resort fallback for the
/// (unexpected) case where the command is empty.
fn pgrep_pattern_for_task(task: &BackgroundTask) -> String {
    let cmd = truncate_for_pgrep(&task.command);
    if !cmd.is_empty() {
        return cmd;
    }
    if !task.id.is_empty() {
        return task.id.clone();
    }
    Path::new(&task.output_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| name.strip_suffix(".output").unwrap_or(name).to_string())
        .unwrap_or_default()
}

async fn probe_background_task(exec: &WorkspaceExec, cwd: &Path, task: &BackgroundTask) -> BgProbe {
    let out_path = shell_single_quote(&task.output_path);
    let task_pattern = shell_single_quote(&pgrep_pattern_for_task(task));
    let script = format!(
        r#"
set -u
P={out_path}
PAT={task_pattern}
EXISTS=0
M=0
if [ -e "$P" ]; then
  EXISTS=1
  M=$(stat -c %Y "$P" 2>/dev/null) || M=$(stat -f %m "$P" 2>/dev/null) || M=0
fi
BUSY=0
HAS_PGREP=0
SELF=$$
# This script embeds the command pattern, so the probe shell (and the subshells
# it forks for $(…), which share its argv) match `pgrep -f $PAT` themselves.
# Skip our own pid and every pid whose /proc cmdline equals ours. Linux-only:
# /proc is absent on macOS host workspaces, so SELFCMD stays empty and only $$
# is skipped there. Production nspawn containers are Linux and get the full guard.
SELFCMD=$(tr '\0' ' ' < /proc/$$/cmdline 2>/dev/null)
if [ -n "$PAT" ] && command -v pgrep >/dev/null 2>&1; then
  HAS_PGREP=1
  for pid in $(pgrep -f -- "$PAT" 2>/dev/null); do
    [ "$pid" = "$SELF" ] && continue
    PIDCMD=$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null)
    [ -n "$SELFCMD" ] && [ "$PIDCMD" = "$SELFCMD" ] && continue
    BUSY=1
    break
  done
fi
if [ "$BUSY" = "0" ] && [ "$EXISTS" = "1" ] && command -v fuser >/dev/null 2>&1; then
  if fuser "$P" >/dev/null 2>&1; then BUSY=1; fi
fi
echo "BG $EXISTS $M $BUSY $HAS_PGREP"
"#,
    );

    let args = vec!["-c".to_string(), script];
    let output = match exec.output(cwd, "/bin/sh", &args, HashMap::new()).await {
        Ok(o) => o,
        Err(e) => {
            return BgProbe::Unknown {
                reason: Some(e.to_string()),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_probe_line(&stdout)
}

/// Parse the `BG <exists> <mtime> <busy> [<pgrep_ok>]` line from the probe.
fn parse_probe_line(stdout: &str) -> BgProbe {
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("BG ") {
            let mut parts = rest.split_whitespace();
            let exists = parts.next().and_then(|s| s.parse::<u8>().ok());
            let mtime = parts.next().and_then(|s| s.parse::<u64>().ok());
            let busy = parts.next().and_then(|s| s.parse::<u8>().ok());
            let pgrep_ok = parts
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .map(|v| v != 0);
            return match (exists, mtime, busy) {
                (Some(e), Some(m), Some(b)) => BgProbe::Observed {
                    exists: e != 0,
                    mtime_epoch: m,
                    busy: b != 0,
                    // Fail closed: without an explicit pgrep_ok=1 we do not trust busy.
                    pgrep_available: pgrep_ok.unwrap_or(false),
                },
                _ => BgProbe::Unknown { reason: None },
            };
        }
    }
    BgProbe::Unknown { reason: None }
}

async fn read_output_tail(exec: &WorkspaceExec, cwd: &Path, output_path: &str) -> Option<String> {
    let out_path = shell_single_quote(output_path);
    let bytes = BG_OUTPUT_TAIL_BYTES;
    let script = format!(
        r#"set -u; P={out_path}; [ -f "$P" ] && tail -c {bytes} "$P" 2>/dev/null || true"#,
    );
    let args = vec!["-c".to_string(), script];
    let output = exec
        .output(cwd, "/bin/sh", &args, HashMap::new())
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let trimmed = text.trim_end_matches('\n');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Fallback when task id and output-path stem are both unavailable.
fn truncate_for_pgrep(command: &str) -> String {
    const MAX: usize = 120;
    let trimmed = command.trim();
    if trimmed.len() <= MAX {
        trimmed.to_string()
    } else {
        let mut end = MAX;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        trimmed[..end].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bg_decide_finished, parse_probe_line, pgrep_pattern_for_task, shell_single_quote,
        truncate_for_pgrep, BackgroundTask, BgProbe, BgWatch, BG_IDLE_STABLE_SECS,
        BG_START_GRACE_SECS,
    };
    use std::time::{Duration, Instant};

    fn sample_task(id: &str, output_path: &str, command: &str) -> BackgroundTask {
        BackgroundTask {
            id: id.to_string(),
            output_path: output_path.to_string(),
            command: command.to_string(),
            started_at: Instant::now(),
        }
    }

    #[test]
    fn pgrep_pattern_prefers_command() {
        // The command is the only task-identifying text in the launched
        // process's cmdline (the id / output path are not), so it must win.
        let task = sample_task("bokwqyjak", "/tmp/tasks/bokwqyjak.output", "sleep 40");
        assert_eq!(pgrep_pattern_for_task(&task), "sleep 40");
    }

    #[test]
    fn pgrep_pattern_falls_back_to_task_id_when_command_empty() {
        let task = sample_task("bokwqyjak", "/tmp/tasks/bokwqyjak.output", "");
        assert_eq!(pgrep_pattern_for_task(&task), "bokwqyjak");
    }

    #[test]
    fn pgrep_pattern_falls_back_to_output_stem_when_command_and_id_empty() {
        let task = sample_task("", "/tmp/tasks/bokwqyjak.output", "");
        assert_eq!(pgrep_pattern_for_task(&task), "bokwqyjak");
    }

    #[test]
    fn parse_probe_exists_idle_with_pgrep() {
        match parse_probe_line("BG 1 1700000000 0 1\n") {
            BgProbe::Observed {
                exists,
                mtime_epoch,
                busy,
                pgrep_available,
            } => {
                assert!(exists);
                assert_eq!(mtime_epoch, 1_700_000_000);
                assert!(!busy);
                assert!(pgrep_available);
            }
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn parse_probe_busy_with_pgrep() {
        match parse_probe_line("noise\nBG 1 42 1 1") {
            BgProbe::Observed {
                busy,
                pgrep_available,
                ..
            } => {
                assert!(busy);
                assert!(pgrep_available);
            }
            other => panic!("expected Observed busy, got {other:?}"),
        }
    }

    #[test]
    fn parse_probe_without_pgrep_field_fails_closed() {
        match parse_probe_line("BG 0 0 0") {
            BgProbe::Observed {
                pgrep_available, ..
            } => assert!(!pgrep_available),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn parse_probe_pgrep_unavailable_flag() {
        match parse_probe_line("BG 0 0 0 0") {
            BgProbe::Observed {
                pgrep_available, ..
            } => assert!(!pgrep_available),
            other => panic!("expected Observed, got {other:?}"),
        }
    }

    #[test]
    fn parse_probe_file_absent_is_still_observed() {
        match parse_probe_line("BG 0 0 0 1") {
            BgProbe::Observed {
                exists,
                mtime_epoch,
                busy,
                pgrep_available,
            } => {
                assert!(!exists);
                assert_eq!(mtime_epoch, 0);
                assert!(!busy);
                assert!(pgrep_available);
            }
            other => panic!("expected Observed (absent file), got {other:?}"),
        }
    }

    #[test]
    fn parse_probe_unknown() {
        assert!(matches!(
            parse_probe_line("garbage"),
            BgProbe::Unknown { .. }
        ));
        assert!(matches!(
            parse_probe_line("BG 1 x y"),
            BgProbe::Unknown { .. }
        ));
    }

    fn watch(was_busy: bool, idle_since: Option<Instant>) -> BgWatch {
        BgWatch {
            was_busy,
            idle_since,
        }
    }

    #[test]
    fn busy_is_never_finished_and_records_was_busy() {
        let now = Instant::now();
        let (finished, next) = bg_decide_finished(None, true, 999, now);
        assert!(!finished);
        assert!(next.was_busy);
        assert!(next.idle_since.is_none());
    }

    #[test]
    fn first_probe_not_busy_within_grace_waits() {
        let now = Instant::now();
        let (finished, next) = bg_decide_finished(None, false, BG_START_GRACE_SECS - 1, now);
        assert!(!finished);
        assert!(next.idle_since.is_some());
        assert!(!next.was_busy);
    }

    #[test]
    fn finished_before_first_probe_trusted_after_grace_and_idle_window() {
        let idle_start = Instant::now();
        let prev = watch(false, Some(idle_start));
        let now = idle_start + Duration::from_secs(BG_IDLE_STABLE_SECS);
        let (finished, _next) =
            bg_decide_finished(Some(&prev), false, BG_START_GRACE_SECS + 5, now);
        assert!(finished);
    }

    #[test]
    fn was_busy_then_idle_stable_finishes_even_before_grace() {
        let idle_start = Instant::now();
        let prev = watch(true, Some(idle_start));
        let now = idle_start + Duration::from_secs(BG_IDLE_STABLE_SECS);
        let (finished, _next) = bg_decide_finished(Some(&prev), false, 1, now);
        assert!(finished);
    }

    #[test]
    fn shell_quoting_escapes_single_quotes() {
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn pgrep_truncation_is_char_safe() {
        let long = "é".repeat(200);
        let t = truncate_for_pgrep(&long);
        assert!(t.len() <= 120);
        assert!(long.starts_with(&t));
    }
}
