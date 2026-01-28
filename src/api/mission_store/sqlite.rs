//! SQLite-based mission store with full event logging.

use super::{
    now_string, sanitize_filename, Mission, MissionHistoryEntry, MissionStatus, MissionStore,
    StoredEvent,
};
use crate::api::control::{AgentEvent, AgentTreeNode, DesktopSessionInfo};
use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

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
    backend TEXT NOT NULL DEFAULT 'opencode',
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
"#;

/// Content size threshold for inline storage (64KB).
const CONTENT_SIZE_THRESHOLD: usize = 64 * 1024;

pub struct SqliteMissionStore {
    conn: Arc<Mutex<Connection>>,
    content_dir: PathBuf,
}

impl SqliteMissionStore {
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
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason
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
                    let desktop_sessions_json: Option<String> = row.get(11)?;
                    let backend: String = row.get(12)?;
                    let session_id: Option<String> = row.get(13)?;
                    let terminal_reason: Option<String> = row.get(14)?;

                    Ok(Mission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        backend,
                        history: vec![], // Loaded separately if needed
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        interrupted_at: row.get(9)?,
                        resumable: row.get::<_, i32>(10)? != 0,
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
                            created_at, updated_at, interrupted_at, resumable, desktop_sessions,
                            COALESCE(backend, 'opencode') as backend, session_id, terminal_reason
                     FROM missions WHERE id = ?1",
                )
                .map_err(|e| e.to_string())?;

            let mission: Option<Mission> = stmt
                .query_row(params![&id_str], |row| {
                    let id_str: String = row.get(0)?;
                    let status_str: String = row.get(1)?;
                    let workspace_id_str: String = row.get(3)?;
                    let desktop_sessions_json: Option<String> = row.get(11)?;
                    let backend: String = row.get(12)?;
                    let session_id: Option<String> = row.get(13)?;
                    let terminal_reason: Option<String> = row.get(14)?;

                    Ok(Mission {
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        backend,
                        history: vec![],
                        created_at: row.get(7)?,
                        updated_at: row.get(8)?,
                        interrupted_at: row.get(9)?,
                        resumable: row.get::<_, i32>(10)? != 0,
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
        backend: Option<&str>,
    ) -> Result<Mission, String> {
        let conn = self.conn.clone();
        let now = now_string();
        let id = Uuid::new_v4();
        let workspace_id = workspace_id.unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID);
        let backend = backend.unwrap_or("opencode").to_string();
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
            backend: backend.clone(),
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
                "INSERT INTO missions (id, status, title, workspace_id, agent, model_override, backend, created_at, updated_at, resumable, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    m.id.to_string(),
                    status_to_string(m.status),
                    m.title,
                    m.workspace_id.to_string(),
                    m.agent,
                    m.model_override,
                    m.backend,
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
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        backend,
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
                        id: Uuid::parse_str(&id_str).unwrap_or_default(),
                        status: parse_status(&status_str),
                        title: row.get(2)?,
                        workspace_id: Uuid::parse_str(&workspace_id_str)
                            .unwrap_or(crate::workspace::DEFAULT_WORKSPACE_ID),
                        workspace_name: row.get(4)?,
                        agent: row.get(5)?,
                        model_override: row.get(6)?,
                        backend,
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
                model,
                shared_files,
                resumable,
                ..
            } => (
                "assistant_message",
                Some(id.to_string()),
                None,
                None,
                content.clone(),
                serde_json::json!({
                    "success": success,
                    "cost_cents": cost_cents,
                    "model": model,
                    "shared_files": shared_files,
                    "resumable": resumable,
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
            | AgentEvent::TextDelta { .. }
            | AgentEvent::MissionActivity { .. } => return Ok(()),
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
                    conn.execute(
                        "UPDATE mission_events SET metadata = ?1, timestamp = ?2 WHERE id = ?3",
                        params![metadata_str, now, row_id],
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
                    mission_id: Uuid::parse_str(&mid_str).unwrap_or_default(),
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

        // Use SQLite JSON1 extension to extract cost_cents from metadata
        // and sum across all assistant_message events
        let query = r#"
            SELECT COALESCE(
                SUM(
                    CAST(
                        COALESCE(json_extract(metadata, '$.cost_cents'), 0) AS INTEGER
                    )
                ),
                0
            ) as total_cost
            FROM mission_events
            WHERE event_type = 'assistant_message'
        "#;

        let total: i64 = conn
            .query_row(query, [], |row| row.get(0))
            .map_err(|e| e.to_string())?;

        Ok(total as u64)
    }
}
