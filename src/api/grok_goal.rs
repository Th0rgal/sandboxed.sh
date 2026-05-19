//! Server-side `/goal <objective>` loop for the Grok Build backend.
//!
//! Unlike codex (which has a harness-native `thread/goal/*` RPC) and Claude
//! Code (whose CLI parses `/goal` internally), Grok has no goal-mode
//! primitive. Sandboxed.sh drives the loop itself:
//!
//! 1. When `/goal <objective>` lands in a grok mission, we strip the prefix,
//!    inject a sentinel protocol into the first-turn prompt, and create an
//!    `AgentFinished`-triggered [`Automation`] row whose inline command is
//!    the continuation prompt for subsequent turns.
//! 2. After every grok turn, the post-turn hook parses the assistant's
//!    final text for one of the sentinels (`<goal_complete/>`,
//!    `<goal_continue/>`, `<goal_aborted reason="..."/>`).
//!    - `Complete` / `Aborted` → disable the automation and emit
//!      `GoalStatus { status: ... }`.
//!    - `Continue` (or `Missing`, treated leniently up to a small limit) →
//!      increment the iteration counter, emit
//!      `GoalIteration { iteration: N }`, and let the existing
//!      `agent_finished_automation_messages` flow re-fire the continuation
//!      prompt as the next user message.
//! 3. An iteration budget caps runaway loops; reaching it emits
//!    `GoalStatus { status: "budget_limited" }` and disables the automation.
//!
//! The shape mirrors the codex `/goal` adapter so the dashboard / iOS clients
//! render iteration + status pills with no UI changes (the SSE events
//! `goal_iteration` and `goal_status` are already wired through
//! `mission_runner.rs:13499-13511`).

use crate::api::mission_store::{
    self, Automation, CommandSource, FreshSession, MissionStore, RetryConfig, StopPolicy,
    TriggerType,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

// ─── Tunables ────────────────────────────────────────────────────────────────

/// Cap on goal iterations. Hit → `GoalStatus { status: "budget_limited" }`.
pub const MAX_ITERATIONS: u32 = 25;

/// Number of consecutive missing-sentinel turns tolerated before we treat
/// the loop as aborted with reason `"no_goal_sentinel"`. The model often
/// drops the marker on the first turn; ≥2 consecutive misses is intent.
pub const MAX_MISSING_SENTINELS: u32 = 2;

/// Tag stored in the automation's `variables` map so the post-turn hook
/// can recognise a sandboxed.sh-driven grok goal row vs. an unrelated
/// AgentFinished automation a user created manually.
pub const GROK_GOAL_TAG_KEY: &str = "sandboxed.grok_goal";
pub const GROK_GOAL_TAG_VALUE: &str = "1";

/// Variable key under which we store the user's objective string.
pub const VAR_OBJECTIVE: &str = "goal_objective";
/// Variable key under which we store the current iteration count.
pub const VAR_ITERATION: &str = "goal_iteration";
/// Variable key under which we store consecutive missing-sentinel count.
pub const VAR_MISSING_COUNT: &str = "goal_missing_count";

// ─── Sentinel parsing ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum GoalSentinel {
    Complete,
    Continue,
    Aborted {
        reason: String,
    },
    /// No sentinel present in the assistant text. Treated leniently — see
    /// [`MAX_MISSING_SENTINELS`].
    Missing,
}

/// Parse the trailing sentinel marker from an assistant response.
///
/// Searches the whole text rather than just the last line so models that
/// emit the marker followed by a sign-off ("…done. <goal_complete/> Bye!")
/// still trigger. Precedence: `complete` > `aborted` > `continue`. A
/// `complete` marker anywhere wins over a later `continue` — the model
/// declared the work done.
pub fn parse_goal_sentinel(text: &str) -> GoalSentinel {
    let lower = text.to_ascii_lowercase();
    if lower.contains("<goal_complete/>") || lower.contains("<goal_complete />") {
        return GoalSentinel::Complete;
    }
    if let Some(reason) = extract_aborted_reason(text) {
        return GoalSentinel::Aborted { reason };
    }
    if lower.contains("<goal_continue/>") || lower.contains("<goal_continue />") {
        return GoalSentinel::Continue;
    }
    GoalSentinel::Missing
}

/// Pull the `reason="..."` attribute out of `<goal_aborted reason="..."/>`.
/// Returns `Some("unspecified")` if the marker is present but has no
/// `reason` attribute. Returns `None` if the marker isn't present at all.
fn extract_aborted_reason(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.find("<goal_aborted")?;
    let tail = &text[idx..];
    let end = tail.find("/>").map(|e| idx + e + 2)?;
    let segment = &text[idx..end];
    // Naive but adequate: find `reason="..."`. Allow either quote style.
    for quote in ['"', '\''] {
        let needle = format!("reason={}", quote);
        if let Some(after) = segment.find(&needle) {
            let start = after + needle.len();
            if let Some(close) = segment[start..].find(quote) {
                return Some(segment[start..start + close].trim().to_string());
            }
        }
    }
    Some("unspecified".to_string())
}

// ─── Prefix detection ────────────────────────────────────────────────────────

/// Single-pass `/goal ` prefix strip, matching the rule used by
/// `src/backend/codex/mod.rs:parse_goal_prefix`. Returns
/// `(is_goal, objective_or_original_message)`.
pub fn parse_goal_prefix(message: &str) -> (bool, String) {
    let trimmed = message.trim_start();
    match trimmed.strip_prefix("/goal ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, message.to_string()),
    }
}

// ─── Prompts ─────────────────────────────────────────────────────────────────

/// First-turn prompt: replaces the user's `/goal X` message. Establishes
/// the sentinel protocol and asks the agent to begin.
pub fn first_turn_prompt(objective: &str) -> String {
    format!(
        "[Goal mode]\n\
         Objective: {objective}\n\n\
         You are running in a sandboxed.sh-driven goal loop. Take concrete \
         steps toward the objective. You will be re-invoked automatically \
         after each reply, so you do not need to do everything in one turn.\n\n\
         At the end of every reply, output EXACTLY ONE of these markers on \
         its own line:\n\
         - `<goal_complete/>` — when the objective is fully achieved.\n\
         - `<goal_continue/>` — when more work is required (you will be \
         re-invoked automatically).\n\
         - `<goal_aborted reason=\"...\"/>` — when a hard blocker prevents \
         further progress.\n\n\
         The marker must be the literal text above. Do not paraphrase it. \
         If no marker is present, the loop will treat your turn as \
         in-progress and continue.\n\n\
         Begin now."
    )
}

/// Continuation prompt fired by the AgentFinished automation between
/// iterations. Kept terse so it doesn't blow up the conversation context.
pub fn continuation_prompt() -> String {
    "Continue toward the goal. Remember to end your reply with exactly one \
     of: `<goal_complete/>`, `<goal_continue/>`, or \
     `<goal_aborted reason=\"...\"/>`."
        .to_string()
}

// ─── Automation lifecycle ────────────────────────────────────────────────────

/// Find the currently active grok-goal automation for a mission, if any.
///
/// Identifies a goal row by the `sandboxed.grok_goal=1` tag in
/// [`Automation::variables`]. Multiple rows on the same mission would be a
/// bug; we return the first match deterministically (sorted by created_at
/// ascending — the original goal wins over any duplicate).
pub async fn active_goal_for_mission(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
) -> Option<Automation> {
    let mut automations = mission_store
        .get_mission_automations(mission_id)
        .await
        .ok()?;
    automations.sort_by_key(|a| a.created_at.clone());
    automations.into_iter().find(|a| {
        a.active
            && matches!(a.trigger, TriggerType::AgentFinished)
            && a.variables.get(GROK_GOAL_TAG_KEY).map(String::as_str) == Some(GROK_GOAL_TAG_VALUE)
    })
}

/// Build the variables map for a fresh grok-goal automation.
fn initial_variables(objective: &str, iteration: u32) -> HashMap<String, String> {
    let mut v = HashMap::new();
    v.insert(
        GROK_GOAL_TAG_KEY.to_string(),
        GROK_GOAL_TAG_VALUE.to_string(),
    );
    v.insert(VAR_OBJECTIVE.to_string(), objective.to_string());
    v.insert(VAR_ITERATION.to_string(), iteration.to_string());
    v.insert(VAR_MISSING_COUNT.to_string(), "0".to_string());
    v
}

/// Read `goal_iteration` out of a row, defaulting to 0 on malformed input.
pub fn iteration_of(row: &Automation) -> u32 {
    row.variables
        .get(VAR_ITERATION)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Read `goal_objective` out of a row, defaulting to empty.
pub fn objective_of(row: &Automation) -> String {
    row.variables
        .get(VAR_OBJECTIVE)
        .cloned()
        .unwrap_or_default()
}

/// Read `goal_missing_count` out of a row, defaulting to 0.
pub fn missing_count_of(row: &Automation) -> u32 {
    row.variables
        .get(VAR_MISSING_COUNT)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Create the AgentFinished automation row that drives the loop. Idempotent
/// at the caller — only call when [`active_goal_for_mission`] returned
/// `None`.
pub async fn create_goal_automation(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    objective: &str,
) -> Result<Automation, String> {
    let automation = Automation {
        id: Uuid::new_v4(),
        mission_id,
        command_source: CommandSource::Inline {
            content: continuation_prompt(),
        },
        trigger: TriggerType::AgentFinished,
        variables: initial_variables(objective, 1),
        active: true,
        created_at: mission_store::now_string(),
        last_triggered_at: None,
        retry_config: RetryConfig::default(),
        // We disable the row explicitly when the sentinel says we're done;
        // we don't want a single failed iteration to silently kill the
        // loop, and we don't have a "stop on goal complete" policy variant.
        stop_policy: StopPolicy::Never,
        fresh_session: FreshSession::Keep,
        consecutive_failures: 0,
        driver: mission_store::AutomationDriver::Scheduler,
    };
    mission_store
        .create_automation(automation.clone())
        .await
        .map(|_| automation)
        .map_err(|e| format!("create_automation failed: {}", e))
}

/// Persist an iteration / missing-count update to the row's variables.
pub async fn update_counters(
    mission_store: &Arc<dyn MissionStore>,
    row: &mut Automation,
    iteration: u32,
    missing_count: u32,
) -> Result<(), String> {
    row.variables
        .insert(VAR_ITERATION.to_string(), iteration.to_string());
    row.variables
        .insert(VAR_MISSING_COUNT.to_string(), missing_count.to_string());
    mission_store
        .update_automation(row.clone())
        .await
        .map(|_| ())
        .map_err(|e| format!("update_automation failed: {}", e))
}

/// Mark the goal automation inactive. Used on Complete / Aborted /
/// BudgetLimited. Errors are logged by the caller.
pub async fn disable_goal(
    mission_store: &Arc<dyn MissionStore>,
    row: &mut Automation,
) -> Result<(), String> {
    row.active = false;
    mission_store
        .update_automation(row.clone())
        .await
        .map(|_| ())
        .map_err(|e| format!("disable_automation failed: {}", e))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_strip_matches_codex() {
        assert_eq!(
            parse_goal_prefix("/goal ship it"),
            (true, "ship it".to_string())
        );
        assert_eq!(
            parse_goal_prefix("   /goal ship it"),
            (true, "ship it".to_string())
        );
        assert_eq!(
            parse_goal_prefix("not a goal"),
            (false, "not a goal".to_string())
        );
        // Embedded `/goal ` inside the objective must survive single-pass.
        assert_eq!(
            parse_goal_prefix("/goal /goal nested"),
            (true, "/goal nested".to_string())
        );
        // Empty objective normalises to empty string but `is_goal` is true.
        // (The caller is responsible for rejecting an empty objective; we
        // don't lie about whether the prefix was present.)
        assert_eq!(parse_goal_prefix("/goal "), (true, "".to_string()));
    }

    #[test]
    fn sentinel_complete_wins_over_continue() {
        let t = "Done. <goal_complete/> Also could add tests later <goal_continue/>";
        assert_eq!(parse_goal_sentinel(t), GoalSentinel::Complete);
    }

    #[test]
    fn sentinel_aborted_extracts_reason() {
        let t = "Can't proceed. <goal_aborted reason=\"no XAI_API_KEY\"/>";
        match parse_goal_sentinel(t) {
            GoalSentinel::Aborted { reason } => assert_eq!(reason, "no XAI_API_KEY"),
            other => panic!("expected Aborted, got {:?}", other),
        }
    }

    #[test]
    fn sentinel_aborted_without_reason_attribute() {
        let t = "Blocked. <goal_aborted/>";
        match parse_goal_sentinel(t) {
            GoalSentinel::Aborted { reason } => assert_eq!(reason, "unspecified"),
            other => panic!("expected Aborted, got {:?}", other),
        }
    }

    #[test]
    fn sentinel_continue_recognised() {
        assert_eq!(
            parse_goal_sentinel("Working on it. <goal_continue/>"),
            GoalSentinel::Continue
        );
        // Space-before-slash variant.
        assert_eq!(
            parse_goal_sentinel("<goal_continue />"),
            GoalSentinel::Continue
        );
    }

    #[test]
    fn sentinel_missing_when_no_marker() {
        assert_eq!(
            parse_goal_sentinel("I did some work but forgot the marker."),
            GoalSentinel::Missing
        );
    }

    #[test]
    fn sentinel_match_is_case_insensitive() {
        assert_eq!(
            parse_goal_sentinel("<GOAL_COMPLETE/>"),
            GoalSentinel::Complete
        );
    }

    #[test]
    fn first_turn_prompt_includes_objective_and_markers() {
        let p = first_turn_prompt("write tests");
        assert!(p.contains("write tests"));
        assert!(p.contains("<goal_complete/>"));
        assert!(p.contains("<goal_continue/>"));
        assert!(p.contains("<goal_aborted"));
    }

    #[test]
    fn continuation_prompt_reminds_sentinels() {
        let p = continuation_prompt();
        assert!(p.contains("<goal_complete/>"));
        assert!(p.contains("<goal_continue/>"));
        assert!(p.contains("<goal_aborted"));
    }

    #[test]
    fn initial_variables_carry_tag_and_objective() {
        let v = initial_variables("ship it", 3);
        assert_eq!(v.get(GROK_GOAL_TAG_KEY).map(String::as_str), Some("1"));
        assert_eq!(v.get(VAR_OBJECTIVE).map(String::as_str), Some("ship it"));
        assert_eq!(v.get(VAR_ITERATION).map(String::as_str), Some("3"));
        assert_eq!(v.get(VAR_MISSING_COUNT).map(String::as_str), Some("0"));
    }
}
