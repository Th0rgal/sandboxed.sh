//! Projects board backend: one read-only endpoint that joins the three
//! sources of project truth on this host into board-ready rows.
//!
//! Sources (each optional — absence degrades the row, never the endpoint):
//! 1. Hermes project trackers (`HERMES_PROJECTS_DIR`, markdown files with a
//!    `**Status**:` line) — the operator-curated project list.
//! 2. sandboxed.sh missions across every mission store, joined by their
//!    `project` tag (live executions per project).
//! 3. Hermes cron deliveries (`HERMES_STATE_DB`, read-only sqlite) — the same
//!    `[Cron delivery: …]` updates the operator receives in sessions, routed
//!    to projects via their `[STATE_SIGNATURE: <key>|…]` trailer.
//!
//! Routing keys and tracker slugs don't always coincide; an optional
//! `routes.json` alias map in the trackers directory bridges them, and
//! anything still unmatched surfaces in an explicit `unrouted` bucket rather
//! than being dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::control::events::MissionStatus;
use super::mission_store::Mission;
use super::projects_store::ProjectConversation;
use super::routes::AppState;

/// Terminal missions younger than this stay on the board (recent history).
const TERMINAL_MISSION_HORIZON_HOURS: i64 = 48;
/// Hard cap of deliveries scanned per request (newest first).
const DELIVERY_SCAN_LIMIT: usize = 600;
/// A tracker marked active with no live mission and no update for this long
/// is flagged stale-active.
const STALE_ACTIVE_HOURS: i64 = 24;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/overview", get(projects_overview))
        .route("/:slug/updates", get(project_updates))
        .route("/:slug/action", axum::routing::post(project_action))
        .route(
            "/:slug/conversation",
            axum::routing::put(bind_project_conversation).delete(unbind_project_conversation),
        )
}

fn hermes_projects_dir() -> Option<PathBuf> {
    std::env::var("HERMES_PROJECTS_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn hermes_state_db() -> Option<PathBuf> {
    std::env::var("HERMES_STATE_DB")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

#[derive(Debug, Clone, Serialize)]
struct TrackerInfo {
    slug: String,
    status_line: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MissionChip {
    id: String,
    status: MissionStatus,
    title: Option<String>,
    updated_at: String,
    github_pr: Option<String>,
}

/// Owned copy of what the health rollup reads.
///
/// The chip list is truncated to the 8 newest missions for display; the rollup
/// must see *all* of them, or a project's oldest broken track becomes invisible
/// precisely when it has been broken longest.
#[derive(Debug, Clone)]
struct OwnedHealthInput {
    track: Option<String>,
    status: MissionStatus,
    desired_state: Option<String>,
    next_check_at: Option<String>,
    updated_at: String,
}

impl OwnedHealthInput {
    fn as_input(&self) -> super::project_health::MissionHealthInput<'_> {
        super::project_health::MissionHealthInput {
            track: self.track.as_deref(),
            status: self.status,
            desired_state: self.desired_state.as_deref(),
            next_check_at: self.next_check_at.as_deref(),
            updated_at: self.updated_at.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryUpdate {
    headline: String,
    body: Option<String>,
    session_id: String,
    at: String,
    signature: Option<String>,
    blocker: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProjectRow {
    slug: String,
    bucket: &'static str,
    tracker: Option<TrackerInfo>,
    missions: Vec<MissionChip>,
    latest_update: Option<DeliveryUpdate>,
    updates_count: usize,
    attention_reasons: Vec<String>,
    /// Per-track rollup, worst-first. Answers "which track is stuck" without
    /// making the reader scan a list of 800 mission chips.
    health: super::project_health::ProjectHealth,
    /// The conversation to open for this project. An explicit binding wins;
    /// otherwise the newest delivery's session is offered as a GUESS, tagged
    /// as such — cron controllers open a throwaway session per tick, so an
    /// inferred conversation is very often already ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation: Option<ProjectConversation>,
}

pub async fn projects_overview(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let trackers_dir = hermes_projects_dir();
    let state_db = hermes_state_db();

    let trackers = trackers_dir
        .as_deref()
        .map(read_trackers)
        .unwrap_or_default();
    let bindings = state
        .projects
        .bindings()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let archived = trackers_dir
        .as_deref()
        .and_then(|dir| dir.parent().map(|p| p.join("archive")))
        .filter(|dir| dir.is_dir())
        .map(|dir| list_markdown_slugs(&dir))
        .unwrap_or_default();
    let aliases = trackers_dir
        .as_deref()
        .map(read_alias_map)
        .unwrap_or_default();
    let overrides = trackers_dir
        .as_deref()
        .map(read_overrides)
        .unwrap_or_default();

    let missions = state
        .control
        .collect_project_missions(chrono::Duration::hours(TERMINAL_MISSION_HORIZON_HOURS))
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;

    let deliveries = match state_db.as_deref() {
        Some(path) => {
            let path = path.to_path_buf();
            tokio::task::spawn_blocking(move || read_deliveries(&path, DELIVERY_SCAN_LIMIT, None))
                .await
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
                .unwrap_or_else(|error| {
                    tracing::warn!("hermes deliveries unavailable: {error}");
                    Vec::new()
                })
        }
        None => Vec::new(),
    };

    // ── Assemble rows: union of tracker slugs, mission project tags, and
    //    routed delivery keys. Every source can create a row; every source
    //    can only enrich, never hide, another.
    // One instant for the whole response: two tracks in the same payload must
    // not disagree about whether the same deadline has passed.
    let now = chrono::Utc::now().to_rfc3339();
    let mut rows: HashMap<String, ProjectRowBuilder> = HashMap::new();

    let deleted = |slug: &str| overrides.get(slug).map(String::as_str) == Some("deleted");
    for tracker in trackers {
        if deleted(&tracker.slug) {
            continue;
        }
        let slug = tracker.slug.clone();
        rows.entry(slug.clone())
            .or_insert_with(|| ProjectRowBuilder::new(slug))
            .tracker = Some(tracker);
    }
    for mission in &missions {
        let Some(project) = mission.project.project.as_deref() else {
            continue;
        };
        // Some missions carry malformed project tags (raw JSON blobs); only
        // plain slugs may create or join a row.
        if !is_plain_key(project) {
            continue;
        }
        let key = resolve_alias(&aliases, project);
        if deleted(&key) {
            continue;
        }
        let builder = rows
            .entry(key.clone())
            .or_insert_with(|| ProjectRowBuilder::new(key));
        builder.missions.push(mission_chip(mission));
        builder.health_inputs.push(OwnedHealthInput {
            track: mission.project.track.clone(),
            status: mission.status,
            desired_state: mission.project.desired_state.clone(),
            next_check_at: mission.project.next_check_at.clone(),
            updated_at: mission.updated_at.clone(),
        });
    }
    let mut unrouted: Vec<DeliveryUpdate> = Vec::new();
    for delivery in deliveries {
        let Some(signature_key) = delivery.signature.as_deref().map(str::to_string) else {
            unrouted.push(delivery);
            continue;
        };
        let key = resolve_alias(&aliases, &signature_key);
        if deleted(&key) {
            continue;
        }
        rows.entry(key.clone())
            .or_insert_with(|| ProjectRowBuilder::new(key))
            .push_delivery(delivery);
    }

    let mut projects: Vec<ProjectRow> = rows
        .into_values()
        .map(|builder| {
            let forced = overrides.get(&builder.slug).cloned();
            let binding = bindings.get(&builder.slug).cloned();
            builder.finish(&archived, forced.as_deref(), binding, &now)
        })
        .collect();
    projects.sort_by(|a, b| {
        bucket_rank(a.bucket)
            .cmp(&bucket_rank(b.bucket))
            .then_with(|| a.slug.cmp(&b.slug))
    });

    Ok(Json(serde_json::json!({
        "projects": projects,
        "archived": archived,
        "unrouted_updates": unrouted.into_iter().take(20).collect::<Vec<_>>(),
        "sources": {
            "trackers": trackers_dir.is_some(),
            "hermes_db": state_db.is_some(),
        },
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdatesQuery {
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn project_updates(
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<UpdatesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(db) = hermes_state_db() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "HERMES_STATE_DB is not configured".to_string(),
        ));
    };
    let aliases = hermes_projects_dir()
        .as_deref()
        .map(read_alias_map)
        .unwrap_or_default();
    // Accept either the routing key or a tracker slug that aliases route TO.
    let mut keys: Vec<String> = vec![slug.clone()];
    for (from, to) in &aliases {
        if to == &slug {
            keys.push(from.clone());
        }
    }
    let limit = query.limit.unwrap_or(50).min(200);
    let updates =
        tokio::task::spawn_blocking(move || read_deliveries(&db, DELIVERY_SCAN_LIMIT, Some(&keys)))
            .await
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;

    Ok(Json(serde_json::json!({
        "slug": slug,
        "updates": updates.into_iter().take(limit).collect::<Vec<_>>(),
    })))
}

struct ProjectRowBuilder {
    slug: String,
    tracker: Option<TrackerInfo>,
    missions: Vec<MissionChip>,
    /// Health inputs, accumulated alongside the display chips.
    health_inputs: Vec<OwnedHealthInput>,
    latest_update: Option<DeliveryUpdate>,
    recent_signatures: Vec<Option<String>>,
    updates_count: usize,
}

impl ProjectRowBuilder {
    fn new(slug: String) -> Self {
        Self {
            slug,
            tracker: None,
            missions: Vec::new(),
            health_inputs: Vec::new(),
            latest_update: None,
            recent_signatures: Vec::new(),
            updates_count: 0,
        }
    }

    /// Deliveries arrive newest-first; keep the first as latest and remember
    /// the three most recent signatures for the repeated-state stall signal.
    fn push_delivery(&mut self, delivery: DeliveryUpdate) {
        self.updates_count += 1;
        if self.recent_signatures.len() < 3 {
            self.recent_signatures.push(delivery.signature.clone());
        }
        if self.latest_update.is_none() {
            self.latest_update = Some(delivery);
        }
    }

    fn finish(
        mut self,
        archived: &[String],
        forced: Option<&str>,
        binding: Option<ProjectConversation>,
        now: &str,
    ) -> ProjectRow {
        // Rolled up before the chips are truncated, so the verdict covers
        // every mission rather than only the 8 shown.
        let inputs: Vec<_> = self.health_inputs.iter().map(|i| i.as_input()).collect();
        let health = super::project_health::rollup(&inputs, now);

        self.missions
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        self.missions.truncate(8);

        let mut attention: Vec<String> = Vec::new();

        if let Some(latest) = &self.latest_update {
            if let Some(blocker) = latest.blocker.as_deref() {
                attention.push(format!("blocker reported: {blocker}"));
            }
            // Same non-silent state three ticks in a row: the controller
            // keeps reporting an unchanged world — the phantom-lease shape.
            if latest.signature.is_some()
                && self.recent_signatures.len() >= 3
                && self
                    .recent_signatures
                    .iter()
                    .all(|signature| signature == &latest.signature)
            {
                attention.push("same state on 3 consecutive updates".to_string());
            }
        }
        // One aggregated line instead of one per mission: the detail pane
        // already lists every mission chip.
        let problem_missions: Vec<&MissionChip> = self
            .missions
            .iter()
            .filter(|chip| {
                matches!(
                    chip.status,
                    MissionStatus::Failed | MissionStatus::Interrupted
                )
            })
            .collect();
        match problem_missions.len() {
            0 => {}
            1 => {
                let chip = problem_missions[0];
                attention.push(format!(
                    "mission {} is {:?}",
                    &chip.id[..8.min(chip.id.len())],
                    chip.status
                ));
            }
            count => {
                attention.push(format!("{count} missions failed or interrupted"));
            }
        }
        let tracker_active = self
            .tracker
            .as_ref()
            .and_then(|t| t.status_line.as_deref())
            .map(|line| {
                let lower = line.to_lowercase();
                lower.contains("active") || lower.contains("running")
            })
            .unwrap_or(false);
        let tracker_paused = self
            .tracker
            .as_ref()
            .and_then(|t| t.status_line.as_deref())
            .map(|line| line.to_lowercase().contains("paused"))
            .unwrap_or(false);
        let has_live_mission = self
            .missions
            .iter()
            .any(|chip| !chip.status.is_terminal() && chip.status != MissionStatus::Acknowledged);
        if tracker_active && !has_live_mission {
            let last_signal = self
                .latest_update
                .as_ref()
                .map(|u| u.at.clone())
                .or_else(|| self.tracker.as_ref().and_then(|t| t.updated_at.clone()));
            let stale = last_signal
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
                .map(|at| {
                    chrono::Utc::now().signed_duration_since(at.with_timezone(&chrono::Utc))
                        > chrono::Duration::hours(STALE_ACTIVE_HOURS)
                })
                .unwrap_or(false);
            if stale {
                attention.push("active tracker with no live mission or recent update".to_string());
            }
        }

        // A board override silences the automatic rules: pausing a project is
        // an explicit "stop flagging this" from the operator.
        let bucket: &'static str = match forced {
            Some("paused") => "paused",
            Some("archived") => "archived",
            _ => {
                if archived.contains(&self.slug) {
                    "archived"
                } else if !attention.is_empty() {
                    "attention"
                } else if tracker_paused {
                    "paused"
                } else {
                    "active"
                }
            }
        };

        let conversation = binding.or_else(|| {
            self.latest_update
                .as_ref()
                .map(|update| update.session_id.clone())
                .filter(|session_id| !session_id.is_empty())
                .map(|session_id| ProjectConversation {
                    session_id,
                    source: "latest_update",
                    bound_at: None,
                })
        });

        ProjectRow {
            slug: self.slug,
            bucket,
            tracker: self.tracker,
            missions: self.missions,
            latest_update: self.latest_update,
            updates_count: self.updates_count,
            attention_reasons: attention,
            health,
            conversation,
        }
    }
}

fn bucket_rank(bucket: &str) -> u8 {
    match bucket {
        "attention" => 0,
        "active" => 1,
        "paused" => 2,
        _ => 3,
    }
}

fn mission_chip(mission: &Mission) -> MissionChip {
    MissionChip {
        id: mission.id.to_string(),
        status: mission.status,
        title: mission.title.clone(),
        updated_at: mission.updated_at.clone(),
        github_pr: mission.project.github_pr.clone(),
    }
}

fn read_trackers(dir: &Path) -> Vec<TrackerInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut trackers = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let status_line = std::fs::read_to_string(&path).ok().and_then(|content| {
            content
                .lines()
                .find_map(|line| {
                    let trimmed = line.trim();
                    trimmed
                        .strip_prefix("**Status**:")
                        .or_else(|| trimmed.strip_prefix("**Status** :"))
                        .map(|rest| rest.trim().to_string())
                })
                .or_else(|| {
                    // Fallback: yaml-ish `status: …` near the top of the file.
                    content.lines().take(20).find_map(|line| {
                        let trimmed = line.trim();
                        let rest = trimmed
                            .strip_prefix("status:")
                            .or_else(|| trimmed.strip_prefix("Status:"))?;
                        let value = rest.trim();
                        (!value.is_empty()).then(|| value.to_string())
                    })
                })
        });
        let updated_at = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
        trackers.push(TrackerInfo {
            slug,
            status_line,
            updated_at,
        });
    }
    trackers
}

fn list_markdown_slugs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            path.file_stem().and_then(|s| s.to_str()).map(String::from)
        })
        .collect();
    slugs.sort();
    slugs
}

/// Optional `routes.json` in the trackers dir: `{ "verity": "verity-roadmap" }`
/// maps a controller routing key (STATE_SIGNATURE prefix or mission project
/// tag) onto the tracker slug that should own its row.
fn read_alias_map(dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(dir.join("routes.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
        .unwrap_or_default()
}

fn resolve_alias(aliases: &HashMap<String, String>, key: &str) -> String {
    aliases.get(key).cloned().unwrap_or_else(|| key.to_string())
}

/// A routing key / project tag must be a plain slug.
fn is_plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 100
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Board-level state overrides (`board-overrides.json` in the trackers dir):
/// `{ "slug": "paused" | "archived" | "deleted" }`. An overlay owned by the
/// dashboard — tracker files stay untouched because Hermes controllers own
/// them. `deleted` hides the row (and drops its deliveries); the two others
/// force the bucket.
fn overrides_path(dir: &Path) -> PathBuf {
    dir.join("board-overrides.json")
}

fn read_overrides(dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(overrides_path(dir))
        .ok()
        .and_then(|raw| serde_json::from_str::<HashMap<String, String>>(&raw).ok())
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
pub struct ProjectActionRequest {
    action: String,
}

/// Apply a board action to a project: `pause` / `archive` / `delete` set the
/// override, `resume` / `unarchive` / `restore` clear it.
#[derive(Debug, serde::Deserialize)]
pub struct BindConversationRequest {
    pub session_id: String,
}

/// Declare which conversation a project reports into.
///
/// Deliberately explicit: the inferred value (newest delivery's session) is
/// almost always a cron tick's throwaway session, which is already ended and
/// cannot be replied to.
pub async fn bind_project_conversation(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(request): Json<BindConversationRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // `is_plain_key` also rejects path traversal, which the slug-length check
    // in `project_action` does not.
    if !is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let session_id = request.session_id.trim();
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "session_id must be 1-128 chars of [A-Za-z0-9._:-]".to_string(),
        ));
    }
    let conversation = state
        .projects
        .set_binding(&slug, session_id, None)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(serde_json::json!({
        "slug": slug,
        "conversation": conversation,
    })))
}

pub async fn unbind_project_conversation(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let removed = state
        .projects
        .clear_binding(&slug)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(
        serde_json::json!({ "slug": slug, "unbound": removed }),
    ))
}

pub async fn project_action(
    AxumPath(slug): AxumPath<String>,
    Json(request): Json<ProjectActionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(dir) = hermes_projects_dir() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "HERMES_PROJECTS_DIR is not configured".to_string(),
        ));
    };
    if slug.is_empty() || slug.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "invalid slug".to_string()));
    }
    let mut overrides = read_overrides(&dir);
    match request.action.as_str() {
        "pause" => {
            overrides.insert(slug.clone(), "paused".to_string());
        }
        "archive" => {
            overrides.insert(slug.clone(), "archived".to_string());
        }
        "delete" => {
            overrides.insert(slug.clone(), "deleted".to_string());
        }
        "resume" | "unarchive" | "restore" => {
            overrides.remove(&slug);
        }
        other => {
            return Err((StatusCode::BAD_REQUEST, format!("unknown action '{other}'")));
        }
    }
    let serialized = serde_json::to_string_pretty(&overrides)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let path = overrides_path(&dir);
    let tmp = dir.join(".board-overrides.json.tmp");
    std::fs::write(&tmp, serialized)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("write {}: {error}", path.display()),
            )
        })?;
    Ok(Json(serde_json::json!({
        "slug": slug,
        "override": overrides.get(&slug),
    })))
}

/// Read `[Cron delivery: …]` updates from the Hermes SessionDB, newest first.
/// `filter_keys`, when set, keeps only deliveries whose signature key matches.
pub fn read_deliveries(
    db_path: &Path,
    scan_limit: usize,
    filter_keys: Option<&[String]>,
) -> Result<Vec<DeliveryUpdate>, String> {
    let connection =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("open {} read-only: {error}", db_path.display()))?;
    // Two shapes coexist in the Hermes DB: the canonical cron-session report
    // (assistant message ending in a real `[STATE_SIGNATURE: …]` trailer) and
    // its `[Cron delivery: …]` copy in target sessions, which has the
    // signature stripped. We read both and drop copies whose body duplicates
    // a signature-bearing report.
    let mut statement = connection
        .prepare(
            "SELECT session_id, timestamp, content FROM messages \
             WHERE role = 'assistant' AND (content LIKE '[Cron delivery:%' \
                OR content LIKE '%[STATE_SIGNATURE:%') \
             ORDER BY timestamp DESC LIMIT ?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([scan_limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut deliveries = Vec::new();
    let mut routed_fingerprints: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for row in rows {
        let (session_id, timestamp, content) = row.map_err(|error| error.to_string())?;
        let parsed = parse_delivery(&session_id, timestamp, &content);
        if parsed.signature.is_some() {
            routed_fingerprints.insert(delivery_fingerprint(&content));
        }
        if let Some(keys) = filter_keys {
            let matches = parsed
                .signature
                .as_deref()
                .is_some_and(|signature| keys.iter().any(|key| key == signature));
            if !matches {
                continue;
            }
        }
        deliveries.push(parsed);
    }
    // Second pass: signature-less delivery copies of a routed report are
    // duplicates, not unrouted updates.
    deliveries.retain(|delivery| {
        delivery.signature.is_some()
            || delivery
                .body
                .as_deref()
                .map(|body| !routed_fingerprints.contains(&delivery_fingerprint(body)))
                .unwrap_or(true)
    });
    Ok(deliveries)
}

/// Whitespace-normalized prefix of the report body (tag and signature lines
/// stripped) — enough to identify a delivery copy of a routed report.
fn delivery_fingerprint(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("[Cron delivery:")
                && !trimmed.starts_with("[STATE_SIGNATURE:")
        })
        .flat_map(|line| line.split_whitespace())
        .flat_map(|word| word.chars())
        .take(160)
        .collect()
}

fn parse_delivery(session_id: &str, timestamp: f64, content: &str) -> DeliveryUpdate {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp(
        timestamp as i64,
        ((timestamp.fract()) * 1e9) as u32,
    )
    .map(|t| t.to_rfc3339())
    .unwrap_or_default();

    // Headline: the first non-empty line after the `[Cron delivery: …]` tag,
    // falling back to the tag's own title (the cron job name).
    let tag_title = content.lines().next().and_then(|line| {
        line.trim()
            .strip_prefix("[Cron delivery:")
            .map(|rest| rest.trim_end_matches(']').trim().to_string())
    });
    let mut headline = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("[Cron delivery:")
            || trimmed.starts_with("[STATE_SIGNATURE:")
        {
            continue;
        }
        headline = trimmed.trim_start_matches('#').trim().to_string();
        break;
    }
    if headline.is_empty() {
        headline = tag_title.unwrap_or_default();
    }

    let signature = content
        .lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("[STATE_SIGNATURE:")
                .and_then(|rest| rest.strip_suffix(']'))
        })
        .and_then(|inner| inner.split('|').next())
        .map(|key| key.trim().to_string())
        // Reject template placeholders like `<project>` and anything that
        // isn't a plain routing key.
        .filter(|key| is_plain_key(key));

    // "Bloqué par:" / "Blocked by:" field, when it names a real blocker.
    let blocker = content.lines().find_map(|line| {
        let lower = line.to_lowercase();
        let (idx, marker_len) = ["bloqué par", "blocked by"]
            .iter()
            .find_map(|marker| lower.find(marker).map(|idx| (idx, marker.len())))?;
        let rest = &line[idx + marker_len..];
        let value = rest
            .trim_start_matches(['*', ':', ' ', '\u{a0}'])
            .trim_end_matches("**")
            .trim();
        let normalized = value.to_lowercase();
        // Controllers routinely write "Blocked by: nothing currently; …" —
        // any negation opener means "not blocked", however long the tail.
        const EMPTY_OPENERS: [&str; 7] = [
            "aucun",
            "rien",
            "none",
            "nothing",
            "n/a",
            "no blocker",
            "not blocked",
        ];
        if value.is_empty()
            || EMPTY_OPENERS
                .iter()
                .any(|opener| normalized.starts_with(opener))
            || normalized.starts_with('—')
            || normalized.starts_with('-')
        {
            None
        } else {
            Some(value.chars().take(200).collect::<String>())
        }
    });

    DeliveryUpdate {
        headline,
        body: Some(content.chars().take(8000).collect()),
        session_id: session_id.to_string(),
        at,
        signature,
        blocker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[Cron delivery: Verity two-phase Fable/Codex progression]\n\
        Verity — BLOQUÉE PAR LE CONTROL PLANE\n\n\
        **Changé :** l'audit est terminé.\n\
        **Bloqué par :** lease writer fantôme sur #2219.\n\n\
        [STATE_SIGNATURE: verity|phase-1b|abc123|blocked|lease|next]";

    #[test]
    fn parses_delivery_headline_signature_and_blocker() {
        let parsed = parse_delivery("sess-1", 1_754_000_000.5, SAMPLE);
        assert_eq!(parsed.headline, "Verity — BLOQUÉE PAR LE CONTROL PLANE");
        assert_eq!(parsed.signature.as_deref(), Some("verity"));
        assert!(parsed
            .blocker
            .as_deref()
            .is_some_and(|b| b.contains("lease writer fantôme")));
        assert!(!parsed.at.is_empty());
    }

    #[test]
    fn empty_blockers_are_not_blockers() {
        for line in [
            "**Bloqué par :** aucun pour l'instant.",
            "**Blocked by:** nothing currently; the four v33 lanes are healthy.",
            "**Blocked by:** none — waiting on CI.",
            "**Blocked by:** n/a",
            "**Blocked by:** not blocked, just slow.",
        ] {
            let content = format!("[Cron delivery: x]\nTitre\n{line}\n");
            let parsed = parse_delivery("s", 0.0, &content);
            assert!(parsed.blocker.is_none(), "should be empty: {line}");
        }
    }

    #[test]
    fn repeated_signature_and_blocker_raise_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.push_delivery(parse_delivery("s", 3.0, SAMPLE));
        builder.push_delivery(parse_delivery("s", 2.0, SAMPLE));
        builder.push_delivery(parse_delivery("s", 1.0, SAMPLE));
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert!(row.attention_reasons.iter().any(|r| r.contains("blocker")));
        assert!(row
            .attention_reasons
            .iter()
            .any(|r| r.contains("same state")));
    }

    #[test]
    fn template_placeholder_signatures_are_rejected() {
        let content = "Rapport\n[STATE_SIGNATURE: <project>|<item>|x]";
        let parsed = parse_delivery("s", 0.0, content);
        assert!(parsed.signature.is_none());
    }

    #[test]
    fn delivery_copy_shares_fingerprint_with_routed_report() {
        let routed = "Verity — état stable.\n\nDétails du tick.\n\n[STATE_SIGNATURE: verity|a|b]";
        let copy = "[Cron delivery: Verity two-phase]\nVerity — état stable.\n\nDétails du tick.";
        assert_eq!(delivery_fingerprint(routed), delivery_fingerprint(copy));
    }

    #[test]
    fn headline_falls_back_to_cron_tag_title() {
        let parsed = parse_delivery("s", 0.0, "[Cron delivery: Lido campaign controller]\n\n");
        assert_eq!(parsed.headline, "Lido campaign controller");
    }

    #[test]
    fn multiple_problem_missions_aggregate_into_one_reason() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        for i in 0..3 {
            builder.missions.push(MissionChip {
                id: format!("0000000{i}-aaaa-bbbb-cccc-dddddddddddd"),
                status: MissionStatus::Failed,
                title: None,
                updated_at: "2026-08-01T00:00:00Z".to_string(),
                github_pr: None,
            });
        }
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        let failed_lines: Vec<&String> = row
            .attention_reasons
            .iter()
            .filter(|r| r.contains("ailed"))
            .collect();
        assert_eq!(failed_lines.len(), 1);
        assert!(failed_lines[0].contains("3 missions failed or interrupted"));
    }

    #[test]
    fn forced_override_silences_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.push_delivery(parse_delivery("s", 3.0, SAMPLE));
        builder.push_delivery(parse_delivery("s", 2.0, SAMPLE));
        builder.push_delivery(parse_delivery("s", 1.0, SAMPLE));
        let row = builder.finish(&[], Some("paused"), None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "paused");
        // Reasons stay visible in the detail pane even when silenced.
        assert!(!row.attention_reasons.is_empty());
    }

    #[test]
    fn paused_tracker_without_signals_lands_in_paused_bucket() {
        let mut builder = ProjectRowBuilder::new("erc".to_string());
        builder.tracker = Some(TrackerInfo {
            slug: "erc".to_string(),
            status_line: Some("paused (drained)".to_string()),
            updated_at: None,
        });
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "paused");
    }

    /// An explicit binding must win over the inferred session, and the
    /// inferred one must be labelled as a guess so the UI can offer to bind it.
    #[test]
    fn explicit_binding_wins_over_the_inferred_session() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.push_delivery(DeliveryUpdate {
            headline: "tick".into(),
            body: None,
            at: "2026-08-04T12:00:00Z".into(),
            session_id: "cron_e594d751447d_20260804_120931".into(),
            signature: None,
            blocker: None,
        });
        let row = builder.finish(
            &[],
            None,
            Some(ProjectConversation {
                session_id: "20260804_103847_86ca5c".into(),
                source: "binding",
                bound_at: Some("2026-08-04T13:00:00Z".into()),
            }),
            "2026-08-04T12:00:00Z",
        );
        let conversation = row.conversation.expect("conversation");
        assert_eq!(conversation.session_id, "20260804_103847_86ca5c");
        assert_eq!(conversation.source, "binding");
    }

    #[test]
    fn without_a_binding_the_latest_update_is_offered_as_a_guess() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.push_delivery(DeliveryUpdate {
            headline: "tick".into(),
            body: None,
            at: "2026-08-04T12:00:00Z".into(),
            session_id: "cron_e594d751447d_20260804_120931".into(),
            signature: None,
            blocker: None,
        });
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        let conversation = row.conversation.expect("conversation");
        assert_eq!(conversation.source, "latest_update");
        assert_eq!(conversation.bound_at, None);
    }
}
