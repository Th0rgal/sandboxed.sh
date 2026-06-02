//! Persistent storage for the Ask assistant (the non-interrupting sidecar
//! co-pilot). Deliberately separate from the mission store / `mission_events`
//! so Ask conversations never enter the working agent's prompt context.
//!
//! Backed by its own SQLite database (`ask.db`) alongside the mission stores.
//! Mission ownership / access control is enforced at the HTTP layer (the caller
//! verifies the user owns the mission via their per-user mission store), so this
//! store is keyed purely by `mission_id` and shared across users.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

/// A conversation thread between the operator and the Ask assistant, scoped to
/// one mission. A mission may have many threads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskThread {
    pub id: Uuid,
    pub mission_id: Uuid,
    pub title: Option<String>,
    /// Model used for this thread (snapshot of the picker at create time).
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One message in an Ask thread: an operator prompt, an assistant reply, or a
/// tool call / result emitted during the agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskMessage {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub seq: i64,
    /// `user` | `assistant` | `tool_call` | `tool_result`
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

/// A passive note queued by the Ask lane to be flushed into the working agent's
/// next turn (the operator-note bridge — see M2). Lives at mission scope, not
/// inside an Ask thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorNote {
    pub id: Uuid,
    pub mission_id: Uuid,
    pub body: String,
    pub source_thread_id: Option<Uuid>,
    pub created_at: String,
}

/// SQLite-backed Ask store.
pub struct AskStore {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for AskStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskStore").finish()
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

impl AskStore {
    /// Open (and create/migrate) the Ask database at `db_path`.
    pub async fn open(db_path: PathBuf) -> Result<Self, String> {
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, String> {
            let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(|e| e.to_string())?;
            conn.pragma_update(None, "foreign_keys", "ON")
                .map_err(|e| e.to_string())?;
            conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
            Ok(conn)
        })
        .await
        .map_err(|e| e.to_string())??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Threads ──────────────────────────────────────────────────────────────

    pub async fn create_thread(
        &self,
        mission_id: Uuid,
        model: Option<String>,
    ) -> Result<AskThread, String> {
        let conn = self.conn.clone();
        let thread = AskThread {
            id: Uuid::new_v4(),
            mission_id,
            title: None,
            model,
            created_at: now(),
            updated_at: now(),
        };
        let t = thread.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO ask_threads (id, mission_id, title, model, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    t.id.to_string(),
                    t.mission_id.to_string(),
                    t.title,
                    t.model,
                    t.created_at,
                    t.updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(thread)
    }

    pub async fn list_threads(&self, mission_id: Uuid) -> Result<Vec<AskThread>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<AskThread>, String> {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, mission_id, title, model, created_at, updated_at \
                     FROM ask_threads WHERE mission_id = ?1 ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![mission_id.to_string()], row_to_thread)
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub async fn get_thread(&self, thread_id: Uuid) -> Result<Option<AskThread>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<AskThread>, String> {
            let conn = conn.blocking_lock();
            conn.query_row(
                "SELECT id, mission_id, title, model, created_at, updated_at \
                 FROM ask_threads WHERE id = ?1",
                params![thread_id.to_string()],
                row_to_thread,
            )
            .optional()
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub async fn delete_thread(&self, thread_id: Uuid) -> Result<(), String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let conn = conn.blocking_lock();
            // Messages cascade via FK; delete the thread row.
            conn.execute(
                "DELETE FROM ask_threads WHERE id = ?1",
                params![thread_id.to_string()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub async fn set_thread_title(&self, thread_id: Uuid, title: &str) -> Result<(), String> {
        let conn = self.conn.clone();
        let title = title.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE ask_threads SET title = ?2, updated_at = ?3 WHERE id = ?1",
                params![thread_id.to_string(), title, now()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    // ── Messages ─────────────────────────────────────────────────────────────

    pub async fn append_message(
        &self,
        thread_id: Uuid,
        role: &str,
        content: &str,
        tool_name: Option<String>,
        tool_call_id: Option<String>,
        metadata: Option<serde_json::Value>,
    ) -> Result<AskMessage, String> {
        let conn = self.conn.clone();
        let role = role.to_string();
        let content = content.to_string();
        tokio::task::spawn_blocking(move || -> Result<AskMessage, String> {
            let conn = conn.blocking_lock();
            let seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM ask_messages WHERE thread_id = ?1",
                    params![thread_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let msg = AskMessage {
                id: Uuid::new_v4(),
                thread_id,
                seq,
                role,
                content,
                tool_name,
                tool_call_id,
                metadata,
                created_at: now(),
            };
            let metadata_str = msg
                .metadata
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default();
            conn.execute(
                "INSERT INTO ask_messages \
                 (id, thread_id, seq, role, content, tool_name, tool_call_id, metadata, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    msg.id.to_string(),
                    msg.thread_id.to_string(),
                    msg.seq,
                    msg.role,
                    msg.content,
                    msg.tool_name,
                    msg.tool_call_id,
                    metadata_str,
                    msg.created_at,
                ],
            )
            .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE ask_threads SET updated_at = ?2 WHERE id = ?1",
                params![msg.thread_id.to_string(), msg.created_at],
            )
            .map_err(|e| e.to_string())?;
            Ok(msg)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub async fn list_messages(&self, thread_id: Uuid) -> Result<Vec<AskMessage>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<AskMessage>, String> {
            let conn = conn.blocking_lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, thread_id, seq, role, content, tool_name, tool_call_id, metadata, created_at \
                     FROM ask_messages WHERE thread_id = ?1 ORDER BY seq ASC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![thread_id.to_string()], row_to_message)
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    // ── Operator notes (M2 bridge) ───────────────────────────────────────────

    pub async fn enqueue_operator_note(
        &self,
        mission_id: Uuid,
        body: &str,
        source_thread_id: Option<Uuid>,
    ) -> Result<(), String> {
        let conn = self.conn.clone();
        let body = body.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO operator_notes (id, mission_id, body, source_thread_id, created_at, flushed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                params![
                    Uuid::new_v4().to_string(),
                    mission_id.to_string(),
                    body,
                    source_thread_id.map(|i| i.to_string()),
                    now(),
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Atomically return all pending (un-flushed) operator notes for a mission
    /// and mark them flushed. Used by the working-agent turn-prep path.
    pub async fn take_pending_operator_notes(
        &self,
        mission_id: Uuid,
    ) -> Result<Vec<OperatorNote>, String> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<OperatorNote>, String> {
            let conn = conn.blocking_lock();
            let notes = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, mission_id, body, source_thread_id, created_at \
                         FROM operator_notes WHERE mission_id = ?1 AND flushed_at IS NULL \
                         ORDER BY created_at ASC",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(params![mission_id.to_string()], row_to_note)
                    .map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| e.to_string())?);
                }
                out
            };
            if !notes.is_empty() {
                conn.execute(
                    "UPDATE operator_notes SET flushed_at = ?2 \
                     WHERE mission_id = ?1 AND flushed_at IS NULL",
                    params![mission_id.to_string(), now()],
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(notes)
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<AskThread> {
    let id: String = row.get(0)?;
    let mission_id: String = row.get(1)?;
    Ok(AskThread {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        mission_id: Uuid::parse_str(&mission_id).unwrap_or_default(),
        title: row.get(2)?,
        model: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<AskMessage> {
    let id: String = row.get(0)?;
    let thread_id: String = row.get(1)?;
    let metadata_str: Option<String> = row.get(7)?;
    let metadata = metadata_str
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(&s).ok());
    Ok(AskMessage {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        thread_id: Uuid::parse_str(&thread_id).unwrap_or_default(),
        seq: row.get(2)?,
        role: row.get(3)?,
        content: row.get(4)?,
        tool_name: row.get(5)?,
        tool_call_id: row.get(6)?,
        metadata,
        created_at: row.get(8)?,
    })
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperatorNote> {
    let id: String = row.get(0)?;
    let mission_id: String = row.get(1)?;
    let source: Option<String> = row.get(3)?;
    Ok(OperatorNote {
        id: Uuid::parse_str(&id).unwrap_or_default(),
        mission_id: Uuid::parse_str(&mission_id).unwrap_or_default(),
        body: row.get(2)?,
        source_thread_id: source.and_then(|s| Uuid::parse_str(&s).ok()),
        created_at: row.get(4)?,
    })
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ask_threads (
    id          TEXT PRIMARY KEY,
    mission_id  TEXT NOT NULL,
    title       TEXT,
    model       TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ask_threads_mission ON ask_threads(mission_id, updated_at);

CREATE TABLE IF NOT EXISTS ask_messages (
    id           TEXT PRIMARY KEY,
    thread_id    TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    tool_name    TEXT,
    tool_call_id TEXT,
    metadata     TEXT,
    created_at   TEXT NOT NULL,
    FOREIGN KEY (thread_id) REFERENCES ask_threads(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ask_messages_thread ON ask_messages(thread_id, seq);

CREATE TABLE IF NOT EXISTS operator_notes (
    id               TEXT PRIMARY KEY,
    mission_id       TEXT NOT NULL,
    body             TEXT NOT NULL,
    source_thread_id TEXT,
    created_at       TEXT NOT NULL,
    flushed_at       TEXT
);
CREATE INDEX IF NOT EXISTS idx_operator_notes_pending ON operator_notes(mission_id, flushed_at);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> AskStore {
        let path = std::env::temp_dir().join(format!("ask-test-{}.db", Uuid::new_v4()));
        AskStore::open(path).await.unwrap()
    }

    #[tokio::test]
    async fn thread_and_message_roundtrip() {
        let store = temp_store().await;
        let mission = Uuid::new_v4();

        let thread = store
            .create_thread(mission, Some("gpt-oss-120b".to_string()))
            .await
            .unwrap();
        assert_eq!(thread.mission_id, mission);

        store
            .append_message(thread.id, "user", "what's happening?", None, None, None)
            .await
            .unwrap();
        store
            .append_message(thread.id, "assistant", "reading logs", None, None, None)
            .await
            .unwrap();

        let msgs = store.list_messages(thread.id).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].seq, 1);
        assert_eq!(msgs[1].seq, 2);
        assert_eq!(msgs[0].role, "user");

        let threads = store.list_threads(mission).await.unwrap();
        assert_eq!(threads.len(), 1);

        // Delete cascades messages.
        store.delete_thread(thread.id).await.unwrap();
        assert!(store.get_thread(thread.id).await.unwrap().is_none());
        assert!(store.list_messages(thread.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn operator_notes_are_taken_once() {
        let store = temp_store().await;
        let mission = Uuid::new_v4();

        store
            .enqueue_operator_note(mission, "Added scripts/probe.sh", None)
            .await
            .unwrap();
        store
            .enqueue_operator_note(mission, "Edited src/foo.rs", None)
            .await
            .unwrap();

        let first = store.take_pending_operator_notes(mission).await.unwrap();
        assert_eq!(first.len(), 2);

        // Second take returns nothing — notes flush exactly once.
        let second = store.take_pending_operator_notes(mission).await.unwrap();
        assert!(second.is_empty());
    }
}
