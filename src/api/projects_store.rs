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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

/// Bumped by every table rebuild. Additive `ensure_column` migrations do not
/// count; they stay idempotent on their own.
pub const SCHEMA_VERSION: i64 = 2;

/// Tracker parser revision recorded on every import row.
pub const TRACKER_PARSER_VERSION: i64 = 1;

pub type SharedProjectsStore = Arc<ProjectsStore>;

pub const TRACK_VERIFIER_CLASSES: [&str; 5] =
    ["external_state", "command", "review", "operator", "manual"];

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
    -- Session that produced the newest delivery folded into this row: the
    -- overview offers it as the inferred conversation for the project.
    session_id    TEXT,
    PRIMARY KEY (slug, first_seen_at)
);
CREATE INDEX IF NOT EXISTS idx_state_events_slug_seen
    ON project_state_events(slug, last_seen_at DESC);

-- Deliveries the ingestor could not attribute to a project: no routing key, or
-- a key whose alias resolves to an archived/deleted target. Kept small (the
-- newest ~50) — this is a triage inbox for the board, not an archive.
CREATE TABLE IF NOT EXISTS unrouted_deliveries (
    session_id TEXT NOT NULL,
    at         TEXT NOT NULL,
    headline   TEXT NOT NULL,
    signature  TEXT,
    mode       TEXT,
    blocker    TEXT,
    PRIMARY KEY (session_id, at)
);

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
    updated_at         TEXT NOT NULL,
    -- Watermark for the last mode write. HTTP set_mode stamps now; ingest
    -- only overwrites when the delivery is at least this new.
    mode_signal_at     TEXT
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
    answered_at       TEXT,
    autonomy_level    TEXT         -- observe | propose | act_reversible | act_full
);

-- One row per workstream: the plan inventory and the only durable truth about
-- what work exists. `lifecycle` is the only stored state; satisfaction is
-- derived from `receipts` (see the situation builder), never written here.
-- `track` is the normalized key (lowercase `[a-z0-9-]`); old spellings live in
-- `project_track_aliases`.
CREATE TABLE IF NOT EXISTS project_tracks (
    id                   TEXT PRIMARY KEY,                 -- stable uuid
    slug                 TEXT NOT NULL,
    track                TEXT NOT NULL,
    title                TEXT NOT NULL,
    desired_state        TEXT,
    lifecycle            TEXT NOT NULL DEFAULT 'active'
                         CHECK (lifecycle IN ('active','cancelled')),
    origin               TEXT NOT NULL DEFAULT 'declared'
                         CHECK (origin IN ('declared','imported','absorbed')),
    explicit_blocker     TEXT,                             -- structured judgment, nullable
    acceptance_criteria  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(acceptance_criteria)),
    depends_on           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on)),
    revision             INTEGER NOT NULL DEFAULT 0,
    position             INTEGER NOT NULL DEFAULT 0,
    governed_artifact_version TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    UNIQUE (slug, track)
);

CREATE TABLE IF NOT EXISTS schema_version (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    version     INTEGER NOT NULL,
    migrated_at TEXT NOT NULL
);

-- Append-only evidence. A receipt is emitted by a completed action or an
-- explicit observation; it never represents a live process (live build state
-- is `remote_jobs`). Terminal rows are never updated: a newer receipt
-- supersedes (`supersedes_receipt_id`) or invalidates an older one.
CREATE TABLE IF NOT EXISTS receipts (
    id                    TEXT PRIMARY KEY,
    idempotency_key       TEXT NOT NULL UNIQUE,
    request_hash          TEXT NOT NULL,
    kind                  TEXT NOT NULL,                   -- legacy_import|accept|invalidate|reconcile|reconcile_ack|build|...
    project_slug          TEXT,
    track_id              TEXT,
    criterion_id          TEXT,
    subject_type          TEXT NOT NULL,                   -- build|pr|command|migration|import|operator|...
    subject_id            TEXT NOT NULL,                   -- immutable external handle
    outcome               TEXT NOT NULL CHECK (outcome IN
                            ('succeeded','failed','cancelled','observed','invalidated')),
    actor_type            TEXT NOT NULL,                   -- mission|operator|controller|system
    actor_id              TEXT NOT NULL,
    verifier              TEXT,
    supersedes_receipt_id TEXT REFERENCES receipts(id),
    observed_at           TEXT NOT NULL,
    payload               TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
    created_at            TEXT NOT NULL,
    FOREIGN KEY (project_slug) REFERENCES projects(slug) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS receipts_track_time
    ON receipts(project_slug, track_id, observed_at);
CREATE INDEX IF NOT EXISTS receipts_subject
    ON receipts(subject_type, subject_id);

-- Old spellings of a track key (`UX1`, `P-ETH-1`) that still resolve.
CREATE TABLE IF NOT EXISTS project_track_aliases (
    slug        TEXT NOT NULL,
    alias_key   TEXT NOT NULL,
    track_id    TEXT NOT NULL REFERENCES project_tracks(id) ON DELETE CASCADE,
    reason      TEXT NOT NULL,                             -- normalized_collision|imported_code|renamed
    created_at  TEXT NOT NULL,
    PRIMARY KEY (slug, alias_key)
);

-- External references a track governs. A shared PR is a matching hint for
-- absorption, never an automatic merge.
CREATE TABLE IF NOT EXISTS project_track_refs (
    track_id    TEXT NOT NULL REFERENCES project_tracks(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,                             -- pr|issue|mission
    repository  TEXT NOT NULL DEFAULT '',
    number      INTEGER NOT NULL DEFAULT 0,
    url         TEXT,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (track_id, kind, repository, number)
);

-- Who may mutate a track right now. A mission (attempt) holds at most one
-- writer lease per (track, mutation_domain); readers coexist. Ownership is
-- derived from live leases — never copied onto the track row.
CREATE TABLE IF NOT EXISTS track_leases (
    id               TEXT PRIMARY KEY,
    slug             TEXT NOT NULL,
    track_id         TEXT NOT NULL REFERENCES project_tracks(id) ON DELETE CASCADE,
    mutation_domain  TEXT NOT NULL,
    attempt_id       TEXT NOT NULL,                        -- mission id
    mode             TEXT NOT NULL CHECK (mode IN ('reader','writer')),
    state            TEXT NOT NULL CHECK (state IN ('reserved','active','released','expired')),
    lease_until      TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
-- One live lease per dispatch key; a released/expired row keeps its key as
-- history and never satisfies a retry.
CREATE UNIQUE INDEX IF NOT EXISTS track_leases_live_key
    ON track_leases(idempotency_key) WHERE state IN ('reserved','active');
CREATE INDEX IF NOT EXISTS track_leases_domain
    ON track_leases(track_id, mutation_domain, state, lease_until);
CREATE INDEX IF NOT EXISTS track_leases_attempt
    ON track_leases(attempt_id, state);

-- One row per (source, content hash, parser): the importer is idempotent.
CREATE TABLE IF NOT EXISTS project_imports (
    slug            TEXT NOT NULL,
    source_path     TEXT NOT NULL,
    source_hash     TEXT NOT NULL,
    parser_version  INTEGER NOT NULL,
    imported_at     TEXT NOT NULL,
    items           INTEGER NOT NULL,
    receipt_id      TEXT,
    PRIMARY KEY (slug, source_path, source_hash, parser_version)
);

-- Evidence is attached to the acceptance-criterion text rather than its array
-- position. Reordering a contract cannot accidentally make an old receipt
-- satisfy a different criterion.
CREATE TABLE IF NOT EXISTS project_track_evidence (
    evidence_id       TEXT PRIMARY KEY NOT NULL,
    slug              TEXT NOT NULL,
    track             TEXT NOT NULL,
    track_revision    INTEGER NOT NULL,
    criterion         TEXT NOT NULL,
    verifier_class    TEXT NOT NULL,
    evidence_ref      TEXT NOT NULL,
    artifact_version  TEXT,
    observed_at       TEXT NOT NULL,
    accepted_at       TEXT NOT NULL,
    accepted_by       TEXT NOT NULL,
    FOREIGN KEY (slug, track) REFERENCES project_tracks(slug, track) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_track_evidence_contract
    ON project_track_evidence(slug, track, track_revision, criterion, artifact_version);

-- A durable dispatch intent is committed before the mission-store mutation.
-- If the process dies between stores, the `reserved` row is visible and can
-- be reconciled instead of silently losing ownership/linkage.
CREATE TABLE IF NOT EXISTS project_track_dispatches (
    idempotency_key  TEXT PRIMARY KEY NOT NULL,
    slug             TEXT NOT NULL,
    track            TEXT NOT NULL,
    track_revision   INTEGER NOT NULL,
    owner_lease_id   TEXT NOT NULL,
    lease_expires_at TEXT NOT NULL,
    state            TEXT NOT NULL, -- reserved|started|failed|superseded
    mission_id       TEXT,
    receipt          TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    FOREIGN KEY (slug, track) REFERENCES project_tracks(slug, track) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_track_dispatch_owner
    ON project_track_dispatches(slug, track, state, lease_expires_at);

-- The decision ledger. Two kinds of rows share it: escalations (questions the
-- controller batched for the owner, `authority = 'escalation'`) and autonomous
-- acts the controller declared before doing (`authority = 'granted'`). The
-- legacy `answered` flag is dual-written (1 for anything not pending) so an
-- older binary's "open decisions" query never surfaces autonomous acts.
CREATE TABLE IF NOT EXISTS project_decisions (
    slug        TEXT NOT NULL,
    at          TEXT NOT NULL,
    question    TEXT NOT NULL,
    rationale   TEXT,
    answered    INTEGER NOT NULL DEFAULT 0,
    kind        TEXT,                                    -- merge|dispatch|scope|budget|...
    authority   TEXT NOT NULL DEFAULT 'escalation',      -- granted|escalation
    status      TEXT,                                    -- decided|pending_user|answered|expired
    answer      TEXT,
    answered_at TEXT,
    evidence    TEXT,                                    -- JSON: {pr_url, mission_id, ...}
    PRIMARY KEY (slug, at)
);
CREATE INDEX IF NOT EXISTS idx_project_decisions_open
    ON project_decisions(slug, answered);
-- idx_project_decisions_status is created in initialize(), after the additive
-- column migration guarantees `status` exists on legacy tables.

-- Chat-planned roadmap items. Board tasks live in the mission store and are
-- writable only by their boss mission; a proposal is the project-scoped,
-- owner/assistant-writable precursor. The roadmap read unions proposals in as
-- `status = "proposed"`, deduped by task_key — a boss planning a real task
-- under the same key silently supersedes ("adopts") the proposal.
CREATE TABLE IF NOT EXISTS project_roadmap_proposals (
    slug                TEXT NOT NULL,
    task_key            TEXT NOT NULL,
    title               TEXT NOT NULL,
    prompt              TEXT,
    acceptance_criteria TEXT,                             -- JSON array of strings
    depends_on          TEXT,                             -- JSON array of task keys
    status              TEXT NOT NULL DEFAULT 'proposed', -- proposed|cancelled
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    PRIMARY KEY (slug, task_key)
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
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        // Required for `project_grant`'s ON DELETE CASCADE — SQLite defaults it
        // off, so without this a deleted project would strand its grant row.
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // A table rebuild is the one migration that is not trivially
        // reversible: copy the file first, next to itself, so an operator can
        // roll back by moving the copy back.
        if Self::needs_tracks_rebuild(&connection)? {
            let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
            let backup = path.with_extension(format!("db.bak-{stamp}"));
            connection.execute("VACUUM INTO ?1", params![backup.to_string_lossy()])?;
            tracing::warn!(backup = %backup.display(), "projects.db: rebuilding project_tracks (v2); backup written");
        }
        Self::initialize(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Self::initialize(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Create-or-migrate: `SCHEMA` covers fresh databases, and additive column
    /// migrations bring an existing database up to the current shape.
    /// `CREATE TABLE IF NOT EXISTS` never alters an existing table, so a new
    /// column must be reconciled explicitly.
    fn initialize(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute_batch(SCHEMA)?;
        // project_state_events.session_id (2026-08: the overview builds
        // latest_update from the store, so the delivery's session rides along).
        Self::ensure_column(
            connection,
            "project_state_events",
            "session_id",
            "session_id TEXT",
        )?;
        // 2026-08: the decision ledger grew authority/status/evidence, and the
        // grant grew a normalized autonomy level.
        Self::ensure_column(connection, "project_decisions", "kind", "kind TEXT")?;
        Self::ensure_column(
            connection,
            "project_decisions",
            "authority",
            "authority TEXT NOT NULL DEFAULT 'escalation'",
        )?;
        Self::ensure_column(connection, "project_decisions", "status", "status TEXT")?;
        Self::ensure_column(connection, "project_decisions", "answer", "answer TEXT")?;
        Self::ensure_column(
            connection,
            "project_decisions",
            "answered_at",
            "answered_at TEXT",
        )?;
        Self::ensure_column(connection, "project_decisions", "evidence", "evidence TEXT")?;
        Self::ensure_column(
            connection,
            "project_grant",
            "autonomy_level",
            "autonomy_level TEXT",
        )?;
        Self::ensure_column(
            connection,
            "projects",
            "mode_signal_at",
            "mode_signal_at TEXT",
        )?;
        // Rows that already had a mode before this column existed must not
        // look unstamped: the first ingest would otherwise overwrite them.
        connection.execute(
            "UPDATE projects SET mode_signal_at = updated_at \
             WHERE mode_signal_at IS NULL AND mode IS NOT NULL",
            [],
        )?;
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_project_decisions_status \
             ON project_decisions(slug, status);",
        )?;
        // Normalize every row missing a status — unconditionally, not only
        // when the column was just added: after a rollback, the OLD binary's
        // legacy INSERT writes rows with `status` NULL into the already-
        // migrated table, and those must land as escalations on the next
        // upgrade too. Idempotent (WHERE status IS NULL), so it costs nothing
        // on a healthy database.
        connection.execute(
            "UPDATE project_decisions SET status = \
             CASE WHEN answered = 1 THEN 'answered' ELSE 'pending_user' END \
             WHERE status IS NULL",
            [],
        )?;
        // 2026-08: planned items persist the submitted title/contract on the
        // track itself so `plan_project_tasks` is not a silent title-only write.
        Self::ensure_column(connection, "project_tracks", "title", "title TEXT")?;
        Self::ensure_column(
            connection,
            "project_tracks",
            "acceptance_criteria",
            "acceptance_criteria TEXT",
        )?;
        Self::ensure_column(
            connection,
            "project_tracks",
            "depends_on",
            "depends_on TEXT",
        )?;
        // 2026-09: tracks gain a stable id, a checked lifecycle, an origin and
        // a revision; legacy `done`/`closed` become claim receipts. Table
        // rebuild, one transaction, idempotent.
        Self::migrate_tracks_v2(connection)?;
        Ok(())
    }

    /// True while `project_tracks` still has the pre-v2 shape.
    fn needs_tracks_rebuild(connection: &Connection) -> rusqlite::Result<bool> {
        let table_exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'project_tracks'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if table_exists.is_none() {
            return Ok(false);
        }
        // v2 rows carry a stable `id`. The interim evidence-driven schema
        // (2026-09-01, `feat/roadmap-evidence-projection`) already had a
        // `lifecycle` column, so that is not the marker.
        Ok(!Self::table_has_column(connection, "project_tracks", "id")?)
    }

    fn table_has_column(
        connection: &Connection,
        table: &str,
        column: &str,
    ) -> rusqlite::Result<bool> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let found = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == column);
        Ok(found)
    }

    fn table_exists(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
        Ok(connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    /// Rebuild `project_tracks` into the v2 shape.
    ///
    /// Legacy mapping (design doc §A1): `cancelled` → lifecycle `cancelled`;
    /// `done` / `closed` → lifecycle `active` plus a `legacy_import` claim
    /// receipt; anything else (NULL, empty, `running`, `in-progress`,
    /// unknown) → `active` plus a reconciliation correction. Keys are
    /// normalized; a spelling that collides after normalization becomes an
    /// alias of the most recently updated row and is reported as an
    /// ambiguity — never silently merged as the same semantic track.
    fn migrate_tracks_v2(connection: &Connection) -> rusqlite::Result<()> {
        if !Self::needs_tracks_rebuild(connection)? {
            Self::stamp_schema_version(connection)?;
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> rusqlite::Result<()> {
            connection.execute_batch(
                "CREATE TABLE project_tracks_v2 (
                    id                   TEXT PRIMARY KEY,
                    slug                 TEXT NOT NULL,
                    track                TEXT NOT NULL,
                    title                TEXT NOT NULL,
                    desired_state        TEXT,
                    lifecycle            TEXT NOT NULL DEFAULT 'active'
                                         CHECK (lifecycle IN ('active','cancelled')),
                    origin               TEXT NOT NULL DEFAULT 'declared'
                                         CHECK (origin IN ('declared','imported','absorbed')),
                    explicit_blocker     TEXT,
                    acceptance_criteria  TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(acceptance_criteria)),
                    depends_on           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(depends_on)),
                    revision             INTEGER NOT NULL DEFAULT 0,
                    position             INTEGER NOT NULL DEFAULT 0,
                    governed_artifact_version TEXT,
                    created_at           TEXT NOT NULL,
                    updated_at           TEXT NOT NULL,
                    UNIQUE (slug, track)
                );",
            )?;
            struct Legacy {
                slug: String,
                track: String,
                desired_state: Option<String>,
                status: Option<String>,
                title: Option<String>,
                acceptance_criteria: Option<String>,
                depends_on: Option<String>,
                updated_at: String,
                // Interim evidence-driven schema (2026-09-01): present only
                // when the columns exist.
                lifecycle: Option<String>,
                position: Option<i64>,
                governed_artifact_version: Option<String>,
                revision: Option<i64>,
            }
            let interim = Self::table_has_column(connection, "project_tracks", "lifecycle")?;
            let legacy: Vec<Legacy> = {
                let select = if interim {
                    "SELECT slug, track, desired_state, status, title, acceptance_criteria, \
                            depends_on, updated_at, lifecycle, position, governed_artifact_version, revision \
                     FROM project_tracks ORDER BY slug, updated_at DESC, track"
                } else {
                    "SELECT slug, track, desired_state, status, title, acceptance_criteria, \
                            depends_on, updated_at, NULL, NULL, NULL, NULL \
                     FROM project_tracks ORDER BY slug, updated_at DESC, track"
                };
                let mut statement = connection.prepare(select)?;
                let rows = statement.query_map([], |row| {
                    Ok(Legacy {
                        slug: row.get(0)?,
                        track: row.get(1)?,
                        desired_state: row.get(2)?,
                        status: row.get(3)?,
                        title: row.get(4)?,
                        acceptance_criteria: row.get(5)?,
                        depends_on: row.get(6)?,
                        updated_at: row.get(7)?,
                        lifecycle: row.get(8)?,
                        position: row.get(9)?,
                        governed_artifact_version: row.get(10)?,
                        revision: row.get(11)?,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            struct InterimEvidence {
                criterion: String,
                verifier_class: String,
                evidence_ref: String,
                artifact_version: String,
                observed_at: String,
                accepted_at: String,
                accepted_by: String,
            }
            let mut interim_evidence: HashMap<(String, String), Vec<InterimEvidence>> =
                HashMap::new();
            if Self::table_exists(connection, "project_track_evidence")? {
                let mut statement = connection.prepare(
                    "SELECT slug, track, criterion, verifier_class, evidence_ref, artifact_version, \
                            observed_at, accepted_at, accepted_by \
                     FROM project_track_evidence ORDER BY accepted_at",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                        InterimEvidence {
                            criterion: row.get(2)?,
                            verifier_class: row.get(3)?,
                            evidence_ref: row.get(4)?,
                            artifact_version: row.get(5)?,
                            observed_at: row.get(6)?,
                            accepted_at: row.get(7)?,
                            accepted_by: row.get(8)?,
                        },
                    ))
                })?;
                for row in rows {
                    let (key, evidence) = row?;
                    interim_evidence.entry(key).or_default().push(evidence);
                }
            }
            let mut interim_dispatches: Vec<(String, String, String, String, String)> = Vec::new();
            if Self::table_exists(connection, "project_track_dispatches")? {
                let mut statement = connection.prepare(
                    "SELECT slug, track, idempotency_key, mission_id, lease_expires_at \
                     FROM project_track_dispatches \
                     WHERE state IN ('reserved', 'started') AND mission_id IS NOT NULL",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?;
                interim_dispatches = rows.collect::<Result<Vec<_>, _>>()?;
            }
            let mut lease_rows: Vec<(String, String, String, String, String)> = Vec::new();
            // Per project: corrections for the reconcile receipt.
            let mut corrections: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
            // Alias rows reference `project_tracks(id)`, which the legacy
            // table does not have yet: buffer them until after the rename.
            let mut aliases: Vec<(String, String, String, &'static str)> = Vec::new();
            // (slug, normalized key) → id of the row that won.
            let mut winners: HashMap<(String, String), String> = HashMap::new();
            for row in legacy {
                let key = normalize_track_key(&row.track);
                let slug_corrections = corrections.entry(row.slug.clone()).or_default();
                if let Some(winner) = winners.get(&(row.slug.clone(), key.clone())) {
                    // Rows are ordered newest-first, so the winner is the most
                    // recently updated spelling. Keep this one resolvable.
                    aliases.push((
                        row.slug.clone(),
                        row.track.clone(),
                        winner.clone(),
                        "normalized_collision",
                    ));
                    slug_corrections.push(serde_json::json!({
                        "op": "normalized_collision",
                        "alias": row.track,
                        "into": key,
                        "ambiguous": true,
                        "note": "two spellings normalize to one key; the newer row won, the older spelling is an alias — verify they are the same track",
                    }));
                    continue;
                }
                let id = Uuid::new_v4().to_string();
                let status = row
                    .status
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let interim_lifecycle = row
                    .lifecycle
                    .as_deref()
                    .map(str::trim)
                    .map(str::to_ascii_lowercase);
                let lifecycle = match (interim_lifecycle.as_deref(), status) {
                    (Some("cancelled"), _) | (None, Some("cancelled")) => "cancelled",
                    _ => "active",
                };
                let blocker: Option<&str> = match interim_lifecycle.as_deref() {
                    Some("blocked") => Some("blocked"),
                    _ => None,
                };
                let position = row.position.unwrap_or(0).max(0);
                let start_revision = row.revision.unwrap_or(0).max(0);
                let title = row
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        row.desired_state
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| {
                        slug_corrections.push(serde_json::json!({
                            "op": "title_synthesized",
                            "track": key,
                            "from_key": row.track,
                        }));
                        humanize_track_key(&row.track)
                    });
                let criteria = row
                    .acceptance_criteria
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                    .unwrap_or_default();
                let depends: Vec<String> = row
                    .depends_on
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|dep| normalize_track_key(&dep))
                    .collect();
                connection.execute(
                    "INSERT INTO project_tracks_v2 \
                       (id, slug, track, title, desired_state, lifecycle, origin, explicit_blocker, \
                        acceptance_criteria, depends_on, revision, position, governed_artifact_version, \
                        created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'declared', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                    params![
                        id,
                        row.slug,
                        key,
                        title,
                        row.desired_state,
                        lifecycle,
                        blocker,
                        serde_json::to_string(&criteria).unwrap_or_else(|_| "[]".into()),
                        serde_json::to_string(&depends).unwrap_or_else(|_| "[]".into()),
                        start_revision,
                        position,
                        row.governed_artifact_version,
                        row.updated_at,
                    ],
                )?;
                // Interim evidence becomes real accept receipts (verified),
                // one per accepted criterion; an interim `satisfied` without
                // evidence is a claim only.
                let evidence = interim_evidence
                    .remove(&(row.slug.clone(), row.track.clone()))
                    .unwrap_or_default();
                let mut imported_evidence = 0usize;
                for (index, item) in evidence.iter().enumerate() {
                    let criterion_id = if item.criterion == "__track__" {
                        None
                    } else {
                        Some(item.criterion.clone())
                    };
                    let verifier_kind = match item.verifier_class.as_str() {
                        "review" | "command" | "external_state" | "manual" => {
                            item.verifier_class.as_str()
                        }
                        _ => "operator",
                    };
                    Self::insert_receipt_row(
                        connection,
                        &NewReceipt {
                            idempotency_key: format!(
                                "accept:interim:{}:{}:{}:{}",
                                row.slug, key, index, item.accepted_at
                            ),
                            kind: "accept".into(),
                            project_slug: Some(row.slug.clone()),
                            track_id: Some(id.clone()),
                            criterion_id,
                            subject_type: evidence_subject_type(verifier_kind).into(),
                            subject_id: format!("{}@{}", item.evidence_ref, item.artifact_version),
                            outcome: "succeeded".into(),
                            actor_type: "operator".into(),
                            actor_id: item.accepted_by.clone(),
                            verifier: Some(item.verifier_class.clone()),
                            supersedes_receipt_id: None,
                            observed_at: item.observed_at.clone(),
                            payload: serde_json::json!({
                                "evidence_kind": item.verifier_class,
                                "evidence_ref": item.evidence_ref,
                                "artifact_version": item.artifact_version,
                                "accepted_at": item.accepted_at,
                                "track": key,
                                "source": "project_track_evidence",
                            }),
                        },
                        &now,
                    )?;
                    imported_evidence += 1;
                }
                if imported_evidence > 0 {
                    slug_corrections.push(serde_json::json!({
                        "op": "interim_evidence_imported",
                        "track": key,
                        "receipts": imported_evidence,
                    }));
                }
                for (dslug, _dtrack, dkey, mission_id, lease_until) in interim_dispatches
                    .iter()
                    .filter(|(dslug, dtrack, _, _, _)| dslug == &row.slug && dtrack == &row.track)
                {
                    lease_rows.push((
                        dslug.clone(),
                        id.clone(),
                        mission_id.clone(),
                        format!("lease:dispatch:{dkey}"),
                        lease_until.clone(),
                    ));
                }
                if key != row.track {
                    aliases.push((row.slug.clone(), row.track.clone(), id.clone(), "renamed"));
                    slug_corrections.push(serde_json::json!({
                        "op": "key_normalized",
                        "track": key,
                        "from": row.track,
                    }));
                }
                let interim_satisfied = interim_lifecycle.as_deref() == Some("satisfied");
                let claims_done =
                    matches!(status, Some("done") | Some("closed")) || interim_satisfied;
                match (claims_done && imported_evidence == 0, status) {
                    (true, _) => {
                        Self::insert_receipt_row(
                            connection,
                            &NewReceipt {
                                idempotency_key: format!(
                                    "legacy_import:{}:{}:{}",
                                    row.slug, key, row.updated_at
                                ),
                                kind: "legacy_import".into(),
                                project_slug: Some(row.slug.clone()),
                                track_id: Some(id.clone()),
                                criterion_id: None,
                                subject_type: "migration".into(),
                                subject_id: format!("project_tracks:{}:{}", row.slug, row.track),
                                outcome: "observed".into(),
                                actor_type: "system".into(),
                                actor_id: "projects_store::migrate_tracks_v2".into(),
                                verifier: None,
                                supersedes_receipt_id: None,
                                observed_at: row.updated_at.clone(),
                                payload: serde_json::json!({
                                    "legacy_status": status,
                                    "note": "marked done before receipts existed; claim only",
                                }),
                            },
                            &now,
                        )?;
                        slug_corrections.push(serde_json::json!({
                            "op": "legacy_done_to_claim_only",
                            "track": key,
                            "legacy_status": status,
                            "interim_lifecycle": interim_lifecycle,
                        }));
                    }
                    (false, Some("cancelled")) => {}
                    (false, other) => {
                        if imported_evidence == 0 && !matches!(other, None | Some("open")) {
                            slug_corrections.push(serde_json::json!({
                                "op": "status_normalized",
                                "track": key,
                                "from": other,
                                "to": "open",
                            }));
                        }
                    }
                }
                winners.insert((row.slug.clone(), key), id);
            }
            for (slug, corrections) in corrections {
                if corrections.is_empty() {
                    continue;
                }
                Self::insert_receipt_row(
                    connection,
                    &NewReceipt {
                        idempotency_key: format!("reconcile:migrate_tracks_v2:{slug}:{now}"),
                        kind: "reconcile".into(),
                        project_slug: Some(slug.clone()),
                        track_id: None,
                        criterion_id: None,
                        subject_type: "migration".into(),
                        subject_id: "project_tracks_v2".into(),
                        outcome: "observed".into(),
                        actor_type: "system".into(),
                        actor_id: "projects_store::migrate_tracks_v2".into(),
                        verifier: None,
                        supersedes_receipt_id: None,
                        observed_at: now.clone(),
                        payload: serde_json::json!({ "corrections": corrections }),
                    },
                    &now,
                )?;
            }
            connection.execute_batch(
                "DROP TABLE project_tracks;
                 ALTER TABLE project_tracks_v2 RENAME TO project_tracks;",
            )?;
            for (slug, alias, track_id, reason) in aliases {
                connection.execute(
                    "INSERT OR IGNORE INTO project_track_aliases \
                       (slug, alias_key, track_id, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![slug, alias, track_id, reason, now],
                )?;
            }
            // Interim dispatch intents that still owned a track become writer
            // leases so the running mission keeps its exclusivity.
            for (slug, track_id, mission_id, key, lease_until) in lease_rows {
                connection.execute(
                    "INSERT OR IGNORE INTO track_leases \
                       (id, slug, track_id, mutation_domain, attempt_id, mode, state, lease_until, \
                        idempotency_key, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, 'track', ?4, 'writer', 'active', ?5, ?6, ?7, ?7)",
                    params![
                        Uuid::new_v4().to_string(),
                        slug,
                        track_id,
                        mission_id,
                        lease_until,
                        key,
                        now
                    ],
                )?;
            }
            Self::stamp_schema_version(connection)?;
            Ok(())
        })();
        match result {
            Ok(()) => connection.execute_batch("COMMIT"),
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn stamp_schema_version(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute(
            "INSERT INTO schema_version (id, version, migrated_at) VALUES (1, ?1, ?2) \
             ON CONFLICT(id) DO UPDATE SET version = excluded.version, \
               migrated_at = CASE WHEN schema_version.version = excluded.version \
                                  THEN schema_version.migrated_at ELSE excluded.migrated_at END",
            params![SCHEMA_VERSION, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT version FROM schema_version WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
            .map(|version| version.unwrap_or(0))
    }

    /// Append one receipt on an already-held connection (migration / txn).
    fn insert_receipt_row(
        connection: &Connection,
        receipt: &NewReceipt,
        now: &str,
    ) -> rusqlite::Result<Receipt> {
        let id = Uuid::new_v4().to_string();
        // `receipts.project_slug` references `projects(slug)`. Tracks (and
        // legacy rows) can live under alias/orphan slugs that have no roster
        // record; such receipts keep the slug in the payload and leave the
        // FK column NULL instead of failing the whole transaction.
        let project_slug = match receipt.project_slug.as_deref() {
            Some(slug) => {
                let known: Option<i64> = connection
                    .query_row(
                        "SELECT 1 FROM projects WHERE slug = ?1",
                        params![slug],
                        |row| row.get(0),
                    )
                    .optional()?;
                if known.is_some() {
                    Some(slug.to_string())
                } else {
                    tracing::warn!(
                        slug,
                        kind = %receipt.kind,
                        "receipt for a slug with no roster record; stored without project link"
                    );
                    None
                }
            }
            None => None,
        };
        let mut payload_value = receipt.payload.clone();
        if project_slug.is_none() {
            if let (Some(slug), Some(object)) = (
                receipt.project_slug.as_deref(),
                payload_value.as_object_mut(),
            ) {
                object.insert(
                    "unlinked_slug".into(),
                    serde_json::Value::String(slug.into()),
                );
            }
        }
        let payload = serde_json::to_string(&payload_value).unwrap_or_else(|_| "{}".into());
        let request_hash = receipt.request_hash();
        connection.execute(
            "INSERT INTO receipts \
               (id, idempotency_key, request_hash, kind, project_slug, track_id, criterion_id, \
                subject_type, subject_id, outcome, actor_type, actor_id, verifier, \
                supersedes_receipt_id, observed_at, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                id,
                receipt.idempotency_key,
                request_hash,
                receipt.kind,
                project_slug,
                receipt.track_id,
                receipt.criterion_id,
                receipt.subject_type,
                receipt.subject_id,
                receipt.outcome,
                receipt.actor_type,
                receipt.actor_id,
                receipt.verifier,
                receipt.supersedes_receipt_id,
                receipt.observed_at,
                payload,
                now,
            ],
        )?;
        Ok(Receipt {
            id,
            idempotency_key: receipt.idempotency_key.clone(),
            kind: receipt.kind.clone(),
            project_slug,
            track_id: receipt.track_id.clone(),
            criterion_id: receipt.criterion_id.clone(),
            subject_type: receipt.subject_type.clone(),
            subject_id: receipt.subject_id.clone(),
            outcome: receipt.outcome.clone(),
            actor_type: receipt.actor_type.clone(),
            actor_id: receipt.actor_id.clone(),
            verifier: receipt.verifier.clone(),
            supersedes_receipt_id: receipt.supersedes_receipt_id.clone(),
            observed_at: receipt.observed_at.clone(),
            payload: receipt.payload.clone(),
            created_at: now.to_string(),
        })
    }

    /// Additive column migration; returns true when the column was added.
    fn ensure_column(
        connection: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> rusqlite::Result<bool> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let exists = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == column);
        drop(statement);
        if exists {
            return Ok(false);
        }
        connection.execute(&format!("ALTER TABLE {table} ADD COLUMN {definition}"), [])?;
        Ok(true)
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
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

    /// Bind stored under the canonical slug; drop nickname rows that fold onto it.
    pub fn set_canonical_binding(
        &self,
        canonical: &str,
        session_id: &str,
        aliases: &[String],
        bound_by: Option<&str>,
    ) -> Result<ProjectConversation, String> {
        let conversation = self.set_binding(canonical, session_id, bound_by)?;
        for alias in aliases {
            if alias != canonical {
                self.clear_binding(alias)?;
            }
        }
        Ok(conversation)
    }

    /// Canonical bind first, then any nickname row (one-release fallback).
    pub fn binding_for_canonical(
        &self,
        canonical: &str,
        aliases: &[String],
    ) -> Result<Option<ProjectConversation>, String> {
        if let Some(found) = self.binding(canonical)? {
            return Ok(Some(found));
        }
        for alias in aliases {
            if alias == canonical {
                continue;
            }
            if let Some(found) = self.binding(alias)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
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
    /// count. Returns the resulting observation count, or `0` when this `at`
    /// was already counted (or is older than the open event) so ingest can
    /// skip mode.
    pub fn record_state(
        &self,
        slug: &str,
        signature: &str,
        headline: Option<&str>,
        at: &str,
        session_id: Option<&str>,
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
                    return Ok(0);
                }
                connection
                    .execute(
                        "UPDATE project_state_events \
                         SET last_seen_at = ?1, observations = observations + 1, \
                             headline = COALESCE(?2, headline), \
                             session_id = COALESCE(?5, session_id) \
                         WHERE slug = ?3 AND first_seen_at = ?4",
                        params![at, headline, slug, first_seen_at, session_id],
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
                   (slug, signature, headline, first_seen_at, last_seen_at, observations, session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?4, 1, ?5) \
                 ON CONFLICT(slug, first_seen_at) DO NOTHING",
                params![slug, signature, headline, at, session_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(1)
    }

    /// Record a `[SILENT]` (or headline-less) delivery for `slug` at `at`.
    ///
    /// `[SILENT]` is the controllers-policy convention for "nothing to
    /// report": the observation must advance freshness (so the controller is
    /// not flagged silent — that is the point of emitting it), but it must
    /// never become the headline the board shows. So instead of opening a new
    /// state event, the newest event is extended — last_seen_at/observations
    /// move forward while signature and headline stay what the last meaningful
    /// delivery said, like a repeated-signature fold. Only when no event
    /// exists at all is one created, with `descriptor` and no headline.
    ///
    /// Idempotent on `at`, same as [`Self::record_state`]. Returns the
    /// resulting observation count, or `0` when this `at` was already counted.
    pub fn record_silent_observation(
        &self,
        slug: &str,
        descriptor: &str,
        at: &str,
        session_id: Option<&str>,
    ) -> Result<u32, String> {
        let connection = self.lock()?;
        let current: Option<(String, String, u32)> = connection
            .query_row(
                "SELECT first_seen_at, last_seen_at, observations \
                 FROM project_state_events WHERE slug = ?1 \
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![slug],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        if let Some((first_seen_at, last_seen_at, observations)) = current {
            // Already counted, or older than what we have: nothing to do.
            if at <= last_seen_at.as_str() {
                return Ok(0);
            }
            connection
                .execute(
                    "UPDATE project_state_events \
                     SET last_seen_at = ?1, observations = observations + 1, \
                         session_id = COALESCE(?4, session_id) \
                     WHERE slug = ?2 AND first_seen_at = ?3",
                    params![at, slug, first_seen_at, session_id],
                )
                .map_err(|e| e.to_string())?;
            return Ok(observations + 1);
        }

        connection
            .execute(
                "INSERT INTO project_state_events \
                   (slug, signature, headline, first_seen_at, last_seen_at, observations, session_id) \
                 VALUES (?1, ?2, NULL, ?3, ?3, 1, ?4) \
                 ON CONFLICT(slug, first_seen_at) DO NOTHING",
                params![slug, descriptor, at, session_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(1)
    }

    /// Advance freshness for an inspect callback without opening a chapter
    /// or incrementing the stall counter.
    ///
    /// Mission-complete inspect callbacks used to become `latest_update` and
    /// trip "same state on 3 consecutive updates" while the controller had
    /// already told the operator there was nothing to do (Verity #2397).
    pub fn touch_freshness(
        &self,
        slug: &str,
        at: &str,
        session_id: Option<&str>,
    ) -> Result<u32, String> {
        let connection = self.lock()?;
        let current: Option<(String, String, u32)> = connection
            .query_row(
                "SELECT first_seen_at, last_seen_at, observations \
                 FROM project_state_events WHERE slug = ?1 \
                 ORDER BY last_seen_at DESC LIMIT 1",
                params![slug],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((first_seen_at, last_seen_at, observations)) = current else {
            return Ok(0);
        };
        if at <= last_seen_at.as_str() {
            return Ok(0);
        }
        connection
            .execute(
                "UPDATE project_state_events \
                 SET last_seen_at = ?1, \
                     session_id = COALESCE(?4, session_id) \
                 WHERE slug = ?2 AND first_seen_at = ?3",
                params![at, slug, first_seen_at, session_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(observations)
    }

    /// A project's state history, newest first.
    pub fn state_timeline(&self, slug: &str, limit: usize) -> Result<Vec<ProjectState>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT signature, headline, first_seen_at, last_seen_at, observations, session_id \
                 FROM project_state_events WHERE slug = ?1 \
                 ORDER BY last_seen_at DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug, limit as i64], Self::state_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    fn state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectState> {
        Ok(ProjectState {
            signature: row.get(0)?,
            headline: row.get(1)?,
            first_seen_at: row.get(2)?,
            last_seen_at: row.get(3)?,
            observations: row.get(4)?,
            session_id: row.get(5)?,
        })
    }

    /// The state each project is currently in, keyed by slug.
    ///
    /// One query for the whole overview: this is what lets the board render
    /// `latest_update` without scanning the Hermes delivery log per request.
    pub fn latest_states(&self) -> Result<HashMap<String, ProjectState>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, signature, headline, first_seen_at, last_seen_at, \
                        observations, session_id \
                 FROM project_state_events e \
                 WHERE last_seen_at = ( \
                   SELECT MAX(last_seen_at) FROM project_state_events \
                   WHERE slug = e.slug \
                 )",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ProjectState {
                        signature: row.get(1)?,
                        headline: row.get(2)?,
                        first_seen_at: row.get(3)?,
                        last_seen_at: row.get(4)?,
                        observations: row.get(5)?,
                        session_id: row.get(6)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Newest non-inspect chapter per slug. Inspect callbacks must not own
    /// the board headline or the stall counter; they only refresh `last_seen_at`
    /// via [`Self::touch_freshness`].
    pub fn latest_controller_states(&self) -> Result<HashMap<String, ProjectState>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, signature, headline, first_seen_at, last_seen_at, \
                        observations, session_id \
                 FROM project_state_events e \
                 WHERE IFNULL(signature, '') NOT LIKE 'mission-callback%' \
                   AND IFNULL(headline, '') NOT LIKE '[Mission callback:%' \
                   AND last_seen_at = ( \
                     SELECT MAX(last_seen_at) FROM project_state_events \
                     WHERE slug = e.slug \
                       AND IFNULL(signature, '') NOT LIKE 'mission-callback%' \
                       AND IFNULL(headline, '') NOT LIKE '[Mission callback:%' \
                   )",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ProjectState {
                        signature: row.get(1)?,
                        headline: row.get(2)?,
                        first_seen_at: row.get(3)?,
                        last_seen_at: row.get(4)?,
                        observations: row.get(5)?,
                        session_id: row.get(6)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Total deliveries folded into each project's timeline (sum of
    /// observations across its state rows), keyed by slug. The durable form of
    /// the overview's old per-request `updates_count`.
    pub fn state_event_totals(&self) -> Result<HashMap<String, usize>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT slug, SUM(observations) FROM project_state_events GROUP BY slug")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as usize,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())
    }

    /// How many unrouted deliveries the store retains. A triage inbox, not an
    /// archive: old entries beyond this are dropped on insert.
    const UNROUTED_RETENTION: usize = 50;

    /// Record a delivery the ingestor could not attribute to a project.
    ///
    /// Idempotent on `(session_id, at)` — the ingestor replays an overlapping
    /// window every cycle. Retention is enforced here rather than by a sweeper.
    pub fn record_unrouted(
        &self,
        session_id: &str,
        at: &str,
        headline: &str,
        signature: Option<&str>,
        mode: Option<&str>,
        blocker: Option<&str>,
    ) -> Result<(), String> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO unrouted_deliveries \
                   (session_id, at, headline, signature, mode, blocker) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(session_id, at) DO NOTHING",
                params![session_id, at, headline, signature, mode, blocker],
            )
            .map_err(|e| e.to_string())?;
        connection
            .execute(
                "DELETE FROM unrouted_deliveries WHERE (session_id, at) NOT IN ( \
                   SELECT session_id, at FROM unrouted_deliveries \
                   ORDER BY at DESC LIMIT ?1)",
                params![Self::UNROUTED_RETENTION as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The newest unrouted deliveries, newest first.
    pub fn unrouted(&self, limit: usize) -> Result<Vec<UnroutedDelivery>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT session_id, at, headline, signature, mode, blocker \
                 FROM unrouted_deliveries ORDER BY at DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok(UnroutedDelivery {
                    session_id: row.get(0)?,
                    at: row.get(1)?,
                    headline: row.get(2)?,
                    signature: row.get(3)?,
                    mode: row.get(4)?,
                    blocker: row.get(5)?,
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
                 created_at, updated_at, mode_signal_at FROM projects \
                 ORDER BY updated_at DESC, slug",
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
                 created_at, updated_at, mode_signal_at FROM projects WHERE slug = ?1",
                params![slug],
                Self::project_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Remove the authoritative project object and every project-owned row.
    ///
    /// Missions and Hermes conversation messages live in separate stores and
    /// are deliberately not touched here. Callers that offer a wider delete
    /// must remove those explicitly before committing this project deletion.
    pub fn delete_project(&self, slug: &str) -> Result<bool, String> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM project_bindings WHERE slug = ?1",
                params![slug],
            )
            .map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM project_state_events WHERE slug = ?1",
                params![slug],
            )
            .map_err(|e| e.to_string())?;
        transaction
            .execute("DELETE FROM project_tracks WHERE slug = ?1", params![slug])
            .map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM project_decisions WHERE slug = ?1",
                params![slug],
            )
            .map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM project_roadmap_proposals WHERE slug = ?1",
                params![slug],
            )
            .map_err(|e| e.to_string())?;
        for table in [
            "project_imports",
            "project_track_aliases",
            "track_leases",
            "receipts",
        ] {
            let column = if table == "receipts" {
                "project_slug"
            } else {
                "slug"
            };
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE {column} = ?1"),
                    params![slug],
                )
                .map_err(|e| e.to_string())?;
        }
        let removed = transaction
            .execute("DELETE FROM projects WHERE slug = ?1", params![slug])
            .map_err(|e| e.to_string())?
            > 0;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(removed)
    }

    /// Move a project to a new slug across every table that keys on it, in one
    /// transaction. This is only the store-side move: the caller owns alias
    /// forwarding (routes.json) so external references — mission tags, cron
    /// `deliver: project:<old>`, `[CTRL:]` signatures — keep resolving.
    ///
    /// Refuses to clobber: the target slug must not already be a roster
    /// project. Historical rows that would collide under the new slug (state
    /// events, tracks, decisions ingested under it before the rename) keep the
    /// target's copy and drop the source's, so the move never fails midway on
    /// a primary-key conflict.
    pub fn rename_project(&self, old: &str, new: &str) -> Result<ProjectRecord, String> {
        if old == new {
            return Err("new slug is identical to the current one".to_string());
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|e| e.to_string())?;
        // `project_grant` references projects(slug) with no ON UPDATE action,
        // so parent and child must both move before constraints are checked.
        transaction
            .pragma_update(None, "defer_foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM projects WHERE slug = ?1",
                params![old],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if exists.is_none() {
            return Err(format!("project '{old}' not found"));
        }
        let taken: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM projects WHERE slug = ?1",
                params![new],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if taken.is_some() {
            return Err(format!(
                "project '{new}' already exists — renaming onto an existing project is a merge, which stays explicit via the alias map"
            ));
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE projects SET slug = ?2, updated_at = ?3 WHERE slug = ?1",
                params![old, new, now],
            )
            .map_err(|e| e.to_string())?;
        transaction
            .execute(
                "UPDATE receipts SET project_slug = ?2 WHERE project_slug = ?1",
                params![old, new],
            )
            .map_err(|e| e.to_string())?;
        for table in [
            "project_grant",
            "project_bindings",
            "project_tracks",
            "project_track_aliases",
            "track_leases",
            "project_imports",
            "project_state_events",
            "project_decisions",
            "project_roadmap_proposals",
        ] {
            transaction
                .execute(
                    &format!("UPDATE OR IGNORE {table} SET slug = ?2 WHERE slug = ?1"),
                    params![old, new],
                )
                .map_err(|e| e.to_string())?;
            // Rows OR IGNORE skipped collided with an existing row under the
            // new slug; the target's copy wins and the leftover is dropped.
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE slug = ?1"),
                    params![old],
                )
                .map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        drop(connection);
        self.get_project(new)?
            .ok_or_else(|| "project vanished after rename".to_string())
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
            mode_signal_at: row.get(12)?,
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
    /// not a scan of the delivery log. Rephrasing `next=` does not reset wait.
    /// A watch-idle next-action yields to an implement-ready stored track.
    pub fn set_mode(
        &self,
        slug: &str,
        mode: &str,
        next_action: Option<&str>,
        blocker: Option<&str>,
    ) -> Result<(), String> {
        let next_action = self.honest_next_action_for(slug, next_action)?;
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
                 updated_at = ?6, mode_signal_at = ?6 WHERE slug = ?1",
                params![slug, mode, next_action, blocker, unchanged, now],
            )
            .map_err(|e| e.to_string())?;
        if affected == 0 {
            return Err(format!("unknown project '{slug}'"));
        }
        Ok(())
    }

    fn honest_next_action_for(
        &self,
        slug: &str,
        reported: Option<&str>,
    ) -> Result<Option<String>, String> {
        let tracks = self.tracks(slug)?;
        let tuples: Vec<(String, Option<String>, Option<String>)> = tracks
            .into_iter()
            .map(|track| (track.track, track.desired_state, track.status))
            .collect();
        Ok(super::controller_honesty::honest_next_action(
            reported, &tuples,
        ))
    }

    /// Project the mode onto an existing project record from the delivery
    /// ingestor. Unlike `set_mode`, this is **idempotent** (it does not count
    /// ticks — `wait` is passed in from the state timeline's observation count)
    /// so the ingestor replaying the same delivery every cycle is harmless. It
    /// silently no-ops for an unknown slug: the ingestor must not fabricate a
    /// project from a routed trailer.
    pub fn project_mode_from_signal(
        &self,
        slug: &str,
        mode: &str,
        wait: i64,
        next_action: Option<&str>,
        blocker: Option<&str>,
        at: Option<&str>,
    ) -> Result<(), String> {
        let next_action = self.honest_next_action_for(slug, next_action)?;
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let previous: Option<(Option<String>, Option<String>, i64)> = connection
            .query_row(
                "SELECT mode, blocker, wait_ticks FROM projects WHERE slug = ?1",
                params![slug],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let wait = match previous {
            Some((m, b, stored)) if m.as_deref() == Some(mode) && b.as_deref() == blocker => {
                // Same stall: never let a rephrased next= / wait=0 reset the
                // counter. Overlapping ingest of the same delivery can still
                // pass the same wait twice without inflating it.
                stored.max(wait)
            }
            _ => wait,
        };
        match at {
            Some(at) => {
                let watermark: Option<String> = connection
                    .query_row(
                        "SELECT mode_signal_at FROM projects WHERE slug = ?1",
                        params![slug],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?
                    .flatten();
                if watermark
                    .as_deref()
                    .is_some_and(|watermark| !rfc3339_at_least(at, watermark))
                {
                    return Ok(());
                }
                connection
                    .execute(
                        "UPDATE projects SET mode = ?2, wait_ticks = ?3, \
                         next_action = COALESCE(?4, next_action), \
                         blocker = ?5, updated_at = ?6, mode_signal_at = ?7 \
                         WHERE slug = ?1",
                        params![slug, mode, wait, next_action, blocker, now, at],
                    )
                    .map_err(|e| e.to_string())?;
            }
            None => {
                connection
                    .execute(
                        "UPDATE projects SET mode = ?2, wait_ticks = ?3, \
                         next_action = COALESCE(?4, next_action), \
                         blocker = ?5, updated_at = ?6 WHERE slug = ?1",
                        params![slug, mode, wait, next_action, blocker, now],
                    )
                    .map_err(|e| e.to_string())?;
            }
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
        Self::tracks_on(&connection, slug)
    }

    fn tracks_on(connection: &Connection, slug: &str) -> Result<Vec<ProjectTrack>, String> {
        let mut statement = connection
            .prepare(TRACK_SELECT)
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug], track_row)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut claims: HashMap<String, Vec<StandingClaim>> = HashMap::new();
        let mut statement = connection
            .prepare(STANDING_CLAIMS_FOR_SLUG)
            .map_err(|e| e.to_string())?;
        for entry in statement
            .query_map(params![slug], standing_claim_from_row)
            .map_err(|e| e.to_string())?
        {
            let (track_id, claim) = entry.map_err(|e| e.to_string())?;
            claims.entry(track_id).or_default().push(claim);
        }
        Ok(rows
            .into_iter()
            .map(|row| {
                let standing = claims.get(&row.id).map(Vec::as_slice).unwrap_or(&[]);
                finish_track(row, standing)
            })
            .collect())
    }

    /// One track by normalized key or alias, within `slug`.
    pub fn track(&self, slug: &str, key: &str) -> Result<Option<ProjectTrack>, String> {
        let connection = self.lock()?;
        Self::track_on(&connection, slug, key)
    }

    fn track_on(
        connection: &Connection,
        slug: &str,
        key: &str,
    ) -> Result<Option<ProjectTrack>, String> {
        let Some(id) = Self::resolve_track_id_on(connection, slug, key)? else {
            return Ok(None);
        };
        let Some(row) = connection
            .query_row(TRACK_SELECT_BY_ID, params![id], track_row)
            .optional()
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let mut statement = connection
            .prepare(STANDING_CLAIMS_FOR_TRACK)
            .map_err(|e| e.to_string())?;
        let claims: Vec<StandingClaim> = statement
            .query_map(params![id], standing_claim_from_row)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|(_, claim)| claim))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(Some(finish_track(row, &claims)))
    }

    /// Resolve a caller-supplied key (any spelling) to the row id. Exact
    /// normalized key first, then the alias table.
    fn resolve_track_id_on(
        connection: &Connection,
        slug: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        let normalized = normalize_track_key(key);
        if normalized.is_empty() {
            return Ok(None);
        }
        let direct: Option<String> = connection
            .query_row(
                "SELECT id FROM project_tracks WHERE slug = ?1 AND track = ?2",
                params![slug, normalized],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if direct.is_some() {
            return Ok(direct);
        }
        connection
            .query_row(
                "SELECT track_id FROM project_track_aliases WHERE slug = ?1 AND (alias_key = ?2 OR alias_key = ?3)",
                params![slug, key.trim(), normalized],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Canonical key for any spelling, if the track exists.
    pub fn resolve_track_key(&self, slug: &str, key: &str) -> Result<Option<String>, String> {
        let connection = self.lock()?;
        let Some(id) = Self::resolve_track_id_on(&connection, slug, key)? else {
            return Ok(None);
        };
        connection
            .query_row(
                "SELECT track FROM project_tracks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Slug that actually owns this track among `slugs` (canonical + aliases).
    pub fn find_track_slug(&self, slugs: &[String], track: &str) -> Result<Option<String>, String> {
        let connection = self.lock()?;
        for slug in slugs {
            if Self::resolve_track_id_on(&connection, slug, track)?.is_some() {
                return Ok(Some(slug.clone()));
            }
        }
        Ok(None)
    }

    /// Declare or update one workstream. `status` is the legacy vocabulary
    /// and maps onto lifecycle / blocker; it can never mark a track done —
    /// that takes a receipt (`accept_track_evidence`).
    pub fn set_track(
        &self,
        slug: &str,
        track: &str,
        desired_state: Option<&str>,
        status: Option<&str>,
    ) -> Result<(), TrackWriteError> {
        self.patch_track(slug, track, desired_state, status, None, None, None)
    }

    /// Partial update: NULL / omitted contract fields leave the stored values.
    /// Creates the row (`origin = declared`) when the key is unknown.
    #[allow(clippy::too_many_arguments)]
    pub fn patch_track(
        &self,
        slug: &str,
        track: &str,
        desired_state: Option<&str>,
        status: Option<&str>,
        title: Option<&str>,
        acceptance_criteria: Option<&[String]>,
        depends_on: Option<&[String]>,
    ) -> Result<(), TrackWriteError> {
        let transition = TrackTransition::from_legacy_status(status)?;
        let key = normalize_track_key(track);
        if key.is_empty() {
            return Err(TrackWriteError::Invalid(format!(
                "track key '{track}' is empty after normalization"
            )));
        }
        let now = Utc::now().to_rfc3339();
        let criteria = acceptance_criteria
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| TrackWriteError::Store(e.to_string()))?;
        let depends = depends_on
            .map(|deps| {
                deps.iter()
                    .map(|dep| normalize_track_key(dep))
                    .collect::<Vec<_>>()
            })
            .map(|deps| serde_json::to_string(&deps))
            .transpose()
            .map_err(|e| TrackWriteError::Store(e.to_string()))?;
        let mut connection = self.lock().map_err(TrackWriteError::Store)?;
        let tx = connection
            .transaction()
            .map_err(|e| TrackWriteError::Store(e.to_string()))?;
        let existing =
            Self::resolve_track_id_on(&tx, slug, track).map_err(TrackWriteError::Store)?;
        match existing {
            Some(id) => {
                let (lifecycle_sql, blocker_sql) = transition.update_clauses();
                tx.execute(
                    &format!(
                        "UPDATE project_tracks SET \
                           desired_state = COALESCE(?2, desired_state), \
                           title = COALESCE(?3, title), \
                           acceptance_criteria = COALESCE(?4, acceptance_criteria), \
                           depends_on = COALESCE(?5, depends_on), \
                           {lifecycle_sql} \
                           {blocker_sql} \
                           revision = revision + 1, \
                           updated_at = ?6 \
                         WHERE id = ?1"
                    ),
                    params![id, desired_state, title, criteria, depends, now],
                )
                .map_err(|e| TrackWriteError::Store(e.to_string()))?;
            }
            None => {
                let title = title
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        desired_state
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| humanize_track_key(track));
                let (lifecycle, blocker) = transition.insert_values();
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO project_tracks \
                       (id, slug, track, title, desired_state, lifecycle, origin, explicit_blocker, \
                        acceptance_criteria, depends_on, revision, position, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'declared', ?7, ?8, ?9, 0, \
                       (SELECT COALESCE(MAX(position), -1) + 1 FROM project_tracks WHERE slug = ?2), ?10, ?10)",
                    params![
                        id,
                        slug,
                        key,
                        title,
                        desired_state,
                        lifecycle,
                        blocker,
                        criteria.unwrap_or_else(|| "[]".into()),
                        depends.unwrap_or_else(|| "[]".into()),
                        now
                    ],
                )
                .map_err(|e| TrackWriteError::Store(e.to_string()))?;
                if key != track.trim() {
                    tx.execute(
                        "INSERT OR IGNORE INTO project_track_aliases \
                           (slug, alias_key, track_id, reason, created_at) \
                         VALUES (?1, ?2, ?3, 'renamed', ?4)",
                        params![slug, track.trim(), id, now],
                    )
                    .map_err(|e| TrackWriteError::Store(e.to_string()))?;
                }
            }
        }
        tx.commit()
            .map_err(|e| TrackWriteError::Store(e.to_string()))
    }

    /// Plan items from chat: latest title + contract win, lifecycle returns
    /// to `active` (a re-planned cancelled key is un-cancelled). One
    /// transaction so a later invalid caller never leaves a prefix persisted.
    pub fn upsert_planned_tracks(&self, slug: &str, items: &[PlannedTrack]) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        for item in items {
            let key = normalize_track_key(&item.track);
            if key.is_empty() {
                return Err(format!(
                    "task_key '{}' is empty after normalization",
                    item.track
                ));
            }
            let title = item.title.trim();
            if title.is_empty() {
                return Err(format!("task '{}' needs a title", item.track));
            }
            let criteria =
                serde_json::to_string(&item.acceptance_criteria).map_err(|e| e.to_string())?;
            let depends: Vec<String> = item
                .depends_on
                .iter()
                .map(|dep| normalize_track_key(dep))
                .collect();
            let depends = serde_json::to_string(&depends).map_err(|e| e.to_string())?;
            let existing = Self::resolve_track_id_on(&tx, slug, &item.track)?;
            match existing {
                Some(id) => {
                    // Planning never reopens or revises a satisfied track:
                    // its standing evidence stays authoritative. It may
                    // still reorder it.
                    let satisfied = Self::track_on(&tx, slug, &item.track)?
                        .is_some_and(|track| track.claim.as_deref() == Some("accept"));
                    if satisfied {
                        tx.execute(
                            "UPDATE project_tracks SET position = COALESCE(?2, position) WHERE id = ?1",
                            params![id, item.position],
                        )
                        .map_err(|e| e.to_string())?;
                    } else {
                        tx.execute(
                            "UPDATE project_tracks SET \
                               title = ?2, desired_state = ?3, acceptance_criteria = ?4, depends_on = ?5, \
                               origin = 'declared', explicit_blocker = NULL, \
                               position = COALESCE(?7, position), \
                               revision = revision + 1, updated_at = ?6 \
                             WHERE id = ?1",
                            params![id, title, item.desired_state, criteria, depends, now, item.position],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO project_tracks \
                           (id, slug, track, title, desired_state, lifecycle, origin, explicit_blocker, \
                            acceptance_criteria, depends_on, revision, position, created_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, 'active', 'declared', NULL, ?6, ?7, 0, \
                           COALESCE(?9, (SELECT COALESCE(MAX(position), -1) + 1 FROM project_tracks WHERE slug = ?2)), ?8, ?8)",
                        params![id, slug, key, title, item.desired_state, criteria, depends, now, item.position],
                    )
                    .map_err(|e| e.to_string())?;
                    if key != item.track.trim() {
                        tx.execute(
                            "INSERT OR IGNORE INTO project_track_aliases \
                               (slug, alias_key, track_id, reason, created_at) \
                             VALUES (?1, ?2, ?3, 'renamed', ?4)",
                            params![slug, item.track.trim(), id, now],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    // ── Receipts ────────────────────────────────────────────────────────

    /// Append one receipt. Idempotent on `idempotency_key`: the same key with
    /// the same body returns the stored row; the same key with a different
    /// body is a conflict.
    pub fn insert_receipt(&self, receipt: &NewReceipt) -> Result<Receipt, ReceiptWriteError> {
        let mut connection = self.lock().map_err(ReceiptWriteError::Store)?;
        let tx = connection
            .transaction()
            .map_err(|e| ReceiptWriteError::Store(e.to_string()))?;
        let result = Self::insert_receipt_on(&tx, receipt)?;
        tx.commit()
            .map_err(|e| ReceiptWriteError::Store(e.to_string()))?;
        Ok(result)
    }

    fn insert_receipt_on(
        connection: &Connection,
        receipt: &NewReceipt,
    ) -> Result<Receipt, ReceiptWriteError> {
        if let Some(prior) = Self::receipt_by_key_on(connection, &receipt.idempotency_key)
            .map_err(ReceiptWriteError::Store)?
        {
            let stored_hash: String = connection
                .query_row(
                    "SELECT request_hash FROM receipts WHERE id = ?1",
                    params![prior.id],
                    |row| row.get(0),
                )
                .map_err(|e| ReceiptWriteError::Store(e.to_string()))?;
            if stored_hash == receipt.request_hash() {
                return Ok(prior);
            }
            return Err(ReceiptWriteError::IdempotencyMismatch {
                idempotency_key: receipt.idempotency_key.clone(),
                prior_receipt_id: prior.id,
            });
        }
        let now = Utc::now().to_rfc3339();
        Self::insert_receipt_row(connection, receipt, &now)
            .map_err(|e| ReceiptWriteError::Store(e.to_string()))
    }

    fn receipt_by_key_on(connection: &Connection, key: &str) -> Result<Option<Receipt>, String> {
        connection
            .query_row(
                &format!("{RECEIPT_SELECT} WHERE idempotency_key = ?1"),
                params![key],
                receipt_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn receipt(&self, id: &str) -> Result<Option<Receipt>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                &format!("{RECEIPT_SELECT} WHERE id = ?1"),
                params![id],
                receipt_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Every receipt on one track, oldest first.
    pub fn receipts_for_track(&self, slug: &str, key: &str) -> Result<Vec<Receipt>, String> {
        let connection = self.lock()?;
        let Some(id) = Self::resolve_track_id_on(&connection, slug, key)? else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare(&format!(
                "{RECEIPT_SELECT} WHERE track_id = ?1 ORDER BY observed_at, created_at"
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![id], receipt_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// The newest `reconcile` receipt for the project that no `reconcile_ack`
    /// supersedes. Surfaced on `get_project` until the operator acknowledges.
    pub fn latest_unacked_reconcile(&self, slug: &str) -> Result<Option<Receipt>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                &format!(
                    "{RECEIPT_SELECT} WHERE project_slug = ?1 AND kind = 'reconcile' \
                       AND NOT EXISTS (SELECT 1 FROM receipts ack \
                                       WHERE ack.kind = 'reconcile_ack' \
                                         AND ack.supersedes_receipt_id = receipts.id) \
                     ORDER BY observed_at DESC, created_at DESC LIMIT 1"
                ),
                params![slug],
                receipt_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Acknowledge a reconcile receipt: appends `reconcile_ack`, mutates nothing.
    pub fn ack_reconcile(
        &self,
        slug: &str,
        receipt_id: &str,
        actor: &str,
    ) -> Result<Receipt, ReceiptWriteError> {
        let now = Utc::now().to_rfc3339();
        self.insert_receipt(&NewReceipt {
            idempotency_key: format!("reconcile_ack:{receipt_id}"),
            kind: "reconcile_ack".into(),
            project_slug: Some(slug.to_string()),
            track_id: None,
            criterion_id: None,
            subject_type: "receipt".into(),
            subject_id: receipt_id.to_string(),
            outcome: "observed".into(),
            actor_type: "operator".into(),
            actor_id: actor.to_string(),
            verifier: None,
            supersedes_receipt_id: Some(receipt_id.to_string()),
            observed_at: now,
            payload: serde_json::json!({}),
        })
    }

    // ── Acceptance ──────────────────────────────────────────────────────

    /// Satisfy a track with evidence. One transaction: revision check,
    /// evidence validation against the acceptance criteria, immutable
    /// receipts, revision bump. Retrying the same idempotency key with the
    /// same body replays the prior receipts without bumping the revision.
    pub fn accept_track_evidence(
        &self,
        slug: &str,
        key: &str,
        request: &AcceptRequest,
    ) -> Result<AcceptResult, AcceptError> {
        if request.evidence.is_empty() {
            return Err(AcceptError::Invalid("evidence is required".into()));
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(AcceptError::Invalid("idempotency_key is required".into()));
        }
        for (index, evidence) in request.evidence.iter().enumerate() {
            if !EVIDENCE_KINDS.contains(&evidence.kind.as_str()) {
                return Err(AcceptError::Invalid(format!(
                    "evidence[{index}].kind '{}' is not one of {}",
                    evidence.kind,
                    EVIDENCE_KINDS.join(" | ")
                )));
            }
            if evidence.subject_id.trim().is_empty() {
                return Err(AcceptError::Invalid(format!(
                    "evidence[{index}].subject_id is required (an immutable handle: PR head, commit, job id)"
                )));
            }
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock().map_err(AcceptError::Store)?;
        let tx = connection
            .transaction()
            .map_err(|e| AcceptError::Store(e.to_string()))?;
        let Some(track) = Self::track_on(&tx, slug, key).map_err(AcceptError::Store)? else {
            return Err(AcceptError::NotFound);
        };
        if track.lifecycle == "cancelled" {
            return Err(AcceptError::Invalid(format!(
                "track '{}' is cancelled; re-plan it before accepting evidence",
                track.track
            )));
        }
        // An idempotent retry (same key, first receipt already stored) must
        // return the prior response even though the first call bumped the
        // revision; a mismatching body still fails below.
        let prior = Self::receipt_by_key_on(&tx, &format!("{}:0", request.idempotency_key.trim()))
            .map_err(AcceptError::Store)?;
        let is_retry = prior.is_some();
        // A server-defaulted observation time must not make a retry look
        // like a different body: reuse the prior receipt's.
        let observed_at = request
            .observed_at
            .clone()
            .or_else(|| prior.as_ref().map(|receipt| receipt.observed_at.clone()))
            .unwrap_or_else(|| now.clone());
        if let Some(expected) = request.expected_revision {
            if !is_retry && expected != track.revision {
                return Err(AcceptError::StaleRevision {
                    expected,
                    current: track.revision,
                });
            }
        }
        // Every declared criterion needs evidence. Criteria are plain strings
        // today; a criterion id is the string itself or `c<n>` (1-based).
        if !track.acceptance_criteria.is_empty() {
            let mut covered = vec![false; track.acceptance_criteria.len()];
            for (index, evidence) in request.evidence.iter().enumerate() {
                let Some(criterion) = evidence.criterion_id.as_deref().map(str::trim) else {
                    return Err(AcceptError::Invalid(format!(
                        "evidence[{index}] needs a criterion_id; track '{}' declares {} criteria",
                        track.track,
                        track.acceptance_criteria.len()
                    )));
                };
                let position = track
                    .acceptance_criteria
                    .iter()
                    .position(|declared| declared.trim() == criterion)
                    .or_else(|| {
                        criterion
                            .strip_prefix('c')
                            .and_then(|n| n.parse::<usize>().ok())
                            .filter(|n| *n >= 1 && *n <= track.acceptance_criteria.len())
                            .map(|n| n - 1)
                    });
                match position {
                    Some(position) => covered[position] = true,
                    None => {
                        return Err(AcceptError::Invalid(format!(
                            "evidence[{index}].criterion_id '{criterion}' matches no declared criterion of '{}'",
                            track.track
                        )))
                    }
                }
            }
            // Partial acceptance is allowed: receipts accumulate per
            // criterion and the track reads satisfied only once every
            // declared criterion has standing evidence (see `derive_claim`).
            let _ = covered;
        }
        let mut receipts = Vec::with_capacity(request.evidence.len());
        let mut replayed = 0usize;
        for (index, evidence) in request.evidence.iter().enumerate() {
            let before: i64 = tx
                .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
                .map_err(|e| AcceptError::Store(e.to_string()))?;
            let receipt = Self::insert_receipt_on(
                &tx,
                &NewReceipt {
                    idempotency_key: format!("{}:{index}", request.idempotency_key.trim()),
                    kind: "accept".into(),
                    project_slug: Some(slug.to_string()),
                    track_id: Some(track.id.clone()),
                    criterion_id: evidence.criterion_id.clone(),
                    subject_type: evidence_subject_type(&evidence.kind).into(),
                    subject_id: evidence.subject_id.trim().to_string(),
                    outcome: "succeeded".into(),
                    actor_type: request.actor_type.clone(),
                    actor_id: request.actor_id.clone(),
                    verifier: evidence.verifier.clone(),
                    supersedes_receipt_id: None,
                    observed_at: observed_at.clone(),
                    payload: serde_json::json!({
                        "evidence_kind": evidence.kind,
                        "track": track.track,
                        "details": evidence.payload,
                    }),
                },
            )
            .map_err(|error| match error {
                ReceiptWriteError::IdempotencyMismatch {
                    idempotency_key,
                    prior_receipt_id,
                } => AcceptError::IdempotencyMismatch {
                    idempotency_key,
                    prior_receipt_id,
                },
                ReceiptWriteError::Store(message) => AcceptError::Store(message),
            })?;
            let after: i64 = tx
                .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
                .map_err(|e| AcceptError::Store(e.to_string()))?;
            if after == before {
                replayed += 1;
            }
            receipts.push(receipt);
        }
        let replayed = replayed == request.evidence.len();
        if !replayed {
            tx.execute(
                "UPDATE project_tracks SET revision = revision + 1, updated_at = ?2 WHERE id = ?1",
                params![track.id, now],
            )
            .map_err(|e| AcceptError::Store(e.to_string()))?;
        }
        let track = Self::track_on(&tx, slug, key)
            .map_err(AcceptError::Store)?
            .ok_or(AcceptError::NotFound)?;
        tx.commit().map_err(|e| AcceptError::Store(e.to_string()))?;
        Ok(AcceptResult {
            track,
            receipts,
            replayed,
        })
    }

    /// Append an `invalidate` receipt over an accept / legacy claim. The
    /// derived state reopens on the next read; nothing is deleted.
    pub fn invalidate_track_evidence(
        &self,
        slug: &str,
        key: &str,
        receipt_id: &str,
        reason: &str,
        actor_type: &str,
        actor_id: &str,
    ) -> Result<Receipt, AcceptError> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock().map_err(AcceptError::Store)?;
        let tx = connection
            .transaction()
            .map_err(|e| AcceptError::Store(e.to_string()))?;
        let Some(track) = Self::track_on(&tx, slug, key).map_err(AcceptError::Store)? else {
            return Err(AcceptError::NotFound);
        };
        let target = tx
            .query_row(
                &format!("{RECEIPT_SELECT} WHERE id = ?1"),
                params![receipt_id],
                receipt_from_row,
            )
            .optional()
            .map_err(|e| AcceptError::Store(e.to_string()))?;
        let Some(target) = target else {
            return Err(AcceptError::Invalid(format!("no receipt '{receipt_id}'")));
        };
        if target.track_id.as_deref() != Some(track.id.as_str())
            || !matches!(target.kind.as_str(), "accept" | "legacy_import")
        {
            return Err(AcceptError::Invalid(format!(
                "receipt '{receipt_id}' is not evidence on track '{}'",
                track.track
            )));
        }
        let receipt = Self::insert_receipt_on(
            &tx,
            &NewReceipt {
                idempotency_key: format!("invalidate:{receipt_id}"),
                kind: "invalidate".into(),
                project_slug: Some(slug.to_string()),
                track_id: Some(track.id.clone()),
                criterion_id: target.criterion_id.clone(),
                subject_type: target.subject_type.clone(),
                subject_id: target.subject_id.clone(),
                outcome: "invalidated".into(),
                actor_type: actor_type.into(),
                actor_id: actor_id.into(),
                verifier: None,
                supersedes_receipt_id: Some(receipt_id.to_string()),
                observed_at: now.clone(),
                payload: serde_json::json!({ "reason": reason, "track": track.track }),
            },
        )
        .map_err(|error| match error {
            ReceiptWriteError::IdempotencyMismatch { .. } => {
                AcceptError::Invalid(format!("receipt '{receipt_id}' is already invalidated"))
            }
            ReceiptWriteError::Store(message) => AcceptError::Store(message),
        })?;
        tx.execute(
            "UPDATE project_tracks SET revision = revision + 1, updated_at = ?2 WHERE id = ?1",
            params![track.id, now],
        )
        .map_err(|e| AcceptError::Store(e.to_string()))?;
        tx.commit().map_err(|e| AcceptError::Store(e.to_string()))?;
        Ok(receipt)
    }

    /// Un-invalidated `accept` receipts with a given subject type, across
    /// every project — what the head-staleness observer walks.
    pub fn active_accept_receipts(&self, subject_type: &str) -> Result<Vec<Receipt>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&format!(
                "{RECEIPT_SELECT} WHERE kind = 'accept' AND subject_type = ?1 \
                   AND NOT EXISTS (SELECT 1 FROM receipts i \
                                   WHERE i.supersedes_receipt_id = receipts.id \
                                     AND i.outcome = 'invalidated') \
                 ORDER BY observed_at"
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![subject_type], receipt_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Track key for a receipt's `track_id`, if it still exists.
    pub fn track_key_for_id(&self, track_id: &str) -> Result<Option<(String, String)>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT slug, track FROM project_tracks WHERE id = ?1",
                params![track_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    // ── Absorption and leases ───────────────────────────────────────────

    /// Resolve a mission's track tag onto the plan, creating an `absorbed`
    /// row when nothing matches. Resolution order: normalized key, alias,
    /// then — only when exactly one track references the mission's PR — that
    /// track (a shared PR is a hint; two tracks on one PR is ambiguous and
    /// falls through to absorption).
    pub fn absorb_track(
        &self,
        slug: &str,
        key: &str,
        title_hint: Option<&str>,
        pr_number: Option<i64>,
    ) -> Result<AbsorbOutcome, String> {
        let normalized = normalize_track_key(key);
        if normalized.is_empty() {
            return Err(format!("track key '{key}' is empty after normalization"));
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        if let Some(id) = Self::resolve_track_id_on(&tx, slug, key)? {
            let canonical: String = tx
                .query_row(
                    "SELECT track FROM project_tracks WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            let matched_by = if canonical == normalized {
                "key"
            } else {
                "alias"
            };
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(AbsorbOutcome {
                key: canonical,
                track_id: id,
                created: false,
                matched_by,
            });
        }
        if let Some(number) = pr_number {
            let mut statement = tx
                .prepare(
                    "SELECT t.id, t.track FROM project_track_refs r \
                     JOIN project_tracks t ON t.id = r.track_id \
                     WHERE t.slug = ?1 AND r.kind = 'pr' AND r.number = ?2 AND t.lifecycle = 'active'",
                )
                .map_err(|e| e.to_string())?;
            let matches: Vec<(String, String)> = statement
                .query_map(params![slug, number], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(statement);
            if matches.len() == 1 {
                let (id, canonical) = matches.into_iter().next().unwrap();
                tx.execute(
                    "INSERT OR IGNORE INTO project_track_aliases \
                       (slug, alias_key, track_id, reason, created_at) VALUES (?1, ?2, ?3, 'pr_match', ?4)",
                    params![slug, key.trim(), id, now],
                )
                .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(AbsorbOutcome {
                    key: canonical,
                    track_id: id,
                    created: false,
                    matched_by: "pr_ref",
                });
            }
        }
        let id = Uuid::new_v4().to_string();
        let title = title_hint
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| humanize_track_key(key));
        tx.execute(
            "INSERT INTO project_tracks \
               (id, slug, track, title, desired_state, lifecycle, origin, explicit_blocker, \
                acceptance_criteria, depends_on, revision, position, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, NULL, 'active', 'absorbed', NULL, '[]', '[]', 0, \
               (SELECT COALESCE(MAX(position), -1) + 1 FROM project_tracks WHERE slug = ?2), ?5, ?5)",
            params![id, slug, normalized, title, now],
        )
        .map_err(|e| e.to_string())?;
        if normalized != key.trim() {
            tx.execute(
                "INSERT OR IGNORE INTO project_track_aliases \
                   (slug, alias_key, track_id, reason, created_at) VALUES (?1, ?2, ?3, 'renamed', ?4)",
                params![slug, key.trim(), id, now],
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(number) = pr_number {
            tx.execute(
                "INSERT OR IGNORE INTO project_track_refs \
                   (track_id, kind, repository, number, url, created_at) VALUES (?1, 'pr', '', ?2, NULL, ?3)",
                params![id, number, now],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(AbsorbOutcome {
            key: normalized,
            track_id: id,
            created: true,
            matched_by: "created",
        })
    }

    /// Take a lease on a track's mutation domain for an attempt. Writers are
    /// exclusive per domain; readers always coexist. Idempotent on
    /// `idempotency_key`. Runs under `BEGIN IMMEDIATE` so two creators cannot
    /// both see "no writer" and both win.
    pub fn acquire_track_lease(&self, request: &LeaseRequest) -> Result<TrackLease, LeaseError> {
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let until = (now + chrono::Duration::seconds(request.ttl_secs.max(60) as i64)).to_rfc3339();
        let connection = self.lock().map_err(LeaseError::Store)?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| LeaseError::Store(e.to_string()))?;
        let result = (|| -> Result<TrackLease, LeaseError> {
            // Idempotent replay only for a *live* lease of the *same* attempt.
            // A released/expired row is history; a live row under the same key
            // held by another attempt means the dispatch key was reused by a
            // retry that created a second mission — exactly the duplicate the
            // lease exists to refuse.
            if let Some(existing) = connection
                .query_row(
                    &format!(
                        "{LEASE_SELECT} WHERE l.idempotency_key = ?1 \
                           AND l.state IN ('reserved','active') AND l.lease_until > ?2"
                    ),
                    params![request.idempotency_key, now_text],
                    lease_from_row,
                )
                .optional()
                .map_err(|e| LeaseError::Store(e.to_string()))?
            {
                if existing.attempt_id == request.attempt_id {
                    return Ok(existing);
                }
                return Err(LeaseError::Owned {
                    holder_attempt_id: existing.attempt_id,
                    lease_until: existing.lease_until,
                    lease_id: existing.id,
                });
            }
            let Some(track_id) =
                Self::resolve_track_id_on(&connection, &request.slug, &request.track)
                    .map_err(LeaseError::Store)?
            else {
                return Err(LeaseError::NotFound);
            };
            if request.mode == "writer" {
                let holder = connection
                    .query_row(
                        &format!(
                            "{LEASE_SELECT} WHERE l.track_id = ?1 AND l.mutation_domain = ?2 \
                               AND l.mode = 'writer' AND l.state IN ('reserved','active') \
                               AND l.lease_until > ?3 AND l.attempt_id != ?4 \
                             ORDER BY l.created_at LIMIT 1"
                        ),
                        params![
                            track_id,
                            request.mutation_domain,
                            now_text,
                            request.attempt_id
                        ],
                        lease_from_row,
                    )
                    .optional()
                    .map_err(|e| LeaseError::Store(e.to_string()))?;
                if let Some(holder) = holder {
                    return Err(LeaseError::Owned {
                        holder_attempt_id: holder.attempt_id,
                        lease_until: holder.lease_until,
                        lease_id: holder.id,
                    });
                }
            }
            let id = Uuid::new_v4().to_string();
            connection
                .execute(
                    "INSERT INTO track_leases \
                       (id, slug, track_id, mutation_domain, attempt_id, mode, state, lease_until, \
                        idempotency_key, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?9)",
                    params![
                        id,
                        request.slug,
                        track_id,
                        request.mutation_domain,
                        request.attempt_id,
                        request.mode,
                        until,
                        request.idempotency_key,
                        now_text
                    ],
                )
                .map_err(|e| LeaseError::Store(e.to_string()))?;
            connection
                .query_row(
                    &format!("{LEASE_SELECT} WHERE l.id = ?1"),
                    params![id],
                    lease_from_row,
                )
                .map_err(|e| LeaseError::Store(e.to_string()))
        })();
        match result {
            Ok(lease) => {
                connection
                    .execute_batch("COMMIT")
                    .map_err(|e| LeaseError::Store(e.to_string()))?;
                Ok(lease)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Release every live lease held by an attempt (mission). Returns how
    /// many were released.
    pub fn release_leases_for_attempt(&self, attempt_id: &str) -> Result<usize, String> {
        let connection = self.lock()?;
        connection
            .execute(
                "UPDATE track_leases SET state = 'released', updated_at = ?2 \
                 WHERE attempt_id = ?1 AND state IN ('reserved','active')",
                params![attempt_id, Utc::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())
    }

    /// Release the live leases of an attempt on tracks other than `keep_key`
    /// (used when a mission moves to another track).
    pub fn release_leases_except(
        &self,
        attempt_id: &str,
        slug: &str,
        keep_key: &str,
    ) -> Result<usize, String> {
        let connection = self.lock()?;
        let keep_id = Self::resolve_track_id_on(&connection, slug, keep_key)?;
        connection
            .execute(
                "UPDATE track_leases SET state = 'released', updated_at = ?2 \
                 WHERE attempt_id = ?1 AND state IN ('reserved','active') \
                   AND (?3 IS NULL OR track_id != ?3)",
                params![attempt_id, Utc::now().to_rfc3339(), keep_id],
            )
            .map_err(|e| e.to_string())
    }

    pub fn renew_lease(&self, lease_id: &str, ttl_secs: u64) -> Result<bool, String> {
        let now = Utc::now();
        let until = (now + chrono::Duration::seconds(ttl_secs.max(60) as i64)).to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "UPDATE track_leases SET lease_until = ?2, updated_at = ?3 \
                 WHERE id = ?1 AND state IN ('reserved','active')",
                params![lease_id, until, now.to_rfc3339()],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    pub fn expire_lease(&self, lease_id: &str) -> Result<bool, String> {
        let connection = self.lock()?;
        connection
            .execute(
                "UPDATE track_leases SET state = 'expired', updated_at = ?2 \
                 WHERE id = ?1 AND state IN ('reserved','active')",
                params![lease_id, Utc::now().to_rfc3339()],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    /// Live (reserved/active, unexpired) leases; all projects when `slug` is None.
    pub fn live_leases(&self, slug: Option<&str>) -> Result<Vec<TrackLease>, String> {
        let connection = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let mut statement = connection
            .prepare(&format!(
                "{LEASE_SELECT} WHERE l.state IN ('reserved','active') AND l.lease_until > ?1 \
                   AND (?2 IS NULL OR l.slug = ?2) ORDER BY l.created_at"
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![now, slug], lease_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Leases still marked live whose `lease_until` has passed.
    pub fn overdue_leases(&self) -> Result<Vec<TrackLease>, String> {
        let connection = self.lock()?;
        let now = Utc::now().to_rfc3339();
        let mut statement = connection
            .prepare(&format!(
                "{LEASE_SELECT} WHERE l.state IN ('reserved','active') AND l.lease_until <= ?1"
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![now], lease_from_row)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    // ── Evidence-driven compatibility surface (2026-09-01 API) ───────────

    /// True when every current criterion has standing accepted evidence.
    pub fn track_contract_satisfied(&self, slug: &str, track: &str) -> Result<bool, String> {
        Ok(self
            .track(slug, track)?
            .is_some_and(|row| row.claim.as_deref() == Some("accept")))
    }

    /// Accept evidence for one criterion (the per-criterion shape the
    /// `accept_project_track_evidence` tool speaks). Delegates to
    /// [`Self::accept_track_evidence`]; the track reads satisfied once every
    /// criterion is covered.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_track_criterion_evidence(
        &self,
        slug: &str,
        track: &str,
        criterion: Option<&str>,
        verifier_class: &str,
        evidence_ref: &str,
        artifact_version: &str,
        observed_at: Option<&str>,
        accepted_by: &str,
    ) -> Result<AcceptResult, AcceptError> {
        let verifier_class = verifier_class.trim();
        if !TRACK_VERIFIER_CLASSES.contains(&verifier_class) {
            return Err(AcceptError::Invalid(format!(
                "invalid verifier_class '{verifier_class}'; expected {}",
                TRACK_VERIFIER_CLASSES.join(", ")
            )));
        }
        let evidence_ref = evidence_ref.trim();
        let artifact_version = artifact_version.trim();
        if evidence_ref.is_empty() || artifact_version.is_empty() {
            return Err(AcceptError::Invalid(
                "evidence_ref and artifact_version are required".into(),
            ));
        }
        let Some(current) = self.track(slug, track).map_err(AcceptError::Store)? else {
            return Err(AcceptError::NotFound);
        };
        let criterion_id = match (
            criterion
                .map(str::trim)
                .filter(|v| !v.is_empty() && *v != "__track__"),
            current.acceptance_criteria.len(),
        ) {
            (Some(value), _) => Some(value.to_string()),
            (None, 0) => None,
            (None, 1) => Some(current.acceptance_criteria[0].clone()),
            (None, _) => {
                return Err(AcceptError::Invalid(
                    "criterion is required when a track has multiple acceptance criteria".into(),
                ))
            }
        };
        if let Some(governed) = current
            .governed_artifact_version
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            if governed != artifact_version {
                return Err(AcceptError::Invalid(format!(
                    "evidence artifact '{artifact_version}' does not match governed artifact '{governed}'"
                )));
            }
        }
        let kind = match verifier_class {
            "review" => "review",
            "command" => "command",
            "external_state" => "external_state",
            "manual" => "manual",
            _ => "operator",
        };
        let idempotency_key = {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(
                format!(
                    "{slug}\0{}\0{}\0{evidence_ref}\0{artifact_version}",
                    current.track,
                    criterion_id.as_deref().unwrap_or("")
                )
                .as_bytes(),
            );
            format!("evidence:{}", hex::encode(hasher.finalize()))
        };
        self.accept_track_evidence(
            slug,
            track,
            &AcceptRequest {
                idempotency_key,
                expected_revision: None,
                evidence: vec![EvidenceInput {
                    criterion_id,
                    kind: kind.to_string(),
                    subject_id: format!("{evidence_ref}@{artifact_version}"),
                    verifier: Some(verifier_class.to_string()),
                    payload: serde_json::json!({
                        "evidence_ref": evidence_ref,
                        "artifact_version": artifact_version,
                    }),
                }],
                actor_type: "operator".into(),
                actor_id: accepted_by.trim().to_string(),
                observed_at: observed_at.map(str::to_string),
            },
        )
    }

    /// Reopen a satisfied or cancelled track: every standing claim is
    /// invalidated with the reason, a cancelled lifecycle returns to active,
    /// the revision moves. Nothing is deleted.
    pub fn reopen_track(
        &self,
        slug: &str,
        track: &str,
        reason: &str,
        governed_artifact_version: Option<&str>,
        reopened_by: &str,
    ) -> Result<ProjectTrack, AcceptError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AcceptError::Invalid("reopen reason is required".into()));
        }
        let Some(current) = self.track(slug, track).map_err(AcceptError::Store)? else {
            return Err(AcceptError::NotFound);
        };
        if current.lifecycle != "cancelled" && current.claim.is_none() {
            return Err(AcceptError::Invalid(format!(
                "track '{slug}/{track}' is unknown or not terminal"
            )));
        }
        let all = self
            .receipts_for_track(slug, track)
            .map_err(AcceptError::Store)?;
        let invalidated: std::collections::HashSet<String> = all
            .iter()
            .filter(|r| r.outcome == "invalidated")
            .filter_map(|r| r.supersedes_receipt_id.clone())
            .collect();
        for receipt in all
            .iter()
            .filter(|r| matches!(r.kind.as_str(), "accept" | "legacy_import"))
            .filter(|r| !invalidated.contains(&r.id))
        {
            self.invalidate_track_evidence(
                slug,
                track,
                &receipt.id,
                &format!("reopened: {reason}"),
                "operator",
                reopened_by,
            )?;
        }
        let now = Utc::now().to_rfc3339();
        {
            let connection = self.lock().map_err(AcceptError::Store)?;
            connection
                .execute(
                    "UPDATE project_tracks SET lifecycle = 'active', explicit_blocker = NULL, \
                       governed_artifact_version = ?3, revision = revision + 1, updated_at = ?2 \
                     WHERE id = ?1",
                    params![current.id, now, governed_artifact_version],
                )
                .map_err(|e| AcceptError::Store(e.to_string()))?;
        }
        self.insert_receipt(&NewReceipt {
            idempotency_key: format!("reopen:{}:{}", current.id, now),
            kind: "reconcile".into(),
            project_slug: Some(slug.to_string()),
            track_id: Some(current.id.clone()),
            criterion_id: None,
            subject_type: "operator".into(),
            subject_id: format!("reopen:{}", current.track),
            outcome: "observed".into(),
            actor_type: "operator".into(),
            actor_id: reopened_by.into(),
            verifier: None,
            supersedes_receipt_id: None,
            observed_at: now,
            payload: serde_json::json!({
                "corrections": [{ "op": "reopened", "track": current.track, "reason": reason }],
            }),
        })
        .map_err(|e| AcceptError::Store(e.to_string()))?;
        self.track(slug, track)
            .map_err(AcceptError::Store)?
            .ok_or(AcceptError::NotFound)
    }

    /// The live lease taken under a dispatch key, if any (create_mission
    /// coalesces a retried dispatch onto the mission that holds it).
    pub fn lease_by_key(&self, idempotency_key: &str) -> Result<Option<TrackLease>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                &format!(
                    "{LEASE_SELECT} WHERE l.idempotency_key = ?1 \
                       AND l.state IN ('reserved','active') AND l.lease_until > ?2"
                ),
                params![idempotency_key, Utc::now().to_rfc3339()],
                lease_from_row,
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    // ── Aliases, refs, imports ──────────────────────────────────────────

    pub fn add_track_alias(
        &self,
        slug: &str,
        key: &str,
        alias: &str,
        reason: &str,
    ) -> Result<bool, String> {
        let connection = self.lock()?;
        let Some(id) = Self::resolve_track_id_on(&connection, slug, key)? else {
            return Err(format!("no track '{key}' in '{slug}'"));
        };
        let alias = alias.trim();
        if alias.is_empty() {
            return Ok(false);
        }
        connection
            .execute(
                "INSERT OR IGNORE INTO project_track_aliases \
                   (slug, alias_key, track_id, reason, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![slug, alias, id, reason, Utc::now().to_rfc3339()],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    pub fn add_track_ref(
        &self,
        slug: &str,
        key: &str,
        kind: &str,
        repository: Option<&str>,
        number: i64,
        url: Option<&str>,
    ) -> Result<bool, String> {
        let connection = self.lock()?;
        let Some(id) = Self::resolve_track_id_on(&connection, slug, key)? else {
            return Err(format!("no track '{key}' in '{slug}'"));
        };
        connection
            .execute(
                "INSERT OR IGNORE INTO project_track_refs \
                   (track_id, kind, repository, number, url, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    kind,
                    repository.unwrap_or(""),
                    number,
                    url,
                    Utc::now().to_rfc3339()
                ],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    /// Tracks in `slug` that reference PR `number` (any repository when
    /// `repository` is None). A matching hint for absorption, not a merge.
    pub fn tracks_for_pr(
        &self,
        slug: &str,
        repository: Option<&str>,
        number: i64,
    ) -> Result<Vec<String>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT t.track FROM project_track_refs r \
                 JOIN project_tracks t ON t.id = r.track_id \
                 WHERE t.slug = ?1 AND r.kind = 'pr' AND r.number = ?2 \
                   AND (?3 IS NULL OR r.repository = ?3 OR r.repository = '') \
                 ORDER BY t.track",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug, number, repository], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// True when this exact (source, hash, parser) was already imported.
    pub fn import_exists(
        &self,
        slug: &str,
        source_path: &str,
        source_hash: &str,
        parser_version: i64,
    ) -> Result<bool, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT 1 FROM project_imports \
                 WHERE slug = ?1 AND source_path = ?2 AND source_hash = ?3 AND parser_version = ?4",
                params![slug, source_path, source_hash, parser_version],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
            .map_err(|e| e.to_string())
    }

    pub fn record_import(
        &self,
        slug: &str,
        source_path: &str,
        source_hash: &str,
        parser_version: i64,
        items: usize,
        receipt_id: Option<&str>,
    ) -> Result<bool, String> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT OR IGNORE INTO project_imports \
                   (slug, source_path, source_hash, parser_version, imported_at, items, receipt_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    slug,
                    source_path,
                    source_hash,
                    parser_version,
                    Utc::now().to_rfc3339(),
                    items as i64,
                    receipt_id
                ],
            )
            .map(|changed| changed > 0)
            .map_err(|e| e.to_string())
    }

    /// Insert an imported track (no-op when the key or an alias already
    /// exists; returns whether a row was created).
    pub fn import_track(
        &self,
        slug: &str,
        key: &str,
        title: &str,
        aliases: &[String],
    ) -> Result<bool, String> {
        let normalized = normalize_track_key(key);
        if normalized.is_empty() {
            return Err(format!("track key '{key}' is empty after normalization"));
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        let existing = Self::resolve_track_id_on(&tx, slug, key)?;
        let created = existing.is_none();
        let id = match existing {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4().to_string();
                let title = title.trim();
                let title = if title.is_empty() {
                    humanize_track_key(key)
                } else {
                    title.to_string()
                };
                tx.execute(
                    "INSERT INTO project_tracks \
                       (id, slug, track, title, desired_state, lifecycle, origin, explicit_blocker, \
                        acceptance_criteria, depends_on, revision, position, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, NULL, 'active', 'imported', NULL, '[]', '[]', 0, \
                       (SELECT COALESCE(MAX(position), -1) + 1 FROM project_tracks WHERE slug = ?2), ?5, ?5)",
                    params![id, slug, normalized, title, now],
                )
                .map_err(|e| e.to_string())?;
                id
            }
        };
        for alias in aliases.iter().chain(std::iter::once(&key.to_string())) {
            let alias = alias.trim();
            if alias.is_empty() || alias == normalized {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO project_track_aliases \
                   (slug, alias_key, track_id, reason, created_at) VALUES (?1, ?2, ?3, 'imported_code', ?4)",
                params![slug, alias, id, now],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(created)
    }

    /// Leftover proposal rows become imported tracks and are retired.
    pub fn import_open_proposals(&self, slug: &str) -> Result<usize, String> {
        let proposals = self.list_open_proposals(slug)?;
        let mut created = 0usize;
        for proposal in &proposals {
            if self.import_track(slug, &proposal.task_key, &proposal.title, &[])? {
                created += 1;
            }
            if !proposal.acceptance_criteria.is_empty() || !proposal.depends_on.is_empty() {
                self.patch_track(
                    slug,
                    &proposal.task_key,
                    proposal.prompt.as_deref(),
                    None,
                    None,
                    Some(&proposal.acceptance_criteria),
                    Some(&proposal.depends_on),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        if !proposals.is_empty() {
            let connection = self.lock()?;
            connection
                .execute(
                    "UPDATE project_roadmap_proposals SET status = 'imported', updated_at = ?2 \
                     WHERE slug = ?1 AND status = 'proposed'",
                    params![slug, Utc::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(created)
    }

    #[allow(clippy::too_many_arguments)]

    pub fn get_grant(&self, slug: &str) -> Result<Option<ProjectGrant>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT merge_authority, budget_per_tick, parallel_missions, \
                 pause_reason, resume_condition, material_bar, answered_at, autonomy_level \
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
                        autonomy_level: row.get(7)?,
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
        autonomy_level: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO project_grant \
                   (slug, merge_authority, budget_per_tick, parallel_missions, \
                    pause_reason, resume_condition, material_bar, answered_at, autonomy_level) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(slug) DO UPDATE SET \
                   merge_authority = COALESCE(excluded.merge_authority, project_grant.merge_authority), \
                   budget_per_tick = COALESCE(excluded.budget_per_tick, project_grant.budget_per_tick), \
                   parallel_missions = COALESCE(excluded.parallel_missions, project_grant.parallel_missions), \
                   pause_reason = COALESCE(excluded.pause_reason, project_grant.pause_reason), \
                   resume_condition = COALESCE(excluded.resume_condition, project_grant.resume_condition), \
                   material_bar = COALESCE(excluded.material_bar, project_grant.material_bar), \
                   answered_at = excluded.answered_at, \
                   autonomy_level = COALESCE(excluded.autonomy_level, project_grant.autonomy_level)",
                params![
                    slug,
                    merge_authority,
                    budget_per_tick,
                    parallel_missions,
                    pause_reason,
                    resume_condition,
                    material_bar,
                    now,
                    autonomy_level
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Create-or-refresh chat-planned roadmap items. Re-proposing an existing
    /// key updates it in place and revives a cancelled one — the caller's
    /// latest intent wins; there is nothing destructive to protect here.
    pub fn upsert_proposals(&self, slug: &str, proposals: &[NewProposal]) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(|e| e.to_string())?;
        for proposal in proposals {
            let criteria =
                serde_json::to_string(&proposal.acceptance_criteria).map_err(|e| e.to_string())?;
            let depends = serde_json::to_string(&proposal.depends_on).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO project_roadmap_proposals \
                   (slug, task_key, title, prompt, acceptance_criteria, depends_on, status, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', ?7, ?7) \
                 ON CONFLICT(slug, task_key) DO UPDATE SET \
                   title = excluded.title, \
                   prompt = COALESCE(excluded.prompt, project_roadmap_proposals.prompt), \
                   acceptance_criteria = excluded.acceptance_criteria, \
                   depends_on = excluded.depends_on, \
                   status = 'proposed', \
                   updated_at = excluded.updated_at",
                params![
                    slug,
                    proposal.task_key,
                    proposal.title,
                    proposal.prompt,
                    criteria,
                    depends,
                    now
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Patch an open proposal. Returns false when the key doesn't exist or is
    /// cancelled — an adopted (board-shadowed) key is the mission's to edit,
    /// but the proposal row itself stays patchable until cancelled.
    pub fn update_proposal(
        &self,
        slug: &str,
        task_key: &str,
        title: Option<&str>,
        prompt: Option<&str>,
        acceptance_criteria: Option<&[String]>,
        depends_on: Option<&[String]>,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let criteria = acceptance_criteria
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let depends = depends_on
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE project_roadmap_proposals SET \
                   title = COALESCE(?3, title), \
                   prompt = COALESCE(?4, prompt), \
                   acceptance_criteria = COALESCE(?5, acceptance_criteria), \
                   depends_on = COALESCE(?6, depends_on), \
                   updated_at = ?7 \
                 WHERE slug = ?1 AND task_key = ?2 AND status = 'proposed'",
                params![slug, task_key, title, prompt, criteria, depends, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(changed > 0)
    }

    /// Cancel an open proposal. Returns false when there was nothing open.
    pub fn cancel_proposal(&self, slug: &str, task_key: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE project_roadmap_proposals SET status = 'cancelled', updated_at = ?3 \
                 WHERE slug = ?1 AND task_key = ?2 AND status = 'proposed'",
                params![slug, task_key, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(changed > 0)
    }

    /// Open (non-cancelled) proposals for a project, oldest first — the order
    /// they were planned in is the order the roadmap shows them.
    pub fn list_open_proposals(&self, slug: &str) -> Result<Vec<RoadmapProposal>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT task_key, title, prompt, acceptance_criteria, depends_on, created_at, updated_at \
                 FROM project_roadmap_proposals \
                 WHERE slug = ?1 AND status = 'proposed' ORDER BY created_at, task_key",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug], |row| {
                let criteria: Option<String> = row.get(3)?;
                let depends: Option<String> = row.get(4)?;
                Ok(RoadmapProposal {
                    task_key: row.get(0)?,
                    title: row.get(1)?,
                    prompt: row.get(2)?,
                    acceptance_criteria: parse_string_list(criteria),
                    depends_on: parse_string_list(depends),
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Slug that actually owns this leftover proposal among `slugs`.
    pub fn find_open_proposal_slug(
        &self,
        slugs: &[String],
        task_key: &str,
    ) -> Result<Option<String>, String> {
        let connection = self.lock()?;
        for slug in slugs {
            let found: Option<String> = connection
                .query_row(
                    "SELECT slug FROM project_roadmap_proposals \
                     WHERE slug = ?1 AND task_key = ?2 AND status = 'proposed'",
                    params![slug, task_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Record a decision and return its `at` key. The legacy `answered` flag is
    /// dual-written: only `pending_user` rows count as open, so an older binary
    /// reading `answered = 0` never surfaces autonomous acts.
    pub fn record_decision(&self, slug: &str, decision: &NewDecision) -> Result<String, String> {
        if decision.status == "pending_user" {
            if let Some(existing) = self.pending_question_at(slug, &decision.question)? {
                return Ok(existing);
            }
        }
        let now = Utc::now().to_rfc3339();
        let answered = i64::from(decision.status != "pending_user");
        let evidence = decision.evidence.as_ref().map(|value| value.to_string());
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO project_decisions \
                   (slug, at, question, rationale, answered, kind, authority, status, evidence) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    slug,
                    now,
                    decision.question,
                    decision.rationale,
                    answered,
                    decision.kind,
                    decision.authority,
                    decision.status,
                    evidence
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(now)
    }

    /// Ingest a decision carried by a delivery trailer. Keyed by the
    /// delivery's own timestamp and INSERT OR IGNORE, so the overlapping
    /// ingest windows replaying the same delivery record it exactly once —
    /// and a later answer is never clobbered back to pending.
    pub fn record_decision_from_delivery(
        &self,
        slug: &str,
        at: &str,
        decision: &NewDecision,
    ) -> Result<bool, String> {
        if decision.status == "pending_user"
            && self
                .pending_question_at(slug, &decision.question)?
                .is_some()
        {
            return Ok(false);
        }
        let answered = i64::from(decision.status != "pending_user");
        let evidence = decision.evidence.as_ref().map(|value| value.to_string());
        let connection = self.lock()?;
        let inserted = connection
            .execute(
                "INSERT OR IGNORE INTO project_decisions \
                   (slug, at, question, rationale, answered, kind, authority, status, evidence) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    slug,
                    at,
                    decision.question,
                    decision.rationale,
                    answered,
                    decision.kind,
                    decision.authority,
                    decision.status,
                    evidence
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(inserted > 0)
    }

    /// Answer a pending escalation. Returns false when no pending decision
    /// exists at that key (already answered, expired, or never recorded).
    pub fn answer_decision(&self, slug: &str, at: &str, answer: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE project_decisions \
                 SET status = 'answered', answer = ?3, answered_at = ?4, answered = 1 \
                 WHERE slug = ?1 AND at = ?2 AND status = 'pending_user'",
                params![slug, at, answer, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(changed > 0)
    }

    /// Close pending escalations that mention any of `needles` (a `#N` hash
    /// or a full GitHub PR URL). Used when a later delivery reports that PR
    /// as merged, so the board stops asking about a decision that is already
    /// done. `except_at` skips the delivery that just recorded itself — a
    /// coerced "Merged #N" trailer must stay pending, not close itself.
    pub fn close_pending_decisions_referencing(
        &self,
        slug: &str,
        needles: &[String],
        answer: &str,
        except_at: Option<&str>,
    ) -> Result<u32, String> {
        if needles.is_empty() {
            return Ok(0);
        }
        let open = self.open_decisions(slug)?;
        let mut closed = 0u32;
        for decision in open {
            if except_at.is_some_and(|at| at == decision.at) {
                continue;
            }
            if !decision_mentions_any(&decision, needles) {
                continue;
            }
            if self.answer_decision(slug, &decision.at, answer)? {
                closed += 1;
            }
        }
        Ok(closed)
    }

    pub fn open_decisions(&self, slug: &str) -> Result<Vec<ProjectDecision>, String> {
        self.decisions_where(slug, "status = 'pending_user'", "ORDER BY at", None)
    }

    /// Existing open row for this question, if any. Used so Coldcard cannot
    /// record the same checkpoint prompt twice two seconds apart.
    fn pending_question_at(&self, slug: &str, question: &str) -> Result<Option<String>, String> {
        let needle = super::controller_honesty::decision_identity(question);
        if needle.is_empty() {
            return Ok(None);
        }
        Ok(self.open_decisions(slug)?.into_iter().find_map(|decision| {
            (super::controller_honesty::decision_identity(&decision.question) == needle)
                .then_some(decision.at)
        }))
    }

    /// Expire unanswered owner questions older than `max_age`. Returns how
    /// many rows flipped. The card then stops showing them; the controller
    /// must act on the conservative default instead of re-asking.
    pub fn expire_pending_decisions(&self, max_age: chrono::Duration) -> Result<u32, String> {
        let cutoff = (Utc::now() - max_age).to_rfc3339();
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE project_decisions \
                 SET status = 'expired', answered = 1 \
                 WHERE status = 'pending_user' AND at < ?1",
                params![cutoff],
            )
            .map_err(|e| e.to_string())?;
        Ok(changed as u32)
    }

    /// Newest-first autonomous acts (`status = 'decided'`) plus recently
    /// answered escalations, for the "recent activity" panel.
    pub fn recent_decisions(&self, slug: &str, limit: u32) -> Result<Vec<ProjectDecision>, String> {
        self.decisions_where(
            slug,
            "status IN ('decided', 'answered')",
            "ORDER BY at DESC",
            Some(limit),
        )
    }

    /// What the Hermes project rail shows as Recent activity: the ledger
    /// (`decided` / `answered`) plus material controller state headlines that
    /// never carried a `[DECISION:]` trailer. Without the union the panel
    /// freezes on the last owner question even while the controller is
    /// merging and ticking (Lido SRv3, 2026-08-16).
    pub fn recent_activity(&self, slug: &str, limit: u32) -> Result<Vec<ProjectDecision>, String> {
        let mut items = self.recent_decisions(slug, limit)?;
        let seen_at: HashSet<String> = items.iter().map(|item| item.at.clone()).collect();
        let seen_question: HashSet<String> = items
            .iter()
            .map(|item| super::controller_honesty::normalize_decision_question(&item.question))
            .collect();
        for state in self.state_timeline(slug, limit as usize)? {
            let Some(headline) = state.headline.as_deref() else {
                continue;
            };
            if !super::controller_honesty::is_material_activity_headline(headline) {
                continue;
            }
            if seen_at.contains(&state.last_seen_at) {
                continue;
            }
            let normalized = super::controller_honesty::normalize_decision_question(headline);
            if seen_question.contains(&normalized) {
                continue;
            }
            items.push(ProjectDecision {
                at: state.last_seen_at,
                question: headline.to_string(),
                rationale: None,
                kind: Some("report".to_string()),
                authority: "granted".to_string(),
                status: Some("decided".to_string()),
                answer: None,
                answered_at: None,
                evidence: None,
            });
        }
        items.sort_by(|a, b| b.at.cmp(&a.at));
        items.truncate(limit as usize);
        Ok(items)
    }

    fn decisions_where(
        &self,
        slug: &str,
        predicate: &str,
        order: &str,
        limit: Option<u32>,
    ) -> Result<Vec<ProjectDecision>, String> {
        let limit_clause = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT at, question, rationale, kind, authority, status, answer, answered_at, evidence \
                 FROM project_decisions WHERE slug = ?1 AND {predicate} {order}{limit_clause}",
            ))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![slug], |row| {
                let evidence: Option<String> = row.get(8)?;
                Ok(ProjectDecision {
                    at: row.get(0)?,
                    question: row.get(1)?,
                    rationale: row.get(2)?,
                    kind: row.get(3)?,
                    authority: row.get(4)?,
                    status: row.get(5)?,
                    answer: row.get(6)?,
                    answered_at: row.get(7)?,
                    evidence: evidence.and_then(|raw| serde_json::from_str(&raw).ok()),
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Bulk autonomy levels, one board render = one query.
    pub fn autonomy_levels(&self) -> Result<HashMap<String, String>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, autonomy_level FROM project_grant WHERE autonomy_level IS NOT NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Count of decisions waiting on the owner, for the board row badge.
    pub fn pending_decision_counts(&self) -> Result<HashMap<String, u32>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT slug, COUNT(*) FROM project_decisions \
                 WHERE status = 'pending_user' GROUP BY slug",
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

/// Input for [`ProjectsStore::record_decision`], already validated/coerced by
/// the caller (see `resolve_decision_disposition` in `projects_overview`).
#[derive(Debug, Clone)]
pub struct NewDecision {
    pub question: String,
    pub rationale: Option<String>,
    pub kind: Option<String>,
    /// granted | escalation
    pub authority: String,
    /// decided | pending_user | answered | expired
    pub status: String,
    pub evidence: Option<serde_json::Value>,
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
    /// Session of the newest delivery folded into this row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// A delivery the ingestor refused to route: no key, or a key aliased onto an
/// archived project. Surfaces on the board so it can be triaged instead of
/// vanishing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UnroutedDelivery {
    pub session_id: String,
    pub at: String,
    pub headline: String,
    pub signature: Option<String>,
    pub mode: Option<String>,
    pub blocker: Option<String>,
}

/// Parsed RFC3339 compare so `Z` and `+00:00` (and other offsets) order by
/// instant, not by the serialized suffix.
pub(crate) fn rfc3339_at_least(candidate: &str, watermark: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(candidate),
        chrono::DateTime::parse_from_rfc3339(watermark),
    ) {
        (Ok(candidate), Ok(watermark)) => candidate >= watermark,
        // Unparseable candidate must not clobber a known watermark.
        (Err(_), Ok(_)) => false,
        _ => true,
    }
}

/// Strict instant compare for newest-in-batch selection. Unparseable
/// values fall back to string order.
pub(crate) fn rfc3339_after(candidate: &str, existing: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(candidate),
        chrono::DateTime::parse_from_rfc3339(existing),
    ) {
        (Ok(candidate), Ok(existing)) => candidate > existing,
        _ => candidate > existing,
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_signal_at: Option<String>,
}

/// A chat-planned roadmap item: the project-scoped precursor of a board task.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RoadmapProposal {
    pub task_key: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub depends_on: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

const TRACK_SELECT: &str =
    "SELECT t.id, t.track, t.desired_state, t.title, t.acceptance_criteria, t.depends_on, \
     t.updated_at, t.lifecycle, t.origin, t.explicit_blocker, t.revision, t.position, \
     t.governed_artifact_version \
     FROM project_tracks t WHERE t.slug = ?1 ORDER BY t.position, t.track";

const TRACK_SELECT_BY_ID: &str =
    "SELECT t.id, t.track, t.desired_state, t.title, t.acceptance_criteria, t.depends_on, \
     t.updated_at, t.lifecycle, t.origin, t.explicit_blocker, t.revision, t.position, \
     t.governed_artifact_version \
     FROM project_tracks t WHERE t.id = ?1";

/// Standing (un-invalidated) claim receipts, keyed by track id.
const STANDING_CLAIMS_FOR_SLUG: &str =
    "SELECT r.track_id, r.kind, r.criterion_id, r.observed_at, r.payload FROM receipts r \
     JOIN project_tracks t ON t.id = r.track_id \
     WHERE t.slug = ?1 AND r.kind IN ('legacy_import', 'accept') \
       AND r.outcome IN ('observed', 'succeeded') \
       AND NOT EXISTS (SELECT 1 FROM receipts i \
                       WHERE i.supersedes_receipt_id = r.id AND i.outcome = 'invalidated') \
     ORDER BY r.observed_at, r.created_at";

const STANDING_CLAIMS_FOR_TRACK: &str =
    "SELECT r.track_id, r.kind, r.criterion_id, r.observed_at, r.payload FROM receipts r \
     WHERE r.track_id = ?1 AND r.kind IN ('legacy_import', 'accept') \
       AND r.outcome IN ('observed', 'succeeded') \
       AND NOT EXISTS (SELECT 1 FROM receipts i \
                       WHERE i.supersedes_receipt_id = r.id AND i.outcome = 'invalidated') \
     ORDER BY r.observed_at, r.created_at";

#[derive(Debug, Clone)]
struct StandingClaim {
    kind: String,
    criterion_id: Option<String>,
    observed_at: String,
    artifact_version: Option<String>,
}

fn standing_claim_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, StandingClaim)> {
    let payload: String = row.get(4)?;
    let artifact_version = serde_json::from_str::<serde_json::Value>(&payload)
        .ok()
        .and_then(|value| {
            value
                .get("artifact_version")
                .or_else(|| value.get("details").and_then(|d| d.get("artifact_version")))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    Ok((
        row.get(0)?,
        StandingClaim {
            kind: row.get(1)?,
            criterion_id: row.get(2)?,
            observed_at: row.get(3)?,
            artifact_version,
        },
    ))
}

/// Resolve a criterion reference (`c<n>` or the criterion text) to its index.
fn criterion_index(criteria: &[String], reference: &str) -> Option<usize> {
    let reference = reference.trim();
    criteria
        .iter()
        .position(|declared| declared.trim() == reference)
        .or_else(|| {
            reference
                .strip_prefix('c')
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n >= 1 && *n <= criteria.len())
                .map(|n| n - 1)
        })
}

/// Derive the standing claim of a track from its un-invalidated receipts:
/// `accept` when every declared criterion is covered (or none is declared
/// and at least one accept stands), else `legacy_import` when a pre-receipt
/// claim stands, else none. Returns (claim, accepted_at, artifact_version).
fn derive_claim(
    criteria: &[String],
    claims: &[StandingClaim],
) -> (Option<String>, Option<String>, Option<String>) {
    let accepts: Vec<&StandingClaim> = claims.iter().filter(|c| c.kind == "accept").collect();
    let covered = if criteria.is_empty() {
        !accepts.is_empty()
    } else {
        let mut seen = vec![false; criteria.len()];
        for accept in &accepts {
            match accept.criterion_id.as_deref() {
                Some(reference) => {
                    if let Some(index) = criterion_index(criteria, reference) {
                        seen[index] = true;
                    }
                }
                // An accept without a criterion on a track that declares
                // some (interim `__track__` evidence) covers the contract as
                // it stood when it was accepted.
                None => seen.iter_mut().for_each(|s| *s = true),
            }
        }
        seen.iter().all(|s| *s)
    };
    if covered {
        let newest = accepts.iter().max_by_key(|c| c.observed_at.clone());
        return (
            Some("accept".into()),
            newest.map(|c| c.observed_at.clone()),
            newest.and_then(|c| c.artifact_version.clone()),
        );
    }
    if let Some(legacy) = claims
        .iter()
        .filter(|c| c.kind == "legacy_import")
        .max_by_key(|c| c.observed_at.clone())
    {
        return (
            Some("legacy_import".into()),
            Some(legacy.observed_at.clone()),
            None,
        );
    }
    (None, None, None)
}

struct TrackRow {
    id: String,
    track: String,
    desired_state: Option<String>,
    title: Option<String>,
    acceptance_criteria: Vec<String>,
    depends_on: Vec<String>,
    updated_at: String,
    lifecycle: String,
    origin: String,
    explicit_blocker: Option<String>,
    revision: u64,
    position: u64,
    governed_artifact_version: Option<String>,
}

fn track_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackRow> {
    let criteria: Option<String> = row.get(4)?;
    let depends: Option<String> = row.get(5)?;
    Ok(TrackRow {
        id: row.get(0)?,
        track: row.get(1)?,
        desired_state: row.get(2)?,
        title: row.get(3)?,
        acceptance_criteria: parse_string_list(criteria),
        depends_on: parse_string_list(depends),
        updated_at: row.get(6)?,
        lifecycle: row.get(7)?,
        origin: row.get(8)?,
        explicit_blocker: row.get(9)?,
        revision: row.get::<_, i64>(10)?.max(0) as u64,
        position: row.get::<_, i64>(11)?.max(0) as u64,
        governed_artifact_version: row.get(12)?,
    })
}

fn finish_track(row: TrackRow, claims: &[StandingClaim]) -> ProjectTrack {
    let (claim, accepted_at, artifact) = derive_claim(&row.acceptance_criteria, claims);
    let status = derived_track_status(
        &row.lifecycle,
        row.explicit_blocker.as_deref(),
        claim.as_deref(),
    );
    ProjectTrack {
        id: row.id,
        track: row.track,
        desired_state: row.desired_state,
        status: Some(status.to_string()),
        title: row.title,
        acceptance_criteria: row.acceptance_criteria,
        depends_on: row.depends_on,
        updated_at: row.updated_at,
        lifecycle: row.lifecycle,
        origin: row.origin,
        explicit_blocker: row.explicit_blocker,
        revision: row.revision,
        position: row.position,
        governed_artifact_version: artifact.or(row.governed_artifact_version),
        accepted_at,
        claim,
    }
}

/// The legacy status vocabulary, derived from stored facts. Readers that
/// predate `lifecycle` (mission horizon, honest next-action) keep working;
/// nothing writes this back.
pub fn derived_track_status(
    lifecycle: &str,
    blocker: Option<&str>,
    claim: Option<&str>,
) -> &'static str {
    if lifecycle == "cancelled" {
        return "cancelled";
    }
    match claim {
        Some("accept") => "satisfied",
        Some(_) => "done",
        None if blocker.is_some() => "blocked",
        None => "open",
    }
}

/// Track keys are lowercase `[a-z0-9-]` with collapsed dashes. `UX1`,
/// `ux1`, `UX_1` and ` ux-1 ` are the same key. Normalization is a spelling
/// rule, not a proof of semantic identity: `ux1` and `ux1-pr229-cert` stay
/// distinct.
pub fn normalize_track_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true;
    for ch in raw.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_alphanumeric() {
            // Non-ASCII letters: keep them lowercased so a French title key
            // does not collapse into dashes.
            ch.to_lowercase().next()
        } else {
            None
        };
        match mapped {
            Some(ch) => {
                out.push(ch);
                last_dash = false;
            }
            None => {
                if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `ux1-pr229-cert` → `UX1 PR229 Cert`. Same rule as the situation builder;
/// duplicated here so the store has no dependency on the API layer.
pub fn humanize_track_key(key: &str) -> String {
    key.split(['-', '_', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let has_digit = part.chars().any(|c| c.is_ascii_digit());
            if (has_digit && part.len() <= 6) || part.len() <= 3 {
                part.to_ascii_uppercase()
            } else {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a legacy `status` string moves lifecycle / blocker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackTransition {
    Keep,
    Activate,
    Block,
    Cancel,
}

impl TrackTransition {
    fn from_legacy_status(status: Option<&str>) -> Result<Self, TrackWriteError> {
        match status.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(Self::Keep),
            Some(
                "open" | "active" | "running" | "in-progress" | "in_progress" | "ready" | "waiting"
                | "planned" | "executing" | "pending" | "proposed" | "settled",
            ) => Ok(Self::Activate),
            Some("blocked") => Ok(Self::Block),
            Some("cancelled" | "canceled") => Ok(Self::Cancel),
            Some(
                done @ ("done" | "closed" | "satisfied" | "accepted" | "complete" | "completed"),
            ) => Err(TrackWriteError::NeedsReceipt(done.to_string())),
            Some(other) => Err(TrackWriteError::Invalid(format!(
                "unknown track status '{other}' (open | blocked | cancelled; done needs a receipt)"
            ))),
        }
    }

    fn update_clauses(self) -> (&'static str, &'static str) {
        match self {
            Self::Keep => ("", ""),
            Self::Activate => ("lifecycle = 'active',", "explicit_blocker = NULL,"),
            Self::Block => (
                "lifecycle = 'active',",
                "explicit_blocker = COALESCE(?2, explicit_blocker, 'blocked'),",
            ),
            Self::Cancel => ("lifecycle = 'cancelled',", ""),
        }
    }

    fn insert_values(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Keep | Self::Activate => ("active", None),
            Self::Block => ("active", Some("blocked")),
            Self::Cancel => ("cancelled", None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackWriteError {
    /// The caller tried to mark a track done/closed/satisfied. That is a
    /// receipt (`POST /tracks/:track/accept`), not a status write.
    NeedsReceipt(String),
    Invalid(String),
    Store(String),
}

impl std::fmt::Display for TrackWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsReceipt(status) => write!(
                f,
                "status '{status}' cannot be written: a track is satisfied only by accepted evidence (accept_project_track)"
            ),
            Self::Invalid(message) | Self::Store(message) => f.write_str(message),
        }
    }
}

impl From<TrackWriteError> for String {
    fn from(error: TrackWriteError) -> Self {
        error.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptWriteError {
    IdempotencyMismatch {
        idempotency_key: String,
        prior_receipt_id: String,
    },
    Store(String),
}

impl std::fmt::Display for ReceiptWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdempotencyMismatch {
                idempotency_key,
                prior_receipt_id,
            } => write!(
                f,
                "idempotency key '{idempotency_key}' was already used with a different body (receipt {prior_receipt_id})"
            ),
            Self::Store(message) => f.write_str(message),
        }
    }
}

impl From<ReceiptWriteError> for String {
    fn from(error: ReceiptWriteError) -> Self {
        error.to_string()
    }
}

/// Verifier classes (design doc, "Completion and truth") plus the two PR
/// shapes controllers actually produce.
pub const EVIDENCE_KINDS: &[&str] = &[
    "pr_merged",
    "pr_head_review",
    "review",
    "command",
    "external_state",
    "operator",
    "manual",
];

/// Where the evidence's immutable handle lives, for the observers.
pub fn evidence_subject_type(kind: &str) -> &'static str {
    match kind {
        "pr_merged" | "pr_head_review" | "review" => "pr",
        "command" => "command",
        "external_state" => "external",
        _ => "operator",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct EvidenceInput {
    #[serde(default)]
    pub criterion_id: Option<String>,
    pub kind: String,
    /// Immutable handle: `owner/repo#233@<head sha>`, a commit, a job id.
    pub subject_id: String,
    #[serde(default)]
    pub verifier: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AcceptRequest {
    pub idempotency_key: String,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    pub evidence: Vec<EvidenceInput>,
    #[serde(default = "default_actor_type")]
    pub actor_type: String,
    #[serde(default = "default_actor_id")]
    pub actor_id: String,
    #[serde(default)]
    pub observed_at: Option<String>,
}

fn default_actor_type() -> String {
    "controller".into()
}

fn default_actor_id() -> String {
    "unknown".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptResult {
    pub track: ProjectTrack,
    pub receipts: Vec<Receipt>,
    /// True when every receipt already existed (idempotent retry).
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptError {
    NotFound,
    StaleRevision {
        expected: u64,
        current: u64,
    },
    Invalid(String),
    IdempotencyMismatch {
        idempotency_key: String,
        prior_receipt_id: String,
    },
    Store(String),
}

impl std::fmt::Display for AcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such track"),
            Self::StaleRevision { expected, current } => write!(
                f,
                "stale revision: expected {expected}, track is at {current}"
            ),
            Self::Invalid(message) | Self::Store(message) => f.write_str(message),
            Self::IdempotencyMismatch {
                idempotency_key,
                prior_receipt_id,
            } => write!(
                f,
                "idempotency key '{idempotency_key}' was already used with a different body (receipt {prior_receipt_id})"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AbsorbOutcome {
    /// Canonical key the mission is now attached to.
    pub key: String,
    pub track_id: String,
    /// True when an `absorbed` row was created.
    pub created: bool,
    /// `key` | `alias` | `pr_ref` | `created`
    pub matched_by: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRequest {
    pub slug: String,
    pub track: String,
    pub mutation_domain: String,
    pub attempt_id: String,
    /// `reader` | `writer`
    pub mode: String,
    pub idempotency_key: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrackLease {
    pub id: String,
    pub slug: String,
    pub track_id: String,
    /// Canonical track key (joined for convenience).
    pub track: String,
    pub mutation_domain: String,
    pub attempt_id: String,
    pub mode: String,
    pub state: String,
    pub lease_until: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    NotFound,
    Owned {
        holder_attempt_id: String,
        lease_until: String,
        lease_id: String,
    },
    Store(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such track"),
            Self::Owned {
                holder_attempt_id,
                lease_until,
                ..
            } => write!(
                f,
                "track is owned by mission {holder_attempt_id} until {lease_until}"
            ),
            Self::Store(message) => f.write_str(message),
        }
    }
}

const LEASE_SELECT: &str =
    "SELECT l.id, l.slug, l.track_id, t.track, l.mutation_domain, l.attempt_id, l.mode, \
     l.state, l.lease_until, l.idempotency_key, l.created_at, l.updated_at \
     FROM track_leases l JOIN project_tracks t ON t.id = l.track_id";

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackLease> {
    Ok(TrackLease {
        id: row.get(0)?,
        slug: row.get(1)?,
        track_id: row.get(2)?,
        track: row.get(3)?,
        mutation_domain: row.get(4)?,
        attempt_id: row.get(5)?,
        mode: row.get(6)?,
        state: row.get(7)?,
        lease_until: row.get(8)?,
        idempotency_key: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// An immutable receipt row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Receipt {
    pub id: String,
    pub idempotency_key: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    pub subject_type: String,
    pub subject_id: String,
    pub outcome: String,
    pub actor_type: String,
    pub actor_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_receipt_id: Option<String>,
    pub observed_at: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReceipt {
    pub idempotency_key: String,
    pub kind: String,
    pub project_slug: Option<String>,
    pub track_id: Option<String>,
    pub criterion_id: Option<String>,
    pub subject_type: String,
    pub subject_id: String,
    pub outcome: String,
    pub actor_type: String,
    pub actor_id: String,
    pub verifier: Option<String>,
    pub supersedes_receipt_id: Option<String>,
    pub observed_at: String,
    pub payload: serde_json::Value,
}

impl NewReceipt {
    /// Hash of everything but the idempotency key, so a key reused with a
    /// different body is detectable.
    pub fn request_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let body = serde_json::json!({
            "kind": self.kind,
            "project_slug": self.project_slug,
            "track_id": self.track_id,
            "criterion_id": self.criterion_id,
            "subject_type": self.subject_type,
            "subject_id": self.subject_id,
            "outcome": self.outcome,
            "actor_type": self.actor_type,
            "actor_id": self.actor_id,
            "verifier": self.verifier,
            "supersedes_receipt_id": self.supersedes_receipt_id,
            "observed_at": self.observed_at,
            "payload": self.payload,
        });
        let mut hasher = Sha256::new();
        hasher.update(body.to_string().as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

const RECEIPT_SELECT: &str =
    "SELECT id, idempotency_key, kind, project_slug, track_id, criterion_id, subject_type, \
     subject_id, outcome, actor_type, actor_id, verifier, supersedes_receipt_id, observed_at, \
     payload, created_at FROM receipts";

fn receipt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    let payload: String = row.get(14)?;
    Ok(Receipt {
        id: row.get(0)?,
        idempotency_key: row.get(1)?,
        kind: row.get(2)?,
        project_slug: row.get(3)?,
        track_id: row.get(4)?,
        criterion_id: row.get(5)?,
        subject_type: row.get(6)?,
        subject_id: row.get(7)?,
        outcome: row.get(8)?,
        actor_type: row.get(9)?,
        actor_id: row.get(10)?,
        verifier: row.get(11)?,
        supersedes_receipt_id: row.get(12)?,
        observed_at: row.get(13)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        created_at: row.get(15)?,
    })
}

/// Input for `upsert_proposals`.
#[derive(Debug, Clone)]
pub struct NewProposal {
    pub task_key: String,
    pub title: String,
    pub prompt: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub depends_on: Vec<String>,
}

/// Input for `upsert_planned_tracks` — the chat-submitted item contract.
#[derive(Debug, Clone)]
pub struct PlannedTrack {
    pub track: String,
    pub title: String,
    pub desired_state: String,
    pub acceptance_criteria: Vec<String>,
    pub depends_on: Vec<String>,
    pub position: Option<i64>,
}

/// JSON-array column → Vec, treating NULL/garbage as empty rather than erroring
/// the whole roadmap read.
fn parse_string_list(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
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
    /// observe | propose | act_reversible | act_full
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy_level: Option<String>,
}

/// One workstream within a project.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectTrack {
    /// Stable id, survives key renames.
    pub id: String,
    /// Normalized key.
    pub track: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<String>,
    /// Legacy vocabulary, **derived**: `cancelled` | `done` (claim only) |
    /// `satisfied` (accepted evidence) | `blocked` | `open`. Kept for readers
    /// that predate `lifecycle`; the store never persists it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub updated_at: String,
    pub lifecycle: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_blocker: Option<String>,
    pub revision: u64,
    /// Roadmap order (explicit via planning, else insertion order).
    pub position: u64,
    /// Immutable artifact version the standing evidence was accepted at
    /// (normally a commit SHA). Derived from the newest standing accept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governed_artifact_version: Option<String>,
    /// When the standing claim was accepted (newest standing receipt).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_at: Option<String>,
    /// Kind of the standing claim: `accept` only when every declared
    /// criterion has un-invalidated accepted evidence (or the track declares
    /// none and has at least one), `legacy_import` for pre-receipt claims.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
}

/// True when `needle` appears in `hay` without being a prefix of a longer
/// number (`#1` must not match `#10`).
fn text_mentions_needle(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    if needle.starts_with('#') {
        let mut start = 0;
        while let Some(rel) = hay[start..].find(needle) {
            let abs = start + rel;
            let after = &hay[abs + needle.len()..];
            if after
                .chars()
                .next()
                .map(|c| !c.is_ascii_digit())
                .unwrap_or(true)
            {
                return true;
            }
            start = abs + 1;
        }
        return false;
    }
    hay.contains(needle)
}

fn decision_mentions_any(decision: &ProjectDecision, needles: &[String]) -> bool {
    let evidence = decision
        .evidence
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let rationale = decision.rationale.as_deref().unwrap_or("");
    let hay = format!("{} {} {}", decision.question, rationale, evidence);
    needles
        .iter()
        .any(|needle| text_mentions_needle(&hay, needle))
}

/// One ledger entry: an owner escalation or a declared autonomous act.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectDecision {
    pub at: String,
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// granted | escalation
    pub authority: String,
    /// decided | pending_user | answered | expired
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rename moves every table that keys on the slug — record, grant
    /// (FK child), binding, tracks, state events, decisions — and the old
    /// slug is gone from all of them.
    #[test]
    fn rename_moves_every_slug_keyed_row() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("old-name", Some("Title"), None, None, Some("job123"))
            .expect("create");
        store
            .set_grant("old-name", Some("full"), None, None, None, None, None, None)
            .expect("grant");
        store.set_binding("old-name", "sess-1", None).expect("bind");
        store
            .set_track("old-name", "track-a", Some("green"), Some("running"))
            .expect("track");
        store
            .record_state(
                "old-name",
                "sig|state",
                Some("headline"),
                "2026-08-13T10:00:00Z",
                None,
            )
            .expect("state");

        let renamed = store
            .rename_project("old-name", "new-name")
            .expect("rename");
        assert_eq!(renamed.slug, "new-name");
        assert_eq!(renamed.controller_cron_id.as_deref(), Some("job123"));

        assert!(store.get_project("old-name").expect("read").is_none());
        assert_eq!(
            store
                .binding("new-name")
                .expect("read")
                .expect("bound")
                .session_id,
            "sess-1"
        );
        assert!(store.binding("old-name").expect("read").is_none());
        assert_eq!(store.tracks("new-name").expect("tracks").len(), 1);
        assert_eq!(
            store
                .state_timeline("new-name", 10)
                .expect("timeline")
                .len(),
            1
        );
        assert!(store
            .state_timeline("old-name", 10)
            .expect("timeline")
            .is_empty());
    }

    /// Renaming onto an existing project is refused: a merge stays an explicit
    /// alias-map operation, never an accidental clobber.
    #[test]
    fn rename_refuses_to_clobber_an_existing_project() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("a", None, None, None, None)
            .expect("a");
        store
            .upsert_project("b", None, None, None, None)
            .expect("b");
        let error = store.rename_project("a", "b").expect_err("must refuse");
        assert!(error.contains("already exists"), "{error}");
        assert!(store.get_project("a").expect("read").is_some());
    }

    #[test]
    fn rename_of_a_missing_project_is_not_found() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let error = store.rename_project("ghost", "x").expect_err("must fail");
        assert!(error.contains("not found"), "{error}");
    }

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
                .record_state("verity", "phase1-blocked", Some("still blocked"), at, None)
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
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z", None)
            .expect("record");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z", None)
            .expect("record");
        store
            .record_state("verity", "merged", None, "2026-08-04T10:30:00Z", None)
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
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z", None)
            .expect("record");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z", None)
            .expect("record");
        for _ in 0..5 {
            assert_eq!(
                store
                    .record_state("verity", "blocked", None, "2026-08-04T10:15:00Z", None)
                    .expect("replay"),
                0
            );
        }
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline[0].observations, 2);
    }

    #[test]
    fn silent_replay_returns_zero_and_does_not_extend() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert_eq!(
            store
                .record_silent_observation("verity", "ctrl:active", "2026-08-04T10:00:00Z", None)
                .expect("first"),
            1
        );
        assert_eq!(
            store
                .record_silent_observation("verity", "ctrl:blocked", "2026-08-04T10:00:00Z", None)
                .expect("replay"),
            0
        );
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline[0].observations, 1);
    }

    #[test]
    fn touch_freshness_does_not_bump_observations_or_change_headline() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state(
                "verity-core",
                "pr-2397-merged",
                Some("PR #2397 merged"),
                "2026-08-24T13:28:30Z",
                Some("1299f6"),
            )
            .expect("record");
        assert_eq!(
            store
                .touch_freshness("verity-core", "2026-08-24T13:30:46Z", Some("1299f6"))
                .expect("touch"),
            1
        );
        let timeline = store.state_timeline("verity-core", 5).expect("timeline");
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].observations, 1);
        assert_eq!(timeline[0].headline.as_deref(), Some("PR #2397 merged"));
        assert_eq!(timeline[0].last_seen_at, "2026-08-24T13:30:46Z");
        let material = store.latest_controller_states().expect("material");
        assert_eq!(
            material["verity-core"].headline.as_deref(),
            Some("PR #2397 merged")
        );
    }

    #[test]
    fn canonical_bind_drops_nickname_rows() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .set_binding("verity", "old-session", None)
            .expect("alias bind");
        store
            .set_canonical_binding(
                "verity-core",
                "1299f6",
                &[
                    "verity".into(),
                    "verity-core".into(),
                    "verity-roadmap".into(),
                ],
                None,
            )
            .expect("canonical");
        assert_eq!(
            store
                .binding("verity-core")
                .expect("canonical row")
                .unwrap()
                .session_id,
            "1299f6"
        );
        assert!(store.binding("verity").expect("alias gone").is_none());
        assert_eq!(
            store
                .binding_for_canonical("verity-core", &["verity".into()])
                .expect("lookup")
                .unwrap()
                .session_id,
            "1299f6"
        );
    }

    /// An out-of-order delivery from the overlap window must not be recorded
    /// as a transition — that would fabricate a flap between two states the
    /// project never actually made.
    #[test]
    fn an_older_delivery_never_fabricates_a_transition() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:00:00Z", None)
            .expect("record");
        store
            .record_state("verity", "merged", None, "2026-08-04T11:00:00Z", None)
            .expect("record");
        // Arrives late, older than the current state.
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:30:00Z", None)
            .expect("late");

        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline.len(), 2, "no third row for the replayed state");
        assert_eq!(timeline[0].signature, "merged");
    }

    #[test]
    fn projects_keep_separate_timelines() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state("verity", "a", None, "2026-08-04T10:00:00Z", None)
            .expect("record");
        store
            .record_state("lido", "b", None, "2026-08-04T10:01:00Z", None)
            .expect("record");
        store
            .record_state("lido", "b", None, "2026-08-04T10:02:00Z", None)
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
            .record_state("verity", "s", None, "2026-08-04T10:00:00Z", None)
            .expect("record");
        store
            .record_state(
                "verity",
                "s",
                Some("now we know"),
                "2026-08-04T10:15:00Z",
                None,
            )
            .expect("record");
        store
            .record_state("verity", "s", None, "2026-08-04T10:30:00Z", None)
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
        let after_http = store.get_project("bench").unwrap().unwrap();
        assert_eq!(after_http.wait_ticks, 0);
        assert!(
            after_http.mode_signal_at.is_some(),
            "HTTP set_mode must stamp mode_signal_at so explicit blockers stay fresh"
        );

        // Same mode+blocker two more ticks: the counter is how long it's been here.
        store
            .set_mode("bench", "blocked", Some("x"), Some("transport-cap"))
            .expect("m2");
        store
            .set_mode("bench", "blocked", Some("x"), Some("transport-cap"))
            .expect("m3");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 1);

        // Rephrasing next= is not a new stall.
        store
            .set_mode(
                "bench",
                "blocked",
                Some("watch for a new Lido PR"),
                Some("transport-cap"),
            )
            .expect("m3b");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 2);

        // A changed blocker resets the counter — a new thing to be stuck on.
        store
            .set_mode("bench", "blocked", Some("x"), Some("other"))
            .expect("m4");
        assert_eq!(store.get_project("bench").unwrap().unwrap().wait_ticks, 0);
    }

    #[test]
    fn implement_ready_track_beats_watch_idle_next_action() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .set_track(
                "lido",
                "p-reserve-relational",
                Some("implement"),
                Some("open"),
            )
            .expect("track");
        store
            .set_mode(
                "lido",
                "active",
                Some("watch for a new Lido PR or exact-head finding"),
                None,
            )
            .expect("mode");
        let project = store.get_project("lido").unwrap().unwrap();
        assert_eq!(
            project.next_action.as_deref(),
            Some("implement p-reserve-relational")
        );
    }

    #[test]
    fn trailer_wait_zero_does_not_reset_an_existing_stall() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .project_mode_from_signal("lido", "blocked", 4, Some("watch PRs"), Some("disk"), None)
            .expect("first");
        store
            .project_mode_from_signal(
                "lido",
                "blocked",
                0,
                Some("surveiller toute nouvelle PR"),
                Some("disk"),
                None,
            )
            .expect("rephrase");
        let project = store.get_project("lido").unwrap().unwrap();
        assert_eq!(project.wait_ticks, 4);
        assert_eq!(project.blocker.as_deref(), Some("disk"));
    }

    #[test]
    fn set_mode_on_unknown_project_errors() {
        let store = ProjectsStore::open_in_memory().expect("store");
        assert!(store.set_mode("ghost", "active", None, None).is_err());
    }

    #[test]
    fn mode_projection_is_idempotent_and_never_fabricates() {
        let store = ProjectsStore::open_in_memory().expect("store");
        // No project row: the ingestor must not create one from a trailer.
        store
            .project_mode_from_signal("ghost", "active", 0, None, None, None)
            .expect("no-op, not an error");
        assert!(store.get_project("ghost").expect("read").is_none());

        store
            .upsert_project("bench", None, None, None, None)
            .expect("seed");
        // Replaying the same signal twice does not inflate wait — it is passed
        // in, not incremented, so overlapping ingest cycles are harmless.
        store
            .project_mode_from_signal("bench", "blocked", 2, None, Some("transport-cap"), None)
            .expect("m1");
        store
            .project_mode_from_signal("bench", "blocked", 2, None, Some("transport-cap"), None)
            .expect("m2 replay");
        let p = store.get_project("bench").expect("read").expect("present");
        assert_eq!(p.mode.as_deref(), Some("blocked"));
        assert_eq!(p.wait_ticks, 2);
        assert_eq!(p.blocker.as_deref(), Some("transport-cap"));
    }

    #[test]
    fn mode_projection_does_not_wipe_next_action_when_ingest_passes_none() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .set_mode("lido", "active", Some("certify #88"), None)
            .expect("set");
        store
            .project_mode_from_signal("lido", "active", 0, None, None, None)
            .expect("ingest wipe?");
        let p = store.get_project("lido").expect("read").expect("present");
        assert_eq!(p.next_action.as_deref(), Some("certify #88"));
        store
            .project_mode_from_signal(
                "lido",
                "active",
                0,
                Some("2 live: certify #88 · reserve scout"),
                None,
                None,
            )
            .expect("explicit");
        let p = store.get_project("lido").expect("read").expect("present");
        assert_eq!(
            p.next_action.as_deref(),
            Some("2 live: certify #88 · reserve scout")
        );
    }

    #[test]
    fn mode_signal_watermark_refuses_older_deliveries() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("bench", None, None, None, None)
            .expect("seed");
        store
            .set_mode("bench", "active", None, None)
            .expect("http set_mode");
        store.set_mode("bench", "active", None, None).expect("tick");
        let before = store.get_project("bench").expect("read").expect("present");
        assert_eq!(before.mode.as_deref(), Some("active"));
        assert_eq!(before.wait_ticks, 1);

        store
            .project_mode_from_signal(
                "bench",
                "blocked",
                0,
                None,
                Some("stale-callback"),
                Some("2020-01-01T00:00:00Z"),
            )
            .expect("older signal");
        let after = store.get_project("bench").expect("read").expect("present");
        assert_eq!(after.mode.as_deref(), Some("active"));
        assert_eq!(after.wait_ticks, 1);
        assert_eq!(after.blocker, None);
    }

    #[test]
    fn rfc3339_watermark_compares_instants_not_suffixes() {
        assert!(rfc3339_at_least(
            "2026-08-16T12:00:00Z",
            "2026-08-16T12:00:00+00:00"
        ));
        assert!(rfc3339_at_least(
            "2026-08-16T12:00:00+00:00",
            "2026-08-16T12:00:00Z"
        ));
        assert!(!rfc3339_at_least(
            "2026-08-16T11:00:00Z",
            "2026-08-16T12:00:00+00:00"
        ));
        assert!(rfc3339_after(
            "2026-08-16T13:00:00Z",
            "2026-08-16T12:00:00+00:00"
        ));
        assert!(!rfc3339_after(
            "2026-08-16T12:00:00Z",
            "2026-08-16T12:00:00+00:00"
        ));
    }

    #[test]
    fn mode_signal_at_backfills_from_updated_at_on_existing_modes() {
        let connection = Connection::open_in_memory().expect("conn");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                    slug TEXT PRIMARY KEY NOT NULL,
                    title TEXT, objective TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    mode TEXT,
                    wait_ticks INTEGER NOT NULL DEFAULT 0,
                    next_action TEXT, blocker TEXT,
                    controller_cron_id TEXT, repository TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                INSERT INTO projects
                    (slug, status, mode, wait_ticks, created_at, updated_at)
                VALUES
                    ('bench', 'active', 'active', 0,
                     '2026-08-01T00:00:00Z', '2026-08-10T12:00:00Z'),
                    ('idle', 'active', NULL, 0,
                     '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z');",
            )
            .expect("old schema");
        ProjectsStore::initialize(&connection).expect("migrate");
        let stamped: String = connection
            .query_row(
                "SELECT mode_signal_at FROM projects WHERE slug = 'bench'",
                [],
                |row| row.get(0),
            )
            .expect("stamped");
        assert_eq!(stamped, "2026-08-10T12:00:00Z");
        let idle: Option<String> = connection
            .query_row(
                "SELECT mode_signal_at FROM projects WHERE slug = 'idle'",
                [],
                |row| row.get(0),
            )
            .expect("idle");
        assert_eq!(idle, None);
    }

    #[test]
    fn roadmap_proposals_upsert_update_cancel_revive() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");

        let plan = |key: &str, title: &str| NewProposal {
            task_key: key.to_string(),
            title: title.to_string(),
            prompt: None,
            acceptance_criteria: vec!["lane green".to_string()],
            depends_on: vec![],
        };
        store
            .upsert_proposals("lido", &[plan("guarantee-3", "Third guarantee")])
            .expect("plan");

        let open = store.list_open_proposals("lido").expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].task_key, "guarantee-3");
        assert_eq!(open[0].acceptance_criteria, vec!["lane green"]);

        // Patch title only — everything else survives.
        assert!(store
            .update_proposal(
                "lido",
                "guarantee-3",
                Some("Third guarantee (srv3)"),
                None,
                None,
                None
            )
            .expect("update"));
        let open = store.list_open_proposals("lido").expect("list");
        assert_eq!(open[0].title, "Third guarantee (srv3)");
        assert_eq!(open[0].acceptance_criteria, vec!["lane green"]);

        // Cancel closes it; updates then miss; re-planning revives it.
        assert!(store
            .cancel_proposal("lido", "guarantee-3")
            .expect("cancel"));
        assert!(store.list_open_proposals("lido").expect("list").is_empty());
        assert!(!store
            .update_proposal("lido", "guarantee-3", Some("x"), None, None, None)
            .expect("update-cancelled"));
        store
            .upsert_proposals("lido", &[plan("guarantee-3", "Back on the plan")])
            .expect("revive");
        let open = store.list_open_proposals("lido").expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].title, "Back on the plan");

        // Unknown keys report false, not errors.
        assert!(!store.cancel_proposal("lido", "ghost").expect("ghost"));

        // Proposals ride project lifecycle: rename moves them, delete drops them.
        store.rename_project("lido", "lido-v2").expect("rename");
        assert!(store.list_open_proposals("lido").expect("old").is_empty());
        assert_eq!(store.list_open_proposals("lido-v2").expect("new").len(), 1);
        assert!(store.delete_project("lido-v2").expect("delete"));
        assert!(store
            .list_open_proposals("lido-v2")
            .expect("gone")
            .is_empty());
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
                Some("act_reversible"),
            )
            .expect("grant");
        let g = store.get_grant("lido").expect("read").expect("present");
        assert_eq!(g.merge_authority.as_deref(), Some("review-first"));
        assert_eq!(g.parallel_missions, Some(3));
        assert_eq!(g.autonomy_level.as_deref(), Some("act_reversible"));
        assert_eq!(
            store.autonomy_levels().expect("levels")["lido"],
            "act_reversible"
        );

        store
            .set_track("lido", "P-ETH-1", Some("proved"), Some("in-progress"))
            .expect("track");
        let tracks = store.tracks("lido").expect("tracks");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track, "p-eth-1", "keys are normalized on write");
        assert_eq!(
            store.resolve_track_key("lido", "P-ETH-1").expect("alias"),
            Some("p-eth-1".into())
        );

        store
            .record_decision("lido", &escalation("merge #48?", Some("blocks A.2")))
            .expect("dec");
        assert_eq!(store.open_decisions("lido").expect("open").len(), 1);

        store
            .set_binding("lido", "control-session", None)
            .expect("binding");
        store
            .record_state(
                "lido",
                "active",
                Some("working"),
                "2026-08-04T10:00:00Z",
                Some("delivery-session"),
            )
            .expect("state");

        // A true project delete removes every project-owned row. Missions and
        // conversation messages are separate stores and are not represented
        // here.
        assert!(store.delete_project("lido").expect("delete"));
        assert!(store.get_project("lido").expect("project").is_none());
        assert!(store
            .get_grant("lido")
            .expect("grant after delete")
            .is_none());
        assert!(store
            .tracks("lido")
            .expect("tracks after delete")
            .is_empty());
        assert!(store
            .open_decisions("lido")
            .expect("decisions after delete")
            .is_empty());
        assert!(store
            .state_timeline("lido", 10)
            .expect("timeline after delete")
            .is_empty());
        assert!(store
            .binding("lido")
            .expect("binding after delete")
            .is_none());
        assert!(!store.delete_project("lido").expect("second delete"));
    }

    // ---- session_id on state events ----

    #[test]
    fn the_newest_delivery_session_rides_on_the_state_row() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_state(
                "verity",
                "blocked",
                None,
                "2026-08-04T10:00:00Z",
                Some("s1"),
            )
            .expect("record");
        // Extending the row moves the session to the newest delivery's.
        store
            .record_state(
                "verity",
                "blocked",
                None,
                "2026-08-04T10:15:00Z",
                Some("s2"),
            )
            .expect("extend");
        // A delivery with no session must not erase what we have.
        store
            .record_state("verity", "blocked", None, "2026-08-04T10:30:00Z", None)
            .expect("extend without session");

        let latest = store.latest_states().expect("latest");
        let state = latest.get("verity").expect("row");
        assert_eq!(state.session_id.as_deref(), Some("s2"));
        assert_eq!(state.signature, "blocked");
        assert_eq!(state.observations, 3);
        assert_eq!(store.state_event_totals().expect("totals")["verity"], 3);
    }

    /// An existing database predating the `session_id` column must gain it on
    /// open — `CREATE TABLE IF NOT EXISTS` alone would leave it missing.
    #[test]
    fn an_old_state_events_table_gains_the_session_column() {
        let connection = Connection::open_in_memory().expect("conn");
        connection
            .execute_batch(
                "CREATE TABLE project_state_events (
                    slug TEXT NOT NULL, signature TEXT NOT NULL, headline TEXT,
                    first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                    observations INTEGER NOT NULL DEFAULT 1,
                    PRIMARY KEY (slug, first_seen_at));
                 INSERT INTO project_state_events VALUES
                    ('verity', 'blocked', NULL, '2026-08-04T10:00:00Z',
                     '2026-08-04T10:00:00Z', 1);",
            )
            .expect("old schema");
        ProjectsStore::initialize(&connection).expect("migrate");
        let store = ProjectsStore {
            connection: Mutex::new(connection),
        };
        let timeline = store.state_timeline("verity", 10).expect("timeline");
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].session_id, None);
        store
            .record_state(
                "verity",
                "blocked",
                None,
                "2026-08-04T10:15:00Z",
                Some("s1"),
            )
            .expect("write through the new column");
        assert_eq!(
            store.latest_states().expect("latest")["verity"]
                .session_id
                .as_deref(),
            Some("s1")
        );
    }

    // ---- decision ledger ----

    fn escalation(question: &str, rationale: Option<&str>) -> NewDecision {
        NewDecision {
            question: question.to_string(),
            rationale: rationale.map(str::to_string),
            kind: None,
            authority: "escalation".to_string(),
            status: "pending_user".to_string(),
            evidence: None,
        }
    }

    #[test]
    fn autonomous_acts_never_appear_open_and_answers_close_escalations() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let act = NewDecision {
            question: "Merged PR #2213".to_string(),
            rationale: Some("CI green, criteria met".to_string()),
            kind: Some("merge".to_string()),
            authority: "granted".to_string(),
            status: "decided".to_string(),
            evidence: Some(serde_json::json!({"pr_url": "https://github.com/x/y/pull/2213"})),
        };
        store.record_decision("verity", &act).expect("act");
        let at = store
            .record_decision("verity", &escalation("Ship v2?", None))
            .expect("escalation");

        // The act is activity, not a question: open shows only the escalation.
        let open = store.open_decisions("verity").expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].question, "Ship v2?");

        // Answering closes it and stamps the answer.
        assert!(store
            .answer_decision("verity", &at, "yes, ship")
            .expect("answer"));
        assert!(store.open_decisions("verity").expect("open").is_empty());
        // Answering twice (or a bogus key) is a no-op, not an error.
        assert!(!store
            .answer_decision("verity", &at, "again")
            .expect("re-answer"));

        // Recent activity = the act plus the answered escalation, newest first.
        let recent = store.recent_decisions("verity", 10).expect("recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].answer.as_deref(), Some("yes, ship"));
        assert_eq!(recent[1].kind.as_deref(), Some("merge"));
        assert_eq!(
            recent[1].evidence.as_ref().unwrap()["pr_url"],
            "https://github.com/x/y/pull/2213"
        );

        store
            .record_state(
                "verity",
                "pr-81|review",
                Some("Lido #81 — RÉPARATION/REVIEW EN COURS"),
                "2026-08-16T15:30:54Z",
                Some("cron_1"),
            )
            .expect("state");
        store
            .record_state(
                "verity",
                "mission-callback|x",
                Some("[Mission callback: Repair Lido #76]"),
                "2026-08-16T15:31:00Z",
                Some("s"),
            )
            .expect("inspect");
        let activity = store.recent_activity("verity", 10).expect("activity");
        assert!(
            activity
                .iter()
                .any(|d| d.question.contains("#81") && d.status.as_deref() == Some("decided")),
            "controller chapter must surface on Recent activity"
        );
        assert!(
            activity
                .iter()
                .all(|d| !d.question.starts_with("[Mission callback:")),
            "inspect callbacks must not flood Recent activity"
        );

        assert_eq!(store.pending_decision_counts().expect("counts").len(), 0);
        store
            .record_decision("verity", &escalation("Another?", None))
            .expect("second escalation");
        assert_eq!(
            store.pending_decision_counts().expect("counts")["verity"],
            1
        );
    }

    #[test]
    fn pending_user_questions_dedupe_and_expire() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let first = store
            .record_decision(
                "coldcard",
                &escalation("relancer coldcard_skip depuis le checkpoint ?", None),
            )
            .expect("first");
        let second = store
            .record_decision(
                "coldcard",
                &escalation("Relancer  coldcard_skip depuis le checkpoint ?", None),
            )
            .expect("dup");
        assert_eq!(first, second, "duplicate question must reuse the row");
        assert_eq!(store.open_decisions("coldcard").expect("open").len(), 1);

        let vl_a = store
            .record_decision(
                "verity-lido",
                &escalation(
                    "VL-002 — rétablir de préférence l'OAuth Codex, ou la facturation Muse.",
                    None,
                ),
            )
            .expect("vl a");
        let vl_b = store
            .record_decision(
                "verity-lido",
                &escalation(
                    "VL-002 — restaurer Codex OAuth sandboxed.sh ou Muse billing pour débloquer P-ETH-1.",
                    None,
                ),
            )
            .expect("vl b");
        assert_eq!(vl_a, vl_b, "same VL-002 ticket must reuse the pending row");
        assert_eq!(store.open_decisions("verity-lido").expect("open").len(), 1);

        let inserted = store
            .record_decision_from_delivery(
                "coldcard",
                "2026-08-13T20:22:14Z",
                &escalation("relancer coldcard_skip depuis le checkpoint ?", None),
            )
            .expect("from delivery");
        assert!(!inserted, "ingest must not insert a second pending row");

        {
            let aged = (Utc::now() - chrono::Duration::hours(25)).to_rfc3339();
            let connection = store.lock().expect("lock");
            connection
                .execute(
                    "UPDATE project_decisions SET at = ?1 WHERE slug = 'coldcard'",
                    params![aged],
                )
                .expect("age");
        }
        let expired = store
            .expire_pending_decisions(chrono::Duration::hours(24))
            .expect("expire");
        assert_eq!(expired, 1);
        assert!(store.open_decisions("coldcard").expect("open").is_empty());
    }

    #[test]
    fn a_merged_pr_closes_only_the_pending_decisions_that_name_it() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .record_decision("lido", &escalation("merge #66?", Some("blocks A.2")))
            .expect("66");
        store
            .record_decision("lido", &escalation("merge #70?", None))
            .expect("70");
        let same_delivery = store
            .record_decision(
                "lido",
                &escalation("Merged #66", Some("just recorded this tick")),
            )
            .expect("same-tick");

        let closed = store
            .close_pending_decisions_referencing(
                "lido",
                &["#66".to_string()],
                "closed: referenced PR merged",
                Some(same_delivery.as_str()),
            )
            .expect("close");
        assert_eq!(closed, 1, "only the older #66 question closes");

        let open = store.open_decisions("lido").expect("open");
        let questions: Vec<&str> = open.iter().map(|d| d.question.as_str()).collect();
        assert!(questions.contains(&"merge #70?"));
        assert!(questions.contains(&"Merged #66"));
        assert!(!questions.iter().any(|q| q.contains("#66?")));

        // `#6` is not `#66`.
        assert_eq!(
            store
                .close_pending_decisions_referencing(
                    "lido",
                    &["#6".to_string()],
                    "should not match",
                    None,
                )
                .expect("narrow"),
            0
        );

        // A URL needle matches evidence.pr_url.
        let mut with_url = escalation("ship the cert?", None);
        with_url.evidence = Some(serde_json::json!({
            "pr_url": "https://github.com/lfglabs-dev/verity/pull/70"
        }));
        store.record_decision("lido", &with_url).expect("url");
        assert_eq!(
            store
                .close_pending_decisions_referencing(
                    "lido",
                    &["https://github.com/lfglabs-dev/verity/pull/70".to_string()],
                    "closed: referenced PR merged",
                    None,
                )
                .expect("url close"),
            1
        );
        let left: Vec<String> = store
            .open_decisions("lido")
            .expect("left")
            .into_iter()
            .map(|d| d.question)
            .collect();
        assert_eq!(
            left,
            vec!["merge #70?".to_string(), "Merged #66".to_string()]
        );
    }

    /// A database from before the ledger columns must migrate: legacy rows are
    /// owner escalations (open → pending_user, resolved → answered), and the
    /// legacy `answered` flag keeps meaning "not open" for old binaries.
    #[test]
    fn a_legacy_decisions_table_backfills_status_on_open() {
        let connection = Connection::open_in_memory().expect("conn");
        connection
            .execute_batch(
                "CREATE TABLE project_decisions (
                    slug TEXT NOT NULL, at TEXT NOT NULL, question TEXT NOT NULL,
                    rationale TEXT, answered INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (slug, at));
                 CREATE TABLE project_grant (
                    slug TEXT PRIMARY KEY NOT NULL, merge_authority TEXT,
                    budget_per_tick TEXT, parallel_missions INTEGER,
                    pause_reason TEXT, resume_condition TEXT, material_bar TEXT,
                    answered_at TEXT);
                 INSERT INTO project_decisions VALUES
                    ('lido', '2026-08-01T10:00:00Z', 'open one', NULL, 0),
                    ('lido', '2026-08-01T11:00:00Z', 'closed one', NULL, 1);",
            )
            .expect("old schema");
        ProjectsStore::initialize(&connection).expect("migrate");
        let store = ProjectsStore {
            connection: Mutex::new(connection),
        };
        let open = store.open_decisions("lido").expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].question, "open one");
        assert_eq!(open[0].authority, "escalation");
        assert_eq!(open[0].status.as_deref(), Some("pending_user"));
        // Migrating twice is harmless.
        ProjectsStore::initialize(&store.connection.lock().unwrap()).expect("re-migrate");

        // Rollback shape: an OLD binary writing into the already-migrated
        // table leaves status NULL (its INSERT names only the legacy
        // columns). The next initialize() must normalize that row even
        // though the column already exists.
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO project_decisions (slug, at, question, rationale, answered) \
                 VALUES ('lido', '2026-08-02T10:00:00Z', 'written during rollback', NULL, 0)",
                [],
            )
            .expect("legacy insert");
        ProjectsStore::initialize(&store.connection.lock().unwrap())
            .expect("post-rollback migrate");
        let open = store.open_decisions("lido").expect("open after rollback");
        assert!(
            open.iter().any(|d| d.question == "written during rollback"
                && d.status.as_deref() == Some("pending_user")),
            "a rollback-era row must surface as a pending escalation: {open:?}"
        );
    }

    // ---- unrouted deliveries ----

    #[test]
    fn unrouted_deliveries_dedupe_and_cap_retention() {
        let store = ProjectsStore::open_in_memory().expect("store");
        // Replaying the same delivery (overlapping ingest window) is one row.
        for _ in 0..3 {
            store
                .record_unrouted("s1", "2026-08-04T10:00:00Z", "orphan", None, None, None)
                .expect("record");
        }
        assert_eq!(store.unrouted(100).expect("read").len(), 1);

        // Retention keeps only the newest UNROUTED_RETENTION rows.
        for i in 0..60 {
            store
                .record_unrouted(
                    "s2",
                    &format!("2026-08-05T10:{i:02}:00Z"),
                    "orphan",
                    Some("ghost"),
                    None,
                    None,
                )
                .expect("record");
        }
        let rows = store.unrouted(100).expect("read");
        assert_eq!(rows.len(), ProjectsStore::UNROUTED_RETENTION);
        // Newest first, and the oldest batch (including s1) was evicted.
        assert_eq!(rows[0].at, "2026-08-05T10:59:00Z");
        assert!(rows.iter().all(|row| row.session_id == "s2"));
    }

    #[test]
    fn planned_tracks_persist_the_submitted_contract() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("verity-core", None, None, None, None)
            .expect("seed");
        store
            .upsert_planned_tracks(
                "verity-core",
                &[PlannedTrack {
                    track: "land-2332".into(),
                    title: "Land #2332".into(),
                    desired_state: "merge after certify".into(),
                    acceptance_criteria: vec!["CI green".into(), "review resolved".into()],
                    depends_on: vec!["freeze-head".into()],
                    position: None,
                }],
            )
            .expect("plan");
        let tracks = store.tracks("verity-core").expect("tracks");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title.as_deref(), Some("Land #2332"));
        assert_eq!(
            tracks[0].desired_state.as_deref(),
            Some("merge after certify")
        );
        assert_eq!(tracks[0].status.as_deref(), Some("open"));
        assert_eq!(
            tracks[0].acceptance_criteria,
            vec!["CI green", "review resolved"]
        );
        assert_eq!(tracks[0].depends_on, vec!["freeze-head"]);

        store
            .upsert_planned_tracks(
                "verity-core",
                &[PlannedTrack {
                    track: "land-2332".into(),
                    title: "Land #2332 (retry)".into(),
                    desired_state: "merge after certify".into(),
                    acceptance_criteria: vec!["exact-head clean".into()],
                    depends_on: vec![],
                    position: None,
                }],
            )
            .expect("replan");
        let tracks = store.tracks("verity-core").expect("re-read");
        assert_eq!(tracks[0].title.as_deref(), Some("Land #2332 (retry)"));
        assert_eq!(tracks[0].acceptance_criteria, vec!["exact-head clean"]);
        assert!(tracks[0].depends_on.is_empty());
    }

    #[test]
    fn explicit_positions_define_the_server_roadmap_order() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_planned_tracks(
                "lido",
                &[
                    PlannedTrack {
                        track: "ux2".into(),
                        title: "UX2".into(),
                        desired_state: "accepted".into(),
                        acceptance_criteria: vec![],
                        depends_on: vec!["ux1".into()],
                        position: Some(1),
                    },
                    PlannedTrack {
                        track: "ux1".into(),
                        title: "UX1".into(),
                        desired_state: "merged".into(),
                        acceptance_criteria: vec![],
                        depends_on: vec![],
                        position: Some(0),
                    },
                ],
            )
            .expect("plan ordered roadmap");
        assert_eq!(
            store
                .tracks("lido")
                .expect("tracks")
                .iter()
                .map(|track| track.track.as_str())
                .collect::<Vec<_>>(),
            vec!["ux1", "ux2"]
        );
    }

    #[test]
    fn find_open_proposal_slug_uses_the_stored_alias() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_proposals(
                "verity",
                &[NewProposal {
                    task_key: "docs".into(),
                    title: "Write the guide".into(),
                    prompt: None,
                    acceptance_criteria: vec!["published".into()],
                    depends_on: vec![],
                }],
            )
            .expect("seed alias proposal");
        let keys = vec!["verity-core".into(), "verity".into()];
        assert_eq!(
            store
                .find_open_proposal_slug(&keys, "docs")
                .expect("lookup")
                .as_deref(),
            Some("verity")
        );
        assert!(store
            .update_proposal(
                "verity",
                "docs",
                Some("Write the core guide"),
                None,
                None,
                None
            )
            .expect("update under alias"));
        let open = store.list_open_proposals("verity").expect("list");
        assert_eq!(open[0].title, "Write the core guide");
        assert!(!store
            .update_proposal("verity-core", "docs", Some("miss"), None, None, None)
            .expect("canonical miss"));
    }

    fn legacy_tracks_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("conn");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("fk");
        connection
            .execute_batch(
                "CREATE TABLE projects (slug TEXT PRIMARY KEY, title TEXT, objective TEXT, \
                    status TEXT NOT NULL DEFAULT 'active', mode TEXT, wait_ticks INTEGER NOT NULL DEFAULT 0, \
                    next_action TEXT, blocker TEXT, controller_cron_id TEXT, repository TEXT, \
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO projects (slug, created_at, updated_at) VALUES ('lido', 'x', 'x');
                 CREATE TABLE project_tracks (
                    slug TEXT NOT NULL, track TEXT NOT NULL,
                    desired_state TEXT, status TEXT, updated_at TEXT NOT NULL,
                    PRIMARY KEY (slug, track));
                 INSERT INTO project_tracks VALUES
                    ('orphan-alias', 'x1', 'done elsewhere', 'done', '2026-08-01T00:00:00Z'),
                    ('lido', 'P-ETH-1', 'proved', 'in-progress', '2026-08-01T00:00:00Z'),
                    ('lido', 'S3', 'token bound', 'done', '2026-08-02T00:00:00Z'),
                    ('lido', 'wave-1', NULL, 'cancelled', '2026-08-03T00:00:00Z'),
                    ('lido', 'closed-one', 'x', 'closed', '2026-08-04T00:00:00Z'),
                    ('lido', 'UX1', 'new spelling', NULL, '2026-08-06T00:00:00Z'),
                    ('lido', 'ux1', 'old spelling', 'running', '2026-08-05T00:00:00Z');",
            )
            .expect("old schema");
        connection
    }

    #[test]
    fn an_old_tracks_table_is_rebuilt_with_lifecycle_and_claims() {
        let connection = legacy_tracks_connection();
        ProjectsStore::initialize(&connection).expect("migrate");
        let store = ProjectsStore {
            connection: Mutex::new(connection),
        };
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
        // A slug with no roster record (alias / orphan) migrates too: its
        // claim receipt is stored without the FK link instead of aborting.
        let orphan = store
            .track("orphan-alias", "x1")
            .expect("orphan")
            .expect("row");
        assert_eq!(orphan.claim.as_deref(), Some("legacy_import"));
        let tracks = store.tracks("lido").expect("read");
        let by_key = |key: &str| tracks.iter().find(|t| t.track == key).expect(key).clone();

        // in-progress → active/open, key normalized, old spelling aliased.
        let eth = by_key("p-eth-1");
        assert_eq!(eth.lifecycle, "active");
        assert_eq!(eth.status.as_deref(), Some("open"));
        assert_eq!(
            eth.title.as_deref(),
            Some("proved"),
            "title falls back to desired_state"
        );
        assert_eq!(
            store.resolve_track_key("lido", "P-ETH-1").expect("alias"),
            Some("p-eth-1".into())
        );

        // done → active + legacy_import claim; the derived status says done
        // but the situation builder will count it as claim_only.
        let s3 = by_key("s3");
        assert_eq!(s3.lifecycle, "active");
        assert_eq!(s3.claim.as_deref(), Some("legacy_import"));
        assert_eq!(s3.status.as_deref(), Some("done"));
        assert_eq!(by_key("closed-one").claim.as_deref(), Some("legacy_import"));

        assert_eq!(by_key("wave-1").lifecycle, "cancelled");
        assert_eq!(by_key("wave-1").status.as_deref(), Some("cancelled"));

        // UX1 / ux1 collide after normalization: the newest row wins, the
        // other spelling resolves as an alias, and the ambiguity is reported.
        assert_eq!(tracks.iter().filter(|t| t.track == "ux1").count(), 1);
        assert_eq!(by_key("ux1").desired_state.as_deref(), Some("new spelling"));
        let reconcile = store
            .latest_unacked_reconcile("lido")
            .expect("reconcile")
            .expect("one reconcile receipt");
        let ops: Vec<&str> = reconcile.payload["corrections"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["op"].as_str())
            .collect();
        assert!(ops.contains(&"normalized_collision"));
        assert!(ops.contains(&"legacy_done_to_claim_only"));
        assert!(ops.contains(&"status_normalized"));
        assert!(ops.contains(&"key_normalized"));

        // Acknowledging appends; it does not mutate the receipt.
        store
            .ack_reconcile("lido", &reconcile.id, "thomas")
            .expect("ack");
        assert!(store
            .latest_unacked_reconcile("lido")
            .expect("reconcile")
            .is_none());
        assert_eq!(
            store
                .receipt(&reconcile.id)
                .expect("receipt")
                .unwrap()
                .payload,
            reconcile.payload
        );
    }

    #[test]
    fn the_rebuild_is_idempotent_across_restarts() {
        let connection = legacy_tracks_connection();
        ProjectsStore::initialize(&connection).expect("first");
        let before: i64 = connection
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
            .unwrap();
        ProjectsStore::initialize(&connection).expect("second");
        let after: i64 = connection
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            before, after,
            "a second initialize must not re-run the rebuild"
        );
        assert!(!ProjectsStore::needs_tracks_rebuild(&connection).unwrap());
    }

    #[test]
    fn done_cannot_be_written_as_a_status() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .set_track("lido", "a1", Some("x"), Some("open"))
            .expect("open");
        for status in ["done", "closed", "satisfied"] {
            let error = store
                .set_track("lido", "a1", None, Some(status))
                .expect_err(status);
            assert!(
                matches!(error, TrackWriteError::NeedsReceipt(_)),
                "{status}"
            );
        }
        assert!(matches!(
            store.set_track("lido", "a1", None, Some("weird")),
            Err(TrackWriteError::Invalid(_))
        ));
        store
            .set_track("lido", "A1", Some("why"), Some("blocked"))
            .expect("blocked");
        let track = store.track("lido", "a1").expect("read").expect("row");
        assert_eq!(track.status.as_deref(), Some("blocked"));
        assert_eq!(track.explicit_blocker.as_deref(), Some("why"));
        assert_eq!(
            track.revision, 1,
            "rejected writes do not bump the revision"
        );
        store
            .set_track("lido", "a1", None, Some("cancelled"))
            .expect("cancel");
        assert_eq!(
            store.track("lido", "a1").unwrap().unwrap().lifecycle,
            "cancelled"
        );
    }

    #[test]
    fn receipts_are_idempotent_on_key_and_reject_a_changed_body() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        let receipt = NewReceipt {
            idempotency_key: "k1".into(),
            kind: "accept".into(),
            project_slug: Some("lido".into()),
            track_id: None,
            criterion_id: None,
            subject_type: "pr".into(),
            subject_id: "org/repo#1@abc".into(),
            outcome: "observed".into(),
            actor_type: "operator".into(),
            actor_id: "thomas".into(),
            verifier: None,
            supersedes_receipt_id: None,
            observed_at: "2026-09-01T00:00:00Z".into(),
            payload: serde_json::json!({"a": 1}),
        };
        let first = store.insert_receipt(&receipt).expect("first");
        let again = store.insert_receipt(&receipt).expect("retry");
        assert_eq!(first.id, again.id);
        let changed = NewReceipt {
            payload: serde_json::json!({"a": 2}),
            ..receipt
        };
        assert!(matches!(
            store.insert_receipt(&changed),
            Err(ReceiptWriteError::IdempotencyMismatch { .. })
        ));
    }

    #[test]
    fn import_track_is_idempotent_and_keeps_declared_titles() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        assert!(store
            .import_track("lido", "A1", "A1 no P-ALLOC-3", &["A2".into()])
            .expect("import"));
        assert!(!store
            .import_track("lido", "a1", "other title", &[])
            .expect("again"));
        let track = store.track("lido", "A2").expect("alias").expect("row");
        assert_eq!(track.origin, "imported");
        assert_eq!(track.title.as_deref(), Some("A1 no P-ALLOC-3"));
        assert_eq!(store.tracks("lido").unwrap().len(), 1);
    }

    fn evidence(criterion: Option<&str>, subject: &str) -> EvidenceInput {
        EvidenceInput {
            criterion_id: criterion.map(str::to_string),
            kind: "pr_head_review".into(),
            subject_id: subject.into(),
            verifier: Some("codex-review".into()),
            payload: serde_json::json!({"head": "abc"}),
        }
    }

    fn accept(key: &str, evidence: Vec<EvidenceInput>, revision: Option<u64>) -> AcceptRequest {
        AcceptRequest {
            idempotency_key: key.into(),
            expected_revision: revision,
            evidence,
            actor_type: "controller".into(),
            actor_id: "cron-1".into(),
            observed_at: None,
        }
    }

    #[test]
    fn accept_accumulates_per_criterion_then_satisfies_and_replays() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .patch_track(
                "lido",
                "ux1",
                None,
                Some("open"),
                Some("UX1"),
                Some(&["review clean".into(), "deployed".into()]),
                None,
            )
            .expect("declare");
        // Partial acceptance is recorded but does not satisfy: the second
        // criterion still has no standing evidence.
        let partial = accept("k1", vec![evidence(Some("c1"), "o/r#229@abc")], None);
        let first = store
            .accept_track_evidence("lido", "UX1", &partial)
            .expect("partial accept is a receipt");
        assert_eq!(first.receipts.len(), 1);
        assert_eq!(first.track.claim, None);
        assert_eq!(first.track.status.as_deref(), Some("open"));
        assert!(!store.track_contract_satisfied("lido", "ux1").unwrap());
        assert_eq!(first.track.revision, 1);

        let full = accept(
            "k2",
            vec![
                evidence(Some("review clean"), "o/r#229@abc"),
                evidence(Some("c2"), "o/r#229@abc"),
            ],
            Some(1),
        );
        let result = store
            .accept_track_evidence("lido", "ux1", &full)
            .expect("accepted");
        assert!(!result.replayed);
        assert_eq!(result.receipts.len(), 2);
        assert_eq!(result.track.status.as_deref(), Some("satisfied"));
        assert_eq!(result.track.claim.as_deref(), Some("accept"));
        assert_eq!(result.track.revision, 2);
        assert!(store.track_contract_satisfied("lido", "ux1").unwrap());

        let again = store
            .accept_track_evidence("lido", "ux1", &full)
            .expect("retry");
        assert!(again.replayed, "same key + body replays");
        assert_eq!(again.track.revision, 2);
        assert!(matches!(
            store.accept_track_evidence(
                "lido",
                "ux1",
                &accept("k3", full.evidence.clone(), Some(0))
            ),
            Err(AcceptError::StaleRevision {
                expected: 0,
                current: 2
            })
        ));
        let mut changed = full.clone();
        changed.evidence[0].payload = serde_json::json!({"head": "zzz"});
        assert!(matches!(
            store.accept_track_evidence("lido", "ux1", &changed),
            Err(AcceptError::IdempotencyMismatch { .. })
        ));
        assert!(matches!(
            store.accept_track_evidence("lido", "nope", &full),
            Err(AcceptError::NotFound)
        ));
        let bad_kind = accept(
            "k4",
            vec![EvidenceInput {
                kind: "vibes".into(),
                ..evidence(Some("c1"), "x")
            }],
            None,
        );
        assert!(matches!(
            store.accept_track_evidence("lido", "ux1", &bad_kind),
            Err(AcceptError::Invalid(_))
        ));

        // Invalidation reopens without deleting anything. Two accepts cover
        // c1 (k1 and k2); one covers c2.
        let c2_receipt = result
            .receipts
            .iter()
            .find(|r| r.criterion_id.as_deref() == Some("c2"))
            .unwrap()
            .id
            .clone();
        let invalidated = store
            .invalidate_track_evidence("lido", "ux1", &c2_receipt, "head moved", "system", "watch")
            .expect("invalidate");
        assert_eq!(invalidated.outcome, "invalidated");
        assert_eq!(
            invalidated.supersedes_receipt_id.as_deref(),
            Some(c2_receipt.as_str())
        );
        let track = store.track("lido", "ux1").unwrap().unwrap();
        assert_eq!(track.claim, None, "c2 lost its evidence");
        assert_eq!(track.status.as_deref(), Some("open"));
        assert_eq!(track.revision, 3);
        assert_eq!(store.active_accept_receipts("pr").unwrap().len(), 2);
        assert_eq!(store.receipts_for_track("lido", "ux1").unwrap().len(), 4);
    }

    #[test]
    fn per_criterion_evidence_and_reopen_use_the_same_receipts() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .patch_track(
                "lido",
                "s3",
                None,
                Some("open"),
                Some("S3"),
                Some(&["proved".into(), "merged".into()]),
                None,
            )
            .expect("declare");
        let first = store
            .accept_track_criterion_evidence(
                "lido",
                "s3",
                Some("proved"),
                "operator",
                "https://github.com/o/r/pull/1",
                "abc123",
                None,
                "user:prod",
            )
            .expect("first criterion");
        assert_eq!(first.track.claim, None, "one of two criteria");
        let replay = store
            .accept_track_criterion_evidence(
                "lido",
                "s3",
                Some("proved"),
                "operator",
                "https://github.com/o/r/pull/1",
                "abc123",
                None,
                "user:prod",
            )
            .expect("replay");
        assert!(replay.replayed);
        assert!(matches!(
            store.accept_track_criterion_evidence("lido", "s3", None, "operator", "x", "abc123", None, "u"),
            Err(AcceptError::Invalid(ref m)) if m.contains("criterion is required")
        ));
        assert!(matches!(
            store.accept_track_criterion_evidence("lido", "s3", Some("merged"), "vibes", "x", "abc123", None, "u"),
            Err(AcceptError::Invalid(ref m)) if m.contains("verifier_class")
        ));
        let second = store
            .accept_track_criterion_evidence(
                "lido",
                "s3",
                Some("merged"),
                "review",
                "https://github.com/o/r/pull/1#review",
                "abc123",
                None,
                "user:prod",
            )
            .expect("second criterion");
        assert_eq!(second.track.claim.as_deref(), Some("accept"));
        assert_eq!(
            second.track.governed_artifact_version.as_deref(),
            Some("abc123")
        );
        assert!(store.track_contract_satisfied("lido", "s3").unwrap());
        assert!(matches!(
            store.accept_track_criterion_evidence("lido", "s3", Some("merged"), "review", "y", "def456", None, "u"),
            Err(AcceptError::Invalid(ref m)) if m.contains("governed artifact")
        ));
        // Planning never reopens or revises a satisfied track.
        store
            .upsert_planned_tracks(
                "lido",
                &[PlannedTrack {
                    track: "s3".into(),
                    title: "S3 renamed".into(),
                    desired_state: "x".into(),
                    acceptance_criteria: vec!["something else".into()],
                    depends_on: vec![],
                    position: Some(4),
                }],
            )
            .expect("plan");
        let still = store.track("lido", "s3").unwrap().unwrap();
        assert_eq!(still.claim.as_deref(), Some("accept"));
        assert_eq!(still.title.as_deref(), Some("S3"));
        assert_eq!(still.position, 4, "reordering is allowed");
        assert!(matches!(
            store.reopen_track("lido", "s3", "  ", None, "u"),
            Err(AcceptError::Invalid(_))
        ));
        let reopened = store
            .reopen_track("lido", "s3", "head moved", Some("def456"), "user:thomas")
            .expect("reopen");
        assert_eq!(reopened.claim, None);
        assert_eq!(reopened.status.as_deref(), Some("open"));
        assert_eq!(
            reopened.governed_artifact_version.as_deref(),
            Some("def456")
        );
        assert!(!store.track_contract_satisfied("lido", "s3").unwrap());
        assert!(matches!(
            store.reopen_track("lido", "s3", "again", None, "u"),
            Err(AcceptError::Invalid(ref m)) if m.contains("not terminal")
        ));
        store
            .set_track("lido", "s3", None, Some("cancelled"))
            .expect("cancel");
        let back = store
            .reopen_track("lido", "s3", "needed after all", None, "u")
            .expect("uncancel");
        assert_eq!(back.lifecycle, "active");
    }

    #[test]
    fn the_interim_evidence_schema_migrates_to_receipts_and_leases() {
        let connection = Connection::open_in_memory().expect("conn");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("fk");
        connection
            .execute_batch(
                "CREATE TABLE projects (slug TEXT PRIMARY KEY, title TEXT, objective TEXT, \
                    status TEXT NOT NULL DEFAULT 'active', mode TEXT, wait_ticks INTEGER NOT NULL DEFAULT 0, \
                    next_action TEXT, blocker TEXT, controller_cron_id TEXT, repository TEXT, \
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO projects (slug, created_at, updated_at) VALUES ('verity-lido', 'x', 'x');
                 CREATE TABLE project_tracks (
                    slug TEXT NOT NULL, track TEXT NOT NULL, desired_state TEXT, status TEXT,
                    updated_at TEXT NOT NULL, title TEXT, acceptance_criteria TEXT, depends_on TEXT,
                    lifecycle TEXT NOT NULL DEFAULT 'planned', revision INTEGER NOT NULL DEFAULT 1,
                    position INTEGER NOT NULL DEFAULT 0, governed_artifact_version TEXT,
                    accepted_at TEXT, reopened_at TEXT, reopen_reason TEXT, reopened_by TEXT,
                    PRIMARY KEY (slug, track));
                 INSERT INTO project_tracks (slug, track, desired_state, status, updated_at, title, lifecycle, revision, position, governed_artifact_version, accepted_at) VALUES
                    ('verity-lido', 's0', 'removed', 'done', '2026-09-01T00:00:00Z', 'S0 P-DEREF-1 removal', 'satisfied', 1, 3, 'fb4021', '2026-09-01T01:00:00Z'),
                    ('verity-lido', 'ux1', 'accepted', 'running', '2026-09-02T00:00:00Z', 'UX1', 'executing', 2, 1, NULL, NULL),
                    ('verity-lido', 'ux2', NULL, 'open', '2026-09-02T00:00:00Z', 'UX2', 'planned', 1, 2, NULL, NULL),
                    ('verity-lido', 'ghost', NULL, 'done', '2026-09-02T00:00:00Z', 'Ghost', 'satisfied', 1, 5, NULL, NULL),
                    ('verity-lido', 'blk', NULL, 'blocked', '2026-09-02T00:00:00Z', 'Blocked', 'blocked', 1, 6, NULL, NULL),
                    ('verity-lido', 'wave-1', NULL, 'cancelled', '2026-08-01T00:00:00Z', 'Wave 1', 'cancelled', 1, 0, NULL, NULL);
                 CREATE TABLE project_track_evidence (
                    evidence_id TEXT PRIMARY KEY NOT NULL, slug TEXT NOT NULL, track TEXT NOT NULL,
                    track_revision INTEGER NOT NULL, criterion TEXT NOT NULL, verifier_class TEXT NOT NULL,
                    evidence_ref TEXT NOT NULL, artifact_version TEXT, observed_at TEXT NOT NULL,
                    accepted_at TEXT NOT NULL, accepted_by TEXT NOT NULL);
                 INSERT INTO project_track_evidence VALUES
                    ('e1', 'verity-lido', 's0', 1, '__track__', 'operator', 'https://github.com/o/r/pull/215', 'fb4021', '2026-09-01T00:30:00Z', '2026-09-01T01:00:00Z', 'user:prod');
                 CREATE TABLE project_track_dispatches (
                    idempotency_key TEXT PRIMARY KEY NOT NULL, slug TEXT NOT NULL, track TEXT NOT NULL,
                    track_revision INTEGER NOT NULL, owner_lease_id TEXT NOT NULL, lease_expires_at TEXT NOT NULL,
                    state TEXT NOT NULL, mission_id TEXT, receipt TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO project_track_dispatches VALUES
                    ('verity-lido-ux1-v1', 'verity-lido', 'ux1', 2, 'l1', '2099-01-01T00:00:00Z', 'started', '914dcc48-4f27-4ac3-96a4-613544fbf8d7', NULL, 'x', 'x'),
                    ('verity-lido-ux1-v0', 'verity-lido', 'ux1', 1, 'l0', '2099-01-01T00:00:00Z', 'superseded', '8fd9bffd-43e7-4894-b2a9-a7920ba42b1c', NULL, 'x', 'x');",
            )
            .expect("interim schema");
        ProjectsStore::initialize(&connection).expect("migrate");
        let store = ProjectsStore {
            connection: Mutex::new(connection),
        };
        let tracks = store.tracks("verity-lido").expect("tracks");
        let by = |key: &str| tracks.iter().find(|t| t.track == key).expect(key).clone();
        assert_eq!(
            tracks.iter().map(|t| t.track.as_str()).collect::<Vec<_>>(),
            vec!["wave-1", "ux1", "ux2", "s0", "ghost", "blk"]
        );
        let s0 = by("s0");
        assert_eq!(s0.claim.as_deref(), Some("accept"));
        assert_eq!(s0.status.as_deref(), Some("satisfied"));
        assert_eq!(s0.governed_artifact_version.as_deref(), Some("fb4021"));
        assert_eq!(s0.accepted_at.as_deref(), Some("2026-09-01T00:30:00Z"));
        assert_eq!(by("ghost").claim.as_deref(), Some("legacy_import"));
        assert_eq!(by("ghost").status.as_deref(), Some("done"));
        assert_eq!(by("ux1").lifecycle, "active");
        assert_eq!(by("ux1").revision, 2, "interim revision carried");
        assert_eq!(by("blk").status.as_deref(), Some("blocked"));
        assert_eq!(by("wave-1").lifecycle, "cancelled");
        let leases = store.live_leases(Some("verity-lido")).expect("leases");
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].attempt_id, "914dcc48-4f27-4ac3-96a4-613544fbf8d7");
        assert_eq!(leases[0].track, "ux1");
        assert_eq!(leases[0].mode, "writer");
        assert!(store
            .lease_by_key("lease:dispatch:verity-lido-ux1-v1")
            .unwrap()
            .is_some());
        let reconcile = store
            .latest_unacked_reconcile("verity-lido")
            .unwrap()
            .expect("reconcile receipt");
        let ops: Vec<&str> = reconcile.payload["corrections"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["op"].as_str())
            .collect();
        assert!(ops.contains(&"interim_evidence_imported"));
        assert!(ops.contains(&"legacy_done_to_claim_only"));
    }

    fn lease(slug: &str, track: &str, attempt: &str, mode: &str, key: &str) -> LeaseRequest {
        LeaseRequest {
            slug: slug.into(),
            track: track.into(),
            mutation_domain: "track".into(),
            attempt_id: attempt.into(),
            mode: mode.into(),
            idempotency_key: key.into(),
            ttl_secs: 3600,
        }
    }

    #[test]
    fn absorb_resolves_key_alias_and_single_pr_ref_else_creates() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .patch_track("lido", "UX1", None, Some("open"), Some("UX1"), None, None)
            .expect("declare");
        store
            .add_track_ref("lido", "ux1", "pr", None, 229, None)
            .expect("ref");

        let by_key = store.absorb_track("lido", "ux1", None, None).expect("key");
        assert_eq!(
            (by_key.key.as_str(), by_key.created, by_key.matched_by),
            ("ux1", false, "key")
        );
        // Case is normalization, not an alias.
        assert_eq!(
            store
                .absorb_track("lido", "UX1", None, None)
                .unwrap()
                .matched_by,
            "key"
        );
        store
            .add_track_alias("lido", "ux1", "legacy-ux", "imported_code")
            .expect("alias");
        let by_alias = store
            .absorb_track("lido", "legacy-ux", None, None)
            .expect("alias");
        assert_eq!(
            (by_alias.key.as_str(), by_alias.matched_by),
            ("ux1", "alias")
        );
        // A mission tagged `repair-pr-229` on PR 229 attaches to ux1 instead
        // of creating a phantom row.
        let by_pr = store
            .absorb_track("lido", "repair-pr-229", Some("repair"), Some(229))
            .expect("pr");
        assert_eq!(
            (by_pr.key.as_str(), by_pr.created, by_pr.matched_by),
            ("ux1", false, "pr_ref")
        );
        assert_eq!(
            store.resolve_track_key("lido", "repair-pr-229").unwrap(),
            Some("ux1".into())
        );
        // Two tracks on the same PR: ambiguous, so a new key is absorbed.
        store
            .patch_track("lido", "ux2", None, Some("open"), Some("UX2"), None, None)
            .expect("declare 2");
        store
            .add_track_ref("lido", "ux2", "pr", None, 230, None)
            .expect("ref");
        store
            .add_track_ref("lido", "ux1", "pr", None, 230, None)
            .expect("ref");
        let created = store
            .absorb_track("lido", "pr-230-repair", None, Some(230))
            .expect("created");
        assert!(created.created);
        assert_eq!(created.matched_by, "created");
        let track = store.track("lido", "pr-230-repair").unwrap().unwrap();
        assert_eq!(track.origin, "absorbed");
        assert_eq!(track.title.as_deref(), Some("PR 230 Repair"));
        assert_eq!(store.tracks("lido").unwrap().len(), 3);
    }

    #[test]
    fn writer_leases_are_exclusive_readers_coexist_and_release_frees() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .patch_track("lido", "ux1", None, Some("open"), Some("UX1"), None, None)
            .expect("declare");
        let first = store
            .acquire_track_lease(&lease("lido", "ux1", "m1", "writer", "k1"))
            .expect("first writer");
        assert_eq!(first.state, "active");
        let again = store
            .acquire_track_lease(&lease("lido", "ux1", "m1", "writer", "k1"))
            .expect("idempotent");
        assert_eq!(again.id, first.id);
        let conflict = store
            .acquire_track_lease(&lease("lido", "UX1", "m2", "writer", "k2"))
            .expect_err("second writer");
        assert!(
            matches!(conflict, LeaseError::Owned { ref holder_attempt_id, .. } if holder_attempt_id == "m1")
        );
        store
            .acquire_track_lease(&lease("lido", "ux1", "m3", "reader", "k3"))
            .expect("reader coexists");
        assert!(matches!(
            store.acquire_track_lease(&lease("lido", "nope", "m4", "writer", "k4")),
            Err(LeaseError::NotFound)
        ));
        assert_eq!(store.live_leases(Some("lido")).unwrap().len(), 2);
        // The same dispatch key from another attempt (a retried create that
        // made a second mission) is the duplicate, not a replay.
        assert!(matches!(
            store.acquire_track_lease(&lease("lido", "ux1", "m9", "writer", "k1")),
            Err(LeaseError::Owned { ref holder_attempt_id, .. }) if holder_attempt_id == "m1"
        ));
        assert_eq!(store.release_leases_for_attempt("m1").unwrap(), 1);
        store
            .acquire_track_lease(&lease("lido", "ux1", "m2", "writer", "k2b"))
            .expect("writer after release");
        // A released row never satisfies its old key again; the key can be
        // taken by a fresh lease (partial unique index on live rows only).
        store
            .acquire_track_lease(&lease("lido", "ux1", "m2", "reader", "k1"))
            .expect("old key reusable once its lease is released");
        assert!(
            !store.expire_lease(&first.id).unwrap(),
            "released is not live"
        );
        assert!(store.overdue_leases().unwrap().is_empty());
    }

    #[test]
    fn normalize_track_key_is_a_spelling_rule() {
        assert_eq!(normalize_track_key("UX1"), "ux1");
        assert_eq!(normalize_track_key(" P-ETH-1 "), "p-eth-1");
        assert_eq!(normalize_track_key("ux1--pr229__cert"), "ux1-pr229-cert");
        assert_eq!(normalize_track_key("repair pr 233"), "repair-pr-233");
        assert_eq!(normalize_track_key("---"), "");
        assert_ne!(
            normalize_track_key("ux1"),
            normalize_track_key("ux1-pr229-cert")
        );
    }
}
