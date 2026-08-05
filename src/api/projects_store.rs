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

-- One row per *distinct consecutive* state a project reported. A controller
-- that reports the same signature for six hours produces one row with
-- observations=24, not 24 rows: the question worth answering is "how long has
-- it been saying this", and a flat event log makes that a scan.
CREATE TABLE IF NOT EXISTS project_state_events (
    slug          TEXT NOT NULL,
    signature     TEXT NOT NULL,
    headline      TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL,
    observations  INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (slug, first_seen_at)
);
CREATE INDEX IF NOT EXISTS idx_state_events_slug_seen
    ON project_state_events(slug, last_seen_at DESC);
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

    /// The project bound to `session_id`, if any.
    ///
    /// The reverse of [`Self::bindings`], used to tag a mission with the
    /// project of the conversation that spawned it. Bindings are one project
    /// per slug but several slugs may share a conversation, so this returns
    /// the lexicographically first match: an arbitrary-but-stable choice is
    /// better than a tag that changes between two calls.
    pub fn project_for_session(&self, session_id: &str) -> Result<Option<String>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT slug FROM project_bindings WHERE control_session_id = ?1 \
                 ORDER BY slug LIMIT 1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())
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

    /// Record that `slug` reported `signature` at `at`.
    ///
    /// Collapses repeats: re-reporting the state a project is already in
    /// extends the open row rather than opening a new one. The count that
    /// results is the durable form of the overview's in-memory "same signature
    /// three ticks running" heuristic, which could only ever see the deliveries
    /// still inside the scan window.
    ///
    /// Idempotent on `at`: the ingestor re-reads an overlapping window every
    /// cycle, so replaying a delivery it has already seen must not inflate the
    /// count. Returns the resulting observation count for this state.
    pub fn record_state(
        &self,
        slug: &str,
        signature: &str,
        headline: Option<&str>,
        at: &str,
    ) -> Result<u32, String> {
        let connection = self.lock()?;
        let current: Option<(String, String, String, u32)> = connection
            .query_row(
                "SELECT signature, first_seen_at, last_seen_at, observations \
                 FROM project_state_events WHERE slug = ?1 \
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![slug],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        if let Some((open_signature, first_seen_at, last_seen_at, observations)) = current {
            if open_signature == signature {
                // Already counted, or older than what we have: nothing to do.
                if at <= last_seen_at.as_str() {
                    return Ok(observations);
                }
                connection
                    .execute(
                        "UPDATE project_state_events \
                         SET last_seen_at = ?1, observations = observations + 1, \
                             headline = COALESCE(?2, headline) \
                         WHERE slug = ?3 AND first_seen_at = ?4",
                        params![at, headline, slug, first_seen_at],
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(observations + 1);
            }
            // A delivery older than the newest state we hold is a replay from
            // the ingestor's overlap, not a transition. Recording it would
            // fabricate a flap between two states the project never made.
            if at <= last_seen_at.as_str() {
                return Ok(0);
            }
        }

        connection
            .execute(
                "INSERT INTO project_state_events \
                   (slug, signature, headline, first_seen_at, last_seen_at, observations) \
                 VALUES (?1, ?2, ?3, ?4, ?4, 1) \
                 ON CONFLICT(slug, first_seen_at) DO NOTHING",
                params![slug, signature, headline, at],
            )
            .map_err(|e| e.to_string())?;
        Ok(1)
    }

    /// A project's state history, newest first.
    pub fn state_timeline(&self, slug: &str, limit: usize) -> Result<Vec<ProjectState>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT signature, headline, first_seen_at, last_seen_at, observations \
                 FROM project_state_events WHERE slug = ?1 \
                 ORDER BY last_seen_at DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug, limit as i64], |row| {
                Ok(ProjectState {
                    signature: row.get(0)?,
                    headline: row.get(1)?,
                    first_seen_at: row.get(2)?,
                    last_seen_at: row.get(3)?,
                    observations: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Observation counts for the state each project is *currently* in.
    ///
    /// One query for the whole overview: per-row lookups would put a query per
    /// project inside a page render.
    pub fn current_state_observations(&self) -> Result<HashMap<String, u32>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, observations FROM project_state_events e \
                 WHERE last_seen_at = ( \
                   SELECT MAX(last_seen_at) FROM project_state_events \
                   WHERE slug = e.slug \
                 )",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())
    }
}

/// One distinct state a project reported, with how long it stayed there.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectState {
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    /// How many deliveries reported this same state in a row.
    pub observations: u32,
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

    // ---- reverse lookup ----

    #[test]
    fn a_bound_session_resolves_back_to_its_project() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("verity", "sess-a", None).expect("bind");
        store.set_binding("lido", "sess-b", None).expect("bind");
        assert_eq!(
            store
                .project_for_session("sess-a")
                .expect("lookup")
                .as_deref(),
            Some("verity")
        );
        assert_eq!(
            store
                .project_for_session("sess-b")
                .expect("lookup")
                .as_deref(),
            Some("lido")
        );
    }

    #[test]
    fn an_unbound_session_resolves_to_nothing() {
        // Must be None rather than a guess: tagging a mission with the wrong
        // project is worse than leaving it untagged, because a wrong tag is
        // believed.
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("verity", "sess-a", None).expect("bind");
        assert_eq!(
            store.project_for_session("sess-unknown").expect("lookup"),
            None
        );
        assert_eq!(store.project_for_session("").expect("lookup"), None);
    }

    #[test]
    fn a_shared_conversation_resolves_stably() {
        // Several projects may report into one conversation. Any answer is
        // arbitrary, but it must not change between two identical calls.
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("zulu", "shared", None).expect("bind");
        store.set_binding("alpha", "shared", None).expect("bind");
        let first = store.project_for_session("shared").expect("lookup");
        let second = store.project_for_session("shared").expect("lookup");
        assert_eq!(first, second);
        assert_eq!(first.as_deref(), Some("alpha"));
    }

    #[test]
    fn rebinding_moves_the_reverse_lookup_too() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store.set_binding("verity", "old", None).expect("bind");
        store.set_binding("verity", "new", None).expect("rebind");
        assert_eq!(store.project_for_session("old").expect("lookup"), None);
        assert_eq!(
            store.project_for_session("new").expect("lookup").as_deref(),
            Some("verity")
        );
    }

    // ---- state timeline ----

    #[test]
    fn repeating_a_state_extends_it_instead_of_opening_a_new_one() {
        let store = ProjectsStore::open_in_memory().expect("store");
        for (i, at) in [
            "2026-08-04T10:00:00Z",
            "2026-08-04T10:15:00Z",
            "2026-08-04T10:30:00Z",
        ]
        .iter()
        .enumerate()
        {
            let count = store
                .record_state("verity", "phase1-blocked", Some("still blocked"), at)
                .expect("record");
            assert_eq!(count as usize, i + 1);
        }
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].observations, 3);
        assert_eq!(timeline[0].first_seen_at, "2026-08-04T10:00:00Z");
        assert_eq!(timeline[0].last_seen_at, "2026-08-04T10:30:00Z");
    }

    #[test]
    fn a_new_state_opens_a_new_row_and_leaves_the_old_one_closed() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z")
            .expect("record");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z")
            .expect("record");
        store
            .record_state("verity", "merged", None, "2026-08-04T10:30:00Z")
            .expect("record");

        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].signature, "merged");
        assert_eq!(timeline[0].observations, 1);
        assert_eq!(timeline[1].signature, "blocked");
        assert_eq!(timeline[1].observations, 2);
        assert_eq!(timeline[1].last_seen_at, "2026-08-04T10:15:00Z");
    }

    /// The ingestor re-reads an overlapping window each cycle. Replaying a
    /// delivery it has already counted must not inflate the observation count,
    /// or "how long has it been stuck" grows purely from polling frequency.
    #[test]
    fn replaying_a_delivery_does_not_inflate_the_count() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z")
            .expect("record");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z")
            .expect("record");
        for _ in 0..5 {
            store
                .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z")
                .expect("replay");
        }
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline[0].observations, 2);
    }

    /// An out-of-order delivery from the overlap window must not be recorded
    /// as a transition — that would fabricate a flap between two states the
    /// project never actually made.
    #[test]
    fn an_older_delivery_never_fabricates_a_transition() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z")
            .expect("record");
        store
            .record_state("verity", "merged", None, "2026-08-04T11:00:00Z")
            .expect("record");
        // Arrives late, older than the current state.
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:30:00Z")
            .expect("late");

        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline.len(), 2, "no third row for the replayed state");
        assert_eq!(timeline[0].signature, "merged");
    }

    #[test]
    fn projects_keep_separate_timelines() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "a", None, "2026-08-04T10:00:00Z")
            .expect("record");
        store
            .record_state("lido", "b", None, "2026-08-04T10:01:00Z")
            .expect("record");
        store
            .record_state("lido", "b", None, "2026-08-04T10:02:00Z")
            .expect("record");

        let observations = store.current_state_observations().expect("observations");
        assert_eq!(observations.get("verity"), Some(&1));
        assert_eq!(observations.get("lido"), Some(&2));
    }

    #[test]
    fn an_unknown_project_has_an_empty_timeline() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert!(store
            .state_timeline("nope", 10)
            .expect("timeline")
            .is_empty());
        assert!(store
            .current_state_observations()
            .expect("observations")
            .is_empty());
    }

    #[test]
    fn a_headline_backfills_but_never_erases() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "s", None, "2026-08-04T10:00:00Z")
            .expect("record");
        store
            .record_state("verity", "s", Some("now we know"), "2026-08-04T10:15:00Z")
            .expect("record");
        store
            .record_state("verity", "s", None, "2026-08-04T10:30:00Z")
            .expect("record");
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline[0].headline.as_deref(), Some("now we know"));
    }
}
