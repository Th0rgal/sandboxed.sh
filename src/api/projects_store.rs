//! Durable, explicitly-declared facts about a project.
//!
//! Today a project's conversation is *inferred* from whichever session
//! produced its newest delivery. That guess is wrong in the common case: cron
//! controllers open a throwaway session per tick and end it, so the inferred
//! conversation is a corpse that nobody can reply to (measured on Verity: 52
//! deliveries across 52 distinct ended sessions). A project's control
//! conversation is a decision, not a statistic, so it is stored.
//!
//! This lives in its own SQLite file rather than the `board-overrides.json`
//! overlay for three reasons: that overlay is unwritable (503) whenever
//! `HERMES_PROJECTS_DIR` is missing, its reader silently discards the whole
//! file on any shape change, and it is read-modify-write — fine for a human
//! toggling a bucket, not for something written per tick.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub type SharedProjectsStore = Arc<ProjectsStore>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS project_bindings (
    slug               TEXT PRIMARY KEY NOT NULL,
    control_session_id TEXT NOT NULL,
    bound_at           TEXT NOT NULL,
    bound_by           TEXT
);
"#;

/// A project's control conversation and how we know about it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectConversation {
    pub session_id: String,
    /// `"binding"` when explicitly declared, `"latest_update"` when inferred
    /// from the newest delivery. Callers render the two differently: a guess
    /// invites a "bind this" action, a binding does not.
    pub source: &'static str,
    /// Only set for an explicit binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_at: Option<String>,
}

pub struct ProjectsStore {
    connection: Mutex<Connection>,
}

impl ProjectsStore {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.connection
            .lock()
            .map_err(|_| "projects database lock poisoned".to_string())
    }

    /// Every explicit binding, keyed by project slug. Read once per overview
    /// render rather than per row.
    pub fn bindings(&self) -> Result<HashMap<String, ProjectConversation>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT slug, control_session_id, bound_at FROM project_bindings")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ProjectConversation {
                        session_id: row.get(1)?,
                        source: "binding",
                        bound_at: row.get::<_, Option<String>>(2)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        for row in rows {
            let (slug, conversation) = row.map_err(|e| e.to_string())?;
            out.insert(slug, conversation);
        }
        Ok(out)
    }

    pub fn binding(&self, slug: &str) -> Result<Option<ProjectConversation>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT control_session_id, bound_at FROM project_bindings WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(ProjectConversation {
                        session_id: row.get(0)?,
                        source: "binding",
                        bound_at: row.get::<_, Option<String>>(1)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Bind (or re-bind) a project's control conversation.
    ///
    /// Re-binding is a normal operation — an operator opening a fresh thread
    /// for the same project — so this upserts rather than refusing.
    pub fn set_binding(
        &self,
        slug: &str,
        session_id: &str,
        bound_by: Option<&str>,
    ) -> Result<ProjectConversation, String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO project_bindings (slug, control_session_id, bound_at, bound_by) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(slug) DO UPDATE SET \
                   control_session_id = excluded.control_session_id, \
                   bound_at = excluded.bound_at, \
                   bound_by = excluded.bound_by",
                params![slug, session_id, now, bound_by],
            )
            .map_err(|e| e.to_string())?;
        Ok(ProjectConversation {
            session_id: session_id.to_string(),
            source: "binding",
            bound_at: Some(now),
        })
    }

    /// Returns whether a binding existed.
    pub fn clear_binding(&self, slug: &str) -> Result<bool, String> {
        let connection = self.lock()?;
        let removed = connection
            .execute(
                "DELETE FROM project_bindings WHERE slug = ?1",
                params![slug],
            )
            .map_err(|e| e.to_string())?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_round_trips_and_rebinds_in_place() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert!(store.binding("verity").expect("read").is_none());

        let bound = store
            .set_binding("verity", "20260804_103847_86ca5c", Some("thomas"))
            .expect("bind");
        assert_eq!(bound.session_id, "20260804_103847_86ca5c");
        assert_eq!(bound.source, "binding");

        // Re-binding is the "I opened a new thread" workflow, not an error.
        store
            .set_binding("verity", "20260805_090000_aaaaaa", None)
            .expect("rebind");
        assert_eq!(
            store
                .binding("verity")
                .expect("read")
                .expect("bound")
                .session_id,
            "20260805_090000_aaaaaa",
            "a project has exactly one control conversation"
        );

        assert!(store.clear_binding("verity").expect("clear"));
        assert!(!store.clear_binding("verity").expect("clear again"));
        assert!(store.binding("verity").expect("read").is_none());
    }

    #[test]
    fn bindings_are_isolated_per_project() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("verity", "sess-a", None).expect("bind");
        store.set_binding("lido", "sess-b", None).expect("bind");
        let all = store.bindings().expect("all");
        assert_eq!(all.len(), 2);
        assert_eq!(all["verity"].session_id, "sess-a");
        assert_eq!(all["lido"].session_id, "sess-b");
    }

    /// Two projects may legitimately report into one conversation.
    #[test]
    fn one_conversation_can_serve_several_projects() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("verity", "shared", None).expect("bind");
        store
            .set_binding("verity-docs", "shared", None)
            .expect("bind");
        let all = store.bindings().expect("all");
        assert_eq!(all["verity"].session_id, "shared");
        assert_eq!(all["verity-docs"].session_id, "shared");
    }
}
