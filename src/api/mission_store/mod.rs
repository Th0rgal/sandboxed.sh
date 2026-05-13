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
}

fn default_backend() -> String {
    "claudecode".to_string()
}

fn default_workspace_id() -> Uuid {
    crate::workspace::DEFAULT_WORKSPACE_ID
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

    /// Update mission session ID (for backends like Amp that generate their own IDs).
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

    /// Count events for a mission, optionally filtered by type.
    async fn count_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
    ) -> Result<usize, String> {
        let _ = (mission_id, event_types);
        Ok(0)
    }

    /// Return the highest `sequence` value for this mission, or 0 if
    /// the mission has no events yet.
    async fn max_event_sequence(&self, mission_id: Uuid) -> Result<i64, String> {
        let _ = mission_id;
        Ok(0)
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
}
