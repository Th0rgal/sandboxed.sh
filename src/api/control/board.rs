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
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::agents::TerminalReason;
use crate::api::mission_store::{
    now_string, BoardOutboxItem, BoardTask, BoardTaskOutcome, BoardTaskRole, BoardTaskStatus,
    MissionHistoryEntry, MissionProjectPatch, MissionStore, TaskAttempt,
};

use super::{ControlCommand, MissionStatus, UserMessageAck};

/// One slot is always reserved for the boss itself so digest delivery can
/// never be starved by board workers occupying every parallel slot.
const RESERVED_BOSS_SLOTS: usize = 1;
const RETRY_PREFLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Max attempts per task (1 original + 1 automatic retry).
const MAX_ATTEMPTS: u32 = 2;

/// How long a `running` task may sit with its worker mission still `pending`
/// (spawn message lost, e.g. dropped on a capacity race) before the scheduler
/// re-kicks it.
const STUCK_PENDING_SECS: i64 = 90;

/// Digest truncation: keep the head and tail of the worker's final message.
const DIGEST_HEAD_CHARS: usize = 400;
const DIGEST_TAIL_CHARS: usize = 1200;

fn role_default_model(task: &BoardTask) -> Option<&'static str> {
    if task.backend != "codex" {
        return None;
    }
    Some(match task.role {
        BoardTaskRole::Planner | BoardTaskRole::Reviewer => "gpt-5.6-sol",
        BoardTaskRole::Reconciler if task.risk_class == "high" => "gpt-5.6-sol",
        BoardTaskRole::Worker | BoardTaskRole::Reconciler => "gpt-5.6-terra",
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetryPreflight {
    NothingFound,
    Surviving {
        branch_state: String,
        pr_number: Option<u64>,
    },
    Merged {
        pr_number: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryDisposition {
    Spawn,
    ParkForBossReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomaticRetry {
    Allowed,
    Suppressed,
    DifferentChatGptUiProfile(usize),
}

fn chatgpt_ui_compatibility_profile_slot(output: &str) -> Option<usize> {
    const INDEX_MARKER: &str = "profile_index=";
    const LEGACY_MARKER: &str = "compatibility=chatgpt-ui-v2; profile_slot=";
    let tail = output
        .split_once(INDEX_MARKER)
        .or_else(|| output.split_once(LEGACY_MARKER))?
        .1;
    let digits = tail
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn automatic_retry(
    task: &BoardTask,
    terminal_reason: Option<TerminalReason>,
    output: &str,
) -> AutomaticRetry {
    if task.backend != "chatgpt_ui" {
        return AutomaticRetry::Allowed;
    }
    match terminal_reason {
        Some(TerminalReason::AuthError | TerminalReason::RateLimited) => AutomaticRetry::Suppressed,
        _ => chatgpt_ui_compatibility_profile_slot(output)
            .map(AutomaticRetry::DifferentChatGptUiProfile)
            .unwrap_or(AutomaticRetry::Allowed),
    }
}

fn persisted_terminal_reason(reason: Option<&str>) -> Option<TerminalReason> {
    match reason {
        Some("turn_complete") => Some(TerminalReason::TurnComplete),
        Some("completed") => Some(TerminalReason::Completed),
        Some("cancelled") => Some(TerminalReason::Cancelled),
        Some("server_shutdown") => Some(TerminalReason::ServerShutdown),
        Some("llm_error") => Some(TerminalReason::LlmError),
        Some("stalled") => Some(TerminalReason::Stalled),
        Some("infinite_loop") => Some(TerminalReason::InfiniteLoop),
        Some("max_iterations") => Some(TerminalReason::MaxIterations),
        Some("rate_limited") => Some(TerminalReason::RateLimited),
        Some("capacity_limited") => Some(TerminalReason::CapacityLimited),
        Some("auth_error") => Some(TerminalReason::AuthError),
        _ => None,
    }
}

fn retry_disposition(task: &BoardTask, preflight: &RetryPreflight) -> RetryDisposition {
    if task.attempts > 0 && matches!(preflight, RetryPreflight::Merged { .. }) {
        RetryDisposition::ParkForBossReview
    } else {
        RetryDisposition::Spawn
    }
}

/// Guidance prepended to automatic-retry prompts when the task declares an
/// outcome contract. A failed attempt often means the prompt over-specified
/// the approach, not just that the worker slipped: the retry is told
/// explicitly that only the task's acceptance criteria / verification are
/// binding, so it can pick a simpler approach instead of mechanically
/// re-running the one that just failed.
const RETRY_RELAXATION_GUIDANCE: &str = "[Retry guidance] The prior attempt failed. Do not \
    mechanically repeat it. The task's success condition — its acceptance criteria and \
    verification command (see the task-board contract below) — is the only hard requirement; \
    any suggested approach in the prompt is advisory. Choose the simplest approach that \
    satisfies the success condition and addresses the prior failure.";

/// Retry guidance for tasks WITHOUT an outcome contract. The prompt is then
/// the task's only specification, so it must stay binding — declaring it
/// advisory here would leave the retry with no success condition at all and
/// let a worker simplify away required behavior.
const RETRY_PROMPT_BINDING_GUIDANCE: &str = "[Retry guidance] The prior attempt failed. Do \
    not mechanically repeat it. The prompt's stated scope and success condition remain \
    binding; what is open is the approach — try a different or simpler way to satisfy them, \
    addressing the prior failure.";

/// Whether the task declares an objective success condition beyond its
/// prompt: at least one non-blank acceptance criterion or a non-blank
/// verification command. Registration normalizes blanks away
/// (`validate_and_normalize_board_tasks`), but tasks persisted before that —
/// or written through another path — must not have `[" "]` count as a
/// contract and get the prompt declared advisory.
fn has_outcome_contract(task: &BoardTask) -> bool {
    task.acceptance_criteria
        .iter()
        .any(|criterion| !criterion.trim().is_empty())
        || task
            .verification_command
            .as_deref()
            .map(str::trim)
            .is_some_and(|command| !command.is_empty())
}

fn retry_prompt(task: &BoardTask, preflight: &RetryPreflight) -> String {
    let mut sections: Vec<String> = Vec::new();

    let branch_guard = match preflight {
        RetryPreflight::Surviving {
            branch_state,
            pr_number,
        } => Some((branch_state.as_str(), *pr_number)),
        // A spawn message can be dropped after the live preflight result was
        // computed. The zombie re-kick only has persisted task metadata, so
        // retain the same conservative branch guard for every declared retry
        // rather than falling back to the original unguarded prompt.
        RetryPreflight::NothingFound
            if task.attempts > 1
                && task.prior_worker_mission_id.is_some()
                && task.branch.is_some() =>
        {
            Some(("declared retry branch; re-check before editing", None))
        }
        _ => None,
    };
    if let Some((branch_state, pr_number)) = branch_guard {
        let pr = pr_number
            .map(|number| format!("#{number}"))
            .unwrap_or_else(|| "none".to_string());
        sections.push(format!(
            "[Prior-attempt digest]\nPrior worker: {}\nPrior outcome: {}\nPrior result: {}\nBranch: {} ({branch_state})\nPR: {pr}\n\
             Continue the prior attempt: checkout the existing branch, never recreate from master, never force-push.",
            task.prior_worker_mission_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".into()),
            task.prior_outcome
                .map(|outcome| outcome.to_string())
                .unwrap_or_else(|| "unknown".into()),
            task.prior_result_digest.as_deref().unwrap_or("unavailable"),
            task.branch.as_deref().unwrap_or("unknown"),
        ));
    } else if task.attempts > 1 {
        // No branch to guard, but this is still an automatic retry: surface
        // what the prior attempt produced so the retry reacts to the failure
        // instead of rediscovering it.
        if let Some(digest) = task.prior_result_digest.as_deref() {
            sections.push(format!(
                "[Prior-attempt digest]\nPrior outcome: {}\nPrior result: {digest}",
                task.prior_outcome
                    .map(|outcome| outcome.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ));
        }
    }

    if task.attempts > 1 {
        let guidance = if has_outcome_contract(task) {
            RETRY_RELAXATION_GUIDANCE
        } else {
            RETRY_PROMPT_BINDING_GUIDANCE
        };
        sections.push(guidance.to_string());
    }

    sections.push(task.prompt.clone());
    sections.join("\n\n")
}

fn repository_identity(repository: &str) -> Option<String> {
    if !Path::new(repository).exists() {
        return repository.contains('/').then(|| repository.to_string());
    }
    let output = Command::new("git")
        .current_dir(repository)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let trimmed = url.trim_end_matches(".git");
    let slug = trimmed
        .split("github.com/")
        .nth(1)
        .or_else(|| trimmed.rsplit_once(':').map(|(_, tail)| tail))?;
    Some(slug.trim_start_matches('/').to_string())
}

fn retry_pr_state(
    rows: &[serde_json::Value],
    current_branch_sha: Option<&str>,
) -> (Option<u64>, Option<u64>) {
    let mut open_pr = None;
    let mut matching_merged_pr = None;
    let mut any_merged_pr = None;
    for row in rows {
        let number = row["number"].as_u64().unwrap_or_default();
        if row["state"].as_str() == Some("OPEN") {
            open_pr.get_or_insert(number);
        }
        if row["mergedAt"].is_string() {
            any_merged_pr.get_or_insert(number);
            if current_branch_sha.is_some_and(|sha| row["headRefOid"].as_str() == Some(sha)) {
                matching_merged_pr.get_or_insert(number);
            }
        }
    }
    let merged_pr = if open_pr.is_some() {
        None
    } else if current_branch_sha.is_some() {
        matching_merged_pr
    } else {
        any_merged_pr
    };
    (open_pr, merged_pr)
}

async fn retry_preflight(task: &BoardTask) -> RetryPreflight {
    let Some(repository) = task.repository.clone() else {
        return RetryPreflight::NothingFound;
    };
    let Some(branch) = task.branch.clone() else {
        return RetryPreflight::NothingFound;
    };
    let working_directory = task.working_directory.clone();
    let task_key = task.task_key.clone();
    let preflight = tokio::task::spawn_blocking(move || {
        let local_repo = if Path::new(&repository).exists() {
            Some(repository.as_str())
        } else {
            working_directory
                .as_deref()
                .filter(|path| Path::new(path).exists())
        };
        let local_sha = local_repo.and_then(|repo| {
            let output = Command::new("git")
                .current_dir(repo)
                .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        });
        let remote_sha = local_repo.and_then(|repo| {
            match Command::new("git")
                .current_dir(repo)
                .args(["ls-remote", "--exit-code", "origin", &format!("refs/heads/{branch}")])
                .output()
            {
                Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .next()
                    .map(str::to_string),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(task = %task_key, "board: retry remote branch lookup failed: {error}");
                    None
                }
            }
        });
        let local_exists = local_sha.is_some();
        let remote_exists = remote_sha.is_some();
        let current_branch_sha = remote_sha.as_deref().or(local_sha.as_deref());

        let mut open_pr = None;
        let mut merged_pr = None;
        if let Some(identity) = repository_identity(&repository) {
            match Command::new("gh")
                .args([
                    "pr",
                    "list",
                    "--repo",
                    &identity,
                    "--head",
                    &branch,
                    "--state",
                    "all",
                    "--limit",
                    "20",
                    "--json",
                    "number,state,mergedAt,headRefOid",
                ])
                .output()
            {
                Ok(output) if output.status.success() => {
                    if let Ok(rows) =
                        serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
                    {
                        (open_pr, merged_pr) = retry_pr_state(&rows, current_branch_sha);
                    }
                }
                Ok(output) => tracing::warn!(task = %task_key, status = %output.status,
                    "board: retry PR lookup failed; continuing best-effort"),
                Err(error) => tracing::warn!(task = %task_key,
                    "board: retry PR lookup unavailable; continuing best-effort: {error}"),
            }
        }
        // An open PR always represents the current continuation. A historical
        // merged PR for a reused deterministic branch only parks the retry
        // when its reviewed head matches the branch we can currently observe.
        if let Some(pr_number) = merged_pr {
            return RetryPreflight::Merged { pr_number };
        }
        if local_exists || remote_exists || open_pr.is_some() {
            let state = match (local_exists, remote_exists) {
                (true, true) => "local and remote branch exist",
                (true, false) => "local branch exists",
                (false, true) => "remote branch exists",
                (false, false) => "open PR exists",
            };
            RetryPreflight::Surviving {
                branch_state: state.into(),
                pr_number: open_pr,
            }
        } else {
            RetryPreflight::NothingFound
        }
    });
    match tokio::time::timeout(RETRY_PREFLIGHT_TIMEOUT, preflight).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            tracing::warn!(task = %task.task_key, "board: retry preflight failed: {error}");
            RetryPreflight::NothingFound
        }
        Err(_) => {
            tracing::warn!(
                task = %task.task_key,
                timeout_secs = RETRY_PREFLIGHT_TIMEOUT.as_secs(),
                "board: retry preflight timed out; continuing best-effort"
            );
            RetryPreflight::NothingFound
        }
    }
}

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
    if has_delivery_evidence(trimmed) {
        BoardTaskOutcome::Success
    } else {
        BoardTaskOutcome::Failed
    }
}

/// Require evidence that the worker delivered, rather than merely promising to
/// start. New workers emit `DELIVERED:`; legacy/free-form completion summaries
/// remain valid unless the message is shaped like future intent. Thus terse
/// reports such as "Fixed the parser." still pass.
fn has_delivery_evidence(output: &str) -> bool {
    let explicit_delivery = output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| {
            line.trim_start()
                .trim_start_matches("**")
                .to_ascii_uppercase()
                .starts_with("DELIVERED:")
        });

    // Normalize common typography produced by rich-text clients before
    // matching progress-only replies. Without this, `I’ll` and an em dash
    // after an acknowledgement bypass the ASCII-oriented intent checks.
    let normalized = output
        .trim()
        .to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{2013}', '\u{2014}'], "-");
    let mut progress = normalized.as_str();
    // Longest phrases first so `sure thing` is removed as one acknowledgement
    // rather than leaving `thing, ...` in front of a future-only reply.
    loop {
        let mut stripped = false;
        for prefix in [
            "acknowledged",
            "sounds good",
            "sure thing",
            "of course",
            "absolutely",
            "certainly",
            "understood",
            "got it",
            "okay",
            "sure",
            "ok",
        ] {
            if let Some(rest) = progress.strip_prefix(prefix) {
                let boundary = match rest.chars().next() {
                    Some(c) => c.is_ascii_punctuation() || c.is_whitespace(),
                    None => true,
                };
                if boundary {
                    progress = rest.trim_start_matches(|c: char| {
                        c.is_ascii_punctuation() || c.is_whitespace()
                    });
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    if progress.is_empty() {
        return false;
    }

    // Check this after stripping an acknowledgement so replies such as
    // `OK, I will mark this complete: ...` retain the documented legacy
    // completion shape.
    if let Some(summary) = progress.strip_prefix("i will mark this complete:") {
        progress = summary.trim_start();
        if progress.is_empty() {
            return false;
        }
    }

    // First-person remaining-work statements are future intent even when an
    // investigative update precedes them (`Found the cause; I'll fix it`).
    let explicit_future_intent = [
        "i'll",
        "i will",
        "we'll",
        "we will",
        "i'm going to",
        "we're going to",
    ]
    .iter()
    .any(|phrase| contains_bounded_phrase(progress, phrase));
    let action_only_prefix = ["start on", "inspect it", "look into", "take a look"]
        .iter()
        .any(|phrase| progress.starts_with(phrase));
    // `Get Started` is often a UI label. Only the standalone/action forms are
    // intent; `Get Started button fixed` remains a valid legacy summary.
    let get_started_intent = progress == "get started"
        || progress.starts_with("get started on ")
        || progress.starts_with("get started with ");

    // Bare imperative prefixes need a word boundary. In particular, do not
    // reject completion summaries such as `Beginning-state reset fixed` or
    // `Work on the parser is complete` merely because their first bytes look
    // like an instruction.
    let begin_intent = progress == "begin" || progress.starts_with("begin ");
    if explicit_future_intent || action_only_prefix || get_started_intent || begin_intent {
        return false;
    }

    // Never let a positive keyword inside a negated/unfinished statement act
    // as delivery evidence (`not fixed`, `not complete yet`, `unfinished`).
    const NON_COMPLETION_MARKERS: [&str; 13] = [
        " not complete",
        " not completed",
        " not done",
        " not fixed",
        " not implemented",
        " not resolved",
        " incomplete",
        " unfinished",
        " still need",
        " need to ",
        " needs to ",
        " continue ",
        " remaining work",
    ];
    if NON_COMPLETION_MARKERS
        .iter()
        .any(|marker| progress.contains(marker))
    {
        return false;
    }

    if explicit_delivery {
        return true;
    }

    // Legacy workers did not yet emit `DELIVERED:`, but still need positive
    // completion evidence. A blacklist-only fallback turns arbitrary chatter
    // (thanks, acknowledgements, partial observations) into Success and can
    // unblock dependent tasks without any delivered work.
    const COMPLETION_PREFIXES: [&str; 22] = [
        "added",
        "analyzed",
        "changed",
        "completed",
        "confirmed",
        "created",
        "documented",
        "done",
        "finished",
        "fixed",
        "found",
        "implemented",
        "merged",
        "proved",
        "pushed",
        "refactored",
        "removed",
        "repaired",
        "resolved",
        "reviewed",
        "updated",
        "verified",
    ];
    const COMPLETION_MARKERS: [&str; 18] = [
        " is complete",
        " are complete",
        " was completed",
        " were completed",
        " has been completed",
        " have been completed",
        " test passes",
        " tests pass",
        " build passes",
        " build succeeded",
        " pr merged",
        " commit pushed",
        " fixed",
        " implemented",
        " resolved",
        " updated",
        " verified with",
        " verified by",
    ];
    COMPLETION_PREFIXES
        .iter()
        .any(|prefix| progress.starts_with(prefix))
        || COMPLETION_MARKERS
            .iter()
            .any(|marker| progress.contains(marker))
}

fn contains_bounded_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(start, matched)| {
        let before_ok = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        let end = start + matched.len();
        let after_ok = end == text.len()
            || text[end..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        before_ok && after_ok
    })
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

/// The standing contract appended to every worker prompt. When the task
/// declares acceptance criteria / a verification command, they are delivered
/// here as the authoritative success condition: acceptance is judged on
/// whether the result satisfies them, not on whether the worker followed the
/// prompt's suggested approach — so the weakest spec that passes verification
/// is always an acceptable delivery.
fn worker_contract(task: &BoardTask) -> String {
    let mut contract = format!(
        "\n\n---\n[task-board contract] You are the worker for task `{key}` (\"{title}\") \
         of boss mission {boss}.\n\
         - Work autonomously until the success condition in the task is met and verified.\n\
         - Do NOT end your turn to report progress; partial updates are wasted.\n\
         - End your turn ONLY when: (a) the task is done and verified — finish with a line \
         starting `DELIVERED:` followed by a short summary of what changed and how you \
         verified it; or (b) you are genuinely stuck — \
         finish with a line starting `BLOCKED:` plus the obstacle, what you tried, and ONE \
         specific question.\n\
         - Never widen scope beyond the task.",
        key = task.task_key,
        title = task.title,
        boss = task.boss_mission_id,
    );
    let verification = task
        .verification_command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let criteria: Vec<&str> = task
        .acceptance_criteria
        .iter()
        .map(|criterion| criterion.trim())
        .filter(|criterion| !criterion.is_empty())
        .collect();
    if !criteria.is_empty() {
        contract
            .push_str("\n- Acceptance criteria (ALL must hold; this is the success condition):");
        for criterion in criteria {
            contract.push_str("\n  * ");
            contract.push_str(criterion);
        }
    }
    if let Some(command) = verification {
        contract.push_str("\n- Verification command (must pass before DELIVERED): `");
        contract.push_str(command);
        contract.push('`');
    }
    if has_outcome_contract(task) {
        contract.push_str(
            "\n- The criteria/verification above define success. Any approach that satisfies \
             them is acceptable — the prompt's suggested approach is advisory; prefer the \
             simplest solution that passes.",
        );
    }
    contract
}

/// Dependency keys referenced by pending tasks that do not exist on the
/// board. `ready_tasks` blocks such tasks forever by design (a typo'd key
/// must never silently pass), but that parking must be *visible*: these feed
/// `board_needs_attention` so the boss is woken to fix its plan instead of
/// the board sitting idle until someone notices in the UI.
pub fn unresolvable_dependencies(tasks: &[BoardTask]) -> Vec<(String, String)> {
    let known: HashSet<&str> = tasks.iter().map(|t| t.task_key.as_str()).collect();
    tasks
        .iter()
        .filter(|t| t.status == BoardTaskStatus::Pending)
        .flat_map(|t| {
            t.depends_on
                .iter()
                .filter(|dep| !known.contains(dep.as_str()))
                .map(|dep| (t.task_key.clone(), dep.clone()))
        })
        .collect()
}

/// True when a board has at least one task needing a boss decision — a
/// settled task awaiting a verdict, a task that exhausted its retries and
/// failed, or a pending task parked forever on a dependency key that doesn't
/// exist on the board. This is the wake trigger.
fn board_needs_attention(tasks: &[BoardTask]) -> bool {
    tasks
        .iter()
        .any(|t| matches!(t.status, BoardTaskStatus::Settled | BoardTaskStatus::Failed))
        || !unresolvable_dependencies(tasks).is_empty()
}

/// Stable revision for a controller wake. Board state alone is insufficient:
/// an acknowledged outbox row remains durable after the boss consumes it. If
/// the boss turn does not mutate any task, reusing the same task-only key would
/// make the next wake look acknowledged without actually queueing a message.
///
/// Mission history advances when the boss starts consuming the wake, so it
/// distinguishes successive controller turns while preserving the same key
/// across retries/restarts before consumption.
fn board_wake_revision(tasks: &[BoardTask], history: &[MissionHistoryEntry]) -> Uuid {
    let latest_task_update = tasks
        .iter()
        .map(|task| task.updated_at.as_str())
        .max()
        .unwrap_or("empty");
    let latest_history = history.last();
    let revision_material = format!(
        "{latest_task_update}\0{}\0{}\0{}",
        history.len(),
        latest_history
            .map(|entry| entry.role.as_str())
            .unwrap_or(""),
        latest_history
            .map(|entry| entry.content.as_str())
            .unwrap_or("")
    );
    Uuid::new_v5(&Uuid::NAMESPACE_OID, revision_material.as_bytes())
}

/// Generic, content-free wake delivered to a boss when its board changes.
/// Deliberately mentions NO specific task or other board — the boss reacts to
/// its OWN board state. If this ever reaches the wrong mission, that mission
/// finds nothing to act on and simply ends its turn, so a misroute can't leak
/// one board's work into another mission.
const BOARD_WAKE_PROMPT: &str = "[task-board] Your task board changed — one or more tasks \
    settled, failed, need a decision, or are parked on an unresolvable depends_on key. Call \
    board_status now and act on YOUR board only: judge each settled task with accept_task / \
    reject_task (review_task for detail), merge_branch finished worktree branches, fix any \
    task listed under unresolvable_deps by re-registering it via plan_tasks with corrected \
    depends_on, and plan_tasks for newly-unblocked or follow-up work. Scheduling, retries, \
    and worker dispatch are automatic — never wait or poll. If board_status shows nothing \
    needing action, just end your turn.";

pub type BoardOutboxInflight = Arc<std::sync::Mutex<HashSet<Uuid>>>;

/// Dispatch a persisted outbox item into the actor's own command channel.
/// The deterministic message id makes concurrent/startup replay harmless.
/// A background waiter acknowledges the outbox only after the consumer has
/// accepted the command; the scheduler itself must not await its own channel.
fn dispatch_board_outbox_item(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    inflight: &BoardOutboxInflight,
    delivery_id: Uuid,
    idempotency_key: String,
    target_mission_id: Uuid,
    content: String,
) -> bool {
    if !inflight
        .lock()
        .expect("board outbox lock")
        .insert(delivery_id)
    {
        return true;
    }
    let (respond, rx) = oneshot::channel();
    match cmd_tx.try_send(ControlCommand::UserMessage {
        id: delivery_id,
        content,
        agent: None,
        target_mission_id: Some(target_mission_id),
        strict: true,
        source: Some("task-board".to_string()),
        respond,
    }) {
        Ok(()) => {
            let store = Arc::clone(mission_store);
            let inflight = Arc::clone(inflight);
            let release_tx = cmd_tx.clone();
            tokio::spawn(async move {
                match rx.await {
                    Ok(UserMessageAck::Queued | UserMessageAck::Delivered) => {
                        match store.acknowledge_board_outbox(&idempotency_key).await {
                            Ok(()) => {
                                inflight
                                    .lock()
                                    .expect("board outbox lock")
                                    .remove(&delivery_id);
                            }
                            Err(error) => {
                                tracing::warn!(target = %target_mission_id, %idempotency_key,
                                    "board: accepted delivery acknowledgement failed: {error}");
                                inflight
                                    .lock()
                                    .expect("board outbox lock")
                                    .remove(&delivery_id);
                            }
                        }
                    }
                    Ok(UserMessageAck::Dropped) => {
                        let _ = release_tx
                            .send(ControlCommand::ReleaseUserMessageId { id: delivery_id })
                            .await;
                        inflight
                            .lock()
                            .expect("board outbox lock")
                            .remove(&delivery_id);
                        tracing::warn!(
                            target = %target_mission_id,
                            %idempotency_key,
                            "board: actor dropped delivery; leaving outbox pending"
                        );
                    }
                    Ok(UserMessageAck::Rejected(reason)) => {
                        let _ = release_tx
                            .send(ControlCommand::ReleaseUserMessageId { id: delivery_id })
                            .await;
                        inflight
                            .lock()
                            .expect("board outbox lock")
                            .remove(&delivery_id);
                        tracing::warn!(
                            target = %target_mission_id,
                            %idempotency_key,
                            %reason,
                            "board: actor rejected delivery; leaving outbox pending"
                        );
                    }
                    Err(error) => {
                        let _ = release_tx
                            .send(ControlCommand::ReleaseUserMessageId { id: delivery_id })
                            .await;
                        inflight
                            .lock()
                            .expect("board outbox lock")
                            .remove(&delivery_id);
                        tracing::warn!(
                            target = %target_mission_id,
                            %idempotency_key,
                            "board: delivery acknowledgement channel closed; leaving outbox pending: {error}"
                        );
                    }
                }
            });
            true
        }
        Err(e) => {
            inflight
                .lock()
                .expect("board outbox lock")
                .remove(&delivery_id);
            tracing::warn!(target = %target_mission_id, %idempotency_key,
                "board: outbox dispatch failed: {e}");
            false
        }
    }
}

fn outbox_payload(item: &BoardOutboxItem) -> Result<(Uuid, String), String> {
    let target = item
        .payload
        .get("target_mission_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing target_mission_id".to_string())
        .and_then(|value| Uuid::parse_str(value).map_err(|error| error.to_string()))?;
    let content = item
        .payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing content".to_string())?
        .to_string();
    Ok((target, content))
}

/// Revalidate a durable delivery against the current board and mission rows.
///
/// Outbox rows survive crashes by design, but the operator may cancel a board
/// or a worker may finish before replay. Re-sending such a stale row through
/// `UserMessage` would reactivate the terminal target. Store read failures keep
/// the row pending for a later pass; conclusive state mismatches retire it.
async fn board_outbox_replay_is_eligible(
    mission_store: &Arc<dyn MissionStore>,
    item: &BoardOutboxItem,
    target: Uuid,
) -> Result<bool, String> {
    let Some(boss) = mission_store.get_mission(item.boss_mission_id).await? else {
        return Ok(false);
    };
    if boss_status_is_terminal(boss.status) || boss.status == MissionStatus::Paused {
        return Ok(false);
    }

    match item.delivery_kind.as_str() {
        "spawn" | "retry" => {
            let Some(task_id) = item.task_id else {
                return Ok(false);
            };
            let Some(task) = mission_store.get_board_task(task_id).await? else {
                return Ok(false);
            };
            if task.boss_mission_id != item.boss_mission_id
                || task.status != BoardTaskStatus::Running
                || task.worker_mission_id != Some(target)
            {
                return Ok(false);
            }
            let current_spawn_key = format!("board:{}:attempt:{}:spawn", task.id, task.attempts);
            let current_retry_key = format!("board:{}:attempt:{}:retry", task.id, task.attempts);
            if item.idempotency_key != current_spawn_key
                && item.idempotency_key != current_retry_key
            {
                return Ok(false);
            }
            let Some(worker) = mission_store.get_mission(target).await? else {
                return Ok(false);
            };
            // A board spawn delivery is the transition out of Pending. Any
            // later presentation/terminal state proves this exact intent is
            // stale or was already consumed before its acknowledgement landed.
            Ok(worker.status == MissionStatus::Pending)
        }
        "controller_notification" => {
            if item.task_id.is_some() || target != item.boss_mission_id {
                return Ok(false);
            }
            let tasks = mission_store.list_board_tasks(item.boss_mission_id).await?;
            let current_key = format!(
                "board:{}:wake:{}",
                item.boss_mission_id,
                board_wake_revision(&tasks, &boss.history)
            );
            Ok(board_needs_attention(&tasks) && item.idempotency_key == current_key)
        }
        _ => Ok(false),
    }
}

async fn replay_pending_board_outbox(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    inflight: &BoardOutboxInflight,
) {
    let pending = match mission_store.list_pending_board_outbox(1000).await {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!("board: failed to load pending outbox: {error}");
            return;
        }
    };
    let mut eligible = Vec::new();
    for item in pending {
        let (target, content) = match outbox_payload(&item) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(idempotency_key = %item.idempotency_key,
                    "board: invalid pending outbox payload: {error}");
                continue;
            }
        };
        match board_outbox_replay_is_eligible(mission_store, &item, target).await {
            Ok(true) => eligible.push((item, target, content)),
            Ok(false) => {
                tracing::info!(
                    target = %target,
                    idempotency_key = %item.idempotency_key,
                    delivery_kind = %item.delivery_kind,
                    "board: retiring stale pending outbox delivery"
                );
                if let Err(error) = mission_store
                    .acknowledge_board_outbox(&item.idempotency_key)
                    .await
                {
                    tracing::warn!(
                        idempotency_key = %item.idempotency_key,
                        "board: failed to retire stale outbox delivery: {error}"
                    );
                }
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    target = %target,
                    idempotency_key = %item.idempotency_key,
                    "board: could not revalidate pending outbox delivery: {error}"
                );
                continue;
            }
        }
    }

    // A zombie sweep may persist `:retry` while the original same-attempt
    // `:spawn` row is still pending. Coalesce before sending anything: the
    // actor cannot consume its own channel until this scheduler pass returns,
    // so dispatching both would queue the task twice.
    let retry_tasks: HashSet<Uuid> = eligible
        .iter()
        .filter(|(item, _, _)| item.idempotency_key.ends_with(":retry"))
        .filter_map(|(item, _, _)| item.task_id)
        .collect();
    for (item, target, content) in eligible {
        if item.idempotency_key.ends_with(":spawn")
            && item
                .task_id
                .is_some_and(|task_id| retry_tasks.contains(&task_id))
        {
            tracing::info!(
                target = %target,
                idempotency_key = %item.idempotency_key,
                "board: retiring spawn delivery superseded by same-attempt retry"
            );
            if let Err(error) = mission_store
                .acknowledge_board_outbox(&item.idempotency_key)
                .await
            {
                tracing::warn!(
                    idempotency_key = %item.idempotency_key,
                    "board: failed to retire superseded spawn delivery: {error}"
                );
            }
            continue;
        }
        if matches!(item.delivery_kind.as_str(), "spawn" | "retry") {
            if let Some(task_id) = item.task_id {
                match mission_store.get_board_task(task_id).await {
                    Ok(Some(task)) => {
                        if let Some(profile_slot) = task
                            .prior_result_digest
                            .as_deref()
                            .and_then(chatgpt_ui_compatibility_profile_slot)
                        {
                            crate::api::runners::chatgpt_ui::profile_pool::exclude_profile_for_mission(
                                target,
                                profile_slot,
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            task = %task_id,
                            target = %target,
                            "board: could not restore ChatGPT UI retry requirement before outbox replay: {error}"
                        );
                        continue;
                    }
                }
            }
        }
        let _ = dispatch_board_outbox_item(
            mission_store,
            cmd_tx,
            inflight,
            item.id,
            item.idempotency_key,
            target,
            content,
        );
    }
}

struct BoardDelivery {
    boss_mission_id: Uuid,
    task_id: Option<Uuid>,
    target_mission_id: Uuid,
    delivery_kind: &'static str,
    idempotency_key: String,
    content: String,
}

async fn durable_self_send(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    inflight: &BoardOutboxInflight,
    delivery: BoardDelivery,
) -> bool {
    let BoardDelivery {
        boss_mission_id,
        task_id,
        target_mission_id,
        delivery_kind,
        idempotency_key,
        content,
    } = delivery;
    if let Some(prefix) = idempotency_key.strip_suffix(":retry") {
        let superseded_spawn_key = format!("{prefix}:spawn");
        if let Err(error) = mission_store
            .acknowledge_board_outbox(&superseded_spawn_key)
            .await
        {
            tracing::warn!(
                target = %target_mission_id,
                %idempotency_key,
                superseded = %superseded_spawn_key,
                "board: refusing retry until its superseded spawn is retired: {error}"
            );
            return false;
        }
    }
    let delivery_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, idempotency_key.as_bytes());
    let item = BoardOutboxItem {
        id: delivery_id,
        boss_mission_id,
        task_id,
        delivery_kind: delivery_kind.to_string(),
        idempotency_key: idempotency_key.clone(),
        payload: serde_json::json!({
            "target_mission_id": target_mission_id,
            "content": content,
        }),
        state: "pending".to_string(),
        attempts: 0,
        created_at: now_string(),
        acknowledged_at: None,
    };
    let persisted = match mission_store.enqueue_board_outbox(item).await {
        Ok(item) => item,
        Err(error) => {
            tracing::warn!(target = %target_mission_id, %idempotency_key,
                "board: refusing unjournaled delivery: {error}");
            return false;
        }
    };
    if persisted.state == "acknowledged" {
        return true;
    }
    let (persisted_target, persisted_content) = match outbox_payload(&persisted) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(target = %target_mission_id, %idempotency_key,
                "board: invalid persisted outbox payload: {error}");
            return false;
        }
    };
    dispatch_board_outbox_item(
        mission_store,
        cmd_tx,
        inflight,
        persisted.id,
        persisted.idempotency_key,
        persisted_target,
        persisted_content,
    )
}

/// Fire-and-forget cancel of a specific (worker) mission's runner. Mirrors
/// [`self_send_message`]: try_send only, receiver dropped — the scheduler runs
/// on the task that consumes this channel and must never await it.
fn self_cancel_mission(cmd_tx: &mpsc::Sender<ControlCommand>, mission_id: Uuid) -> bool {
    let (respond, _rx) = oneshot::channel();
    match cmd_tx.try_send(ControlCommand::CancelMission {
        mission_id,
        min_idle: None,
        respond,
    }) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(target = %mission_id, "board: cancel-worker send failed: {}", e);
            false
        }
    }
}

/// Boss-mission statuses that mean the board can no longer be driven: the boss
/// will never run another turn to deliver verdicts or read a wake, so its board
/// must stop scheduling. `Active`/`Pending` are live; `AwaitingUser` and
/// `Acknowledged` are the NORMAL idle states a boss parks in between board
/// wakes, so they are deliberately NOT terminal here.
fn boss_status_is_terminal(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Completed
            | MissionStatus::Failed
            | MissionStatus::Interrupted
            | MissionStatus::Blocked
            | MissionStatus::NotFeasible
    )
}

/// Tear down the board of a boss mission that has terminated: cancel every
/// non-terminal task (and stop any live worker) so the scheduler stops reviving
/// work the boss can never judge. Once all tasks are terminal the boss drops out
/// of `list_active_board_missions` on the next pass.
async fn cancel_dead_boss_board(
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    boss_id: Uuid,
    tasks: &[BoardTask],
) {
    let mut cancelled = 0u32;
    for task in tasks.iter().filter(|t| !t.status.is_terminal()) {
        if let Some(worker_id) = task.worker_mission_id {
            self_cancel_mission(cmd_tx, worker_id);
        }
        let mut t = task.clone();
        t.status = BoardTaskStatus::Cancelled;
        t.notes = append_note(&t.notes, "cancelled: boss mission terminated");
        match mission_store.save_board_task(&t).await {
            Ok(()) => cancelled += 1,
            Err(e) => tracing::warn!(
                boss = %boss_id, task = %t.task_key,
                "board: failed to cancel orphaned task: {}", e
            ),
        }
    }
    if cancelled > 0 {
        tracing::info!(boss = %boss_id, cancelled,
            "board: boss mission terminated — cancelled orphaned board tasks");
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
    control_hub: Option<&super::ControlHub>,
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    snapshot: &RunnerSnapshot,
    max_parallel: usize,
    // Per-boss "a wake is outstanding" flag, owned by the actor loop. Coalesces
    // wakes: at most one pending wake per boss until it next runs (consuming it).
    wake_state: &mut HashMap<Uuid, bool>,
    // Rows already handed to the actor in this process. Pending rows are
    // replayed after restart, but not on every scheduler tick while an
    // acknowledgement is still in flight.
    outbox_inflight: &BoardOutboxInflight,
) {
    // Re-drive every durable intent before inspecting live board state. The
    // deterministic command id prevents duplicate execution if a prior send
    // is still in the channel; an item remains pending until the actor sends a
    // queued/delivered acknowledgement.
    replay_pending_board_outbox(mission_store, cmd_tx, outbox_inflight).await;
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

        // --- Dead-boss teardown: `list_active_board_missions` keys only on task
        // status, so a boss whose own mission has terminated (failed, completed,
        // interrupted, …) with tasks still in flight would otherwise keep
        // getting workers re-spawned and wake banners it can never act on.
        // Cancel its non-terminal tasks (+ live workers) and skip; next pass the
        // boss drops out entirely.
        let boss = match mission_store.get_mission(boss_id).await {
            Ok(boss) => boss,
            Err(error) => {
                tracing::warn!(boss = %boss_id,
                    "board: failed to load boss mission for scheduling: {error}");
                None
            }
        };
        if let Some(boss) = boss.as_ref() {
            if boss_status_is_terminal(boss.status) {
                cancel_dead_boss_board(mission_store, cmd_tx, boss_id, &tasks).await;
                wake_state.remove(&boss_id);
                continue;
            }
        }

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
                        if let Some(profile_slot) = task
                            .prior_result_digest
                            .as_deref()
                            .and_then(chatgpt_ui_compatibility_profile_slot)
                        {
                            crate::api::runners::chatgpt_ui::profile_pool::exclude_profile_for_mission(
                                worker_id,
                                profile_slot,
                            );
                        }
                        let prompt = format!(
                            "{}{}",
                            retry_prompt(task, &RetryPreflight::NothingFound),
                            worker_contract(task)
                        );
                        if durable_self_send(
                            mission_store,
                            cmd_tx,
                            outbox_inflight,
                            BoardDelivery {
                                boss_mission_id: boss_id,
                                task_id: Some(task.id),
                                target_mission_id: worker_id,
                                delivery_kind: "retry",
                                idempotency_key: format!(
                                    "board:{}:attempt:{}:retry",
                                    task.id, task.attempts
                                ),
                                content: prompt,
                            },
                        )
                        .await
                        {
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
                        persisted_terminal_reason(worker.terminal_reason.as_deref()),
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
                    settle_task(
                        mission_store,
                        task.clone(),
                        BoardTaskOutcome::Failed,
                        &last,
                        persisted_terminal_reason(worker.terminal_reason.as_deref()),
                    )
                    .await;
                }
                MissionStatus::Active => {
                    // Runner may exist in another control session or be mid-start;
                    // leave it alone.
                }
                MissionStatus::WaitingBackground => {
                    // Worker parked on live background jobs; the auto-resume
                    // watcher will wake it when they finish. Not settled — leave it.
                }
                MissionStatus::Paused => {
                    // Operator-paused worker: leave it alone, the dispatcher will
                    // resume it when unpaused.
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
                    let preflight = if task.attempts > 0 {
                        retry_preflight(&task).await
                    } else {
                        RetryPreflight::NothingFound
                    };
                    if retry_disposition(&task, &preflight) == RetryDisposition::ParkForBossReview {
                        let RetryPreflight::Merged { pr_number } = preflight else {
                            unreachable!();
                        };
                        let mut parked = task.clone();
                        parked.status = BoardTaskStatus::Settled;
                        parked.outcome = Some(BoardTaskOutcome::Blocked);
                        parked.result_digest = Some(format!(
                            "Retry suppressed: declared branch was already merged in PR #{pr_number}; boss review required."
                        ));
                        parked.notes = append_note(
                            &parked.notes,
                            &format!("auto-respawn parked: PR #{pr_number} is merged"),
                        );
                        if let Err(error) = mission_store.save_board_task(&parked).await {
                            tracing::warn!(task = %task.task_key,
                                "board: failed to park merged retry: {error}");
                        }
                        continue;
                    }
                    match spawn_task_worker(
                        control_hub,
                        mission_store,
                        cmd_tx,
                        outbox_inflight,
                        &task,
                        boss.workspace_id,
                        &preflight,
                    )
                    .await
                    {
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
        } else if !wake_state.get(&boss_id).copied().unwrap_or(false) {
            if let Some(boss) = boss.as_ref() {
                if durable_self_send(
                    mission_store,
                    cmd_tx,
                    outbox_inflight,
                    BoardDelivery {
                        boss_mission_id: boss_id,
                        task_id: None,
                        target_mission_id: boss_id,
                        delivery_kind: "controller_notification",
                        idempotency_key: format!(
                            "board:{boss_id}:wake:{}",
                            board_wake_revision(&fresh, &boss.history)
                        ),
                        content: BOARD_WAKE_PROMPT.to_string(),
                    },
                )
                .await
                {
                    wake_state.insert(boss_id, true);
                    tracing::info!(boss = %boss_id, "board: sent wake (tasks awaiting decision)");
                }
            }
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
    control_hub: Option<&super::ControlHub>,
    mission_store: &Arc<dyn MissionStore>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
    outbox_inflight: &BoardOutboxInflight,
    task: &BoardTask,
    workspace_id: Uuid,
    preflight: &RetryPreflight,
) -> Result<Uuid, String> {
    // Board tasks bypass the public create-mission handler, so apply the same
    // backend-aware normalization here as a defensive migration for tasks that
    // were persisted before upsert started normalizing them.
    let requested_model = task
        .model_override
        .as_deref()
        .or_else(|| role_default_model(task));
    let model_override = requested_model
        .and_then(|model| super::normalize_model_override_for_backend(Some(&task.backend), model));
    // Board workers are local missions just like REST/Ask-created workers.
    // Keep the admission lock over both mission persistence and the trusted
    // ledger write: otherwise two schedulers can each observe the same free
    // bytes and overcommit before either worker is visible to reconstruction.
    // Production callers always supply the hub. `None` exists solely for
    // focused in-memory unit tests that exercise board metadata without a
    // filesystem-backed control server; it is private to this module and
    // cannot be reached by an API caller.
    let admission = if let Some(control_hub) = control_hub {
        let workspace = crate::workspace::resolve_workspace(
            &control_hub.workspaces,
            &control_hub.config,
            Some(workspace_id),
        )
        .await;
        let (guard, reservation) = super::reserve_local_mission_disk(
            control_hub,
            &control_hub.config,
            &workspace,
            super::mission_disk_default_estimate_gib(),
        )
        .await?;
        Some((guard, reservation, workspace))
    } else {
        None
    };
    let mission = mission_store
        .create_mission_with_parent(
            Some(&format!("[{}] {}", task.task_key, task.title)),
            Some(workspace_id),
            None,
            model_override.as_deref(),
            task.model_effort.as_deref(),
            false,
            Some(&task.backend),
            None,
            Some(task.boss_mission_id),
            task.working_directory.as_deref(),
        )
        .await?;
    if let Some((admission_guard, mut reservation, workspace)) = admission {
        reservation.mission_id = mission.id;
        reservation.workspace_dir = Some(crate::workspace::mission_workspace_dir_for_workspace(
            &workspace, mission.id,
        ));
        let mut ledger =
            super::read_disk_reservation_ledger(&control_hub.expect("admission hub").config)?;
        ledger.reservations.insert(mission.id, reservation);
        if let Err(error) = super::write_disk_reservation_ledger(
            &control_hub.expect("admission hub").config,
            &ledger,
        ) {
            let _ = mission_store
                .update_mission_status(mission.id, MissionStatus::Failed)
                .await;
            return Err(format!(
                "board worker creation rolled back: persist disk admission ledger: {error}"
            ));
        }
        drop(admission_guard);
    }
    // Inherit the boss's project tagging. Board tasks bypass the public
    // create-mission handler, and `create_mission_with_parent` carries no
    // project metadata — so workers were landing untagged. The parent link
    // alone does not group them: an untagged worker is invisible in the
    // per-project inventory the board and every controller reconcile against,
    // which is exactly where a campaign's own work needs to be visible.
    // The task's own key becomes the track when the boss has none, so sibling
    // workers stay distinguishable.
    if let Ok(Some(boss)) = mission_store.get_mission(task.boss_mission_id).await {
        if boss.project.project.is_some() {
            let patch = MissionProjectPatch {
                project: Some(boss.project.project.clone()),
                track: Some(
                    boss.project
                        .track
                        .clone()
                        .or_else(|| Some(task.task_key.clone())),
                ),
                intent: Some(boss.project.intent.clone()),
                ..Default::default()
            };
            if let Err(error) = mission_store
                .update_mission_project(mission.id, patch)
                .await
            {
                tracing::warn!(
                    mission = %mission.id,
                    boss = %task.boss_mission_id,
                    "board: could not inherit project tagging: {error}"
                );
            }
        }
    }

    if let Some(profile_slot) = task
        .prior_result_digest
        .as_deref()
        .and_then(chatgpt_ui_compatibility_profile_slot)
    {
        crate::api::runners::chatgpt_ui::profile_pool::exclude_profile_for_mission(
            mission.id,
            profile_slot,
        );
    }

    let mut t = task.clone();
    t.model_override = model_override;
    t.worker_mission_id = Some(mission.id);
    t.status = BoardTaskStatus::Running;
    t.attempts += 1;
    if t.attempts > 1 {
        t.notes = append_note(&t.notes, &format!("retry: attempt {}", t.attempts));
    }
    mission_store.save_board_task(&t).await?;
    mission_store
        .create_task_attempt(TaskAttempt {
            id: Uuid::new_v4(),
            task_id: t.id,
            attempt_number: t.attempts,
            mission_id: mission.id,
            backend: t.backend.clone(),
            model: t.model_override.clone(),
            role: t.role,
            run_id: None,
            commit_sha: None,
            changed_files: vec![],
            verification_evidence: serde_json::json!({}),
            cost_cents: None,
            terminal_class: None,
            started_at: now_string(),
            finished_at: None,
        })
        .await?;

    let prompt = format!("{}{}", retry_prompt(&t, preflight), worker_contract(&t));
    let delivery_key = format!("board:{}:attempt:{}:spawn", t.id, t.attempts);
    if !durable_self_send(
        mission_store,
        cmd_tx,
        outbox_inflight,
        BoardDelivery {
            boss_mission_id: t.boss_mission_id,
            task_id: Some(t.id),
            target_mission_id: mission.id,
            delivery_kind: if t.attempts > 1 { "retry" } else { "spawn" },
            idempotency_key: delivery_key,
            content: prompt,
        },
    )
    .await
    {
        // Channel full: leave the task running; the zombie sweep re-kicks the
        // pending worker mission after the grace period.
        tracing::warn!(task = %t.task_key, "board: spawn message deferred (channel full)");
    }
    Ok(mission.id)
}

/// Whether a failed settle re-queues silently for its one automatic retry.
/// High-risk tasks never retry silently: a failed high-risk attempt is a boss
/// decision, not a scheduler decision — the board wake surfaces it instead.
fn eligible_for_automatic_retry(
    task: &BoardTask,
    outcome: BoardTaskOutcome,
    retry: AutomaticRetry,
) -> bool {
    outcome == BoardTaskOutcome::Failed
        && task.attempts < MAX_ATTEMPTS
        && retry != AutomaticRetry::Suppressed
        && !task.risk_class.eq_ignore_ascii_case("high")
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
    terminal_reason: Option<TerminalReason>,
) {
    let retry = automatic_retry(&task, terminal_reason, output);
    let terminal_class = match terminal_reason {
        Some(TerminalReason::AuthError) => "auth",
        Some(TerminalReason::RateLimited) => "rate_limited",
        _ if matches!(retry, AutomaticRetry::DifferentChatGptUiProfile(_)) => "compatibility",
        _ => match outcome {
            BoardTaskOutcome::Success => "success",
            BoardTaskOutcome::Blocked => "blocked",
            BoardTaskOutcome::Failed => "agent_failure",
        },
    };
    let evidence = serde_json::json!({
        "result_digest": digest_excerpt(output),
        "verification_command": task.verification_command.clone(),
    });
    if let Err(error) = mission_store
        .finish_task_attempt(
            task.id,
            task.attempts,
            terminal_class,
            None,
            &[],
            &evidence,
            None,
        )
        .await
    {
        tracing::warn!(task = %task.task_key, "board: failed to close task attempt: {error}");
    }
    let high_risk = task.risk_class.eq_ignore_ascii_case("high");
    if eligible_for_automatic_retry(&task, outcome, retry) {
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
        task.prior_worker_mission_id = task.worker_mission_id;
        task.prior_outcome = Some(outcome);
        task.prior_result_digest = Some(digest_excerpt(output));
        task.worker_mission_id = None;
        if let Err(e) = mission_store.save_board_task(&task).await {
            tracing::warn!(task = %task.task_key, "board: failed to persist retry: {}", e);
        }
        return;
    }

    if outcome == BoardTaskOutcome::Failed && retry == AutomaticRetry::Suppressed {
        let reason = match terminal_reason {
            Some(TerminalReason::AuthError) => {
                "authentication failure requires operator reauthentication; automatic retry suppressed"
            }
            Some(TerminalReason::RateLimited) => {
                "rate limit requires allowance recovery; automatic retry suppressed"
            }
            _ => "policy suppressed automatic retry",
        };
        task.notes = append_note(&task.notes, reason);
    } else if outcome == BoardTaskOutcome::Failed && high_risk && task.attempts < MAX_ATTEMPTS {
        task.notes = append_note(
            &task.notes,
            "risk_class=high: automatic retry suppressed; boss review required",
        );
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
    settle_task(mission_store, task, outcome, output, terminal_reason).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::{InMemoryMissionStore, NewBoardTask};

    #[tokio::test]
    async fn durable_delivery_stays_pending_until_actor_acknowledges() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let boss = Uuid::new_v4();
        let key = format!("board:{boss}:wake:test");

        assert!(
            durable_self_send(
                &store,
                &cmd_tx,
                &inflight,
                BoardDelivery {
                    boss_mission_id: boss,
                    task_id: None,
                    target_mission_id: boss,
                    delivery_kind: "controller_notification",
                    idempotency_key: key.clone(),
                    content: "wake".to_string(),
                },
            )
            .await
        );
        assert_eq!(store.list_pending_board_outbox(10).await.unwrap().len(), 1);

        let command = cmd_rx.recv().await.expect("board command");
        let ControlCommand::UserMessage {
            id,
            source,
            respond,
            ..
        } = command
        else {
            panic!("expected user message");
        };
        assert_eq!(id, Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()));
        assert_eq!(source.as_deref(), Some("task-board"));
        respond
            .send(UserMessageAck::Delivered)
            .expect("ack receiver alive");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if store
                    .list_pending_board_outbox(10)
                    .await
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("outbox acknowledgement persisted");
        assert!(inflight.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn task_workers_inherit_the_boss_project_tagging() {
        // An untagged worker is invisible in the per-project inventory the
        // board and every controller reconcile against — a campaign's own
        // work must not vanish from its project.
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        store
            .update_mission_project(
                boss.id,
                MissionProjectPatch {
                    project: Some(Some("verity-benchmark".into())),
                    intent: Some(Some("evaluate".into())),
                    ..Default::default()
                },
            )
            .await
            .expect("tag boss");
        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "fast-verdict".into(),
                    title: "Obtain verdicts".into(),
                    prompt: "p".into(),
                    backend: "codex".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("create task");
        let task = store.list_board_tasks(boss.id).await.expect("list")[0].clone();

        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let inflight: BoardOutboxInflight = Default::default();
        let worker_id = spawn_task_worker(
            None,
            &store,
            &cmd_tx,
            &inflight,
            &task,
            Uuid::new_v4(),
            &RetryPreflight::NothingFound,
        )
        .await
        .expect("spawn worker");

        let worker = store
            .get_mission(worker_id)
            .await
            .expect("load")
            .expect("worker exists");
        assert_eq!(worker.project.project.as_deref(), Some("verity-benchmark"));
        assert_eq!(worker.project.intent.as_deref(), Some("evaluate"));
        // The boss has no track, so the task key keeps siblings apart.
        assert_eq!(worker.project.track.as_deref(), Some("fast-verdict"));
    }

    #[tokio::test]
    async fn unresolvable_dependencies_are_visible_and_trigger_attention() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        store
            .upsert_board_tasks(
                boss.id,
                vec![
                    NewBoardTask {
                        task_key: "a".into(),
                        title: "a".into(),
                        prompt: "p".into(),
                        backend: "codex".into(),
                        ..Default::default()
                    },
                    NewBoardTask {
                        task_key: "b".into(),
                        title: "b".into(),
                        prompt: "p".into(),
                        backend: "codex".into(),
                        depends_on: vec!["a".into(), "typo-key".into()],
                        ..Default::default()
                    },
                ],
            )
            .await
            .expect("create tasks");
        let tasks = store.list_board_tasks(boss.id).await.expect("list tasks");

        // `b` is parked forever (ready_tasks never returns it) …
        assert!(ready_tasks(&tasks).iter().all(|t| t.task_key != "b"));
        // … so the parking must be visible and wake the boss.
        assert_eq!(
            unresolvable_dependencies(&tasks),
            vec![("b".to_string(), "typo-key".to_string())]
        );
        assert!(board_needs_attention(&tasks));

        // A board whose deps all resolve raises no attention.
        let resolved: Vec<BoardTask> = tasks
            .into_iter()
            .map(|mut t| {
                t.depends_on.retain(|d| d != "typo-key");
                t
            })
            .collect();
        assert!(unresolvable_dependencies(&resolved).is_empty());
        assert!(!board_needs_attention(&resolved));
    }

    #[tokio::test]
    async fn running_task_outcome_contract_can_still_be_corrected() {
        // spec_warnings arrive after registration and the scheduler can spawn
        // within one pass — so re-registering the same task_key must land the
        // corrected contract on a RUNNING task (contract fields only; the
        // in-flight prompt stays frozen).
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "t".into(),
                    title: "t".into(),
                    prompt: "original prompt".into(),
                    backend: "codex".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("register");
        let mut task = store
            .list_board_tasks(boss.id)
            .await
            .expect("list")
            .remove(0);
        task.status = BoardTaskStatus::Running;
        store.save_board_task(&task).await.expect("mark running");

        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "t".into(),
                    title: "ignored".into(),
                    prompt: "ignored".into(),
                    backend: "codex".into(),
                    acceptance_criteria: vec!["tests pass".into()],
                    verification_command: Some("cargo test".into()),
                    risk_class: "high".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("correct contract");

        let corrected = store
            .list_board_tasks(boss.id)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(corrected.status, BoardTaskStatus::Running);
        assert_eq!(corrected.prompt, "original prompt");
        assert_eq!(
            corrected.acceptance_criteria,
            vec!["tests pass".to_string()]
        );
        assert_eq!(
            corrected.verification_command.as_deref(),
            Some("cargo test")
        );
        assert_eq!(corrected.risk_class, "high");
    }

    #[tokio::test]
    async fn dropped_delivery_is_released_for_pending_replay() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let boss = Uuid::new_v4();

        assert!(
            durable_self_send(
                &store,
                &cmd_tx,
                &inflight,
                BoardDelivery {
                    boss_mission_id: boss,
                    task_id: None,
                    target_mission_id: boss,
                    delivery_kind: "controller_notification",
                    idempotency_key: format!("board:{boss}:wake:dropped"),
                    content: "wake".to_string(),
                },
            )
            .await
        );
        let ControlCommand::UserMessage { respond, .. } =
            cmd_rx.recv().await.expect("board command")
        else {
            panic!("expected user message");
        };
        respond
            .send(UserMessageAck::Dropped)
            .expect("ack receiver alive");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if inflight.lock().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped delivery released");
        assert!(matches!(
            cmd_rx.recv().await,
            Some(ControlCommand::ReleaseUserMessageId { .. })
        ));
        assert_eq!(store.list_pending_board_outbox(10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replay_retires_wake_for_terminal_boss_without_dispatching() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("terminal boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "review".into(),
                    title: "review".into(),
                    prompt: "p".into(),
                    backend: "codex".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("create task");
        let mut task = store
            .list_board_tasks(boss.id)
            .await
            .expect("list task")
            .remove(0);
        task.status = BoardTaskStatus::Settled;
        store.save_board_task(&task).await.expect("settle task");

        let key = format!("board:{}:wake:stale", boss.id);
        store
            .enqueue_board_outbox(BoardOutboxItem {
                id: Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()),
                boss_mission_id: boss.id,
                task_id: None,
                delivery_kind: "controller_notification".into(),
                idempotency_key: key,
                payload: serde_json::json!({
                    "target_mission_id": boss.id,
                    "content": BOARD_WAKE_PROMPT,
                }),
                state: "pending".into(),
                attempts: 0,
                created_at: now_string(),
                acknowledged_at: None,
            })
            .await
            .expect("enqueue wake");
        store
            .update_mission_status(boss.id, MissionStatus::Interrupted)
            .await
            .expect("cancel boss");

        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        replay_pending_board_outbox(&store, &cmd_tx, &inflight).await;

        assert!(
            cmd_rx.try_recv().is_err(),
            "terminal boss must not be woken"
        );
        assert!(store
            .list_pending_board_outbox(10)
            .await
            .expect("pending outbox")
            .is_empty());
    }

    #[tokio::test]
    async fn replay_retires_spawn_for_acknowledged_worker_without_dispatching() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("live boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        let worker = store
            .create_mission_with_parent(
                Some("worker"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                Some(boss.id),
                None,
            )
            .await
            .expect("create worker");
        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "worker".into(),
                    title: "worker".into(),
                    prompt: "p".into(),
                    backend: "codex".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("create task");
        let mut task = store
            .list_board_tasks(boss.id)
            .await
            .expect("list task")
            .remove(0);
        task.status = BoardTaskStatus::Running;
        task.worker_mission_id = Some(worker.id);
        task.attempts = 1;
        store
            .save_board_task(&task)
            .await
            .expect("save running task");
        store
            .update_mission_status(worker.id, MissionStatus::Acknowledged)
            .await
            .expect("acknowledge worker");

        let key = format!("board:{}:attempt:1:spawn", task.id);
        store
            .enqueue_board_outbox(BoardOutboxItem {
                id: Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()),
                boss_mission_id: boss.id,
                task_id: Some(task.id),
                delivery_kind: "spawn".into(),
                idempotency_key: key,
                payload: serde_json::json!({
                    "target_mission_id": worker.id,
                    "content": "do work",
                }),
                state: "pending".into(),
                attempts: 0,
                created_at: now_string(),
                acknowledged_at: None,
            })
            .await
            .expect("enqueue spawn");

        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        replay_pending_board_outbox(&store, &cmd_tx, &inflight).await;

        assert!(
            cmd_rx.try_recv().is_err(),
            "acknowledged worker must not be reactivated"
        );
        assert!(store
            .list_pending_board_outbox(10)
            .await
            .expect("pending outbox")
            .is_empty());
    }

    #[tokio::test]
    async fn replay_coalesces_same_attempt_spawn_and_retry() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("live boss"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        let worker = store
            .create_mission_with_parent(
                Some("pending worker"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                Some(boss.id),
                None,
            )
            .await
            .expect("create worker");
        store
            .upsert_board_tasks(
                boss.id,
                vec![NewBoardTask {
                    task_key: "worker".into(),
                    title: "worker".into(),
                    prompt: "p".into(),
                    backend: "codex".into(),
                    ..Default::default()
                }],
            )
            .await
            .expect("create task");
        let mut task = store
            .list_board_tasks(boss.id)
            .await
            .expect("list task")
            .remove(0);
        task.backend = "chatgpt_ui".into();
        task.status = BoardTaskStatus::Running;
        task.worker_mission_id = Some(worker.id);
        task.prior_result_digest = Some(
            "chatgpt_ui: compatibility=chatgpt-ui-v2; profile_slot=4; selector mismatch".into(),
        );
        task.attempts = 1;
        store
            .save_board_task(&task)
            .await
            .expect("save running task");

        let spawn_key = format!("board:{}:attempt:1:spawn", task.id);
        let retry_key = format!("board:{}:attempt:1:retry", task.id);
        for (key, kind, content) in [
            (&spawn_key, "spawn", "initial"),
            (&retry_key, "retry", "re-kick"),
        ] {
            store
                .enqueue_board_outbox(BoardOutboxItem {
                    id: Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()),
                    boss_mission_id: boss.id,
                    task_id: Some(task.id),
                    delivery_kind: kind.into(),
                    idempotency_key: key.clone(),
                    payload: serde_json::json!({
                        "target_mission_id": worker.id,
                        "content": content,
                    }),
                    state: "pending".into(),
                    attempts: 0,
                    created_at: now_string(),
                    acknowledged_at: None,
                })
                .await
                .expect("enqueue delivery");
        }

        let (cmd_tx, mut cmd_rx) = mpsc::channel(2);
        let inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));
        replay_pending_board_outbox(&store, &cmd_tx, &inflight).await;

        let ControlCommand::UserMessage { id, content, .. } =
            cmd_rx.try_recv().expect("retry should dispatch")
        else {
            panic!("expected user message");
        };
        assert_eq!(id, Uuid::new_v5(&Uuid::NAMESPACE_OID, retry_key.as_bytes()));
        assert_eq!(content, "re-kick");
        assert!(
            cmd_rx.try_recv().is_err(),
            "superseded spawn must not also dispatch"
        );
        let pending = store
            .list_pending_board_outbox(10)
            .await
            .expect("pending outbox");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].idempotency_key, retry_key);
        assert_eq!(
            crate::api::runners::chatgpt_ui::profile_pool::take_excluded_profile_for_tests(
                worker.id
            ),
            Some(4),
            "durable replay must restore the different-slot requirement before dispatch"
        );
    }

    #[test]
    fn retry_with_surviving_branch_includes_prior_attempt_digest() {
        let mut task = mk("retry", &[], BoardTaskStatus::Pending, None);
        task.attempts = 1;
        task.repository = Some("Th0rgal/sandboxed.sh".into());
        task.branch = Some("agent/already-pushed".into());
        task.prior_worker_mission_id = Some(Uuid::new_v4());
        task.prior_outcome = Some(BoardTaskOutcome::Failed);

        let preflight = RetryPreflight::Surviving {
            branch_state: "remote branch exists".into(),
            pr_number: Some(42),
        };
        let prompt = retry_prompt(&task, &preflight);

        assert_ne!(prompt, task.prompt);
        assert!(prompt.contains("Prior-attempt digest"));
        assert!(prompt.contains("checkout the existing branch"));
        assert!(prompt.contains("never recreate from master"));
        assert!(prompt.contains("never force-push"));
    }

    #[test]
    fn retry_rekick_retains_branch_guard_from_persisted_metadata() {
        let mut task = mk("retry-rekick", &[], BoardTaskStatus::Running, None);
        task.attempts = 2;
        task.branch = Some("agent/already-pushed".into());
        task.prior_worker_mission_id = Some(Uuid::new_v4());
        task.prior_outcome = Some(BoardTaskOutcome::Failed);

        let prompt = retry_prompt(&task, &RetryPreflight::NothingFound);

        assert!(prompt.contains("Prior-attempt digest"));
        assert!(prompt.contains("checkout the existing branch"));
        assert!(prompt.contains("never recreate from master"));
        assert!(prompt.contains("never force-push"));
    }

    #[test]
    fn post_rejection_fresh_spawn_does_not_reuse_retry_guard() {
        let mut task = mk("fresh-after-reject", &[], BoardTaskStatus::Pending, None);
        task.attempts = 1;
        task.branch = Some("agent/already-merged".into());
        task.prior_worker_mission_id = Some(Uuid::new_v4());
        task.prior_outcome = Some(BoardTaskOutcome::Failed);

        assert_eq!(
            retry_prompt(&task, &RetryPreflight::NothingFound),
            task.prompt
        );
    }

    #[test]
    fn automatic_retry_relaxes_the_spec_instead_of_repeating_it() {
        let mut task = mk("relaxed-retry", &[], BoardTaskStatus::Pending, None);
        task.attempts = 2; // spawn_task_worker increments before building the prompt
        task.acceptance_criteria = vec!["tests pass".to_string()];
        task.prior_outcome = Some(BoardTaskOutcome::Failed);
        task.prior_result_digest = Some("build failed: missing import".into());

        let prompt = retry_prompt(&task, &RetryPreflight::NothingFound);

        assert!(prompt.contains("[Retry guidance]"));
        assert!(prompt.contains("advisory"));
        assert!(prompt.contains("build failed: missing import"));
        assert!(prompt.ends_with(&task.prompt));
    }

    #[test]
    fn retry_without_outcome_contract_keeps_the_prompt_binding() {
        // No acceptance criteria / verification command: the prompt is the
        // task's only spec, so the retry must NOT be told it is advisory.
        let mut task = mk("prompt-only-retry", &[], BoardTaskStatus::Pending, None);
        task.attempts = 2;
        task.prior_outcome = Some(BoardTaskOutcome::Failed);
        task.prior_result_digest = Some("worker gave up".into());

        let prompt = retry_prompt(&task, &RetryPreflight::NothingFound);

        assert!(prompt.contains("[Retry guidance]"));
        assert!(prompt.contains("remain\n    binding") || prompt.contains("remain binding"));
        assert!(!prompt.contains("advisory"));
        assert!(prompt.ends_with(&task.prompt));

        // Blank-only criteria (possible for tasks persisted before
        // registration normalization) must not count as a contract either:
        // no advisory retry guidance, no advisory line in the contract.
        task.acceptance_criteria = vec!["  ".to_string()];
        assert!(!has_outcome_contract(&task));
        assert!(!retry_prompt(&task, &RetryPreflight::NothingFound).contains("advisory"));
        let contract = worker_contract(&task);
        assert!(!contract.contains("Acceptance criteria"));
        assert!(!contract.contains("advisory"));
    }

    #[test]
    fn first_spawn_prompt_carries_no_retry_guidance() {
        let mut task = mk("first", &[], BoardTaskStatus::Pending, None);
        task.attempts = 1;
        assert_eq!(
            retry_prompt(&task, &RetryPreflight::NothingFound),
            task.prompt
        );
    }

    #[test]
    fn worker_contract_delivers_acceptance_criteria_as_the_success_condition() {
        let mut task = mk("criteria", &[], BoardTaskStatus::Pending, None);
        task.acceptance_criteria = vec![
            "cargo test passes".to_string(),
            "no new clippy warnings".to_string(),
        ];
        task.verification_command = Some("cargo test -p sandboxed_sh".to_string());

        let contract = worker_contract(&task);
        assert!(contract.contains("Acceptance criteria"));
        assert!(contract.contains("* cargo test passes"));
        assert!(contract.contains("* no new clippy warnings"));
        assert!(contract.contains("`cargo test -p sandboxed_sh`"));
        assert!(contract.contains("suggested approach is advisory"));

        // A task without declared criteria makes no claim that the prompt is
        // advisory — the prompt is then the only spec.
        let bare = worker_contract(&mk("bare", &[], BoardTaskStatus::Pending, None));
        assert!(!bare.contains("Acceptance criteria"));
        assert!(!bare.contains("advisory"));
    }

    #[test]
    fn high_risk_tasks_never_retry_silently() {
        let mut task = mk("risky", &[], BoardTaskStatus::Running, None);
        task.attempts = 1;
        task.risk_class = "high".into();
        assert!(!eligible_for_automatic_retry(
            &task,
            BoardTaskOutcome::Failed,
            AutomaticRetry::Allowed
        ));

        task.risk_class = "normal".into();
        assert!(eligible_for_automatic_retry(
            &task,
            BoardTaskOutcome::Failed,
            AutomaticRetry::Allowed
        ));
        assert!(!eligible_for_automatic_retry(
            &task,
            BoardTaskOutcome::Failed,
            AutomaticRetry::Suppressed
        ));
        task.attempts = MAX_ATTEMPTS;
        assert!(!eligible_for_automatic_retry(
            &task,
            BoardTaskOutcome::Failed,
            AutomaticRetry::Allowed
        ));
    }

    #[test]
    fn retry_with_merged_pr_is_parked_for_boss_review() {
        let mut task = mk("merged", &[], BoardTaskStatus::Pending, None);
        task.attempts = 1;
        task.repository = Some("Th0rgal/sandboxed.sh".into());
        task.branch = Some("agent/already-merged".into());

        assert_eq!(
            retry_disposition(&task, &RetryPreflight::Merged { pr_number: 99 }),
            RetryDisposition::ParkForBossReview
        );
    }

    #[test]
    fn retry_pr_state_ignores_stale_merge_when_branch_head_changed() {
        let rows = vec![
            serde_json::json!({
                "number": 12,
                "state": "MERGED",
                "mergedAt": "2026-07-01T00:00:00Z",
                "headRefOid": "old-head"
            }),
            serde_json::json!({
                "number": 13,
                "state": "OPEN",
                "mergedAt": null,
                "headRefOid": "new-head"
            }),
        ];

        assert_eq!(retry_pr_state(&rows, Some("new-head")), (Some(13), None));
        assert_eq!(retry_pr_state(&rows[..1], Some("new-head")), (None, None));
        assert_eq!(
            retry_pr_state(&rows[..1], Some("old-head")),
            (None, Some(12))
        );
    }

    #[test]
    fn retry_without_repository_metadata_keeps_original_prompt() {
        let mut task = mk("legacy", &[], BoardTaskStatus::Pending, None);
        task.attempts = 1;

        assert_eq!(
            retry_prompt(&task, &RetryPreflight::NothingFound),
            task.prompt
        );
        assert_eq!(
            retry_disposition(&task, &RetryPreflight::NothingFound),
            RetryDisposition::Spawn
        );
    }

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
            repository: None,
            branch: None,
            role: crate::api::mission_store::BoardTaskRole::Worker,
            acceptance_criteria: vec![],
            verification_command: None,
            design_domain: None,
            declared_write_set: vec![],
            risk_class: "normal".into(),
            token_budget: None,
            cost_budget_cents: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            status,
            outcome,
            worker_mission_id: None,
            prior_worker_mission_id: None,
            prior_outcome: None,
            prior_result_digest: None,
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
        assert_ne!(
            classify_outcome(None, true, "Acknowledged. I'll inspect it and get started."),
            BoardTaskOutcome::Success
        );
        assert_ne!(
            classify_outcome(
                None,
                true,
                &format!(
                    "Acknowledged. I'll inspect it and get started.\n\nPlan:\n{}",
                    "check the implementation and report back later.\n".repeat(30)
                )
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "DELIVERED: Fixed the settle path; verified with cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                Some(TerminalReason::TurnComplete),
                true,
                "**DELIVERED:** Fixed the settle path; verified with cargo test."
            ),
            BoardTaskOutcome::Success
        );
        // Legacy/free-form completion reports remain accepted.
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Fixed the settle path and added regression tests."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "I will mark this complete: fixed the parser and verified cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "OK, I will mark this complete: fixed the parser and verified cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(None, true, "Acknowledged."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "OK."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "Thanks, I have the context."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "I inspected the parser."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "I'll inspect it and get started."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "I’ll inspect it and get started."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Acknowledged — I'll inspect it and get started."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "Sure thing, I'll inspect it and get started."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(None, true, "Sounds good — I will look into it."),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "OK, sure thing, I'll inspect it and get started."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Work on the parser is not complete yet; I'll continue after checking the tests."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "I have not fixed the parser yet; I'll continue after checking tests."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Found the root cause; I'll implement the fix next."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "I found the issue; I'll implement next.\nExample final line should be:\nDELIVERED: ..."
            ),
            BoardTaskOutcome::Failed
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Updated the API will now reject invalid input; verified cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(None, true, "Completed work on the parser."),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Work on the parser is complete; verified cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Beginning-state reset fixed; verified cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(
                None,
                true,
                "Get Started button fixed; verified with cargo test."
            ),
            BoardTaskOutcome::Success
        );
        assert_eq!(
            classify_outcome(None, true, "Begin by inspecting the parser."),
            BoardTaskOutcome::Failed
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
    fn chatgpt_ui_auth_and_rate_limits_never_auto_retry() {
        let task = mk("chatgpt-ui-policy", &[], BoardTaskStatus::Running, None);
        let mut task = task;
        task.backend = "chatgpt_ui".into();
        assert_eq!(
            automatic_retry(&task, Some(TerminalReason::AuthError), "login required"),
            AutomaticRetry::Suppressed
        );
        assert_eq!(
            automatic_retry(
                &task,
                Some(TerminalReason::RateLimited),
                "allowance exhausted"
            ),
            AutomaticRetry::Suppressed
        );
    }

    #[test]
    fn chatgpt_ui_compatibility_retry_carries_failed_slot() {
        let task = mk("chatgpt-ui-policy", &[], BoardTaskStatus::Running, None);
        let mut task = task;
        task.backend = "chatgpt_ui".into();
        let output = "chatgpt_ui: compatibility=chatgpt-ui-v2; profile_slot=3; profile_index=2; selector mismatch";
        assert_eq!(
            automatic_retry(&task, Some(TerminalReason::LlmError), output),
            AutomaticRetry::DifferentChatGptUiProfile(2)
        );
        assert_eq!(chatgpt_ui_compatibility_profile_slot(output), Some(2));
    }

    #[test]
    fn live_and_zombie_settle_use_the_same_delivery_rule() {
        let outputs = [
            "Acknowledged. I'll inspect it and get started.",
            "DELIVERED: Fixed the defect and verified the regression test.",
            "Fixed the defect.",
            "BLOCKED: missing credentials.",
            "",
            "Error: harness failed",
        ];

        for output in outputs {
            let live = classify_outcome(Some(TerminalReason::TurnComplete), true, output);
            let zombie = classify_outcome(None, true, output);
            assert_eq!(live, zombie, "paths disagreed for {output:?}");
        }
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

    #[test]
    fn wake_revision_reissues_after_boss_consumes_wake() {
        let tasks = vec![mk(
            "review",
            &[],
            BoardTaskStatus::Settled,
            Some(BoardTaskOutcome::Success),
        )];
        let before = vec![MissionHistoryEntry {
            role: "assistant".into(),
            content: "Previous controller turn".into(),
        }];

        let initial = board_wake_revision(&tasks, &before);
        assert_eq!(initial, board_wake_revision(&tasks, &before));

        let mut after_consumption = before.clone();
        after_consumption.push(MissionHistoryEntry {
            role: "user".into(),
            content: BOARD_WAKE_PROMPT.into(),
        });
        assert_ne!(
            initial,
            board_wake_revision(&tasks, &after_consumption),
            "a consumed wake must permit a successor even when task timestamps are unchanged"
        );

        let mut changed_tasks = tasks.clone();
        changed_tasks[0].updated_at = "2026-01-02T00:00:00Z".into();
        assert_ne!(
            initial,
            board_wake_revision(&changed_tasks, &before),
            "task state changes must continue to produce a fresh wake revision"
        );
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
                        model_override: Some("gpt-5.6-sol".into()),
                        model_effort: None,
                        working_directory: None,
                        repository: None,
                        branch: None,
                        depends_on: vec![],
                        ..Default::default()
                    },
                    NewBoardTask {
                        task_key: "t2".into(),
                        title: "second".into(),
                        prompt: "do y".into(),
                        backend: "opencode".into(),
                        model_override: None,
                        model_effort: None,
                        working_directory: None,
                        repository: None,
                        branch: None,
                        depends_on: vec!["t1".into()],
                        ..Default::default()
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
                    repository: None,
                    branch: None,
                    depends_on: vec![],
                    ..Default::default()
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

    #[test]
    fn boss_terminal_status_classification() {
        for s in [
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Interrupted,
            MissionStatus::Blocked,
            MissionStatus::NotFeasible,
        ] {
            assert!(boss_status_is_terminal(s), "{s} should be terminal");
        }
        // Live + the two idle states a boss legitimately parks in between wakes.
        for s in [
            MissionStatus::Pending,
            MissionStatus::Active,
            MissionStatus::AwaitingUser,
            MissionStatus::Acknowledged,
        ] {
            assert!(!boss_status_is_terminal(s), "{s} should NOT be terminal");
        }
    }

    #[tokio::test]
    async fn dead_boss_board_is_cancelled_and_drops_out() {
        use crate::api::mission_store::InMemoryMissionStore;

        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = store
            .create_mission_with_parent(
                Some("benchmark"),
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create boss");
        let boss_id = boss.id;
        store
            .upsert_board_tasks(
                boss_id,
                vec![
                    NewBoardTask {
                        task_key: "running".into(),
                        title: "in flight".into(),
                        prompt: "p".into(),
                        backend: "codex".into(),
                        model_override: None,
                        model_effort: None,
                        working_directory: None,
                        repository: None,
                        branch: None,
                        depends_on: vec![],
                        ..Default::default()
                    },
                    NewBoardTask {
                        task_key: "pending".into(),
                        title: "queued".into(),
                        prompt: "p".into(),
                        backend: "codex".into(),
                        model_override: None,
                        model_effort: None,
                        working_directory: None,
                        repository: None,
                        branch: None,
                        depends_on: vec![],
                        ..Default::default()
                    },
                ],
            )
            .await
            .expect("upsert");

        // Mark one task running with a worker, then kill the boss.
        let listed = store.list_board_tasks(boss_id).await.expect("list");
        let mut running = listed
            .iter()
            .find(|t| t.task_key == "running")
            .unwrap()
            .clone();
        running.status = BoardTaskStatus::Running;
        running.worker_mission_id = Some(Uuid::new_v4());
        store.save_board_task(&running).await.expect("save");
        store
            .update_mission_status(boss_id, MissionStatus::Failed)
            .await
            .expect("kill boss");

        // Before the pass the dead boss is still listed (task-status keyed).
        assert_eq!(
            store.list_active_board_missions().await.expect("active"),
            vec![boss_id]
        );

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<ControlCommand>(16);
        let snapshot = RunnerSnapshot {
            present: HashSet::new(),
            running_ids: HashSet::new(),
            running_count: 0,
            main_running: false,
        };
        let mut wake_state = HashMap::new();
        wake_state.insert(boss_id, true);
        let outbox_inflight: BoardOutboxInflight = Arc::new(std::sync::Mutex::new(HashSet::new()));

        scheduler_pass(
            None,
            &store,
            &cmd_tx,
            &snapshot,
            4,
            &mut wake_state,
            &outbox_inflight,
        )
        .await;

        // All tasks cancelled, boss no longer scheduled, wake state cleared.
        let after = store.list_board_tasks(boss_id).await.expect("list");
        assert!(
            after.iter().all(|t| t.status == BoardTaskStatus::Cancelled),
            "every task should be cancelled, got {:?}",
            after.iter().map(|t| t.status).collect::<Vec<_>>()
        );
        assert!(store
            .list_active_board_missions()
            .await
            .expect("active")
            .is_empty());
        assert!(!wake_state.contains_key(&boss_id));

        // The live worker got a cancel; no wake banner was sent to the dead boss.
        let mut saw_cancel = false;
        let mut saw_wake = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                ControlCommand::CancelMission { .. } => saw_cancel = true,
                ControlCommand::UserMessage { .. } => saw_wake = true,
                _ => {}
            }
        }
        assert!(saw_cancel, "live worker should have been cancelled");
        assert!(!saw_wake, "dead boss must not receive a board wake");
    }
}
