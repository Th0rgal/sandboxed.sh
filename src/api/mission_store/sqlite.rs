//! SQLite-based mission store with full event logging.

use super::{
    now_string, sanitize_filename, Automation, AutomationExecution, BoardTask, BoardTaskOutcome,
    BoardTaskStatus, CommandSource, DailyUsageStats, ExecutionStatus, FreshSession,
    HourlyUsageStats, Mission, MissionHistoryEntry, MissionMode, MissionScheduling, MissionStatus,
    MissionStatusCounts, MissionStore, ModelUsageStats, NewBoardTask, PalomaCooldownState,
    PalomaDecision, PalomaMissionCard, PalomaSchedulerJob, PalomaUserPreferences, RetryConfig,
    StopPolicy, StoredEvent, TelegramActionExecution, TelegramActionExecutionKind,
    TelegramActionExecutionStatus, TelegramAlert, TelegramAlertPreference, TelegramChannel,
    TelegramChatMission, TelegramConversation, TelegramConversationMessage,
    TelegramConversationMessageDirection, TelegramMissionInterestLevel,
    TelegramMissionSubscription, TelegramScheduledMessage, TelegramScheduledMessageStatus,
    TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind, TelegramStructuredMemoryScope,
    TelegramStructuredMemorySearchHit, TelegramUser, TelegramUserCursor, TelegramUserRole,
    TelegramWorkflow, TelegramWorkflowEvent, TelegramWorkflowKind, TelegramWorkflowStatus,
    ToolCallSummary, TriggerType, WebhookConfig,
};
use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo, TextOp};
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

type LegacyAutomationRow = (String, String, String, i64, i64, String, Option<String>);
const COST_CURRENCY_USD: &str = "USD";
const METADATA_SOURCE_USER: &str = "user";
const TELEGRAM_MEMORY_SEARCH_MAX_CANDIDATES: usize = 256;
const TELEGRAM_MEMORY_SEARCH_STOPWORDS: &[&str] = &[
    "a",
    "ai",
    "alors",
    "and",
    "au",
    "aux",
    "avec",
    "ce",
    "ces",
    "comment",
    "dans",
    "de",
    "des",
    "do",
    "does",
    "du",
    "elle",
    "en",
    "est",
    "et",
    "for",
    "how",
    "i",
    "il",
    "is",
    "je",
    "la",
    "le",
    "les",
    "ma",
    "me",
    "mes",
    "mi",
    "mon",
    "my",
    "of",
    "on",
    "ou",
    "où",
    "par",
    "pas",
    "pour",
    "prefere",
    "préféré",
    "prefere",
    "preference",
    "preferences",
    "preference",
    "preferences",
    "que",
    "quel",
    "quelle",
    "quelles",
    "quels",
    "qui",
    "remember",
    "rappelle",
    "rappelle-moi",
    "souviens",
    "sur",
    "the",
    "to",
    "toi",
    "tu",
    "un",
    "une",
    "user",
    "veux",
    "veux-tu",
    "what",
    "when",
    "where",
    "with",
    "you",
];

fn usage_cost_with_read_side_estimate(
    model: &str,
    stored_cost_cents: u64,
    cost_source: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> u64 {
    if stored_cost_cents > 0 && cost_source == "actual" {
        return stored_cost_cents;
    }
    let estimated = crate::cost::cost_cents_from_usage(
        model,
        &crate::cost::TokenUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: Some(cache_creation_tokens),
            cache_read_input_tokens: Some(cache_read_tokens),
        },
    );
    if estimated > 0 {
        return estimated;
    }
    stored_cost_cents
}

fn usage_model_key(raw_model: &str, stored_normalized_model: &str) -> String {
    let raw_model = raw_model.trim();
    if !raw_model.is_empty() {
        return crate::cost::normalized_model(raw_model);
    }
    stored_normalized_model.trim().to_string()
}

#[derive(serde::Serialize)]
struct AssistantCostMetadata {
    amount_cents: u64,
    currency: &'static str,
    source: crate::agents::CostSource,
}

#[derive(serde::Serialize)]
struct AssistantMessageMetadata {
    success: bool,
    cost_cents: u64,
    cost: AssistantCostMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<crate::cost::TokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_normalized: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_files: Option<Vec<crate::api::control::SharedFile>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_evidence: Option<crate::agents::CompletionEvidence>,
}

struct AssistantMessageMetadataInput<'a> {
    success: bool,
    cost_cents: u64,
    cost_source: crate::agents::CostSource,
    usage: &'a Option<crate::cost::TokenUsage>,
    model: &'a Option<String>,
    model_normalized: &'a Option<String>,
    shared_files: &'a Option<Vec<crate::api::control::SharedFile>>,
    resumable: bool,
    completion_evidence: &'a Option<crate::agents::CompletionEvidence>,
}

fn assistant_message_metadata(input: AssistantMessageMetadataInput<'_>) -> serde_json::Value {
    let metadata = AssistantMessageMetadata {
        success: input.success,
        cost_cents: input.cost_cents,
        cost: AssistantCostMetadata {
            amount_cents: input.cost_cents,
            currency: COST_CURRENCY_USD,
            source: input.cost_source,
        },
        usage: input.usage.clone(),
        model: input.model.clone(),
        model_normalized: input.model_normalized.clone(),
        shared_files: input.shared_files.clone(),
        resumable: input.resumable,
        completion_evidence: input.completion_evidence.clone(),
    };
    serde_json::to_value(metadata).expect("assistant metadata should serialize")
}

fn fold_search_char(ch: char) -> char {
    match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => ch,
    }
}

fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut prev_space = true;
    for ch in value.chars().flat_map(|ch| ch.to_lowercase()) {
        let folded = fold_search_char(ch);
        if folded.is_ascii_alphanumeric() {
            normalized.push(folded);
            prev_space = false;
        } else if !prev_space {
            normalized.push(' ');
            prev_space = true;
        }
    }
    normalized.trim().to_string()
}

fn tokenize_search_text(value: &str) -> Vec<String> {
    normalize_search_text(value)
        .split_whitespace()
        .filter(|token| token.len() >= 2 && !TELEGRAM_MEMORY_SEARCH_STOPWORDS.contains(token))
        .map(ToOwned::to_owned)
        .collect()
}

fn build_telegram_memory_search_text(entry: &TelegramStructuredMemoryEntry) -> String {
    let mut parts = Vec::new();
    if let Some(label) = entry.label.as_deref() {
        parts.push(label);
    }
    parts.push(entry.value.as_str());
    if let Some(display_name) = entry.subject_display_name.as_deref() {
        parts.push(display_name);
    }
    if let Some(username) = entry.subject_username.as_deref() {
        parts.push(username);
    }
    match entry.kind {
        TelegramStructuredMemoryKind::Fact => parts.push("fact"),
        TelegramStructuredMemoryKind::Preference => parts.push("preference"),
        TelegramStructuredMemoryKind::Task => parts.push("task"),
        TelegramStructuredMemoryKind::Note => parts.push("note"),
    }
    normalize_search_text(&parts.join(" "))
}

fn build_fts_query_from_tokens(tokens: &[String], normalized_query: &str) -> Option<String> {
    if !tokens.is_empty() {
        return Some(
            tokens
                .iter()
                .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }

    if normalized_query.is_empty() {
        None
    } else {
        Some(format!("\"{}\"", normalized_query.replace('"', "\"\"")))
    }
}

fn scope_rank(scope: &TelegramStructuredMemoryScope) -> i32 {
    match scope {
        TelegramStructuredMemoryScope::User => 0,
        TelegramStructuredMemoryScope::Chat => 1,
        TelegramStructuredMemoryScope::Channel => 2,
    }
}

fn scope_score(scope: &TelegramStructuredMemoryScope) -> f64 {
    match scope {
        TelegramStructuredMemoryScope::User => 6.0,
        TelegramStructuredMemoryScope::Chat => 5.0,
        TelegramStructuredMemoryScope::Channel => 3.0,
    }
}

fn recency_score(updated_at: &str) -> f64 {
    let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(updated_at) else {
        return 0.0;
    };
    let age_hours = (Utc::now() - updated_at.with_timezone(&Utc))
        .num_hours()
        .max(0) as f64;
    if age_hours <= 24.0 {
        4.0
    } else if age_hours <= 24.0 * 7.0 {
        2.0
    } else if age_hours <= 24.0 * 30.0 {
        1.0
    } else {
        0.0
    }
}

fn score_memory_entry(
    entry: &TelegramStructuredMemoryEntry,
    normalized_query: &str,
    core_query: &str,
    query_tokens: &[String],
    fts_score: Option<f64>,
) -> Option<TelegramStructuredMemorySearchHit> {
    if normalized_query.is_empty() {
        return None;
    }

    let normalized_label = entry
        .label
        .as_deref()
        .map(normalize_search_text)
        .unwrap_or_default();
    let normalized_value = normalize_search_text(&entry.value);
    let normalized_subject = normalize_search_text(
        &[
            entry.subject_display_name.as_deref().unwrap_or_default(),
            entry.subject_username.as_deref().unwrap_or_default(),
        ]
        .join(" "),
    );
    let search_text = build_telegram_memory_search_text(entry);
    let entry_tokens: HashSet<String> = tokenize_search_text(&search_text).into_iter().collect();

    let mut score = 0.0;
    let mut reasons = Vec::new();
    let mut matched_terms = Vec::new();

    if !core_query.is_empty() {
        if !normalized_label.is_empty() && normalized_label == core_query {
            score += 95.0;
            reasons.push("exact_core_label".to_string());
        } else if !normalized_label.is_empty() && normalized_label.contains(core_query) {
            score += 48.0;
            reasons.push("label_contains_core_query".to_string());
        }

        if normalized_value == core_query {
            score += 80.0;
            reasons.push("exact_core_value".to_string());
        } else if normalized_value.contains(core_query) {
            score += 42.0;
            reasons.push("value_contains_core_query".to_string());
        }
    }

    if !normalized_label.is_empty() && normalized_label == normalized_query {
        score += 80.0;
        reasons.push("exact_label".to_string());
    } else if !normalized_label.is_empty() && normalized_label.contains(normalized_query) {
        score += 40.0;
        reasons.push("label_contains_query".to_string());
    }

    if normalized_value == normalized_query {
        score += 70.0;
        reasons.push("exact_value".to_string());
    } else if normalized_value.contains(normalized_query) {
        score += 35.0;
        reasons.push("value_contains_query".to_string());
    }

    if !normalized_subject.is_empty() && normalized_subject.contains(normalized_query) {
        score += 24.0;
        reasons.push("subject_contains_query".to_string());
    }

    if !normalized_label.is_empty()
        && normalized_label.split_whitespace().count() == 1
        && TELEGRAM_MEMORY_SEARCH_STOPWORDS.contains(&normalized_label.as_str())
    {
        score -= 36.0;
        reasons.push("suspicious_label".to_string());
    }

    let mut token_matches = 0usize;
    for token in query_tokens {
        if entry_tokens.contains(token) {
            token_matches += 1;
            matched_terms.push(token.clone());
        }
    }
    if token_matches > 0 {
        let ratio = token_matches as f64 / query_tokens.len().max(1) as f64;
        score += 30.0 * ratio;
        reasons.push("token_overlap".to_string());
    }

    if let Some(fts_score) = fts_score {
        score += fts_score;
        reasons.push("full_text".to_string());
    }

    // Require at least one relevance signal (token overlap or FTS match)
    // before adding scope/recency bonuses. Without this gate, every
    // candidate receives a positive base score from scope+recency alone.
    if score <= 0.0 {
        return None;
    }

    score += scope_score(&entry.scope);
    score += recency_score(&entry.updated_at);

    matched_terms.sort();
    matched_terms.dedup();
    reasons.sort();
    reasons.dedup();

    Some(TelegramStructuredMemorySearchHit {
        entry: entry.clone(),
        score,
        matched_terms,
        reasons,
    })
}

/// Parse a UUID from a database string, logging a warning and falling back to
/// the nil UUID when the value is malformed.  This prevents silent data
/// corruption that `Uuid::parse_str(...).unwrap_or_default()` would introduce
/// without any diagnostic.
fn parse_uuid_or_nil(raw: &str) -> Uuid {
    Uuid::parse_str(raw).unwrap_or_else(|e| {
        tracing::warn!(
            raw_value = %raw,
            error = %e,
            "Corrupt UUID in database; substituting nil UUID"
        );
        Uuid::nil()
    })
}

fn apply_text_ops(buffer: &mut String, ops: &[TextOp]) {
    for op in ops {
        match op {
            TextOp::Insert { pos, text } => {
                let mut chars: Vec<char> = buffer.chars().collect();
                let pos = (*pos).min(chars.len());
                chars.splice(pos..pos, text.chars());
                *buffer = chars.into_iter().collect();
            }
            TextOp::Replace { range, text } => {
                let mut chars: Vec<char> = buffer.chars().collect();
                let start = range.0.min(chars.len());
                let end = range.1.min(chars.len()).max(start);
                chars.splice(start..end, text.chars());
                *buffer = chars.into_iter().collect();
            }
            TextOp::Finalize => {}
        }
    }
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS missions (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    title TEXT,
    short_description TEXT,
    metadata_updated_at TEXT,
    metadata_source TEXT,
    metadata_model TEXT,
    metadata_version TEXT,
    workspace_id TEXT NOT NULL,
    workspace_name TEXT,
    agent TEXT,
    model_override TEXT,
    model_effort TEXT,
    backend TEXT NOT NULL DEFAULT 'opencode',
    config_profile TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    interrupted_at TEXT,
    resumable INTEGER NOT NULL DEFAULT 0,
    desktop_sessions TEXT,
    terminal_reason TEXT,
    first_viewed_at TEXT,
    priority INTEGER NOT NULL DEFAULT 0,
    not_before TEXT,
    deadline TEXT,
    deferred_goal TEXT,
    paused_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_missions_updated_at ON missions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_missions_status ON missions(status);
CREATE INDEX IF NOT EXISTS idx_missions_status_updated ON missions(status, updated_at);

CREATE TABLE IF NOT EXISTS mission_trees (
    mission_id TEXT PRIMARY KEY NOT NULL,
    tree_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS mission_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    event_id TEXT,
    tool_call_id TEXT,
    tool_name TEXT,
    content TEXT,
    content_file TEXT,
    metadata TEXT,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_events_mission ON mission_events(mission_id, sequence);
CREATE INDEX IF NOT EXISTS idx_events_type ON mission_events(mission_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_mission_type_sequence ON mission_events(mission_id, event_type, sequence);
CREATE INDEX IF NOT EXISTS idx_events_tool_call ON mission_events(tool_call_id) WHERE tool_call_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_event_type ON mission_events(event_type);
-- Stale-mission detection takes MAX(timestamp) per mission. Sequence ordering
-- doesn't help here because in-place updates (e.g. text_delta_latest rewriting
-- an existing event_id row) bump `timestamp` without changing `sequence`.
CREATE INDEX IF NOT EXISTS idx_events_mission_timestamp ON mission_events(mission_id, timestamp DESC);

CREATE TABLE IF NOT EXISTS mission_summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mission_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    key_files TEXT,
    success INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_summaries_mission ON mission_summaries(mission_id);

CREATE TABLE IF NOT EXISTS automations (
    id TEXT PRIMARY KEY NOT NULL,
    mission_id TEXT NOT NULL,
    command_source_type TEXT NOT NULL,
    command_source_data TEXT NOT NULL,
    trigger_type TEXT NOT NULL,
    trigger_data TEXT NOT NULL,
    variables TEXT NOT NULL DEFAULT '{}',
    active INTEGER NOT NULL DEFAULT 1,
    stop_policy TEXT NOT NULL DEFAULT 'consecutive_failures:2',
    fresh_session TEXT NOT NULL DEFAULT 'keep',
    driver TEXT NOT NULL DEFAULT 'scheduler',
    created_at TEXT NOT NULL,
    last_triggered_at TEXT,
    retry_max_retries INTEGER NOT NULL DEFAULT 3,
    retry_delay_seconds INTEGER NOT NULL DEFAULT 60,
    retry_backoff_multiplier REAL NOT NULL DEFAULT 2.0,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_automations_mission ON automations(mission_id);
CREATE INDEX IF NOT EXISTS idx_automations_active ON automations(mission_id, active);

CREATE TABLE IF NOT EXISTS automation_executions (
    id TEXT PRIMARY KEY NOT NULL,
    automation_id TEXT NOT NULL,
    mission_id TEXT NOT NULL,
    triggered_at TEXT NOT NULL,
    trigger_source TEXT NOT NULL,
    status TEXT NOT NULL,
    webhook_payload TEXT,
    variables_used TEXT NOT NULL DEFAULT '{}',
    completed_at TEXT,
    error TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (automation_id) REFERENCES automations(id) ON DELETE CASCADE,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_executions_automation ON automation_executions(automation_id, triggered_at DESC);
CREATE INDEX IF NOT EXISTS idx_executions_mission ON automation_executions(mission_id, triggered_at DESC);
CREATE INDEX IF NOT EXISTS idx_executions_status ON automation_executions(status);

-- Telegram channels (communication bridges between Telegram and missions)
CREATE TABLE IF NOT EXISTS telegram_channels (
    id TEXT PRIMARY KEY NOT NULL,
    mission_id TEXT NOT NULL,
    bot_token TEXT NOT NULL,
    bot_username TEXT,
    allowed_chat_ids TEXT NOT NULL DEFAULT '[]',
    trigger_mode TEXT NOT NULL DEFAULT 'direct_message',
    active INTEGER NOT NULL DEFAULT 1,
    webhook_secret TEXT,
    instructions TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    auto_create_missions INTEGER NOT NULL DEFAULT 0,
    default_backend TEXT,
    default_model_override TEXT,
    default_model_effort TEXT,
    default_workspace_id TEXT,
    default_config_profile TEXT,
    default_agent TEXT,
    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_telegram_channels_mission ON telegram_channels(mission_id);
CREATE INDEX IF NOT EXISTS idx_telegram_channels_active ON telegram_channels(active);
CREATE UNIQUE INDEX IF NOT EXISTS idx_telegram_channels_bot_token ON telegram_channels(bot_token);

-- Token usage from the OpenAI-compatible /v1 router (external clients:
-- assistant gateways, IDEs with proxy keys, ...). Mission usage lives in
-- mission_events metadata; both sources are merged by the usage summary.
CREATE TABLE IF NOT EXISTS proxy_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cost_cents INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_proxy_usage_timestamp ON proxy_usage(timestamp);
CREATE INDEX IF NOT EXISTS idx_proxy_usage_model ON proxy_usage(model);

-- Task board: server-scheduled worker tasks owned by a boss mission.
-- The control loop spawns workers for ready tasks and settles them when the
-- worker turn ends; the boss only plans and judges. See api::control::board.
CREATE TABLE IF NOT EXISTS board_tasks (
    id TEXT PRIMARY KEY,
    boss_mission_id TEXT NOT NULL,
    task_key TEXT NOT NULL,
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    backend TEXT NOT NULL,
    model_override TEXT,
    model_effort TEXT,
    working_directory TEXT,
    depends_on TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'pending',
    outcome TEXT,
    worker_mission_id TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    result_digest TEXT,
    notes TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(boss_mission_id, task_key),
    FOREIGN KEY (boss_mission_id) REFERENCES missions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_board_tasks_boss ON board_tasks(boss_mission_id);
CREATE INDEX IF NOT EXISTS idx_board_tasks_worker ON board_tasks(worker_mission_id);
CREATE INDEX IF NOT EXISTS idx_board_tasks_status ON board_tasks(status);

-- Persisted control-session message queue (JSON snapshot) so pending queued
-- messages survive a restart. Single row (the DB is already per-user).
CREATE TABLE IF NOT EXISTS control_queue (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    payload TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#;

/// Content size threshold for inline storage (64KB).
const CONTENT_SIZE_THRESHOLD: usize = 64 * 1024;

pub struct SqliteMissionStore {
    conn: Arc<Mutex<Connection>>,
    content_dir: PathBuf,
}

impl SqliteMissionStore {
    /// Parse an automation row from the database.
    fn parse_automation_row(row: &rusqlite::Row<'_>) -> Result<Automation, rusqlite::Error> {
        let id: String = row.get(0)?;
        let mission_id: String = row.get(1)?;
        let command_source_type: String = row.get(2)?;
        let command_source_data: String = row.get(3)?;
        let trigger_type: String = row.get(4)?;
        let trigger_data: String = row.get(5)?;
        let variables_json: String = row.get(6)?;
        let active: i64 = row.get(7)?;
        let stop_policy_str: String = row.get(8)?;
        let fresh_session_str: String = row.get(9).unwrap_or_else(|_| "keep".to_string());
        let created_at: String = row.get(10)?;
        let last_triggered_at: Option<String> = row.get(11)?;
        let retry_max_retries: i64 = row.get(12)?;
        let retry_delay_seconds: i64 = row.get(13)?;
        let retry_backoff_multiplier: f64 = row.get(14)?;
        // `driver` is appended to the SELECT list by all callers below. If a
        // legacy SELECT doesn't include it, default to `scheduler`.
        let driver_str: String = row
            .get::<_, String>(15)
            .unwrap_or_else(|_| "scheduler".to_string());

        // Parse command source
        let command_source: CommandSource = match command_source_type.as_str() {
            "library" => {
                let data: serde_json::Value = serde_json::from_str(&command_source_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                CommandSource::Library {
                    name: data["name"].as_str().unwrap_or("").to_string(),
                }
            }
            "local_file" => {
                let data: serde_json::Value = serde_json::from_str(&command_source_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                CommandSource::LocalFile {
                    path: data["path"].as_str().unwrap_or("").to_string(),
                }
            }
            "inline" => {
                let data: serde_json::Value = serde_json::from_str(&command_source_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                CommandSource::Inline {
                    content: data["content"].as_str().unwrap_or("").to_string(),
                }
            }
            "native_loop" => {
                let data: serde_json::Value = serde_json::from_str(&command_source_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                CommandSource::NativeLoop {
                    harness: data["harness"].as_str().unwrap_or("").to_string(),
                    command: data["command"].as_str().unwrap_or("").to_string(),
                    args: data.get("args").cloned().unwrap_or(serde_json::Value::Null),
                }
            }
            _ => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("Unknown command source type: {}", command_source_type).into(),
                ));
            }
        };

        // Parse trigger
        let trigger: TriggerType = match trigger_type.as_str() {
            "interval" => {
                let data: serde_json::Value = serde_json::from_str(&trigger_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                TriggerType::Interval {
                    seconds: data["seconds"].as_u64().unwrap_or(60),
                }
            }
            "cron" => {
                let data: serde_json::Value = serde_json::from_str(&trigger_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                TriggerType::Cron {
                    expression: data["expression"]
                        .as_str()
                        .unwrap_or("0 * * * *")
                        .to_string(),
                    timezone: data["timezone"].as_str().unwrap_or("UTC").to_string(),
                }
            }
            "webhook" => {
                let config: WebhookConfig = serde_json::from_str(&trigger_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                TriggerType::Webhook { config }
            }
            "agent_finished" => TriggerType::AgentFinished,
            "telegram" => {
                let config: super::TelegramTriggerConfig = serde_json::from_str(&trigger_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                TriggerType::Telegram { config }
            }
            _ => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("Unknown trigger type: {}", trigger_type).into(),
                ));
            }
        };

        // Parse variables
        let variables: HashMap<String, String> =
            serde_json::from_str(&variables_json).unwrap_or_default();
        // Parse stop_policy - handle both old format and new format
        let stop_policy = if stop_policy_str.starts_with("consecutive_failures:") {
            let count = stop_policy_str
                .split(':')
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(2);
            StopPolicy::WhenFailingConsecutively { count }
        } else if stop_policy_str.starts_with("all_issues_closed_and_prs_merged:") {
            let repo = stop_policy_str.split(':').nth(1).unwrap_or("").to_string();
            StopPolicy::WhenAllIssuesClosedAndPRsMerged { repo }
        } else {
            match stop_policy_str.as_str() {
                "never" => StopPolicy::Never,
                "after_first_fire" => StopPolicy::AfterFirstFire,
                _ => StopPolicy::Never,
            }
        };

        // Parse fresh_session
        let fresh_session = match fresh_session_str.as_str() {
            "always" => FreshSession::Always,
            "switch" => FreshSession::Switch,
            _ => FreshSession::Keep,
        };

        let driver = match driver_str.as_str() {
            "harness_loop" => super::AutomationDriver::HarnessLoop,
            _ => super::AutomationDriver::Scheduler,
        };

        Ok(Automation {
            id: Uuid::parse_str(&id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            mission_id: Uuid::parse_str(&mission_id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            command_source,
            trigger,
            variables,
            active: active != 0,
            stop_policy,
            fresh_session,
            created_at,
            last_triggered_at,
            retry_config: RetryConfig {
                max_retries: retry_max_retries as u32,
                retry_delay_seconds: retry_delay_seconds as u64,
                backoff_multiplier: retry_backoff_multiplier,
            },
            consecutive_failures: 0,
            driver,
        })
    }

    /// Parse an automation execution row from the database.
    fn parse_execution_row(
        row: &rusqlite::Row<'_>,
    ) -> Result<AutomationExecution, rusqlite::Error> {
        let id: String = row.get(0)?;
        let automation_id: String = row.get(1)?;
        let mission_id: String = row.get(2)?;
        let triggered_at: String = row.get(3)?;
        let trigger_source: String = row.get(4)?;
        let status_str: String = row.get(5)?;
        let webhook_payload: Option<String> = row.get(6)?;
        let variables_used_json: String = row.get(7)?;
        let completed_at: Option<String> = row.get(8)?;
        let error: Option<String> = row.get(9)?;
        let retry_count: i64 = row.get(10)?;

        // Parse status
        let status = match status_str.as_str() {
            "pending" => ExecutionStatus::Pending,
            "running" => ExecutionStatus::Running,
            "success" => ExecutionStatus::Success,
            "failed" => ExecutionStatus::Failed,
            "cancelled" => ExecutionStatus::Cancelled,
            "skipped" => ExecutionStatus::Skipped,
            _ => ExecutionStatus::Failed,
        };

        // Parse webhook payload
        let webhook_payload_value = webhook_payload.and_then(|s| serde_json::from_str(&s).ok());

        // Parse variables
        let variables_used: HashMap<String, String> =
            serde_json::from_str(&variables_used_json).unwrap_or_default();

        Ok(AutomationExecution {
            id: Uuid::parse_str(&id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            automation_id: Uuid::parse_str(&automation_id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            mission_id: Uuid::parse_str(&mission_id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            triggered_at,
            trigger_source,
            status,
            webhook_payload: webhook_payload_value,
            variables_used,
            completed_at,
            error,
            retry_count: retry_count as u32,
        })
    }

    pub async fn new(base_dir: PathBuf, user_id: &str) -> Result<Self, String> {
        let sanitized = sanitize_filename(user_id);
        let db_path = base_dir.join(format!("missions-{}.db", sanitized));
        let content_dir = base_dir.join("mission_data").join(&sanitized);

        // Create directories
        tokio::fs::create_dir_all(&base_dir)
            .await
            .map_err(|e| format!("Failed to create mission store dir: {}", e))?;
        tokio::fs::create_dir_all(&content_dir)
            .await
            .map_err(|e| format!("Failed to create content dir: {}", e))?;

        // Open database in blocking task
        let conn = tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path)
                .map_err(|e| format!("Failed to open SQLite database: {}", e))?;

            // Run schema
            conn.execute_batch(SCHEMA)
                .map_err(|e| format!("Failed to run schema: {}", e))?;

            // Run migrations for existing databases
            Self::run_migrations(&conn)?;

            Ok::<_, String>(conn)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            content_dir,
        })
    }

    /// Test-only: force-set a mission's `updated_at` and (if any) the timestamp
    /// of every persisted event for that mission. Lets tests exercise the
    /// stale-cleanup path with deterministic clock values without sleeping or
    /// stubbing `now`.
    #[cfg(test)]
    pub(super) async fn force_backdate_for_test(
        &self,
        mission_id: Uuid,
        timestamp: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let ts = timestamp.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET updated_at = ?1 WHERE id = ?2",
                rusqlite::params![ts, mid],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE mission_events SET timestamp = ?1 WHERE mission_id = ?2",
                rusqlite::params![ts, mid],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(())
    }

    /// Store content, either inline or in a file if too large.
    fn store_content(
        content_dir: &std::path::Path,
        mission_id: Uuid,
        sequence: i64,
        event_type: &str,
        content: &str,
    ) -> (Option<String>, Option<String>) {
        if content.len() <= CONTENT_SIZE_THRESHOLD {
            (Some(content.to_string()), None)
        } else {
            let events_dir = content_dir.join(mission_id.to_string()).join("events");
            if let Err(e) = std::fs::create_dir_all(&events_dir) {
                tracing::warn!("Failed to create events dir: {}", e);
                // Fall back to inline storage
                return (Some(content.to_string()), None);
            }

            let file_path = events_dir.join(format!("event_{}_{}.txt", sequence, event_type));
            if let Err(e) = std::fs::write(&file_path, content) {
                tracing::warn!("Failed to write content file: {}", e);
                return (Some(content.to_string()), None);
            }

            (None, Some(file_path.to_string_lossy().to_string()))
        }
    }

    /// Load content from inline or file.
    fn load_content(content: Option<&str>, content_file: Option<&str>) -> String {
        if let Some(c) = content {
            c.to_string()
        } else if let Some(path) = content_file {
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        }
    }

    fn ensure_telegram_memory_search_index(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS telegram_structured_memory_fts
             USING fts5(
                entry_id UNINDEXED,
                channel_id UNINDEXED,
                chat_id UNINDEXED,
                scope UNINDEXED,
                subject_user_id UNINDEXED,
                search_text,
                tokenize = 'unicode61 remove_diacritics 2'
             );",
        )
        .map_err(|e| {
            format!(
                "Failed to create telegram_structured_memory_fts table: {}",
                e
            )
        })
    }

    fn rebuild_telegram_memory_search_index(conn: &Connection) -> Result<(), String> {
        Self::ensure_telegram_memory_search_index(conn)?;

        conn.execute("DELETE FROM telegram_structured_memory_fts", [])
            .map_err(|e| format!("Failed to clear telegram memory search index: {}", e))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory",
            )
            .map_err(|e| format!("Failed to scan telegram structured memory: {}", e))?;

        let entries = stmt
            .query_map([], row_to_telegram_structured_memory)
            .map_err(|e| format!("Failed to query telegram structured memory: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect telegram structured memory rows: {}", e))?;

        for entry in entries {
            conn.execute(
                "INSERT INTO telegram_structured_memory_fts (
                    entry_id, channel_id, chat_id, scope, subject_user_id, search_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry.id.to_string(),
                    entry.channel_id.to_string(),
                    entry.chat_id,
                    match entry.scope {
                        TelegramStructuredMemoryScope::Chat => "chat",
                        TelegramStructuredMemoryScope::User => "user",
                        TelegramStructuredMemoryScope::Channel => "channel",
                    },
                    entry.subject_user_id,
                    build_telegram_memory_search_text(&entry),
                ],
            )
            .map_err(|e| format!("Failed to populate telegram memory search index: {}", e))?;
        }

        Ok(())
    }

    fn upsert_telegram_memory_search_index_entry(
        conn: &Connection,
        entry: &TelegramStructuredMemoryEntry,
    ) -> Result<(), String> {
        Self::ensure_telegram_memory_search_index(conn)?;

        conn.execute(
            "DELETE FROM telegram_structured_memory_fts WHERE entry_id = ?1",
            params![entry.id.to_string()],
        )
        .map_err(|e| {
            format!(
                "Failed to replace telegram memory search index entry: {}",
                e
            )
        })?;

        conn.execute(
            "INSERT INTO telegram_structured_memory_fts (
                entry_id, channel_id, chat_id, scope, subject_user_id, search_text
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id.to_string(),
                entry.channel_id.to_string(),
                entry.chat_id,
                match entry.scope {
                    TelegramStructuredMemoryScope::Chat => "chat",
                    TelegramStructuredMemoryScope::User => "user",
                    TelegramStructuredMemoryScope::Channel => "channel",
                },
                entry.subject_user_id,
                build_telegram_memory_search_text(entry),
            ],
        )
        .map_err(|e| format!("Failed to upsert telegram memory search index entry: {}", e))?;

        Ok(())
    }

    fn load_telegram_structured_memory_for_upsert(
        conn: &Connection,
        entry: &TelegramStructuredMemoryEntry,
        scope: &str,
        kind: &str,
        normalized_label: &str,
    ) -> Result<Option<TelegramStructuredMemoryEntry>, String> {
        let sql = match entry.scope {
            TelegramStructuredMemoryScope::User if entry.subject_user_id.is_some() => {
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE channel_id = ?1
                   AND scope = ?2
                   AND subject_user_id = ?3
                   AND kind = ?4
                   AND normalized_label = ?5
                 LIMIT 1"
            }
            TelegramStructuredMemoryScope::Channel => {
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE channel_id = ?1
                   AND scope = ?2
                   AND kind = ?3
                   AND normalized_label = ?4
                 LIMIT 1"
            }
            _ => {
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE channel_id = ?1
                   AND scope = ?2
                   AND chat_id = ?3
                   AND kind = ?4
                   AND normalized_label = ?5
                 LIMIT 1"
            }
        };

        let mut stmt = conn.prepare(sql).map_err(|e| {
            format!(
                "Failed to prepare telegram structured memory lookup after upsert: {}",
                e
            )
        })?;

        let entry = match entry.scope {
            TelegramStructuredMemoryScope::User if entry.subject_user_id.is_some() => stmt
                .query_row(
                    params![
                        entry.channel_id.to_string(),
                        scope,
                        entry.subject_user_id,
                        kind,
                        normalized_label
                    ],
                    row_to_telegram_structured_memory,
                )
                .optional(),
            TelegramStructuredMemoryScope::Channel => stmt
                .query_row(
                    params![entry.channel_id.to_string(), scope, kind, normalized_label],
                    row_to_telegram_structured_memory,
                )
                .optional(),
            _ => stmt
                .query_row(
                    params![
                        entry.channel_id.to_string(),
                        scope,
                        entry.chat_id,
                        kind,
                        normalized_label
                    ],
                    row_to_telegram_structured_memory,
                )
                .optional(),
        }
        .map_err(|e| {
            format!(
                "Failed to load telegram structured memory row after upsert: {}",
                e
            )
        })?;

        Ok(entry)
    }

    /// Run database migrations for existing databases.
    /// CREATE TABLE IF NOT EXISTS doesn't add columns to existing tables,
    /// so we need to handle schema changes manually.
    fn run_migrations(conn: &Connection) -> Result<(), String> {
        // Check if 'backend' column exists in missions table
        let has_backend_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'backend'")
            .map_err(|e| format!("Failed to check for backend column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_backend_column {
            tracing::info!("Running migration: adding 'backend' column to missions table");
            conn.execute(
                "ALTER TABLE missions ADD COLUMN backend TEXT NOT NULL DEFAULT 'opencode'",
                [],
            )
            .map_err(|e| format!("Failed to add backend column: {}", e))?;
        }

        // Check if 'session_id' column exists in missions table
        let has_session_id_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'session_id'")
            .map_err(|e| format!("Failed to check for session_id column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_session_id_column {
            tracing::info!("Running migration: adding 'session_id' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN session_id TEXT", [])
                .map_err(|e| format!("Failed to add session_id column: {}", e))?;
        }

        // Add performance indexes if they don't exist (idempotent)
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_missions_status_updated ON missions(status, updated_at);
             CREATE INDEX IF NOT EXISTS idx_events_mission_type_sequence ON mission_events(mission_id, event_type, sequence);
             CREATE INDEX IF NOT EXISTS idx_events_event_type ON mission_events(event_type);
             CREATE INDEX IF NOT EXISTS idx_events_mission_timestamp ON mission_events(mission_id, timestamp DESC);",
        )
        .map_err(|e| format!("Failed to create performance indexes: {}", e))?;

        // Check if 'terminal_reason' column exists in missions table
        let has_terminal_reason_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'terminal_reason'")
            .map_err(|e| format!("Failed to check for terminal_reason column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_terminal_reason_column {
            tracing::info!("Running migration: adding 'terminal_reason' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN terminal_reason TEXT", [])
                .map_err(|e| format!("Failed to add terminal_reason column: {}", e))?;
        }

        // Check if 'config_profile' column exists in missions table
        let has_config_profile_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'config_profile'")
            .map_err(|e| format!("Failed to check for config_profile column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_config_profile_column {
            tracing::info!("Running migration: adding 'config_profile' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN config_profile TEXT", [])
                .map_err(|e| format!("Failed to add config_profile column: {}", e))?;
        }

        // Check if 'model_effort' column exists in missions table
        let has_model_effort_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'model_effort'")
            .map_err(|e| format!("Failed to check for model_effort column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_model_effort_column {
            tracing::info!("Running migration: adding 'model_effort' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN model_effort TEXT", [])
                .map_err(|e| format!("Failed to add model_effort column: {}", e))?;
        }

        // Check if 'short_description' column exists in missions table
        let has_short_description_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'short_description'")
            .map_err(|e| format!("Failed to check for short_description column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_short_description_column {
            tracing::info!(
                "Running migration: adding 'short_description' column to missions table"
            );
            conn.execute("ALTER TABLE missions ADD COLUMN short_description TEXT", [])
                .map_err(|e| format!("Failed to add short_description column: {}", e))?;
        }

        // Check if 'metadata_updated_at' column exists in missions table
        let has_metadata_updated_at_column: bool = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('missions') WHERE name = 'metadata_updated_at'",
            )
            .map_err(|e| format!("Failed to check for metadata_updated_at column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_metadata_updated_at_column {
            tracing::info!(
                "Running migration: adding 'metadata_updated_at' column to missions table"
            );
            conn.execute(
                "ALTER TABLE missions ADD COLUMN metadata_updated_at TEXT",
                [],
            )
            .map_err(|e| format!("Failed to add metadata_updated_at column: {}", e))?;
        }

        let has_metadata_source_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'metadata_source'")
            .map_err(|e| format!("Failed to check for metadata_source column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_metadata_source_column {
            tracing::info!("Running migration: adding 'metadata_source' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN metadata_source TEXT", [])
                .map_err(|e| format!("Failed to add metadata_source column: {}", e))?;
        }

        let has_metadata_model_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'metadata_model'")
            .map_err(|e| format!("Failed to check for metadata_model column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_metadata_model_column {
            tracing::info!("Running migration: adding 'metadata_model' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN metadata_model TEXT", [])
                .map_err(|e| format!("Failed to add metadata_model column: {}", e))?;
        }

        let has_metadata_version_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'metadata_version'")
            .map_err(|e| format!("Failed to check for metadata_version column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_metadata_version_column {
            tracing::info!("Running migration: adding 'metadata_version' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN metadata_version TEXT", [])
                .map_err(|e| format!("Failed to add metadata_version column: {}", e))?;
        }

        // Check if 'parent_mission_id' column exists in missions table
        let has_parent_mission_id_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'parent_mission_id'")
            .map_err(|e| format!("Failed to check for parent_mission_id column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_parent_mission_id_column {
            tracing::info!(
                "Running migration: adding 'parent_mission_id' column to missions table"
            );
            conn.execute("ALTER TABLE missions ADD COLUMN parent_mission_id TEXT", [])
                .map_err(|e| format!("Failed to add parent_mission_id column: {}", e))?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_missions_parent ON missions(parent_mission_id)",
                [],
            )
            .map_err(|e| format!("Failed to create parent_mission_id index: {}", e))?;
        }

        // Check if 'working_directory' column exists in missions table
        let has_working_directory_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'working_directory'")
            .map_err(|e| format!("Failed to check for working_directory column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_working_directory_column {
            tracing::info!(
                "Running migration: adding 'working_directory' column to missions table"
            );
            conn.execute("ALTER TABLE missions ADD COLUMN working_directory TEXT", [])
                .map_err(|e| format!("Failed to add working_directory column: {}", e))?;
        }

        // Check if 'mission_mode' column exists in missions table
        let has_mission_mode_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'mission_mode'")
            .map_err(|e| format!("Failed to check for mission_mode column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if !has_mission_mode_column {
            tracing::info!("Running migration: adding 'mission_mode' column to missions table");
            conn.execute(
                "ALTER TABLE missions ADD COLUMN mission_mode TEXT NOT NULL DEFAULT 'task'",
                [],
            )
            .map_err(|e| format!("Failed to add mission_mode column: {}", e))?;
        }

        // Ensure telegram_channels table exists (for existing databases)
        let has_telegram_channels: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='telegram_channels'")
            .map_err(|e| format!("Failed to check for telegram_channels table: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query sqlite_master: {}", e))?;

        if !has_telegram_channels {
            tracing::info!("Running migration: creating 'telegram_channels' table");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS telegram_channels (
                    id TEXT PRIMARY KEY NOT NULL,
                    mission_id TEXT NOT NULL,
                    bot_token TEXT NOT NULL,
                    bot_username TEXT,
                    allowed_chat_ids TEXT NOT NULL DEFAULT '[]',
                    trigger_mode TEXT NOT NULL DEFAULT 'mention_or_dm',
                    active INTEGER NOT NULL DEFAULT 1,
                    webhook_secret TEXT,
                    instructions TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    auto_create_missions INTEGER NOT NULL DEFAULT 0,
                    default_backend TEXT,
                    default_model_override TEXT,
                    default_model_effort TEXT,
                    default_workspace_id TEXT,
                    default_config_profile TEXT,
                    default_agent TEXT,
                    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_telegram_channels_mission ON telegram_channels(mission_id);
                CREATE INDEX IF NOT EXISTS idx_telegram_channels_active ON telegram_channels(active);
                CREATE UNIQUE INDEX IF NOT EXISTS idx_telegram_channels_bot_token ON telegram_channels(bot_token);",
            )
            .map_err(|e| format!("Failed to create telegram_channels table: {}", e))?;
        } else {
            // Add webhook_secret column if missing (migration for existing tables)
            let has_webhook_secret: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('telegram_channels') WHERE name='webhook_secret'")
                .map_err(|e| format!("Failed to check for webhook_secret column: {}", e))?
                .exists([])
                .map_err(|e| format!("Failed to query pragma_table_info: {}", e))?;
            if !has_webhook_secret {
                tracing::info!(
                    "Running migration: adding 'webhook_secret' column to telegram_channels"
                );
                conn.execute(
                    "ALTER TABLE telegram_channels ADD COLUMN webhook_secret TEXT",
                    [],
                )
                .map_err(|e| format!("Failed to add webhook_secret column: {}", e))?;
            }
            // Add instructions column if missing (ignore "duplicate column" from concurrent init)
            let has_instructions: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('telegram_channels') WHERE name='instructions'")
                .map_err(|e| format!("Failed to check for instructions column: {}", e))?
                .exists([])
                .map_err(|e| format!("Failed to query pragma_table_info: {}", e))?;
            if !has_instructions {
                tracing::info!(
                    "Running migration: adding 'instructions' column to telegram_channels"
                );
                match conn.execute(
                    "ALTER TABLE telegram_channels ADD COLUMN instructions TEXT",
                    [],
                ) {
                    Ok(_) => {}
                    Err(e) if e.to_string().contains("duplicate column") => {
                        tracing::debug!(
                            "instructions column already exists (concurrent migration)"
                        );
                    }
                    Err(e) => return Err(format!("Failed to add instructions column: {}", e)),
                }
            }

            // Add auto-create mission columns
            let auto_create_cols = [
                ("auto_create_missions", "INTEGER NOT NULL DEFAULT 0"),
                ("default_backend", "TEXT"),
                ("default_model_override", "TEXT"),
                ("default_model_effort", "TEXT"),
                ("default_workspace_id", "TEXT"),
                ("default_config_profile", "TEXT"),
                ("default_agent", "TEXT"),
            ];
            for (col_name, col_type) in &auto_create_cols {
                let has_col: bool = conn
                    .prepare(&format!(
                        "SELECT 1 FROM pragma_table_info('telegram_channels') WHERE name='{}'",
                        col_name
                    ))
                    .map_err(|e| format!("Failed to check for {} column: {}", col_name, e))?
                    .exists([])
                    .map_err(|e| format!("Failed to query pragma_table_info: {}", e))?;
                if !has_col {
                    tracing::info!(
                        "Running migration: adding '{}' column to telegram_channels",
                        col_name
                    );
                    match conn.execute(
                        &format!(
                            "ALTER TABLE telegram_channels ADD COLUMN {} {}",
                            col_name, col_type
                        ),
                        [],
                    ) {
                        Ok(_) => {}
                        Err(e) if e.to_string().contains("duplicate column") => {}
                        Err(e) => return Err(format!("Failed to add {} column: {}", col_name, e)),
                    }
                }
            }
        }

        if let Err(e) = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_telegram_channels_bot_token ON telegram_channels(bot_token)",
            [],
        ) {
            tracing::warn!(
                "Failed to enforce unique Telegram bot tokens at the database layer: {}",
                e
            );
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_users (
                id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL UNIQUE,
                username TEXT,
                display_name TEXT,
                role TEXT NOT NULL DEFAULT 'observer',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tusers_role
                ON telegram_users(role);

            CREATE TABLE IF NOT EXISTS telegram_user_cursors (
                id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL UNIQUE,
                last_status_at TEXT,
                last_dashboard_seen_at TEXT,
                last_alert_ack_at TEXT,
                last_digest_at TEXT,
                last_seen_event_sequence_by_mission_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS telegram_mission_subscriptions (
                id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL,
                mission_id TEXT NOT NULL,
                interest_level TEXT NOT NULL DEFAULT 'normal',
                reason TEXT,
                expires_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(telegram_user_id, mission_id),
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_tsub_user_interest
                ON telegram_mission_subscriptions(telegram_user_id, interest_level);
            CREATE INDEX IF NOT EXISTS idx_tsub_mission
                ON telegram_mission_subscriptions(mission_id);

            CREATE TABLE IF NOT EXISTS telegram_alert_preferences (
                id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL,
                scope TEXT NOT NULL,
                scope_value TEXT,
                rule_text TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_from_message_id INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_talert_pref_user_scope
                ON telegram_alert_preferences(telegram_user_id, scope, scope_value);

            CREATE TABLE IF NOT EXISTS telegram_alerts (
                id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL,
                mission_id TEXT,
                event_kind TEXT NOT NULL,
                importance TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                telegram_message_id INTEGER,
                last_error TEXT,
                created_at TEXT NOT NULL,
                sent_at TEXT,
                acknowledged_at TEXT,
                UNIQUE(telegram_user_id, mission_id, event_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_talerts_user_status
                ON telegram_alerts(telegram_user_id, status, created_at);

            CREATE TABLE IF NOT EXISTS paloma_decisions (
                id TEXT PRIMARY KEY NOT NULL,
                event_source TEXT NOT NULL,
                mission_id TEXT,
                user_id INTEGER,
                channel TEXT NOT NULL,
                reason_code TEXT NOT NULL,
                proposed_action TEXT NOT NULL,
                allowed INTEGER NOT NULL,
                suppression_reason TEXT,
                policy_snapshot_json TEXT NOT NULL,
                generated_text_hash TEXT,
                generated_text_preview TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_paloma_decisions_created
                ON paloma_decisions(created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_paloma_decisions_mission
                ON paloma_decisions(mission_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS paloma_scheduler_jobs (
                name TEXT PRIMARY KEY NOT NULL,
                lease_owner TEXT,
                lease_expires_at TEXT,
                last_started_at TEXT,
                last_finished_at TEXT,
                last_error TEXT,
                run_count INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS paloma_mission_cards (
                mission_id TEXT PRIMARY KEY NOT NULL,
                telegram_user_id INTEGER NOT NULL,
                channel_id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                anchor_ts TEXT NOT NULL,
                last_edit_ts TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                archived INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_paloma_mc_user
                ON paloma_mission_cards(telegram_user_id, archived);

            CREATE TABLE IF NOT EXISTS paloma_cooldown_state (
                mission_id TEXT NOT NULL,
                alert_class TEXT NOT NULL,
                telegram_user_id INTEGER NOT NULL,
                last_sent_at TEXT NOT NULL,
                next_eligible_at TEXT NOT NULL,
                backoff_step INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (mission_id, alert_class, telegram_user_id),
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_paloma_cd_user_eligibility
                ON paloma_cooldown_state(telegram_user_id, next_eligible_at);

            CREATE TABLE IF NOT EXISTS paloma_user_preferences (
                telegram_user_id INTEGER PRIMARY KEY NOT NULL,
                timezone TEXT NOT NULL DEFAULT 'UTC',
                quiet_hours_start INTEGER,
                quiet_hours_end INTEGER,
                max_interrupts_per_hour INTEGER NOT NULL DEFAULT 1,
                max_interrupts_per_day INTEGER NOT NULL DEFAULT 4,
                failure_override_quiet INTEGER NOT NULL DEFAULT 1,
                alert_class_overrides_json TEXT NOT NULL DEFAULT '{}',
                mission_overrides_json TEXT NOT NULL DEFAULT '{}',
                digest_cadence TEXT NOT NULL DEFAULT 'daily',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .map_err(|e| format!("Failed to create Paloma Telegram state tables: {}", e))?;

        // Create telegram_chat_missions table if it doesn't exist
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_chat_missions (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                mission_id TEXT NOT NULL,
                chat_title TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tcm_channel_chat ON telegram_chat_missions(channel_id, chat_id);
            CREATE INDEX IF NOT EXISTS idx_tcm_mission ON telegram_chat_missions(mission_id);",
        )
        .map_err(|e| format!("Failed to create telegram_chat_missions table: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_scheduled_messages (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                source_mission_id TEXT,
                chat_id INTEGER NOT NULL,
                chat_title TEXT,
                text TEXT NOT NULL,
                send_at TEXT NOT NULL,
                sent_at TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                last_error TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (source_mission_id) REFERENCES missions(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tsm_due
                ON telegram_scheduled_messages(status, send_at);
            CREATE INDEX IF NOT EXISTS idx_tsm_channel
                ON telegram_scheduled_messages(channel_id, status, send_at);",
        )
        .map_err(|e| format!("Failed to create telegram_scheduled_messages table: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_structured_memory (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                mission_id TEXT,
                scope TEXT NOT NULL DEFAULT 'chat',
                kind TEXT NOT NULL,
                label TEXT,
                normalized_label TEXT,
                value TEXT NOT NULL,
                subject_user_id INTEGER,
                subject_username TEXT,
                subject_display_name TEXT,
                source_message_id INTEGER,
                source_role TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tmem_channel_search
                ON telegram_structured_memory(channel_id, normalized_label, updated_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_structured_memory table: {}", e))?;

        let telegram_memory_cols = [
            ("scope", "TEXT NOT NULL DEFAULT 'chat'"),
            ("subject_user_id", "INTEGER"),
            ("subject_username", "TEXT"),
            ("subject_display_name", "TEXT"),
        ];
        for (col_name, col_type) in &telegram_memory_cols {
            let has_col: bool = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('telegram_structured_memory') WHERE name='{}'",
                    col_name
                ))
                .map_err(|e| format!("Failed to check for {} column: {}", col_name, e))?
                .exists([])
                .map_err(|e| format!("Failed to query pragma_table_info: {}", e))?;
            if !has_col {
                tracing::info!(
                    "Running migration: adding '{}' column to telegram_structured_memory",
                    col_name
                );
                match conn.execute(
                    &format!(
                        "ALTER TABLE telegram_structured_memory ADD COLUMN {} {}",
                        col_name, col_type
                    ),
                    [],
                ) {
                    Ok(_) => {}
                    Err(e) if e.to_string().contains("duplicate column") => {}
                    Err(e) => return Err(format!("Failed to add {} column: {}", col_name, e)),
                }
            }
        }

        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tmem_channel_chat_updated
                ON telegram_structured_memory(channel_id, scope, chat_id, updated_at DESC);
             CREATE INDEX IF NOT EXISTS idx_tmem_channel_user_updated
                ON telegram_structured_memory(channel_id, scope, subject_user_id, updated_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_structured_memory indexes: {}", e))?;

        Self::ensure_telegram_memory_search_index(conn)?;
        let memory_row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM telegram_structured_memory",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count telegram structured memory rows: {}", e))?;
        if memory_row_count > 0 {
            let search_row_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM telegram_structured_memory_fts",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to count telegram memory search rows: {}", e))?;
            if search_row_count == 0 {
                Self::rebuild_telegram_memory_search_index(conn)?;
            }
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_action_executions (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                source_mission_id TEXT,
                source_chat_id INTEGER,
                target_chat_id INTEGER NOT NULL,
                target_chat_title TEXT,
                action_kind TEXT NOT NULL,
                target_kind TEXT NOT NULL,
                target_value TEXT NOT NULL,
                text TEXT NOT NULL,
                delay_seconds INTEGER NOT NULL DEFAULT 0,
                scheduled_message_id TEXT,
                status TEXT NOT NULL,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (source_mission_id) REFERENCES missions(id) ON DELETE SET NULL,
                FOREIGN KEY (scheduled_message_id) REFERENCES telegram_scheduled_messages(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tae_channel_updated
                ON telegram_action_executions(channel_id, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_tae_scheduled
                ON telegram_action_executions(scheduled_message_id);",
        )
        .map_err(|e| format!("Failed to create telegram_action_executions table: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_conversations (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                mission_id TEXT,
                chat_title TEXT,
                chat_type TEXT,
                last_message_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE SET NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_tconv_channel_chat
                ON telegram_conversations(channel_id, chat_id);
            CREATE INDEX IF NOT EXISTS idx_tconv_channel_updated
                ON telegram_conversations(channel_id, updated_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_conversations table: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_conversation_messages (
                id TEXT PRIMARY KEY NOT NULL,
                conversation_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                mission_id TEXT,
                workflow_id TEXT,
                telegram_message_id INTEGER,
                direction TEXT NOT NULL,
                role TEXT NOT NULL,
                sender_user_id INTEGER,
                sender_username TEXT,
                sender_display_name TEXT,
                reply_to_message_id INTEGER,
                text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (conversation_id) REFERENCES telegram_conversations(id) ON DELETE CASCADE,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tconv_msg_conversation_created
                ON telegram_conversation_messages(conversation_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_tconv_msg_chat_created
                ON telegram_conversation_messages(channel_id, chat_id, created_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_conversation_messages table: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_workflows (
                id TEXT PRIMARY KEY NOT NULL,
                channel_id TEXT NOT NULL,
                origin_conversation_id TEXT NOT NULL,
                origin_chat_id INTEGER NOT NULL,
                origin_mission_id TEXT,
                target_conversation_id TEXT,
                target_chat_id INTEGER,
                target_chat_title TEXT,
                target_chat_type TEXT,
                target_request_message_id INTEGER,
                initiated_by_user_id INTEGER,
                initiated_by_username TEXT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                request_text TEXT NOT NULL,
                latest_reply_text TEXT,
                summary TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                completed_at TEXT,
                FOREIGN KEY (channel_id) REFERENCES telegram_channels(id) ON DELETE CASCADE,
                FOREIGN KEY (origin_conversation_id) REFERENCES telegram_conversations(id) ON DELETE CASCADE,
                FOREIGN KEY (target_conversation_id) REFERENCES telegram_conversations(id) ON DELETE SET NULL,
                FOREIGN KEY (origin_mission_id) REFERENCES missions(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_twf_channel_updated
                ON telegram_workflows(channel_id, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_twf_target_status
                ON telegram_workflows(channel_id, target_chat_id, status, updated_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_workflows table: {}", e))?;
        for (col_name, col_type) in [
            ("target_chat_type", "TEXT"),
            ("target_request_message_id", "INTEGER"),
        ] {
            let has_col: bool = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('telegram_workflows') WHERE name='{}'",
                    col_name
                ))
                .map_err(|e| format!("Failed to check for {} column: {}", col_name, e))?
                .exists([])
                .map_err(|e| format!("Failed to query pragma_table_info: {}", e))?;
            if !has_col {
                tracing::info!(
                    "Running migration: adding '{}' column to telegram_workflows",
                    col_name
                );
                conn.execute(
                    &format!(
                        "ALTER TABLE telegram_workflows ADD COLUMN {} {}",
                        col_name, col_type
                    ),
                    [],
                )
                .map_err(|e| format!("Failed to add {} column: {}", col_name, e))?;
            }
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_workflow_events (
                id TEXT PRIMARY KEY NOT NULL,
                workflow_id TEXT NOT NULL,
                conversation_id TEXT,
                event_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (workflow_id) REFERENCES telegram_workflows(id) ON DELETE CASCADE,
                FOREIGN KEY (conversation_id) REFERENCES telegram_conversations(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_twf_events_workflow_created
                ON telegram_workflow_events(workflow_id, created_at DESC);",
        )
        .map_err(|e| format!("Failed to create telegram_workflow_events table: {}", e))?;

        // Telegram webhook dedup table (persists across restarts)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_webhook_dedup (
                channel_id TEXT NOT NULL,
                update_id INTEGER NOT NULL,
                seen_at TEXT NOT NULL,
                PRIMARY KEY (channel_id, update_id)
            );
            CREATE INDEX IF NOT EXISTS idx_tg_dedup_seen
                ON telegram_webhook_dedup(seen_at);",
        )
        .map_err(|e| format!("Failed to create telegram_webhook_dedup table: {}", e))?;

        // Migrate automations table to new schema
        Self::migrate_automations_table(conn)?;
        Self::ensure_automation_indexes(conn)?;

        // Goal-mode columns (PR #403): set to true when a mission was started
        // via codex `/goal <objective>`. The codex backend infers from the
        // user's message at send time; persisting on the row lets the UI
        // render the goal pill from a fresh page load without SSE replay.
        let has_goal_mode_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'goal_mode'")
            .map_err(|e| format!("Failed to check for goal_mode column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_goal_mode_column {
            tracing::info!("Running migration: adding 'goal_mode' column to missions table");
            conn.execute(
                "ALTER TABLE missions ADD COLUMN goal_mode INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| format!("Failed to add goal_mode column: {}", e))?;
        }
        let has_goal_objective_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'goal_objective'")
            .map_err(|e| format!("Failed to check for goal_objective column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_goal_objective_column {
            tracing::info!("Running migration: adding 'goal_objective' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN goal_objective TEXT", [])
                .map_err(|e| format!("Failed to add goal_objective column: {}", e))?;
        }

        // first_viewed_at: timestamp of the user's first open of the mission
        // since it last entered AwaitingUser. Drives the ack grace timer and
        // the "opened" dot on Finished missions.
        let has_first_viewed_at_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'first_viewed_at'")
            .map_err(|e| format!("Failed to check for first_viewed_at column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_first_viewed_at_column {
            tracing::info!("Running migration: adding 'first_viewed_at' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN first_viewed_at TEXT", [])
                .map_err(|e| format!("Failed to add first_viewed_at column: {}", e))?;
        }

        // FLEET-001 scheduling columns: priority (dispatch ordering), not_before
        // (earliest dispatch time), deadline (soft deadline for observability).
        let has_priority_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'priority'")
            .map_err(|e| format!("Failed to check for priority column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_priority_column {
            tracing::info!("Running migration: adding 'priority' column to missions table");
            conn.execute(
                "ALTER TABLE missions ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| format!("Failed to add priority column: {}", e))?;
        }

        let has_not_before_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'not_before'")
            .map_err(|e| format!("Failed to check for not_before column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_not_before_column {
            tracing::info!("Running migration: adding 'not_before' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN not_before TEXT", [])
                .map_err(|e| format!("Failed to add not_before column: {}", e))?;
        }

        let has_deadline_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'deadline'")
            .map_err(|e| format!("Failed to check for deadline column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_deadline_column {
            tracing::info!("Running migration: adding 'deadline' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN deadline TEXT", [])
                .map_err(|e| format!("Failed to add deadline column: {}", e))?;
        }

        // FLEET-001 scheduling dispatch: the goal a not_before-deferred mission
        // will run once the scheduler dispatches it. Held here (rather than in
        // mission history) so a Pending mission carries dispatchable work
        // without prematurely appearing as a committed turn.
        let has_deferred_goal_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'deferred_goal'")
            .map_err(|e| format!("Failed to check for deferred_goal column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_deferred_goal_column {
            tracing::info!("Running migration: adding 'deferred_goal' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN deferred_goal TEXT", [])
                .map_err(|e| format!("Failed to add deferred_goal column: {}", e))?;
        }

        // FLEET-004: when a mission was last paused (status Paused). Drives pause
        // age in the UI and future zombie-pause cleanup.
        let has_paused_at_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('missions') WHERE name = 'paused_at'")
            .map_err(|e| format!("Failed to check for paused_at column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_paused_at_column {
            tracing::info!("Running migration: adding 'paused_at' column to missions table");
            conn.execute("ALTER TABLE missions ADD COLUMN paused_at TEXT", [])
                .map_err(|e| format!("Failed to add paused_at column: {}", e))?;
        }

        Ok(())
    }

    /// Migrate the automations table from old schema to new schema.
    fn migrate_automations_table(conn: &Connection) -> Result<(), String> {
        // Check if the automations table has the old schema
        let has_command_name: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('automations') WHERE name = 'command_name'")
            .map_err(|e| format!("Failed to check automations schema: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if has_command_name {
            tracing::info!("Running migration: updating automations table to new schema");

            // Read existing automations
            let mut stmt = conn
                .prepare("SELECT id, mission_id, command_name, interval_seconds, active, created_at, last_triggered_at FROM automations")
                .map_err(|e| format!("Failed to read old automations: {}", e))?;

            let old_automations: Vec<LegacyAutomationRow> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .map_err(|e| format!("Failed to query old automations: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to collect old automations: {}", e))?;

            // Drop the old table
            conn.execute("DROP TABLE IF EXISTS automations", [])
                .map_err(|e| format!("Failed to drop old automations table: {}", e))?;

            // Create the new table
            conn.execute_batch(
                "CREATE TABLE automations (
                    id TEXT PRIMARY KEY NOT NULL,
                    mission_id TEXT NOT NULL,
                    command_source_type TEXT NOT NULL,
                    command_source_data TEXT NOT NULL,
                    trigger_type TEXT NOT NULL,
                    trigger_data TEXT NOT NULL,
                    variables TEXT NOT NULL DEFAULT '{}',
                    active INTEGER NOT NULL DEFAULT 1,
                    stop_policy TEXT NOT NULL DEFAULT 'consecutive_failures:2',
                    created_at TEXT NOT NULL,
                    last_triggered_at TEXT,
                    retry_max_retries INTEGER NOT NULL DEFAULT 3,
                    retry_delay_seconds INTEGER NOT NULL DEFAULT 60,
                    retry_backoff_multiplier REAL NOT NULL DEFAULT 2.0,
                    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_automations_mission ON automations(mission_id);
                CREATE INDEX IF NOT EXISTS idx_automations_active ON automations(mission_id, active);
                CREATE INDEX IF NOT EXISTS idx_automations_webhook_id ON automations(json_extract(trigger_data, '$.webhook_id')) WHERE trigger_type = 'webhook';

                CREATE TABLE IF NOT EXISTS automation_executions (
                    id TEXT PRIMARY KEY NOT NULL,
                    automation_id TEXT NOT NULL,
                    mission_id TEXT NOT NULL,
                    triggered_at TEXT NOT NULL,
                    trigger_source TEXT NOT NULL,
                    status TEXT NOT NULL,
                    webhook_payload TEXT,
                    variables_used TEXT NOT NULL DEFAULT '{}',
                    completed_at TEXT,
                    error TEXT,
                    retry_count INTEGER NOT NULL DEFAULT 0,
                    FOREIGN KEY (automation_id) REFERENCES automations(id) ON DELETE CASCADE,
                    FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_executions_automation ON automation_executions(automation_id, triggered_at DESC);
                CREATE INDEX IF NOT EXISTS idx_executions_mission ON automation_executions(mission_id, triggered_at DESC);
                CREATE INDEX IF NOT EXISTS idx_executions_status ON automation_executions(status);"
            )
            .map_err(|e| format!("Failed to create new automations table: {}", e))?;

            // Migrate old data to new schema
            let automation_count = old_automations.len();
            for (
                id,
                mission_id,
                command_name,
                interval_seconds,
                active,
                created_at,
                last_triggered_at,
            ) in old_automations
            {
                // Convert old format to new format
                let command_source_data = serde_json::json!({
                    "name": command_name
                })
                .to_string();

                let trigger_data = serde_json::json!({
                    "seconds": interval_seconds
                })
                .to_string();

                conn.execute(
                    "INSERT INTO automations (id, mission_id, command_source_type, command_source_data,
                                             trigger_type, trigger_data, variables, active, stop_policy,
                                             fresh_session, created_at, last_triggered_at, retry_max_retries,
                                             retry_delay_seconds, retry_backoff_multiplier)
                     VALUES (?, ?, 'library', ?, 'interval', ?, '{}', ?, 'consecutive_failures:2', 'keep', ?, ?, 3, 60, 2.0)",
                    params![id, mission_id, command_source_data, trigger_data, active, created_at, last_triggered_at],
                )
                .map_err(|e| format!("Failed to migrate automation: {}", e))?;
            }

            tracing::info!(
                "Successfully migrated {} automations to new schema",
                automation_count
            );
        } else {
            // Check if automation_executions table exists
            let has_executions_table: bool = conn
                .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='automation_executions'")
                .map_err(|e| format!("Failed to check for automation_executions table: {}", e))?
                .exists([])
                .map_err(|e| format!("Failed to query sqlite_master: {}", e))?;

            if !has_executions_table {
                tracing::info!("Creating automation_executions table");
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS automation_executions (
                        id TEXT PRIMARY KEY NOT NULL,
                        automation_id TEXT NOT NULL,
                        mission_id TEXT NOT NULL,
                        triggered_at TEXT NOT NULL,
                        trigger_source TEXT NOT NULL,
                        status TEXT NOT NULL,
                        webhook_payload TEXT,
                        variables_used TEXT NOT NULL DEFAULT '{}',
                        completed_at TEXT,
                        error TEXT,
                        retry_count INTEGER NOT NULL DEFAULT 0,
                        FOREIGN KEY (automation_id) REFERENCES automations(id) ON DELETE CASCADE,
                        FOREIGN KEY (mission_id) REFERENCES missions(id) ON DELETE CASCADE
                    );

                    CREATE INDEX IF NOT EXISTS idx_executions_automation ON automation_executions(automation_id, triggered_at DESC);
                    CREATE INDEX IF NOT EXISTS idx_executions_mission ON automation_executions(mission_id, triggered_at DESC);
                    CREATE INDEX IF NOT EXISTS idx_executions_status ON automation_executions(status);"
                )
                .map_err(|e| format!("Failed to create automation_executions table: {}", e))?;
            }
        }

        Ok(())
    }

    fn ensure_automation_indexes(conn: &Connection) -> Result<(), String> {
        let has_trigger_data: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('automations') WHERE name = 'trigger_data'")
            .map_err(|e| format!("Failed to check automations columns: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;

        if has_trigger_data {
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_automations_webhook_id ON automations(json_extract(trigger_data, '$.webhook_id')) WHERE trigger_type = 'webhook'",
                [],
            )
            .map_err(|e| format!("Failed to create automation webhook index: {}", e))?;
        }

        let has_stop_policy: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('automations') WHERE name = 'stop_policy'")
            .map_err(|e| format!("Failed to check stop_policy column: {}", e))?
            .exists([])
            .map_err(|e| format!("Failed to query table info: {}", e))?;
        if !has_stop_policy {
            tracing::info!("Running migration: adding 'stop_policy' column to automations table");
            conn.execute(
                "ALTER TABLE automations ADD COLUMN stop_policy TEXT NOT NULL DEFAULT 'consecutive_failures:2'",
                [],
            )
            .map_err(|e| format!("Failed to add stop_policy column: {}", e))?;
        }

        // Migration: add fresh_session column if it doesn't exist
        let has_fresh_session: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('automations') WHERE name = 'fresh_session'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_fresh_session {
            tracing::info!("Running migration: adding 'fresh_session' column to automations table");
            conn.execute(
                "ALTER TABLE automations ADD COLUMN fresh_session TEXT NOT NULL DEFAULT 'keep'",
                [],
            )
            .map_err(|e| format!("Failed to add fresh_session column: {}", e))?;
        }

        // Migration: add driver column distinguishing OA-scheduled automations
        // from harness-driven native loops (claudecode/codex `/goal`, etc).
        let has_driver: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('automations') WHERE name = 'driver'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_driver {
            tracing::info!("Running migration: adding 'driver' column to automations table");
            conn.execute(
                "ALTER TABLE automations ADD COLUMN driver TEXT NOT NULL DEFAULT 'scheduler'",
                [],
            )
            .map_err(|e| format!("Failed to add driver column: {}", e))?;
        }

        // Cleanup: drop stale inactive harness-loop automations. These are
        // per-`/goal`-cycle UI artifacts that the native_loop_observer now
        // deletes on terminal status; this purges any rows accumulated under
        // the old "deactivate-and-keep" behavior (one mission had 50+ such
        // rows for a single recurring interval). Cascade FK removes the
        // matching automation_executions. Safe to run on every startup —
        // idempotent and only affects rows that should not exist under the
        // current model.
        let purged: usize = conn
            .execute(
                "DELETE FROM automations WHERE active = 0 AND driver = 'harness_loop'",
                [],
            )
            .map_err(|e| format!("Failed to purge inactive harness_loop rows: {}", e))?;
        if purged > 0 {
            tracing::info!(
                "Cleanup: purged {} inactive harness_loop automation rows",
                purged
            );
        }

        Ok(())
    }
}

fn parse_status(s: &str) -> MissionStatus {
    match s {
        "pending" => MissionStatus::Pending,
        "active" => MissionStatus::Active,
        "awaiting_user" => MissionStatus::AwaitingUser,
        "acknowledged" => MissionStatus::Acknowledged,
        "completed" => MissionStatus::Completed,
        "failed" => MissionStatus::Failed,
        "interrupted" => MissionStatus::Interrupted,
        "blocked" => MissionStatus::Blocked,
        "not_feasible" => MissionStatus::NotFeasible,
        "paused" => MissionStatus::Paused,
        _ => MissionStatus::Pending,
    }
}

fn status_to_string(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::Pending => "pending",
        MissionStatus::Active => "active",
        MissionStatus::AwaitingUser => "awaiting_user",
        MissionStatus::Acknowledged => "acknowledged",
        MissionStatus::Completed => "completed",
        MissionStatus::Failed => "failed",
        MissionStatus::Interrupted => "interrupted",
        MissionStatus::Blocked => "blocked",
        MissionStatus::NotFeasible => "not_feasible",
        MissionStatus::Paused => "paused",
    }
}

#[async_trait]
impl MissionStore for SqliteMissionStore {
    fn is_persistent(&self) -> bool {
        true
    }

    async fn list_missions(&self, limit: usize, offset: usize) -> Result<Vec<Mission>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, short_description, metadata_updated_at, metadata_source, metadata_model, metadata_version, workspace_id, workspace_name, agent, model_override,
                            model_effort,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason,
                            config_profile, parent_mission_id, working_directory,
                            COALESCE(mission_mode, 'task') as mission_mode,
                            COALESCE(goal_mode, 0) as goal_mode, goal_objective, first_viewed_at,
                            COALESCE(priority, 0) as priority, not_before, deadline, paused_at
                     FROM missions
                     ORDER BY updated_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(|e| e.to_string())?;

            let missions = stmt
                .query_map(params![limit as i64, offset as i64], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(8)?;
                    let desktop_sessions_json: Option<String> = row.get(17)?;
                    let backend: String = row.get(18)?;
                    let session_id: Option<String> = row.get(19)?;
                    let terminal_reason: Option<String> = row.get(20)?;
                    let config_profile: Option<String> = row.get(21)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        short_description: row.get(3)?,
                        metadata_updated_at: row.get(4)?,
                        metadata_source: row.get(5)?,
                        metadata_model: row.get(6)?,
                        metadata_version: row.get(7)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(9)?,
                        agent: row.get(10)?,
                        model_override: row.get(11)?,
                        model_effort: row.get(12)?,
                        backend,
                        config_profile,
                        history: vec![], // Loaded separately if needed
                        created_at: row.get(13)?,
                        updated_at: row.get(14)?,
                        interrupted_at: row.get(15)?,
                        paused_at: row.get(31).ok().flatten(),
                        resumable: row.get::<_, i32>(16)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id,
                        terminal_reason,
                        parent_mission_id: row.get::<_, Option<String>>(22)?.and_then(|s| Uuid::parse_str(&s).ok()),
                        working_directory: row.get(23)?,
                        mission_mode: row.get::<_, Option<String>>(24)?
                            .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
                            .unwrap_or_default(),
                            goal_mode: row.get::<_, i32>(25).unwrap_or(0) != 0,
                            goal_objective: row.get(26).ok().flatten(),
                            first_viewed_at: row.get(27).ok().flatten(),
                            scheduling: MissionScheduling {
                                priority: row.get::<_, i32>(28).unwrap_or(0),
                                not_before: row.get(29).ok().flatten(),
                                deadline: row.get(30).ok().flatten(),
                            },
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(missions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn count_missions_by_status(&self) -> Result<MissionStatusCounts, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare("SELECT status, COUNT(*) FROM missions GROUP BY status")
                .map_err(|e| e.to_string())?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| e.to_string())?;

            let mut counts = MissionStatusCounts::default();
            for row in rows {
                let (status, count) = row.map_err(|e| e.to_string())?;
                let count = usize::try_from(count).unwrap_or(0);
                counts.total += count;
                match parse_status(&status) {
                    MissionStatus::Active => counts.active += count,
                    MissionStatus::Completed => counts.completed += count,
                    MissionStatus::Failed => counts.failed += count,
                    _ => {}
                }
            }
            Ok(counts)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_mission(&self, id: Uuid) -> Result<Option<Mission>, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // Get mission
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, short_description, metadata_updated_at, metadata_source, metadata_model, metadata_version, workspace_id, workspace_name, agent, model_override,
                            model_effort,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason,
                            config_profile, parent_mission_id, working_directory,
                            COALESCE(mission_mode, 'task') as mission_mode, COALESCE(goal_mode, 0) as goal_mode, goal_objective, first_viewed_at,
                            COALESCE(priority, 0) as priority, not_before, deadline, paused_at FROM missions WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;

            let mission: Option<Mission> = stmt
                .query_row(params![&id_str], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(8)?;
                    let desktop_sessions_json: Option<String> = row.get(17)?;
                    let backend: String = row.get(18)?;
                    let session_id: Option<String> = row.get(19)?;
                    let terminal_reason: Option<String> = row.get(20)?;
                    let config_profile: Option<String> = row.get(21)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        short_description: row.get(3)?,
                        metadata_updated_at: row.get(4)?,
                        metadata_source: row.get(5)?,
                        metadata_model: row.get(6)?,
                        metadata_version: row.get(7)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(9)?,
                        agent: row.get(10)?,
                        model_override: row.get(11)?,
                        model_effort: row.get(12)?,
                        backend,
                        config_profile,
                        history: vec![],
                        created_at: row.get(13)?,
                        updated_at: row.get(14)?,
                        interrupted_at: row.get(15)?,
                        paused_at: row.get(31).ok().flatten(),
                        resumable: row.get::<_, i32>(16)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id,
                        terminal_reason,
                        parent_mission_id: row.get::<_, Option<String>>(22)?.and_then(|s| Uuid::parse_str(&s).ok()),
                        working_directory: row.get(23)?,
                        mission_mode: row.get::<_, Option<String>>(24)?
                            .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
                            .unwrap_or_default(),
                            goal_mode: row.get::<_, i32>(25).unwrap_or(0) != 0,
                            goal_objective: row.get(26).ok().flatten(),
                            first_viewed_at: row.get(27).ok().flatten(),
                            scheduling: MissionScheduling {
                                priority: row.get::<_, i32>(28).unwrap_or(0),
                                not_before: row.get(29).ok().flatten(),
                                deadline: row.get(30).ok().flatten(),
                            },
                    })
                })
                .optional()
                .map_err(|e| e.to_string())?;

            // Load history from events (limited to last 200 messages for performance)
            // Full history can be retrieved via get_events() if needed
            if let Some(mut m) = mission {
                let mut history_stmt = conn
                    .prepare(
                        "SELECT event_type, content, content_file FROM (
                             SELECT event_type, content, content_file, sequence
                             FROM mission_events
                             WHERE mission_id = ?1 AND event_type IN ('user_message', 'assistant_message')
                             ORDER BY sequence DESC
                             LIMIT 200
                         ) ORDER BY sequence ASC",
                    )
                    .map_err(|e| e.to_string())?;

                let history: Vec<MissionHistoryEntry> = history_stmt
                    .query_map(params![&id_str], |row| {
                        let event_type: String = row.get(0)?;
                        let content: Option<String> = row.get(1)?;
                        let content_file: Option<String> = row.get(2)?;
                        let full_content =
                            SqliteMissionStore::load_content(content.as_deref(), content_file.as_deref());
                        Ok(MissionHistoryEntry {
                            role: if event_type == "user_message" {
                                "user".to_string()
                            } else {
                                "assistant".to_string()
                            },
                            content: full_content,
                        })
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;

                m.history = history;
                Ok(Some(m))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

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
    ) -> Result<Mission, String> {
        let conn = self.conn.clone();
        let now = now_string();
        let id = Uuid::new_v4();
        // Inherit workspace from parent mission when not explicitly provided.
        let workspace_id = if let Some(ws) = workspace_id {
            ws
        } else if let Some(parent_id) = parent_mission_id {
            match self.get_mission(parent_id).await {
                Ok(Some(parent)) => parent.workspace_id,
                _ => crate::workspace::DEFAULT_WORKSPACE_ID,
            }
        } else {
            crate::workspace::DEFAULT_WORKSPACE_ID
        };
        let backend = backend.unwrap_or("claudecode").to_string();
        let metadata_source = title.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(METADATA_SOURCE_USER.to_string())
            }
        });
        let metadata_updated_at = metadata_source.as_ref().map(|_| now.clone());
        // Generate session_id for conversation persistence (used by Claude Code --session-id)
        let session_id = Uuid::new_v4().to_string();

        let mission = Mission {
            id,
            status: MissionStatus::Pending,
            title: title.map(|s| s.to_string()),
            short_description: None,
            metadata_updated_at,
            metadata_source,
            metadata_model: None,
            metadata_version: None,
            workspace_id,
            workspace_name: None,
            agent: agent.map(|s| s.to_string()),
            model_override: model_override.map(|s| s.to_string()),
            model_effort: model_effort.map(|s| s.to_string()),
            backend: backend.clone(),
            config_profile: config_profile.map(|s| s.to_string()),
            history: vec![],
            created_at: now.clone(),
            updated_at: now.clone(),
            interrupted_at: None,
            paused_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: Some(session_id.clone()),
            terminal_reason: None,
            parent_mission_id,
            working_directory: working_directory.map(|s| s.to_string()),
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
            first_viewed_at: None,
            scheduling: Default::default(),
        };

        let m = mission.clone();
        let mission_mode_str = serde_json::to_value(&m.mission_mode)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "task".to_string());
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO missions (id, status, title, short_description, metadata_updated_at, metadata_source, metadata_model, metadata_version, workspace_id, agent, model_override, model_effort, backend, config_profile, created_at, updated_at, resumable, session_id, parent_mission_id, working_directory, mission_mode, goal_mode, goal_objective)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
                params![
                    m.id.to_string(),
                    status_to_string(m.status),
                    m.title,
                    m.short_description,
                    m.metadata_updated_at,
                    m.metadata_source,
                    m.metadata_model,
                    m.metadata_version,
                    m.workspace_id.to_string(),
                    m.agent,
                    m.model_override,
                    m.model_effort,
                    m.backend,
                    m.config_profile,
                    m.created_at,
                    m.updated_at,
                    0,
                    m.session_id,
                    m.parent_mission_id.map(|id| id.to_string()),
                    m.working_directory,
                    mission_mode_str,
                    if m.goal_mode { 1i64 } else { 0i64 },
                    m.goal_objective,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;

        Ok(mission)
    }

    async fn get_child_missions(&self, parent_id: Uuid) -> Result<Vec<Mission>, String> {
        let conn = self.conn.clone();
        let parent_id_str = parent_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare("SELECT id, status, title, short_description, metadata_updated_at, metadata_source, metadata_model, metadata_version, workspace_id, agent, model_override, model_effort, backend, config_profile, created_at, updated_at, interrupted_at, resumable, session_id, terminal_reason, parent_mission_id, working_directory, COALESCE(mission_mode, 'task') as mission_mode FROM missions WHERE parent_mission_id = ?1")
                .map_err(|e| e.to_string())?;
            let missions = stmt
                .query_map(params![parent_id_str], |row| {
                    Ok(Mission {
                        id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_default(),
                        status: parse_status(&row.get::<_, String>(1)?),
                        title: row.get(2)?,
                        short_description: row.get(3)?,
                        metadata_updated_at: row.get(4)?,
                        metadata_source: row.get(5)?,
                        metadata_model: row.get(6)?,
                        metadata_version: row.get(7)?,
                        workspace_id: Uuid::parse_str(&row.get::<_, String>(8)?).unwrap_or_default(),
                        workspace_name: None,
                        agent: row.get(9)?,
                        model_override: row.get(10)?,
                        model_effort: row.get(11)?,
                        backend: row.get::<_, String>(12)?,
                        config_profile: row.get(13)?,
                        history: vec![],
                        created_at: row.get(14)?,
                        updated_at: row.get(15)?,
                        interrupted_at: row.get(16)?,
                        paused_at: None,
                        resumable: row.get::<_, i32>(17)? != 0,
                        desktop_sessions: Vec::new(),
                        session_id: row.get(18)?,
                        terminal_reason: row.get(19)?,
                        parent_mission_id: row.get::<_, Option<String>>(20)?.and_then(|s| Uuid::parse_str(&s).ok()),
                        working_directory: row.get(21)?,
                        mission_mode: row.get::<_, Option<String>>(22)?
                            .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
                            .unwrap_or_default(),
                            goal_mode: row.get::<_, i32>(23).unwrap_or(0) != 0,
                            goal_objective: row.get(24).ok().flatten(),
                            first_viewed_at: None,
                            scheduling: Default::default(),
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(missions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_status(&self, id: Uuid, status: MissionStatus) -> Result<(), String> {
        self.update_mission_status_with_reason(id, status, None)
            .await
    }

    async fn set_mission_scheduling(
        &self,
        id: Uuid,
        scheduling: &MissionScheduling,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let priority = scheduling.priority;
        let not_before = scheduling.not_before.clone();
        let deadline = scheduling.deadline.clone();
        let now = now_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET priority = ?1, not_before = ?2, deadline = ?3, updated_at = ?4 WHERE id = ?5",
                params![priority, not_before, deadline, now, id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_status_with_reason(
        &self,
        id: Uuid,
        status: MissionStatus,
        terminal_reason: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let interrupted_at =
            if matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked) {
                Some(now.clone())
            } else {
                None
            };
        // Failed missions with LlmError are also resumable (transient API errors).
        // AwaitingUser missions are also resumable (the user can send another
        // message at any time to wake the agent back up).
        let resumable = matches!(
            status,
            MissionStatus::Interrupted
                | MissionStatus::Blocked
                | MissionStatus::Failed
                | MissionStatus::AwaitingUser
                | MissionStatus::Acknowledged
        );
        let terminal_reason = terminal_reason.map(|s| s.to_string());
        // Transitioning back to Active means the user just sent a new message —
        // clear `first_viewed_at` so the next AwaitingUser round starts fresh
        // (and so the "opened" dot disappears once the agent picks up again).
        let clear_first_viewed_at = matches!(status, MissionStatus::Active);

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            // Read the old status before the UPDATE so we can decide whether
            // to invalidate the Paloma cooldown. Wiping cooldown on a
            // no-op status write (e.g. heartbeat that re-sets Active=Active)
            // would let "still running" alerts skip past the user's
            // exponential backoff and arrive too soon.
            let previous_status: Option<String> = conn
                .query_row(
                    "SELECT status FROM missions WHERE id = ?1",
                    params![id.to_string()],
                    |row| row.get(0),
                )
                .ok();
            if clear_first_viewed_at {
                conn.execute(
                    "UPDATE missions SET status = ?1, updated_at = ?2, interrupted_at = ?3, resumable = ?4, terminal_reason = ?5, first_viewed_at = NULL WHERE id = ?6",
                    params![
                        status_to_string(status),
                        now,
                        interrupted_at,
                        if resumable { 1 } else { 0 },
                        terminal_reason,
                        id.to_string(),
                    ],
                )
                .map_err(|e| e.to_string())?;
            } else {
                conn.execute(
                    "UPDATE missions SET status = ?1, updated_at = ?2, interrupted_at = ?3, resumable = ?4, terminal_reason = ?5 WHERE id = ?6",
                    params![
                        status_to_string(status),
                        now,
                        interrupted_at,
                        if resumable { 1 } else { 0 },
                        terminal_reason,
                        id.to_string(),
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            // Only reset cooldown on a *genuine* status transition. Best
            // effort — if the table is missing or the row is gone, nothing
            // to do.
            let status_changed = previous_status.as_deref() != Some(status_to_string(status));
            if status_changed {
                let _ = conn.execute(
                    "DELETE FROM paloma_cooldown_state WHERE mission_id = ?1",
                    params![id.to_string()],
                );
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_history(
        &self,
        id: Uuid,
        _history: &[MissionHistoryEntry],
    ) -> Result<(), String> {
        // For SQLite store, history is derived from events logged via log_event().
        // This method only updates the mission's updated_at timestamp.
        // Events are NOT inserted here to avoid race condition duplicates with the
        // event logger task that also inserts via log_event().
        let conn = self.conn.clone();
        let now = now_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            conn.execute(
                "UPDATE missions SET updated_at = ?1 WHERE id = ?2",
                params![&now, id.to_string()],
            )
            .map_err(|e| e.to_string())?;

            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn set_mission_first_viewed_at_if_unset(
        &self,
        id: Uuid,
        timestamp: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let ts = timestamp.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let updated = conn
                .execute(
                    "UPDATE missions SET first_viewed_at = ?1 WHERE id = ?2 AND first_viewed_at IS NULL",
                    params![&ts, &id_str],
                )
                .map_err(|e| e.to_string())?;
            Ok(if updated > 0 { Some(ts) } else { None })
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn acknowledge_stale_awaiting_user_missions(
        &self,
        grace_seconds: u64,
    ) -> Result<Vec<Uuid>, String> {
        let conn = self.conn.clone();
        let cutoff = (Utc::now() - chrono::Duration::seconds(grace_seconds as i64)).to_rfc3339();
        let now = now_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            let ids: Vec<Uuid> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM missions
                         WHERE status = 'awaiting_user'
                           AND first_viewed_at IS NOT NULL
                           AND first_viewed_at <= ?1",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&cutoff], |row| {
                        let id_str: String = row.get(0)?;
                        Ok(parse_uuid_or_nil(&id_str))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                rows
            };
            if !ids.is_empty() {
                tx.execute(
                    "UPDATE missions SET status = 'acknowledged', updated_at = ?1
                     WHERE status = 'awaiting_user'
                       AND first_viewed_at IS NOT NULL
                       AND first_viewed_at <= ?2",
                    params![&now, &cutoff],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            Ok(ids)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_desktop_sessions(
        &self,
        id: Uuid,
        sessions: &[DesktopSessionInfo],
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let sessions_json = serde_json::to_string(sessions).unwrap_or_else(|_| "[]".to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET desktop_sessions = ?1, updated_at = ?2 WHERE id = ?3",
                params![sessions_json, now, id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_title(&self, id: Uuid, title: &str) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let title = title.to_string();
        let source = "user".to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions
                 SET title = ?1,
                     metadata_source = ?2,
                     metadata_model = NULL,
                     metadata_version = NULL,
                     metadata_updated_at = ?3,
                     updated_at = ?4
                 WHERE id = ?5",
                params![title, source, now.clone(), now, id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn save_control_queue(&self, _user_id: &str, payload: &str) -> Result<(), String> {
        let conn = self.conn.clone();
        let payload = payload.to_string();
        let now = now_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO control_queue (id, payload, updated_at) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET payload = ?1, updated_at = ?2",
                params![payload, now],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn load_control_queue(&self, _user_id: &str) -> Result<String, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload FROM control_queue WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            Ok(payload.unwrap_or_default())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_run_settings(
        &self,
        id: Uuid,
        backend: Option<&str>,
        agent: Option<Option<&str>>,
        model_override: Option<Option<&str>>,
        model_effort: Option<Option<&str>>,
        config_profile: Option<Option<&str>>,
        session_id: &str,
    ) -> Result<Mission, String> {
        let conn = self.conn.clone();
        let now = now_string();
        let backend_set = backend.is_some();
        let agent_set = agent.is_some();
        let model_override_set = model_override.is_some();
        let model_effort_set = model_effort.is_some();
        let config_profile_set = config_profile.is_some();
        let backend = backend.map(ToString::to_string);
        let agent = agent.flatten().map(ToString::to_string);
        let model_override = model_override.flatten().map(ToString::to_string);
        let model_effort = model_effort.flatten().map(ToString::to_string);
        let config_profile = config_profile.flatten().map(ToString::to_string);
        let session_id = session_id.to_string();
        let id_str = id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let changed = conn
                .execute(
                    "UPDATE missions
                     SET backend = CASE WHEN ?1 THEN ?2 ELSE backend END,
                         agent = CASE WHEN ?3 THEN ?4 ELSE agent END,
                         model_override = CASE WHEN ?5 THEN ?6 ELSE model_override END,
                         model_effort = CASE WHEN ?7 THEN ?8 ELSE model_effort END,
                         config_profile = CASE WHEN ?9 THEN ?10 ELSE config_profile END,
                         session_id = ?11,
                         resumable = 0,
                         interrupted_at = NULL,
                         terminal_reason = NULL,
                         updated_at = ?12
                     WHERE id = ?13",
                    params![
                        backend_set,
                        backend,
                        agent_set,
                        agent,
                        model_override_set,
                        model_override,
                        model_effort_set,
                        model_effort,
                        config_profile_set,
                        config_profile,
                        session_id,
                        now,
                        id_str,
                    ],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err(format!("Mission {} not found", id_str));
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;

        self.get_mission(id)
            .await?
            .ok_or_else(|| format!("Mission {} not found", id))
    }

    async fn update_mission_metadata(
        &self,
        id: Uuid,
        title: Option<Option<&str>>,
        short_description: Option<Option<&str>>,
        metadata_source: Option<Option<&str>>,
        metadata_model: Option<Option<&str>>,
        metadata_version: Option<Option<&str>>,
    ) -> Result<(), String> {
        if title.is_none()
            && short_description.is_none()
            && metadata_source.is_none()
            && metadata_model.is_none()
            && metadata_version.is_none()
        {
            return Ok(());
        }

        let conn = self.conn.clone();
        let now = now_string();
        let title_set = title.is_some();
        let short_description_set = short_description.is_some();
        let metadata_source_set = metadata_source.is_some();
        let metadata_model_set = metadata_model.is_some();
        let metadata_version_set = metadata_version.is_some();
        let title = title.flatten().map(|s| s.to_string());
        let short_description = short_description.flatten().map(|s| s.to_string());
        let metadata_source = metadata_source.flatten().map(|s| s.to_string());
        let metadata_model = metadata_model.flatten().map(|s| s.to_string());
        let metadata_version = metadata_version.flatten().map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions
                 SET title = CASE WHEN ?1 THEN ?2 ELSE title END,
                     short_description = CASE WHEN ?3 THEN ?4 ELSE short_description END,
                     metadata_source = CASE WHEN ?5 THEN ?6 ELSE metadata_source END,
                     metadata_model = CASE WHEN ?7 THEN ?8 ELSE metadata_model END,
                     metadata_version = CASE WHEN ?9 THEN ?10 ELSE metadata_version END,
                     metadata_updated_at = ?11,
                     updated_at = ?11
                 WHERE id = ?12",
                params![
                    title_set,
                    title,
                    short_description_set,
                    short_description,
                    metadata_source_set,
                    metadata_source,
                    metadata_model_set,
                    metadata_model,
                    metadata_version_set,
                    metadata_version,
                    now,
                    id.to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_session_id(&self, id: Uuid, session_id: &str) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let session_id = session_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET session_id = ?1, updated_at = ?2 WHERE id = ?3",
                params![session_id, now, id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_goal(
        &self,
        id: Uuid,
        goal_mode: bool,
        goal_objective: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let goal_objective = goal_objective.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET goal_mode = ?1, goal_objective = ?2, updated_at = ?3 WHERE id = ?4",
                params![
                    if goal_mode { 1i64 } else { 0i64 },
                    goal_objective,
                    now,
                    id.to_string()
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_mission_tree(&self, id: Uuid, tree: &AgentTreeNode) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let tree_json = serde_json::to_string(tree).map_err(|e| e.to_string())?;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT OR REPLACE INTO mission_trees (mission_id, tree_json, updated_at)
                 VALUES (?1, ?2, ?3)",
                params![id.to_string(), tree_json, now],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_mission_tree(&self, id: Uuid) -> Result<Option<AgentTreeNode>, String> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let tree_json: Option<String> = conn
                .query_row(
                    "SELECT tree_json FROM mission_trees WHERE mission_id = ?1",
                    params![id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;

            if let Some(json) = tree_json {
                let tree: AgentTreeNode = serde_json::from_str(&json).map_err(|e| e.to_string())?;
                Ok(Some(tree))
            } else {
                Ok(None)
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn delete_mission(&self, id: Uuid) -> Result<bool, String> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let rows = conn
                .execute(
                    "DELETE FROM missions WHERE id = ?1",
                    params![id.to_string()],
                )
                .map_err(|e| e.to_string())?;
            Ok(rows > 0)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn delete_empty_untitled_missions_excluding(
        &self,
        exclude: &[Uuid],
    ) -> Result<usize, String> {
        let conn = self.conn.clone();
        let exclude_strs: Vec<String> = exclude.iter().map(|id| id.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // Find missions to delete
            let mut stmt = conn
                .prepare(
                    "SELECT m.id, COALESCE(goal_mode, 0) as goal_mode, goal_objective FROM missions m
                     LEFT JOIN mission_events e ON m.id = e.mission_id AND e.event_type IN ('user_message', 'assistant_message')
                     WHERE m.status = 'active'
                       AND (m.title IS NULL OR m.title = '' OR m.title = 'Untitled Mission')
                     GROUP BY m.id
                     HAVING COUNT(e.id) = 0",
                )
                .map_err(|e| e.to_string())?;

            let to_delete: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .filter(|id| !exclude_strs.contains(id))
                .collect();

            let count = to_delete.len();
            for id in to_delete {
                conn.execute("DELETE FROM missions WHERE id = ?1", params![id])
                    .ok();
            }

            Ok(count)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_stale_active_missions(&self, stale_hours: u64) -> Result<Vec<Mission>, String> {
        if stale_hours == 0 {
            return Ok(Vec::new());
        }

        let conn = self.conn.clone();
        let cutoff = Utc::now() - chrono::Duration::hours(stale_hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        // A mission is stale only when *both* its metadata `updated_at` and
        // its newest event are older than the cutoff. Previously the query
        // looked at `updated_at` alone, but `updated_at` is bumped by
        // `update_mission_history` (assistant-turn boundary) and metadata
        // writes — not by individual `tool_call` / `tool_result` events. A
        // long agent run that's actively producing tool calls but no
        // assistant turns for >2h (e.g. waiting on a CI build via repeated
        // `gh run watch` invocations) would falsely trip this scan. Joining
        // against `mission_events.timestamp` ties the stale signal to real
        // activity.
        //
        // We compute `MAX(timestamp)` rather than the timestamp of the row
        // with the highest `sequence`: `log_event` updates an existing row
        // in-place when it sees a duplicate `event_id` (e.g. the
        // `text_delta_latest` row gets its `timestamp` rewritten on every
        // streamed delta), so a higher-sequence row can carry an *older*
        // timestamp than a refreshed lower-sequence row. The query is backed
        // by `idx_events_mission_timestamp(mission_id, timestamp DESC)` so
        // the per-mission MAX is an O(log n) seek; missions with no events
        // fall back to `updated_at` via COALESCE.
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, COALESCE(goal_mode, 0) as goal_mode, goal_objective FROM missions m
                     WHERE status = 'active'
                       AND max(
                             m.updated_at,
                             COALESCE(
                               (SELECT MAX(timestamp) FROM mission_events
                                WHERE mission_id = m.id),
                               m.updated_at
                             )
                           ) < ?1",
                )
                .map_err(|e| e.to_string())?;

            let missions = stmt
                .query_map(params![cutoff_str], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(11)?;
                    let backend: String = row.get(12)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        short_description: None,
                        metadata_updated_at: None,
                        metadata_source: None,
                        metadata_model: None,
                        metadata_version: None,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        model_effort: None, // Not needed for stale mission checks
                        backend,
                        config_profile: None, // Not needed for stale mission checks
                        history: vec![],
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        interrupted_at: row.get(9)?,
                        paused_at: None,
                        resumable: row.get::<_, i32>(10)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id: None, // Not needed for stale mission checks
                        terminal_reason: None,
                        parent_mission_id: None,
                        working_directory: None,
                        mission_mode: MissionMode::default(),
                        goal_mode: false,
                        goal_objective: None,
                        first_viewed_at: None,
                        scheduling: Default::default(),
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(missions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_all_active_missions(&self) -> Result<Vec<Mission>, String> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend,
                            COALESCE(mission_mode, 'task') as mission_mode,
                            COALESCE(goal_mode, 0) as goal_mode,
                            goal_objective
                     FROM missions
                     WHERE status = 'active'",
                )
                .map_err(|e| e.to_string())?;

            let missions = stmt
                .query_map(params![], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(11)?;
                    let backend: String = row.get(12)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        short_description: None,
                        metadata_updated_at: None,
                        metadata_source: None,
                        metadata_model: None,
                        metadata_version: None,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        model_effort: None, // Not needed for active mission checks
                        backend,
                        config_profile: None, // Not needed for active mission checks
                        history: vec![],
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        interrupted_at: row.get(9)?,
                        paused_at: None,
                        resumable: row.get::<_, i32>(10)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id: None,
                        terminal_reason: None,
                        parent_mission_id: None,
                        working_directory: None,
                        mission_mode: row
                            .get::<_, Option<String>>(13)?
                            .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
                            .unwrap_or_default(),
                        goal_mode: row.get::<_, i32>(14).unwrap_or(0) != 0,
                        goal_objective: row.get(15).ok().flatten(),
                        first_viewed_at: None,
                        scheduling: Default::default(),
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(missions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn set_deferred_goal(
        &self,
        mission_id: Uuid,
        goal: Option<String>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let id = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET deferred_goal = ?1 WHERE id = ?2",
                params![goal, id],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_deferred_goal(&self, mission_id: Uuid) -> Result<Option<String>, String> {
        let conn = self.conn.clone();
        let id = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT deferred_goal FROM missions WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn set_mission_paused_at(
        &self,
        mission_id: Uuid,
        paused_at: Option<String>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let id = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET paused_at = ?1 WHERE id = ?2",
                params![paused_at, id],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_scheduled_pending_missions(&self) -> Result<Vec<Mission>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend,
                            COALESCE(mission_mode, 'task') as mission_mode,
                            COALESCE(goal_mode, 0) as goal_mode,
                            goal_objective,
                            COALESCE(priority, 0) as priority, not_before, deadline
                     FROM missions
                     WHERE status = 'pending' AND deferred_goal IS NOT NULL
                     ORDER BY created_at ASC",
                )
                .map_err(|e| e.to_string())?;

            let missions = stmt
                .query_map(params![], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(11)?;
                    let backend: String = row.get(12)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        short_description: None,
                        metadata_updated_at: None,
                        metadata_source: None,
                        metadata_model: None,
                        metadata_version: None,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        model_effort: None,
                        backend,
                        config_profile: None,
                        history: vec![],
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        interrupted_at: row.get(9)?,
                        paused_at: None,
                        resumable: row.get::<_, i32>(10)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id: None,
                        terminal_reason: None,
                        parent_mission_id: None,
                        working_directory: None,
                        mission_mode: row
                            .get::<_, Option<String>>(13)?
                            .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
                            .unwrap_or_default(),
                        goal_mode: row.get::<_, i32>(14).unwrap_or(0) != 0,
                        goal_objective: row.get(15).ok().flatten(),
                        first_viewed_at: None,
                        scheduling: MissionScheduling {
                            priority: row.get::<_, i32>(16).unwrap_or(0),
                            not_before: row.get(17).ok().flatten(),
                            deadline: row.get(18).ok().flatten(),
                        },
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(missions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_recent_server_shutdown_mission_ids(
        &self,
        max_age_hours: u64,
    ) -> Result<Vec<Uuid>, String> {
        let conn = self.conn.clone();
        let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id
                     FROM missions
                     WHERE status = 'interrupted'
                       AND resumable = 1
                       AND terminal_reason = 'server_shutdown'
                       AND COALESCE(mission_mode, 'task') != 'assistant'
                       AND interrupted_at IS NOT NULL
                       AND interrupted_at >= ?1
                     ORDER BY interrupted_at ASC",
                )
                .map_err(|e| e.to_string())?;

            let mission_ids = stmt
                .query_map(params![cutoff_str], |row| {
                    let id_str: String = row.get(0)?;
                    Ok(parse_uuid_or_nil(&id_str))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(mission_ids)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn insert_mission_summary(
        &self,
        mission_id: Uuid,
        summary: &str,
        key_files: &[String],
        success: bool,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let now = now_string();
        let summary = summary.to_string();
        let key_files_json = serde_json::to_string(key_files).unwrap_or_else(|_| "[]".to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO mission_summaries (mission_id, summary, key_files, success, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    mission_id.to_string(),
                    summary,
                    key_files_json,
                    if success { 1 } else { 0 },
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    // === Event logging methods ===

    async fn log_event(&self, mission_id: Uuid, event: &AgentEvent) -> Result<(), String> {
        if matches!(event, AgentEvent::UserMessage { queued: true, .. }) {
            // Keep queued messages out of persisted mission history until they actually start.
            return Ok(());
        }

        let conn = self.conn.clone();
        let content_dir = self.content_dir.clone();
        let now = now_string();
        let mid = mission_id.to_string();

        if let AgentEvent::TextOp { bubble_id, ops, .. } = event {
            let bubble_id = bubble_id.clone();
            let ops = ops.clone();
            let has_finalize = ops.iter().any(|op| matches!(op, TextOp::Finalize));
            return tokio::task::spawn_blocking(move || {
                let conn = conn.blocking_lock();

                if has_finalize {
                    let mut stmt = conn
                        .prepare(
                            "SELECT content, content_file
                             FROM mission_events
                             WHERE mission_id = ?1
                               AND event_type = 'text_op'
                               AND json_extract(metadata, '$.bubble_id') = ?2
                             ORDER BY sequence ASC",
                        )
                        .map_err(|e| e.to_string())?;
                    let rows = stmt
                        .query_map(params![&mid, &bubble_id], |row| {
                            let content: Option<String> = row.get(0)?;
                            let content_file: Option<String> = row.get(1)?;
                            Ok(SqliteMissionStore::load_content(
                                content.as_deref(),
                                content_file.as_deref(),
                            ))
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    drop(stmt);

                    let mut content = String::new();
                    for row in rows {
                        let row_ops: Vec<TextOp> = serde_json::from_str(&row).unwrap_or_default();
                        apply_text_ops(&mut content, &row_ops);
                    }
                    apply_text_ops(&mut content, &ops);

                    conn.execute(
                        "DELETE FROM mission_events
                         WHERE mission_id = ?1
                           AND event_type = 'text_op'
                           AND json_extract(metadata, '$.bubble_id') = ?2",
                        params![&mid, &bubble_id],
                    )
                    .map_err(|e| e.to_string())?;

                    let sequence: i64 = conn
                        .query_row(
                            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM mission_events WHERE mission_id = ?1",
                            params![&mid],
                            |row| row.get(0),
                        )
                        .unwrap_or(1);
                    let (content_inline, content_file) = SqliteMissionStore::store_content(
                        &content_dir,
                        mission_id,
                        sequence,
                        "assistant_message_canonical",
                        &content,
                    );
                    conn.execute(
                        "INSERT INTO mission_events
                         (mission_id, sequence, event_type, timestamp, event_id, content, content_file, metadata)
                         VALUES (?1, ?2, 'assistant_message_canonical', ?3, ?4, ?5, ?6, ?7)",
                        params![
                            &mid,
                            sequence,
                            &now,
                            &bubble_id,
                            content_inline,
                            content_file,
                            serde_json::json!({
                                "bubble_id": bubble_id,
                                "canonical_from": "text_op"
                            })
                            .to_string(),
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    let sequence: i64 = conn
                        .query_row(
                            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM mission_events WHERE mission_id = ?1",
                            params![&mid],
                            |row| row.get(0),
                        )
                        .unwrap_or(1);
                    let content = serde_json::to_string(&ops).unwrap_or_else(|_| "[]".to_string());
                    let (content_inline, content_file) = SqliteMissionStore::store_content(
                        &content_dir,
                        mission_id,
                        sequence,
                        "text_op",
                        &content,
                    );
                    conn.execute(
                        "INSERT INTO mission_events
                         (mission_id, sequence, event_type, timestamp, content, content_file, metadata)
                         VALUES (?1, ?2, 'text_op', ?3, ?4, ?5, ?6)",
                        params![
                            &mid,
                            sequence,
                            &now,
                            content_inline,
                            content_file,
                            serde_json::json!({ "bubble_id": bubble_id }).to_string(),
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                }

                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        }

        // Extract event data
        let (event_type, event_id, tool_call_id, tool_name, content, metadata) = match event {
            AgentEvent::UserMessage {
                id,
                content,
                queued,
                source,
                ..
            } => (
                "user_message",
                Some(id.to_string()),
                None,
                None,
                content.clone(),
                {
                    // Always record `queued`; record `source` only when present so
                    // the persisted metadata is the forensic record of who/what
                    // posted this message (audit/attribution).
                    let mut meta = serde_json::Map::new();
                    meta.insert("queued".to_string(), serde_json::json!(queued));
                    if let Some(src) = source {
                        meta.insert("source".to_string(), serde_json::json!(src));
                    }
                    serde_json::Value::Object(meta)
                },
            ),
            AgentEvent::AssistantMessage {
                id,
                content,
                success,
                cost_cents,
                cost_source,
                usage,
                model,
                model_normalized,
                shared_files,
                resumable,
                completion_evidence,
                ..
            } => (
                "assistant_message",
                Some(id.to_string()),
                None,
                None,
                content.clone(),
                assistant_message_metadata(AssistantMessageMetadataInput {
                    success: *success,
                    cost_cents: *cost_cents,
                    cost_source: *cost_source,
                    usage,
                    model,
                    model_normalized,
                    shared_files,
                    resumable: *resumable,
                    completion_evidence,
                }),
            ),
            AgentEvent::Thinking { content, done, .. } => (
                "thinking",
                None,
                None,
                None,
                content.clone(),
                serde_json::json!({ "done": done }),
            ),
            AgentEvent::ToolCall {
                tool_call_id,
                name,
                args,
                ..
            } => (
                "tool_call",
                None,
                Some(tool_call_id.clone()),
                Some(name.clone()),
                args.to_string(),
                serde_json::json!({}),
            ),
            AgentEvent::ToolResult {
                tool_call_id,
                name,
                result,
                ..
            } => (
                "tool_result",
                None,
                Some(tool_call_id.clone()),
                Some(name.clone()),
                result.to_string(),
                serde_json::json!({}),
            ),
            AgentEvent::Error {
                message, resumable, ..
            } => (
                "error",
                None,
                None,
                None,
                message.clone(),
                serde_json::json!({ "resumable": resumable }),
            ),
            AgentEvent::TextDelta { content, .. } => (
                "text_delta",
                Some("text_delta_latest".to_string()),
                None,
                None,
                content.clone(),
                serde_json::json!({}),
            ),
            AgentEvent::TextOp { .. } => return Ok(()),
            AgentEvent::MissionStatusChanged {
                status, summary, ..
            } => (
                "mission_status_changed",
                None,
                None,
                None,
                summary.clone().unwrap_or_default(),
                serde_json::json!({ "status": status.to_string() }),
            ),
            AgentEvent::MissionMetadataUpdated {
                title,
                short_description,
                metadata_updated_at,
                updated_at,
                metadata_source,
                metadata_model,
                metadata_version,
                ..
            } => (
                "mission_metadata_updated",
                None,
                None,
                None,
                title.clone().unwrap_or_default(),
                serde_json::json!({
                    "title": title,
                    "short_description": short_description,
                    "metadata_updated_at": metadata_updated_at,
                    "updated_at": updated_at,
                    "metadata_source": metadata_source,
                    "metadata_model": metadata_model,
                    "metadata_version": metadata_version
                }),
            ),
            AgentEvent::MissionSettingsUpdated {
                backend,
                agent,
                model_override,
                model_effort,
                config_profile,
                session_id,
                updated_at,
                ..
            } => (
                "mission_settings_updated",
                None,
                None,
                None,
                backend.clone(),
                serde_json::json!({
                    "backend": backend,
                    "agent": agent,
                    "model_override": model_override,
                    "model_effort": model_effort,
                    "config_profile": config_profile,
                    "session_id": session_id,
                    "updated_at": updated_at
                }),
            ),
            AgentEvent::GoalIteration {
                iteration,
                objective,
                ..
            } => (
                "goal_iteration",
                None,
                None,
                None,
                objective.clone(),
                serde_json::json!({ "iteration": iteration }),
            ),
            AgentEvent::GoalStatus {
                status, objective, ..
            } => (
                "goal_status",
                None,
                None,
                None,
                objective.clone(),
                serde_json::json!({ "status": status }),
            ),
            // Skip events that are less important for debugging
            AgentEvent::Status { .. }
            | AgentEvent::AgentPhase { .. }
            | AgentEvent::AgentTree { .. }
            | AgentEvent::Progress { .. }
            | AgentEvent::SessionIdUpdate { .. }
            | AgentEvent::MissionActivity { .. }
            | AgentEvent::MissionTitleChanged { .. }
            | AgentEvent::FidoSignRequest { .. } => return Ok(()),
        };

        let event_type = event_type.to_string();
        let metadata_str = metadata.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // If this event has an event_id that already exists for this mission,
            // update the existing row's metadata instead of inserting a duplicate.
            if let Some(ref eid) = event_id {
                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM mission_events WHERE mission_id = ?1 AND event_id = ?2",
                        params![&mid, eid],
                        |row| row.get(0),
                    )
                    .optional()
                    .unwrap_or(None);

                if let Some(row_id) = existing {
                    let (content_inline, content_file) = SqliteMissionStore::store_content(
                        &content_dir,
                        mission_id,
                        row_id,
                        &event_type,
                        &content,
                    );
                    if event_type == "text_delta" {
                        let sequence: i64 = conn
                            .query_row(
                                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM mission_events WHERE mission_id = ?1",
                                params![&mid],
                                |row| row.get(0),
                            )
                            .unwrap_or(1);
                        conn.execute(
                            "UPDATE mission_events
                             SET sequence = ?1, metadata = ?2, timestamp = ?3, content = ?4, content_file = ?5
                             WHERE id = ?6",
                            params![sequence, metadata_str, now, content_inline, content_file, row_id],
                        )
                        .map_err(|e| e.to_string())?;
                    } else {
                        conn.execute(
                            "UPDATE mission_events
                             SET metadata = ?1, timestamp = ?2, content = ?3, content_file = ?4
                             WHERE id = ?5",
                            params![metadata_str, now, content_inline, content_file, row_id],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    return Ok(());
                }
            }

            // Get next sequence
            let sequence: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) + 1 FROM mission_events WHERE mission_id = ?1",
                    params![&mid],
                    |row| row.get(0),
                )
                .unwrap_or(1);

            // Store content
            let (content_inline, content_file) = SqliteMissionStore::store_content(
                &content_dir,
                mission_id,
                sequence,
                &event_type,
                &content,
            );

            conn.execute(
                "INSERT INTO mission_events
                 (mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    mid,
                    sequence,
                    event_type,
                    now,
                    event_id,
                    tool_call_id,
                    tool_name,
                    content_inline,
                    content_file,
                    metadata_str,
                ],
            )
            .map_err(|e| e.to_string())?;

            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_latest_events(
        &self,
        mission_id: Uuid,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let limit = limit.clamp(1, 50_000) as i64;
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, sequence, event_type, timestamp, event_id,
                            tool_call_id, tool_name, content, content_file, metadata
                     FROM mission_events
                     WHERE mission_id = ?1
                     ORDER BY sequence DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![mid, limit], |row| {
                    let content: Option<String> = row.get(8)?;
                    let content_file: Option<String> = row.get(9)?;
                    let full_content = SqliteMissionStore::load_content(
                        content.as_deref(),
                        content_file.as_deref(),
                    );
                    let metadata_str: String = row
                        .get::<_, Option<String>>(10)?
                        .unwrap_or_else(|| "{}".to_string());
                    let mid_str: String = row.get(1)?;
                    Ok(StoredEvent {
                        id: row.get(0)?,
                        mission_id: parse_uuid_or_nil(&mid_str),
                        sequence: row.get(2)?,
                        event_type: row.get(3)?,
                        timestamp: row.get(4)?,
                        event_id: row.get(5)?,
                        tool_call_id: row.get(6)?,
                        tool_name: row.get(7)?,
                        content: full_content,
                        metadata: serde_json::from_str(&metadata_str)
                            .unwrap_or(serde_json::json!({})),
                    })
                })
                .map_err(|e| e.to_string())?;
            let mut events = Vec::new();
            for row in rows {
                events.push(row.map_err(|e| e.to_string())?);
            }
            // SQL returned DESC; flip to ASC so callers can keep their
            // existing `iter().rev()` semantics for "walk newest first".
            events.reverse();
            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Option<Vec<String>> =
            event_types.map(|t| t.iter().map(|s| s.to_string()).collect());
        let limit = limit.unwrap_or(50000) as i64;
        let offset = offset.unwrap_or(0) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            let query = if types.is_some() {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM mission_events
                 WHERE mission_id = ?1 AND event_type IN (SELECT value FROM json_each(?2))
                 ORDER BY sequence ASC
                 LIMIT ?3 OFFSET ?4"
            } else {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM mission_events
                 WHERE mission_id = ?1
                 ORDER BY sequence ASC
                 LIMIT ?2 OFFSET ?3"
            };

            // Helper closure to parse a row into StoredEvent
            fn parse_row(row: &rusqlite::Row<'_>) -> Result<StoredEvent, rusqlite::Error> {
                let content: Option<String> = row.get(8)?;
                let content_file: Option<String> = row.get(9)?;
                let full_content = SqliteMissionStore::load_content(content.as_deref(), content_file.as_deref());
                let metadata_str: String = row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "{}".to_string());
                let mid_str: String = row.get(1)?;

                Ok(StoredEvent {
                    id: row.get(0)?,
                    mission_id: parse_uuid_or_nil(&mid_str),
                    sequence: row.get(2)?,
                    event_type: row.get(3)?,
                    timestamp: row.get(4)?,
                    event_id: row.get(5)?,
                    tool_call_id: row.get(6)?,
                    tool_name: row.get(7)?,
                    content: full_content,
                    metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
                })
            }

            let events: Vec<StoredEvent> = if let Some(types) = types {
                let types_json = serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt.query_map(params![&mid, &types_json, limit, offset], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            } else {
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt.query_map(params![&mid, limit, offset], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            };

            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_events_since(
        &self,
        mission_id: Uuid,
        since_seq: i64,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Option<Vec<String>> =
            event_types.map(|t| t.iter().map(|s| s.to_string()).collect());
        let limit = limit.unwrap_or(50000) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            let query = if types.is_some() {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM mission_events
                 WHERE mission_id = ?1 AND sequence > ?2 AND event_type IN (SELECT value FROM json_each(?3))
                 ORDER BY sequence ASC
                 LIMIT ?4"
            } else {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM mission_events
                 WHERE mission_id = ?1 AND sequence > ?2
                 ORDER BY sequence ASC
                 LIMIT ?3"
            };

            fn parse_row(row: &rusqlite::Row<'_>) -> Result<StoredEvent, rusqlite::Error> {
                let content: Option<String> = row.get(8)?;
                let content_file: Option<String> = row.get(9)?;
                let full_content = SqliteMissionStore::load_content(content.as_deref(), content_file.as_deref());
                let metadata_str: String = row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "{}".to_string());
                let mid_str: String = row.get(1)?;

                Ok(StoredEvent {
                    id: row.get(0)?,
                    mission_id: parse_uuid_or_nil(&mid_str),
                    sequence: row.get(2)?,
                    event_type: row.get(3)?,
                    timestamp: row.get(4)?,
                    event_id: row.get(5)?,
                    tool_call_id: row.get(6)?,
                    tool_name: row.get(7)?,
                    content: full_content,
                    metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
                })
            }

            let events: Vec<StoredEvent> = if let Some(types) = types {
                let types_json = serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid, since_seq, &types_json, limit], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            } else {
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid, since_seq, limit], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            };

            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_events_before(
        &self,
        mission_id: Uuid,
        before_seq: i64,
        event_types: Option<&[&str]>,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Option<Vec<String>> =
            event_types.map(|t| t.iter().map(|s| s.to_string()).collect());
        let limit = limit.unwrap_or(50000) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // We want the N events immediately preceding `before_seq`,
            // but in chronological (ASC) order so the client can prepend
            // them without re-sorting. SQLite has no `ORDER BY DESC LIMIT
            // ... ORDER BY ASC` form, so do the DESC selection in a
            // subquery and reverse it in the outer query.
            let query = if types.is_some() {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM (
                   SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                   FROM mission_events
                   WHERE mission_id = ?1 AND sequence < ?2 AND event_type IN (SELECT value FROM json_each(?3))
                   ORDER BY sequence DESC
                   LIMIT ?4
                 )
                 ORDER BY sequence ASC"
            } else {
                "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                 FROM (
                   SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                   FROM mission_events
                   WHERE mission_id = ?1 AND sequence < ?2
                   ORDER BY sequence DESC
                   LIMIT ?3
                 )
                 ORDER BY sequence ASC"
            };

            fn parse_row(row: &rusqlite::Row<'_>) -> Result<StoredEvent, rusqlite::Error> {
                let content: Option<String> = row.get(8)?;
                let content_file: Option<String> = row.get(9)?;
                let full_content = SqliteMissionStore::load_content(content.as_deref(), content_file.as_deref());
                let metadata_str: String = row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "{}".to_string());
                let mid_str: String = row.get(1)?;

                Ok(StoredEvent {
                    id: row.get(0)?,
                    mission_id: parse_uuid_or_nil(&mid_str),
                    sequence: row.get(2)?,
                    event_type: row.get(3)?,
                    timestamp: row.get(4)?,
                    event_id: row.get(5)?,
                    tool_call_id: row.get(6)?,
                    tool_name: row.get(7)?,
                    content: full_content,
                    metadata: serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({})),
                })
            }

            let events: Vec<StoredEvent> = if let Some(types) = types {
                let types_json = serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid, before_seq, &types_json, limit], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            } else {
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid, before_seq, limit], parse_row)
                    .map_err(|e| e.to_string())?;
                let mut result = Vec::new();
                for row in rows {
                    result.push(row.map_err(|e| e.to_string())?);
                }
                result
            };

            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_events_for_tool_call(
        &self,
        mission_id: Uuid,
        tool_call_id: &str,
    ) -> Result<Vec<StoredEvent>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let tool_call_id = tool_call_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata
                     FROM mission_events
                     WHERE mission_id = ?1 AND tool_call_id = ?2
                     ORDER BY sequence ASC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![&mid, &tool_call_id], |row| {
                    let content: Option<String> = row.get(8)?;
                    let content_file: Option<String> = row.get(9)?;
                    let full_content = SqliteMissionStore::load_content(
                        content.as_deref(),
                        content_file.as_deref(),
                    );
                    let metadata_str: String = row
                        .get::<_, Option<String>>(10)?
                        .unwrap_or_else(|| "{}".to_string());
                    let mid_str: String = row.get(1)?;

                    Ok(StoredEvent {
                        id: row.get(0)?,
                        mission_id: parse_uuid_or_nil(&mid_str),
                        sequence: row.get(2)?,
                        event_type: row.get(3)?,
                        timestamp: row.get(4)?,
                        event_id: row.get(5)?,
                        tool_call_id: row.get(6)?,
                        tool_name: row.get(7)?,
                        content: full_content,
                        metadata: serde_json::from_str(&metadata_str)
                            .unwrap_or(serde_json::json!({})),
                    })
                })
                .map_err(|e| e.to_string())?;
            let mut events = Vec::new();
            for row in rows {
                events.push(row.map_err(|e| e.to_string())?);
            }
            Ok(events)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_recent_tool_call_ids(
        &self,
        mission_id: Uuid,
        limit: usize,
    ) -> Result<Vec<String>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let limit = limit as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT tool_call_id
                     FROM mission_events
                     WHERE mission_id = ?1
                       AND tool_call_id IS NOT NULL
                       AND event_type IN ('tool_call', 'tool_result')
                     GROUP BY tool_call_id
                     ORDER BY MAX(sequence) DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![&mid, limit], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(|e| e.to_string())?);
            }
            Ok(ids)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_tool_call_summaries(
        &self,
        mission_id: Uuid,
        tool_call_ids: &[String],
    ) -> Result<HashMap<String, ToolCallSummary>, String> {
        if tool_call_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let ids = tool_call_ids.to_vec();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let ids_json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
            let mut stmt = conn
                .prepare(
                    "SELECT tool_call_id, event_type, sequence, timestamp, content, content_file
                     FROM mission_events
                     WHERE mission_id = ?1
                       AND tool_call_id IN (SELECT value FROM json_each(?2))
                       AND event_type IN ('tool_call', 'tool_result')
                     ORDER BY sequence ASC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![&mid, &ids_json], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut summaries: HashMap<String, ToolCallSummary> = HashMap::new();
            for row in rows {
                let (tool_call_id, event_type, sequence, timestamp, content, content_file) =
                    row.map_err(|e| e.to_string())?;
                let full_content =
                    SqliteMissionStore::load_content(content.as_deref(), content_file.as_deref());
                let summary = summaries.entry(tool_call_id).or_default();
                if event_type == "tool_call" {
                    summary.call_content_bytes = full_content.len();
                } else {
                    summary.has_result = true;
                    summary.result_sequence = Some(sequence);
                    summary.result_timestamp = Some(timestamp);
                    summary.result_content_bytes = full_content.len();
                }
            }
            Ok(summaries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn max_event_sequence(&self, mission_id: Uuid) -> Result<i64, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let max_seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(sequence), 0) FROM mission_events WHERE mission_id = ?1",
                    params![&mid],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            Ok(max_seq)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn nth_recent_event_sequence(
        &self,
        mission_id: Uuid,
        anchor_types: &[&str],
        n: usize,
    ) -> Result<Option<i64>, String> {
        if n == 0 {
            return Ok(None);
        }
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Vec<String> = anchor_types.iter().map(|s| s.to_string()).collect();
        // OFFSET is zero-based: the Nth-most-recent row is at offset N-1.
        let offset = (n - 1) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let types_json = serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
            // Backed by idx_events_mission_type_sequence (mission_id,
            // event_type, sequence) — the DESC walk + small OFFSET is cheap.
            let seq: Option<i64> = conn
                .query_row(
                    "SELECT sequence FROM mission_events
                     WHERE mission_id = ?1 AND event_type IN (SELECT value FROM json_each(?2))
                     ORDER BY sequence DESC
                     LIMIT 1 OFFSET ?3",
                    params![&mid, &types_json, offset],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            Ok(seq)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn count_events(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
    ) -> Result<usize, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Option<Vec<String>> =
            event_types.map(|t| t.iter().map(|s| s.to_string()).collect());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            let count: i64 = if let Some(types) = types {
                let types_json =
                    serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
                conn.query_row(
                    "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1 AND event_type IN (SELECT value FROM json_each(?2))",
                    params![&mid, &types_json],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?
            } else {
                conn.query_row(
                    "SELECT COUNT(*) FROM mission_events WHERE mission_id = ?1",
                    params![&mid],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?
            };

            Ok(count as usize)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn count_events_by_type(
        &self,
        mission_id: Uuid,
        event_types: Option<&[&str]>,
    ) -> Result<HashMap<String, usize>, String> {
        let conn = self.conn.clone();
        let mid = mission_id.to_string();
        let types: Option<Vec<String>> =
            event_types.map(|t| t.iter().map(|s| s.to_string()).collect());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let query = if types.is_some() {
                "SELECT event_type, COUNT(*)
                 FROM mission_events
                 WHERE mission_id = ?1 AND event_type IN (SELECT value FROM json_each(?2))
                 GROUP BY event_type"
            } else {
                "SELECT event_type, COUNT(*)
                 FROM mission_events
                 WHERE mission_id = ?1
                 GROUP BY event_type"
            };

            let mut counts = HashMap::new();
            if let Some(types) = types {
                let types_json = serde_json::to_string(&types).unwrap_or_else(|_| "[]".to_string());
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid, &types_json], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    let (event_type, count) = row.map_err(|e| e.to_string())?;
                    counts.insert(event_type, count as usize);
                }
            } else {
                let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![&mid], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    let (event_type, count) = row.map_err(|e| e.to_string())?;
                    counts.insert(event_type, count as usize);
                }
            }
            Ok(counts)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_total_cost_cents(&self) -> Result<u64, String> {
        let conn = self.conn.lock().await;

        // Prefer normalized cost.amount_cents while remaining backward-compatible
        // with legacy flat cost_cents metadata. Clamp malformed negative values
        // to zero so aggregate cost invariants remain non-negative.
        let query = r#"
            WITH assistant_costs AS (
                SELECT CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) AS raw_cost
                FROM mission_events
                WHERE event_type = 'assistant_message'
            )
            SELECT COALESCE(
                SUM(CASE WHEN raw_cost > 0 THEN raw_cost ELSE 0 END),
                0
            ) as total_cost
            FROM assistant_costs
        "#;

        let total: i64 = conn
            .query_row(query, [], |row| row.get(0))
            .map_err(|e| e.to_string())?;

        u64::try_from(total).map_err(|_| format!("negative aggregate cost is invalid: {total}"))
    }

    async fn get_cost_by_source(&self) -> Result<(u64, u64, u64), String> {
        let conn = self.conn.lock().await;

        // Group costs by source provenance. Events may store cost in the
        // normalized shape (cost.amount_cents + cost.source) or in the legacy
        // flat shape (cost_cents, with no source — treated as unknown).
        let query = r#"
            WITH source_costs AS (
                SELECT
                    CAST(
                        COALESCE(
                            json_extract(metadata, '$.cost.amount_cents'),
                            json_extract(metadata, '$.cost_cents'),
                            0
                        ) AS INTEGER
                    ) AS raw_cost,
                    COALESCE(
                        json_extract(metadata, '$.cost.source'),
                        'unknown'
                    ) AS source
                FROM mission_events
                WHERE event_type = 'assistant_message'
            )
            SELECT
                source,
                COALESCE(SUM(CASE WHEN raw_cost > 0 THEN raw_cost ELSE 0 END), 0) AS total
            FROM source_costs
            GROUP BY source
        "#;

        let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                let source: String = row.get(0)?;
                let total: i64 = row.get(1)?;
                Ok((source, total.max(0) as u64))
            })
            .map_err(|e| e.to_string())?;

        let mut actual: u64 = 0;
        let mut estimated: u64 = 0;
        let mut unknown: u64 = 0;

        for row in rows {
            let (source, total) = row.map_err(|e| e.to_string())?;
            match source.as_str() {
                "actual" => actual = total,
                "estimated" => estimated = total,
                _ => unknown = unknown.saturating_add(total),
            }
        }

        Ok((actual, estimated, unknown))
    }

    async fn get_total_cost_cents_since(&self, since: &str) -> Result<u64, String> {
        let conn = self.conn.lock().await;
        let query = r#"
            WITH assistant_costs AS (
                SELECT CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) AS raw_cost
                FROM mission_events
                WHERE event_type = 'assistant_message'
                  AND timestamp >= ?1
            )
            SELECT COALESCE(
                SUM(CASE WHEN raw_cost > 0 THEN raw_cost ELSE 0 END),
                0
            ) as total_cost
            FROM assistant_costs
        "#;
        let total: i64 = conn
            .query_row(query, [since], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        u64::try_from(total).map_err(|_| format!("negative aggregate cost is invalid: {total}"))
    }

    async fn get_cost_by_source_since(&self, since: &str) -> Result<(u64, u64, u64), String> {
        let conn = self.conn.lock().await;
        let query = r#"
            WITH source_costs AS (
                SELECT
                    CAST(
                        COALESCE(
                            json_extract(metadata, '$.cost.amount_cents'),
                            json_extract(metadata, '$.cost_cents'),
                            0
                        ) AS INTEGER
                    ) AS raw_cost,
                    COALESCE(
                        json_extract(metadata, '$.cost.source'),
                        'unknown'
                    ) AS source
                FROM mission_events
                WHERE event_type = 'assistant_message'
                  AND timestamp >= ?1
            )
            SELECT
                source,
                COALESCE(SUM(CASE WHEN raw_cost > 0 THEN raw_cost ELSE 0 END), 0) AS total
            FROM source_costs
            GROUP BY source
        "#;
        let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([since], |row| {
                let source: String = row.get(0)?;
                let total: i64 = row.get(1)?;
                Ok((source, total.max(0) as u64))
            })
            .map_err(|e| e.to_string())?;

        let mut actual: u64 = 0;
        let mut estimated: u64 = 0;
        let mut unknown: u64 = 0;
        for row in rows {
            let (source, total) = row.map_err(|e| e.to_string())?;
            match source.as_str() {
                "actual" => actual = total,
                "estimated" => estimated = total,
                _ => unknown = unknown.saturating_add(total),
            }
        }
        Ok((actual, estimated, unknown))
    }

    async fn get_usage_by_model(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<ModelUsageStats>, String> {
        let conn = self.conn.lock().await;

        // Read individual rows so stale stored `model_normalized` values can be
        // corrected with the current normalizer and estimated costs can be
        // recalculated under the corrected model.
        let base_query = r#"
            SELECT
                COALESCE(json_extract(metadata, '$.model'), '') AS raw_model,
                COALESCE(json_extract(metadata, '$.model_normalized'), '') AS stored_model,
                COALESCE(CAST(json_extract(metadata, '$.usage.input_tokens') AS INTEGER), 0) AS input_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.output_tokens') AS INTEGER), 0) AS output_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_creation_input_tokens') AS INTEGER), 0) AS cache_creation_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_read_input_tokens') AS INTEGER), 0) AS cache_read_tokens,
                COALESCE(
                    json_extract(metadata, '$.cost.source'),
                    ''
                ) AS cost_source,
                CASE WHEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) > 0
                THEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) ELSE 0 END AS cost_cents
            FROM mission_events
            WHERE event_type = 'assistant_message'
        "#;

        let sql = match since {
            Some(_) => format!("{base_query} AND timestamp >= ?1"),
            None => base_query.to_string(),
        };

        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                let raw_model: String = row.get(0)?;
                let stored_model: String = row.get(1)?;
                let input_tokens: i64 = row.get(2)?;
                let output_tokens: i64 = row.get(3)?;
                let cache_creation_tokens: i64 = row.get(4)?;
                let cache_read_tokens: i64 = row.get(5)?;
                let cost_source: String = row.get(6)?;
                let cost_cents: i64 = row.get(7)?;
                let model = usage_model_key(&raw_model, &stored_model);
                let input_tokens = input_tokens.max(0) as u64;
                let output_tokens = output_tokens.max(0) as u64;
                let cache_creation_tokens = cache_creation_tokens.max(0) as u64;
                let cache_read_tokens = cache_read_tokens.max(0) as u64;
                let stored_cost_cents = cost_cents.max(0) as u64;
                let cost_cents = usage_cost_with_read_side_estimate(
                    &model,
                    stored_cost_cents,
                    &cost_source,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                );
                Ok(ModelUsageStats {
                    model,
                    requests: 1,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                    cost_cents,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut by_model: BTreeMap<String, ModelUsageStats> = BTreeMap::new();
        for r in rows {
            let row = r.map_err(|e| e.to_string())?;
            // Skip empty-model rows that carry no usage signal at all
            if row.model.is_empty()
                && row.input_tokens == 0
                && row.output_tokens == 0
                && row.cache_creation_tokens == 0
                && row.cache_read_tokens == 0
                && row.cost_cents == 0
            {
                continue;
            }
            let entry = by_model
                .entry(row.model.clone())
                .or_insert_with(|| ModelUsageStats {
                    model: row.model.clone(),
                    requests: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    cost_cents: 0,
                });
            entry.requests = entry.requests.saturating_add(row.requests);
            entry.input_tokens = entry.input_tokens.saturating_add(row.input_tokens);
            entry.output_tokens = entry.output_tokens.saturating_add(row.output_tokens);
            entry.cache_creation_tokens = entry
                .cache_creation_tokens
                .saturating_add(row.cache_creation_tokens);
            entry.cache_read_tokens = entry
                .cache_read_tokens
                .saturating_add(row.cache_read_tokens);
            entry.cost_cents = entry.cost_cents.saturating_add(row.cost_cents);
        }
        let mut out: Vec<ModelUsageStats> = by_model.into_values().collect();
        out.sort_by(|a, b| {
            b.cost_cents
                .cmp(&a.cost_cents)
                .then_with(|| b.requests.cmp(&a.requests))
                .then_with(|| a.model.cmp(&b.model))
        });
        Ok(out)
    }

    async fn get_usage_by_day(&self, since: Option<&str>) -> Result<Vec<DailyUsageStats>, String> {
        let conn = self.conn.lock().await;

        // Group by the UTC date prefix of the ISO-8601 timestamp.
        // We rely on timestamps being stored in RFC3339 / ISO-8601 form, which
        // is what `now_string()` and SQLite's CURRENT_TIMESTAMP produce.
        let base_query = r#"
            SELECT
                substr(timestamp, 1, 10) AS day,
                COALESCE(json_extract(metadata, '$.model'), '') AS raw_model,
                COALESCE(json_extract(metadata, '$.model_normalized'), '') AS stored_model,
                COALESCE(CAST(json_extract(metadata, '$.usage.input_tokens') AS INTEGER), 0) AS input_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.output_tokens') AS INTEGER), 0) AS output_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_creation_input_tokens') AS INTEGER), 0) AS cache_creation_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_read_input_tokens') AS INTEGER), 0) AS cache_read_tokens,
                COALESCE(json_extract(metadata, '$.cost.source'), '') AS cost_source,
                CASE WHEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) > 0
                THEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) ELSE 0 END AS cost_cents
            FROM mission_events
            WHERE event_type = 'assistant_message'
        "#;

        let sql = match since {
            Some(_) => format!("{base_query} AND timestamp >= ?1 ORDER BY day ASC"),
            None => format!("{base_query} ORDER BY day ASC"),
        };

        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                let day: String = row.get(0)?;
                let raw_model: String = row.get(1)?;
                let stored_model: String = row.get(2)?;
                let input_tokens: i64 = row.get(3)?;
                let output_tokens: i64 = row.get(4)?;
                let cache_creation_tokens: i64 = row.get(5)?;
                let cache_read_tokens: i64 = row.get(6)?;
                let cost_source: String = row.get(7)?;
                let cost_cents: i64 = row.get(8)?;
                let model = usage_model_key(&raw_model, &stored_model);
                let input_tokens = input_tokens.max(0) as u64;
                let output_tokens = output_tokens.max(0) as u64;
                let cache_creation_tokens = cache_creation_tokens.max(0) as u64;
                let cache_read_tokens = cache_read_tokens.max(0) as u64;
                let cost_cents = usage_cost_with_read_side_estimate(
                    &model,
                    cost_cents.max(0) as u64,
                    &cost_source,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                );
                Ok((
                    day,
                    1,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cost_cents,
                ))
            })
            .map_err(|e| e.to_string())?;

        let mut by_day: BTreeMap<String, DailyUsageStats> = BTreeMap::new();
        for r in rows {
            let (day, requests, input_tokens, output_tokens, cache_read_tokens, cost_cents) =
                r.map_err(|e| e.to_string())?;
            if day.is_empty() {
                continue;
            }
            let entry = by_day.entry(day.clone()).or_insert(DailyUsageStats {
                day,
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cost_cents: 0,
            });
            entry.requests = entry.requests.saturating_add(requests);
            entry.input_tokens = entry.input_tokens.saturating_add(input_tokens);
            entry.output_tokens = entry.output_tokens.saturating_add(output_tokens);
            entry.cache_read_tokens = entry.cache_read_tokens.saturating_add(cache_read_tokens);
            entry.cost_cents = entry.cost_cents.saturating_add(cost_cents);
        }
        Ok(by_day.into_values().collect())
    }

    async fn get_usage_by_hour(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<HourlyUsageStats>, String> {
        let conn = self.conn.lock().await;

        // Bucket by `YYYY-MM-DDTHH` (the first 13 chars of an RFC3339 stamp).
        // Same cost-source fallback as get_usage_by_day.
        let base_query = r#"
            SELECT
                substr(timestamp, 1, 13) AS hour,
                COALESCE(json_extract(metadata, '$.model'), '') AS raw_model,
                COALESCE(json_extract(metadata, '$.model_normalized'), '') AS stored_model,
                COALESCE(CAST(json_extract(metadata, '$.usage.input_tokens') AS INTEGER), 0) AS input_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.output_tokens') AS INTEGER), 0) AS output_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_creation_input_tokens') AS INTEGER), 0) AS cache_creation_tokens,
                COALESCE(CAST(json_extract(metadata, '$.usage.cache_read_input_tokens') AS INTEGER), 0) AS cache_read_tokens,
                COALESCE(json_extract(metadata, '$.cost.source'), '') AS cost_source,
                CASE WHEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) > 0
                THEN CAST(
                    COALESCE(
                        json_extract(metadata, '$.cost.amount_cents'),
                        json_extract(metadata, '$.cost_cents'),
                        0
                    ) AS INTEGER
                ) ELSE 0 END AS cost_cents
            FROM mission_events
            WHERE event_type = 'assistant_message'
        "#;
        let sql = match since {
            Some(_) => format!("{base_query} AND timestamp >= ?1 ORDER BY hour ASC"),
            None => format!("{base_query} ORDER BY hour ASC"),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                let hour: String = row.get(0)?;
                let raw_model: String = row.get(1)?;
                let stored_model: String = row.get(2)?;
                let input_tokens: i64 = row.get(3)?;
                let output_tokens: i64 = row.get(4)?;
                let cache_creation_tokens: i64 = row.get(5)?;
                let cache_read_tokens: i64 = row.get(6)?;
                let cost_source: String = row.get(7)?;
                let cost_cents: i64 = row.get(8)?;
                let model = usage_model_key(&raw_model, &stored_model);
                let input_tokens = input_tokens.max(0) as u64;
                let output_tokens = output_tokens.max(0) as u64;
                let cache_creation_tokens = cache_creation_tokens.max(0) as u64;
                let cache_read_tokens = cache_read_tokens.max(0) as u64;
                let cost_cents = usage_cost_with_read_side_estimate(
                    &model,
                    cost_cents.max(0) as u64,
                    &cost_source,
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                );
                Ok((
                    hour,
                    1,
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cost_cents,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut by_hour: BTreeMap<String, HourlyUsageStats> = BTreeMap::new();
        for r in rows {
            let (hour, requests, input_tokens, output_tokens, cache_read_tokens, cost_cents) =
                r.map_err(|e| e.to_string())?;
            if hour.is_empty() {
                continue;
            }
            let entry = by_hour.entry(hour.clone()).or_insert(HourlyUsageStats {
                hour,
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cost_cents: 0,
            });
            entry.requests = entry.requests.saturating_add(requests);
            entry.input_tokens = entry.input_tokens.saturating_add(input_tokens);
            entry.output_tokens = entry.output_tokens.saturating_add(output_tokens);
            entry.cache_read_tokens = entry.cache_read_tokens.saturating_add(cache_read_tokens);
            entry.cost_cents = entry.cost_cents.saturating_add(cost_cents);
        }
        Ok(by_hour.into_values().collect())
    }

    async fn record_proxy_usage(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO proxy_usage (timestamp, model, input_tokens, output_tokens, cost_cents)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                chrono::Utc::now().to_rfc3339(),
                model,
                input_tokens as i64,
                output_tokens as i64,
                cost_cents as i64,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn get_proxy_usage_by_model(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<ModelUsageStats>, String> {
        let conn = self.conn.lock().await;
        // Models are normalized and costs finalized at write time, so plain
        // SQL aggregation is enough (unlike mission_events, which re-derives
        // both from stored metadata on read).
        let base_query = r#"
            SELECT
                model,
                COUNT(*) AS requests,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_creation_tokens) AS cache_creation_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cost_cents) AS cost_cents
            FROM proxy_usage
        "#;
        let sql = match since {
            Some(_) => format!("{base_query} WHERE timestamp >= ?1 GROUP BY model"),
            None => format!("{base_query} GROUP BY model"),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(ModelUsageStats {
                    model: row.get(0)?,
                    requests: row.get::<_, i64>(1)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(2)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    cache_creation_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                    cost_cents: row.get::<_, i64>(6)?.max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    async fn get_proxy_usage_by_day(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<DailyUsageStats>, String> {
        let conn = self.conn.lock().await;
        let base_query = r#"
            SELECT
                substr(timestamp, 1, 10) AS day,
                COUNT(*) AS requests,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cost_cents) AS cost_cents
            FROM proxy_usage
        "#;
        let sql = match since {
            Some(_) => format!("{base_query} WHERE timestamp >= ?1 GROUP BY day ORDER BY day ASC"),
            None => format!("{base_query} GROUP BY day ORDER BY day ASC"),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(DailyUsageStats {
                    day: row.get(0)?,
                    requests: row.get::<_, i64>(1)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(2)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cost_cents: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    async fn get_proxy_usage_by_hour(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<HourlyUsageStats>, String> {
        let conn = self.conn.lock().await;
        let base_query = r#"
            SELECT
                substr(timestamp, 1, 13) AS hour,
                COUNT(*) AS requests,
                SUM(input_tokens) AS input_tokens,
                SUM(output_tokens) AS output_tokens,
                SUM(cache_read_tokens) AS cache_read_tokens,
                SUM(cost_cents) AS cost_cents
            FROM proxy_usage
        "#;
        let sql = match since {
            Some(_) => {
                format!("{base_query} WHERE timestamp >= ?1 GROUP BY hour ORDER BY hour ASC")
            }
            None => format!("{base_query} GROUP BY hour ORDER BY hour ASC"),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let since_owned = since.map(|s| s.to_string());
        let params: Vec<&dyn rusqlite::ToSql> = match since_owned.as_ref() {
            Some(s) => vec![s],
            None => vec![],
        };
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(HourlyUsageStats {
                    hour: row.get(0)?,
                    requests: row.get::<_, i64>(1)?.max(0) as u64,
                    input_tokens: row.get::<_, i64>(2)?.max(0) as u64,
                    output_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    cache_read_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                    cost_cents: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    async fn create_automation(&self, automation: Automation) -> Result<Automation, String> {
        let conn = self.conn.clone();

        // Serialize command source
        let (command_source_type, command_source_data) = match &automation.command_source {
            CommandSource::Library { name } => {
                ("library", serde_json::json!({ "name": name }).to_string())
            }
            CommandSource::LocalFile { path } => (
                "local_file",
                serde_json::json!({ "path": path }).to_string(),
            ),
            CommandSource::Inline { content } => (
                "inline",
                serde_json::json!({ "content": content }).to_string(),
            ),
            CommandSource::NativeLoop {
                harness,
                command,
                args,
            } => (
                "native_loop",
                serde_json::json!({
                    "harness": harness,
                    "command": command,
                    "args": args,
                })
                .to_string(),
            ),
        };

        // Serialize trigger
        let (trigger_type, trigger_data) = match &automation.trigger {
            TriggerType::Interval { seconds } => (
                "interval",
                serde_json::json!({ "seconds": seconds }).to_string(),
            ),
            TriggerType::Cron {
                expression,
                timezone,
            } => (
                "cron",
                serde_json::json!({ "expression": expression, "timezone": timezone }).to_string(),
            ),
            TriggerType::Webhook { config } => (
                "webhook",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
            TriggerType::AgentFinished => ("agent_finished", "{}".to_string()),
            TriggerType::Telegram { config } => (
                "telegram",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
        };

        // Serialize variables
        let variables_json =
            serde_json::to_string(&automation.variables).map_err(|e| e.to_string())?;

        let a = automation.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let stop_policy_str = match &a.stop_policy {
                StopPolicy::Never => "never".to_string(),
                StopPolicy::WhenFailingConsecutively { count } => format!("consecutive_failures:{}", count),
                StopPolicy::WhenAllIssuesClosedAndPRsMerged { repo } => format!("all_issues_closed_and_prs_merged:{}", repo),
                StopPolicy::AfterFirstFire => "after_first_fire".to_string(),
            };
            let fresh_session_str = match a.fresh_session {
                FreshSession::Always => "always",
                FreshSession::Switch => "switch",
                FreshSession::Keep => "keep",
            };
            let driver_str = match a.driver {
                super::AutomationDriver::Scheduler => "scheduler",
                super::AutomationDriver::HarnessLoop => "harness_loop",
            };
            conn.execute(
                "INSERT INTO automations (id, mission_id, command_source_type, command_source_data,
                                         trigger_type, trigger_data, variables, active, stop_policy,
                                         fresh_session, driver, created_at, last_triggered_at, retry_max_retries,
                                         retry_delay_seconds, retry_backoff_multiplier)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    a.id.to_string(),
                    a.mission_id.to_string(),
                    command_source_type,
                    command_source_data,
                    trigger_type,
                    trigger_data,
                    variables_json,
                    if a.active { 1 } else { 0 },
                    stop_policy_str,
                    fresh_session_str,
                    driver_str,
                    a.created_at,
                    a.last_triggered_at,
                    a.retry_config.max_retries as i64,
                    a.retry_config.retry_delay_seconds as i64,
                    a.retry_config.backoff_multiplier,
                ],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())?;

        Ok(automation)
    }

    async fn get_mission_automations(&self, mission_id: Uuid) -> Result<Vec<Automation>, String> {
        let conn = self.conn.clone();
        let mission_id_str = mission_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare("SELECT id, mission_id, command_source_type, command_source_data,
                                trigger_type, trigger_data, variables, active, stop_policy, fresh_session, created_at, last_triggered_at,
                                retry_max_retries, retry_delay_seconds, retry_backoff_multiplier, driver
                         FROM automations WHERE mission_id = ? ORDER BY created_at DESC")
                .map_err(|e| e.to_string())?;

            let automations = stmt
                .query_map([mission_id_str], |row| {
                    Self::parse_automation_row(row)
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(automations)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn list_active_automations(&self) -> Result<Vec<Automation>, String> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, command_source_type, command_source_data,
                            trigger_type, trigger_data, variables, active, stop_policy, fresh_session, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier, driver
                     FROM automations WHERE active = 1 ORDER BY created_at DESC",
                )
                .map_err(|e| e.to_string())?;

            let automations = stmt
                .query_map([], |row| {
                    Self::parse_automation_row(row)
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(automations)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn get_automation(&self, id: Uuid) -> Result<Option<Automation>, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let result = conn
                .query_row(
                    "SELECT id, mission_id, command_source_type, command_source_data,
                            trigger_type, trigger_data, variables, active, stop_policy, fresh_session, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier, driver
                     FROM automations WHERE id = ?",
                    [id_str],
                    Self::parse_automation_row,
                )
                .optional()
                .map_err(|e| e.to_string())?;

            Ok(result)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn update_automation_active(&self, id: Uuid, active: bool) -> Result<(), String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE automations SET active = ? WHERE id = ?",
                params![if active { 1 } else { 0 }, id_str],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())
    }

    async fn update_automation_last_triggered(&self, id: Uuid) -> Result<(), String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let now = now_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE automations SET last_triggered_at = ? WHERE id = ?",
                params![now, id_str],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())
    }

    async fn delete_automation(&self, id: Uuid) -> Result<bool, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let rows = conn
                .execute("DELETE FROM automations WHERE id = ?", params![id_str])
                .map_err(|e| e.to_string())?;
            Ok(rows > 0)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn update_automation(&self, automation: Automation) -> Result<(), String> {
        let conn = self.conn.clone();

        // Serialize command source
        let (command_source_type, command_source_data) = match &automation.command_source {
            CommandSource::Library { name } => {
                ("library", serde_json::json!({ "name": name }).to_string())
            }
            CommandSource::LocalFile { path } => (
                "local_file",
                serde_json::json!({ "path": path }).to_string(),
            ),
            CommandSource::Inline { content } => (
                "inline",
                serde_json::json!({ "content": content }).to_string(),
            ),
            CommandSource::NativeLoop {
                harness,
                command,
                args,
            } => (
                "native_loop",
                serde_json::json!({
                    "harness": harness,
                    "command": command,
                    "args": args,
                })
                .to_string(),
            ),
        };

        // Serialize trigger
        let (trigger_type, trigger_data) = match &automation.trigger {
            TriggerType::Interval { seconds } => (
                "interval",
                serde_json::json!({ "seconds": seconds }).to_string(),
            ),
            TriggerType::Cron {
                expression,
                timezone,
            } => (
                "cron",
                serde_json::json!({ "expression": expression, "timezone": timezone }).to_string(),
            ),
            TriggerType::Webhook { config } => (
                "webhook",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
            TriggerType::AgentFinished => ("agent_finished", "{}".to_string()),
            TriggerType::Telegram { config } => (
                "telegram",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
        };

        // Serialize variables
        let variables_json =
            serde_json::to_string(&automation.variables).map_err(|e| e.to_string())?;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let stop_policy_str = match &automation.stop_policy {
                StopPolicy::Never => "never".to_string(),
                StopPolicy::WhenFailingConsecutively { count } => format!("consecutive_failures:{}", count),
                StopPolicy::WhenAllIssuesClosedAndPRsMerged { repo } => format!("all_issues_closed_and_prs_merged:{}", repo),
                StopPolicy::AfterFirstFire => "after_first_fire".to_string(),
            };
            let fresh_session_str = match automation.fresh_session {
                FreshSession::Always => "always",
                FreshSession::Switch => "switch",
                FreshSession::Keep => "keep",
            };
            conn.execute(
                "UPDATE automations SET command_source_type = ?, command_source_data = ?,
                                       trigger_type = ?, trigger_data = ?, variables = ?, active = ?,
                                       stop_policy = ?, fresh_session = ?, last_triggered_at = ?, retry_max_retries = ?, retry_delay_seconds = ?,
                                       retry_backoff_multiplier = ?
                  WHERE id = ?",
                params![
                    command_source_type,
                    command_source_data,
                    trigger_type,
                    trigger_data,
                    variables_json,
                    if automation.active { 1 } else { 0 },
                    stop_policy_str,
                    fresh_session_str,
                    automation.last_triggered_at,
                    automation.retry_config.max_retries as i64,
                    automation.retry_config.retry_delay_seconds as i64,
                    automation.retry_config.backoff_multiplier,
                    automation.id.to_string(),
                ],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())
    }

    async fn get_automation_by_webhook_id(
        &self,
        webhook_id: &str,
    ) -> Result<Option<Automation>, String> {
        let conn = self.conn.clone();
        let webhook_id = webhook_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let result = conn
                .query_row(
                    "SELECT id, mission_id, command_source_type, command_source_data,
                            trigger_type, trigger_data, variables, active, stop_policy, fresh_session, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier, driver
                     FROM automations
                     WHERE trigger_type = 'webhook' AND json_extract(trigger_data, '$.webhook_id') = ?",
                    [webhook_id],
                    Self::parse_automation_row,
                )
                .optional()
                .map_err(|e| e.to_string())?;

            Ok(result)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn create_automation_execution(
        &self,
        execution: AutomationExecution,
    ) -> Result<AutomationExecution, String> {
        let conn = self.conn.clone();

        let webhook_payload_json = execution
            .webhook_payload
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let variables_used_json =
            serde_json::to_string(&execution.variables_used).unwrap_or_else(|_| "{}".to_string());

        let status_str = match execution.status {
            ExecutionStatus::Pending => "pending",
            ExecutionStatus::Running => "running",
            ExecutionStatus::Success => "success",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Cancelled => "cancelled",
            ExecutionStatus::Skipped => "skipped",
        };

        let exec = execution.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO automation_executions (id, automation_id, mission_id, triggered_at,
                                                    trigger_source, status, webhook_payload, variables_used,
                                                    completed_at, error, retry_count)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    exec.id.to_string(),
                    exec.automation_id.to_string(),
                    exec.mission_id.to_string(),
                    exec.triggered_at,
                    exec.trigger_source,
                    status_str,
                    webhook_payload_json,
                    variables_used_json,
                    exec.completed_at,
                    exec.error,
                    exec.retry_count as i64,
                ],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())?;

        Ok(execution)
    }

    async fn update_automation_execution(
        &self,
        execution: AutomationExecution,
    ) -> Result<(), String> {
        let conn = self.conn.clone();

        let webhook_payload_json = execution
            .webhook_payload
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));

        let variables_used_json =
            serde_json::to_string(&execution.variables_used).unwrap_or_else(|_| "{}".to_string());

        let status_str = match execution.status {
            ExecutionStatus::Pending => "pending",
            ExecutionStatus::Running => "running",
            ExecutionStatus::Success => "success",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Cancelled => "cancelled",
            ExecutionStatus::Skipped => "skipped",
        };

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE automation_executions SET status = ?, webhook_payload = ?, variables_used = ?,
                                                 completed_at = ?, error = ?, retry_count = ?
                 WHERE id = ?",
                params![
                    status_str,
                    webhook_payload_json,
                    variables_used_json,
                    execution.completed_at,
                    execution.error,
                    execution.retry_count as i64,
                    execution.id.to_string(),
                ],
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
        .map_err(|e| e.to_string())
    }

    async fn get_automation_executions(
        &self,
        automation_id: Uuid,
        limit: Option<usize>,
    ) -> Result<Vec<AutomationExecution>, String> {
        let conn = self.conn.clone();
        let automation_id_str = automation_id.to_string();
        let limit = limit.unwrap_or(100) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, automation_id, mission_id, triggered_at, trigger_source, status,
                            webhook_payload, variables_used, completed_at, error, retry_count
                     FROM automation_executions
                     WHERE automation_id = ?
                     ORDER BY triggered_at DESC
                     LIMIT ?",
                )
                .map_err(|e| e.to_string())?;

            let executions = stmt
                .query_map(params![automation_id_str, limit], |row| {
                    Self::parse_execution_row(row)
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(executions)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn get_mission_automation_executions(
        &self,
        mission_id: Uuid,
        limit: Option<usize>,
    ) -> Result<Vec<AutomationExecution>, String> {
        let conn = self.conn.clone();
        let mission_id_str = mission_id.to_string();
        let limit = limit.unwrap_or(100) as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, automation_id, mission_id, triggered_at, trigger_source, status,
                            webhook_payload, variables_used, completed_at, error, retry_count
                     FROM automation_executions
                     WHERE mission_id = ?
                     ORDER BY triggered_at DESC
                     LIMIT ?",
                )
                .map_err(|e| e.to_string())?;

            let executions = stmt
                .query_map(params![mission_id_str, limit], |row| {
                    Self::parse_execution_row(row)
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            Ok(executions)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn complete_running_executions_for_mission(
        &self,
        mission_id: Uuid,
        success: bool,
        error: Option<String>,
    ) -> Result<u32, String> {
        let conn = self.conn.clone();
        let mission_id_str = mission_id.to_string();
        let new_status = if success { "success" } else { "failed" };
        let completed_at = now_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let updated = conn
                .execute(
                    "UPDATE automation_executions
                     SET status = ?, completed_at = ?, error = ?
                     WHERE mission_id = ? AND status IN ('running', 'pending')",
                    params![new_status, completed_at, error, mission_id_str],
                )
                .map_err(|e| e.to_string())?;
            Ok(updated as u32)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn update_mission_mode(&self, id: Uuid, mode: MissionMode) -> Result<(), String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let mode_str = serde_json::to_value(&mode)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "task".to_string());
        let now = now_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET mission_mode = ?1, updated_at = ?2 WHERE id = ?3",
                params![mode_str, now, id_str],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn list_assistant_missions(&self) -> Result<Vec<Mission>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, workspace_id, agent, backend, created_at, updated_at, mission_mode, short_description, COALESCE(goal_mode, 0) as goal_mode, goal_objective FROM missions WHERE mission_mode = 'assistant' ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let missions = stmt
                .query_map([], |row| {
                    let id_str: String = row.get(0).unwrap_or_default();
                    let status_str: String = row.get(1).unwrap_or_else(|_| "pending".to_string());
                    let workspace_id_str: String = row.get(3).unwrap_or_default();
                    let mode_str: String = row.get(8).unwrap_or_else(|_| "task".to_string());
                    Ok(Mission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        status: serde_json::from_value(serde_json::Value::String(status_str))
                            .unwrap_or(MissionStatus::Pending),
                        title: row.get(2).unwrap_or_default(),
                        short_description: row.get(9).unwrap_or_default(),
                        metadata_updated_at: None,
                        metadata_source: None,
                        metadata_model: None,
                        metadata_version: None,
                        workspace_id: Uuid::parse_str(&workspace_id_str).unwrap_or_default(),
                        workspace_name: None,
                        agent: row.get(4).unwrap_or_default(),
                        model_override: None,
                        model_effort: None,
                        backend: row.get(5).unwrap_or_else(|_| "claudecode".to_string()),
                        config_profile: None,
                        history: vec![],
                        created_at: row.get(6).unwrap_or_default(),
                        updated_at: row.get(7).unwrap_or_default(),
                        interrupted_at: None,
                        paused_at: None,
                        resumable: false,
                        desktop_sessions: vec![],
                        session_id: None,
                        terminal_reason: None,
                        parent_mission_id: None,
                        working_directory: None,
                        mission_mode: serde_json::from_value(serde_json::Value::String(mode_str))
                            .unwrap_or_default(),
                        goal_mode: false,
                        goal_objective: None,
                        first_viewed_at: None,
                        scheduling: Default::default(),
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(missions)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    // === Telegram Channel methods ===

    async fn create_telegram_channel(
        &self,
        channel: TelegramChannel,
    ) -> Result<TelegramChannel, String> {
        let conn = self.conn.clone();
        let c = channel.clone();
        let allowed_chat_ids_json =
            serde_json::to_string(&c.allowed_chat_ids).unwrap_or_else(|_| "[]".to_string());
        let trigger_mode_str = serde_json::to_value(&c.trigger_mode)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "direct_message".to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_channels (id, mission_id, bot_token, bot_username, allowed_chat_ids, trigger_mode, active, webhook_secret, instructions, created_at, updated_at, auto_create_missions, default_backend, default_model_override, default_model_effort, default_workspace_id, default_config_profile, default_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    c.id.to_string(),
                    c.mission_id.to_string(),
                    c.bot_token,
                    c.bot_username,
                    allowed_chat_ids_json,
                    trigger_mode_str,
                    c.active as i32,
                    c.webhook_secret,
                    c.instructions,
                    c.created_at,
                    c.updated_at,
                    c.auto_create_missions as i32,
                    c.default_backend,
                    c.default_model_override,
                    c.default_model_effort,
                    c.default_workspace_id.map(|u| u.to_string()),
                    c.default_config_profile,
                    c.default_agent,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;

        Ok(channel)
    }

    async fn get_telegram_channel(&self, id: Uuid) -> Result<Option<TelegramChannel>, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT id, mission_id, bot_token, bot_username, allowed_chat_ids, trigger_mode, active, webhook_secret, instructions, created_at, updated_at, auto_create_missions, default_backend, default_model_override, default_model_effort, default_workspace_id, default_config_profile, default_agent
                 FROM telegram_channels WHERE id = ?1",
                params![id_str],
                |row| Ok(row_to_telegram_channel(row)),
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_channels(
        &self,
        mission_id: Uuid,
    ) -> Result<Vec<TelegramChannel>, String> {
        let conn = self.conn.clone();
        let mission_id_str = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, bot_token, bot_username, allowed_chat_ids, trigger_mode, active, webhook_secret, instructions, created_at, updated_at, auto_create_missions, default_backend, default_model_override, default_model_effort, default_workspace_id, default_config_profile, default_agent
                     FROM telegram_channels WHERE mission_id = ?1 ORDER BY created_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let channels = stmt
                .query_map(params![mission_id_str], |row| Ok(row_to_telegram_channel(row)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(channels)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_all_active_telegram_channels(&self) -> Result<Vec<TelegramChannel>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, bot_token, bot_username, allowed_chat_ids, trigger_mode, active, webhook_secret, instructions, created_at, updated_at, auto_create_missions, default_backend, default_model_override, default_model_effort, default_workspace_id, default_config_profile, default_agent
                     FROM telegram_channels WHERE active = 1",
                )
                .map_err(|e| e.to_string())?;
            let channels = stmt
                .query_map([], |row| Ok(row_to_telegram_channel(row)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(channels)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_telegram_channel(&self, channel: TelegramChannel) -> Result<(), String> {
        let conn = self.conn.clone();
        let allowed_chat_ids_json =
            serde_json::to_string(&channel.allowed_chat_ids).unwrap_or_else(|_| "[]".to_string());
        let trigger_mode_str = serde_json::to_value(&channel.trigger_mode)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "direct_message".to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_channels SET bot_token = ?1, bot_username = ?2, allowed_chat_ids = ?3, trigger_mode = ?4, active = ?5, webhook_secret = ?6, instructions = ?7, updated_at = ?8, auto_create_missions = ?10, default_backend = ?11, default_model_override = ?12, default_model_effort = ?13, default_workspace_id = ?14, default_config_profile = ?15, default_agent = ?16 WHERE id = ?9",
                params![
                    channel.bot_token,
                    channel.bot_username,
                    allowed_chat_ids_json,
                    trigger_mode_str,
                    channel.active as i32,
                    channel.webhook_secret,
                    channel.instructions,
                    channel.updated_at,
                    channel.id.to_string(),
                    channel.auto_create_missions as i32,
                    channel.default_backend,
                    channel.default_model_override,
                    channel.default_model_effort,
                    channel.default_workspace_id.map(|u| u.to_string()),
                    channel.default_config_profile,
                    channel.default_agent,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn delete_telegram_channel(&self, id: Uuid) -> Result<bool, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let deleted = conn
                .execute(
                    "DELETE FROM telegram_channels WHERE id = ?1",
                    params![id_str],
                )
                .map_err(|e| e.to_string())?;
            Ok(deleted > 0)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_all_telegram_channels(&self) -> Result<Vec<TelegramChannel>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, bot_token, bot_username, allowed_chat_ids, trigger_mode, active, webhook_secret, instructions, created_at, updated_at, auto_create_missions, default_backend, default_model_override, default_model_effort, default_workspace_id, default_config_profile, default_agent
                     FROM telegram_channels ORDER BY created_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let channels = stmt
                .query_map([], |row| Ok(row_to_telegram_channel(row)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(channels)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_telegram_user(&self, user: TelegramUser) -> Result<TelegramUser, String> {
        let conn = self.conn.clone();
        let u = user.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_users
                 (id, telegram_user_id, username, display_name, role, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(telegram_user_id) DO UPDATE SET
                    username = excluded.username,
                    display_name = excluded.display_name,
                    role = excluded.role,
                    updated_at = excluded.updated_at",
                params![
                    u.id.to_string(),
                    u.telegram_user_id,
                    u.username,
                    u.display_name,
                    telegram_user_role_to_str(u.role),
                    u.created_at,
                    u.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(user)
    }

    async fn get_telegram_user(
        &self,
        telegram_user_id: i64,
    ) -> Result<Option<TelegramUser>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT id, telegram_user_id, username, display_name, role, created_at, updated_at
                 FROM telegram_users WHERE telegram_user_id = ?1",
                params![telegram_user_id],
                row_to_telegram_user,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_or_create_telegram_user_cursor(
        &self,
        telegram_user_id: i64,
    ) -> Result<TelegramUserCursor, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            if let Some(cursor) = conn
                .query_row(
                    "SELECT id, telegram_user_id, last_status_at, last_dashboard_seen_at,
                            last_alert_ack_at, last_digest_at,
                            last_seen_event_sequence_by_mission_json, created_at, updated_at
                     FROM telegram_user_cursors WHERE telegram_user_id = ?1",
                    params![telegram_user_id],
                    row_to_telegram_user_cursor,
                )
                .optional()
                .map_err(|e| e.to_string())?
            {
                return Ok(cursor);
            }

            let now = now_string();
            let cursor = TelegramUserCursor {
                id: Uuid::new_v4(),
                telegram_user_id,
                last_status_at: None,
                last_dashboard_seen_at: None,
                last_alert_ack_at: None,
                last_digest_at: None,
                last_seen_event_sequence_by_mission_json: "{}".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            conn.execute(
                "INSERT INTO telegram_user_cursors
                 (id, telegram_user_id, last_status_at, last_dashboard_seen_at, last_alert_ack_at,
                  last_digest_at, last_seen_event_sequence_by_mission_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    cursor.id.to_string(),
                    cursor.telegram_user_id,
                    cursor.last_status_at,
                    cursor.last_dashboard_seen_at,
                    cursor.last_alert_ack_at,
                    cursor.last_digest_at,
                    cursor.last_seen_event_sequence_by_mission_json,
                    cursor.created_at,
                    cursor.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(cursor)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_telegram_user_last_status_at(
        &self,
        telegram_user_id: i64,
        last_status_at: &str,
        last_seen_event_sequence_by_mission_json: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let last_status_at = last_status_at.to_string();
        let sequence_json = last_seen_event_sequence_by_mission_json.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let now = now_string();
            conn.execute(
                "INSERT INTO telegram_user_cursors
                 (id, telegram_user_id, last_status_at, last_seen_event_sequence_by_mission_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(telegram_user_id) DO UPDATE SET
                    last_status_at = excluded.last_status_at,
                    last_seen_event_sequence_by_mission_json = excluded.last_seen_event_sequence_by_mission_json,
                    updated_at = excluded.updated_at",
                params![
                    Uuid::new_v4().to_string(),
                    telegram_user_id,
                    last_status_at,
                    sequence_json,
                    now,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_telegram_user_last_digest_at(
        &self,
        telegram_user_id: i64,
        last_digest_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let last_digest_at = last_digest_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let now = now_string();
            conn.execute(
                "INSERT INTO telegram_user_cursors
                 (id, telegram_user_id, last_digest_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(telegram_user_id) DO UPDATE SET
                    last_digest_at = excluded.last_digest_at,
                    updated_at = excluded.updated_at",
                params![
                    Uuid::new_v4().to_string(),
                    telegram_user_id,
                    last_digest_at,
                    now,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_telegram_user_last_alert_ack_at(
        &self,
        telegram_user_id: i64,
        last_alert_ack_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let last_alert_ack_at = last_alert_ack_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let now = now_string();
            conn.execute(
                "INSERT INTO telegram_user_cursors
                 (id, telegram_user_id, last_alert_ack_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(telegram_user_id) DO UPDATE SET
                    last_alert_ack_at = excluded.last_alert_ack_at,
                    updated_at = excluded.updated_at",
                params![
                    Uuid::new_v4().to_string(),
                    telegram_user_id,
                    last_alert_ack_at,
                    now,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_telegram_mission_subscription(
        &self,
        subscription: TelegramMissionSubscription,
    ) -> Result<TelegramMissionSubscription, String> {
        let conn = self.conn.clone();
        let s = subscription.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_mission_subscriptions
                 (id, telegram_user_id, mission_id, interest_level, reason, expires_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(telegram_user_id, mission_id) DO UPDATE SET
                    interest_level = excluded.interest_level,
                    reason = excluded.reason,
                    expires_at = excluded.expires_at,
                    updated_at = excluded.updated_at",
                params![
                    s.id.to_string(),
                    s.telegram_user_id,
                    s.mission_id.to_string(),
                    telegram_interest_to_str(s.interest_level),
                    s.reason,
                    s.expires_at,
                    s.created_at,
                    s.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(subscription)
    }

    async fn list_telegram_mission_subscriptions(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<TelegramMissionSubscription>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, telegram_user_id, mission_id, interest_level, reason, expires_at,
                            created_at, updated_at
                     FROM telegram_mission_subscriptions
                     WHERE telegram_user_id = ?1
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                    params![telegram_user_id],
                    row_to_telegram_mission_subscription,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_alert_preference(
        &self,
        preference: TelegramAlertPreference,
    ) -> Result<TelegramAlertPreference, String> {
        let conn = self.conn.clone();
        let p = preference.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_alert_preferences
                 (id, telegram_user_id, scope, scope_value, rule_text, enabled,
                  created_from_message_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    p.id.to_string(),
                    p.telegram_user_id,
                    p.scope,
                    p.scope_value,
                    p.rule_text,
                    if p.enabled { 1 } else { 0 },
                    p.created_from_message_id,
                    p.created_at,
                    p.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(preference)
    }

    async fn list_telegram_alert_preferences(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<TelegramAlertPreference>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, telegram_user_id, scope, scope_value, rule_text, enabled,
                            created_from_message_id, created_at, updated_at
                     FROM telegram_alert_preferences
                     WHERE telegram_user_id = ?1 AND enabled = 1
                     ORDER BY updated_at DESC, created_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![telegram_user_id], row_to_telegram_alert_preference)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_alert_if_absent(
        &self,
        alert: TelegramAlert,
    ) -> Result<Option<TelegramAlert>, String> {
        let conn = self.conn.clone();
        let a = alert.clone();
        let inserted = tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let changed = conn
                .execute(
                    "INSERT OR IGNORE INTO telegram_alerts
                     (id, telegram_user_id, mission_id, event_kind, importance, title, body,
                      status, telegram_message_id, last_error, created_at, sent_at, acknowledged_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        a.id.to_string(),
                        a.telegram_user_id,
                        a.mission_id.map(|id| id.to_string()),
                        a.event_kind,
                        a.importance,
                        a.title,
                        a.body,
                        a.status,
                        a.telegram_message_id,
                        a.last_error,
                        a.created_at,
                        a.sent_at,
                        a.acknowledged_at,
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(changed > 0)
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(inserted.then_some(alert))
    }

    async fn list_pending_telegram_alerts(
        &self,
        telegram_user_id: i64,
        limit: usize,
    ) -> Result<Vec<TelegramAlert>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, telegram_user_id, mission_id, event_kind, importance, title, body,
                            status, telegram_message_id, last_error, created_at, sent_at, acknowledged_at
                     FROM telegram_alerts
                     WHERE telegram_user_id = ?1 AND status = 'pending'
                     ORDER BY created_at ASC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![telegram_user_id, limit as i64], row_to_telegram_alert)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn mark_telegram_alert_sent(
        &self,
        id: Uuid,
        telegram_message_id: Option<i64>,
        sent_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let sent_at = sent_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_alerts
                 SET status = 'sent', telegram_message_id = ?2, sent_at = ?3, last_error = NULL
                 WHERE id = ?1",
                params![id.to_string(), telegram_message_id, sent_at],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_telegram_alert_by_message_id(
        &self,
        telegram_user_id: i64,
        telegram_message_id: i64,
    ) -> Result<Option<TelegramAlert>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, telegram_user_id, mission_id, event_kind, importance, title, body,
                            status, telegram_message_id, last_error, created_at, sent_at, acknowledged_at
                     FROM telegram_alerts
                     WHERE telegram_user_id = ?1
                       AND telegram_message_id = ?2
                     ORDER BY sent_at DESC, created_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let alerts = stmt
                .query_map(params![telegram_user_id, telegram_message_id], row_to_telegram_alert)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            if alerts.is_empty() {
                return Ok(None);
            }
            let first_mission_id = alerts[0].mission_id;
            if alerts
                .iter()
                .any(|alert| alert.mission_id != first_mission_id)
            {
                return Err("ambiguous_digest_reply".to_string());
            }
            Ok(alerts.into_iter().next())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn acknowledge_pending_telegram_alerts_for_mission(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        acknowledged_at: &str,
    ) -> Result<usize, String> {
        let conn = self.conn.clone();
        let acknowledged_at = acknowledged_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let changed = conn
                .execute(
                    "UPDATE telegram_alerts
                     SET status = 'acknowledged', acknowledged_at = ?3, last_error = NULL
                     WHERE telegram_user_id = ?1
                       AND mission_id = ?2
                       AND status = 'pending'",
                    params![telegram_user_id, mission_id.to_string(), acknowledged_at],
                )
                .map_err(|e| e.to_string())?;
            Ok(changed)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn acknowledge_pending_telegram_alert(
        &self,
        telegram_user_id: i64,
        alert_id: Uuid,
        acknowledged_at: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.clone();
        let acknowledged_at = acknowledged_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let changed = conn
                .execute(
                    "UPDATE telegram_alerts
                     SET status = 'acknowledged', acknowledged_at = ?3, last_error = NULL
                     WHERE telegram_user_id = ?1
                       AND id = ?2
                       AND status = 'pending'",
                    params![telegram_user_id, alert_id.to_string(), acknowledged_at],
                )
                .map_err(|e| e.to_string())?;
            Ok(changed > 0)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn mark_telegram_alert_failed(&self, id: Uuid, error: &str) -> Result<(), String> {
        let conn = self.conn.clone();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_alerts
                 SET status = 'pending', last_error = ?2
                 WHERE id = ?1 AND status = 'pending'",
                params![id.to_string(), error],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn recover_stale_telegram_alerts(
        &self,
        before: &str,
        limit: usize,
    ) -> Result<usize, String> {
        let conn = self.conn.clone();
        let before = before.to_string();
        let limit = limit.clamp(1, 10_000) as i64;
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let ids = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id
                         FROM telegram_alerts
                         WHERE status = 'pending'
                           AND last_error IS NOT NULL
                           AND created_at <= ?1
                         ORDER BY created_at ASC
                         LIMIT ?2",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![before, limit], |row| row.get::<_, String>(0))
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                rows
            };

            for id in &ids {
                conn.execute(
                    "UPDATE telegram_alerts
                     SET last_error = NULL
                     WHERE id = ?1 AND status = 'pending'",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
            }

            Ok(ids.len())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_paloma_decision(
        &self,
        decision: PalomaDecision,
    ) -> Result<PalomaDecision, String> {
        let conn = self.conn.clone();
        let stored = decision.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO paloma_decisions (
                    id, event_source, mission_id, user_id, channel, reason_code,
                    proposed_action, allowed, suppression_reason, policy_snapshot_json,
                    generated_text_hash, generated_text_preview, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    decision.id.to_string(),
                    decision.event_source,
                    decision.mission_id.map(|id| id.to_string()),
                    decision.user_id,
                    decision.channel,
                    decision.reason_code,
                    decision.proposed_action,
                    if decision.allowed { 1 } else { 0 },
                    decision.suppression_reason,
                    decision.policy_snapshot_json,
                    decision.generated_text_hash,
                    decision.generated_text_preview,
                    decision.created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(stored)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_paloma_decisions(&self, limit: usize) -> Result<Vec<PalomaDecision>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, event_source, mission_id, user_id, channel, reason_code,
                            proposed_action, allowed, suppression_reason, policy_snapshot_json,
                            generated_text_hash, generated_text_preview, created_at
                     FROM paloma_decisions
                     ORDER BY created_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![limit as i64], row_to_paloma_decision)
                .map_err(|e| e.to_string())?;
            let mut decisions = Vec::new();
            for row in rows {
                decisions.push(row.map_err(|e| e.to_string())?);
            }
            Ok(decisions)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn claim_paloma_scheduler_job(
        &self,
        name: &str,
        lease_owner: &str,
        now: &str,
        lease_expires_at: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.clone();
        let name = name.to_string();
        let lease_owner = lease_owner.to_string();
        let now = now.to_string();
        let lease_expires_at = lease_expires_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT OR IGNORE INTO paloma_scheduler_jobs
                 (name, run_count, updated_at)
                 VALUES (?1, 0, ?2)",
                params![name, now],
            )
            .map_err(|e| e.to_string())?;
            let changed = conn
                .execute(
                    "UPDATE paloma_scheduler_jobs
                     SET lease_owner = ?2,
                         lease_expires_at = ?4,
                         last_started_at = ?3,
                         updated_at = ?3
                     WHERE name = ?1
                       AND (lease_expires_at IS NULL OR lease_expires_at <= ?3 OR lease_owner = ?2)",
                    params![name, lease_owner, now, lease_expires_at],
                )
                .map_err(|e| e.to_string())?;
            Ok(changed > 0)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn finish_paloma_scheduler_job(
        &self,
        name: &str,
        lease_owner: &str,
        finished_at: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let name = name.to_string();
        let lease_owner = lease_owner.to_string();
        let finished_at = finished_at.to_string();
        let error = error.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE paloma_scheduler_jobs
                 SET lease_owner = NULL,
                     lease_expires_at = NULL,
                     last_finished_at = ?3,
                     last_error = ?4,
                     run_count = run_count + 1,
                     updated_at = ?3
                 WHERE name = ?1 AND lease_owner = ?2",
                params![name, lease_owner, finished_at, error],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_paloma_scheduler_jobs(&self) -> Result<Vec<PalomaSchedulerJob>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT name, lease_owner, lease_expires_at, last_started_at,
                            last_finished_at, last_error, run_count, updated_at
                     FROM paloma_scheduler_jobs
                     ORDER BY name ASC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], row_to_paloma_scheduler_job)
                .map_err(|e| e.to_string())?;
            let mut jobs = Vec::new();
            for row in rows {
                jobs.push(row.map_err(|e| e.to_string())?);
            }
            Ok(jobs)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_paloma_mission_card(
        &self,
        mission_id: Uuid,
    ) -> Result<Option<PalomaMissionCard>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT mission_id, telegram_user_id, channel_id, chat_id, message_id,
                            content_hash, anchor_ts, last_edit_ts, version, archived
                     FROM paloma_mission_cards
                     WHERE mission_id = ?1",
                )
                .map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query_map(params![mission_id.to_string()], row_to_paloma_mission_card)
                .map_err(|e| e.to_string())?;
            match rows.next() {
                Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_paloma_mission_card(
        &self,
        card: PalomaMissionCard,
    ) -> Result<PalomaMissionCard, String> {
        let conn = self.conn.clone();
        let c = card.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO paloma_mission_cards
                 (mission_id, telegram_user_id, channel_id, chat_id, message_id,
                  content_hash, anchor_ts, last_edit_ts, version, archived)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(mission_id) DO UPDATE SET
                    telegram_user_id = excluded.telegram_user_id,
                    channel_id = excluded.channel_id,
                    chat_id = excluded.chat_id,
                    message_id = excluded.message_id,
                    content_hash = excluded.content_hash,
                    anchor_ts = excluded.anchor_ts,
                    last_edit_ts = excluded.last_edit_ts,
                    version = excluded.version,
                    archived = excluded.archived",
                params![
                    c.mission_id.to_string(),
                    c.telegram_user_id,
                    c.channel_id.to_string(),
                    c.chat_id,
                    c.message_id,
                    c.content_hash,
                    c.anchor_ts,
                    c.last_edit_ts,
                    c.version,
                    if c.archived { 1 } else { 0 },
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(card)
    }

    async fn touch_paloma_mission_card(
        &self,
        mission_id: Uuid,
        content_hash: &str,
        last_edit_ts: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let mission_id = mission_id.to_string();
        let content_hash = content_hash.to_string();
        let last_edit_ts = last_edit_ts.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE paloma_mission_cards
                 SET content_hash = ?2,
                     last_edit_ts = ?3,
                     version = version + 1
                 WHERE mission_id = ?1",
                params![mission_id, content_hash, last_edit_ts],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn archive_paloma_mission_card(&self, mission_id: Uuid) -> Result<(), String> {
        let conn = self.conn.clone();
        let mission_id = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE paloma_mission_cards SET archived = 1 WHERE mission_id = ?1",
                params![mission_id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_active_paloma_mission_cards(
        &self,
        telegram_user_id: i64,
    ) -> Result<Vec<PalomaMissionCard>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT mission_id, telegram_user_id, channel_id, chat_id, message_id,
                            content_hash, anchor_ts, last_edit_ts, version, archived
                     FROM paloma_mission_cards
                     WHERE telegram_user_id = ?1 AND archived = 0
                     ORDER BY last_edit_ts DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![telegram_user_id], row_to_paloma_mission_card)
                .map_err(|e| e.to_string())?;
            let mut cards = Vec::new();
            for row in rows {
                cards.push(row.map_err(|e| e.to_string())?);
            }
            Ok(cards)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_paloma_cooldown_state(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        alert_class: &str,
    ) -> Result<Option<PalomaCooldownState>, String> {
        let conn = self.conn.clone();
        let alert_class = alert_class.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT mission_id, alert_class, telegram_user_id,
                            last_sent_at, next_eligible_at, backoff_step
                     FROM paloma_cooldown_state
                     WHERE mission_id = ?1 AND alert_class = ?2 AND telegram_user_id = ?3",
                )
                .map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query_map(
                    params![mission_id.to_string(), alert_class, telegram_user_id],
                    row_to_paloma_cooldown_state,
                )
                .map_err(|e| e.to_string())?;
            match rows.next() {
                Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_paloma_cooldown_state(
        &self,
        state: PalomaCooldownState,
    ) -> Result<PalomaCooldownState, String> {
        let conn = self.conn.clone();
        let s = state.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO paloma_cooldown_state
                 (mission_id, alert_class, telegram_user_id,
                  last_sent_at, next_eligible_at, backoff_step)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(mission_id, alert_class, telegram_user_id) DO UPDATE SET
                    last_sent_at = excluded.last_sent_at,
                    next_eligible_at = excluded.next_eligible_at,
                    backoff_step = excluded.backoff_step",
                params![
                    s.mission_id.to_string(),
                    s.alert_class,
                    s.telegram_user_id,
                    s.last_sent_at,
                    s.next_eligible_at,
                    s.backoff_step,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(state)
    }

    async fn reset_paloma_cooldown_for_mission(&self, mission_id: Uuid) -> Result<(), String> {
        let conn = self.conn.clone();
        let mission_id = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "DELETE FROM paloma_cooldown_state WHERE mission_id = ?1",
                params![mission_id],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_paloma_user_preferences(
        &self,
        telegram_user_id: i64,
    ) -> Result<Option<PalomaUserPreferences>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT telegram_user_id, timezone, quiet_hours_start, quiet_hours_end,
                            max_interrupts_per_hour, max_interrupts_per_day,
                            failure_override_quiet, alert_class_overrides_json,
                            mission_overrides_json, digest_cadence, created_at, updated_at
                     FROM paloma_user_preferences
                     WHERE telegram_user_id = ?1",
                )
                .map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query_map(params![telegram_user_id], row_to_paloma_user_preferences)
                .map_err(|e| e.to_string())?;
            match rows.next() {
                Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_paloma_user_preferences(
        &self,
        preferences: PalomaUserPreferences,
    ) -> Result<PalomaUserPreferences, String> {
        let conn = self.conn.clone();
        let p = preferences.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO paloma_user_preferences
                 (telegram_user_id, timezone, quiet_hours_start, quiet_hours_end,
                  max_interrupts_per_hour, max_interrupts_per_day, failure_override_quiet,
                  alert_class_overrides_json, mission_overrides_json, digest_cadence,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(telegram_user_id) DO UPDATE SET
                    timezone = excluded.timezone,
                    quiet_hours_start = excluded.quiet_hours_start,
                    quiet_hours_end = excluded.quiet_hours_end,
                    max_interrupts_per_hour = excluded.max_interrupts_per_hour,
                    max_interrupts_per_day = excluded.max_interrupts_per_day,
                    failure_override_quiet = excluded.failure_override_quiet,
                    alert_class_overrides_json = excluded.alert_class_overrides_json,
                    mission_overrides_json = excluded.mission_overrides_json,
                    digest_cadence = excluded.digest_cadence,
                    updated_at = excluded.updated_at",
                params![
                    p.telegram_user_id,
                    p.timezone,
                    p.quiet_hours_start,
                    p.quiet_hours_end,
                    p.max_interrupts_per_hour,
                    p.max_interrupts_per_day,
                    if p.failure_override_quiet { 1 } else { 0 },
                    p.alert_class_overrides_json,
                    p.mission_overrides_json,
                    p.digest_cadence,
                    p.created_at,
                    p.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(preferences)
    }

    async fn refresh_pending_telegram_alert_body(
        &self,
        telegram_user_id: i64,
        mission_id: Uuid,
        event_kind: &str,
        title: &str,
        body: &str,
        importance: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.clone();
        let event_kind = event_kind.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let importance = importance.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let changed = conn
                .execute(
                    "UPDATE telegram_alerts
                     SET title = ?4, body = ?5, importance = ?6
                     WHERE telegram_user_id = ?1
                       AND mission_id = ?2
                       AND event_kind = ?3
                       AND status = 'pending'",
                    params![
                        telegram_user_id,
                        mission_id.to_string(),
                        event_kind,
                        title,
                        body,
                        importance,
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok(changed > 0)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn count_paloma_sent_alerts_since(
        &self,
        telegram_user_id: i64,
        since: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.clone();
        let since = since.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            // Rate-limit budget is "how many Telegram messages did we send
            // to this user in the window", not "how many alert rows we
            // marked sent". A digest folds N alerts into one Telegram
            // message; counting the rows would burn the user's budget N×
            // faster than reality.
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(DISTINCT telegram_message_id) FROM telegram_alerts
                     WHERE telegram_user_id = ?1
                       AND status = 'sent'
                       AND sent_at IS NOT NULL
                       AND telegram_message_id IS NOT NULL
                       AND sent_at >= ?2",
                    params![telegram_user_id, since],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            Ok(count)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn consolidate_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<usize, String> {
        let conn = self.conn.clone();
        let channel_id = channel_id.to_string();
        let limit = limit.clamp(1, 10_000) as i64;
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "WITH ranked AS (
                         SELECT id, channel_id, scope, chat_id, subject_user_id, normalized_label,
                                ROW_NUMBER() OVER (
                                    PARTITION BY channel_id, scope, chat_id,
                                                 COALESCE(subject_user_id, -9223372036854775808),
                                                 normalized_label
                                    ORDER BY updated_at DESC, created_at DESC
                                ) AS row_rank,
                                MAX(updated_at) OVER (
                                    PARTITION BY channel_id, scope, chat_id,
                                                 COALESCE(subject_user_id, -9223372036854775808),
                                                 normalized_label
                                ) AS group_updated_at
                         FROM telegram_structured_memory
                         WHERE channel_id = ?1
                           AND kind IN ('fact', 'preference')
                           AND normalized_label IS NOT NULL
                           AND source_role = 'user'
                     ),
                     selected_groups AS (
                         SELECT DISTINCT channel_id, scope, chat_id, subject_user_id,
                                         normalized_label, group_updated_at
                         FROM ranked
                         ORDER BY group_updated_at DESC
                         LIMIT ?2
                     )
                     SELECT ranked.id
                     FROM ranked
                     WHERE ranked.row_rank > 1
                       AND EXISTS (
                           SELECT 1
                           FROM selected_groups
                           WHERE selected_groups.channel_id = ranked.channel_id
                             AND selected_groups.scope = ranked.scope
                             AND selected_groups.chat_id = ranked.chat_id
                             AND COALESCE(selected_groups.subject_user_id, -9223372036854775808)
                                 = COALESCE(ranked.subject_user_id, -9223372036854775808)
                             AND selected_groups.normalized_label = ranked.normalized_label
                       )",
                )
                .map_err(|e| e.to_string())?;
            let delete_ids = stmt
                .query_map(params![channel_id, limit], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            for id in &delete_ids {
                conn.execute(
                    "DELETE FROM telegram_structured_memory WHERE id = ?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    "DELETE FROM telegram_structured_memory_fts WHERE entry_id = ?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
            }

            Ok(delete_ids.len())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_telegram_chat_mission(
        &self,
        channel_id: Uuid,
        chat_id: i64,
    ) -> Result<Option<TelegramChatMission>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT id, channel_id, chat_id, mission_id, chat_title, created_at
                 FROM telegram_chat_missions WHERE channel_id = ?1 AND chat_id = ?2",
                params![channel_id_str, chat_id],
                |row| {
                    let id_str: String = row.get(0)?;
                    let ch_id_str: String = row.get(1)?;
                    let m_id_str: String = row.get(3)?;
                    Ok(TelegramChatMission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        channel_id: Uuid::parse_str(&ch_id_str).unwrap_or_default(),
                        chat_id: row.get(2)?,
                        mission_id: Uuid::parse_str(&m_id_str).unwrap_or_default(),
                        chat_title: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_chat_mission(
        &self,
        mapping: TelegramChatMission,
    ) -> Result<TelegramChatMission, String> {
        let conn = self.conn.clone();
        let m = mapping.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_chat_missions (id, channel_id, chat_id, mission_id, chat_title, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    m.id.to_string(),
                    m.channel_id.to_string(),
                    m.chat_id,
                    m.mission_id.to_string(),
                    m.chat_title,
                    m.created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(mapping)
    }

    async fn update_telegram_chat_mission_title(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        chat_title: Option<String>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_chat_missions
                 SET chat_title = ?3
                 WHERE channel_id = ?1 AND chat_id = ?2",
                params![channel_id_str, chat_id, chat_title],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_telegram_chat_mission_by_mission_id(
        &self,
        mission_id: Uuid,
    ) -> Result<Option<TelegramChatMission>, String> {
        let conn = self.conn.clone();
        let mission_id_str = mission_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT id, channel_id, chat_id, mission_id, chat_title, created_at
                 FROM telegram_chat_missions WHERE mission_id = ?1 LIMIT 1",
                params![mission_id_str],
                |row| {
                    let id_str: String = row.get(0)?;
                    let channel_id_str: String = row.get(1)?;
                    let mission_id_str2: String = row.get(3)?;
                    Ok(TelegramChatMission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        channel_id: Uuid::parse_str(&channel_id_str).unwrap_or_default(),
                        chat_id: row.get(2)?,
                        mission_id: Uuid::parse_str(&mission_id_str2).unwrap_or_default(),
                        chat_title: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_chat_missions(
        &self,
        channel_id: Uuid,
    ) -> Result<Vec<TelegramChatMission>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, chat_title, created_at
                     FROM telegram_chat_missions WHERE channel_id = ?1 ORDER BY created_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let mappings = stmt
                .query_map(params![channel_id_str], |row| {
                    let id_str: String = row.get(0)?;
                    let ch_id_str: String = row.get(1)?;
                    let m_id_str: String = row.get(3)?;
                    Ok(TelegramChatMission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        channel_id: Uuid::parse_str(&ch_id_str).unwrap_or_default(),
                        chat_id: row.get(2)?,
                        mission_id: Uuid::parse_str(&m_id_str).unwrap_or_default(),
                        chat_title: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(mappings)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_scheduled_message(
        &self,
        message: TelegramScheduledMessage,
    ) -> Result<TelegramScheduledMessage, String> {
        let conn = self.conn.clone();
        let msg = message.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_scheduled_messages (
                    id, channel_id, source_mission_id, chat_id, chat_title, text,
                    send_at, sent_at, status, last_error, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    msg.id.to_string(),
                    msg.channel_id.to_string(),
                    msg.source_mission_id.map(|id| id.to_string()),
                    msg.chat_id,
                    msg.chat_title,
                    msg.text,
                    msg.send_at,
                    msg.sent_at,
                    match msg.status {
                        TelegramScheduledMessageStatus::Pending => "pending",
                        TelegramScheduledMessageStatus::Sent => "sent",
                        TelegramScheduledMessageStatus::Failed => "failed",
                    },
                    msg.last_error,
                    msg.created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(message)
    }

    async fn list_due_telegram_scheduled_messages(
        &self,
        channel_id: Uuid,
        send_at: &str,
        limit: usize,
    ) -> Result<Vec<TelegramScheduledMessage>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        let send_at = send_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, source_mission_id, chat_id, chat_title, text,
                            send_at, sent_at, status, last_error, created_at
                     FROM telegram_scheduled_messages
                     WHERE channel_id = ?1
                       AND status = 'pending'
                       AND send_at <= ?2
                     ORDER BY send_at ASC
                     LIMIT ?3",
                )
                .map_err(|e| e.to_string())?;
            let messages = stmt
                .query_map(
                    params![channel_id_str, send_at, limit as i64],
                    row_to_telegram_scheduled_message,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(messages)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_scheduled_messages(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramScheduledMessage>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let sql = if chat_id.is_some() {
                "SELECT id, channel_id, source_mission_id, chat_id, chat_title, text,
                        send_at, sent_at, status, last_error, created_at
                 FROM telegram_scheduled_messages
                 WHERE channel_id = ?1 AND chat_id = ?2
                 ORDER BY created_at DESC
                 LIMIT ?3"
            } else {
                "SELECT id, channel_id, source_mission_id, chat_id, chat_title, text,
                        send_at, sent_at, status, last_error, created_at
                 FROM telegram_scheduled_messages
                 WHERE channel_id = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
            let limit_i64 = limit as i64;
            let messages = if let Some(chat_id) = chat_id {
                stmt.query_map(
                    params![channel_id_str, chat_id, limit_i64],
                    row_to_telegram_scheduled_message,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
            } else {
                stmt.query_map(
                    params![channel_id_str, limit_i64],
                    row_to_telegram_scheduled_message,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
            };
            Ok(messages)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn claim_telegram_scheduled_message(&self, id: Uuid) -> Result<bool, String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let updated = conn
                .execute(
                    "UPDATE telegram_scheduled_messages
                     SET status = 'sending', sent_at = ?2
                     WHERE id = ?1 AND status = 'pending'",
                    params![id_str, now],
                )
                .map_err(|e| e.to_string())?;
            Ok(updated > 0)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn recover_stale_sending_scheduled_messages(
        &self,
        max_age_secs: i64,
    ) -> Result<u32, String> {
        let conn = self.conn.clone();
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(max_age_secs)).to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let updated = conn
                .execute(
                    "UPDATE telegram_scheduled_messages
                     SET status = 'pending', sent_at = NULL
                     WHERE status = 'sending'
                       AND sent_at IS NOT NULL AND sent_at < ?1",
                    params![cutoff],
                )
                .map_err(|e| e.to_string())?;
            Ok(updated as u32)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn mark_telegram_scheduled_message_sent(
        &self,
        id: Uuid,
        sent_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let sent_at = sent_at.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_scheduled_messages
                 SET status = 'sent', sent_at = ?1, last_error = NULL
                 WHERE id = ?2",
                params![sent_at, id_str],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn mark_telegram_scheduled_message_failed(
        &self,
        id: Uuid,
        error: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let id_str = id.to_string();
        let error = error.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_scheduled_messages
                 SET status = 'failed', last_error = ?1
                 WHERE id = ?2",
                params![error, id_str],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_telegram_structured_memory(
        &self,
        entry: TelegramStructuredMemoryEntry,
    ) -> Result<TelegramStructuredMemoryEntry, String> {
        let conn = self.conn.clone();
        let entry_clone = entry.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let normalized_label = entry_clone
                .label
                .as_deref()
                .map(|label| label.trim().to_lowercase())
                .filter(|label| !label.is_empty());
            let kind = match entry_clone.kind {
                TelegramStructuredMemoryKind::Fact => "fact",
                TelegramStructuredMemoryKind::Note => "note",
                TelegramStructuredMemoryKind::Task => "task",
                TelegramStructuredMemoryKind::Preference => "preference",
            };
            let scope = match entry_clone.scope {
                TelegramStructuredMemoryScope::Chat => "chat",
                TelegramStructuredMemoryScope::User => "user",
                TelegramStructuredMemoryScope::Channel => "channel",
            };

            let is_upsertable_kind = matches!(
                entry_clone.kind,
                TelegramStructuredMemoryKind::Fact | TelegramStructuredMemoryKind::Preference
            ) && normalized_label.is_some();

            if is_upsertable_kind {
                let updated = match entry_clone.scope.clone() {
                    TelegramStructuredMemoryScope::User
                        if entry_clone.subject_user_id.is_some() =>
                    {
                        conn.execute(
                            "UPDATE telegram_structured_memory
                             SET mission_id = ?1,
                                 chat_id = ?2,
                                 value = ?3,
                                 subject_username = ?4,
                                 subject_display_name = ?5,
                                 source_message_id = ?6,
                                 source_role = ?7,
                                 updated_at = ?8
                             WHERE channel_id = ?9
                               AND scope = ?10
                               AND subject_user_id = ?11
                               AND kind = ?12
                               AND normalized_label = ?13",
                            params![
                                entry_clone.mission_id.map(|id| id.to_string()),
                                entry_clone.chat_id,
                                entry_clone.value.clone(),
                                entry_clone.subject_username.clone(),
                                entry_clone.subject_display_name.clone(),
                                entry_clone.source_message_id,
                                entry_clone.source_role.clone(),
                                entry_clone.updated_at.clone(),
                                entry_clone.channel_id.to_string(),
                                scope,
                                entry_clone.subject_user_id,
                                kind,
                                normalized_label,
                            ],
                        )
                        .map_err(|e| e.to_string())?;
                        conn.changes() > 0
                    }
                    TelegramStructuredMemoryScope::Channel => {
                        conn.execute(
                            "UPDATE telegram_structured_memory
                             SET mission_id = ?1,
                                 chat_id = ?2,
                                 value = ?3,
                                 source_message_id = ?4,
                                 source_role = ?5,
                                 updated_at = ?6
                             WHERE channel_id = ?7
                               AND scope = ?8
                               AND kind = ?9
                               AND normalized_label = ?10",
                            params![
                                entry_clone.mission_id.map(|id| id.to_string()),
                                entry_clone.chat_id,
                                entry_clone.value.clone(),
                                entry_clone.source_message_id,
                                entry_clone.source_role.clone(),
                                entry_clone.updated_at.clone(),
                                entry_clone.channel_id.to_string(),
                                scope,
                                kind,
                                normalized_label,
                            ],
                        )
                        .map_err(|e| e.to_string())?;
                        conn.changes() > 0
                    }
                    _ => {
                        conn.execute(
                            "UPDATE telegram_structured_memory
                             SET mission_id = ?1,
                                 value = ?2,
                                 source_message_id = ?3,
                                 source_role = ?4,
                                 updated_at = ?5
                             WHERE channel_id = ?6
                               AND scope = ?7
                               AND chat_id = ?8
                               AND kind = ?9
                               AND normalized_label = ?10",
                            params![
                                entry_clone.mission_id.map(|id| id.to_string()),
                                entry_clone.value.clone(),
                                entry_clone.source_message_id,
                                entry_clone.source_role.clone(),
                                entry_clone.updated_at.clone(),
                                entry_clone.channel_id.to_string(),
                                scope,
                                entry_clone.chat_id,
                                kind,
                                normalized_label,
                            ],
                        )
                        .map_err(|e| e.to_string())?;
                        conn.changes() > 0
                    }
                };

                if updated {
                    let updated_entry = Self::load_telegram_structured_memory_for_upsert(
                        &conn,
                        &entry_clone,
                        scope,
                        kind,
                        normalized_label.as_deref().unwrap_or_default(),
                    )?
                    .ok_or_else(|| {
                        "Updated telegram structured memory row could not be reloaded".to_string()
                    })?;
                    Self::upsert_telegram_memory_search_index_entry(&conn, &updated_entry)?;
                    return Ok::<_, String>(());
                }
            }

            conn.execute(
                "INSERT INTO telegram_structured_memory (
                    id, channel_id, chat_id, mission_id, scope, kind, label, normalized_label, value,
                    subject_user_id, subject_username, subject_display_name,
                    source_message_id, source_role, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    entry_clone.id.to_string(),
                    entry_clone.channel_id.to_string(),
                    entry_clone.chat_id,
                    entry_clone.mission_id.map(|id| id.to_string()),
                    scope,
                    kind,
                    entry_clone.label,
                    normalized_label,
                    entry_clone.value,
                    entry_clone.subject_user_id,
                    entry_clone.subject_username,
                    entry_clone.subject_display_name,
                    entry_clone.source_message_id,
                    entry_clone.source_role,
                    entry_clone.created_at,
                    entry_clone.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Self::upsert_telegram_memory_search_index_entry(&conn, &entry_clone)?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(entry)
    }

    async fn list_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        subject_user_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            // Build WHERE clause dynamically so all filters are applied
            // before the LIMIT truncation.
            let mut where_clauses = vec!["channel_id = ?1".to_string()];
            let mut param_idx = 2u32;
            if chat_id.is_some() {
                where_clauses.push(format!("chat_id = ?{}", param_idx));
                param_idx += 1;
            }
            if subject_user_id.is_some() {
                where_clauses.push(format!("subject_user_id = ?{}", param_idx));
                param_idx += 1;
            }
            let sql = format!(
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE {}
                 ORDER BY updated_at DESC
                 LIMIT ?{}",
                where_clauses.join(" AND "),
                param_idx,
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            // Build params vec dynamically matching the WHERE clause above.
            let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(channel_id_str)];
            if let Some(cid) = chat_id {
                params_vec.push(Box::new(cid));
            }
            if let Some(suid) = subject_user_id {
                params_vec.push(Box::new(suid));
            }
            params_vec.push(Box::new(limit as i64));
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params_vec.iter().map(|p| p.as_ref()).collect();
            let entries = stmt
                .query_map(param_refs.as_slice(), row_to_telegram_structured_memory)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(entries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn search_telegram_structured_memory(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        let query = query.trim().to_lowercase();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let sql = if chat_id.is_some() {
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE channel_id = ?1
                   AND chat_id = ?2
                   AND (
                     instr(lower(coalesce(label, '')), ?3) > 0
                     OR instr(lower(value), ?3) > 0
                   )
                 ORDER BY updated_at DESC
                 LIMIT ?4"
            } else {
                "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                        subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                 FROM telegram_structured_memory
                 WHERE channel_id = ?1
                   AND (
                     instr(lower(coalesce(label, '')), ?2) > 0
                     OR instr(lower(value), ?2) > 0
                   )
                 ORDER BY updated_at DESC
                 LIMIT ?3"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
            let entries = if let Some(chat_id) = chat_id {
                stmt.query_map(
                    params![channel_id_str, chat_id, query, limit as i64],
                    row_to_telegram_structured_memory,
                )
            } else {
                stmt.query_map(
                    params![channel_id_str, query, limit as i64],
                    row_to_telegram_structured_memory,
                )
            }
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
            Ok(entries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn search_telegram_structured_memory_hybrid(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        subject_user_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemorySearchHit>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        let normalized_query = normalize_search_text(query.trim());
        let query_tokens = tokenize_search_text(query);
        let core_query = query_tokens.join(" ");
        let fts_query = build_fts_query_from_tokens(&query_tokens, &normalized_query);

        tokio::task::spawn_blocking(move || {
            if normalized_query.is_empty() {
                return Ok(Vec::new());
            }

            let conn = conn.blocking_lock();
            let candidate_limit = (limit.max(8) * 8).min(TELEGRAM_MEMORY_SEARCH_MAX_CANDIDATES);

            let mut stmt = match chat_id {
                Some(_) => conn.prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE channel_id = ?1
                       AND (
                         scope = 'channel'
                         OR (scope = 'chat' AND chat_id = ?2)
                         OR (?3 IS NOT NULL AND scope = 'user' AND subject_user_id = ?3)
                       )
                     ORDER BY updated_at DESC
                     LIMIT ?4",
                ),
                None if subject_user_id.is_some() => conn.prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE channel_id = ?1
                       AND (
                         scope = 'channel'
                         OR (scope = 'user' AND subject_user_id = ?2)
                       )
                     ORDER BY updated_at DESC
                     LIMIT ?3",
                ),
                None => conn.prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE channel_id = ?1
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                ),
            }
            .map_err(|e| e.to_string())?;

            let mut candidates = match (chat_id, subject_user_id) {
                (Some(chat_id), subject_user_id) => stmt.query_map(
                    params![
                        channel_id_str.as_str(),
                        chat_id,
                        subject_user_id,
                        candidate_limit as i64
                    ],
                    row_to_telegram_structured_memory,
                ),
                (None, Some(subject_user_id)) => stmt.query_map(
                    params![
                        channel_id_str.as_str(),
                        subject_user_id,
                        candidate_limit as i64
                    ],
                    row_to_telegram_structured_memory,
                ),
                (None, None) => stmt.query_map(
                    params![channel_id_str.as_str(), candidate_limit as i64],
                    row_to_telegram_structured_memory,
                ),
            }
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

            let mut fts_scores: HashMap<Uuid, f64> = HashMap::new();
            if let Some(fts_query) = fts_query {
                let mut stmt = match chat_id {
                    Some(_) => conn.prepare(
                        "SELECT entry_id, bm25(telegram_structured_memory_fts) AS rank
                         FROM telegram_structured_memory_fts
                         WHERE channel_id = ?1
                           AND (
                             scope = 'channel'
                             OR (scope = 'chat' AND chat_id = ?2)
                             OR (?3 IS NOT NULL AND scope = 'user' AND subject_user_id = ?3)
                           )
                           AND telegram_structured_memory_fts MATCH ?4
                         ORDER BY rank
                         LIMIT ?5",
                    ),
                    None if subject_user_id.is_some() => conn.prepare(
                        "SELECT entry_id, bm25(telegram_structured_memory_fts) AS rank
                         FROM telegram_structured_memory_fts
                         WHERE channel_id = ?1
                           AND (
                             scope = 'channel'
                             OR (scope = 'user' AND subject_user_id = ?2)
                           )
                           AND telegram_structured_memory_fts MATCH ?3
                         ORDER BY rank
                         LIMIT ?4",
                    ),
                    None => conn.prepare(
                        "SELECT entry_id, bm25(telegram_structured_memory_fts) AS rank
                         FROM telegram_structured_memory_fts
                         WHERE channel_id = ?1
                           AND telegram_structured_memory_fts MATCH ?2
                         ORDER BY rank
                         LIMIT ?3",
                    ),
                }
                .map_err(|e| e.to_string())?;

                let rows = match (chat_id, subject_user_id) {
                    (Some(chat_id), subject_user_id) => stmt.query_map(
                        params![
                            channel_id_str.as_str(),
                            chat_id,
                            subject_user_id,
                            fts_query.as_str(),
                            limit.max(8) as i64
                        ],
                        row_to_telegram_memory_search_rank,
                    ),
                    (None, Some(subject_user_id)) => stmt.query_map(
                        params![
                            channel_id_str.as_str(),
                            subject_user_id,
                            fts_query.as_str(),
                            limit.max(8) as i64
                        ],
                        row_to_telegram_memory_search_rank,
                    ),
                    (None, None) => stmt.query_map(
                        params![
                            channel_id_str.as_str(),
                            fts_query.as_str(),
                            limit.max(8) as i64
                        ],
                        row_to_telegram_memory_search_rank,
                    ),
                }
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

                for (entry_id, rank) in rows {
                    let entry_uuid = parse_uuid_or_nil(&entry_id);
                    if entry_uuid.is_nil() {
                        continue;
                    }
                    let weight = 18.0 / (1.0 + rank.abs());
                    fts_scores.insert(entry_uuid, weight);
                }
            }

            // Inject FTS-matched entries that fell outside the recency window
            // so older but highly relevant memories are not silently dropped.
            let candidate_ids: std::collections::HashSet<Uuid> =
                candidates.iter().map(|e| e.id).collect();
            for fts_id in fts_scores.keys() {
                if candidate_ids.contains(fts_id) {
                    continue;
                }
                let id_str = fts_id.to_string();
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE id = ?1",
                ) {
                    if let Ok(mut rows) =
                        stmt.query_map(params![id_str], row_to_telegram_structured_memory)
                    {
                        if let Some(Ok(entry)) = rows.next() {
                            candidates.push(entry);
                        }
                    }
                }
            }

            let mut hits = candidates
                .into_iter()
                .filter_map(|entry| {
                    score_memory_entry(
                        &entry,
                        &normalized_query,
                        &core_query,
                        &query_tokens,
                        fts_scores.get(&entry.id).copied(),
                    )
                })
                .collect::<Vec<_>>();

            hits.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| {
                        scope_rank(&left.entry.scope).cmp(&scope_rank(&right.entry.scope))
                    })
                    .then_with(|| right.entry.updated_at.cmp(&left.entry.updated_at))
            });
            hits.truncate(limit);
            Ok(hits)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_memory_context(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        subject_user_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE channel_id = ?1
                       AND (
                         scope = 'channel'
                         OR (scope = 'chat' AND chat_id = ?2)
                         OR (?3 IS NOT NULL AND scope = 'user' AND subject_user_id = ?3)
                       )
                     ORDER BY
                       CASE scope
                         WHEN 'chat' THEN 0
                         WHEN 'user' THEN 1
                         ELSE 2
                       END,
                       updated_at DESC
                     LIMIT ?4",
                )
                .map_err(|e| e.to_string())?;
            let entries = stmt
                .query_map(
                    params![channel_id_str, chat_id, subject_user_id, limit as i64],
                    row_to_telegram_structured_memory,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(entries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn search_telegram_memory_context(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        subject_user_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<TelegramStructuredMemoryEntry>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        let query = query.trim().to_lowercase();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, scope, kind, label, value,
                            subject_user_id, subject_username, subject_display_name,
                            source_message_id, source_role, created_at, updated_at
                     FROM telegram_structured_memory
                     WHERE channel_id = ?1
                       AND (
                         scope = 'channel'
                         OR (scope = 'chat' AND chat_id = ?2)
                         OR (?3 IS NOT NULL AND scope = 'user' AND subject_user_id = ?3)
                       )
                       AND (
                         instr(lower(coalesce(label, '')), ?4) > 0
                         OR instr(lower(value), ?4) > 0
                         OR instr(lower(coalesce(subject_display_name, '')), ?4) > 0
                         OR instr(lower(coalesce(subject_username, '')), ?4) > 0
                       )
                     ORDER BY
                       CASE scope
                         WHEN 'chat' THEN 0
                         WHEN 'user' THEN 1
                         ELSE 2
                       END,
                       updated_at DESC
                     LIMIT ?5",
                )
                .map_err(|e| e.to_string())?;
            let entries = stmt
                .query_map(
                    params![
                        channel_id_str,
                        chat_id,
                        subject_user_id,
                        query,
                        limit as i64
                    ],
                    row_to_telegram_structured_memory,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            Ok(entries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_action_execution(
        &self,
        execution: TelegramActionExecution,
    ) -> Result<TelegramActionExecution, String> {
        let conn = self.conn.clone();
        let execution_clone = execution.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_action_executions (
                    id, channel_id, source_mission_id, source_chat_id, target_chat_id, target_chat_title,
                    action_kind, target_kind, target_value, text, delay_seconds, scheduled_message_id,
                    status, last_error, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    execution_clone.id.to_string(),
                    execution_clone.channel_id.to_string(),
                    execution_clone.source_mission_id.map(|id| id.to_string()),
                    execution_clone.source_chat_id,
                    execution_clone.target_chat_id,
                    execution_clone.target_chat_title,
                    match execution_clone.action_kind {
                        TelegramActionExecutionKind::Send => "send",
                        TelegramActionExecutionKind::Reminder => "reminder",
                    },
                    execution_clone.target_kind,
                    execution_clone.target_value,
                    execution_clone.text,
                    execution_clone.delay_seconds as i64,
                    execution_clone.scheduled_message_id.map(|id| id.to_string()),
                    match execution_clone.status {
                        TelegramActionExecutionStatus::Pending => "pending",
                        TelegramActionExecutionStatus::Sent => "sent",
                        TelegramActionExecutionStatus::Failed => "failed",
                    },
                    execution_clone.last_error,
                    execution_clone.created_at,
                    execution_clone.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(execution)
    }

    async fn list_telegram_action_executions(
        &self,
        channel_id: Uuid,
        chat_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TelegramActionExecution>, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let sql = if chat_id.is_some() {
                "SELECT id, channel_id, source_mission_id, source_chat_id, target_chat_id,
                        target_chat_title, action_kind, target_kind, target_value, text,
                        delay_seconds, scheduled_message_id, status, last_error, created_at, updated_at
                 FROM telegram_action_executions
                 WHERE channel_id = ?1 AND target_chat_id = ?2
                 ORDER BY updated_at DESC
                 LIMIT ?3"
            } else {
                "SELECT id, channel_id, source_mission_id, source_chat_id, target_chat_id,
                        target_chat_title, action_kind, target_kind, target_value, text,
                        delay_seconds, scheduled_message_id, status, last_error, created_at, updated_at
                 FROM telegram_action_executions
                 WHERE channel_id = ?1
                 ORDER BY updated_at DESC
                 LIMIT ?2"
            };
            let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
            let limit_i64 = limit as i64;
            let entries = if let Some(chat_id) = chat_id {
                stmt.query_map(
                    params![channel_id_str, chat_id, limit_i64],
                    row_to_telegram_action_execution,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
            } else {
                stmt.query_map(
                    params![channel_id_str, limit_i64],
                    row_to_telegram_action_execution,
                )
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
            };
            Ok(entries)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn mark_telegram_action_execution_by_scheduled_message(
        &self,
        scheduled_message_id: Uuid,
        status: TelegramActionExecutionStatus,
        last_error: Option<&str>,
        updated_at: &str,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let scheduled_message_id = scheduled_message_id.to_string();
        let updated_at = updated_at.to_string();
        let last_error = last_error.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_action_executions
                 SET status = ?1,
                     last_error = ?2,
                     updated_at = ?3
                 WHERE scheduled_message_id = ?4",
                params![
                    match status {
                        TelegramActionExecutionStatus::Pending => "pending",
                        TelegramActionExecutionStatus::Sent => "sent",
                        TelegramActionExecutionStatus::Failed => "failed",
                    },
                    last_error,
                    updated_at,
                    scheduled_message_id,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn upsert_telegram_conversation(
        &self,
        conversation: TelegramConversation,
    ) -> Result<TelegramConversation, String> {
        let conn = self.conn.clone();
        let conversation_clone = conversation.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_conversations (
                    id, channel_id, chat_id, mission_id, chat_title, chat_type, last_message_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(channel_id, chat_id) DO UPDATE SET
                    mission_id = excluded.mission_id,
                    chat_title = coalesce(excluded.chat_title, telegram_conversations.chat_title),
                    chat_type = coalesce(excluded.chat_type, telegram_conversations.chat_type),
                    last_message_at = coalesce(excluded.last_message_at, telegram_conversations.last_message_at),
                    updated_at = excluded.updated_at",
                params![
                    conversation_clone.id.to_string(),
                    conversation_clone.channel_id.to_string(),
                    conversation_clone.chat_id,
                    conversation_clone.mission_id.map(|id| id.to_string()),
                    conversation_clone.chat_title.clone(),
                    conversation_clone.chat_type.clone(),
                    conversation_clone.last_message_at.clone(),
                    conversation_clone.created_at.clone(),
                    conversation_clone.updated_at.clone(),
                ],
            )
            .map_err(|e| e.to_string())?;

            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, chat_title, chat_type, last_message_at, created_at, updated_at
                     FROM telegram_conversations
                     WHERE channel_id = ?1 AND chat_id = ?2",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(
                params![
                    conversation_clone.channel_id.to_string(),
                    conversation_clone.chat_id
                ],
                row_to_telegram_conversation,
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_telegram_conversation_by_chat(
        &self,
        channel_id: Uuid,
        chat_id: i64,
    ) -> Result<Option<TelegramConversation>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, chat_title, chat_type, last_message_at, created_at, updated_at
                     FROM telegram_conversations
                     WHERE channel_id = ?1 AND chat_id = ?2",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(
                params![channel_id.to_string(), chat_id],
                row_to_telegram_conversation,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_conversations(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramConversation>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, chat_id, mission_id, chat_title, chat_type, last_message_at, created_at, updated_at
                     FROM telegram_conversations
                     WHERE channel_id = ?1
                     ORDER BY coalesce(last_message_at, updated_at) DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                params![channel_id.to_string(), limit as i64],
                row_to_telegram_conversation,
            )
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_conversation_message(
        &self,
        message: TelegramConversationMessage,
    ) -> Result<TelegramConversationMessage, String> {
        let conn = self.conn.clone();
        let message_clone = message.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_conversation_messages (
                    id, conversation_id, channel_id, chat_id, mission_id, workflow_id, telegram_message_id,
                    direction, role, sender_user_id, sender_username, sender_display_name,
                    reply_to_message_id, text, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    message_clone.id.to_string(),
                    message_clone.conversation_id.to_string(),
                    message_clone.channel_id.to_string(),
                    message_clone.chat_id,
                    message_clone.mission_id.map(|id| id.to_string()),
                    message_clone.workflow_id.map(|id| id.to_string()),
                    message_clone.telegram_message_id,
                    match message_clone.direction {
                        TelegramConversationMessageDirection::Inbound => "inbound",
                        TelegramConversationMessageDirection::Outbound => "outbound",
                    },
                    message_clone.role.clone(),
                    message_clone.sender_user_id,
                    message_clone.sender_username.clone(),
                    message_clone.sender_display_name.clone(),
                    message_clone.reply_to_message_id,
                    message_clone.text.clone(),
                    message_clone.created_at.clone(),
                ],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE telegram_conversations
                 SET last_message_at = ?1, updated_at = ?1
                 WHERE id = ?2",
                params![message_clone.created_at, message_clone.conversation_id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(message_clone)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_conversation_messages(
        &self,
        conversation_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramConversationMessage>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, conversation_id, channel_id, chat_id, mission_id, workflow_id, telegram_message_id,
                            direction, role, sender_user_id, sender_username, sender_display_name,
                            reply_to_message_id, text, created_at
                     FROM telegram_conversation_messages
                     WHERE conversation_id = ?1
                     ORDER BY created_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                params![conversation_id.to_string(), limit as i64],
                row_to_telegram_conversation_message,
            )
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_workflow(
        &self,
        workflow: TelegramWorkflow,
    ) -> Result<TelegramWorkflow, String> {
        let conn = self.conn.clone();
        let workflow_clone = workflow.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_workflows (
                    id, channel_id, origin_conversation_id, origin_chat_id, origin_mission_id,
                    target_conversation_id, target_chat_id, target_chat_title, target_chat_type,
                    target_request_message_id, initiated_by_user_id, initiated_by_username, kind,
                    status, request_text, latest_reply_text, summary, last_error, created_at,
                    updated_at, completed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    workflow_clone.id.to_string(),
                    workflow_clone.channel_id.to_string(),
                    workflow_clone.origin_conversation_id.to_string(),
                    workflow_clone.origin_chat_id,
                    workflow_clone.origin_mission_id.map(|id| id.to_string()),
                    workflow_clone.target_conversation_id.map(|id| id.to_string()),
                    workflow_clone.target_chat_id,
                    workflow_clone.target_chat_title.clone(),
                    workflow_clone.target_chat_type.clone(),
                    workflow_clone.target_request_message_id,
                    workflow_clone.initiated_by_user_id,
                    workflow_clone.initiated_by_username.clone(),
                    match workflow_clone.kind {
                        TelegramWorkflowKind::RequestReply => "request_reply",
                    },
                    match workflow_clone.status {
                        TelegramWorkflowStatus::WaitingExternal => "waiting_external",
                        TelegramWorkflowStatus::RelayedToOrigin => "relayed_to_origin",
                        TelegramWorkflowStatus::Completed => "completed",
                        TelegramWorkflowStatus::Failed => "failed",
                        TelegramWorkflowStatus::Cancelled => "cancelled",
                    },
                    workflow_clone.request_text.clone(),
                    workflow_clone.latest_reply_text.clone(),
                    workflow_clone.summary.clone(),
                    workflow_clone.last_error.clone(),
                    workflow_clone.created_at.clone(),
                    workflow_clone.updated_at.clone(),
                    workflow_clone.completed_at.clone(),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(workflow_clone)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn update_telegram_workflow(&self, workflow: TelegramWorkflow) -> Result<(), String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE telegram_workflows
                 SET target_conversation_id = ?1,
                     target_chat_id = ?2,
                     target_chat_title = ?3,
                     target_chat_type = ?4,
                     target_request_message_id = ?5,
                     status = ?6,
                     latest_reply_text = ?7,
                     summary = ?8,
                     last_error = ?9,
                     updated_at = ?10,
                     completed_at = ?11
                 WHERE id = ?12",
                params![
                    workflow.target_conversation_id.map(|id| id.to_string()),
                    workflow.target_chat_id,
                    workflow.target_chat_title,
                    workflow.target_chat_type,
                    workflow.target_request_message_id,
                    match workflow.status {
                        TelegramWorkflowStatus::WaitingExternal => "waiting_external",
                        TelegramWorkflowStatus::RelayedToOrigin => "relayed_to_origin",
                        TelegramWorkflowStatus::Completed => "completed",
                        TelegramWorkflowStatus::Failed => "failed",
                        TelegramWorkflowStatus::Cancelled => "cancelled",
                    },
                    workflow.latest_reply_text,
                    workflow.summary,
                    workflow.last_error,
                    workflow.updated_at,
                    workflow.completed_at,
                    workflow.id.to_string(),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn timeout_stale_telegram_workflows(&self, max_age_secs: i64) -> Result<u32, String> {
        let conn = self.conn.clone();
        let now = now_string();
        // Calculate the cutoff timestamp in Rust (RFC 3339 format sorts lexicographically)
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(max_age_secs);
        let cutoff_str = cutoff.to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let updated = conn
                .execute(
                    "UPDATE telegram_workflows
                     SET status = 'failed',
                         last_error = 'Timed out waiting for external reply',
                         updated_at = ?1,
                         completed_at = ?1
                     WHERE status = 'waiting_external'
                       AND updated_at < ?2",
                    params![now, cutoff_str],
                )
                .map_err(|e| e.to_string())?;
            Ok(updated as u32)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn register_webhook_update(
        &self,
        channel_id: Uuid,
        update_id: i64,
    ) -> Result<bool, String> {
        let conn = self.conn.clone();
        let channel_id_str = channel_id.to_string();
        let now = now_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            // INSERT OR IGNORE: if the row already exists, nothing happens
            // and changes() returns 0.
            let inserted = conn
                .execute(
                    "INSERT OR IGNORE INTO telegram_webhook_dedup (channel_id, update_id, seen_at)
                     VALUES (?1, ?2, ?3)",
                    params![channel_id_str, update_id, now],
                )
                .map_err(|e| e.to_string())?;
            Ok(inserted > 0)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn cleanup_webhook_dedup(&self, max_age_secs: i64) -> Result<u32, String> {
        let conn = self.conn.clone();
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(max_age_secs)).to_rfc3339();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let deleted = conn
                .execute(
                    "DELETE FROM telegram_webhook_dedup
                     WHERE seen_at < ?1",
                    params![cutoff],
                )
                .map_err(|e| e.to_string())?;
            Ok(deleted as u32)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    async fn list_telegram_workflows(
        &self,
        channel_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramWorkflow>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, origin_conversation_id, origin_chat_id, origin_mission_id,
                            target_conversation_id, target_chat_id, target_chat_title, target_chat_type,
                            target_request_message_id, initiated_by_user_id, initiated_by_username,
                            kind, status, request_text, latest_reply_text, summary, last_error,
                            created_at, updated_at, completed_at
                     FROM telegram_workflows
                     WHERE channel_id = ?1
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                params![channel_id.to_string(), limit as i64],
                row_to_telegram_workflow,
            )
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_pending_telegram_workflow_for_target_chat(
        &self,
        channel_id: Uuid,
        target_chat_id: i64,
    ) -> Result<Option<TelegramWorkflow>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, origin_conversation_id, origin_chat_id, origin_mission_id,
                            target_conversation_id, target_chat_id, target_chat_title, target_chat_type,
                            target_request_message_id, initiated_by_user_id, initiated_by_username,
                            kind, status, request_text, latest_reply_text, summary, last_error,
                            created_at, updated_at, completed_at
                     FROM telegram_workflows
                     WHERE channel_id = ?1
                       AND target_chat_id = ?2
                       AND status = 'waiting_external'
                     ORDER BY updated_at DESC
                     LIMIT 1",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(
                params![channel_id.to_string(), target_chat_id],
                row_to_telegram_workflow,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn get_pending_telegram_workflow_for_target_message(
        &self,
        channel_id: Uuid,
        target_chat_id: i64,
        request_message_id: i64,
    ) -> Result<Option<TelegramWorkflow>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, channel_id, origin_conversation_id, origin_chat_id, origin_mission_id,
                            target_conversation_id, target_chat_id, target_chat_title, target_chat_type,
                            target_request_message_id, initiated_by_user_id, initiated_by_username,
                            kind, status, request_text, latest_reply_text, summary, last_error,
                            created_at, updated_at, completed_at
                     FROM telegram_workflows
                     WHERE channel_id = ?1
                       AND target_chat_id = ?2
                       AND target_request_message_id = ?3
                       AND status = 'waiting_external'
                     ORDER BY updated_at DESC
                     LIMIT 1",
                )
                .map_err(|e| e.to_string())?;
            stmt.query_row(
                params![channel_id.to_string(), target_chat_id, request_message_id],
                row_to_telegram_workflow,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn create_telegram_workflow_event(
        &self,
        event: TelegramWorkflowEvent,
    ) -> Result<TelegramWorkflowEvent, String> {
        let conn = self.conn.clone();
        let event_clone = event.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO telegram_workflow_events (
                    id, workflow_id, conversation_id, event_type, payload_json, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event_clone.id.to_string(),
                    event_clone.workflow_id.to_string(),
                    event_clone.conversation_id.map(|id| id.to_string()),
                    event_clone.event_type.clone(),
                    event_clone.payload_json.clone(),
                    event_clone.created_at.clone(),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(event_clone)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn list_telegram_workflow_events(
        &self,
        workflow_id: Uuid,
        limit: usize,
    ) -> Result<Vec<TelegramWorkflowEvent>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, workflow_id, conversation_id, event_type, payload_json, created_at
                     FROM telegram_workflow_events
                     WHERE workflow_id = ?1
                     ORDER BY created_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                    params![workflow_id.to_string(), limit as i64],
                    row_to_telegram_workflow_event,
                )
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn import_mission_bundle(
        &self,
        bundle: super::MissionBundle,
        options: super::MissionImportOptions,
    ) -> Result<Uuid, String> {
        use super::MissionBundle;
        // Always mint fresh IDs on import so a bundle can be re-imported
        // into the same instance for debugging without collisions and
        // without clobbering the source history if the bundle round-trips
        // back. Mapping tables below rewrite child rows accordingly.
        let new_mission_id = Uuid::new_v4();
        let target_workspace_id = options
            .target_workspace_id
            .unwrap_or(bundle.mission.workspace_id);
        // Prefer the caller-provided target name; only fall back to the
        // bundle's own name when no override was passed. This keeps the
        // stored workspace_name consistent with the target workspace_id.
        let target_workspace_name = options
            .target_workspace_name
            .clone()
            .or_else(|| bundle.mission.workspace_name.clone());
        let keep_active = options.keep_automations_active;

        // Remap automation IDs: bundle's automation_id -> freshly minted
        // UUID, so imported automations are distinguishable from the
        // originals and don't collide if the source lives on the same
        // database.
        let mut automation_id_map: HashMap<Uuid, Uuid> = HashMap::new();
        for auto in &bundle.automations {
            automation_id_map.insert(auto.id, Uuid::new_v4());
        }

        let MissionBundle {
            mission,
            events,
            automations,
            executions,
            ..
        } = bundle;

        let conn = self.conn.clone();
        let content_dir = self.content_dir.clone();

        tokio::task::spawn_blocking(move || -> Result<Uuid, String> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction().map_err(|e| e.to_string())?;

            // --- mission row ---
            let mission_mode_str = serde_json::to_value(&mission.mission_mode)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "task".to_string());
            // Normalize statuses that imply an attached runtime session.
            // The bundle clears `session_id` (the CLI-level handles don't
            // travel), so `active`/`pending` would leave the record
            // looking running with nothing behind it — it couldn't be
            // continued and couldn't be resumed via the normal path
            // (which only accepts interrupted/failed/blocked). Rewrite to
            // `interrupted` so the user can explicitly resume it.
            use crate::api::control::MissionStatus;
            let normalized_status = match mission.status {
                MissionStatus::Active | MissionStatus::Pending => MissionStatus::Interrupted,
                other => other,
            };
            let status_str = status_to_string(normalized_status);
            let now = Utc::now().to_rfc3339();
            let imported_title = mission
                .title
                .clone()
                .map(|t| format!("{t} (imported)"))
                .or_else(|| Some("Imported mission".to_string()));
            tx.execute(
                "INSERT INTO missions (id, status, title, short_description, metadata_updated_at, metadata_source, metadata_model, metadata_version, workspace_id, workspace_name, agent, model_override, model_effort, backend, config_profile, created_at, updated_at, interrupted_at, resumable, desktop_sessions, session_id, terminal_reason, parent_mission_id, working_directory, mission_mode, goal_mode, goal_objective)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
                rusqlite::params![
                    new_mission_id.to_string(),
                    status_str,
                    imported_title,
                    mission.short_description,
                    mission.metadata_updated_at,
                    mission.metadata_source,
                    mission.metadata_model,
                    mission.metadata_version,
                    target_workspace_id.to_string(),
                    target_workspace_name,
                    mission.agent,
                    mission.model_override,
                    mission.model_effort,
                    mission.backend,
                    mission.config_profile,
                    mission.created_at,
                    now,
                    mission.interrupted_at,
                    if mission.resumable { 1 } else { 0 },
                    serde_json::to_string(&mission.desktop_sessions).ok(),
                    // Blank the session_id: the CLI-level session files
                    // (.claude/.credentials.json, codex threads) don't
                    // travel with the bundle, so resuming would reuse a
                    // stale handle. Let the import side start fresh on
                    // next turn.
                    Option::<String>::None,
                    mission.terminal_reason,
                    // Skip parent_mission_id remapping — parent probably
                    // doesn't exist on the target side.
                    Option::<String>::None,
                    mission.working_directory,
                    mission_mode_str,
                    if mission.goal_mode { 1i64 } else { 0i64 },
                    mission.goal_objective.clone(),
                ],
            )
            .map_err(|e| format!("Failed to insert mission: {e}"))?;

            // --- events ---
            // Re-spill large content to the target's content_dir so the
            // existing inline/file split logic stays consistent.
            for event in &events {
                let (content_inline, content_file) = SqliteMissionStore::store_content(
                    &content_dir,
                    new_mission_id,
                    event.sequence,
                    &event.event_type,
                    &event.content,
                );
                let metadata_json =
                    serde_json::to_string(&event.metadata).unwrap_or_else(|_| "{}".to_string());
                tx.execute(
                    "INSERT INTO mission_events
                     (mission_id, sequence, event_type, timestamp, event_id, tool_call_id, tool_name, content, content_file, metadata)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        new_mission_id.to_string(),
                        event.sequence,
                        event.event_type,
                        event.timestamp,
                        event.event_id,
                        event.tool_call_id,
                        event.tool_name,
                        content_inline,
                        content_file,
                        metadata_json,
                    ],
                )
                .map_err(|e| format!("Failed to insert event: {e}"))?;
            }

            // --- automations ---
            for auto in &automations {
                let new_auto_id = *automation_id_map
                    .get(&auto.id)
                    .expect("remap table built from this list");
                let (command_source_type, command_source_data) = match &auto.command_source {
                    CommandSource::Library { name } => (
                        "library",
                        serde_json::json!({ "name": name }).to_string(),
                    ),
                    CommandSource::LocalFile { path } => (
                        "local_file",
                        serde_json::json!({ "path": path }).to_string(),
                    ),
                    CommandSource::Inline { content } => (
                        "inline",
                        serde_json::json!({ "content": content }).to_string(),
                    ),
                    CommandSource::NativeLoop {
                        harness,
                        command,
                        args,
                    } => (
                        "native_loop",
                        serde_json::json!({
                            "harness": harness,
                            "command": command,
                            "args": args,
                        })
                        .to_string(),
                    ),
                };
                let (trigger_type, trigger_data) = match &auto.trigger {
                    TriggerType::Interval { seconds } => (
                        "interval",
                        serde_json::json!({ "seconds": seconds }).to_string(),
                    ),
                    TriggerType::Cron {
                        expression,
                        timezone,
                    } => (
                        "cron",
                        serde_json::json!({
                            "expression": expression,
                            "timezone": timezone,
                        })
                        .to_string(),
                    ),
                    TriggerType::Webhook { config } => (
                        "webhook",
                        serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    TriggerType::AgentFinished => ("agent_finished", "{}".to_string()),
                    TriggerType::Telegram { config } => (
                        "telegram",
                        serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string()),
                    ),
                };
                let variables_json = serde_json::to_string(&auto.variables)
                    .unwrap_or_else(|_| "{}".to_string());
                let stop_policy_str = match &auto.stop_policy {
                    StopPolicy::Never => "never".to_string(),
                    StopPolicy::WhenFailingConsecutively { count } => {
                        format!("consecutive_failures:{}", count)
                    }
                    StopPolicy::WhenAllIssuesClosedAndPRsMerged { repo } => {
                        format!("all_issues_closed_and_prs_merged:{}", repo)
                    }
                    StopPolicy::AfterFirstFire => "after_first_fire".to_string(),
                };
                let fresh_session_str = match auto.fresh_session {
                    FreshSession::Always => "always",
                    FreshSession::Switch => "switch",
                    FreshSession::Keep => "keep",
                };
                let driver_str = match auto.driver {
                    super::AutomationDriver::Scheduler => "scheduler",
                    super::AutomationDriver::HarnessLoop => "harness_loop",
                };
                tx.execute(
                    "INSERT INTO automations (id, mission_id, command_source_type, command_source_data,
                                             trigger_type, trigger_data, variables, active, stop_policy,
                                             fresh_session, driver, created_at, last_triggered_at, retry_max_retries,
                                             retry_delay_seconds, retry_backoff_multiplier)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        new_auto_id.to_string(),
                        new_mission_id.to_string(),
                        command_source_type,
                        command_source_data,
                        trigger_type,
                        trigger_data,
                        variables_json,
                        if keep_active && auto.active { 1 } else { 0 },
                        stop_policy_str,
                        fresh_session_str,
                        driver_str,
                        auto.created_at,
                        // Clear last_triggered_at so interval-based automations
                        // get a full interval on the target before firing.
                        Option::<String>::None,
                        auto.retry_config.max_retries as i64,
                        auto.retry_config.retry_delay_seconds as i64,
                        auto.retry_config.backoff_multiplier,
                    ],
                )
                .map_err(|e| format!("Failed to insert automation: {e}"))?;
            }

            // --- automation_executions ---
            for exec in &executions {
                let Some(new_auto_id) = automation_id_map.get(&exec.automation_id) else {
                    // Execution referenced an automation not in the bundle
                    // (shouldn't happen for bundles produced by export, but
                    // stay defensive).
                    continue;
                };
                let status_str = match exec.status {
                    ExecutionStatus::Pending => "pending",
                    ExecutionStatus::Running => "running",
                    ExecutionStatus::Success => "success",
                    ExecutionStatus::Failed => "failed",
                    ExecutionStatus::Cancelled => "cancelled",
                    ExecutionStatus::Skipped => "skipped",
                };
                let variables_json = serde_json::to_string(&exec.variables_used)
                    .unwrap_or_else(|_| "{}".to_string());
                let webhook_payload = exec
                    .webhook_payload
                    .as_ref()
                    .map(|v| v.to_string());
                tx.execute(
                    "INSERT INTO automation_executions (id, automation_id, mission_id, triggered_at,
                                                        trigger_source, status, webhook_payload,
                                                        variables_used, completed_at, error, retry_count)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        Uuid::new_v4().to_string(),
                        new_auto_id.to_string(),
                        new_mission_id.to_string(),
                        exec.triggered_at,
                        exec.trigger_source,
                        status_str,
                        webhook_payload,
                        variables_json,
                        exec.completed_at,
                        exec.error,
                        exec.retry_count as i64,
                    ],
                )
                .map_err(|e| format!("Failed to insert execution: {e}"))?;
            }

            tx.commit().map_err(|e| e.to_string())?;
            Ok(new_mission_id)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    // ---- Task board ------------------------------------------------------

    async fn upsert_board_tasks(
        &self,
        boss_mission_id: Uuid,
        tasks: Vec<NewBoardTask>,
    ) -> Result<Vec<BoardTask>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let now = now_string();
            let mut out = Vec::with_capacity(tasks.len());
            for t in tasks {
                let existing: Option<(String, String)> = conn
                    .query_row(
                        "SELECT id, status FROM board_tasks WHERE boss_mission_id = ?1 AND task_key = ?2",
                        params![boss_mission_id.to_string(), t.task_key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| format!("Failed to look up board task: {e}"))?;

                let depends_on_json = serde_json::to_string(&t.depends_on)
                    .map_err(|e| format!("Failed to serialize depends_on: {e}"))?;

                let row_id = match existing {
                    Some((id, status)) if status == "pending" => {
                        conn.execute(
                            "UPDATE board_tasks SET title = ?1, prompt = ?2, backend = ?3, \
                             model_override = ?4, model_effort = ?5, working_directory = ?6, \
                             depends_on = ?7, updated_at = ?8 WHERE id = ?9",
                            params![
                                t.title,
                                t.prompt,
                                t.backend,
                                t.model_override,
                                t.model_effort,
                                t.working_directory,
                                depends_on_json,
                                now,
                                id,
                            ],
                        )
                        .map_err(|e| format!("Failed to update board task: {e}"))?;
                        id
                    }
                    Some((id, _)) => id, // non-pending: leave untouched
                    None => {
                        let id = Uuid::new_v4().to_string();
                        conn.execute(
                            "INSERT INTO board_tasks (id, boss_mission_id, task_key, title, prompt, \
                             backend, model_override, model_effort, working_directory, depends_on, \
                             status, attempts, created_at, updated_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', 0, ?11, ?11)",
                            params![
                                id,
                                boss_mission_id.to_string(),
                                t.task_key,
                                t.title,
                                t.prompt,
                                t.backend,
                                t.model_override,
                                t.model_effort,
                                t.working_directory,
                                depends_on_json,
                                now,
                            ],
                        )
                        .map_err(|e| format!("Failed to insert board task: {e}"))?;
                        id
                    }
                };

                let task = conn
                    .query_row(
                        &format!("SELECT {BOARD_TASK_COLUMNS} FROM board_tasks WHERE id = ?1"),
                        params![row_id],
                        parse_board_task_row,
                    )
                    .map_err(|e| format!("Failed to re-read board task: {e}"))?;
                out.push(task);
            }
            Ok(out)
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }

    async fn list_board_tasks(&self, boss_mission_id: Uuid) -> Result<Vec<BoardTask>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {BOARD_TASK_COLUMNS} FROM board_tasks \
                     WHERE boss_mission_id = ?1 ORDER BY created_at ASC, task_key ASC"
                ))
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![boss_mission_id.to_string()], parse_board_task_row)
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }

    async fn list_active_board_missions(&self) -> Result<Vec<Uuid>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT boss_mission_id FROM board_tasks \
                     WHERE status IN ('pending', 'running', 'settled')",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                let s = r.map_err(|e| e.to_string())?;
                if let Ok(id) = Uuid::parse_str(&s) {
                    out.push(id);
                }
            }
            Ok(out)
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }

    async fn get_board_task(&self, task_id: Uuid) -> Result<Option<BoardTask>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                &format!("SELECT {BOARD_TASK_COLUMNS} FROM board_tasks WHERE id = ?1"),
                params![task_id.to_string()],
                parse_board_task_row,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }

    async fn get_board_task_by_worker(
        &self,
        worker_mission_id: Uuid,
    ) -> Result<Option<BoardTask>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.query_row(
                &format!(
                    "SELECT {BOARD_TASK_COLUMNS} FROM board_tasks \
                     WHERE worker_mission_id = ?1 ORDER BY updated_at DESC LIMIT 1"
                ),
                params![worker_mission_id.to_string()],
                parse_board_task_row,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }

    async fn save_board_task(&self, task: &BoardTask) -> Result<(), String> {
        let conn = self.conn.clone();
        let t = task.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let depends_on_json = serde_json::to_string(&t.depends_on)
                .map_err(|e| format!("Failed to serialize depends_on: {e}"))?;
            let updated = conn
                .execute(
                    "UPDATE board_tasks SET task_key = ?1, title = ?2, prompt = ?3, backend = ?4, \
                     model_override = ?5, model_effort = ?6, working_directory = ?7, depends_on = ?8, \
                     status = ?9, outcome = ?10, worker_mission_id = ?11, attempts = ?12, \
                     result_digest = ?13, notes = ?14, updated_at = ?15 WHERE id = ?16",
                    params![
                        t.task_key,
                        t.title,
                        t.prompt,
                        t.backend,
                        t.model_override,
                        t.model_effort,
                        t.working_directory,
                        depends_on_json,
                        t.status.to_string(),
                        t.outcome.map(|o| o.to_string()),
                        t.worker_mission_id.map(|id| id.to_string()),
                        t.attempts as i64,
                        t.result_digest,
                        t.notes,
                        now_string(),
                        t.id.to_string(),
                    ],
                )
                .map_err(|e| format!("Failed to save board task: {e}"))?;
            if updated == 0 {
                return Err(format!("Board task {} not found", t.id));
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {e}"))?
    }
}

/// Column list shared by every board task SELECT so `parse_board_task_row`
/// indices stay in sync.
const BOARD_TASK_COLUMNS: &str = "id, boss_mission_id, task_key, title, prompt, backend, \
    model_override, model_effort, working_directory, depends_on, status, outcome, \
    worker_mission_id, attempts, result_digest, notes, created_at, updated_at";

fn parse_board_task_row(row: &rusqlite::Row<'_>) -> Result<BoardTask, rusqlite::Error> {
    let id: String = row.get(0)?;
    let boss_mission_id: String = row.get(1)?;
    let depends_on_json: String = row.get(9)?;
    let status_str: String = row.get(10)?;
    let outcome_str: Option<String> = row.get(11)?;
    let worker_mission_id: Option<String> = row.get(12)?;
    let attempts: i64 = row.get(13)?;
    Ok(BoardTask {
        id: Uuid::parse_str(&id)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        boss_mission_id: Uuid::parse_str(&boss_mission_id)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        task_key: row.get(2)?,
        title: row.get(3)?,
        prompt: row.get(4)?,
        backend: row.get(5)?,
        model_override: row.get(6)?,
        model_effort: row.get(7)?,
        working_directory: row.get(8)?,
        depends_on: serde_json::from_str(&depends_on_json).unwrap_or_default(),
        status: BoardTaskStatus::parse(&status_str).unwrap_or(BoardTaskStatus::Pending),
        outcome: outcome_str.as_deref().and_then(BoardTaskOutcome::parse),
        worker_mission_id: worker_mission_id.and_then(|s| Uuid::parse_str(&s).ok()),
        attempts: attempts as u32,
        result_digest: row.get(14)?,
        notes: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn telegram_user_role_to_str(role: TelegramUserRole) -> &'static str {
    match role {
        TelegramUserRole::Owner => "owner",
        TelegramUserRole::TrustedFriend => "trusted_friend",
        TelegramUserRole::Observer => "observer",
        TelegramUserRole::Blocked => "blocked",
    }
}

fn parse_telegram_user_role(raw: String) -> TelegramUserRole {
    match raw.as_str() {
        "owner" => TelegramUserRole::Owner,
        "trusted_friend" => TelegramUserRole::TrustedFriend,
        "blocked" => TelegramUserRole::Blocked,
        _ => TelegramUserRole::Observer,
    }
}

fn telegram_interest_to_str(level: TelegramMissionInterestLevel) -> &'static str {
    match level {
        TelegramMissionInterestLevel::Muted => "muted",
        TelegramMissionInterestLevel::Normal => "normal",
        TelegramMissionInterestLevel::High => "high",
    }
}

fn parse_telegram_interest(raw: String) -> TelegramMissionInterestLevel {
    match raw.as_str() {
        "muted" => TelegramMissionInterestLevel::Muted,
        "high" => TelegramMissionInterestLevel::High,
        _ => TelegramMissionInterestLevel::Normal,
    }
}

fn row_to_telegram_user(row: &rusqlite::Row<'_>) -> Result<TelegramUser, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let role_str: String = row.get(4)?;
    Ok(TelegramUser {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        username: row.get(2)?,
        display_name: row.get(3)?,
        role: parse_telegram_user_role(role_str),
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn row_to_telegram_user_cursor(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramUserCursor, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    Ok(TelegramUserCursor {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        last_status_at: row.get(2)?,
        last_dashboard_seen_at: row.get(3)?,
        last_alert_ack_at: row.get(4)?,
        last_digest_at: row.get(5)?,
        last_seen_event_sequence_by_mission_json: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_telegram_mission_subscription(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramMissionSubscription, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let mission_id_str: String = row.get(2)?;
    let interest_str: String = row.get(3)?;
    Ok(TelegramMissionSubscription {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        mission_id: Uuid::parse_str(&mission_id_str).unwrap_or_default(),
        interest_level: parse_telegram_interest(interest_str),
        reason: row.get(4)?,
        expires_at: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn row_to_telegram_alert(row: &rusqlite::Row<'_>) -> Result<TelegramAlert, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let mission_id_str: Option<String> = row.get(2)?;
    Ok(TelegramAlert {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        mission_id: mission_id_str
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok()),
        event_kind: row.get(3)?,
        importance: row.get(4)?,
        title: row.get(5)?,
        body: row.get(6)?,
        status: row.get(7)?,
        telegram_message_id: row.get(8)?,
        last_error: row.get(9)?,
        created_at: row.get(10)?,
        sent_at: row.get(11)?,
        acknowledged_at: row.get(12)?,
    })
}

fn row_to_telegram_alert_preference(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramAlertPreference, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let enabled: i32 = row.get(5)?;
    Ok(TelegramAlertPreference {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        scope: row.get(2)?,
        scope_value: row.get(3)?,
        rule_text: row.get(4)?,
        enabled: enabled != 0,
        created_from_message_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// Parse a Telegram channel from a SQLite row.
/// Column order: id(0), mission_id(1), bot_token(2), bot_username(3),
///   allowed_chat_ids(4), trigger_mode(5), active(6), webhook_secret(7),
///   instructions(8), created_at(9), updated_at(10),
///   auto_create_missions(11), default_backend(12), default_model_override(13),
///   default_model_effort(14), default_workspace_id(15), default_config_profile(16),
///   default_agent(17)
fn row_to_telegram_channel(row: &rusqlite::Row<'_>) -> TelegramChannel {
    let id_str: String = row.get(0).unwrap_or_default();
    let mission_id_str: String = row.get(1).unwrap_or_default();
    let allowed_chat_ids_json: String = row.get(4).unwrap_or_else(|_| "[]".to_string());
    let trigger_mode_str: String = row.get(5).unwrap_or_else(|_| "direct_message".to_string());
    let default_ws_str: Option<String> = row.get(15).unwrap_or_default();

    TelegramChannel {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        mission_id: Uuid::parse_str(&mission_id_str).unwrap_or_default(),
        bot_token: row.get(2).unwrap_or_default(),
        bot_username: row.get(3).unwrap_or_default(),
        allowed_chat_ids: serde_json::from_str(&allowed_chat_ids_json).unwrap_or_default(),
        trigger_mode: serde_json::from_value(serde_json::Value::String(trigger_mode_str))
            .unwrap_or_default(),
        active: row.get::<_, i32>(6).unwrap_or(0) != 0,
        webhook_secret: row.get(7).unwrap_or_default(),
        instructions: row.get(8).unwrap_or_default(),
        auto_create_missions: row.get::<_, i32>(11).unwrap_or(0) != 0,
        default_backend: row.get(12).unwrap_or_default(),
        default_model_override: row.get(13).unwrap_or_default(),
        default_model_effort: row.get(14).unwrap_or_default(),
        default_workspace_id: default_ws_str.and_then(|s| Uuid::parse_str(&s).ok()),
        default_config_profile: row.get(16).unwrap_or_default(),
        default_agent: row.get(17).unwrap_or_default(),
        created_at: row.get(9).unwrap_or_default(),
        updated_at: row.get(10).unwrap_or_default(),
    }
}

fn row_to_paloma_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<PalomaDecision> {
    let id_str: String = row.get(0)?;
    let mission_id_str: Option<String> = row.get(2)?;
    let allowed: i32 = row.get(7)?;
    Ok(PalomaDecision {
        id: Uuid::parse_str(&id_str).unwrap_or_default(),
        event_source: row.get(1)?,
        mission_id: mission_id_str.and_then(|id| Uuid::parse_str(&id).ok()),
        user_id: row.get(3)?,
        channel: row.get(4)?,
        reason_code: row.get(5)?,
        proposed_action: row.get(6)?,
        allowed: allowed != 0,
        suppression_reason: row.get(8)?,
        policy_snapshot_json: row.get(9)?,
        generated_text_hash: row.get(10)?,
        generated_text_preview: row.get(11)?,
        created_at: row.get(12)?,
    })
}

fn row_to_paloma_user_preferences(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PalomaUserPreferences> {
    let failure_override: i64 = row.get(6)?;
    Ok(PalomaUserPreferences {
        telegram_user_id: row.get(0)?,
        timezone: row.get(1)?,
        quiet_hours_start: row.get(2)?,
        quiet_hours_end: row.get(3)?,
        max_interrupts_per_hour: row.get(4)?,
        max_interrupts_per_day: row.get(5)?,
        failure_override_quiet: failure_override != 0,
        alert_class_overrides_json: row.get(7)?,
        mission_overrides_json: row.get(8)?,
        digest_cadence: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn row_to_paloma_cooldown_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<PalomaCooldownState> {
    let mission_id_str: String = row.get(0)?;
    Ok(PalomaCooldownState {
        mission_id: Uuid::parse_str(&mission_id_str).unwrap_or_default(),
        alert_class: row.get(1)?,
        telegram_user_id: row.get(2)?,
        last_sent_at: row.get(3)?,
        next_eligible_at: row.get(4)?,
        backoff_step: row.get(5)?,
    })
}

fn row_to_paloma_mission_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<PalomaMissionCard> {
    let mission_id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(2)?;
    let archived: i64 = row.get(9)?;
    Ok(PalomaMissionCard {
        mission_id: Uuid::parse_str(&mission_id_str).unwrap_or_default(),
        telegram_user_id: row.get(1)?,
        channel_id: Uuid::parse_str(&channel_id_str).unwrap_or_default(),
        chat_id: row.get(3)?,
        message_id: row.get(4)?,
        content_hash: row.get(5)?,
        anchor_ts: row.get(6)?,
        last_edit_ts: row.get(7)?,
        version: row.get(8)?,
        archived: archived != 0,
    })
}

fn row_to_paloma_scheduler_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<PalomaSchedulerJob> {
    Ok(PalomaSchedulerJob {
        name: row.get(0)?,
        lease_owner: row.get(1)?,
        lease_expires_at: row.get(2)?,
        last_started_at: row.get(3)?,
        last_finished_at: row.get(4)?,
        last_error: row.get(5)?,
        run_count: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn parse_telegram_scheduled_status(raw: String) -> TelegramScheduledMessageStatus {
    match raw.as_str() {
        "sent" => TelegramScheduledMessageStatus::Sent,
        "failed" => TelegramScheduledMessageStatus::Failed,
        _ => TelegramScheduledMessageStatus::Pending,
    }
}

fn row_to_telegram_scheduled_message(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramScheduledMessage, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(1)?;
    let source_mission_id_str: Option<String> = row.get(2)?;
    let status_str: String = row.get(8)?;

    Ok(TelegramScheduledMessage {
        id: parse_uuid_or_nil(&id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        source_mission_id: source_mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        chat_id: row.get(3)?,
        chat_title: row.get(4)?,
        text: row.get(5)?,
        send_at: row.get(6)?,
        sent_at: row.get(7)?,
        status: parse_telegram_scheduled_status(status_str),
        last_error: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn parse_telegram_structured_memory_kind(raw: String) -> TelegramStructuredMemoryKind {
    match raw.as_str() {
        "fact" => TelegramStructuredMemoryKind::Fact,
        "task" => TelegramStructuredMemoryKind::Task,
        "preference" => TelegramStructuredMemoryKind::Preference,
        _ => TelegramStructuredMemoryKind::Note,
    }
}

fn parse_telegram_structured_memory_scope(raw: String) -> TelegramStructuredMemoryScope {
    match raw.as_str() {
        "user" => TelegramStructuredMemoryScope::User,
        "channel" => TelegramStructuredMemoryScope::Channel,
        _ => TelegramStructuredMemoryScope::Chat,
    }
}

fn parse_telegram_action_execution_kind(raw: String) -> TelegramActionExecutionKind {
    match raw.as_str() {
        "reminder" => TelegramActionExecutionKind::Reminder,
        _ => TelegramActionExecutionKind::Send,
    }
}

fn parse_telegram_action_execution_status(raw: String) -> TelegramActionExecutionStatus {
    match raw.as_str() {
        "sent" => TelegramActionExecutionStatus::Sent,
        "failed" => TelegramActionExecutionStatus::Failed,
        _ => TelegramActionExecutionStatus::Pending,
    }
}

fn parse_telegram_conversation_message_direction(
    raw: String,
) -> TelegramConversationMessageDirection {
    match raw.as_str() {
        "outbound" => TelegramConversationMessageDirection::Outbound,
        _ => TelegramConversationMessageDirection::Inbound,
    }
}

fn parse_telegram_workflow_kind(raw: String) -> TelegramWorkflowKind {
    match raw.as_str() {
        "request_reply" => TelegramWorkflowKind::RequestReply,
        _ => TelegramWorkflowKind::RequestReply,
    }
}

fn parse_telegram_workflow_status(raw: String) -> TelegramWorkflowStatus {
    match raw.as_str() {
        "relayed_to_origin" => TelegramWorkflowStatus::RelayedToOrigin,
        "completed" => TelegramWorkflowStatus::Completed,
        "failed" => TelegramWorkflowStatus::Failed,
        "cancelled" => TelegramWorkflowStatus::Cancelled,
        _ => TelegramWorkflowStatus::WaitingExternal,
    }
}

fn row_to_telegram_structured_memory(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramStructuredMemoryEntry, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(1)?;
    let mission_id_str: Option<String> = row.get(3)?;
    let scope_str: String = row.get(4)?;
    let kind_str: String = row.get(5)?;
    Ok(TelegramStructuredMemoryEntry {
        id: parse_uuid_or_nil(&id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        chat_id: row.get(2)?,
        mission_id: mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        scope: parse_telegram_structured_memory_scope(scope_str),
        kind: parse_telegram_structured_memory_kind(kind_str),
        label: row.get(6)?,
        value: row.get(7)?,
        subject_user_id: row.get(8)?,
        subject_username: row.get(9)?,
        subject_display_name: row.get(10)?,
        source_message_id: row.get(11)?,
        source_role: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn row_to_telegram_memory_search_rank(
    row: &rusqlite::Row<'_>,
) -> Result<(String, f64), rusqlite::Error> {
    Ok((row.get(0)?, row.get(1)?))
}

fn row_to_telegram_action_execution(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramActionExecution, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(1)?;
    let source_mission_id_str: Option<String> = row.get(2)?;
    let action_kind_str: String = row.get(6)?;
    let scheduled_message_id_str: Option<String> = row.get(11)?;
    let status_str: String = row.get(12)?;

    Ok(TelegramActionExecution {
        id: parse_uuid_or_nil(&id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        source_mission_id: source_mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        source_chat_id: row.get(3)?,
        target_chat_id: row.get(4)?,
        target_chat_title: row.get(5)?,
        action_kind: parse_telegram_action_execution_kind(action_kind_str),
        target_kind: row.get(7)?,
        target_value: row.get(8)?,
        text: row.get(9)?,
        delay_seconds: row.get::<_, i64>(10)?.max(0) as u64,
        scheduled_message_id: scheduled_message_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        status: parse_telegram_action_execution_status(status_str),
        last_error: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn row_to_telegram_conversation(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramConversation, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(1)?;
    let mission_id_str: Option<String> = row.get(3)?;
    Ok(TelegramConversation {
        id: parse_uuid_or_nil(&id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        chat_id: row.get(2)?,
        mission_id: mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        chat_title: row.get(4)?,
        chat_type: row.get(5)?,
        last_message_at: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_telegram_conversation_message(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramConversationMessage, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let conversation_id_str: String = row.get(1)?;
    let channel_id_str: String = row.get(2)?;
    let mission_id_str: Option<String> = row.get(4)?;
    let workflow_id_str: Option<String> = row.get(5)?;
    let direction_str: String = row.get(7)?;
    Ok(TelegramConversationMessage {
        id: parse_uuid_or_nil(&id_str),
        conversation_id: parse_uuid_or_nil(&conversation_id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        chat_id: row.get(3)?,
        mission_id: mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        workflow_id: workflow_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        telegram_message_id: row.get(6)?,
        direction: parse_telegram_conversation_message_direction(direction_str),
        role: row.get(8)?,
        sender_user_id: row.get(9)?,
        sender_username: row.get(10)?,
        sender_display_name: row.get(11)?,
        reply_to_message_id: row.get(12)?,
        text: row.get(13)?,
        created_at: row.get(14)?,
    })
}

fn row_to_telegram_workflow(row: &rusqlite::Row<'_>) -> Result<TelegramWorkflow, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let channel_id_str: String = row.get(1)?;
    let origin_conversation_id_str: String = row.get(2)?;
    let origin_mission_id_str: Option<String> = row.get(4)?;
    let target_conversation_id_str: Option<String> = row.get(5)?;
    let kind_str: String = row.get(12)?;
    let status_str: String = row.get(13)?;
    Ok(TelegramWorkflow {
        id: parse_uuid_or_nil(&id_str),
        channel_id: parse_uuid_or_nil(&channel_id_str),
        origin_conversation_id: parse_uuid_or_nil(&origin_conversation_id_str),
        origin_chat_id: row.get(3)?,
        origin_mission_id: origin_mission_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        target_conversation_id: target_conversation_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        target_chat_id: row.get(6)?,
        target_chat_title: row.get(7)?,
        target_chat_type: row.get(8)?,
        target_request_message_id: row.get(9)?,
        initiated_by_user_id: row.get(10)?,
        initiated_by_username: row.get(11)?,
        kind: parse_telegram_workflow_kind(kind_str),
        status: parse_telegram_workflow_status(status_str),
        request_text: row.get(14)?,
        latest_reply_text: row.get(15)?,
        summary: row.get(16)?,
        last_error: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
        completed_at: row.get(20)?,
    })
}

fn row_to_telegram_workflow_event(
    row: &rusqlite::Row<'_>,
) -> Result<TelegramWorkflowEvent, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let workflow_id_str: String = row.get(1)?;
    let conversation_id_str: Option<String> = row.get(2)?;
    Ok(TelegramWorkflowEvent {
        id: parse_uuid_or_nil(&id_str),
        workflow_id: parse_uuid_or_nil(&workflow_id_str),
        conversation_id: conversation_id_str
            .as_deref()
            .map(parse_uuid_or_nil)
            .filter(|id| !id.is_nil()),
        event_type: row.get(3)?,
        payload_json: row.get(4)?,
        created_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{assistant_message_metadata, AssistantMessageMetadataInput, SqliteMissionStore};
    use crate::agents::{
        CompletionConfidence, CompletionEvidence, CompletionSignal, CostSource, TerminalReason,
    };
    use crate::api::control::AgentEvent;
    use crate::api::mission_store::{
        now_string, Automation, AutomationDriver, CommandSource, FreshSession, MissionMode,
        MissionStatus, MissionStore, PalomaCooldownState, PalomaDecision, PalomaMissionCard,
        PalomaSchedulerJob, PalomaUserPreferences, RetryConfig, StopPolicy, TelegramAlert,
        TelegramAlertPreference, TelegramChannel, TelegramConversation,
        TelegramConversationMessage, TelegramConversationMessageDirection,
        TelegramMissionInterestLevel, TelegramMissionSubscription, TelegramStructuredMemoryEntry,
        TelegramStructuredMemoryKind, TelegramStructuredMemoryScope, TelegramTriggerMode,
        TelegramUser, TelegramUserRole, TelegramWorkflow, TelegramWorkflowEvent,
        TelegramWorkflowKind, TelegramWorkflowStatus, TriggerType,
    };
    use crate::cost::TokenUsage;
    use rusqlite::params;
    use serde_json::json;
    use uuid::Uuid;

    fn test_memory_entry(
        channel_id: Uuid,
        chat_id: i64,
        scope: TelegramStructuredMemoryScope,
        kind: TelegramStructuredMemoryKind,
        label: Option<&str>,
        value: &str,
        subject_user_id: Option<i64>,
    ) -> TelegramStructuredMemoryEntry {
        TelegramStructuredMemoryEntry {
            id: Uuid::new_v4(),
            channel_id,
            chat_id,
            mission_id: None,
            scope,
            kind,
            label: label.map(|value| value.to_string()),
            value: value.to_string(),
            subject_user_id,
            subject_username: subject_user_id.map(|_| "th0rgal".to_string()),
            subject_display_name: subject_user_id.map(|_| "@th0rgal".to_string()),
            source_message_id: Some(1),
            source_role: "user".to_string(),
            created_at: "2026-04-08T10:00:00Z".to_string(),
            updated_at: "2026-04-08T10:00:00Z".to_string(),
        }
    }

    async fn create_test_channel(store: &SqliteMissionStore) -> Uuid {
        let mission = store
            .create_mission(
                Some("Telegram memory tests"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");
        let channel = TelegramChannel {
            id: Uuid::new_v4(),
            mission_id: mission.id,
            bot_token: format!("test-token-{}", Uuid::new_v4()),
            bot_username: Some("ana_lfgbot_test".to_string()),
            allowed_chat_ids: vec![],
            trigger_mode: TelegramTriggerMode::MentionOrDm,
            active: true,
            webhook_secret: None,
            instructions: None,
            auto_create_missions: true,
            default_backend: Some("claudecode".to_string()),
            default_model_override: None,
            default_model_effort: None,
            default_workspace_id: None,
            default_config_profile: None,
            default_agent: None,
            created_at: "2026-04-08T10:00:00Z".to_string(),
            updated_at: "2026-04-08T10:00:00Z".to_string(),
        };
        store
            .create_telegram_channel(channel.clone())
            .await
            .expect("telegram channel");
        channel.id
    }

    #[tokio::test]
    async fn telegram_paloma_user_cursor_and_subscription_round_trip() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Paloma state"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let user = store
            .upsert_telegram_user(TelegramUser {
                id: Uuid::new_v4(),
                telegram_user_id: 1_139_694_048,
                username: Some("thomas".to_string()),
                display_name: Some("Thomas".to_string()),
                role: TelegramUserRole::Owner,
                created_at: "2026-05-20T10:00:00Z".to_string(),
                updated_at: "2026-05-20T10:00:00Z".to_string(),
            })
            .await
            .expect("upsert user");
        assert_eq!(user.role, TelegramUserRole::Owner);

        let loaded = store
            .get_telegram_user(1_139_694_048)
            .await
            .expect("get user")
            .expect("user exists");
        assert_eq!(loaded.username.as_deref(), Some("thomas"));

        let cursor = store
            .get_or_create_telegram_user_cursor(1_139_694_048)
            .await
            .expect("cursor");
        assert!(cursor.last_status_at.is_none());
        store
            .update_telegram_user_last_status_at(
                1_139_694_048,
                "2026-05-20T10:10:00Z",
                "{\"mission\":7}",
            )
            .await
            .expect("update cursor");
        let cursor = store
            .get_or_create_telegram_user_cursor(1_139_694_048)
            .await
            .expect("cursor reload");
        assert_eq!(
            cursor.last_status_at.as_deref(),
            Some("2026-05-20T10:10:00Z")
        );
        assert_eq!(
            cursor.last_seen_event_sequence_by_mission_json,
            "{\"mission\":7}"
        );
        store
            .update_telegram_user_last_digest_at(1_139_694_048, "2026-05-20T10:20:00Z")
            .await
            .expect("update digest cursor");
        let cursor = store
            .get_or_create_telegram_user_cursor(1_139_694_048)
            .await
            .expect("cursor after digest update");
        assert_eq!(
            cursor.last_digest_at.as_deref(),
            Some("2026-05-20T10:20:00Z")
        );
        assert_eq!(
            cursor.last_status_at.as_deref(),
            Some("2026-05-20T10:10:00Z")
        );
        assert_eq!(
            cursor.last_seen_event_sequence_by_mission_json,
            "{\"mission\":7}"
        );
        store
            .update_telegram_user_last_alert_ack_at(1_139_694_048, "2026-05-20T10:30:00Z")
            .await
            .expect("update alert ack cursor");
        let cursor = store
            .get_or_create_telegram_user_cursor(1_139_694_048)
            .await
            .expect("cursor after alert ack update");
        assert_eq!(
            cursor.last_alert_ack_at.as_deref(),
            Some("2026-05-20T10:30:00Z")
        );
        assert_eq!(
            cursor.last_digest_at.as_deref(),
            Some("2026-05-20T10:20:00Z")
        );

        store
            .upsert_telegram_mission_subscription(TelegramMissionSubscription {
                id: Uuid::new_v4(),
                telegram_user_id: 1_139_694_048,
                mission_id: mission.id,
                interest_level: TelegramMissionInterestLevel::High,
                reason: Some("status command".to_string()),
                expires_at: None,
                created_at: "2026-05-20T10:00:00Z".to_string(),
                updated_at: "2026-05-20T10:00:00Z".to_string(),
            })
            .await
            .expect("subscription");
        let subscriptions = store
            .list_telegram_mission_subscriptions(1_139_694_048)
            .await
            .expect("subscriptions");
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(
            subscriptions[0].interest_level,
            TelegramMissionInterestLevel::High
        );

        store
            .create_telegram_alert_preference(TelegramAlertPreference {
                id: Uuid::new_v4(),
                telegram_user_id: 1_139_694_048,
                scope: "mission".to_string(),
                scope_value: Some(mission.id.to_string()),
                rule_text: "mute from Telegram feedback".to_string(),
                enabled: true,
                created_from_message_id: Some(12),
                created_at: "2026-05-20T10:00:00Z".to_string(),
                updated_at: "2026-05-20T10:00:00Z".to_string(),
            })
            .await
            .expect("alert preference");
        let preferences = store
            .list_telegram_alert_preferences(1_139_694_048)
            .await
            .expect("alert preferences");
        assert_eq!(preferences.len(), 1);
        assert_eq!(
            preferences[0].rule_text,
            "mute from Telegram feedback".to_string()
        );

        let alert = TelegramAlert {
            id: Uuid::new_v4(),
            telegram_user_id: 1_139_694_048,
            mission_id: Some(mission.id),
            event_kind: "mission_completed".to_string(),
            importance: "high".to_string(),
            title: "Paloma state".to_string(),
            body: "Paloma state is now completed.".to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: None,
            created_at: "2026-05-20T10:00:00Z".to_string(),
            sent_at: None,
            acknowledged_at: None,
        };
        assert!(store
            .create_telegram_alert_if_absent(alert.clone())
            .await
            .expect("first alert")
            .is_some());
        assert!(store
            .create_telegram_alert_if_absent(alert)
            .await
            .expect("duplicate alert")
            .is_none());
        let pending = store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("pending alerts");
        assert_eq!(pending.len(), 1);
        store
            .mark_telegram_alert_failed(pending[0].id, "temporary Telegram outage")
            .await
            .expect("record failed attempt");
        let retryable = store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("retryable alerts");
        assert_eq!(retryable.len(), 1);
        assert_eq!(
            retryable[0].last_error.as_deref(),
            Some("temporary Telegram outage")
        );
        assert_eq!(
            store
                .recover_stale_telegram_alerts("2026-05-20T10:00:30Z", 10)
                .await
                .expect("recover stale alert"),
            1
        );
        let recovered = store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("recovered alerts");
        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].last_error.is_none());
        store
            .mark_telegram_alert_sent(recovered[0].id, Some(99), "2026-05-20T10:01:00Z")
            .await
            .expect("mark sent");
        let sent_alert = store
            .get_telegram_alert_by_message_id(1_139_694_048, 99)
            .await
            .expect("sent alert by message id")
            .expect("sent alert exists");
        assert_eq!(sent_alert.mission_id, Some(mission.id));
        let second_digest_alert_id = Uuid::new_v4();
        store
            .create_telegram_alert_if_absent(TelegramAlert {
                id: second_digest_alert_id,
                telegram_user_id: 1_139_694_048,
                mission_id: Some(mission.id),
                event_kind: "mission_failed".to_string(),
                importance: "high".to_string(),
                title: "Paloma state".to_string(),
                body: "Paloma state failed.".to_string(),
                status: "pending".to_string(),
                telegram_message_id: None,
                last_error: None,
                created_at: "2026-05-20T10:01:30Z".to_string(),
                sent_at: None,
                acknowledged_at: None,
            })
            .await
            .expect("second digest alert");
        store
            .mark_telegram_alert_sent(second_digest_alert_id, Some(99), "2026-05-20T10:01:40Z")
            .await
            .expect("mark second digest alert sent");
        let same_mission_digest_alert = store
            .get_telegram_alert_by_message_id(1_139_694_048, 99)
            .await
            .expect("same-mission digest alert lookup")
            .expect("same-mission digest alert exists");
        assert_eq!(same_mission_digest_alert.mission_id, Some(mission.id));
        let other_mission = store
            .create_mission(
                Some("Other Paloma state"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("other mission");
        let cross_mission_digest_alert_id = Uuid::new_v4();
        store
            .create_telegram_alert_if_absent(TelegramAlert {
                id: cross_mission_digest_alert_id,
                telegram_user_id: 1_139_694_048,
                mission_id: Some(other_mission.id),
                event_kind: "mission_failed".to_string(),
                importance: "high".to_string(),
                title: "Other Paloma state".to_string(),
                body: "Other Paloma state failed.".to_string(),
                status: "pending".to_string(),
                telegram_message_id: None,
                last_error: None,
                created_at: "2026-05-20T10:01:50Z".to_string(),
                sent_at: None,
                acknowledged_at: None,
            })
            .await
            .expect("cross-mission digest alert");
        store
            .mark_telegram_alert_sent(
                cross_mission_digest_alert_id,
                Some(99),
                "2026-05-20T10:02:00Z",
            )
            .await
            .expect("mark cross-mission digest alert sent");
        assert_eq!(
            store
                .get_telegram_alert_by_message_id(1_139_694_048, 99)
                .await
                .expect_err("cross-mission digest alert lookup"),
            "ambiguous_digest_reply"
        );
        assert!(store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("pending alerts after sent")
            .is_empty());

        let queued_after_mute_id = Uuid::new_v4();
        let queued_after_mute = TelegramAlert {
            id: queued_after_mute_id,
            telegram_user_id: 1_139_694_048,
            mission_id: Some(mission.id),
            event_kind: "mission_awaiting_user".to_string(),
            importance: "high".to_string(),
            title: "Paloma state".to_string(),
            body: "Paloma state needs input.".to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: Some("previous retry".to_string()),
            created_at: "2026-05-20T10:02:00Z".to_string(),
            sent_at: None,
            acknowledged_at: None,
        };
        store
            .create_telegram_alert_if_absent(queued_after_mute)
            .await
            .expect("queued alert after mute");
        assert_eq!(
            store
                .acknowledge_pending_telegram_alerts_for_mission(
                    1_139_694_048,
                    mission.id,
                    "2026-05-20T10:03:00Z",
                )
                .await
                .expect("acknowledge pending alerts"),
            1
        );
        assert!(store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("pending alerts after ack")
            .is_empty());
        store
            .mark_telegram_alert_failed(queued_after_mute_id, "late Telegram failure")
            .await
            .expect("late failure should not requeue acknowledged alert");
        assert!(store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("pending alerts after late failure")
            .is_empty());

        let failure_alert_id = Uuid::new_v4();
        let routine_alert_id = Uuid::new_v4();
        let scoped_ack_mission_id = Uuid::new_v4();
        for (id, event_kind, created_at) in [
            (failure_alert_id, "mission_failed", "2026-05-20T10:04:00Z"),
            (
                routine_alert_id,
                "mission_not_feasible",
                "2026-05-20T10:04:01Z",
            ),
        ] {
            store
                .create_telegram_alert_if_absent(TelegramAlert {
                    id,
                    telegram_user_id: 1_139_694_048,
                    mission_id: Some(scoped_ack_mission_id),
                    event_kind: event_kind.to_string(),
                    importance: "high".to_string(),
                    title: "Paloma state".to_string(),
                    body: format!("Paloma state: {event_kind}."),
                    status: "pending".to_string(),
                    telegram_message_id: None,
                    last_error: None,
                    created_at: created_at.to_string(),
                    sent_at: None,
                    acknowledged_at: None,
                })
                .await
                .expect("create scoped ack alert");
        }
        assert!(store
            .acknowledge_pending_telegram_alert(
                1_139_694_048,
                routine_alert_id,
                "2026-05-20T10:05:00Z",
            )
            .await
            .expect("acknowledge one pending alert"));
        let pending_after_single_ack = store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("pending alerts after single ack");
        assert_eq!(pending_after_single_ack.len(), 1);
        assert_eq!(pending_after_single_ack[0].id, failure_alert_id);

        let decision = PalomaDecision {
            id: Uuid::new_v4(),
            event_source: "scheduler".to_string(),
            mission_id: Some(mission.id),
            user_id: Some(1_139_694_048),
            channel: "telegram".to_string(),
            reason_code: "long_running".to_string(),
            proposed_action: "create_alert".to_string(),
            allowed: true,
            suppression_reason: None,
            policy_snapshot_json: serde_json::json!({
                "long_running_minutes": 30,
                "quiet_after_user_message_minutes": 30,
            })
            .to_string(),
            generated_text_hash: Some("abc123".to_string()),
            generated_text_preview: Some("Paloma state is still running.".to_string()),
            created_at: "2026-05-20T10:04:00Z".to_string(),
        };
        store
            .create_paloma_decision(decision.clone())
            .await
            .expect("paloma decision");
        let decisions = store
            .list_paloma_decisions(10)
            .await
            .expect("paloma decisions");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0], decision);

        assert!(store
            .claim_paloma_scheduler_job(
                "paloma_alert_scan",
                "worker-a",
                "2026-05-20T10:05:00Z",
                "2026-05-20T10:06:00Z",
            )
            .await
            .expect("claim scheduler job"));
        assert!(!store
            .claim_paloma_scheduler_job(
                "paloma_alert_scan",
                "worker-b",
                "2026-05-20T10:05:30Z",
                "2026-05-20T10:06:30Z",
            )
            .await
            .expect("blocked by active lease"));
        store
            .finish_paloma_scheduler_job(
                "paloma_alert_scan",
                "worker-a",
                "2026-05-20T10:05:40Z",
                None,
            )
            .await
            .expect("finish scheduler job");
        let jobs: Vec<PalomaSchedulerJob> = store
            .list_paloma_scheduler_jobs()
            .await
            .expect("scheduler jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "paloma_alert_scan");
        assert_eq!(jobs[0].run_count, 1);
        assert_eq!(
            jobs[0].last_finished_at.as_deref(),
            Some("2026-05-20T10:05:40Z")
        );
    }

    #[tokio::test]
    async fn telegram_conversation_upsert_preserves_row_and_lists_latest_first() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;
        let mission = store
            .create_mission(Some("Origin"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let created = store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id,
                chat_id: 101,
                mission_id: Some(mission.id),
                chat_title: Some("Paloma DM".to_string()),
                chat_type: Some("private".to_string()),
                last_message_at: Some("2026-04-08T10:00:00Z".to_string()),
                created_at: "2026-04-08T10:00:00Z".to_string(),
                updated_at: "2026-04-08T10:00:00Z".to_string(),
            })
            .await
            .expect("create conversation");

        let updated = store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id,
                chat_id: 101,
                mission_id: Some(mission.id),
                chat_title: None,
                chat_type: None,
                last_message_at: Some("2026-04-08T10:05:00Z".to_string()),
                created_at: "2026-04-08T10:05:00Z".to_string(),
                updated_at: "2026-04-08T10:05:00Z".to_string(),
            })
            .await
            .expect("update conversation");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.chat_title.as_deref(), Some("Paloma DM"));
        assert_eq!(updated.chat_type.as_deref(), Some("private"));
        assert_eq!(
            updated.last_message_at.as_deref(),
            Some("2026-04-08T10:05:00Z")
        );

        let listed = store
            .list_telegram_conversations(channel_id, 10)
            .await
            .expect("list conversations");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
    }

    #[tokio::test]
    async fn telegram_conversation_messages_round_trip_and_bump_last_message_at() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;
        let mission = store
            .create_mission(Some("Origin"), None, None, None, None, None, None)
            .await
            .expect("mission");
        let conversation = store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id,
                chat_id: 202,
                mission_id: Some(mission.id),
                chat_title: Some("Thread".to_string()),
                chat_type: Some("private".to_string()),
                last_message_at: Some("2026-04-08T10:00:00Z".to_string()),
                created_at: "2026-04-08T10:00:00Z".to_string(),
                updated_at: "2026-04-08T10:00:00Z".to_string(),
            })
            .await
            .expect("conversation");

        store
            .create_telegram_conversation_message(TelegramConversationMessage {
                id: Uuid::new_v4(),
                conversation_id: conversation.id,
                channel_id,
                chat_id: 202,
                mission_id: Some(mission.id),
                workflow_id: None,
                telegram_message_id: Some(11),
                direction: TelegramConversationMessageDirection::Inbound,
                role: "user".to_string(),
                sender_user_id: Some(1),
                sender_username: Some("marilyn".to_string()),
                sender_display_name: Some("Marilyn".to_string()),
                reply_to_message_id: None,
                text: "First".to_string(),
                created_at: "2026-04-08T10:01:00Z".to_string(),
            })
            .await
            .expect("inbound message");

        store
            .create_telegram_conversation_message(TelegramConversationMessage {
                id: Uuid::new_v4(),
                conversation_id: conversation.id,
                channel_id,
                chat_id: 202,
                mission_id: Some(mission.id),
                workflow_id: None,
                telegram_message_id: Some(12),
                direction: TelegramConversationMessageDirection::Outbound,
                role: "assistant".to_string(),
                sender_user_id: None,
                sender_username: Some("ana_lfgbot_test".to_string()),
                sender_display_name: Some("@ana_lfgbot_test".to_string()),
                reply_to_message_id: Some(11),
                text: "Reply".to_string(),
                created_at: "2026-04-08T10:02:00Z".to_string(),
            })
            .await
            .expect("outbound message");

        let messages = store
            .list_telegram_conversation_messages(conversation.id, 10)
            .await
            .expect("list messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "Reply");
        assert_eq!(
            messages[0].direction,
            TelegramConversationMessageDirection::Outbound
        );
        assert_eq!(messages[1].text, "First");

        let refreshed = store
            .get_telegram_conversation_by_chat(channel_id, 202)
            .await
            .expect("get conversation")
            .expect("conversation exists");
        assert_eq!(
            refreshed.last_message_at.as_deref(),
            Some("2026-04-08T10:02:00Z")
        );
    }

    #[tokio::test]
    async fn telegram_workflow_round_trip_supports_pending_lookup_and_events() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;
        let mission = store
            .create_mission(Some("Origin"), None, None, None, None, None, None)
            .await
            .expect("mission");
        let origin = store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id,
                chat_id: 303,
                mission_id: Some(mission.id),
                chat_title: Some("Origin".to_string()),
                chat_type: Some("private".to_string()),
                last_message_at: Some("2026-04-08T10:00:00Z".to_string()),
                created_at: "2026-04-08T10:00:00Z".to_string(),
                updated_at: "2026-04-08T10:00:00Z".to_string(),
            })
            .await
            .expect("origin conversation");
        let target = store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id,
                chat_id: 404,
                mission_id: None,
                chat_title: Some("Marilyn".to_string()),
                chat_type: Some("private".to_string()),
                last_message_at: Some("2026-04-08T10:00:00Z".to_string()),
                created_at: "2026-04-08T10:00:00Z".to_string(),
                updated_at: "2026-04-08T10:00:00Z".to_string(),
            })
            .await
            .expect("target conversation");

        let workflow = store
            .create_telegram_workflow(TelegramWorkflow {
                id: Uuid::new_v4(),
                channel_id,
                origin_conversation_id: origin.id,
                origin_chat_id: 303,
                origin_mission_id: Some(mission.id),
                target_conversation_id: Some(target.id),
                target_chat_id: Some(404),
                target_chat_title: Some("Marilyn".to_string()),
                target_chat_type: Some("private".to_string()),
                target_request_message_id: Some(9001),
                initiated_by_user_id: Some(7),
                initiated_by_username: Some("th0rgal".to_string()),
                kind: TelegramWorkflowKind::RequestReply,
                status: TelegramWorkflowStatus::WaitingExternal,
                request_text: "Ask Marilyn for leads".to_string(),
                latest_reply_text: None,
                summary: None,
                last_error: None,
                created_at: "2026-04-08T10:00:00Z".to_string(),
                updated_at: "2026-04-08T10:00:00Z".to_string(),
                completed_at: None,
            })
            .await
            .expect("create workflow");

        let pending = store
            .get_pending_telegram_workflow_for_target_chat(channel_id, 404)
            .await
            .expect("pending workflow")
            .expect("workflow exists");
        assert_eq!(pending.id, workflow.id);
        assert_eq!(pending.target_request_message_id, Some(9001));

        let pending_by_request = store
            .get_pending_telegram_workflow_for_target_message(channel_id, 404, 9001)
            .await
            .expect("pending workflow by request")
            .expect("workflow exists by request");
        assert_eq!(pending_by_request.id, workflow.id);

        store
            .create_telegram_workflow_event(TelegramWorkflowEvent {
                id: Uuid::new_v4(),
                workflow_id: workflow.id,
                conversation_id: Some(target.id),
                event_type: "external_reply_received".to_string(),
                payload_json: "{\"text\":\"Lead A\"}".to_string(),
                created_at: "2026-04-08T10:03:00Z".to_string(),
            })
            .await
            .expect("create workflow event");

        let mut completed = workflow.clone();
        completed.status = TelegramWorkflowStatus::RelayedToOrigin;
        completed.latest_reply_text = Some("Lead A".to_string());
        completed.summary = Some("Lead A relayed".to_string());
        completed.updated_at = "2026-04-08T10:04:00Z".to_string();
        completed.completed_at = Some("2026-04-08T10:04:00Z".to_string());
        store
            .update_telegram_workflow(completed.clone())
            .await
            .expect("update workflow");

        let pending_after = store
            .get_pending_telegram_workflow_for_target_chat(channel_id, 404)
            .await
            .expect("pending lookup after completion");
        assert!(pending_after.is_none());

        let listed = store
            .list_telegram_workflows(channel_id, 10)
            .await
            .expect("list workflows");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, TelegramWorkflowStatus::RelayedToOrigin);
        assert_eq!(listed[0].summary.as_deref(), Some("Lead A relayed"));

        let events = store
            .list_telegram_workflow_events(workflow.id, 10)
            .await
            .expect("list workflow events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "external_reply_received");
    }

    #[test]
    fn assistant_message_metadata_uses_normalized_cost_shape() {
        let completion_evidence = Some(CompletionEvidence {
            terminal_reason: Some(TerminalReason::Completed),
            completion_signal: CompletionSignal::NativeTerminal,
            completion_confidence: CompletionConfidence::High,
            native_terminal_seen: true,
            pending_tools: None,
            transport_failure_stage: None,
            provider_error_source: None,
            failure_class: None,
            classification_source: "structured".to_string(),
        });
        let metadata = assistant_message_metadata(AssistantMessageMetadataInput {
            success: true,
            cost_cents: 42,
            cost_source: CostSource::Estimated,
            usage: &Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 2,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            }),
            model: &Some("gpt-4o".to_string()),
            model_normalized: &Some("gpt-4o".to_string()),
            shared_files: &None,
            resumable: false,
            completion_evidence: &completion_evidence,
        });

        assert_eq!(
            metadata,
            json!({
                "success": true,
                "cost_cents": 42,
                "cost": {
                    "amount_cents": 42,
                    "currency": "USD",
                    "source": "estimated",
                },
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "cache_creation_input_tokens": null,
                    "cache_read_input_tokens": null,
                },
                "model": "gpt-4o",
                "model_normalized": "gpt-4o",
                "completion_evidence": {
                    "terminal_reason": "Completed",
                    "completion_signal": "native_terminal",
                    "completion_confidence": "high",
                    "native_terminal_seen": true,
                    "pending_tools": null,
                    "transport_failure_stage": null,
                    "provider_error_source": null,
                    "failure_class": null,
                    "classification_source": "structured",
                },
            })
        );
    }

    #[test]
    fn assistant_message_metadata_skips_optional_none_fields() {
        let metadata = assistant_message_metadata(AssistantMessageMetadataInput {
            success: false,
            cost_cents: 0,
            cost_source: CostSource::Unknown,
            usage: &None,
            model: &None,
            model_normalized: &None,
            shared_files: &None,
            resumable: false,
            completion_evidence: &None,
        });

        assert_eq!(
            metadata,
            json!({
                "success": false,
                "cost_cents": 0,
                "cost": {
                    "amount_cents": 0,
                    "currency": "USD",
                    "source": "unknown",
                },
            })
        );
    }

    #[tokio::test]
    async fn update_mission_metadata_is_noop_when_fields_missing() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Renamed")),
                Some(Some("Short summary")),
                Some(Some("backend_heuristic")),
                None,
                Some(Some("v1")),
            )
            .await
            .expect("set metadata");

        let after_set = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let metadata_updated_at = after_set
            .metadata_updated_at
            .clone()
            .expect("metadata timestamp should be set");
        let updated_at = after_set.updated_at.clone();

        store
            .update_mission_metadata(mission.id, None, None, None, None, None)
            .await
            .expect("noop metadata update");

        let after_noop = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");

        assert_eq!(after_noop.title.as_deref(), Some("Renamed"));
        assert_eq!(
            after_noop.short_description.as_deref(),
            Some("Short summary")
        );
        assert_eq!(
            after_noop.metadata_source.as_deref(),
            Some("backend_heuristic")
        );
        assert_eq!(after_noop.metadata_model.as_deref(), None);
        assert_eq!(after_noop.metadata_version.as_deref(), Some("v1"));
        assert_eq!(
            after_noop.metadata_updated_at.as_deref(),
            Some(metadata_updated_at.as_str())
        );
        assert_eq!(after_noop.updated_at, updated_at);
    }

    #[tokio::test]
    async fn update_mission_metadata_can_clear_fields() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Renamed")),
                Some(Some("Short summary")),
                Some(Some("backend_heuristic")),
                None,
                Some(Some("v1")),
            )
            .await
            .expect("set metadata");

        store
            .update_mission_metadata(
                mission.id,
                Some(None),
                Some(None),
                Some(None),
                None,
                Some(None),
            )
            .await
            .expect("clear metadata fields");

        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(mission.title, None);
        assert_eq!(mission.short_description, None);
        assert_eq!(mission.metadata_source, None);
        assert_eq!(mission.metadata_version, None);
    }

    #[tokio::test]
    async fn update_mission_title_marks_user_metadata_source() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Initial"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .update_mission_metadata(
                mission.id,
                None,
                None,
                Some(Some("backend_heuristic")),
                Some(Some("gpt-5")),
                Some(Some("v1")),
            )
            .await
            .expect("seed metadata source");
        let seeded = store
            .get_mission(mission.id)
            .await
            .expect("get seeded mission")
            .expect("mission exists");
        let seeded_metadata_updated_at = seeded
            .metadata_updated_at
            .expect("seed metadata timestamp should exist");

        store
            .update_mission_title(mission.id, "Manual title")
            .await
            .expect("rename mission");

        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(mission.title.as_deref(), Some("Manual title"));
        assert_eq!(mission.metadata_source.as_deref(), Some("user"));
        assert_eq!(mission.metadata_model, None);
        assert_eq!(mission.metadata_version, None);
        let metadata_updated_at = mission
            .metadata_updated_at
            .expect("manual title update should set metadata timestamp");
        assert!(
            metadata_updated_at >= seeded_metadata_updated_at,
            "manual title update should advance metadata timestamp"
        );
    }

    #[tokio::test]
    async fn create_mission_marks_user_metadata_source_when_title_is_provided() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");

        let titled = store
            .create_mission(
                Some("User titled mission"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create titled mission");
        assert_eq!(titled.metadata_source.as_deref(), Some("user"));
        assert!(
            titled.metadata_updated_at.is_some(),
            "titled mission should set metadata_updated_at"
        );

        let untitled = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create untitled mission");
        assert_eq!(untitled.metadata_source, None);
        assert_eq!(untitled.metadata_updated_at, None);

        let blank_titled = store
            .create_mission(Some("  "), None, None, None, None, None, None)
            .await
            .expect("create blank titled mission");
        assert_eq!(blank_titled.metadata_source, None);
        assert_eq!(blank_titled.metadata_updated_at, None);
    }

    #[tokio::test]
    async fn get_total_cost_cents_prefers_normalized_shape_with_legacy_fallback() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Cost mission"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let conn = store.conn.lock().await;
        let query = r#"
            INSERT INTO mission_events (
                mission_id, sequence, event_type, timestamp, metadata
            ) VALUES (?1, ?2, 'assistant_message', ?3, ?4)
        "#;

        conn.execute(
            query,
            params![
                mission.id.to_string(),
                1i64,
                "2026-02-21T00:00:00Z",
                json!({
                    "cost": { "amount_cents": 150 },
                    "cost_cents": 99
                })
                .to_string()
            ],
        )
        .expect("insert normalized + legacy");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                2i64,
                "2026-02-21T00:00:01Z",
                json!({ "cost_cents": 25 }).to_string()
            ],
        )
        .expect("insert legacy");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                3i64,
                "2026-02-21T00:00:02Z",
                json!({ "cost": { "amount_cents": 5 } }).to_string()
            ],
        )
        .expect("insert normalized");
        drop(conn);

        let total = store
            .get_total_cost_cents()
            .await
            .expect("total cost should calculate");
        assert_eq!(total, 180);
    }

    #[tokio::test]
    async fn get_total_cost_cents_clamps_negative_values_to_zero() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Cost mission"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let conn = store.conn.lock().await;
        let query = r#"
            INSERT INTO mission_events (
                mission_id, sequence, event_type, timestamp, metadata
            ) VALUES (?1, ?2, 'assistant_message', ?3, ?4)
        "#;

        conn.execute(
            query,
            params![
                mission.id.to_string(),
                1i64,
                "2026-02-21T00:00:00Z",
                json!({ "cost": { "amount_cents": -50 } }).to_string()
            ],
        )
        .expect("insert malformed negative normalized");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                2i64,
                "2026-02-21T00:00:01Z",
                json!({ "cost_cents": -10 }).to_string()
            ],
        )
        .expect("insert malformed negative legacy");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                3i64,
                "2026-02-21T00:00:02Z",
                json!({ "cost": { "amount_cents": 25 } }).to_string()
            ],
        )
        .expect("insert valid normalized");
        drop(conn);

        let total = store
            .get_total_cost_cents()
            .await
            .expect("total cost should calculate");
        assert_eq!(total, 25);
    }

    #[tokio::test]
    async fn get_cost_by_source_groups_by_provenance() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("Cost source mission"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");

        let conn = store.conn.lock().await;
        let query = r#"
            INSERT INTO mission_events (
                mission_id, sequence, event_type, timestamp, metadata
            ) VALUES (?1, ?2, 'assistant_message', ?3, ?4)
        "#;

        // Actual cost
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                1i64,
                "2026-02-22T00:00:00Z",
                json!({
                    "cost": { "amount_cents": 100, "source": "actual", "currency": "USD" }
                })
                .to_string()
            ],
        )
        .expect("insert actual");

        // Estimated cost
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                2i64,
                "2026-02-22T00:00:01Z",
                json!({
                    "cost": { "amount_cents": 50, "source": "estimated", "currency": "USD" }
                })
                .to_string()
            ],
        )
        .expect("insert estimated");

        // Unknown cost (explicit)
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                3i64,
                "2026-02-22T00:00:02Z",
                json!({
                    "cost": { "amount_cents": 10, "source": "unknown", "currency": "USD" }
                })
                .to_string()
            ],
        )
        .expect("insert unknown");

        // Legacy cost (no source field → unknown)
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                4i64,
                "2026-02-22T00:00:03Z",
                json!({ "cost_cents": 5 }).to_string()
            ],
        )
        .expect("insert legacy");

        drop(conn);

        let (actual, estimated, unknown) = store
            .get_cost_by_source()
            .await
            .expect("cost by source should calculate");
        assert_eq!(actual, 100);
        assert_eq!(estimated, 50);
        // Unknown (10) + legacy (5) both go into the unknown bucket
        assert_eq!(unknown, 15);
    }

    #[tokio::test]
    async fn get_usage_by_model_aggregates_tokens_and_cost_per_model() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("Usage by model mission"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");

        let conn = store.conn.lock().await;
        let query = r#"
            INSERT INTO mission_events (
                mission_id, sequence, event_type, timestamp, metadata
            ) VALUES (?1, ?2, 'assistant_message', ?3, ?4)
        "#;

        // Two calls to claude-3-5-sonnet with usage + cost
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                1i64,
                "2026-04-22T00:00:00Z",
                json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "model_normalized": "claude-3-5-sonnet",
                    "usage": {
                        "input_tokens": 1000,
                        "output_tokens": 500,
                        "cache_creation_input_tokens": 200,
                        "cache_read_input_tokens": 300
                    },
                    "cost": { "amount_cents": 12, "source": "actual" }
                })
                .to_string()
            ],
        )
        .expect("insert sonnet 1");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                2i64,
                "2026-04-22T00:01:00Z",
                json!({
                    "model": "claude-3-5-sonnet-20241022",
                    "model_normalized": "claude-3-5-sonnet",
                    "usage": {
                        "input_tokens": 2000,
                        "output_tokens": 800,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 100
                    },
                    "cost": { "amount_cents": 25, "source": "actual" }
                })
                .to_string()
            ],
        )
        .expect("insert sonnet 2");

        // One call to gpt-4o with usage + cost
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                3i64,
                "2026-04-22T00:02:00Z",
                json!({
                    "model": "gpt-4o-2024-08-06",
                    "model_normalized": "gpt-4o",
                    "usage": {
                        "input_tokens": 500,
                        "output_tokens": 200
                    },
                    "cost": { "amount_cents": 4, "source": "actual" }
                })
                .to_string()
            ],
        )
        .expect("insert gpt-4o");

        // Legacy rows may carry a stale normalized model. Prefer the raw model
        // and recalculate estimated costs with current pricing.
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                4i64,
                "2026-04-22T00:03:00Z",
                json!({
                    "model": "gpt-5.5",
                    "model_normalized": "gpt-5",
                    "usage": {
                        "input_tokens": 10000,
                        "output_tokens": 2000
                    },
                    "cost": { "amount_cents": 33, "source": "estimated" }
                })
                .to_string()
            ],
        )
        .expect("insert legacy gpt-5.5");

        drop(conn);

        let rows = store
            .get_usage_by_model(None)
            .await
            .expect("aggregate by model");

        assert_eq!(rows.len(), 3);
        let sonnet = rows
            .iter()
            .find(|row| row.model == "claude-3-5-sonnet")
            .expect("sonnet usage");
        assert_eq!(sonnet.requests, 2);
        assert_eq!(sonnet.input_tokens, 3000);
        assert_eq!(sonnet.output_tokens, 1300);
        assert_eq!(sonnet.cache_creation_tokens, 200);
        assert_eq!(sonnet.cache_read_tokens, 400);
        assert_eq!(sonnet.cost_cents, 37);

        let gpt_4o = rows
            .iter()
            .find(|row| row.model == "gpt-4o")
            .expect("gpt-4o usage");
        assert_eq!(gpt_4o.requests, 1);
        assert_eq!(gpt_4o.cost_cents, 4);

        let gpt_55 = rows
            .iter()
            .find(|row| row.model == "gpt-5.5")
            .expect("gpt-5.5 usage");
        assert_eq!(gpt_55.requests, 1);
        assert_eq!(gpt_55.input_tokens, 10000);
        assert_eq!(gpt_55.output_tokens, 2000);
        assert_eq!(gpt_55.cost_cents, 11);
        assert!(!rows.iter().any(|row| row.model == "gpt-5"));

        // since=… filter should drop the older entries.
        let recent = store
            .get_usage_by_model(Some("2026-04-22T00:01:30Z"))
            .await
            .expect("filtered aggregate");
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().any(|row| row.model == "gpt-4o"));
        assert!(recent.iter().any(|row| row.model == "gpt-5.5"));
    }

    #[tokio::test]
    async fn get_cost_since_filters_by_timestamp() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("Period cost mission"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");

        let conn = store.conn.lock().await;
        let query = r#"
            INSERT INTO mission_events (
                mission_id, sequence, event_type, timestamp, metadata
            ) VALUES (?1, ?2, 'assistant_message', ?3, ?4)
        "#;

        // Old event — outside the "since" window
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                1i64,
                "2026-02-01T00:00:00Z",
                json!({
                    "cost": { "amount_cents": 200, "source": "actual" }
                })
                .to_string()
            ],
        )
        .expect("insert old actual");

        // Recent events — inside the "since" window
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                2i64,
                "2026-02-20T00:00:00Z",
                json!({
                    "cost": { "amount_cents": 30, "source": "actual" }
                })
                .to_string()
            ],
        )
        .expect("insert recent actual");
        conn.execute(
            query,
            params![
                mission.id.to_string(),
                3i64,
                "2026-02-21T00:00:00Z",
                json!({
                    "cost": { "amount_cents": 15, "source": "estimated" }
                })
                .to_string()
            ],
        )
        .expect("insert recent estimated");

        drop(conn);

        // All-time totals
        let all_total = store.get_total_cost_cents().await.expect("all-time total");
        assert_eq!(all_total, 245); // 200 + 30 + 15

        // Period-filtered: since 2026-02-15 should only include the two recent events
        let since = "2026-02-15T00:00:00Z";
        let period_total = store
            .get_total_cost_cents_since(since)
            .await
            .expect("period total");
        assert_eq!(period_total, 45); // 30 + 15

        let (actual, estimated, unknown) = store
            .get_cost_by_source_since(since)
            .await
            .expect("period cost by source");
        assert_eq!(actual, 30);
        assert_eq!(estimated, 15);
        assert_eq!(unknown, 0);
    }

    #[tokio::test]
    async fn queued_user_messages_are_not_persisted_until_processing_starts() {
        use crate::api::control::AgentEvent;
        use uuid::Uuid;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Queue order"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let queued_id = Uuid::new_v4();
        store
            .log_event(
                mission.id,
                &AgentEvent::UserMessage {
                    id: queued_id,
                    content: "B".to_string(),
                    queued: true,
                    mission_id: Some(mission.id),
                    source: None,
                },
            )
            .await
            .expect("queued event should be ignored");

        let events = store
            .get_events(mission.id, None, None, None)
            .await
            .expect("events");
        assert!(
            events.is_empty(),
            "queued messages should not be stored yet"
        );

        store
            .log_event(
                mission.id,
                &AgentEvent::UserMessage {
                    id: Uuid::new_v4(),
                    content: "A".to_string(),
                    queued: false,
                    mission_id: Some(mission.id),
                    source: None,
                },
            )
            .await
            .expect("first user");
        store
            .log_event(
                mission.id,
                &AgentEvent::AssistantMessage {
                    id: Uuid::new_v4(),
                    content: "reply A".to_string(),
                    success: true,
                    cost_cents: 0,
                    cost_source: CostSource::Unknown,
                    usage: None,
                    model: None,
                    model_normalized: None,
                    mission_id: Some(mission.id),
                    shared_files: None,
                    resumable: false,
                    completion_evidence: None,
                },
            )
            .await
            .expect("reply A");
        store
            .log_event(
                mission.id,
                &AgentEvent::UserMessage {
                    id: queued_id,
                    content: "B".to_string(),
                    queued: false,
                    mission_id: Some(mission.id),
                    source: None,
                },
            )
            .await
            .expect("dequeued B");
        store
            .log_event(
                mission.id,
                &AgentEvent::AssistantMessage {
                    id: Uuid::new_v4(),
                    content: "reply B".to_string(),
                    success: true,
                    cost_cents: 0,
                    cost_source: CostSource::Unknown,
                    usage: None,
                    model: None,
                    model_normalized: None,
                    mission_id: Some(mission.id),
                    shared_files: None,
                    resumable: false,
                    completion_evidence: None,
                },
            )
            .await
            .expect("reply B");

        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let history: Vec<(String, String)> = mission
            .history
            .into_iter()
            .map(|entry| (entry.role, entry.content))
            .collect();
        assert_eq!(
            history,
            vec![
                ("user".to_string(), "A".to_string()),
                ("assistant".to_string(), "reply A".to_string()),
                ("user".to_string(), "B".to_string()),
                ("assistant".to_string(), "reply B".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn user_message_source_is_persisted_in_metadata() {
        use crate::api::control::AgentEvent;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("source attribution"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");

        store
            .log_event(
                mission.id,
                &AgentEvent::UserMessage {
                    id: Uuid::new_v4(),
                    content: "hello".to_string(),
                    queued: false,
                    mission_id: Some(mission.id),
                    source: Some("api:user-123".to_string()),
                },
            )
            .await
            .expect("user message with source");

        let events = store
            .get_events(mission.id, None, None, None)
            .await
            .expect("events");
        let user_event = events
            .iter()
            .find(|e| e.event_type == "user_message")
            .expect("user_message row");
        // The persisted metadata JSON is the forensic record of who/what posted
        // this message — it must carry the attribution source.
        assert_eq!(
            user_event.metadata.get("source").and_then(|v| v.as_str()),
            Some("api:user-123"),
            "persisted metadata should contain the attribution source, got {}",
            user_event.metadata
        );
    }

    #[tokio::test]
    async fn finalized_text_ops_collapse_to_canonical_assistant_row() {
        use crate::api::control::{AgentEvent, TextOp};

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("text ops"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .log_event(
                mission.id,
                &AgentEvent::TextOp {
                    mission_id: mission.id,
                    bubble_id: "bubble-a".to_string(),
                    ops: vec![TextOp::Insert {
                        pos: 0,
                        text: "Hello wrld".to_string(),
                    }],
                },
            )
            .await
            .expect("first op");
        store
            .log_event(
                mission.id,
                &AgentEvent::TextOp {
                    mission_id: mission.id,
                    bubble_id: "bubble-a".to_string(),
                    ops: vec![
                        TextOp::Insert {
                            pos: 7,
                            text: "o".to_string(),
                        },
                        TextOp::Finalize,
                    ],
                },
            )
            .await
            .expect("finalize op");

        let events = store
            .get_events(mission.id, None, None, None)
            .await
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "assistant_message_canonical");
        assert_eq!(events[0].event_id.as_deref(), Some("bubble-a"));
        assert_eq!(events[0].content, "Hello world");
    }

    #[tokio::test]
    async fn hybrid_memory_search_prioritizes_specific_fact_matches() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;

        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                10,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("identifiant prod"),
                "POLARIS-19",
                Some(42),
            ))
            .await
            .expect("user fact");
        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                10,
                TelegramStructuredMemoryScope::Chat,
                TelegramStructuredMemoryKind::Note,
                None,
                "Les logs prod sont sur le serveur principal",
                None,
            ))
            .await
            .expect("chat note");

        let hits = store
            .search_telegram_memory_context_hybrid(
                channel_id,
                10,
                Some(42),
                "Quel est mon identifiant prod ?",
                5,
            )
            .await
            .expect("hybrid search");

        assert!(!hits.is_empty(), "expected at least one hybrid hit");
        assert_eq!(hits[0].entry.label.as_deref(), Some("identifiant prod"));
        assert_eq!(hits[0].entry.value, "POLARIS-19");
        assert!(hits[0].score > hits.last().map(|hit| hit.score).unwrap_or(0.0));
    }

    #[tokio::test]
    async fn paloma_mission_card_roundtrips_through_upsert_touch_and_archive() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Card mission"), None, None, None, None, None, None)
            .await
            .expect("mission");
        let channel_id = create_test_channel(&store).await;

        let initial = PalomaMissionCard {
            mission_id: mission.id,
            telegram_user_id: 1_139_694_048,
            channel_id,
            chat_id: 1_139_694_048,
            message_id: 100,
            content_hash: "hash-v1".to_string(),
            anchor_ts: "2026-05-24T01:00:00Z".to_string(),
            last_edit_ts: "2026-05-24T01:00:00Z".to_string(),
            version: 1,
            archived: false,
        };
        store
            .upsert_paloma_mission_card(initial.clone())
            .await
            .expect("insert card");

        let loaded = store
            .get_paloma_mission_card(mission.id)
            .await
            .expect("get card")
            .expect("card exists");
        assert_eq!(loaded.message_id, 100);
        assert_eq!(loaded.content_hash, "hash-v1");
        assert!(!loaded.archived);

        store
            .touch_paloma_mission_card(mission.id, "hash-v2", "2026-05-24T01:30:00Z")
            .await
            .expect("touch card");
        let touched = store
            .get_paloma_mission_card(mission.id)
            .await
            .expect("get touched")
            .expect("card exists after touch");
        assert_eq!(touched.content_hash, "hash-v2");
        assert_eq!(touched.version, 2);
        // Anchor untouched by `touch_paloma_mission_card`; only re-anchor
        // replaces it.
        assert_eq!(touched.anchor_ts, "2026-05-24T01:00:00Z");

        let active_cards = store
            .list_active_paloma_mission_cards(1_139_694_048)
            .await
            .expect("list cards");
        assert_eq!(active_cards.len(), 1);

        store
            .archive_paloma_mission_card(mission.id)
            .await
            .expect("archive card");
        let after_archive = store
            .list_active_paloma_mission_cards(1_139_694_048)
            .await
            .expect("list cards post-archive");
        assert!(after_archive.is_empty(), "archived cards must be excluded");
        let still_present = store
            .get_paloma_mission_card(mission.id)
            .await
            .expect("get archived")
            .expect("archived row still readable");
        assert!(still_present.archived);
    }

    #[tokio::test]
    async fn paloma_cooldown_state_roundtrips_and_resets_per_mission() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission_a = store
            .create_mission(Some("Mission A"), None, None, None, None, None, None)
            .await
            .expect("mission A");
        let mission_b = store
            .create_mission(Some("Mission B"), None, None, None, None, None, None)
            .await
            .expect("mission B");

        let state_a = PalomaCooldownState {
            mission_id: mission_a.id,
            alert_class: "mission_long_running".to_string(),
            telegram_user_id: 1_139_694_048,
            last_sent_at: "2026-05-24T01:00:00Z".to_string(),
            next_eligible_at: "2026-05-24T01:30:00Z".to_string(),
            backoff_step: 0,
        };
        store
            .upsert_paloma_cooldown_state(state_a.clone())
            .await
            .expect("insert cooldown A");
        let state_b = PalomaCooldownState {
            mission_id: mission_b.id,
            alert_class: "mission_awaiting_user".to_string(),
            telegram_user_id: 1_139_694_048,
            last_sent_at: "2026-05-24T01:00:00Z".to_string(),
            next_eligible_at: "2026-05-24T01:30:00Z".to_string(),
            backoff_step: 0,
        };
        store
            .upsert_paloma_cooldown_state(state_b.clone())
            .await
            .expect("insert cooldown B");

        // Update mission A in-place (same primary key) to bump the step.
        let bumped = PalomaCooldownState {
            backoff_step: 1,
            next_eligible_at: "2026-05-24T03:30:00Z".to_string(),
            ..state_a.clone()
        };
        store
            .upsert_paloma_cooldown_state(bumped.clone())
            .await
            .expect("bump cooldown");
        let reloaded = store
            .get_paloma_cooldown_state(1_139_694_048, mission_a.id, "mission_long_running")
            .await
            .expect("reload cooldown")
            .expect("row present");
        assert_eq!(reloaded.backoff_step, 1);
        assert_eq!(reloaded.next_eligible_at, "2026-05-24T03:30:00Z");

        // Reset wipes mission A only; mission B is untouched.
        store
            .reset_paloma_cooldown_for_mission(mission_a.id)
            .await
            .expect("reset cooldown A");
        let after_reset = store
            .get_paloma_cooldown_state(1_139_694_048, mission_a.id, "mission_long_running")
            .await
            .expect("get post-reset");
        assert!(after_reset.is_none(), "mission A cooldown must be gone");
        let mission_b_still_there = store
            .get_paloma_cooldown_state(1_139_694_048, mission_b.id, "mission_awaiting_user")
            .await
            .expect("get B")
            .expect("B still present");
        assert_eq!(mission_b_still_there.backoff_step, 0);
    }

    #[tokio::test]
    async fn paloma_cooldown_resets_when_mission_status_changes() {
        // Mission status transitions are a meaningful signal change: the
        // user gets a fresh shot at being notified about the new state, so
        // cooldown must be wiped.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("Reset on status change"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");

        store
            .upsert_paloma_cooldown_state(PalomaCooldownState {
                mission_id: mission.id,
                alert_class: "mission_long_running".to_string(),
                telegram_user_id: 1_139_694_048,
                last_sent_at: "2026-05-24T01:00:00Z".to_string(),
                next_eligible_at: "2026-05-24T09:00:00Z".to_string(),
                backoff_step: 2,
            })
            .await
            .expect("insert cooldown");

        store
            .update_mission_status(mission.id, MissionStatus::AwaitingUser)
            .await
            .expect("status change");

        let after = store
            .get_paloma_cooldown_state(1_139_694_048, mission.id, "mission_long_running")
            .await
            .expect("get cooldown after status change");
        assert!(
            after.is_none(),
            "status change must wipe cooldown so the new state alerts immediately"
        );
    }

    #[tokio::test]
    async fn paloma_user_preferences_roundtrip_and_sent_count_window() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");

        assert!(store
            .get_paloma_user_preferences(1_139_694_048)
            .await
            .expect("get prefs")
            .is_none());

        let mut prefs = PalomaUserPreferences::default_for(1_139_694_048, "2026-05-24T00:00:00Z");
        prefs.timezone = "Europe/Paris".to_string();
        prefs.max_interrupts_per_hour = 2;
        store
            .upsert_paloma_user_preferences(prefs.clone())
            .await
            .expect("upsert prefs");
        let loaded = store
            .get_paloma_user_preferences(1_139_694_048)
            .await
            .expect("get prefs after insert")
            .expect("prefs exist");
        assert_eq!(loaded.timezone, "Europe/Paris");
        assert_eq!(loaded.max_interrupts_per_hour, 2);
        assert_eq!(loaded.quiet_hours_start, Some(23));
        assert_eq!(loaded.quiet_hours_end, Some(8));

        // Re-upsert updates fields in place (no duplicates).
        let mut prefs2 = prefs.clone();
        prefs2.max_interrupts_per_hour = 5;
        prefs2.updated_at = "2026-05-24T01:00:00Z".to_string();
        store
            .upsert_paloma_user_preferences(prefs2)
            .await
            .expect("upsert prefs again");
        let reloaded = store
            .get_paloma_user_preferences(1_139_694_048)
            .await
            .expect("get prefs after re-upsert")
            .expect("prefs exist");
        assert_eq!(reloaded.max_interrupts_per_hour, 5);

        // Sent-count window: 0 with no sent alerts.
        let count = store
            .count_paloma_sent_alerts_since(1_139_694_048, "2026-05-24T00:00:00Z")
            .await
            .expect("count");
        assert_eq!(count, 0);

        // Insert a pending alert and mark it sent, then assert it's counted.
        let mission = store
            .create_mission(Some("Counting"), None, None, None, None, None, None)
            .await
            .expect("mission");
        let alert = TelegramAlert {
            id: Uuid::new_v4(),
            telegram_user_id: 1_139_694_048,
            mission_id: Some(mission.id),
            event_kind: "mission_failed:1".to_string(),
            importance: "high".to_string(),
            title: "X".to_string(),
            body: "X failed.".to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: None,
            created_at: "2026-05-24T02:00:00Z".to_string(),
            sent_at: None,
            acknowledged_at: None,
        };
        let inserted = store
            .create_telegram_alert_if_absent(alert)
            .await
            .expect("insert alert")
            .expect("inserted");
        store
            .mark_telegram_alert_sent(inserted.id, Some(42), "2026-05-24T02:01:00Z")
            .await
            .expect("mark sent");

        let count_after = store
            .count_paloma_sent_alerts_since(1_139_694_048, "2026-05-24T00:00:00Z")
            .await
            .expect("count after");
        assert_eq!(count_after, 1);
        // Window that excludes the send time returns 0.
        let count_recent = store
            .count_paloma_sent_alerts_since(1_139_694_048, "2026-05-24T03:00:00Z")
            .await
            .expect("count recent");
        assert_eq!(count_recent, 0);
    }

    #[tokio::test]
    async fn paloma_sent_count_treats_digest_bundle_as_one_message() {
        // Regression: rate limit must count Telegram messages, not the alert
        // rows they bundled. A digest with N alerts shares one
        // telegram_message_id; counting rows would burn the user's hourly
        // budget N× faster than reality.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Bundled"), None, None, None, None, None, None)
            .await
            .expect("mission");

        // Five distinct pending alerts marked sent under the same digest
        // message_id — counts as one interrupt.
        for n in 0..5 {
            let alert = TelegramAlert {
                id: Uuid::new_v4(),
                telegram_user_id: 1_139_694_048,
                mission_id: Some(mission.id),
                event_kind: format!("mission_failed:{n}"),
                importance: "high".to_string(),
                title: "X".to_string(),
                body: "X failed.".to_string(),
                status: "pending".to_string(),
                telegram_message_id: None,
                last_error: None,
                created_at: format!("2026-05-24T01:0{n}:00Z"),
                sent_at: None,
                acknowledged_at: None,
            };
            let inserted = store
                .create_telegram_alert_if_absent(alert)
                .await
                .expect("insert")
                .expect("new row");
            store
                .mark_telegram_alert_sent(inserted.id, Some(777), "2026-05-24T02:00:00Z")
                .await
                .expect("mark sent");
        }
        // A second digest a few minutes later — different message_id.
        let alert = TelegramAlert {
            id: Uuid::new_v4(),
            telegram_user_id: 1_139_694_048,
            mission_id: Some(mission.id),
            event_kind: "mission_long_running".to_string(),
            importance: "normal".to_string(),
            title: "X".to_string(),
            body: "X still running.".to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: None,
            created_at: "2026-05-24T02:30:00Z".to_string(),
            sent_at: None,
            acknowledged_at: None,
        };
        let inserted = store
            .create_telegram_alert_if_absent(alert)
            .await
            .expect("insert second")
            .expect("new row");
        store
            .mark_telegram_alert_sent(inserted.id, Some(778), "2026-05-24T02:30:00Z")
            .await
            .expect("mark sent second");

        let count = store
            .count_paloma_sent_alerts_since(1_139_694_048, "2026-05-24T00:00:00Z")
            .await
            .expect("count");
        // Six alert rows, two distinct Telegram messages → count == 2.
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn paloma_refresh_pending_telegram_alert_body_overwrites_stale_pending_row() {
        // Regression: long-running alerts collapse to one event_kind per
        // mission and `INSERT OR IGNORE` keeps the first body. The refresh
        // helper must update the pending row in place so a 4-hour-stale
        // "running for 0m" body doesn't show up in the digest.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Long runner"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let alert = TelegramAlert {
            id: Uuid::new_v4(),
            telegram_user_id: 1_139_694_048,
            mission_id: Some(mission.id),
            event_kind: "mission_long_running".to_string(),
            importance: "normal".to_string(),
            title: "Long runner".to_string(),
            body: "Long runner is still running.".to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: None,
            created_at: "2026-05-24T01:00:00Z".to_string(),
            sent_at: None,
            acknowledged_at: None,
        };
        store
            .create_telegram_alert_if_absent(alert)
            .await
            .expect("insert")
            .expect("new row");

        let refreshed = store
            .refresh_pending_telegram_alert_body(
                1_139_694_048,
                mission.id,
                "mission_long_running",
                "Long runner",
                "Long runner is still running.\n\nLatest: now active for 4h.",
                "high",
            )
            .await
            .expect("refresh");
        assert!(refreshed, "should report row updated");

        let pending = store
            .list_pending_telegram_alerts(1_139_694_048, 10)
            .await
            .expect("list");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].body.contains("4h"));
        assert_eq!(pending[0].importance, "high");

        // Sent alerts are not refreshed — only pending.
        store
            .mark_telegram_alert_sent(pending[0].id, Some(99), "2026-05-24T05:00:00Z")
            .await
            .expect("mark sent");
        let refreshed2 = store
            .refresh_pending_telegram_alert_body(
                1_139_694_048,
                mission.id,
                "mission_long_running",
                "Long runner",
                "Long runner is still running.\n\nLatest: now 8h.",
                "high",
            )
            .await
            .expect("refresh after sent");
        assert!(!refreshed2, "sent rows must not be refreshed");
    }

    #[tokio::test]
    async fn get_latest_events_returns_newest_n_in_chronological_order() {
        use crate::api::control::AgentEvent;
        // Regression: callers want "what just happened", not the first
        // events of a long-running mission. `get_events(..., limit, 0)`
        // returns oldest-first; `get_latest_events` must return the tail.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Long stream"), None, None, None, None, None, None)
            .await
            .expect("mission");

        for n in 0..50 {
            store
                .log_event(
                    mission.id,
                    &AgentEvent::AssistantMessage {
                        id: Uuid::new_v4(),
                        content: format!("msg-{n}"),
                        success: true,
                        cost_cents: 0,
                        cost_source: CostSource::Unknown,
                        usage: None,
                        model: None,
                        model_normalized: None,
                        mission_id: Some(mission.id),
                        shared_files: None,
                        resumable: false,
                        completion_evidence: None,
                    },
                )
                .await
                .expect("log");
        }

        let latest = store
            .get_latest_events(mission.id, 5)
            .await
            .expect("get_latest_events");
        assert_eq!(latest.len(), 5);
        // Returned ASC, so the *first* element of latest 5 corresponds to
        // event #45 and the last to event #49.
        let contents: Vec<&str> = latest.iter().map(|e| e.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["msg-45", "msg-46", "msg-47", "msg-48", "msg-49"]
        );
    }

    #[tokio::test]
    async fn nth_recent_event_sequence_anchors_on_conversation_messages() {
        use crate::api::control::AgentEvent;
        // The conversation-anchored snapshot needs the sequence of the
        // Nth-most-recent user/assistant message, ignoring the (far more
        // numerous) tool calls in between.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Tool heavy"), None, None, None, None, None, None)
            .await
            .expect("mission");

        let anchor_types = ["user_message", "assistant_message"];

        // No conversation messages yet → None.
        assert_eq!(
            store
                .nth_recent_event_sequence(mission.id, &anchor_types, 1)
                .await
                .expect("query"),
            None
        );

        // Interleave 5 assistant messages with 3 tool calls between each.
        // Record each assistant message's sequence as we go.
        let mut assistant_seqs: Vec<i64> = Vec::new();
        for n in 0..5 {
            store
                .log_event(
                    mission.id,
                    &AgentEvent::AssistantMessage {
                        id: Uuid::new_v4(),
                        content: format!("assistant-{n}"),
                        success: true,
                        cost_cents: 0,
                        cost_source: CostSource::Unknown,
                        usage: None,
                        model: None,
                        model_normalized: None,
                        mission_id: Some(mission.id),
                        shared_files: None,
                        resumable: false,
                        completion_evidence: None,
                    },
                )
                .await
                .expect("log assistant");
            assistant_seqs.push(store.max_event_sequence(mission.id).await.expect("max"));
            for t in 0..3 {
                store
                    .log_event(
                        mission.id,
                        &AgentEvent::ToolCall {
                            tool_call_id: format!("tc-{n}-{t}"),
                            name: "bash".to_string(),
                            args: serde_json::json!({"cmd": "echo hi"}),
                            mission_id: Some(mission.id),
                        },
                    )
                    .await
                    .expect("log tool");
            }
        }

        // 1st-most-recent conversation message = the last assistant (index 4).
        assert_eq!(
            store
                .nth_recent_event_sequence(mission.id, &anchor_types, 1)
                .await
                .expect("query"),
            Some(assistant_seqs[4])
        );
        // 3rd-most-recent = assistant index 2, even though ~6 tool calls
        // sit between it and the head.
        assert_eq!(
            store
                .nth_recent_event_sequence(mission.id, &anchor_types, 3)
                .await
                .expect("query"),
            Some(assistant_seqs[2])
        );
        // Asking for more conversation messages than exist → None.
        assert_eq!(
            store
                .nth_recent_event_sequence(mission.id, &anchor_types, 6)
                .await
                .expect("query"),
            None
        );
    }

    #[tokio::test]
    async fn paloma_cooldown_preserved_when_status_update_is_a_noop() {
        // Regression: heartbeats and internal restarts can re-set the same
        // status. Wiping cooldown in that case would skip the user's
        // exponential backoff.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Heartbeat"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .upsert_paloma_cooldown_state(PalomaCooldownState {
                mission_id: mission.id,
                alert_class: "mission_long_running".to_string(),
                telegram_user_id: 1_139_694_048,
                last_sent_at: "2026-05-24T01:00:00Z".to_string(),
                next_eligible_at: "2026-05-24T09:00:00Z".to_string(),
                backoff_step: 2,
            })
            .await
            .expect("insert cooldown");

        // Mission starts as Pending. Set it Active for real — should wipe.
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("status change");
        let after_real_change = store
            .get_paloma_cooldown_state(1_139_694_048, mission.id, "mission_long_running")
            .await
            .expect("get cooldown");
        assert!(
            after_real_change.is_none(),
            "real status change must wipe cooldown"
        );

        // Re-insert cooldown, then write Active again — must NOT wipe.
        store
            .upsert_paloma_cooldown_state(PalomaCooldownState {
                mission_id: mission.id,
                alert_class: "mission_long_running".to_string(),
                telegram_user_id: 1_139_694_048,
                last_sent_at: "2026-05-24T02:00:00Z".to_string(),
                next_eligible_at: "2026-05-24T10:00:00Z".to_string(),
                backoff_step: 2,
            })
            .await
            .expect("reinsert cooldown");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("no-op status write");
        let after_noop = store
            .get_paloma_cooldown_state(1_139_694_048, mission.id, "mission_long_running")
            .await
            .expect("get cooldown")
            .expect("cooldown still present");
        assert_eq!(
            after_noop.backoff_step, 2,
            "no-op status writes must leave the backoff ladder intact"
        );
    }

    #[tokio::test]
    async fn paloma_memory_consolidation_deletes_older_explicit_rules_and_search_rows() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;
        let older_id = Uuid::new_v4();
        let newer_id = Uuid::new_v4();
        let channel_id_str = channel_id.to_string();

        {
            let conn = store.conn.lock().await;
            for (id, value, updated_at) in [
                (older_id, "tell me everything", "2026-05-20T10:00:00Z"),
                (newer_id, "only failures", "2026-05-20T11:00:00Z"),
            ] {
                conn.execute(
                    "INSERT INTO telegram_structured_memory (
                        id, channel_id, chat_id, mission_id, scope, kind, label, normalized_label,
                        value, subject_user_id, subject_username, subject_display_name,
                        source_message_id, source_role, created_at, updated_at
                     ) VALUES (?1, ?2, 10, NULL, 'user', 'preference', 'alerts', 'alerts',
                        ?3, 42, 'th0rgal', '@th0rgal', 1, 'user',
                        '2026-05-20T09:00:00Z', ?4)",
                    params![id.to_string(), channel_id_str, value, updated_at],
                )
                .expect("insert duplicate memory");
                conn.execute(
                    "INSERT INTO telegram_structured_memory_fts (
                        entry_id, channel_id, chat_id, scope, subject_user_id, search_text
                     ) VALUES (?1, ?2, 10, 'user', 42, ?3)",
                    params![
                        id.to_string(),
                        channel_id_str,
                        format!("preference alerts {}", value)
                    ],
                )
                .expect("insert duplicate memory search row");
            }
        }

        let deleted = store
            .consolidate_telegram_structured_memory(channel_id, 1)
            .await
            .expect("consolidate memory");
        assert_eq!(deleted, 1);

        let entries = store
            .list_telegram_structured_memory(channel_id, Some(10), Some(42), 10)
            .await
            .expect("list memory");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, newer_id);
        assert_eq!(entries[0].value, "only failures");

        let old_hits = store
            .search_telegram_memory_context(channel_id, 10, Some(42), "everything", 10)
            .await
            .expect("search old memory");
        assert!(old_hits.is_empty());
        let new_hits = store
            .search_telegram_memory_context(channel_id, 10, Some(42), "failures", 10)
            .await
            .expect("search new memory");
        assert_eq!(new_hits.len(), 1);
        assert_eq!(new_hits[0].id, newer_id);
    }

    #[tokio::test]
    async fn hybrid_memory_search_keeps_user_scope_across_chat_contexts() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;

        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                1,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("code universel"),
                "ASTRA-42",
                Some(42),
            ))
            .await
            .expect("user fact");

        let hits = store
            .search_telegram_memory_context_hybrid(
                channel_id,
                999,
                Some(42),
                "Quel est mon code universel ?",
                5,
            )
            .await
            .expect("hybrid search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].entry.scope, TelegramStructuredMemoryScope::User);
        assert_eq!(hits[0].entry.value, "ASTRA-42");
    }

    #[tokio::test]
    async fn hybrid_memory_search_reflects_updated_fact_values() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;

        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                1,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("code universel"),
                "ASTRA-42",
                Some(42),
            ))
            .await
            .expect("initial fact");
        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                1,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("code universel"),
                "ASTRA-43",
                Some(42),
            ))
            .await
            .expect("updated fact");

        let hits = store
            .search_telegram_memory_context_hybrid(channel_id, 1, Some(42), "mon code universel", 5)
            .await
            .expect("hybrid search");

        assert_eq!(hits[0].entry.value, "ASTRA-43");
    }

    #[tokio::test]
    async fn telegram_memory_fts_upsert_replaces_existing_entry_in_place() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let channel_id = create_test_channel(&store).await;

        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                1,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("code universel"),
                "ASTRA-42",
                Some(42),
            ))
            .await
            .expect("initial fact");
        store
            .upsert_telegram_structured_memory(test_memory_entry(
                channel_id,
                1,
                TelegramStructuredMemoryScope::User,
                TelegramStructuredMemoryKind::Fact,
                Some("code universel"),
                "ASTRA-43",
                Some(42),
            ))
            .await
            .expect("updated fact");

        let conn = store.conn.lock().await;
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM telegram_structured_memory_fts",
                [],
                |row| row.get(0),
            )
            .expect("fts row count");
        let search_text: String = conn
            .query_row(
                "SELECT search_text FROM telegram_structured_memory_fts LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("fts search text");
        drop(conn);

        assert_eq!(row_count, 1);
        assert!(search_text.contains("astra 43"));
        assert!(!search_text.contains("astra 42"));
    }

    #[tokio::test]
    async fn telegram_memory_migration_bootstraps_empty_fts_index_only_when_needed() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().to_path_buf();
        let channel_id;

        {
            let store = SqliteMissionStore::new(db_path.clone(), "test-user")
                .await
                .expect("sqlite store");
            channel_id = create_test_channel(&store).await;
            store
                .upsert_telegram_structured_memory(test_memory_entry(
                    channel_id,
                    1,
                    TelegramStructuredMemoryScope::User,
                    TelegramStructuredMemoryKind::Fact,
                    Some("code universel"),
                    "ASTRA-42",
                    Some(42),
                ))
                .await
                .expect("user fact");

            let conn = store.conn.lock().await;
            conn.execute("DELETE FROM telegram_structured_memory_fts", [])
                .expect("clear fts rows");
        }

        let store = SqliteMissionStore::new(db_path.clone(), "test-user")
            .await
            .expect("sqlite store reopen");
        let conn = store.conn.lock().await;
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM telegram_structured_memory_fts",
                [],
                |row| row.get(0),
            )
            .expect("fts row count");
        drop(conn);

        assert_eq!(row_count, 1);

        let hits = store
            .search_telegram_memory_context_hybrid(channel_id, 1, Some(42), "code universel", 5)
            .await
            .expect("hybrid search");
        assert_eq!(hits[0].entry.value, "ASTRA-42");
    }

    #[tokio::test]
    async fn telegram_memory_migration_does_not_rebuild_populated_fts_index() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().to_path_buf();

        {
            let store = SqliteMissionStore::new(db_path.clone(), "test-user")
                .await
                .expect("sqlite store");
            let channel_id = create_test_channel(&store).await;
            store
                .upsert_telegram_structured_memory(test_memory_entry(
                    channel_id,
                    1,
                    TelegramStructuredMemoryScope::User,
                    TelegramStructuredMemoryKind::Fact,
                    Some("code universel"),
                    "ASTRA-42",
                    Some(42),
                ))
                .await
                .expect("user fact");

            let conn = store.conn.lock().await;
            conn.execute(
                "UPDATE telegram_structured_memory_fts SET search_text = 'stale marker' WHERE rowid IN (
                    SELECT rowid FROM telegram_structured_memory_fts LIMIT 1
                )",
                [],
            )
            .expect("overwrite fts search text");
        }

        let store = SqliteMissionStore::new(db_path, "test-user")
            .await
            .expect("sqlite store reopen");
        let conn = store.conn.lock().await;
        let search_text: String = conn
            .query_row(
                "SELECT search_text FROM telegram_structured_memory_fts LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("fts search text");
        drop(conn);

        assert_eq!(search_text, "stale marker");
    }

    #[tokio::test]
    async fn get_events_since_returns_only_events_after_seq() {
        use crate::api::control::AgentEvent;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("seq test"), None, None, None, None, None, None)
            .await
            .expect("mission");

        for i in 0..5 {
            store
                .log_event(
                    mission.id,
                    &AgentEvent::UserMessage {
                        id: Uuid::new_v4(),
                        content: format!("msg {i}"),
                        queued: false,
                        mission_id: Some(mission.id),
                        source: None,
                    },
                )
                .await
                .expect("log user message");
            store
                .log_event(
                    mission.id,
                    &AgentEvent::AssistantMessage {
                        id: Uuid::new_v4(),
                        content: format!("reply {i}"),
                        success: true,
                        cost_cents: 0,
                        cost_source: CostSource::Unknown,
                        usage: None,
                        model: None,
                        model_normalized: None,
                        mission_id: Some(mission.id),
                        shared_files: None,
                        resumable: false,
                        completion_evidence: None,
                    },
                )
                .await
                .expect("log assistant");
        }

        // Max sequence should reflect all 10 events
        let max = store.max_event_sequence(mission.id).await.expect("max seq");
        assert_eq!(max, 10);

        // since_seq=0 returns all 10 events, ordered ASC
        let from_zero = store
            .get_events_since(mission.id, 0, None, None)
            .await
            .expect("get_events_since zero");
        assert_eq!(from_zero.len(), 10);
        let seqs: Vec<i64> = from_zero.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, (1..=10).collect::<Vec<_>>());

        // since_seq=5 returns events 6..=10
        let tail = store
            .get_events_since(mission.id, 5, None, None)
            .await
            .expect("get_events_since tail");
        assert_eq!(tail.len(), 5);
        assert_eq!(tail.first().map(|e| e.sequence), Some(6));
        assert_eq!(tail.last().map(|e| e.sequence), Some(10));

        // since_seq=10 returns empty (caller is already caught up)
        let empty = store
            .get_events_since(mission.id, 10, None, None)
            .await
            .expect("get_events_since empty");
        assert!(empty.is_empty());

        // limit clamps the response
        let limited = store
            .get_events_since(mission.id, 0, None, Some(3))
            .await
            .expect("get_events_since limited");
        assert_eq!(limited.len(), 3);
        assert_eq!(
            limited.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        // event_types filter narrows further
        let assistants_only = store
            .get_events_since(mission.id, 0, Some(&["assistant_message"]), None)
            .await
            .expect("get_events_since types");
        assert_eq!(assistants_only.len(), 5);
        for e in &assistants_only {
            assert_eq!(e.event_type, "assistant_message");
        }
    }

    #[tokio::test]
    async fn get_events_before_returns_only_events_below_seq_in_ascending_order() {
        use crate::api::control::AgentEvent;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("before test"), None, None, None, None, None, None)
            .await
            .expect("mission");

        for i in 0..5 {
            store
                .log_event(
                    mission.id,
                    &AgentEvent::UserMessage {
                        id: Uuid::new_v4(),
                        content: format!("msg {i}"),
                        queued: false,
                        mission_id: Some(mission.id),
                        source: None,
                    },
                )
                .await
                .expect("log user message");
            store
                .log_event(
                    mission.id,
                    &AgentEvent::AssistantMessage {
                        id: Uuid::new_v4(),
                        content: format!("reply {i}"),
                        success: true,
                        cost_cents: 0,
                        cost_source: CostSource::Unknown,
                        usage: None,
                        model: None,
                        model_normalized: None,
                        mission_id: Some(mission.id),
                        shared_files: None,
                        resumable: false,
                        completion_evidence: None,
                    },
                )
                .await
                .expect("log assistant");
        }

        // before_seq=11 returns all 10 events, ordered ASC.
        let all = store
            .get_events_before(mission.id, 11, None, None)
            .await
            .expect("get_events_before all");
        assert_eq!(all.len(), 10);
        let seqs: Vec<i64> = all.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, (1..=10).collect::<Vec<_>>());

        // before_seq=6 returns sequences 1..=5, ordered ASC.
        let head = store
            .get_events_before(mission.id, 6, None, None)
            .await
            .expect("get_events_before head");
        assert_eq!(head.len(), 5);
        assert_eq!(head.first().map(|e| e.sequence), Some(1));
        assert_eq!(head.last().map(|e| e.sequence), Some(5));

        // before_seq=1 returns empty (caller already at the start).
        let empty = store
            .get_events_before(mission.id, 1, None, None)
            .await
            .expect("get_events_before empty");
        assert!(empty.is_empty());

        // With limit=3 and before_seq=11 we want the 3 events IMMEDIATELY
        // preceding seq=11 — i.e. seqs 8, 9, 10 — returned in ASC order.
        // This is the contract that drives backwards pagination: each page
        // must be the latest N below the cursor, then sorted oldest-first.
        let last_three = store
            .get_events_before(mission.id, 11, None, Some(3))
            .await
            .expect("get_events_before limited");
        assert_eq!(last_three.len(), 3);
        assert_eq!(
            last_three.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![8, 9, 10]
        );

        // event_types filter narrows further (asks for only the assistant
        // events strictly before seq=11; sequences 2,4,6,8,10).
        let assistants_only = store
            .get_events_before(mission.id, 11, Some(&["assistant_message"]), None)
            .await
            .expect("get_events_before types");
        assert_eq!(assistants_only.len(), 5);
        for e in &assistants_only {
            assert_eq!(e.event_type, "assistant_message");
        }
        assert_eq!(
            assistants_only
                .iter()
                .map(|e| e.sequence)
                .collect::<Vec<_>>(),
            vec![2, 4, 6, 8, 10]
        );
    }

    #[tokio::test]
    async fn max_event_sequence_is_zero_for_mission_with_no_events() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("empty"), None, None, None, None, None, None)
            .await
            .expect("mission");
        let max = store.max_event_sequence(mission.id).await.expect("max seq");
        assert_eq!(max, 0);
    }

    #[tokio::test]
    async fn stale_query_uses_event_timestamp_when_newer_than_updated_at() {
        // Regression: a long agent run that's actively producing tool_call /
        // tool_result events but no assistant turns leaves `mission.updated_at`
        // frozen at the last turn boundary. The stale-cleanup query used to
        // look at `updated_at` alone and would auto-close such missions even
        // though they were demonstrably still active. With the events-aware
        // query, a recent event must keep the mission out of the result set.
        use crate::api::control::AgentEvent;
        use crate::api::mission_store::MissionStatus;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(
                Some("active-with-tool-calls"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("set active");

        // Back-date both metadata and any pre-existing events to 3 hours ago,
        // *then* log a fresh tool_call event so the events tail is current.
        let three_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        store
            .force_backdate_for_test(mission.id, &three_hours_ago)
            .await
            .expect("backdate");

        store
            .log_event(
                mission.id,
                &AgentEvent::ToolCall {
                    tool_call_id: "tc-1".to_string(),
                    name: "bash".to_string(),
                    args: serde_json::json!({"command": "gh run watch 12345"}),
                    mission_id: Some(mission.id),
                },
            )
            .await
            .expect("log fresh tool_call");

        // 2-hour staleness cutoff: even though `updated_at` is 3h old, the
        // fresh event must keep the mission out of the stale set.
        let stale = store
            .get_stale_active_missions(2)
            .await
            .expect("query stale");
        assert!(
            stale.iter().all(|m| m.id != mission.id),
            "mission with a recent tool_call must not be flagged stale (updated_at \
             alone is no longer the stale signal)"
        );
    }

    #[tokio::test]
    async fn stale_query_flags_truly_idle_active_missions() {
        // Counterpart to the regression: a mission whose metadata *and* event
        // tail are both older than the cutoff is genuinely stale.
        use crate::api::mission_store::MissionStatus;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("idle"), None, None, None, None, None, None)
            .await
            .expect("mission");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("set active");
        let three_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        store
            .force_backdate_for_test(mission.id, &three_hours_ago)
            .await
            .expect("backdate");

        let stale = store
            .get_stale_active_missions(2)
            .await
            .expect("query stale");
        assert!(
            stale.iter().any(|m| m.id == mission.id),
            "mission with no recent events and old updated_at must still be flagged"
        );
    }

    #[tokio::test]
    async fn recent_server_shutdown_query_skips_assistant_missions() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");

        let task_mission = store
            .create_mission(Some("task"), None, None, None, None, None, None)
            .await
            .expect("task mission");
        store
            .update_mission_status_with_reason(
                task_mission.id,
                MissionStatus::Interrupted,
                Some("server_shutdown"),
            )
            .await
            .expect("mark task interrupted");

        let assistant_mission = store
            .create_mission(Some("assistant"), None, None, None, None, None, None)
            .await
            .expect("assistant mission");
        store
            .update_mission_mode(assistant_mission.id, MissionMode::Assistant)
            .await
            .expect("set assistant mode");
        store
            .update_mission_status_with_reason(
                assistant_mission.id,
                MissionStatus::Interrupted,
                Some("server_shutdown"),
            )
            .await
            .expect("mark assistant interrupted");

        let mission_ids = store
            .get_recent_server_shutdown_mission_ids(48)
            .await
            .expect("recent server-shutdown missions");

        assert!(mission_ids.contains(&task_mission.id));
        assert!(!mission_ids.contains(&assistant_mission.id));
    }

    #[tokio::test]
    async fn text_delta_latest_update_advances_sequence_for_since_seq_replay() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("streaming"), None, None, None, None, None, None)
            .await
            .expect("mission");

        store
            .log_event(
                mission.id,
                &AgentEvent::TextDelta {
                    content: "first draft".to_string(),
                    mission_id: Some(mission.id),
                },
            )
            .await
            .expect("first text delta");
        let first_seq = store
            .max_event_sequence(mission.id)
            .await
            .expect("first max sequence");

        store
            .log_event(
                mission.id,
                &AgentEvent::ToolCall {
                    tool_call_id: "tool-1".to_string(),
                    name: "bash".to_string(),
                    args: json!({ "command": "true" }),
                    mission_id: Some(mission.id),
                },
            )
            .await
            .expect("tool call");
        let after_tool_seq = store
            .max_event_sequence(mission.id)
            .await
            .expect("tool max sequence");

        store
            .log_event(
                mission.id,
                &AgentEvent::TextDelta {
                    content: "final useful draft".to_string(),
                    mission_id: Some(mission.id),
                },
            )
            .await
            .expect("updated text delta");
        let final_seq = store
            .max_event_sequence(mission.id)
            .await
            .expect("final max sequence");

        assert!(after_tool_seq > first_seq);
        assert!(final_seq > after_tool_seq);

        let replay = store
            .get_events_since(mission.id, after_tool_seq, None, None)
            .await
            .expect("events since tool");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event_type, "text_delta");
        assert_eq!(replay[0].content, "final useful draft");
        assert_eq!(replay[0].sequence, final_seq);
    }

    #[tokio::test]
    async fn stale_query_uses_max_timestamp_not_max_sequence() {
        // Regression for a subtler variant: `log_event` updates an existing
        // row in-place when it sees a duplicate `event_id` (e.g. a
        // `text_delta_latest` row gets its `timestamp` rewritten on every
        // streamed delta), so the row with the highest `sequence` can carry
        // an *older* timestamp than a refreshed lower-sequence row. A query
        // that reads `ORDER BY sequence DESC LIMIT 1` would then miss the
        // refreshed row and falsely flag the mission stale. We force this
        // ordering by hand and confirm `MAX(timestamp)` recovers correctly.
        use crate::api::mission_store::MissionStatus;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("inplace-update"), None, None, None, None, None, None)
            .await
            .expect("mission");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("set active");

        let now = chrono::Utc::now();
        let three_hours_ago = (now - chrono::Duration::hours(3)).to_rfc3339();
        let one_minute_ago = (now - chrono::Duration::minutes(1)).to_rfc3339();
        let mid = mission.id.to_string();

        // Drive raw inserts so we can pin (sequence, timestamp) independently.
        // Highest sequence carries an old timestamp; a lower-sequence row
        // carries the recent one — exactly the in-place-update layout that
        // a sequence-based MAX would mishandle.
        store
            .force_backdate_for_test(mission.id, &three_hours_ago)
            .await
            .expect("backdate metadata");
        let conn = store.conn.clone();
        let three_hours_ago_clone = three_hours_ago.clone();
        let one_minute_ago_clone = one_minute_ago.clone();
        let mid_clone = mid.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            // Lower sequence, recent timestamp (the "refreshed" row)
            conn.execute(
                "INSERT INTO mission_events (mission_id, sequence, event_type, timestamp, content)
                 VALUES (?1, 1, 'text_delta', ?2, 'streaming…')",
                rusqlite::params![mid_clone, one_minute_ago_clone],
            )
            .expect("insert refreshed row");
            // Higher sequence, old timestamp (would be the row picked by ORDER BY sequence DESC)
            conn.execute(
                "INSERT INTO mission_events (mission_id, sequence, event_type, timestamp, content)
                 VALUES (?1, 2, 'tool_call', ?2, 'cmd')",
                rusqlite::params![mid_clone, three_hours_ago_clone],
            )
            .expect("insert old high-seq row");
        })
        .await
        .expect("raw inserts");

        let stale = store
            .get_stale_active_missions(2)
            .await
            .expect("query stale");
        assert!(
            stale.iter().all(|m| m.id != mission.id),
            "mission whose newest activity lives on a *lower* sequence row than \
             the highest sequence must NOT be flagged stale"
        );
    }

    #[tokio::test]
    async fn stale_query_handles_missions_with_no_events() {
        // A freshly-created active mission with no events at all should fall
        // back to `updated_at` (via COALESCE in the query). When `updated_at`
        // is fresh, the mission is not stale.
        use crate::api::mission_store::MissionStatus;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("brand new"), None, None, None, None, None, None)
            .await
            .expect("mission");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("set active");

        let stale = store
            .get_stale_active_missions(2)
            .await
            .expect("query stale");
        assert!(
            stale.iter().all(|m| m.id != mission.id),
            "freshly-created mission with current updated_at and no events \
             must not be flagged stale"
        );
    }

    #[tokio::test]
    async fn migration_purges_inactive_harness_loop_rows_on_reopen() {
        use std::collections::HashMap;

        // Keep the tempdir alive across two `new()` calls so we exercise the
        // migration path on an existing DB.
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().to_path_buf();

        let store = SqliteMissionStore::new(path.clone(), "test-user")
            .await
            .expect("first open");

        let mission = store
            .create_mission(
                Some("goal mission"),
                None,
                None,
                None,
                None,
                Some("codex"),
                None,
            )
            .await
            .expect("mission");

        // Seed: one inactive harness_loop row (the stale UI artifact we want
        // gone), one inactive scheduler row (user-defined, must be kept),
        // and one active harness_loop row (in-progress loop, must be kept).
        let stale_harness = Automation {
            id: Uuid::new_v4(),
            mission_id: mission.id,
            command_source: CommandSource::NativeLoop {
                harness: "codex".to_string(),
                command: "goal".to_string(),
                args: json!({ "objective": "old cycle" }),
            },
            trigger: TriggerType::AgentFinished,
            variables: HashMap::new(),
            active: false,
            created_at: now_string(),
            last_triggered_at: None,
            retry_config: RetryConfig::default(),
            stop_policy: StopPolicy::Never,
            fresh_session: FreshSession::Keep,
            consecutive_failures: 0,
            driver: AutomationDriver::HarnessLoop,
        };
        let preserved_scheduler = Automation {
            id: Uuid::new_v4(),
            mission_id: mission.id,
            command_source: CommandSource::Inline {
                content: "user-paused automation".into(),
            },
            trigger: TriggerType::AgentFinished,
            variables: HashMap::new(),
            active: false,
            created_at: now_string(),
            last_triggered_at: None,
            retry_config: RetryConfig::default(),
            stop_policy: StopPolicy::Never,
            fresh_session: FreshSession::Keep,
            consecutive_failures: 0,
            driver: AutomationDriver::Scheduler,
        };
        let preserved_active_harness = Automation {
            id: Uuid::new_v4(),
            mission_id: mission.id,
            command_source: CommandSource::NativeLoop {
                harness: "codex".to_string(),
                command: "goal".to_string(),
                args: json!({ "objective": "running cycle" }),
            },
            trigger: TriggerType::AgentFinished,
            variables: HashMap::new(),
            active: true,
            created_at: now_string(),
            last_triggered_at: None,
            retry_config: RetryConfig::default(),
            stop_policy: StopPolicy::Never,
            fresh_session: FreshSession::Keep,
            consecutive_failures: 0,
            driver: AutomationDriver::HarnessLoop,
        };
        let stale_id = stale_harness.id;
        let preserved_scheduler_id = preserved_scheduler.id;
        let preserved_active_id = preserved_active_harness.id;
        store
            .create_automation(stale_harness)
            .await
            .expect("seed stale");
        store
            .create_automation(preserved_scheduler)
            .await
            .expect("seed user-paused");
        store
            .create_automation(preserved_active_harness)
            .await
            .expect("seed active");

        // Drop the store to release the SQLite connection, then re-open —
        // this triggers `run_migrations` against the existing schema.
        drop(store);

        let store = SqliteMissionStore::new(path, "test-user")
            .await
            .expect("reopen triggers migration");
        let rows = store
            .get_mission_automations(mission.id)
            .await
            .expect("list");
        let ids: Vec<_> = rows.iter().map(|r| r.id).collect();
        assert!(
            !ids.contains(&stale_id),
            "stale inactive harness_loop row should be purged by migration"
        );
        assert!(
            ids.contains(&preserved_scheduler_id),
            "inactive scheduler row is user-defined and must survive migration"
        );
        assert!(
            ids.contains(&preserved_active_id),
            "active harness_loop row represents an in-progress loop and must survive"
        );
    }

    #[tokio::test]
    async fn proxy_usage_records_and_aggregates() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");

        store
            .record_proxy_usage("gpt-oss-120b", 1_000, 200, 3)
            .await
            .expect("record 1");
        store
            .record_proxy_usage("gpt-oss-120b", 500, 100, 2)
            .await
            .expect("record 2");
        store
            .record_proxy_usage("zai-glm-4.7", 50, 10, 1)
            .await
            .expect("record 3");

        let by_model = store
            .get_proxy_usage_by_model(None)
            .await
            .expect("by model");
        assert_eq!(by_model.len(), 2);
        let gpt = by_model
            .iter()
            .find(|m| m.model == "gpt-oss-120b")
            .expect("gpt row");
        assert_eq!(gpt.requests, 2);
        assert_eq!(gpt.input_tokens, 1_500);
        assert_eq!(gpt.output_tokens, 300);
        assert_eq!(gpt.cost_cents, 5);

        // All three rows were written "now", so they share one day/hour bucket.
        let by_day = store.get_proxy_usage_by_day(None).await.expect("by day");
        assert_eq!(by_day.len(), 1);
        assert_eq!(by_day[0].requests, 3);
        assert_eq!(by_day[0].input_tokens, 1_550);
        assert_eq!(by_day[0].cost_cents, 6);

        let by_hour = store.get_proxy_usage_by_hour(None).await.expect("by hour");
        assert_eq!(by_hour.len(), 1);
        assert_eq!(by_hour[0].requests, 3);
        assert_eq!(by_hour[0].hour.len(), "2026-06-04T10".len());

        // A `since` in the future excludes everything.
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let filtered = store
            .get_proxy_usage_by_model(Some(&future))
            .await
            .expect("filtered");
        assert!(filtered.is_empty());
    }

    #[tokio::test]
    async fn board_tasks_roundtrip() {
        use crate::api::mission_store::{BoardTaskOutcome, BoardTaskStatus, NewBoardTask};
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("store");

        let boss = store
            .create_mission(Some("boss"), None, None, None, None, None, None)
            .await
            .expect("boss mission");

        let new = |key: &str, deps: Vec<&str>| NewBoardTask {
            task_key: key.to_string(),
            title: format!("title-{key}"),
            prompt: format!("prompt-{key}"),
            backend: "codex".to_string(),
            model_override: Some("gpt-5.5".to_string()),
            model_effort: Some("high".to_string()),
            working_directory: Some("/workspaces/x/wt-1".to_string()),
            depends_on: deps.into_iter().map(String::from).collect(),
        };

        let tasks = store
            .upsert_board_tasks(boss.id, vec![new("t1", vec![]), new("t2", vec!["t1"])])
            .await
            .expect("upsert");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].status, BoardTaskStatus::Pending);
        assert_eq!(tasks[1].depends_on, vec!["t1".to_string()]);

        // Active boards includes our boss.
        let active = store.list_active_board_missions().await.expect("active");
        assert_eq!(active, vec![boss.id]);

        // Bind t1 to a worker, settle it, look it up by worker.
        let worker = Uuid::new_v4();
        let mut t1 = tasks[0].clone();
        t1.worker_mission_id = Some(worker);
        t1.status = BoardTaskStatus::Running;
        t1.attempts = 1;
        store.save_board_task(&t1).await.expect("save running");

        let by_worker = store
            .get_board_task_by_worker(worker)
            .await
            .expect("by worker")
            .expect("found");
        assert_eq!(by_worker.task_key, "t1");

        let mut settled = by_worker;
        settled.status = BoardTaskStatus::Settled;
        settled.outcome = Some(BoardTaskOutcome::Success);
        settled.result_digest = Some("did the thing".to_string());
        store.save_board_task(&settled).await.expect("save settled");

        let listed = store.list_board_tasks(boss.id).await.expect("list");
        let t1 = listed.iter().find(|t| t.task_key == "t1").unwrap();
        assert_eq!(t1.status, BoardTaskStatus::Settled);
        assert_eq!(t1.outcome, Some(BoardTaskOutcome::Success));
        assert_eq!(t1.result_digest.as_deref(), Some("did the thing"));

        // Upserting an existing pending task updates it; settled stays.
        let updated = store
            .upsert_board_tasks(boss.id, vec![new("t2", vec![]), new("t1", vec![])])
            .await
            .expect("upsert again");
        let t2 = updated.iter().find(|t| t.task_key == "t2").unwrap();
        assert!(t2.depends_on.is_empty(), "pending task should be updated");
        let t1 = updated.iter().find(|t| t.task_key == "t1").unwrap();
        assert_eq!(
            t1.status,
            BoardTaskStatus::Settled,
            "settled task must not be reset by upsert"
        );

        // get_board_task by id.
        let fetched = store
            .get_board_task(t2.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(fetched.task_key, "t2");
    }

    /// FLEET-001: deferred goals round-trip and the scheduled-pending query
    /// surfaces only Pending missions that carry a goal, with scheduling fields
    /// populated (verifies the SELECT column indices).
    #[tokio::test]
    async fn deferred_goal_and_scheduled_pending_round_trip() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = SqliteMissionStore::new(temp_dir.path().to_path_buf(), "test-user")
            .await
            .expect("sqlite store");
        let mission = store
            .create_mission(Some("Scheduled"), None, None, None, None, None, None)
            .await
            .expect("mission");
        store
            .set_mission_scheduling(
                mission.id,
                &crate::api::mission_store::MissionScheduling {
                    priority: 7,
                    not_before: Some("2099-01-01T00:00:00Z".to_string()),
                    deadline: Some("2099-02-01T00:00:00Z".to_string()),
                },
            )
            .await
            .expect("set scheduling");

        // No goal yet -> not scheduled-pending.
        assert!(store
            .get_scheduled_pending_missions()
            .await
            .expect("list")
            .is_empty());

        store
            .set_deferred_goal(mission.id, Some("run the report".to_string()))
            .await
            .expect("set goal");
        assert_eq!(
            store
                .get_deferred_goal(mission.id)
                .await
                .expect("get")
                .as_deref(),
            Some("run the report")
        );

        let pending = store.get_scheduled_pending_missions().await.expect("list");
        assert_eq!(pending.len(), 1);
        let m = &pending[0];
        assert_eq!(m.id, mission.id);
        assert_eq!(m.scheduling.priority, 7);
        assert_eq!(
            m.scheduling.not_before.as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        assert_eq!(
            m.scheduling.deadline.as_deref(),
            Some("2099-02-01T00:00:00Z")
        );

        // Clearing the goal removes it from the scheduled-pending set.
        store
            .set_deferred_goal(mission.id, None)
            .await
            .expect("clear");
        assert!(store
            .get_deferred_goal(mission.id)
            .await
            .expect("get")
            .is_none());
        assert!(store
            .get_scheduled_pending_missions()
            .await
            .expect("list")
            .is_empty());
    }
}
