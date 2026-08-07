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

-- The authoritative project roster. Today a project is reconstructed on every
-- read by unioning three sources (markdown trackers, tagged missions, routed
-- deliveries), so a slug typo forks a phantom project and nothing owns the
-- project's own state. This table makes the project an object: it exists
-- because a row says so, and its mode/blocker are columns, not a parsed trailer.
CREATE TABLE IF NOT EXISTS projects (
    slug               TEXT PRIMARY KEY NOT NULL,
    title              TEXT,
    objective          TEXT,
    status             TEXT NOT NULL DEFAULT 'active',   -- active|paused|archived
    mode               TEXT,                             -- active|blocked|paused
    wait_ticks         INTEGER NOT NULL DEFAULT 0,
    next_action        TEXT,
    blocker            TEXT,
    controller_cron_id TEXT,                             -- the controller<->project link
    repository         TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

-- The autonomy grant, structured so it survives a controller rewriting its own
-- prompt: merge authority, budget, and the machine-checkable PAUSED() live here,
-- not in prose the next rollover can drop.
CREATE TABLE IF NOT EXISTS project_grant (
    slug              TEXT PRIMARY KEY NOT NULL
                          REFERENCES projects(slug) ON DELETE CASCADE,
    merge_authority   TEXT,        -- full | repo:a,b | review-first
    budget_per_tick   TEXT,
    parallel_missions INTEGER,
    pause_reason      TEXT,
    resume_condition  TEXT,        -- the structured, checkable resume gate
    material_bar      TEXT,
    answered_at       TEXT
);

-- One row per workstream. `desired_state` is what the track should reach;
-- `status` is where it is. The controller sets these instead of editing prose.
CREATE TABLE IF NOT EXISTS project_tracks (
    slug          TEXT NOT NULL,
    track         TEXT NOT NULL,
    desired_state TEXT,
    status        TEXT,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (slug, track)
);

-- The pending-decision ledger: questions the controller batched for the owner,
-- requestable rather than buried in a markdown file.
CREATE TABLE IF NOT EXISTS project_decisions (
    slug      TEXT NOT NULL,
    at        TEXT NOT NULL,
    question  TEXT NOT NULL,
    rationale TEXT,
    answered  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (slug, at)
);
CREATE INDEX IF NOT EXISTS idx_project_decisions_open
    ON project_decisions(slug, answered);
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
        // Required for `project_grant`'s ON DELETE CASCADE — SQLite defaults it
        // off, so without this a deleted project would strand its grant row.
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
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

    // ---- Authoritative project roster (projects / grant / tracks / decisions) ----

    /// Every project slug in the roster. This is what makes a project *exist*
    /// once the overview is seeded from the DB rather than the source union.
    pub fn list_slugs(&self) -> Result<Vec<String>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT slug FROM projects ORDER BY slug")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// The full roster as records, newest-updated first.
    pub fn list_projects(&self) -> Result<Vec<ProjectRecord>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, title, objective, status, mode, wait_ticks, \
                 next_action, blocker, controller_cron_id, repository, \
                 created_at, updated_at FROM projects ORDER BY updated_at DESC, slug",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], Self::project_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// One project record, or `None` if the slug is not in the roster.
    pub fn get_project(&self, slug: &str) -> Result<Option<ProjectRecord>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT slug, title, objective, status, mode, wait_ticks, \
                 next_action, blocker, controller_cron_id, repository, \
                 created_at, updated_at FROM projects WHERE slug = ?1",
                params![slug],
                Self::project_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
        Ok(ProjectRecord {
            slug: row.get(0)?,
            title: row.get(1)?,
            objective: row.get(2)?,
            status: row.get(3)?,
            mode: row.get(4)?,
            wait_ticks: row.get(5)?,
            next_action: row.get(6)?,
            blocker: row.get(7)?,
            controller_cron_id: row.get(8)?,
            repository: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    /// Create or update a project's descriptive fields. Leaves status/mode and
    /// the runtime fields (wait_ticks/blocker) untouched — those move through
    /// `set_status`/`set_mode`, which is what a controller tick calls.
    pub fn upsert_project(
        &self,
        slug: &str,
        title: Option<&str>,
        objective: Option<&str>,
        repository: Option<&str>,
        controller_cron_id: Option<&str>,
    ) -> Result<ProjectRecord, String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO projects \
                   (slug, title, objective, repository, controller_cron_id, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                 ON CONFLICT(slug) DO UPDATE SET \
                   title = COALESCE(excluded.title, projects.title), \
                   objective = COALESCE(excluded.objective, projects.objective), \
                   repository = COALESCE(excluded.repository, projects.repository), \
                   controller_cron_id = COALESCE(excluded.controller_cron_id, projects.controller_cron_id), \
                   updated_at = excluded.updated_at",
                params![slug, title, objective, repository, controller_cron_id, now],
            )
            .map_err(|e| e.to_string())?;
        drop(connection);
        self.get_project(slug)?
            .ok_or_else(|| "project vanished after upsert".to_string())
    }

    /// Set the controller-reported mode and next-action. Increments
    /// `wait_ticks` while the mode/blocker are unchanged, and resets it to 0 on
    /// any change — so "how long has it been blocked on this" is a column read,
    /// not a scan of the delivery log.
    pub fn set_mode(
        &self,
        slug: &str,
        mode: &str,
        next_action: Option<&str>,
        blocker: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let previous: Option<(Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT mode, blocker FROM projects WHERE slug = ?1",
                params![slug],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let unchanged = matches!(
            &previous,
            Some((m, b)) if m.as_deref() == Some(mode) && b.as_deref() == blocker
        );
        let affected = connection
            .execute(
                "UPDATE projects SET mode = ?2, next_action = ?3, blocker = ?4, \
                 wait_ticks = CASE WHEN ?5 THEN wait_ticks + 1 ELSE 0 END, \
                 updated_at = ?6 WHERE slug = ?1",
                params![slug, mode, next_action, blocker, unchanged, now],
            )
            .map_err(|e| e.to_string())?;
        if affected == 0 {
            return Err(format!("unknown project '{slug}'"));
        }
        Ok(())
    }

    /// Set the operator-facing status (active/paused/archived).
    pub fn set_status(&self, slug: &str, status: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let affected = connection
            .execute(
                "UPDATE projects SET status = ?2, updated_at = ?3 WHERE slug = ?1",
                params![slug, status, now],
            )
            .map_err(|e| e.to_string())?;
        if affected == 0 {
            return Err(format!("unknown project '{slug}'"));
        }
        Ok(())
    }

    pub fn tracks(&self, slug: &str) -> Result<Vec<ProjectTrack>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT track, desired_state, status, updated_at \
                 FROM project_tracks WHERE slug = ?1 ORDER BY track",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug], |row| {
                Ok(ProjectTrack {
                    track: row.get(0)?,
                    desired_state: row.get(1)?,
                    status: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn set_track(
        &self,
        slug: &str,
        track: &str,
        desired_state: Option<&str>,
        status: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO project_tracks (slug, track, desired_state, status, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(slug, track) DO UPDATE SET \
                   desired_state = COALESCE(excluded.desired_state, project_tracks.desired_state), \
                   status = COALESCE(excluded.status, project_tracks.status), \
                   updated_at = excluded.updated_at",
                params![slug, track, desired_state, status, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_grant(&self, slug: &str) -> Result<Option<ProjectGrant>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT merge_authority, budget_per_tick, parallel_missions, \
                 pause_reason, resume_condition, material_bar, answered_at \
                 FROM project_grant WHERE slug = ?1",
                params![slug],
                |row| {
                    Ok(ProjectGrant {
                        merge_authority: row.get(0)?,
                        budget_per_tick: row.get(1)?,
                        parallel_missions: row.get(2)?,
                        pause_reason: row.get(3)?,
                        resume_condition: row.get(4)?,
                        material_bar: row.get(5)?,
                        answered_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_grant(
        &self,
        slug: &str,
        merge_authority: Option<&str>,
        budget_per_tick: Option<&str>,
        parallel_missions: Option<i64>,
        pause_reason: Option<&str>,
        resume_condition: Option<&str>,
        material_bar: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO project_grant \
                   (slug, merge_authority, budget_per_tick, parallel_missions, \
                    pause_reason, resume_condition, material_bar, answered_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(slug) DO UPDATE SET \
                   merge_authority = COALESCE(excluded.merge_authority, project_grant.merge_authority), \
                   budget_per_tick = COALESCE(excluded.budget_per_tick, project_grant.budget_per_tick), \
                   parallel_missions = COALESCE(excluded.parallel_missions, project_grant.parallel_missions), \
                   pause_reason = COALESCE(excluded.pause_reason, project_grant.pause_reason), \
                   resume_condition = COALESCE(excluded.resume_condition, project_grant.resume_condition), \
                   material_bar = COALESCE(excluded.material_bar, project_grant.material_bar), \
                   answered_at = excluded.answered_at",
                params![
                    slug,
                    merge_authority,
                    budget_per_tick,
                    parallel_missions,
                    pause_reason,
                    resume_condition,
                    material_bar,
                    now
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_decision(
        &self,
        slug: &str,
        question: &str,
        rationale: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO project_decisions (slug, at, question, rationale, answered) \
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![slug, now, question, rationale],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn open_decisions(&self, slug: &str) -> Result<Vec<ProjectDecision>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT at, question, rationale FROM project_decisions \
                 WHERE slug = ?1 AND answered = 0 ORDER BY at",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug], |row| {
                Ok(ProjectDecision {
                    at: row.get(0)?,
                    question: row.get(1)?,
                    rationale: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
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

/// A project as an object in its own right, not a union reconstructed per read.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectRecord {
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub wait_ticks: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_cron_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The autonomy grant, structured so it outlives a controller's prompt.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectGrant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_per_tick: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_missions: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_bar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
}

/// One workstream within a project.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectTrack {
    pub track: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub updated_at: String,
}

/// An open question the controller batched for the owner.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectDecision {
    pub at: String,
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
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

    #[test]
    fn a_project_upserts_and_reads_back() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert!(store.get_project("verity").expect("read").is_none());
        assert!(store.list_slugs().expect("slugs").is_empty());

        let p = store
            .upsert_project(
                "verity",
                Some("Verity"),
                Some("prove the compiler"),
                Some("lfglabs-dev/verity"),
                Some("e594d751447d"),
            )
            .expect("upsert");
        assert_eq!(p.slug, "verity");
        assert_eq!(p.status, "active");
        assert_eq!(p.controller_cron_id.as_deref(), Some("e594d751447d"));

        // A second upsert enriches without clobbering unspecified fields.
        let p2 = store
            .upsert_project("verity", None, None, None, None)
            .expect("re-upsert");
        assert_eq!(p2.title.as_deref(), Some("Verity"));
        assert_eq!(p2.repository.as_deref(), Some("lfglabs-dev/verity"));
        assert_eq!(store.list_slugs().expect("slugs"), vec!["verity"]);
    }

    #[test]
    fn set_mode_counts_consecutive_ticks_and_resets_on_change() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("bench", None, None, None, None)
            .expect("seed");

        store
            .set_mode("bench", "active", Some("run"), None)
            .expect("m1");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 0);

        // Same mode+blocker two more ticks: the counter is how long it's been here.
        store
            .set_mode("bench", "blocked", Some("x"), Some("transport-cap"))
            .expect("m2");
        store
            .set_mode("bench", "blocked", Some("x"), Some("transport-cap"))
            .expect("m3");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 1);

        // A changed blocker resets the counter — a new thing to be stuck on.
        store
            .set_mode("bench", "blocked", Some("x"), Some("other"))
            .expect("m4");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 0);
    }

    #[test]
    fn set_mode_on_unknown_project_errors() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert!(store.set_mode("ghost", "active", None, None).is_err());
    }

    #[test]
    fn grant_and_tracks_round_trip_and_cascade_on_delete() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .set_grant(
                "lido",
                Some("review-first"),
                Some("2/tick"),
                Some(3),
                None,
                None,
                Some("only merged PRs"),
            )
            .expect("grant");
        let g = store.get_grant("lido").expect("read").expect("present");
        assert_eq!(g.merge_authority.as_deref(), Some("review-first"));
        assert_eq!(g.parallel_missions, Some(3));

        store
            .set_track("lido", "P-ETH-1", Some("proved"), Some("in-progress"))
            .expect("track");
        let tracks = store.tracks("lido").expect("tracks");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track, "P-ETH-1");

        store
            .record_decision("lido", "merge #48?", Some("blocks A.2"))
            .expect("dec");
        assert_eq!(store.open_decisions("lido").expect("open").len(), 1);

        // Deleting the project cascades the grant away (FK ON DELETE CASCADE).
        store
            .lock()
            .expect("lock")
            .execute("DELETE FROM projects WHERE slug = 'lido'", [])
            .expect("delete");
        assert!(store.get_grant("lido").expect("read").is_none());
    }
}
