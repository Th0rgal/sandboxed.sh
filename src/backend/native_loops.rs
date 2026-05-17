//! Native harness loop adapters.
//!
//! A "native loop" is a harness-driven continuation (claudecode `/goal`,
//! codex `/goal`, future opencode variants). sandboxed.sh's automation
//! infrastructure doesn't decide when these iterate — the harness CLI does —
//! but we still materialize an `Automation` row and record each iteration as
//! an `AutomationExecution` so the panel shows them alongside OA-scheduled
//! automations.
//!
//! Each harness implements [`NativeLoopAdapter`]. The registry is consulted
//! when a `/goal` (or future native loop command) is recognized to:
//!   1. find the right adapter for the active backend,
//!   2. build a [`crate::api::mission_store::CommandSource::NativeLoop`] payload,
//!   3. classify subsequent SSE goal events as iterations or completions.
//!
//! Phase 1 keeps adapters thin: launching and stopping the loop is still
//! handled inside the existing harness paths in `mission_runner.rs`. The
//! adapter is only used for *observation* and *event classification*.

use serde::Serialize;

use crate::api::control::AgentEvent;

/// Semantic role for text emitted while a native `/goal` loop is active.
///
/// Harnesses often emit their real deliverable as reasoning/thinking and then
/// finish with a short loop-control notice. Persisting this role lets the UI
/// show the user's expected final answer without treating terminal bookkeeping
/// as the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalOutputRole {
    Deliverable,
    Progress,
    TerminalNotice,
}

impl GoalOutputRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deliverable => "deliverable",
            Self::Progress => "progress",
            Self::TerminalNotice => "terminal_notice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalOutputSource {
    Assistant,
    Thinking,
}

pub fn is_goal_command(message: &str) -> bool {
    message.trim_start().starts_with("/goal")
}

pub fn classify_goal_output(content: &str, source: GoalOutputSource) -> Option<GoalOutputRole> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    let terminal_phrases = [
        "goal complete",
        "goal window",
        "window has elapsed",
        "waiting for the window",
        "continue if the goal hook",
        "hook is still active",
        "nothing more to do this turn",
    ];
    if terminal_phrases.iter().any(|phrase| lower.contains(phrase)) {
        return Some(GoalOutputRole::TerminalNotice);
    }

    let deliverable_markers = [
        "# summary",
        "summary:",
        "critical:",
        "findings",
        "recommendation",
        "status\n",
        "status\r\n",
        "1. ",
        "## ",
    ];
    if trimmed.len() >= 800 && source == GoalOutputSource::Thinking {
        return Some(GoalOutputRole::Deliverable);
    }
    if trimmed.len() >= 1200 && source == GoalOutputSource::Assistant {
        return Some(GoalOutputRole::Deliverable);
    }
    if deliverable_markers
        .iter()
        .filter(|marker| lower.contains(**marker))
        .count()
        >= 2
    {
        return Some(GoalOutputRole::Deliverable);
    }

    Some(GoalOutputRole::Progress)
}

/// What a single SSE event tells us about the loop's progress.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopObservation {
    /// Event has no bearing on this loop.
    None,
    /// Iteration boundary — record an `AutomationExecution` with this index.
    Iteration {
        index: u32,
        /// One-line summary suitable for the execution row (objective, status, …).
        summary: Option<String>,
    },
    /// Terminal status — close any open execution and mark the automation
    /// inactive when status is a final value.
    Completed {
        /// Canonical: `complete`, `aborted`, `cleared`, `paused`, `budget_limited`.
        status: String,
        summary: Option<String>,
    },
}

/// Per-harness adapter. Phase 1 surface is intentionally narrow — observation
/// only. Phase 2 will add `launch` and `stop` so the panel can re-fire / cancel
/// the loop without going through the harness CLI directly.
pub trait NativeLoopAdapter: Send + Sync {
    /// Backend id (matches `Mission.backend`): `claudecode`, `codex`, …
    fn harness(&self) -> &'static str;

    /// Slash command this adapter implements, without the leading `/`. Today:
    /// always `"goal"`. Kept as a method so a future harness can advertise
    /// `"review"` or similar without changing the trait.
    fn command(&self) -> &'static str;

    /// Translate a single `AgentEvent` into a [`LoopObservation`]. Adapters
    /// return `LoopObservation::None` for unrelated events.
    fn observe(&self, event: &AgentEvent) -> LoopObservation;
}

// ─── Adapter: Claude Code `/goal` ────────────────────────────────────────────
pub struct ClaudeCodeGoal;

impl NativeLoopAdapter for ClaudeCodeGoal {
    fn harness(&self) -> &'static str {
        "claudecode"
    }
    fn command(&self) -> &'static str {
        "goal"
    }
    fn observe(&self, event: &AgentEvent) -> LoopObservation {
        observe_goal_event(event)
    }
}

// ─── Adapter: Codex `/goal` ──────────────────────────────────────────────────
pub struct CodexGoal;

impl NativeLoopAdapter for CodexGoal {
    fn harness(&self) -> &'static str {
        "codex"
    }
    fn command(&self) -> &'static str {
        "goal"
    }
    fn observe(&self, event: &AgentEvent) -> LoopObservation {
        observe_goal_event(event)
    }
}

// ─── Adapter: Grok `/goal` (sandboxed.sh-driven) ─────────────────────────────
//
// Grok has no native goal-mode primitive — see `crate::api::grok_goal` for
// the full design. Sandboxed.sh drives iteration via an AgentFinished
// automation, parses sentinel markers from the assistant text, and emits
// the same `AgentEvent::GoalIteration` / `AgentEvent::GoalStatus` shape as
// codex so the UI surface is identical. Registering the adapter here lets
// `native_loop_observer` materialise Automation + AutomationExecution rows
// for grok-goal missions in the Automations panel alongside codex/claudecode
// entries.
pub struct GrokGoal;

impl NativeLoopAdapter for GrokGoal {
    fn harness(&self) -> &'static str {
        "grok"
    }
    fn command(&self) -> &'static str {
        "goal"
    }
    fn observe(&self, event: &AgentEvent) -> LoopObservation {
        observe_goal_event(event)
    }
}

/// Shared observer for `/goal` — all three harnesses emit `GoalIteration`
/// and `GoalStatus` events with the same shape, so the classification is
/// identical.
fn observe_goal_event(event: &AgentEvent) -> LoopObservation {
    match event {
        AgentEvent::GoalIteration {
            iteration,
            objective,
            ..
        } => LoopObservation::Iteration {
            index: *iteration,
            summary: Some(objective.clone()),
        },
        AgentEvent::GoalStatus {
            status, objective, ..
        } => LoopObservation::Completed {
            status: status.clone(),
            summary: Some(objective.clone()),
        },
        _ => LoopObservation::None,
    }
}

/// Returns the registered adapters in iteration order. Add a new harness here
/// (and only here) to expose it as a native loop.
pub fn registry() -> &'static [&'static dyn NativeLoopAdapter] {
    &[&ClaudeCodeGoal, &CodexGoal, &GrokGoal]
}

/// Find the adapter for a given (harness, command) pair, if any.
pub fn find_adapter(harness: &str, command: &str) -> Option<&'static dyn NativeLoopAdapter> {
    registry()
        .iter()
        .copied()
        .find(|a| a.harness() == harness && a.command() == command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn iteration_event_maps_to_iteration_observation() {
        let evt = AgentEvent::GoalIteration {
            iteration: 3,
            objective: "ship the thing".into(),
            mission_id: Some(Uuid::nil()),
        };
        let obs = ClaudeCodeGoal.observe(&evt);
        assert!(matches!(obs, LoopObservation::Iteration { index: 3, .. }));
    }

    #[test]
    fn status_event_maps_to_completed_observation() {
        let evt = AgentEvent::GoalStatus {
            status: "complete".into(),
            objective: "ship the thing".into(),
            mission_id: Some(Uuid::nil()),
        };
        let obs = CodexGoal.observe(&evt);
        match obs {
            LoopObservation::Completed { status, .. } => assert_eq!(status, "complete"),
            _ => panic!("expected Completed observation"),
        }
    }

    #[test]
    fn unrelated_event_is_none() {
        let evt = AgentEvent::TextDelta {
            content: "hi".into(),
            mission_id: Some(Uuid::nil()),
        };
        let obs = ClaudeCodeGoal.observe(&evt);
        assert_eq!(obs, LoopObservation::None);
    }

    #[test]
    fn registry_finds_known_adapters() {
        assert!(find_adapter("claudecode", "goal").is_some());
        assert!(find_adapter("codex", "goal").is_some());
        assert!(find_adapter("grok", "goal").is_some());
        assert!(find_adapter("opencode", "goal").is_none());
        assert!(find_adapter("claudecode", "audit").is_none());
    }

    #[test]
    fn goal_output_classifier_marks_long_thinking_as_deliverable() {
        let report = format!(
            "# Summary\n{}\n1. Critical: proof target changed\n## Recommendation\nship it",
            "analysis ".repeat(120)
        );
        assert_eq!(
            classify_goal_output(&report, GoalOutputSource::Thinking),
            Some(GoalOutputRole::Deliverable)
        );
    }

    #[test]
    fn goal_output_classifier_marks_goal_control_notice_as_terminal() {
        let notice = "The original goal window has elapsed. Goal complete.";
        assert_eq!(
            classify_goal_output(notice, GoalOutputSource::Assistant),
            Some(GoalOutputRole::TerminalNotice)
        );
    }

    #[test]
    fn goal_output_classifier_marks_short_status_as_progress() {
        assert_eq!(
            classify_goal_output("The analysis is in good shape.", GoalOutputSource::Thinking),
            Some(GoalOutputRole::Progress)
        );
    }
}
