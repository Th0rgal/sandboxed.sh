//! Mission storage module with pluggable backends.
//!
//! Supports:
//! - `memory`: In-memory storage (non-persistent, for testing)
//! - `file`: JSON file-based storage (legacy)
//! - `sqlite`: SQLite database with full event logging

mod file;
mod memory;
mod sqlite;

pub use file::FileMissionStore;
pub use memory::InMemoryMissionStore;
pub use sqlite::SqliteMissionStore;

use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo, MissionStatus};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// FLEET-001 scheduling metadata for a mission. Flattened into [`Mission`] so
/// the fields surface at the top level of API responses (the fleet watcher
/// reads `priority`/`not_before`/`deadline` directly) while keeping the
/// scheduler inputs grouped in one place.
///
/// Timestamps are stored as RFC3339 strings to match the rest of the mission
/// row (`created_at`/`updated_at`) and the underlying TEXT columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionScheduling {
    /// Dispatch priority; higher wins. FIFO (by `created_at`) within a tier.
    #[serde(default)]
    pub priority: i32,
    /// Do not dispatch before this RFC3339 timestamp. The scheduler holds the
    /// mission's goal in `deferred_goal` and only dispatches once `now` passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<String>,
    /// Deadline (RFC3339). The scheduler fails a still-undispatched scheduled
    /// mission with reason `deadline_exceeded` once this passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

/// Project tagging metadata for a mission. Flattened into `Mission` so the
/// fields appear top-level in serialized output. Lets external consumers (e.g.
/// Paloma) group/filter/route missions by project, track, intent, PR, or
/// freeform tags instead of parsing conventions out of free-text titles like
/// `[beal-research] BR-07 ...`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionProject {
    /// Stable project identifier (e.g. "verity-core").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Track / workstream within the project (e.g. "C3-bridge-collapse").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Intent of the mission (e.g. "repair-build", "investigate").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Associated GitHub PR number, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_pr: Option<i64>,
    /// Freeform tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl MissionProject {
    /// True when no project metadata is set.
    pub fn is_empty(&self) -> bool {
        self.project.is_none()
            && self.track.is_none()
            && self.intent.is_none()
            && self.github_pr.is_none()
            && self.tags.is_empty()
    }
}

/// Tri-state patch for project metadata: each field is `None` to leave
/// unchanged, `Some(None)` to clear, `Some(Some(v))` to set. `tags` is
/// `Some(vec)` to replace the whole list.
#[derive(Debug, Clone, Default)]
pub struct MissionProjectPatch {
    pub project: Option<Option<String>>,
    pub track: Option<Option<String>>,
    pub intent: Option<Option<String>>,
    pub github_pr: Option<Option<i64>>,
    pub tags: Option<Vec<String>>,
}

impl MissionProjectPatch {
    /// True when the patch would change nothing.
    pub fn is_empty(&self) -> bool {
        self.project.is_none()
            && self.track.is_none()
            && self.intent.is_none()
            && self.github_pr.is_none()
            && self.tags.is_none()
    }
}

/// Activity timestamps for a mission, surfaced so watchdogs/consumers can
/// reason about staleness without guessing from `updated_at` alone (which also
/// bumps on metadata edits). `last_status_change_at` is persisted; the event
/// timestamps are computed on read from the event log and may be `None` when
/// not requested (e.g. internal store reads that skip enrichment).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionActivity {
    /// When the mission's status last changed (persisted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_change_at: Option<String>,
    /// Timestamp of the most recent mission event of any kind (computed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_event_at: Option<String>,
    /// Timestamp of the most recent assistant output (computed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_at: Option<String>,
    /// Convenience: max(updated_at, last_agent_event_at) (computed). Lets a
    /// consumer derive staleness with a single field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

/// Disambiguates *why* a mission is parked in `AwaitingUser`: the agent asked a
/// real question that needs a decision, vs. it just finished a turn / its work
/// and is waiting to be acknowledged or merged. Only meaningful while the
/// status is `AwaitingUser`; `None` for every other status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingKind {
    /// The agent is asking the user something and needs an answer to proceed.
    Decision,
    /// The agent finished its turn / work and is waiting for acknowledgement.
    Ack,
}

impl AwaitingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AwaitingKind::Decision => "decision",
            AwaitingKind::Ack => "ack",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "decision" => Some(AwaitingKind::Decision),
            "ack" => Some(AwaitingKind::Ack),
            _ => None,
        }
    }
}

/// A mission (persistent goal-oriented session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: Uuid,
    pub status: MissionStatus,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_version: Option<String>,
    /// Workspace ID where this mission runs (defaults to host workspace)
    #[serde(default = "default_workspace_id")]
    pub workspace_id: Uuid,
    /// Workspace name (resolved from workspace_id for display)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// Agent name from library (e.g., "code-reviewer")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional model override (provider/model)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    /// Optional model effort override (e.g. low/medium/high/xhigh/max)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_effort: Option<String>,
    /// Backend to use for this mission ("opencode" or "claudecode")
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Config profile to use for this mission (from library configs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_profile: Option<String>,
    pub history: Vec<MissionHistoryEntry>,
    pub created_at: String,
    pub updated_at: String,
    /// When this mission was interrupted (if status is Interrupted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
    /// FLEET-004: when this mission was last paused (if status is Paused). RFC3339.
    /// Lets the UI show pause age and enables future zombie-pause cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    /// Whether this mission can be resumed
    #[serde(default)]
    pub resumable: bool,
    /// Desktop sessions started during this mission (used for reconnect/stream resume)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub desktop_sessions: Vec<DesktopSessionInfo>,
    /// Session ID for conversation persistence (used by Claude Code --session-id)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Why the mission terminated (for failed/completed missions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    /// Parent mission ID (for orchestrated worker missions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_mission_id: Option<Uuid>,
    /// Working directory override (for git worktrees etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Mission operating mode (task or assistant)
    #[serde(default)]
    pub mission_mode: MissionMode,
    /// True when the mission was started via codex `/goal <objective>`. The
    /// codex backend infers this from the user's message at send time, but
    /// persisting it on the row lets the UI render the goal pill from a
    /// fresh page load (no SSE replay required) and survives reconnects.
    #[serde(default)]
    pub goal_mode: bool,
    /// Cached goal objective when `goal_mode` is true. Updated on each
    /// `thread/goal/updated` notification so the latest text from codex
    /// (which may have been edited via `/goal pause`/`/goal resume`)
    /// stays current.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_objective: Option<String>,
    /// When the user first opened this mission *since it last entered
    /// AwaitingUser*. Drives the 1-hour ack grace timer and the "opened"
    /// dot on Finished missions. Cleared when the mission goes back to
    /// Active via a new user message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_viewed_at: Option<String>,
    /// FLEET-001 scheduling metadata (priority, not_before, deadline).
    /// Flattened so the fields appear top-level in serialized output.
    #[serde(default, flatten)]
    pub scheduling: MissionScheduling,
    /// Project tagging metadata (project, track, intent, github_pr, tags).
    /// Flattened so the fields appear top-level.
    #[serde(default, flatten)]
    pub project: MissionProject,
    /// Activity timestamps (last_status_change_at persisted; event timestamps
    /// computed on read). Flattened so the fields appear top-level.
    #[serde(default, flatten)]
    pub activity: MissionActivity,
    /// When `status` is `AwaitingUser`, classifies whether the agent needs a
    /// decision or just an acknowledgement. `None` for every other status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_kind: Option<AwaitingKind>,
}

/// Aggregate mission counts by status.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MissionStatusCounts {
    pub total: usize,
    pub active: usize,
    pub completed: usize,
    pub failed: usize,
}

fn default_backend() -> String {
    "claudecode".to_string()
}

fn default_workspace_id() -> Uuid {
    crate::workspace::DEFAULT_WORKSPACE_ID
}

impl Mission {
    /// True when the `not_before` constraint (if any) is satisfied at `now`.
    /// An unset or unparseable timestamp never blocks dispatch.
    pub fn is_dispatchable_at(&self, now: chrono::DateTime<Utc>) -> bool {
        match self.scheduling.not_before.as_deref() {
            Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| t.with_timezone(&Utc) <= now)
                .unwrap_or(true),
            None => true,
        }
    }

    /// True when a `deadline` is set and has already elapsed at `now`.
    pub fn is_past_deadline(&self, now: chrono::DateTime<Utc>) -> bool {
        match self.scheduling.deadline.as_deref() {
            Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| t.with_timezone(&Utc) < now)
                .unwrap_or(false),
            None => false,
        }
    }
}

/// FLEET-001: select the next mission to dispatch from a candidate set.
///
/// A mission is *runnable* when it is `Pending` (which excludes `Paused`,
/// terminal, and in-flight states) and its `not_before` constraint is
/// satisfied at `now`. Among runnable missions the highest `priority` wins,
/// with ties broken by oldest `created_at` (FIFO). Returns `None` when
/// nothing is runnable.
///
/// This is a pure function so the dispatch ordering has a single, unit-tested
/// source of truth independent of any storage backend.
pub fn select_next_runnable_mission(
    missions: &[Mission],
    now: chrono::DateTime<Utc>,
) -> Option<&Mission> {
    missions
        .iter()
        .filter(|m| m.status == MissionStatus::Pending)
        .filter(|m| m.is_dispatchable_at(now))
        .max_by(|a, b| {
            a.scheduling
                .priority
                .cmp(&b.scheduling.priority)
                .then_with(|| b.created_at.cmp(&a.created_at))
        })
}

/// A single entry in the mission history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionHistoryEntry {
    pub role: String,
    pub content: String,
}

/// A stored event with full metadata (for event replay/debugging).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: i64,
    pub mission_id: Uuid,
    pub sequence: i64,
    pub event_type: String,
    pub timestamp: String,
    pub event_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub content: String,
    pub metadata: serde_json::Value,
}

/// Persisted summary for one tool call across all of its stored events.
#[derive(Debug, Clone, Default)]
pub struct ToolCallSummary {
    pub has_result: bool,
    pub result_sequence: Option<i64>,
    pub result_timestamp: Option<String>,
    pub call_content_bytes: usize,
    pub result_content_bytes: usize,
}

/// Aggregated AI token/cost usage for a single (normalized) model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageStats {
    /// Canonical model identifier (e.g. "claude-3-5-sonnet", "gpt-4o").
    /// Empty string when the model was not recorded for an event.
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_cents: u64,
}

/// Aggregated AI usage for one UTC day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsageStats {
    /// ISO-8601 day (YYYY-MM-DD, UTC) derived from the event timestamp.
    pub day: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_cents: u64,
}

/// Aggregated AI usage for one UTC hour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlyUsageStats {
    /// `YYYY-MM-DDTHH` (UTC), e.g. "2026-05-19T08".
    pub hour: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_cents: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Automation Types
// ─────────────────────────────────────────────────────────────────────────────

/// Source of the command to execute in an automation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandSource {
    /// Command from the library (by name)
    Library { name: String },
    /// Command from a local file (relative to mission workspace)
    LocalFile { path: String },
    /// Inline command content
    Inline { content: String },
    /// Harness-native loop (e.g. claudecode `/goal`, codex `/goal`). OA does
    /// not drive iteration here — the harness CLI runs its own continuation
    /// loop and we record each iteration as an `AutomationExecution`. See
    /// `crate::backend::native_loops` for the per-harness adapters.
    NativeLoop {
        /// Backend id: `"claudecode"`, `"codex"`, `"opencode"`, …
        harness: String,
        /// Slash command, without the leading `/`. Today: `"goal"`.
        command: String,
        /// Free-form per-command args. For `goal`: `{ "objective": "..." }`.
        #[serde(default)]
        args: serde_json::Value,
    },
}

/// Webhook configuration for webhook-triggered automations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookConfig {
    /// Unique webhook ID (part of the webhook URL path)
    pub webhook_id: String,
    /// Optional secret token for HMAC validation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Variable mappings from webhook payload to command variables
    /// Example: {"repo": "webhook.repository.name", "commit": "webhook.head_commit.id"}
    #[serde(default)]
    pub variable_mappings: HashMap<String, String>,
}

/// Trigger type for an automation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerType {
    /// Fixed interval in seconds
    Interval { seconds: u64 },
    /// Cron expression (e.g. "0 8 * * *" for daily at 8:00 UTC)
    Cron {
        /// Standard 5-field cron expression: minute hour day-of-month month day-of-week
        expression: String,
        /// IANA timezone (e.g. "Europe/Paris"). Defaults to UTC.
        #[serde(default = "default_timezone")]
        timezone: String,
    },
    /// Webhook trigger
    Webhook { config: WebhookConfig },
    /// Trigger immediately after an agent turn finishes for the mission
    AgentFinished,
    /// Telegram bot trigger (messages are routed via the Telegram bridge)
    Telegram { config: TelegramTriggerConfig },
}

/// Configuration for a Telegram-triggered automation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelegramTriggerConfig {
    /// The channel ID this automation is linked to
    pub channel_id: Uuid,
}

/// Stop policy for automation lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopPolicy {
    /// Never auto-disable this automation.
    Never,
    /// Auto-disable after N consecutive failures.
    WhenFailingConsecutively {
        /// Number of consecutive failures before stopping (default: 2)
        #[serde(default = "default_failure_count")]
        count: u32,
    },
    /// Auto-disable when all issues are closed and all PRs are merged in a GitHub repo.
    WhenAllIssuesClosedAndPRsMerged {
        /// GitHub repository in "owner/repo" format
        repo: String,
    },
    /// Auto-disable after the automation fires for the first time. Used by
    /// `schedule_wakeup` to make one-shot wake-ups; the scheduler sets the
    /// automation inactive on the tick after an execution record exists.
    AfterFirstFire,
}

/// Whether to start a fresh session for each automation trigger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FreshSession {
    /// Always start a fresh session (clear context/history).
    Always,
    /// Route completion-triggered automation to another session.
    /// Requires custom variable `nextSessionId` set to a mission UUID.
    Switch,
    /// Keep session alive (default behavior).
    #[default]
    Keep,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_stop_policy() -> StopPolicy {
    StopPolicy::WhenFailingConsecutively {
        count: default_failure_count(),
    }
}

fn default_failure_count() -> u32 {
    2
}

/// Retry configuration for automation execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_retries: u32,
    /// Initial delay in seconds between retries
    pub retry_delay_seconds: u64,
    /// Backoff multiplier for exponential backoff (1.0 = no backoff)
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,
}

fn default_backoff_multiplier() -> f64 {
    2.0
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_delay_seconds: 60,
            backoff_multiplier: 2.0,
        }
    }
}

/// Who actually drives iteration for an automation.
///
/// `Scheduler` is the historical behavior — OA fires the command on a
/// `TriggerType`. `HarnessLoop` means the harness CLI runs its own
/// continuation loop (claudecode/codex `/goal`); OA records iterations
/// but doesn't decide when they fire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationDriver {
    #[default]
    Scheduler,
    HarnessLoop,
}

/// An automation that triggers commands based on various triggers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    pub id: Uuid,
    pub mission_id: Uuid,
    /// Source of the command to execute
    pub command_source: CommandSource,
    /// Trigger configuration
    pub trigger: TriggerType,
    /// Variable substitutions to apply to the command
    /// Example: {"timestamp": "<timestamp/>", "mission_name": "<mission_name/>"}
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Whether this automation is currently active
    pub active: bool,
    /// When this automation was created
    pub created_at: String,
    /// When this automation was last triggered (for interval-based automations)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_triggered_at: Option<String>,
    /// Retry configuration
    #[serde(default)]
    pub retry_config: RetryConfig,
    /// Auto-stop behavior when mission reaches terminal state.
    #[serde(default = "default_stop_policy")]
    pub stop_policy: StopPolicy,
    /// Whether to start a fresh session for each trigger (clears context/history).
    #[serde(default)]
    pub fresh_session: FreshSession,
    /// Number of consecutive failures (used for WhenFailingConsecutively policy).
    /// This is tracked internally and not persisted directly.
    #[serde(default, skip_serializing)]
    pub consecutive_failures: u32,
    /// What drives iteration for this automation. Existing rows default to
    /// `Scheduler` (OA-driven) so the field is back-compatible.
    #[serde(default)]
    pub driver: AutomationDriver,
}

/// Execution status for automation runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Running,
    Success,
    Failed,
    Cancelled,
    Skipped,
}

/// A record of a single automation execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationExecution {
    pub id: Uuid,
    pub automation_id: Uuid,
    pub mission_id: Uuid,
    /// When this execution was triggered
    pub triggered_at: String,
    /// What triggered this execution
    pub trigger_source: String, // "interval", "webhook", "manual"
    /// Current execution status
    pub status: ExecutionStatus,
    /// Webhook payload (if triggered by webhook)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_payload: Option<serde_json::Value>,
    /// Variables that were substituted in the command
    #[serde(default)]
    pub variables_used: HashMap<String, String>,
    /// When execution completed (success or failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Error message if execution failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Number of retry attempts made
    #[serde(default)]
    pub retry_count: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Mission Mode
// ─────────────────────────────────────────────────────────────────────────────

/// Mission operating mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissionMode {
    /// Standard task execution — agent works toward a goal and completes.
    #[default]
    Task,
    /// Persistent assistant — mission stays alive, waits for messages indefinitely.
    /// Used for chat-based assistants (Telegram, Slack, etc.)
    Assistant,
}

// ─────────────────────────────────────────────────────────────────────────────
// Communication Channels
// ─────────────────────────────────────────────────────────────────────────────

/// A communication channel that connects external messaging platforms to a mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannel {
    pub id: Uuid,
    /// Mission this channel is connected to (sentinel UUID when auto_create_missions is true)
    pub mission_id: Uuid,
    /// Bot token for the Telegram Bot API (never exposed in API responses)
    #[serde(skip_serializing)]
    pub bot_token: String,
    /// Optional bot username (e.g. "ana_bot")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_username: Option<String>,
    /// Chat IDs allowed to interact with this bot (empty = allow all)
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
    /// How the bot should be triggered
    #[serde(default)]
    pub trigger_mode: TelegramTriggerMode,
    /// Whether this channel is currently active
    pub active: bool,
    /// Secret token for Telegram webhook verification
    #[serde(skip_serializing)]
    pub webhook_secret: Option<String>,
    /// System instructions prepended to every Telegram message for this channel.
    /// Use this to customize assistant behavior (e.g. "Don't use markdown formatting").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// When true, each Telegram chat auto-creates its own mission using the default_* settings.
    #[serde(default)]
    pub auto_create_missions: bool,
    /// Default backend for auto-created missions (e.g. "claudecode", "opencode")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_backend: Option<String>,
    /// Default model override for auto-created missions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model_override: Option<String>,
    /// Default model effort for auto-created missions (low/medium/high/xhigh/max)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model_effort: Option<String>,
    /// Default workspace ID for auto-created missions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace_id: Option<Uuid>,
    /// Default config profile for auto-created missions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_config_profile: Option<String>,
    /// Default agent for auto-created missions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramUser {
    pub id: Uuid,
    pub telegram_user_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub role: TelegramUserRole,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramUserRole {
    Owner,
    TrustedFriend,
    Observer,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramUserCursor {
    pub id: Uuid,
    pub telegram_user_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dashboard_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_alert_ack_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_digest_at: Option<String>,
    pub last_seen_event_sequence_by_mission_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramMissionSubscription {
    pub id: Uuid,
    pub telegram_user_id: i64,
    pub mission_id: Uuid,
    pub interest_level: TelegramMissionInterestLevel,
    pub reason: Option<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramMissionInterestLevel {
    Muted,
    Normal,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramAlertPreference {
    pub id: Uuid,
    pub telegram_user_id: i64,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_value: Option<String>,
    pub rule_text: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from_message_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramAlert {
    pub id: Uuid,
    pub telegram_user_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    pub event_kind: String,
    pub importance: String,
    pub title: String,
    pub body: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram_message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PalomaDecision {
    pub id: Uuid,
    pub event_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    pub channel: String,
    pub reason_code: String,
    pub proposed_action: String,
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppression_reason: Option<String>,
    pub policy_snapshot_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_text_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_text_preview: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PalomaSchedulerJob {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub run_count: i64,
    pub updated_at: String,
}

/// Per-user notification preferences. Owns quiet hours, rate ceilings, and
/// per-class / per-mission overrides for the Paloma delivery policy.
///
/// Two JSON columns (`alert_class_overrides`, `mission_overrides`) intentionally
/// hold loose JSON: their schemas are still being shaped by Phase 4+ and we'd
/// rather not migrate the table every time the policy evolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PalomaUserPreferences {
    pub telegram_user_id: i64,
    /// IANA timezone name (e.g. `"Europe/Paris"`). UTC fallback if unparseable.
    pub timezone: String,
    /// Inclusive start hour (0..23) of the daily quiet window. `None` disables
    /// quiet hours entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours_start: Option<i64>,
    /// Exclusive end hour (0..23) of the daily quiet window. May be less than
    /// `start` for windows that span midnight (e.g. 23 → 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours_end: Option<i64>,
    pub max_interrupts_per_hour: i64,
    pub max_interrupts_per_day: i64,
    /// When true, alerts whose policy decision marks them `critical`
    /// (production failures, hard breakage) are still delivered during quiet
    /// hours.
    pub failure_override_quiet: bool,
    pub alert_class_overrides_json: String,
    pub mission_overrides_json: String,
    /// Coarse digest cadence selector: `"daily"`, `"hourly"`, or `"off"`.
    pub digest_cadence: String,
    pub created_at: String,
    pub updated_at: String,
}

impl PalomaUserPreferences {
    /// Conservative defaults for a brand-new owner: quiet hours 23:00–08:00
    /// local, one interrupt per hour, four per day, failures override quiet.
    pub fn default_for(telegram_user_id: i64, now: &str) -> Self {
        Self {
            telegram_user_id,
            timezone: "UTC".to_string(),
            quiet_hours_start: Some(23),
            quiet_hours_end: Some(8),
            max_interrupts_per_hour: 1,
            max_interrupts_per_day: 4,
            failure_override_quiet: true,
            alert_class_overrides_json: "{}".to_string(),
            mission_overrides_json: "{}".to_string(),
            digest_cadence: "daily".to_string(),
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }
    }
}

/// Per-mission, per-alert-class cooldown state. The decision pipeline owns
/// cadence here instead of smuggling it through `event_kind` suffixes. When an
/// interrupt fires for a given `(mission_id, alert_class)` we bump the
/// `backoff_step` and push `next_eligible_at` further out — exponential backoff
/// per mission, reset by user reply or status change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PalomaCooldownState {
    pub mission_id: Uuid,
    pub alert_class: String,
    pub telegram_user_id: i64,
    pub last_sent_at: String,
    pub next_eligible_at: String,
    /// 0-indexed step into the backoff ladder. Phase 2 ladder is
    /// `[0, 30m, 2h, 8h, 24h]`; higher steps clamp to the last entry.
    pub backoff_step: i64,
}

/// Persistent anchor for a per-mission Telegram card. The card is a single
/// Telegram message that is edited in place as mission state changes, instead
/// of producing one new alert per status transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PalomaMissionCard {
    pub mission_id: Uuid,
    pub telegram_user_id: i64,
    pub channel_id: Uuid,
    pub chat_id: i64,
    pub message_id: i64,
    /// Hash of the most recently rendered card content. Lets the scheduler
    /// skip `editMessageText` when nothing visible has changed.
    pub content_hash: String,
    /// Timestamp of the most recent (re-)anchor: when this message_id was first
    /// posted. Used to detect the 48-hour edit-window cutoff.
    pub anchor_ts: String,
    pub last_edit_ts: String,
    /// Edit version counter, useful for debugging churn.
    pub version: i64,
    /// True once the mission is in a terminal state and the card should no
    /// longer be updated.
    pub archived: bool,
}

/// A mapping from a Telegram chat to an auto-created mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChatMission {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub chat_id: i64,
    pub mission_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    pub created_at: String,
}

/// A Telegram message queued for immediate or delayed delivery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelegramScheduledMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mission_id: Option<Uuid>,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    pub text: String,
    pub send_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub status: TelegramScheduledMessageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramScheduledMessageStatus {
    Pending,
    Sent,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramStructuredMemoryKind {
    Fact,
    Note,
    Task,
    Preference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TelegramStructuredMemoryScope {
    #[default]
    Chat,
    User,
    Channel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramStructuredMemoryEntry {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    #[serde(default)]
    pub scope: TelegramStructuredMemoryScope,
    pub kind: TelegramStructuredMemoryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_user_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_id: Option<i64>,
    pub source_role: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelegramStructuredMemorySearchHit {
    pub entry: TelegramStructuredMemoryEntry,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramActionExecutionStatus {
    Pending,
    Sent,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramActionExecutionKind {
    Send,
    Reminder,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramActionExecution {
    pub id: Uuid,
    pub channel_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mission_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chat_id: Option<i64>,
    pub target_chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chat_title: Option<String>,
    pub action_kind: TelegramActionExecutionKind,
    pub target_kind: String,
    pub target_value: String,
    pub text: String,
    pub delay_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_message_id: Option<Uuid>,
    pub status: TelegramActionExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramConversationMessageDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramConversation {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramConversationMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub channel_id: Uuid,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram_message_id: Option<i64>,
    pub direction: TelegramConversationMessageDirection,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_user_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramWorkflowKind {
    RequestReply,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TelegramWorkflowStatus {
    WaitingExternal,
    RelayedToOrigin,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramWorkflow {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub origin_conversation_id: Uuid,
    pub origin_chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_mission_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_conversation_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chat_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chat_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chat_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_request_message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_by_user_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_by_username: Option<String>,
    pub kind: TelegramWorkflowKind,
    pub status: TelegramWorkflowStatus,
    pub request_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_reply_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramWorkflowEvent {
    pub id: Uuid,
    pub workflow_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<Uuid>,
    pub event_type: String,
    pub payload_json: String,
    pub created_at: String,
}

/// How Telegram messages trigger the assistant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TelegramTriggerMode {
    /// Trigger on @bot_username mentions in groups, replies to bot, or DMs (recommended default)
    #[default]
    MentionOrDm,
    /// Trigger on @bot_username mentions in groups only
    BotMention,
    /// Trigger on replies to bot messages only
    Reply,
    /// Trigger on DMs only
    DirectMessage,
    /// Trigger on every message in allowed chats (no filtering)
    #[serde(alias = "all")]
    Always,
}

/// Get current timestamp as RFC3339 string.
pub fn now_string() -> String {
    Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Task board: server-scheduled worker tasks owned by a boss mission.
//
// The board replaces LLM-driven scheduling: the boss agent registers a task
// DAG once, and the control loop spawns worker missions for ready tasks up to
// capacity, settles them when their turn ends, and notifies the boss with a
// digest. See `api::control::board` for the scheduler.
// ---------------------------------------------------------------------------

/// Lifecycle of a board task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardTaskStatus {
    /// Registered; waiting on dependencies and/or capacity.
    Pending,
    /// A worker mission is executing this task.
    Running,
    /// The worker's turn ended; awaiting a boss verdict.
    Settled,
    /// Boss accepted the result (terminal).
    Accepted,
    /// Worker failed after retry (terminal unless re-planned).
    Failed,
    /// Cancelled by the boss or the user (terminal).
    Cancelled,
}

impl std::fmt::Display for BoardTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Settled => "settled",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{}", s)
    }
}

impl BoardTaskStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "settled" => Some(Self::Settled),
            "accepted" => Some(Self::Accepted),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Terminal states never transition again (except via explicit re-plan).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Accepted | Self::Failed | Self::Cancelled)
    }
}

/// How a settled task's worker turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardTaskOutcome {
    /// Worker reported completion.
    Success,
    /// Worker stopped with a BLOCKED question for the boss.
    Blocked,
    /// Worker turn failed (llm error, stall, interruption).
    Failed,
}

impl std::fmt::Display for BoardTaskOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        };
        write!(f, "{}", s)
    }
}

impl BoardTaskOutcome {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" => Some(Self::Success),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A task on a boss mission's board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTask {
    pub id: Uuid,
    pub boss_mission_id: Uuid,
    /// Stable, boss-chosen key (unique per board) used for `depends_on`
    /// references and digests.
    pub task_key: String,
    pub title: String,
    pub prompt: String,
    /// Worker backend (e.g. "codex", "opencode", "grok"). Never claudecode.
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_effort: Option<String>,
    /// Working directory for the worker (usually an isolated git worktree).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Task keys (same board) that must settle successfully or be accepted
    /// before this task may start.
    pub depends_on: Vec<String>,
    pub status: BoardTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BoardTaskOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_mission_id: Option<Uuid>,
    /// Number of worker spawns so far (1 = first attempt).
    pub attempts: u32,
    /// Truncated tail of the worker's final message, set on settle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    /// Free-form audit trail: rejections with feedback, retries, cancellations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Payload for registering/updating tasks on a board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewBoardTask {
    pub task_key: String,
    pub title: String,
    pub prompt: String,
    pub backend: String,
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub model_effort: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Sanitize a string for use as a filename.
pub fn sanitize_filename(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// Portable snapshot of a mission for cross-environment transfer.
///
/// Produced by [`MissionStore::export_mission_bundle`] and consumed by
/// [`MissionStore::import_mission_bundle`]. Designed to round-trip between
/// instances that may disagree on workspace UUIDs — the bundle carries
/// `workspace_name` so the import side can resolve against its own
/// workspace store, and does *not* carry runtime session state (Claude/Codex
/// `.credentials.json`, container mount points) which are per-environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionBundle {
    /// Bundle format version. Bump on breaking changes.
    pub version: u32,
    /// When this bundle was exported (ISO-8601 UTC).
    pub exported_at: String,
    /// Optional `SANDBOXED_PUBLIC_URL` of the source instance — purely for
    /// auditing/debug; import logic ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_public_url: Option<String>,
    /// Name of the workspace this mission ran in. The import side resolves
    /// this to a local workspace UUID (or the caller can supply one).
    pub workspace_name: Option<String>,
    /// The mission row itself. On import its `id` may be replaced; the
    /// exported value lets a consumer correlate to the source instance.
    pub mission: Mission,
    /// All `mission_events` rows for this mission, in sequence order.
    /// Content is loaded inline — the import side stores it back through
    /// the normal spill mechanism, so large payloads re-spill on the
    /// target side regardless of where they lived originally.
    pub events: Vec<StoredEvent>,
    /// All `automations` for this mission. Imported as disabled so they
    /// don't immediately fire on the target — the user re-enables
    /// explicitly.
    pub automations: Vec<Automation>,
    /// Last N executions per automation, preserved for history context.
    /// May be empty.
    #[serde(default)]
    pub executions: Vec<AutomationExecution>,
}

/// Options accepted by [`MissionStore::import_mission_bundle`].
#[derive(Debug, Clone, Default)]
pub struct MissionImportOptions {
    /// Override the target workspace UUID. When `None`, the import resolves
    /// `bundle.workspace_name` against the local workspace store.
    pub target_workspace_id: Option<Uuid>,
    /// Display name of the target workspace. When set, this is used
    /// instead of the source bundle's `workspace_name` — otherwise a
    /// `?workspace_id=` override would leave the stored name pointing
    /// at the source workspace and confuse future exports/imports.
    pub target_workspace_name: Option<String>,
    /// Keep the bundle's automations enabled (default: import as disabled).
    pub keep_automations_active: bool,
}

/// Mission store trait - implemented by all storage backends.
#[allow(clippy::too_many_arguments)]
#[async_trait]
pub trait MissionStore: Send + Sync {
    /// Whether this store persists data across restarts.
    fn is_persistent(&self) -> bool;

    /// List missions, ordered by updated_at descending.
    async fn list_missions(&self, limit: usize, offset: usize) -> Result<Vec<Mission>, String>;

    /// Count missions by status without applying list pagination.
    async fn count_missions_by_status(&self) -> Result<MissionStatusCounts, String>;

    /// Get a single mission by ID.
    async fn get_mission(&self, id: Uuid) -> Result<Option<Mission>, String>;

    /// Create a new mission.
    async fn create_mission(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        model_effort: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
    ) -> Result<Mission, String> {
        self.create_mission_with_parent(
            title,
            workspace_id,
            agent,
            model_override,
            model_effort,
            backend,
            config_profile,
            None,
            None,
        )
        .await
    }

    /// Create a new mission with optional parent and working directory.
    async fn create_mission_with_parent(
        &self,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        model_effort: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
        parent_mission_id: Option<Uuid>,
        working_directory: Option<&str>,
    ) -> Result<Mission, String>;

    /// Update mission status.
    async fn update_mission_status(&self, id: Uuid, status: MissionStatus) -> Result<(), String>;

    /// Persist FLEET-001 scheduling metadata (priority, not_before, deadline)
    /// for a mission. Default is a no-op so non-persistent stores can ignore it;
    /// the SQLite store overrides it to write the dedicated columns.
    async fn set_mission_scheduling(
        &self,
        _id: Uuid,
        _scheduling: &MissionScheduling,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Update mission status with terminal reason (for failed/completed missions).
    async fn update_mission_status_with_reason(
        &self,
        id: Uuid,
        status: MissionStatus,
        terminal_reason: Option<&str>,
    ) -> Result<(), String>;

    /// Update mission conversation history.
    async fn update_mission_history(
        &self,
        id: Uuid,
        history: &[MissionHistoryEntry],
    ) -> Result<(), String>;

    /// Update mission desktop sessions.
    async fn update_mission_desktop_sessions(
        &self,
        id: Uuid,
        sessions: &[DesktopSessionInfo],
    ) -> Result<(), String>;

    /// Update mission title.
    async fn update_mission_title(&self, id: Uuid, title: &str) -> Result<(), String>;

    /// Update mission run settings.
    /// Field semantics for optional string settings are tri-state:
    /// - `None` => leave unchanged
    /// - `Some(Some(value))` => set value
    /// - `Some(None)` => clear value
    async fn update_mission_run_settings(
        &self,
        id: Uuid,
        backend: Option<&str>,
        agent: Option<Option<&str>>,
        model_override: Option<Option<&str>>,
        model_effort: Option<Option<&str>>,
        config_profile: Option<Option<&str>>,
        session_id: &str,
    ) -> Result<Mission, String>;

    /// Update mission metadata generated by backend (title + short description).
    /// Field semantics are tri-state:
    /// - `None` => leave unchanged
    /// - `Some(Some(value))` => set value
    /// - `Some(None)` => clear value
    async fn update_mission_metadata(
        &self,
        id: Uuid,
        title: Option<Option<&str>>,
        short_description: Option<Option<&str>>,
        metadata_source: Option<Option<&str>>,
        metadata_model: Option<Option<&str>>,
        metadata_version: Option<Option<&str>>,
    ) -> Result<(), String>;

    /// Update project tagging metadata (see [`MissionProjectPatch`] for the
    /// tri-state semantics). Default no-op for stores that do not persist it.
    async fn update_mission_project(
        &self,
        id: Uuid,
        patch: MissionProjectPatch,
    ) -> Result<(), String> {
        let _ = (id, patch);
        Ok(())
    }

    /// Computed activity timestamps for the given missions, keyed by id:
    /// `(last_agent_event_at, last_output_at)` derived from the event log.
    /// Empty map by default (stores without an event log skip this).
    async fn get_mission_activity(
        &self,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, (Option<String>, Option<String>)>, String> {
        let _ = ids;
        Ok(std::collections::HashMap::new())
    }

    /// Set (or clear) the `awaiting_kind` classification for a mission. Only
    /// meaningful while the mission is in `AwaitingUser`. Default no-op for
    /// stores that do not persist it.
    async fn set_mission_awaiting_kind(
        &self,
        id: Uuid,
        kind: Option<AwaitingKind>,
    ) -> Result<(), String> {
        let _ = (id, kind);
        Ok(())
    }

    /// Update mission session ID (for backends that generate their own IDs).
    async fn update_mission_session_id(&self, id: Uuid, session_id: &str) -> Result<(), String>;

    /// Update cached goal-mode metadata for missions started with `/goal`.
    async fn update_mission_goal(
        &self,
        id: Uuid,
        goal_mode: bool,
        goal_objective: Option<&str>,
    ) -> Result<(), String>;

    /// Update mission agent tree.
    async fn update_mission_tree(&self, id: Uuid, tree: &AgentTreeNode) -> Result<(), String>;

    /// Get mission agent tree.
    async fn get_mission_tree(&self, id: Uuid) -> Result<Option<AgentTreeNode>, String>;

    /// Get all child missions of a parent mission.
    async fn get_child_missions(&self, parent_mission_id: Uuid) -> Result<Vec<Mission>, String> {
        let _ = parent_mission_id;
        Ok(vec![])
    }

    /// Delete a mission.
    async fn delete_mission(&self, id: Uuid) -> Result<bool, String>;

    /// Delete empty untitled missions, excluding the specified IDs.
    async fn delete_empty_untitled_missions_excluding(
        &self,
        exclude: &[Uuid],
    ) -> Result<usize, String>;

    /// Get missions that have been active but stale for the specified hours.
    async fn get_stale_active_missions(&self, stale_hours: u64) -> Result<Vec<Mission>, String>;

    /// Get all missions currently in active status (for startup recovery).
    async fn get_all_active_missions(&self) -> Result<Vec<Mission>, String>;

    /// FLEET-001 scheduling: persist (or clear, with `None`) the deferred goal a
    /// `not_before`-scheduled mission will run once the dispatcher picks it up.
    async fn set_deferred_goal(&self, mission_id: Uuid, goal: Option<String>)
        -> Result<(), String>;

    /// FLEET-001 scheduling: read the deferred goal for a mission, if any.
    async fn get_deferred_goal(&self, mission_id: Uuid) -> Result<Option<String>, String>;

    /// FLEET-001 scheduling: all `Pending` missions that carry a deferred goal
    /// (i.e. are armed for scheduled dispatch). Scheduling fields are populated
    /// so the dispatcher can order/expire them; history is not loaded.
    async fn get_scheduled_pending_missions(&self) -> Result<Vec<Mission>, String>;

    /// FLEET-004: set (or clear, with `None`) the timestamp at which a mission
    /// was paused. Set on pause, cleared on resume.
    async fn set_mission_paused_at(
        &self,
        mission_id: Uuid,
        paused_at: Option<String>,
    ) -> Result<(), String>;

    /// Record the first time the user opened this mission, if not already set.
    /// Returns `Some(timestamp)` if the field was set by this call, or `None`
    /// if it was already populated (no-op). Used by the new
    /// `POST /missions/:id/opened` endpoint to start the AwaitingUser ack
    /// grace timer.
    async fn set_mission_first_viewed_at_if_unset(
        &self,
        id: Uuid,
        timestamp: &str,
    ) -> Result<Option<String>, String>;

    /// Atomically flip any AwaitingUser mission whose `first_viewed_at` is
    /// older than `grace_seconds` to `Acknowledged`. Returns the IDs that
    /// were promoted so the caller can broadcast `MissionStatusChanged`
    /// events for them.
    async fn acknowledge_stale_awaiting_user_missions(
        &self,
        grace_seconds: u64,
    ) -> Result<Vec<Uuid>, String>;

    /// Get recently interrupted missions that were stopped by server shutdown.
    async fn get_recent_server_shutdown_mission_ids(
        &self,
        max_age_hours: u64,
    ) -> Result<Vec<Uuid>, String> {
        let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours as i64);
        let missions = self.list_missions(1000, 0).await?;
        Ok(missions
            .into_iter()
            .filter(|m| {
                m.status == MissionStatus::Interrupted
                    && m.resumable
                    && m.terminal_reason.as_deref() == Some("server_shutdown")
                    && m.mission_mode != MissionMode::Assistant
                    && m.interrupted_at
                        .as_deref()
                        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                        .map(|value| value.with_timezone(&Utc) >= cutoff)
                        .unwrap_or(false)
            })
            .map(|m| m.id)
            .collect())
    }

    /// Insert a mission summary (for historical lookup).
    async fn insert_mission_summary(
        &self,
        mission_id: Uuid,
        summary: &str,
        key_files: &[String],
        success: bool,
    ) -> Result<(), String>;

    // === Event logging methods (default no-op for backward compatibility) ===

    /// Log a streaming event. Called for every AgentEvent during execution.
    async fn log_event(&self, mission_id: Uuid, event: &AgentEvent) -> Result<(), String> {
        let _ = (mission_id, event);
        Ok(())
    }

    /// Get all events for a mission (for replay/debugging).
    async fn get_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, event_types, limit, offset);
        Ok(vec![])
    }

    /// Get the last `limit` events for a mission, returned in chronological
    /// order (sequence ASC). Equivalent to `ORDER BY sequence DESC LIMIT N`
    /// then re-sorted. Use this for "show me what just happened" surfaces
    /// like the Paloma mission card: `get_events` paginates from the
    /// *oldest* event and silently drops anything past the limit, which
    /// makes the card's "Latest" line stale for long-running missions.
    async fn get_latest_events(
        &self,
        mission_id: Uuid,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, limit);
        Ok(vec![])
    }

    /// Get events with `sequence > since_seq`, ordered by sequence ASC.
    /// Used by the client for delta reconnect — pass the highest
    /// sequence the client has seen and get only events that arrived
    /// since. Cheaper than offset-based pagination for long missions.
    async fn get_events_since(
        &self,
        mission_id: Uuid,
        since_seq: i64,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, since_seq, event_types, limit);
        Ok(vec![])
    }

    /// Get events with `sequence < before_seq`, ordered by sequence ASC.
    /// Used by the client for backwards pagination — load the most
    /// recent N events first (`latest=true`), then page older by
    /// passing the lowest sequence already seen as `before_seq`.
    /// Sequence-based so it's robust against concurrent inserts.
    async fn get_events_before(
        &self,
        mission_id: Uuid,
        before_seq: i64,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, before_seq, event_types, limit);
        Ok(vec![])
    }

    /// Get all persisted events for a specific tool call, ordered by
    /// sequence ASC. Used by lazy history hydration.
    async fn get_events_for_tool_call(
        &self,
        mission_id: Uuid,
        tool_call_id: &str,
    ) -> Result<Vec<StoredEvent>, String> {
        let _ = (mission_id, tool_call_id);
        Ok(vec![])
    }

    /// Get the most recent distinct tool call ids for a mission, ordered by
    /// their latest event sequence descending. Used to keep conversation
    /// profile trace tails stable across paged event responses.
    async fn get_recent_tool_call_ids(
        &self,
        mission_id: Uuid,
        limit: usize,
    ) -> Result<Vec<String>, String> {
        let _ = (mission_id, limit);
        Ok(vec![])
    }

    /// Get persisted summaries for specific tool calls.
    async fn get_tool_call_summaries(
        &self,
        mission_id: Uuid,
        tool_call_ids: &[String],
    ) -> Result<HashMap<String, ToolCallSummary>, String> {
        let _ = (mission_id, tool_call_ids);
        Ok(HashMap::new())
    }

    /// Count events for a mission, optionally filtered by type.
    async fn count_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
    ) -> Result<usize, String> {
        let _ = (mission_id, event_types);
        Ok(0)
    }

    /// Count events grouped by event type for one mission.
    async fn count_events_by_type(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
    ) -> Result<HashMap<String, usize>, String> {
        let _ = (mission_id, event_types);
        Ok(HashMap::new())
    }

    /// Return the highest `sequence` value for this mission, or 0 if
    /// the mission has no events yet.
    async fn max_event_sequence(&self, mission_id: Uuid) -> Result<i64, String> {
        let _ = mission_id;
        Ok(0)
    }

    /// Sequence of the Nth-most-recent event whose type is in `anchor_types`
    /// (e.g. the 10th-most-recent user/assistant message). Returns `None`
    /// when the mission has fewer than `n` such events. Used to load a
    /// *conversation-anchored* snapshot tail — everything from this sequence
    /// to the head — instead of a raw event-count tail that, on tool-heavy
    /// missions, would be dominated by tool calls and bury recent messages.
    async fn nth_recent_event_sequence(
        &self,
        mission_id: Uuid,
        anchor_types: &[&str],
        n: usize,
    ) -> Result<Option<i64>, String> {
        let _ = (mission_id, anchor_types, n);
        Ok(None)
    }

    /// Get total cost in cents across all missions.
    /// Aggregates assistant_message metadata cost across all events.
    async fn get_total_cost_cents(&self) -> Result<u64, String> {
        Ok(0)
    }

    /// Get cost in cents grouped by source (actual, estimated, unknown).
    /// Returns (actual, estimated, unknown) tuple.
    async fn get_cost_by_source(&self) -> Result<(u64, u64, u64), String> {
        Ok((0, 0, 0))
    }

    /// Get total cost in cents for events created on or after `since` (ISO-8601).
    async fn get_total_cost_cents_since(&self, _since: &str) -> Result<u64, String> {
        Ok(0)
    }

    /// Get cost in cents grouped by source, for events on or after `since` (ISO-8601).
    async fn get_cost_by_source_since(&self, _since: &str) -> Result<(u64, u64, u64), String> {
        Ok((0, 0, 0))
    }

    /// Aggregate AI usage per (normalized) model across all assistant_message events.
    /// Returns per-model totals (requests, tokens, cost). Time-window optional.
    async fn get_usage_by_model(
        &self,
        _since: Option<&str>,
    ) -> Result<Vec<ModelUsageStats>, String> {
        Ok(Vec::new())
    }

    /// Aggregate AI usage per UTC day. Days with no usage are omitted; the
    /// caller is responsible for filling gaps if a contiguous series is needed.
    async fn get_usage_by_day(&self, _since: Option<&str>) -> Result<Vec<DailyUsageStats>, String> {
        Ok(Vec::new())
    }

    /// Aggregate AI usage per UTC hour. Buckets with no usage are omitted.
    /// Returned timestamps are in `YYYY-MM-DDTHH` form (no minutes/seconds).
    async fn get_usage_by_hour(
        &self,
        _since: Option<&str>,
    ) -> Result<Vec<HourlyUsageStats>, String> {
        Ok(Vec::new())
    }

    /// Record one OpenAI-compatible /v1 router request's token usage.
    /// `model` should already be normalized (see `crate::cost::normalized_model`).
    async fn record_proxy_usage(
        &self,
        _model: &str,
        _input_tokens: u64,
        _output_tokens: u64,
        _cost_cents: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Aggregate /v1 router usage per model. Merged with mission usage by the
    /// usage summary endpoint.
    async fn get_proxy_usage_by_model(
        &self,
        _since: Option<&str>,
    ) -> Result<Vec<ModelUsageStats>, String> {
        Ok(Vec::new())
    }

    /// Aggregate /v1 router usage per UTC day.
    async fn get_proxy_usage_by_day(
        &self,
        _since: Option<&str>,
    ) -> Result<Vec<DailyUsageStats>, String> {
        Ok(Vec::new())
    }

    /// Aggregate /v1 router usage per UTC hour (`YYYY-MM-DDTHH`).
    async fn get_proxy_usage_by_hour(
        &self,
        _since: Option<&str>,
    ) -> Result<Vec<HourlyUsageStats>, String> {
        Ok(Vec::new())
    }

    // === Automation methods (default no-op for backward compatibility) ===

    /// Create an automation for a mission.
    async fn create_automation(&self, automation: Automation) -> Result<Automation, String> {
        let _ = automation;
        Err("Automations not supported by this store".to_string())
    }

    /// Get all automations for a mission.
    async fn get_mission_automations(&self, mission_id: Uuid) -> Result<Vec<Automation>, String> {
        let _ = mission_id;
        Ok(vec![])
    }

    /// List all active automations across missions.
    async fn list_active_automations(&self) -> Result<Vec<Automation>, String> {
        Ok(vec![])
    }

    /// Get an automation by ID.
    async fn get_automation(&self, id: Uuid) -> Result<Option<Automation>, String> {
        let _ = id;
        Ok(None)
    }

    /// Update an automation.
    async fn update_automation(&self, automation: Automation) -> Result<(), String> {
        let _ = automation;
        Err("Automations not supported by this store".to_string())
    }

    /// Update automation active status.
    async fn update_automation_active(&self, id: Uuid, active: bool) -> Result<(), String> {
        let _ = (id, active);
        Err("Automations not supported by this store".to_string())
    }

    /// Update automation last triggered time.
    async fn update_automation_last_triggered(&self, id: Uuid) -> Result<(), String> {
        let _ = id;
        Err("Automations not supported by this store".to_string())
    }

    /// Delete an automation.
    async fn delete_automation(&self, id: Uuid) -> Result<bool, String> {
        let _ = id;
        Ok(false)
    }

    /// Get automation by webhook ID.
    async fn get_automation_by_webhook_id(
        &self,
        webhook_id: &str,
    ) -> Result<Option<Automation>, String> {
        let _ = webhook_id;
        Ok(None)
    }

    // === Automation Execution methods ===

    /// Create an automation execution record.
    async fn create_automation_execution(
        &self,
        execution: AutomationExecution,
    ) -> Result<AutomationExecution, String> {
        let _ = execution;
        Err("Automation executions not supported by this store".to_string())
    }

    /// Update an automation execution record.
    async fn update_automation_execution(
        &self,
        execution: AutomationExecution,
    ) -> Result<(), String> {
        let _ = execution;
        Err("Automation executions not supported by this store".to_string())
    }

    /// Get execution history for an automation.
    async fn get_automation_executions(
        &self,
        automation_id: Uuid,
        limit: Option<usize>,
    ) -> Result<Vec<AutomationExecution>, String> {
        let _ = (automation_id, limit);
        Ok(vec![])
    }

    /// Get execution history for a mission.
    async fn get_mission_automation_executions(
        &self,
        mission_id: Uuid,
        limit: Option<usize>,
    ) -> Result<Vec<AutomationExecution>, String> {
        let _ = (mission_id, limit);
        Ok(vec![])
    }

    /// Complete all running automation executions for a mission, setting them
    /// to either Success or Failed based on the agent outcome.
    async fn complete_running_executions_for_mission(
        &self,
        mission_id: Uuid,
        success: bool,
        error: Option<String>,
    ) -> Result<u32, String> {
        let _ = (mission_id, success, error);
        Ok(0)
    }

    /// Update the mission mode (Task/Assistant).
    async fn update_mission_mode(&self, id: Uuid, mode: MissionMode) -> Result<(), String> {
        let _ = (id, mode);
        Err("Not supported".to_string())
    }

    /// List all missions in Assistant mode.
    async fn list_assistant_missions(&self) -> Result<Vec<Mission>, String> {
        Ok(vec![])
    }

    // === Export / Import ===

    /// Assemble a portable snapshot of a mission for transfer to another
    /// instance.
    ///
    /// Default implementation walks the public trait methods and works for
    /// any store that implements them; backends are free to override for
    /// efficiency (e.g. streaming directly from SQL).
    async fn export_mission_bundle(
        &self,
        id: Uuid,
        source_public_url: Option<String>,
    ) -> Result<MissionBundle, String> {
        let mission = self
            .get_mission(id)
            .await?
            .ok_or_else(|| format!("Mission {} not found", id))?;
        // Paginate events so we capture the full log even when the mission
        // exceeds the 50_000-event default cap used by `get_events`. Large
        // long-running missions can easily cross 100_000 events; truncating
        // silently during export would produce a bundle that looks complete
        // but isn't.
        let mut events = Vec::new();
        let page = 25_000usize;
        let mut offset = 0usize;
        loop {
            let batch = self.get_events(id, None, Some(page), Some(offset)).await?;
            let len = batch.len();
            events.extend(batch);
            if len < page {
                break;
            }
            offset += page;
        }
        let automations = self.get_mission_automations(id).await?;
        // Cap execution history at 100 per mission so bundle size doesn't
        // balloon on long-running missions. Callers that need the full log
        // can pull /api/control/missions/:id/automation-executions directly.
        let executions = self
            .get_mission_automation_executions(id, Some(100))
            .await?;
        let workspace_name = mission.workspace_name.clone();
        Ok(MissionBundle {
            version: 1,
            exported_at: Utc::now().to_rfc3339(),
            source_public_url,
            workspace_name,
            mission,
            events,
            automations,
            executions,
        })
    }

    /// Import a mission bundle, returning the newly assigned mission UUID.
    ///
    /// Default implementation is a no-op error — backends must opt in. The
    /// file/memory backends don't participate because they're debug-only.
    async fn import_mission_bundle(
        &self,
        bundle: MissionBundle,
        options: MissionImportOptions,
    ) -> Result<Uuid, String> {
        let _ = (bundle, options);
        Err("Mission import is only supported by the sqlite backend".to_string())
    }

    // === Telegram Channel methods ===

    /// Create a Telegram channel for a mission.
    async fn create_telegram_channel(
        &self,
        channel: TelegramChannel,
    ) -> Result<TelegramChannel, String> {
        let _ = channel;
        Err("Telegram channels not supported by this store".to_string())
    }

    /// Get a Telegram channel by ID.
    async fn get_telegram_channel(&self, id: Uuid) -> Result<Option<TelegramChannel>, String> {
        let _ = id;
        Ok(None)
    }

    /// List Telegram channels for a mission.
    async fn list_telegram_channels(
        &self,
        mission_id: Uuid,
    ) -> Result<Vec<TelegramChannel>, String> {
        let _ = mission_id;
        Ok(vec![])
    }

    /// List all active Telegram channels across all missions.
    async fn list_all_active_telegram_channels(&self) -> Result<Vec<TelegramChannel>, String> {
        Ok(vec![])
    }

    /// Update a Telegram channel.
    async fn update_telegram_channel(&self, channel: TelegramChannel) -> Result<(), String> {
        let _ = channel;
        Err("Telegram channels not supported by this store".to_string())
    }

    /// Delete a Telegram channel.
    async fn delete_telegram_channel(&self, id: Uuid) -> Result<bool, String> {
        let _ = id;
        Ok(false)
    }

    /// List all Telegram channels (both legacy and auto-create).
    async fn list_all_telegram_channels(&self) -> Result<Vec<TelegramChannel>, String> {
        Ok(vec![])
    }

    /// Upsert a Telegram user identity and role.
    async fn upsert_telegram_user(&self, user: TelegramUser) -> Result<TelegramUser, String> {
        let _ = user;
        Err("Not supported".to_string())
    }

    /// Get a Telegram user by Telegram user id.
    async fn get_telegram_user(
        &self,
        telegram_user_id: i64,
    ) -> Result<Option<TelegramUser>, String> {
        let _ = telegram_user_id;
        Ok(None)
    }

    /// Get or create a per-user Telegram cursor row.
    async fn get_or_create_telegram_user_cursor(
        &self,
        telegram_user_id: i64,
    ) -> Result<TelegramUserCursor, String> {
        let _ = telegram_user_id;
        Err("Not supported".to_string())
    }

    /// Update the /status cursor after successful delivery.
    async fn update_telegram_user_last_status_at(
        &self,
        telegram_user_id: i64,
        last_status_at: &str,
        last_seen_event_sequence_by_mission_json: &str,
    ) -> Result<(), String> {
        let _ = (
            telegram_user_id,
            last_status_at,
            last_seen_event_sequence_by_mission_json,
        );
        Err("Not supported".to_string())
    }

    /// Update the last successful Telegram alert digest delivery timestamp.
    async fn update_telegram_user_last_digest_at(
        &self,
        telegram_user_id: i64,
        last_digest_at: &str,
    ) -> Result<(), String> {
        let _ = (telegram_user_id, last_digest_at);
        Err("Not supported".to_string())
    }

    /// Update the last Telegram alert acknowledgement timestamp.
    async fn update_telegram_user_last_alert_ack_at(
        &self,
        telegram_user_id: i64,
        last_alert_ack_at: &str,
    ) -> Result<(), String> {
        let _ = (telegram_user_id, last_alert_ack_at);
        Err("Not supported".to_string())
    }

    /// Upsert a mission subscription/interest row.
    async fn upsert_telegram_mission_subscription(
        &self,
        subscription: TelegramMissionSubscription,
    ) -> Result<TelegramMissionSubscription, String> {
        let _ = subscription;
        Err("Not supported".to_string())
    }

    /// List mission subscriptions for a Telegram user.
    async fn list_telegram_mission_subscriptions(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<TelegramMissionSubscription>, String> {
        let _ = telegram_user_id;
        Ok(vec![])
    }

    /// Store an explicit Telegram alert preference learned from feedback.
    async fn create_telegram_alert_preference(
        &self,
        preference: TelegramAlertPreference,
    ) -> Result<TelegramAlertPreference, String> {
        let _ = preference;
        Err("Not supported".to_string())
    }

    /// List explicit Telegram alert preferences for a user.
    async fn list_telegram_alert_preferences(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<TelegramAlertPreference>, String> {
        let _ = telegram_user_id;
        Ok(vec![])
    }

    /// Insert an alert unless an equivalent alert already exists.
    async fn create_telegram_alert_if_absent(
        &self,
        alert: TelegramAlert,
    ) -> Result<Option<TelegramAlert>, String> {
        let _ = alert;
        Ok(None)
    }

    /// List pending Telegram alerts for a user.
    async fn list_pending_telegram_alerts(
        &self,
        telegram_user_id: i64,
        limit: usize,
    ) -> Result<Vec<TelegramAlert>, String> {
        let _ = (telegram_user_id, limit);
        Ok(vec![])
    }

    /// Mark an alert as sent.
    async fn mark_telegram_alert_sent(
        &self,
        id: Uuid,
        telegram_message_id: Option<i64>,
        sent_at: &str,
    ) -> Result<(), String> {
        let _ = (id, telegram_message_id, sent_at);
        Err("Not supported".to_string())
    }

    /// Find a sent Telegram alert by its Telegram message id.
    async fn get_telegram_alert_by_message_id(
        &self,
        telegram_user_id: i64,
        telegram_message_id: i64,
    ) -> Result<Option<TelegramAlert>, String> {
        let _ = (telegram_user_id, telegram_message_id);
        Ok(None)
    }

    /// Acknowledge queued alerts for a mission so they will not be delivered.
    async fn acknowledge_pending_telegram_alerts_for_mission(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        acknowledged_at: &str,
    ) -> Result<usize, String> {
        let _ = (telegram_user_id, mission_id, acknowledged_at);
        Ok(0)
    }

    /// Acknowledge a single queued alert so it will not be delivered.
    async fn acknowledge_pending_telegram_alert(
        &self,
        telegram_user_id: i64,
        alert_id: Uuid,
        acknowledged_at: &str,
    ) -> Result<bool, String> {
        let _ = (telegram_user_id, alert_id, acknowledged_at);
        Ok(false)
    }

    /// Record a delivery failure without removing the alert from the retry queue.
    async fn mark_telegram_alert_failed(&self, id: Uuid, error: &str) -> Result<(), String> {
        let _ = (id, error);
        Err("Not supported".to_string())
    }

    /// Clear stale pending alert delivery errors so future scans can retry cleanly.
    async fn recover_stale_telegram_alerts(
        &self,
        before: &str,
        limit: usize,
    ) -> Result<usize, String> {
        let _ = (before, limit);
        Ok(0)
    }

    /// Append an auditable Paloma decision record.
    async fn create_paloma_decision(
        &self,
        decision: PalomaDecision,
    ) -> Result<PalomaDecision, String> {
        let _ = decision;
        Err("Not supported".to_string())
    }

    /// List recent Paloma decisions for debugging/canary review.
    async fn list_paloma_decisions(&self, limit: usize) -> Result<Vec<PalomaDecision>, String> {
        let _ = limit;
        Ok(vec![])
    }

    /// Claim a named Paloma scheduler job if no live lease exists.
    async fn claim_paloma_scheduler_job(
        &self,
        name: &str,
        lease_owner: &str,
        now: &str,
        lease_expires_at: &str,
    ) -> Result<bool, String> {
        let _ = (name, lease_owner, now, lease_expires_at);
        Ok(false)
    }

    /// Finish a named Paloma scheduler job and append its latest result.
    async fn finish_paloma_scheduler_job(
        &self,
        name: &str,
        lease_owner: &str,
        finished_at: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        let _ = (name, lease_owner, finished_at, error);
        Ok(())
    }

    /// List named Paloma scheduler job state/history.
    async fn list_paloma_scheduler_jobs(&self) -> Result<Vec<PalomaSchedulerJob>, String> {
        Ok(vec![])
    }

    // === Paloma Mission Card (per-mission rolling Telegram message) ===

    /// Get the persistent card anchor for a mission, if any.
    async fn get_paloma_mission_card(
        &self,
        mission_id: Uuid,
    ) -> Result<Option<PalomaMissionCard>, String> {
        let _ = mission_id;
        Ok(None)
    }

    /// Insert or overwrite the card anchor for a mission. Callers use this both
    /// when posting a brand-new card (insert) and when re-anchoring after the
    /// Telegram 48-hour edit window closes (replace `message_id`).
    async fn upsert_paloma_mission_card(
        &self,
        card: PalomaMissionCard,
    ) -> Result<PalomaMissionCard, String> {
        let _ = card;
        Err("Not supported".to_string())
    }

    /// Update only the content hash, last-edit timestamp, and version counter
    /// after a successful `editMessageText` round-trip. The message_id and
    /// anchor_ts are unchanged.
    async fn touch_paloma_mission_card(
        &self,
        mission_id: Uuid,
        content_hash: &str,
        last_edit_ts: &str,
    ) -> Result<(), String> {
        let _ = (mission_id, content_hash, last_edit_ts);
        Ok(())
    }

    /// Mark a card archived. The card row is retained so callers can detect
    /// "already shown a final message for this mission", but the scheduler
    /// stops editing it.
    async fn archive_paloma_mission_card(&self, mission_id: Uuid) -> Result<(), String> {
        let _ = mission_id;
        Ok(())
    }

    /// List active (non-archived) cards for a user. Used by the scheduler to
    /// refresh anything that may have drifted.
    async fn list_active_paloma_mission_cards(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<PalomaMissionCard>, String> {
        let _ = telegram_user_id;
        Ok(vec![])
    }

    // === Paloma cooldown state (exponential backoff per mission+class) ===

    /// Get the cooldown row for a given mission + alert class + user.
    async fn get_paloma_cooldown_state(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        alert_class: &str,
    ) -> Result<Option<PalomaCooldownState>, String> {
        let _ = (telegram_user_id, mission_id, alert_class);
        Ok(None)
    }

    /// Insert or replace a cooldown row.
    async fn upsert_paloma_cooldown_state(
        &self,
        state: PalomaCooldownState,
    ) -> Result<PalomaCooldownState, String> {
        let _ = state;
        Err("Not supported".to_string())
    }

    /// Drop all cooldown rows for a mission. Called when the user replies to
    /// the mission, when status changes, or on explicit `/resume`.
    async fn reset_paloma_cooldown_for_mission(&self, mission_id: Uuid) -> Result<(), String> {
        let _ = mission_id;
        Ok(())
    }

    // === Paloma user preferences (quiet hours, rate ceiling) ===

    /// Get the preferences for a user, or `None` if they have not been set.
    /// Callers should fall back to `PalomaUserPreferences::default_for(...)`
    /// when this returns `None`.
    async fn get_paloma_user_preferences(
        &self,
        telegram_user_id: i64,
    ) -> Result<Option<PalomaUserPreferences>, String> {
        let _ = telegram_user_id;
        Ok(None)
    }

    /// Insert or replace preferences for a user. Used by the future settings
    /// UI and by `/quiet` / `/mute` Telegram commands.
    async fn upsert_paloma_user_preferences(
        &self,
        preferences: PalomaUserPreferences,
    ) -> Result<PalomaUserPreferences, String> {
        let _ = preferences;
        Err("Not supported".to_string())
    }

    /// Count the interrupt-class messages already delivered to this user
    /// within the given time window. Used by the delivery policy to enforce
    /// `max_interrupts_per_hour` and `max_interrupts_per_day`.
    async fn count_paloma_sent_alerts_since(
        &self,
        telegram_user_id: i64,
        since: &str,
    ) -> Result<i64, String> {
        let _ = (telegram_user_id, since);
        Ok(0)
    }

    /// Refresh the body / title / importance of a pending Telegram alert.
    /// Called after `create_telegram_alert_if_absent` so collapsed alert
    /// classes (e.g. `mission_long_running`) reflect the latest mission state
    /// at digest time, instead of being frozen at first insert. No-op when
    /// the row was just inserted (idempotent over-write) or already
    /// acknowledged/sent.
    async fn refresh_pending_telegram_alert_body(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        event_kind: &str,
        title: &str,
        body: &str,
        importance: &str,
    ) -> Result<bool, String> {
        let _ = (
            telegram_user_id,
            mission_id,
            event_kind,
            title,
            body,
            importance,
        );
        Ok(false)
    }

    /// Consolidate explicit Telegram memory rows, keeping the latest user-provided rule/fact.
    async fn consolidate_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<usize, String> {
        let _ = (channel_id, limit);
        Ok(0)
    }

    // === Telegram Chat-Mission mapping methods ===

    /// Look up the mission for a specific (channel, chat_id) pair.
    async fn get_telegram_chat_mission(
        &self,
        channel_id: Uuid,
        chat_id: i64,
    ) -> Result<Option<TelegramChatMission>, String> {
        let _ = (channel_id, chat_id);
        Ok(None)
    }

    /// Create a mapping from (channel, chat_id) to mission.
    async fn create_telegram_chat_mission(
        &self,
        mapping: TelegramChatMission,
    ) -> Result<TelegramChatMission, String> {
        let _ = mapping;
        Err("Not supported".to_string())
    }

    /// Update the cached title/label for a Telegram chat mapping.
    async fn update_telegram_chat_mission_title(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        chat_title: Option<String>,
    ) -> Result<(), String> {
        let _ = (channel_id, chat_id, chat_title);
        Ok(())
    }

    /// Look up the Telegram chat mapping for a given mission_id (reverse lookup).
    async fn get_telegram_chat_mission_by_mission_id(
        &self,
        mission_id: Uuid,
    ) -> Result<Option<TelegramChatMission>, String> {
        let _ = mission_id;
        Ok(None)
    }

    /// List all chat-to-mission mappings for a channel.
    async fn list_telegram_chat_missions(
        &self,
        channel_id: Uuid,
    ) -> Result<Vec<TelegramChatMission>, String> {
        let _ = channel_id;
        Ok(vec![])
    }

    /// Queue a Telegram message for immediate or delayed delivery.
    async fn create_telegram_scheduled_message(
        &self,
        message: TelegramScheduledMessage,
    ) -> Result<TelegramScheduledMessage, String> {
        let _ = message;
        Err("Not supported".to_string())
    }

    /// List pending Telegram messages that should be delivered at or before `send_at`.
    async fn list_due_telegram_scheduled_messages(
        &self,
        channel_id: Uuid,
        send_at: &str,
        limit: usize,
    ) -> Result<Vec<TelegramScheduledMessage>, String> {
        let _ = (channel_id, send_at, limit);
        Ok(vec![])
    }

    /// List recent Telegram scheduled messages for a channel.
    async fn list_telegram_scheduled_messages(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramScheduledMessage>, String> {
        let _ = (channel_id, chat_id, limit);
        Ok(vec![])
    }

    /// Atomically claim a pending scheduled message for delivery by setting
    /// status to `'sending'`. Returns `true` if the row was claimed (was
    /// still `'pending'`), `false` if another caller already claimed it.
    async fn claim_telegram_scheduled_message(&self, id: Uuid) -> Result<bool, String> {
        let _ = id;
        Err("Not supported".to_string())
    }

    /// Recover stale `'sending'` scheduled messages back to `'pending'`
    /// (e.g. after a crash). Messages in `'sending'` for longer than
    /// `max_age_secs` are reset.
    async fn recover_stale_sending_scheduled_messages(
        &self,
        max_age_secs: i64,
    ) -> Result<u32, String> {
        let _ = max_age_secs;
        Ok(0)
    }

    /// Mark a scheduled Telegram message as sent.
    async fn mark_telegram_scheduled_message_sent(
        &self,
        id: Uuid,
        sent_at: &str,
    ) -> Result<(), String> {
        let _ = (id, sent_at);
        Err("Not supported".to_string())
    }

    /// Mark a scheduled Telegram message as failed.
    async fn mark_telegram_scheduled_message_failed(
        &self,
        id: Uuid,
        error: &str,
    ) -> Result<(), String> {
        let _ = (id, error);
        Err("Not supported".to_string())
    }

    /// Upsert a Telegram structured memory entry.
    async fn upsert_telegram_structured_memory(
        &self,
        entry: TelegramStructuredMemoryEntry,
    ) -> Result<TelegramStructuredMemoryEntry, String> {
        let _ = entry;
        Err("Not supported".to_string())
    }

    /// List recent Telegram structured memory entries for a channel/chat.
    async fn list_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        subject_user_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let _ = (channel_id, chat_id, subject_user_id, limit);
        Ok(vec![])
    }

    /// Search Telegram structured memory for a channel/chat.
    async fn search_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let _ = (channel_id, chat_id, query, limit);
        Ok(vec![])
    }

    /// Hybrid-search Telegram structured memory with scored matches.
    async fn search_telegram_structured_memory_hybrid(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        subject_user_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemorySearchHit>, String> {
        let _ = (channel_id, chat_id, subject_user_id, query, limit);
        Ok(vec![])
    }

    /// Load memory context relevant to a Telegram chat and optional sender identity.
    async fn list_telegram_memory_context(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        subject_user_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let _ = (channel_id, chat_id, subject_user_id, limit);
        Ok(vec![])
    }

    /// Search memory context relevant to a Telegram chat and optional sender identity.
    async fn search_telegram_memory_context(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        subject_user_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let _ = (channel_id, chat_id, subject_user_id, query, limit);
        Ok(vec![])
    }

    /// Hybrid-search memory context relevant to a Telegram chat and optional sender identity.
    async fn search_telegram_memory_context_hybrid(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        subject_user_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemorySearchHit>, String> {
        self.search_telegram_structured_memory_hybrid(
            channel_id,
            Some(chat_id),
            subject_user_id,
            query,
            limit,
        )
        .await
    }

    /// Record a Telegram action execution for observability/admin tooling.
    async fn create_telegram_action_execution(
        &self,
        execution: TelegramActionExecution,
    ) -> Result<TelegramActionExecution, String> {
        let _ = execution;
        Err("Not supported".to_string())
    }

    /// List recent Telegram action executions for a channel.
    async fn list_telegram_action_executions(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramActionExecution>, String> {
        let _ = (channel_id, chat_id, limit);
        Ok(vec![])
    }

    /// Update action execution status by the linked scheduled message.
    async fn mark_telegram_action_execution_by_scheduled_message(
        &self,
        scheduled_message_id: Uuid,
        status: TelegramActionExecutionStatus,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<(), String> {
        let _ = (scheduled_message_id, status, last_error, updated_at);
        Err("Not supported".to_string())
    }

    /// Upsert a Telegram conversation (one per channel/chat).
    async fn upsert_telegram_conversation(
        &self,
        conversation: TelegramConversation,
    ) -> Result<TelegramConversation, String> {
        let _ = conversation;
        Err("Not supported".to_string())
    }

    /// Get a Telegram conversation by (channel, chat).
    async fn get_telegram_conversation_by_chat(
        &self,
        channel_id: Uuid,
        chat_id: i64,
    ) -> Result<Option<TelegramConversation>, String> {
        let _ = (channel_id, chat_id);
        Ok(None)
    }

    /// List recent Telegram conversations for a channel.
    async fn list_telegram_conversations(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramConversation>, String> {
        let _ = (channel_id, limit);
        Ok(vec![])
    }

    /// Append a message to the Telegram conversation log.
    async fn create_telegram_conversation_message(
        &self,
        message: TelegramConversationMessage,
    ) -> Result<TelegramConversationMessage, String> {
        let _ = message;
        Err("Not supported".to_string())
    }

    /// List recent messages for a Telegram conversation.
    async fn list_telegram_conversation_messages(
        &self,
        conversation_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramConversationMessage>, String> {
        let _ = (conversation_id, limit);
        Ok(vec![])
    }

    /// Timeout stale WaitingExternal Telegram workflows older than `max_age_secs`.
    /// Returns the number of workflows timed out.
    async fn timeout_stale_telegram_workflows(&self, max_age_secs: i64) -> Result<u32, String> {
        let _ = max_age_secs;
        Ok(0)
    }

    /// Register a Telegram webhook update for dedup. Returns true if the
    /// update was not seen before (first occurrence).
    async fn register_webhook_update(
        &self,
        channel_id: Uuid,
        update_id: i64,
    ) -> Result<bool, String> {
        let _ = (channel_id, update_id);
        Ok(true)
    }

    /// Remove webhook dedup entries older than `max_age_secs`.
    async fn cleanup_webhook_dedup(&self, max_age_secs: i64) -> Result<u32, String> {
        let _ = max_age_secs;
        Ok(0)
    }

    /// Create a Telegram workflow.
    async fn create_telegram_workflow(
        &self,
        workflow: TelegramWorkflow,
    ) -> Result<TelegramWorkflow, String> {
        let _ = workflow;
        Err("Not supported".to_string())
    }

    /// Update a Telegram workflow.
    async fn update_telegram_workflow(&self, workflow: TelegramWorkflow) -> Result<(), String> {
        let _ = workflow;
        Err("Not supported".to_string())
    }

    /// List recent Telegram workflows for a channel.
    async fn list_telegram_workflows(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramWorkflow>, String> {
        let _ = (channel_id, limit);
        Ok(vec![])
    }

    /// Find the newest workflow waiting on a specific target chat.
    async fn get_pending_telegram_workflow_for_target_chat(
        &self,
        channel_id: Uuid,
        target_chat_id: i64,
    ) -> Result<Option<TelegramWorkflow>, String> {
        let _ = (channel_id, target_chat_id);
        Ok(None)
    }

    /// Get a pending Telegram workflow for a target chat that expects a reply to a specific request message.
    async fn get_pending_telegram_workflow_for_target_message(
        &self,
        channel_id: Uuid,
        target_chat_id: i64,
        request_message_id: i64,
    ) -> Result<Option<TelegramWorkflow>, String> {
        let _ = (channel_id, target_chat_id, request_message_id);
        Ok(None)
    }

    /// Append an event to a Telegram workflow.
    async fn create_telegram_workflow_event(
        &self,
        event: TelegramWorkflowEvent,
    ) -> Result<TelegramWorkflowEvent, String> {
        let _ = event;
        Err("Not supported".to_string())
    }

    /// List recent events for a Telegram workflow.
    async fn list_telegram_workflow_events(
        &self,
        workflow_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramWorkflowEvent>, String> {
        let _ = (workflow_id, limit);
        Ok(vec![])
    }

    // ---- Task board ------------------------------------------------------

    /// Register or update tasks on a boss mission's board. Tasks are keyed by
    /// `(boss_mission_id, task_key)`: a new key inserts, an existing key in
    /// `pending` status is updated in place, and any other status is left
    /// untouched (the current row is returned so the caller sees the real
    /// state). Returns the post-upsert rows in input order.
    async fn upsert_board_tasks(
        &self,
        boss_mission_id: Uuid,
        tasks: Vec<NewBoardTask>,
    ) -> Result<Vec<BoardTask>, String> {
        let _ = (boss_mission_id, tasks);
        Err("Task board not supported by this mission store".to_string())
    }

    /// All tasks on a boss mission's board, oldest first.
    async fn list_board_tasks(&self, boss_mission_id: Uuid) -> Result<Vec<BoardTask>, String> {
        let _ = boss_mission_id;
        Ok(vec![])
    }

    /// Boss mission ids that have at least one non-terminal task. Drives the
    /// scheduler's per-tick scan.
    async fn list_active_board_missions(&self) -> Result<Vec<Uuid>, String> {
        Ok(vec![])
    }

    async fn get_board_task(&self, task_id: Uuid) -> Result<Option<BoardTask>, String> {
        let _ = task_id;
        Ok(None)
    }

    /// Look up the task currently bound to a worker mission, if any.
    async fn get_board_task_by_worker(
        &self,
        worker_mission_id: Uuid,
    ) -> Result<Option<BoardTask>, String> {
        let _ = worker_mission_id;
        Ok(None)
    }

    /// Persist the full state of a task (matched by `task.id`).
    async fn save_board_task(&self, task: &BoardTask) -> Result<(), String> {
        let _ = task;
        Err("Task board not supported by this mission store".to_string())
    }

    /// Persist the control session's pending message queue as a JSON snapshot
    /// so queued messages survive a server restart. Default no-op for
    /// non-durable stores (memory/file); SQLite overrides this.
    async fn save_control_queue(&self, user_id: &str, payload: &str) -> Result<(), String> {
        let _ = (user_id, payload);
        Ok(())
    }

    /// Load the persisted control-queue snapshot (empty string if none).
    async fn load_control_queue(&self, user_id: &str) -> Result<String, String> {
        let _ = user_id;
        Ok(String::new())
    }
}

/// Mission store type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissionStoreType {
    Memory,
    File,
    #[default]
    Sqlite,
}

impl MissionStoreType {
    /// Parse from environment variable value.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "memory" => Self::Memory,
            "file" | "json" => Self::File,
            "sqlite" | "db" => Self::Sqlite,
            _ => Self::default(),
        }
    }
}

impl std::str::FromStr for MissionStoreType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_str(s))
    }
}

/// Create a mission store based on type and configuration.
pub async fn create_mission_store(
    store_type: MissionStoreType,
    base_dir: PathBuf,
    user_id: &str,
) -> Result<Box<dyn MissionStore>, String> {
    match store_type {
        MissionStoreType::Memory => Ok(Box::new(InMemoryMissionStore::new())),
        MissionStoreType::File => {
            let store = FileMissionStore::new(base_dir, user_id).await?;
            Ok(Box::new(store))
        }
        MissionStoreType::Sqlite => {
            let store = SqliteMissionStore::new(base_dir, user_id).await?;
            Ok(Box::new(store))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that missions are created with Pending status (not Active).
    /// This is critical to prevent the race condition where startup recovery
    /// marks newly created missions as interrupted.
    #[tokio::test]
    async fn test_mission_created_with_pending_status() {
        let store = InMemoryMissionStore::new();

        let mission = store
            .create_mission(Some("Test Mission"), None, None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(
            mission.status,
            MissionStatus::Pending,
            "New missions should have Pending status, not {:?}",
            mission.status
        );
    }

    /// Test that Pending missions are NOT returned by get_all_active_missions.
    /// This ensures the orphan detection won't mark Pending missions as interrupted.
    #[tokio::test]
    async fn test_pending_missions_not_in_active_list() {
        let store = InMemoryMissionStore::new();

        // Create a pending mission
        let mission = store
            .create_mission(Some("Pending Mission"), None, None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(mission.status, MissionStatus::Pending);

        // get_all_active_missions should NOT include pending missions
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        assert!(
            active_missions.is_empty(),
            "Pending missions should not appear in active missions list"
        );
    }

    /// FLEET-001: a Pending mission appears in the scheduled-pending list only
    /// while it carries a deferred goal, and disappears once dispatched (Active)
    /// or the goal is cleared.
    #[tokio::test]
    async fn test_scheduled_pending_missions_track_deferred_goal() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Scheduled"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        // No goal yet -> not scheduled-pending.
        assert!(store
            .get_scheduled_pending_missions()
            .await
            .expect("list")
            .is_empty());

        // Stash a goal -> appears, with the goal readable.
        store
            .set_deferred_goal(mission.id, Some("do the thing".to_string()))
            .await
            .expect("set goal");
        assert_eq!(
            store
                .get_deferred_goal(mission.id)
                .await
                .expect("get goal")
                .as_deref(),
            Some("do the thing")
        );
        let pending = store.get_scheduled_pending_missions().await.expect("list");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, mission.id);

        // Dispatched (Active) -> no longer scheduled-pending even with goal set.
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("activate");
        assert!(store
            .get_scheduled_pending_missions()
            .await
            .expect("list")
            .is_empty());

        // Back to Pending but goal cleared -> still not scheduled-pending.
        store
            .update_mission_status(mission.id, MissionStatus::Pending)
            .await
            .expect("repend");
        store
            .set_deferred_goal(mission.id, None)
            .await
            .expect("clear goal");
        assert!(store
            .get_deferred_goal(mission.id)
            .await
            .expect("get goal")
            .is_none());
        assert!(store
            .get_scheduled_pending_missions()
            .await
            .expect("list")
            .is_empty());
    }

    /// FLEET-004: paused_at round-trips through the store (set on pause, read
    /// back on the mission, cleared on resume).
    #[tokio::test]
    async fn test_paused_at_round_trip() {
        let store = InMemoryMissionStore::new();
        let mission = store
            .create_mission(Some("Pausable"), None, None, None, None, None, None)
            .await
            .expect("create");
        assert!(mission.paused_at.is_none());

        store
            .set_mission_paused_at(mission.id, Some("2026-06-25T05:00:00+00:00".to_string()))
            .await
            .expect("set paused_at");
        let loaded = store
            .get_mission(mission.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(
            loaded.paused_at.as_deref(),
            Some("2026-06-25T05:00:00+00:00")
        );

        store
            .set_mission_paused_at(mission.id, None)
            .await
            .expect("clear paused_at");
        assert!(store
            .get_mission(mission.id)
            .await
            .expect("get")
            .expect("some")
            .paused_at
            .is_none());
    }

    /// Test that missions transition correctly from Pending to Active.
    #[tokio::test]
    async fn test_mission_status_transition_pending_to_active() {
        let store = InMemoryMissionStore::new();

        // Create a pending mission
        let mission = store
            .create_mission(Some("Test Mission"), None, None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        assert_eq!(mission.status, MissionStatus::Pending);

        // Update status to Active
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("Failed to update status");

        // Verify status changed
        let updated = store
            .get_mission(mission.id)
            .await
            .expect("Failed to get mission")
            .expect("Mission not found");

        assert_eq!(
            updated.status,
            MissionStatus::Active,
            "Mission status should be Active after update"
        );

        // Now it should appear in active missions
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        assert_eq!(
            active_missions.len(),
            1,
            "Active mission should appear in active missions list"
        );
        assert_eq!(active_missions[0].id, mission.id);
    }

    /// Test the orphan detection scenario: Active missions should be detected,
    /// but Pending missions should not.
    #[tokio::test]
    async fn test_orphan_detection_ignores_pending() {
        let store = InMemoryMissionStore::new();

        // Create two missions
        let pending_mission = store
            .create_mission(Some("Pending"), None, None, None, None, None, None)
            .await
            .expect("Failed to create pending mission");

        let active_mission = store
            .create_mission(Some("Will be Active"), None, None, None, None, None, None)
            .await
            .expect("Failed to create mission");

        // Activate only one mission
        store
            .update_mission_status(active_mission.id, MissionStatus::Active)
            .await
            .expect("Failed to activate mission");

        // Check active missions (simulating orphan detection)
        let active_missions = store
            .get_all_active_missions()
            .await
            .expect("Failed to get active missions");

        // Only the active mission should be in the list
        assert_eq!(
            active_missions.len(),
            1,
            "Only Active missions should be returned, not Pending ones"
        );
        assert_eq!(active_missions[0].id, active_mission.id);

        // Pending mission should still exist but not be in active list
        let pending = store
            .get_mission(pending_mission.id)
            .await
            .expect("Failed to get pending mission")
            .expect("Pending mission not found");
        assert_eq!(pending.status, MissionStatus::Pending);
    }

    /// Test MissionStatus Display implementation includes Pending.
    #[test]
    fn test_mission_status_display() {
        assert_eq!(format!("{}", MissionStatus::Pending), "pending");
        assert_eq!(format!("{}", MissionStatus::Active), "active");
        assert_eq!(format!("{}", MissionStatus::Completed), "completed");
        assert_eq!(format!("{}", MissionStatus::Interrupted), "interrupted");
    }

    // ---- FLEET-001 / FLEET-004 scheduling tests ----

    /// Build a minimal Pending mission with the given scheduling inputs for
    /// the dispatcher tests.
    fn scheduling_mission(
        created_at: &str,
        priority: i32,
        not_before: Option<&str>,
        status: MissionStatus,
    ) -> Mission {
        Mission {
            id: Uuid::new_v4(),
            status,
            title: None,
            short_description: None,
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: default_workspace_id(),
            workspace_name: None,
            agent: None,
            model_override: None,
            model_effort: None,
            backend: default_backend(),
            config_profile: None,
            history: vec![],
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            interrupted_at: None,
            paused_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
            first_viewed_at: None,
            scheduling: MissionScheduling {
                priority,
                not_before: not_before.map(|s| s.to_string()),
                deadline: None,
            },
            project: MissionProject::default(),
            activity: MissionActivity::default(),
            awaiting_kind: None,
        }
    }

    /// FLEET-004: Paused is a non-terminal status (unlike Blocked), so a paused
    /// mission can later be resumed.
    #[test]
    fn test_paused_not_terminal() {
        use crate::api::control::mission_status_is_terminal;
        assert!(!mission_status_is_terminal(MissionStatus::Paused));
        // Sanity: a genuinely terminal status still reports terminal.
        assert!(mission_status_is_terminal(MissionStatus::Completed));
        assert!(mission_status_is_terminal(MissionStatus::Blocked));
    }

    /// FLEET-001: higher priority wins; ties broken by oldest created_at (FIFO).
    #[test]
    fn test_priority_sorting() {
        let now = Utc::now();
        let missions = vec![
            scheduling_mission("2026-01-01T00:00:00Z", 0, None, MissionStatus::Pending),
            scheduling_mission("2026-01-01T01:00:00Z", 5, None, MissionStatus::Pending),
            scheduling_mission("2026-01-01T02:00:00Z", 5, None, MissionStatus::Pending),
        ];
        let next = select_next_runnable_mission(&missions, now).expect("a runnable mission");
        // Highest priority is 5; between the two priority-5 missions the older
        // (01:00) wins over the newer (02:00).
        assert_eq!(next.scheduling.priority, 5);
        assert_eq!(next.created_at, "2026-01-01T01:00:00Z");
    }

    /// FLEET-001: missions with `not_before` in the future are not runnable;
    /// `Paused` missions are excluded even when otherwise eligible.
    #[test]
    fn test_not_before_filtering() {
        let now = Utc::now();
        let future = (now + chrono::Duration::hours(1)).to_rfc3339();
        let past = (now - chrono::Duration::hours(1)).to_rfc3339();

        // Only a future not_before -> nothing runnable.
        let blocked = vec![scheduling_mission(
            "2026-01-01T00:00:00Z",
            10,
            Some(&future),
            MissionStatus::Pending,
        )];
        assert!(select_next_runnable_mission(&blocked, now).is_none());

        // A past not_before is runnable; a higher-priority Paused mission is
        // skipped; a higher-priority future mission is skipped.
        let mixed = vec![
            scheduling_mission(
                "2026-01-01T00:00:00Z",
                1,
                Some(&past),
                MissionStatus::Pending,
            ),
            scheduling_mission("2026-01-01T00:00:00Z", 99, None, MissionStatus::Paused),
            scheduling_mission(
                "2026-01-01T00:00:00Z",
                99,
                Some(&future),
                MissionStatus::Pending,
            ),
        ];
        let next = select_next_runnable_mission(&mixed, now).expect("the past-eligible mission");
        assert_eq!(next.scheduling.priority, 1);
        assert!(next.is_dispatchable_at(now));
    }

    /// FLEET-001: deadline helper reports elapsed deadlines and ignores unset.
    #[test]
    fn test_deadline_detection() {
        let now = Utc::now();
        let mut m = scheduling_mission("2026-01-01T00:00:00Z", 0, None, MissionStatus::Pending);
        assert!(!m.is_past_deadline(now));
        m.scheduling.deadline = Some((now - chrono::Duration::minutes(1)).to_rfc3339());
        assert!(m.is_past_deadline(now));
        m.scheduling.deadline = Some((now + chrono::Duration::minutes(1)).to_rfc3339());
        assert!(!m.is_past_deadline(now));
    }
}
