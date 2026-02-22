//! SQLite-based mission store with full event logging.

use super::{
    now_string, sanitize_filename, Automation, AutomationExecution, CommandSource, ExecutionStatus,
    Mission, MissionHistoryEntry, MissionStatus, MissionStore, RetryConfig, StopPolicy,
    StoredEvent, TriggerType, WebhookConfig,
};
use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo};
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

type LegacyAutomationRow = (String, String, String, i64, i64, String, Option<String>);
const COST_CURRENCY_USD: &str = "USD";

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
    stop_policy TEXT NOT NULL DEFAULT 'never',
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
        let created_at: String = row.get(9)?;
        let last_triggered_at: Option<String> = row.get(10)?;
        let retry_max_retries: i64 = row.get(11)?;
        let retry_delay_seconds: i64 = row.get(12)?;
        let retry_backoff_multiplier: f64 = row.get(13)?;

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
            "webhook" => {
                let config: WebhookConfig = serde_json::from_str(&trigger_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                TriggerType::Webhook { config }
            }
            "agent_finished" => TriggerType::AgentFinished,
            _ => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("Unknown trigger type: {}", trigger_type).into(),
                ))
            }
        };

        // Parse variables
        let variables: HashMap<String, String> =
            serde_json::from_str(&variables_json).unwrap_or_default();
        let stop_policy = stop_policy_str.parse().unwrap_or_else(|_| {
            tracing::warn!(
                "Unknown stop_policy '{}' for automation {}; defaulting to 'never'",
                stop_policy_str,
                id
            );
            StopPolicy::Never
        });

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
            created_at,
            last_triggered_at,
            retry_config: RetryConfig {
                max_retries: retry_max_retries as u32,
                retry_delay_seconds: retry_delay_seconds as u64,
                backoff_multiplier: retry_backoff_multiplier,
            },
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
             CREATE INDEX IF NOT EXISTS idx_events_event_type ON mission_events(event_type);",
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

        // Migrate automations table to new schema
        Self::migrate_automations_table(conn)?;
        Self::ensure_automation_indexes(conn)?;

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
                    stop_policy TEXT NOT NULL DEFAULT 'never',
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
                                             created_at, last_triggered_at, retry_max_retries,
                                             retry_delay_seconds, retry_backoff_multiplier)
                     VALUES (?, ?, 'library', ?, 'interval', ?, '{}', ?, 'never', ?, ?, 3, 60, 2.0)",
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
                "ALTER TABLE automations ADD COLUMN stop_policy TEXT NOT NULL DEFAULT 'never'",
                [],
            )
            .map_err(|e| format!("Failed to add stop_policy column: {}", e))?;
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
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            model_effort,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason,
                            config_profile
                     FROM missions
                     ORDER BY updated_at DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(|e| e.to_string())?;

            let missions = stmt
                .query_map(params![limit as i64, offset as i64], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(12)?;
                    let backend: String = row.get(13)?;
                    let session_id: Option<String> = row.get(14)?;
                    let terminal_reason: Option<String> = row.get(15)?;
                    let config_profile: Option<String> = row.get(16)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        model_effort: row.get(7)?,
                        backend,
                        config_profile,
                        history: vec![], // Loaded separately if needed
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                        interrupted_at: row.get(10)?,
                        resumable: row.get::<_, i32>(11)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id,
                        terminal_reason,
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
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            model_effort,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason,
                            config_profile
                     FROM missions WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;

            let mission: Option<Mission> = stmt
                .query_row(params![&id_str], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(12)?;
                    let backend: String = row.get(13)?;
                    let session_id: Option<String> = row.get(14)?;
                    let terminal_reason: Option<String> = row.get(15)?;
                    let config_profile: Option<String> = row.get(16)?;

                    Ok(Mission {
                        id: parse_uuid_or_nil(&id_str),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        model_effort: row.get(7)?,
                        backend,
                        config_profile,
                        history: vec![],
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                        interrupted_at: row.get(10)?,
                        resumable: row.get::<_, i32>(11)? != 0,
                        desktop_sessions: desktop_sessions_json
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        session_id,
                        terminal_reason,
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
        let conn = self.conn.clone();
        let now = now_string();
        let id = Uuid::new_v4();
        let workspace_id = workspace_id.unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID);
        let backend = backend.unwrap_or("claudecode").to_string();
        // Generate session_id for conversation persistence (used by Claude Code --session-id)
        let session_id = Uuid::new_v4().to_string();

        let mission = Mission {
            id,
            status: MissionStatus::Pending,
            title: title.map(|s| s.to_string()),
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
        };

        let m = mission.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO missions (id, status, title, workspace_id, agent, model_override, model_effort, backend, config_profile, created_at, updated_at, resumable, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    m.id.to_string(),
                    status_to_string(m.status),
                    m.title,
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
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| e.to_string())??;

        Ok(mission)
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

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE missions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, now, id.to_string()],
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
                    "SELECT m.id FROM missions m
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

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, status, title, workspace_id, workspace_name, agent, model_override,
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend
                     FROM missions
                     WHERE status = 'active' AND updated_at < ?1",
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
                            COALESCE(backend, 'opencode') as backend
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
            // Skip events that are less important for debugging
            AgentEvent::Status { .. }
            | AgentEvent::AgentPhase { .. }
            | AgentEvent::AgentTree { .. }
            | AgentEvent::Progress { .. }
            | AgentEvent::SessionIdUpdate { .. }
            | AgentEvent::MissionActivity { .. }
            | AgentEvent::MissionTitleChanged { .. } => return Ok(()),
        };

        let event_type = event_type.to_string();
        let metadata_str = metadata.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();

            // If this event has an event_id that already exists for this mission,
            // update the existing row's metadata instead of inserting a duplicate.
            // This happens when a queued UserMessage is re-emitted with queued: false.
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
        };

        // Serialize trigger
        let (trigger_type, trigger_data) = match &automation.trigger {
            TriggerType::Interval { seconds } => (
                "interval",
                serde_json::json!({ "seconds": seconds }).to_string(),
            ),
            TriggerType::Webhook { config } => (
                "webhook",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
            TriggerType::AgentFinished => ("agent_finished", "{}".to_string()),
        };

        // Serialize variables
        let variables_json =
            serde_json::to_string(&automation.variables).map_err(|e| e.to_string())?;

        let a = automation.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO automations (id, mission_id, command_source_type, command_source_data,
                                         trigger_type, trigger_data, variables, active, stop_policy,
                                         created_at, last_triggered_at, retry_max_retries,
                                         retry_delay_seconds, retry_backoff_multiplier)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    a.id.to_string(),
                    a.mission_id.to_string(),
                    command_source_type,
                    command_source_data,
                    trigger_type,
                    trigger_data,
                    variables_json,
                    if a.active { 1 } else { 0 },
                    a.stop_policy.as_db_str(),
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
                                trigger_type, trigger_data, variables, active, stop_policy, created_at, last_triggered_at,
                                retry_max_retries, retry_delay_seconds, retry_backoff_multiplier
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
                            trigger_type, trigger_data, variables, active, stop_policy, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier
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
                            trigger_type, trigger_data, variables, active, stop_policy, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier
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
        };

        // Serialize trigger
        let (trigger_type, trigger_data) = match &automation.trigger {
            TriggerType::Interval { seconds } => (
                "interval",
                serde_json::json!({ "seconds": seconds }).to_string(),
            ),
            TriggerType::Webhook { config } => (
                "webhook",
                serde_json::to_string(config).map_err(|e| e.to_string())?,
            ),
            TriggerType::AgentFinished => ("agent_finished", "{}".to_string()),
        };

        // Serialize variables
        let variables_json =
            serde_json::to_string(&automation.variables).map_err(|e| e.to_string())?;

        tokio::task::spawn_blocking(move || {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE automations SET command_source_type = ?, command_source_data = ?,
                                       trigger_type = ?, trigger_data = ?, variables = ?, active = ?,
                                       stop_policy = ?, last_triggered_at = ?, retry_max_retries = ?, retry_delay_seconds = ?,
                                       retry_backoff_multiplier = ?
                 WHERE id = ?",
                params![
                    command_source_type,
                    command_source_data,
                    trigger_type,
                    trigger_data,
                    variables_json,
                    if automation.active { 1 } else { 0 },
                    automation.stop_policy.as_db_str(),
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
                            trigger_type, trigger_data, variables, active, stop_policy, created_at, last_triggered_at,
                            retry_max_retries, retry_delay_seconds, retry_backoff_multiplier
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
}

#[cfg(test)]
mod tests {
    use super::{assistant_message_metadata, AssistantMessageMetadataInput, SqliteMissionStore};
    use crate::agents::CostSource;
    use crate::api::mission_store::MissionStore;
    use crate::cost::TokenUsage;
    use rusqlite::params;
    use serde_json::json;

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
}
