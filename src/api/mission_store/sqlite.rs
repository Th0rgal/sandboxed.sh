//! SQLite-based mission store with full event logging.

use super::{
    now_string, sanitize_filename, Automation, AutomationExecution, CommandSource, ExecutionStatus,
    FreshSession, Mission, MissionHistoryEntry, MissionMode, MissionStatus, MissionStore,
    RetryConfig, StopPolicy, StoredEvent, TelegramActionExecution, TelegramActionExecutionKind,
    TelegramActionExecutionStatus, TelegramChannel, TelegramChatMission, TelegramConversation,
    TelegramConversationMessage, TelegramConversationMessageDirection, TelegramScheduledMessage,
    TelegramScheduledMessageStatus, TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind,
    TelegramStructuredMemoryScope, TelegramStructuredMemorySearchHit, TelegramWorkflow,
    TelegramWorkflowEvent, TelegramWorkflowKind, TelegramWorkflowStatus, TriggerType,
    WebhookConfig,
};
use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo};
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
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
    terminal_reason TEXT
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
                ))
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
                ))
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

        Ok(())
    }
}

fn parse_status(s: &str) -> MissionStatus {
    match s {
        "pending" => MissionStatus::Pending,
        "active" => MissionStatus::Active,
        "completed" => MissionStatus::Completed,
        "failed" => MissionStatus::Failed,
        "interrupted" => MissionStatus::Interrupted,
        "blocked" => MissionStatus::Blocked,
        "not_feasible" => MissionStatus::NotFeasible,
        _ => MissionStatus::Pending,
    }
}

fn status_to_string(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::Pending => "pending",
        MissionStatus::Active => "active",
        MissionStatus::Completed => "completed",
        MissionStatus::Failed => "failed",
        MissionStatus::Interrupted => "interrupted",
        MissionStatus::Blocked => "blocked",
        MissionStatus::NotFeasible => "not_feasible",
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
                            COALESCE(goal_mode, 0) as goal_mode, goal_objective
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
                            COALESCE(mission_mode, 'task') as mission_mode, COALESCE(goal_mode, 0) as goal_mode, goal_objective FROM missions WHERE id = ?1",
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
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: Some(session_id.clone()),
            terminal_reason: None,
            parent_mission_id,
            working_directory: working_directory.map(|s| s.to_string()),
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
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
        // Failed missions with LlmError are also resumable (transient API errors)
        let resumable = matches!(
            status,
            MissionStatus::Interrupted | MissionStatus::Blocked | MissionStatus::Failed
        );
        let terminal_reason = terminal_reason.map(|s| s.to_string());

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
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

        // Extract event data
        let (event_type, event_id, tool_call_id, tool_name, content, metadata) = match event {
            AgentEvent::UserMessage {
                id,
                content,
                queued,
                ..
            } => (
                "user_message",
                Some(id.to_string()),
                None,
                None,
                content.clone(),
                serde_json::json!({ "queued": queued }),
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
                    conn.execute(
                        "UPDATE mission_events
                         SET metadata = ?1, timestamp = ?2, content = ?3, content_file = ?4
                         WHERE id = ?5",
                        params![metadata_str, now, content_inline, content_file, row_id],
                    )
                    .map_err(|e| e.to_string())?;
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
    use crate::agents::CostSource;
    use crate::api::mission_store::{
        MissionMode, MissionStatus, MissionStore, TelegramChannel, TelegramConversation,
        TelegramConversationMessage, TelegramConversationMessageDirection,
        TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind, TelegramStructuredMemoryScope,
        TelegramTriggerMode, TelegramWorkflow, TelegramWorkflowEvent, TelegramWorkflowKind,
        TelegramWorkflowStatus,
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
}
