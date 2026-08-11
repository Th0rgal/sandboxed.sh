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
#[allow(unused_imports)]
use super::projects_store::{ProjectDecision, ProjectGrant, ProjectRecord, ProjectTrack};
use super::routes::AppState;

/// Terminal missions younger than this stay on the board (recent history).
const TERMINAL_MISSION_HORIZON_HOURS: i64 = 48;
/// Hard cap of deliveries scanned per request (newest first).
const DELIVERY_SCAN_LIMIT: usize = 600;
/// A tracker marked active with no live mission and no update for this long
/// is flagged stale-active.
const STALE_ACTIVE_HOURS: i64 = 24;

/// How recent the controller's latest state event must be for the row to count
/// as "the controller is on it". Within this window an `active` record with no
/// blocker suppresses mission-derived attention (failed/interrupted chips): the
/// controller has seen those missions and keeps reporting active — flagging the
/// project anyway is what put 48h-old failures on the attention shelf while the
/// delivery said "Action: aucune". Default 2700s (45min) ≈ 2–3× a typical
/// controller cadence; tune with `ATTENTION_FRESH_SIGNAL_SECS`.
const ATTENTION_FRESH_SIGNAL_SECS_DEFAULT: i64 = 2700;

fn attention_fresh_signal_secs() -> i64 {
    std::env::var("ATTENTION_FRESH_SIGNAL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(ATTENTION_FRESH_SIGNAL_SECS_DEFAULT)
}

/// How often the state ingestor folds new deliveries into the timeline.
///
/// The window it re-reads overlaps generously; `record_state` is idempotent on
/// a delivery's timestamp precisely so that overlap is free.
const STATE_INGEST_INTERVAL_SECS: u64 = 60;

/// Fold controller state signatures into the durable project timeline.
///
/// A background task rather than work done on read: ingesting inside the
/// overview handler would be an unbounded write on a GET, and the history has
/// to accumulate whether or not anyone has the board open — that is the whole
/// point of asking "what has this project been doing for three days".
pub fn spawn_state_ingestor(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(STATE_INGEST_INTERVAL_SECS)).await;
            let Some(path) = hermes_state_db() else {
                continue;
            };
            let deliveries = match tokio::task::spawn_blocking(move || {
                read_deliveries(&path, DELIVERY_SCAN_LIMIT, None)
            })
            .await
            {
                Ok(Ok(deliveries)) => deliveries,
                Ok(Err(error)) => {
                    tracing::warn!("state ingest: hermes deliveries unavailable: {error}");
                    continue;
                }
                Err(error) => {
                    tracing::warn!("state ingest: join failed: {error}");
                    continue;
                }
            };
            // The routing key a controller emits (`lido`) and the roster slug
            // (`lido-audit`) don't always coincide; resolve through the same
            // alias map the overview uses so state and mode land on one row.
            // Overrides gate routing: an alias must not deliver into an
            // archived target.
            let (aliases, overrides) = hermes_projects_dir()
                .map(|dir| (read_alias_map(&dir), read_overrides(&dir)))
                .unwrap_or_default();
            ingest_deliveries(&state.projects, &aliases, &overrides, deliveries);
        }
    });
}

/// Marker prefix for state descriptors the ingestor synthesizes for CTRL-only
/// deliveries (no STATE_SIGNATURE tail). Recording them is what makes the
/// headline/session of a CTRL-only controller land on its board row; the
/// prefix lets readers keep rendering `state` as absent for those.
const CTRL_DESCRIPTOR_PREFIX: &str = "ctrl:";

/// Fold one batch of deliveries into the projects store.
///
/// This is THE delivery router: alias resolution, roster auto-upsert, state
/// and mode recording, and unrouted triage all happen here, once, in the
/// background — the overview handler only reads the store back.
fn ingest_deliveries(
    projects: &super::projects_store::ProjectsStore,
    aliases: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
    deliveries: Vec<DeliveryUpdate>,
) {
    // read_deliveries returns newest-first; replay oldest-first so a
    // run of the same state lands as one extended row rather than
    // being rejected as out-of-order.
    for delivery in deliveries.into_iter().rev() {
        // The routing key is what identifies the project. It comes from
        // the STATE_SIGNATURE trailer, or the CTRL trailer as a fallback
        // (#763) — so a controller that emits only `[CTRL: …]` (no
        // STATE_SIGNATURE, as Lido does) still routes. Without a key the
        // delivery goes to the unrouted triage inbox instead of vanishing.
        let Some(raw_key) = delivery.signature.as_deref() else {
            record_unrouted(projects, &delivery);
            continue;
        };
        let canonical = resolve_alias(aliases, raw_key);
        // An alias pointing at an archived (or board-deleted) project is a
        // stale route: delivering through it would silently feed a row nobody
        // watches. Refuse it — the delivery surfaces as unrouted on the board,
        // which is what prompts the operator to fix routes.json. The file is
        // never rewritten here.
        if aliases.contains_key(raw_key) && slug_is_archived(projects, overrides, &canonical) {
            record_unrouted(projects, &delivery);
            continue;
        }
        let slug = canonical.as_str();
        // Auto-upsert the roster from routed deliveries: a slug that
        // reports is a real project. This is how projects.db becomes the
        // complete authoritative list without a manual seed of every
        // project — in the background ingestor, not on a GET. Cheap when
        // the row already exists (COALESCE upsert).
        if let Err(error) = projects.upsert_project(slug, None, None, None, None) {
            tracing::warn!("state ingest upsert: {slug}: {error}");
        }
        // The descriptor (STATE_SIGNATURE tail) records the state timeline; it
        // may be absent when a controller emits only a CTRL trailer. A
        // synthetic `ctrl:…` descriptor is recorded then, so the delivery's
        // headline and session still reach the timeline — that is what the
        // overview builds `latest_update` from. `observations` drives `wait`.
        let headline = Some(delivery.headline.trim()).filter(|h| !h.is_empty());
        let session = Some(delivery.session_id.as_str()).filter(|s| !s.is_empty());
        let descriptor = delivery.state.clone().unwrap_or_else(|| {
            format!(
                "{CTRL_DESCRIPTOR_PREFIX}{}",
                delivery.mode.as_deref().unwrap_or("report")
            )
        });
        // `[SILENT]` is the controllers-policy convention for "nothing to
        // report". It must advance freshness (a quiet tick is proof of life),
        // but it must never become the headline the card shows — fold it onto
        // the previous state event so `latest_update` keeps surfacing the last
        // meaningful headline with the fresh timestamp.
        let recorded = match headline {
            Some("[SILENT]") | None => {
                projects.record_silent_observation(slug, &descriptor, &delivery.at, session)
            }
            Some(_) => projects.record_state(slug, &descriptor, headline, &delivery.at, session),
        };
        let observations = match recorded {
            Ok(observations) => observations,
            Err(error) => {
                tracing::warn!("state ingest: {slug}: {error}");
                1
            }
        };
        // Project the controller's `[CTRL: … mode=… ]` mode onto the
        // project record — independent of the descriptor, so a
        // CTRL-only delivery still updates the mode column. Idempotent
        // on replay: `wait` comes from the observation count, not a
        // per-call increment.
        if let Some(mode) = delivery.mode.as_deref() {
            let base = mode.split_once(':').map_or(mode, |(base, _)| base);
            let blocker = mode.split_once(':').map(|(_, cause)| cause);
            if let Err(error) = projects.project_mode_from_signal(
                slug,
                base,
                observations.saturating_sub(1) as i64,
                None,
                blocker,
            ) {
                tracing::warn!("state ingest mode: {slug}: {error}");
            }
        }
    }
}

/// Whether routing into `slug` should be refused: board override says
/// archived/deleted, or the roster record itself is archived.
fn slug_is_archived(
    projects: &super::projects_store::ProjectsStore,
    overrides: &HashMap<String, String>,
    slug: &str,
) -> bool {
    if matches!(
        overrides.get(slug).map(String::as_str),
        Some("archived") | Some("deleted")
    ) {
        return true;
    }
    projects
        .get_project(slug)
        .ok()
        .flatten()
        .is_some_and(|record| record.status == "archived")
}

fn record_unrouted(projects: &super::projects_store::ProjectsStore, delivery: &DeliveryUpdate) {
    if let Err(error) = projects.record_unrouted(
        &delivery.session_id,
        &delivery.at,
        &delivery.headline,
        delivery.signature.as_deref(),
        delivery.mode.as_deref(),
        delivery.blocker.as_deref(),
    ) {
        tracing::warn!("state ingest unrouted: {error}");
    }
}

#[derive(Debug, Deserialize)]
pub struct StateQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// A project's state history, newest first.
pub async fn project_state(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<StateQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let limit = query.limit.unwrap_or(50).min(200);
    let states = state
        .projects
        .state_timeline(&slug, limit)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(serde_json::json!({ "slug": slug, "states": states })))
}

fn bad_slug() -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, "invalid project slug".to_string())
}

fn store_err(error: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

/// `GET /api/projects/:slug` — the structured project object: record, grant,
/// tracks, and open decisions. This is what `get_project` (MCP) returns to a
/// controller instead of it scanning markdown.
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    let project = state
        .projects
        .get_project(&slug)
        .map_err(store_err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown project '{slug}'")))?;
    let grant = state.projects.get_grant(&slug).map_err(store_err)?;
    let tracks = state.projects.tracks(&slug).map_err(store_err)?;
    let decisions = state.projects.open_decisions(&slug).map_err(store_err)?;
    let conversation = state.projects.binding(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({
        "project": project,
        "grant": grant,
        "tracks": tracks,
        "open_decisions": decisions,
        "conversation": conversation,
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpsertProjectRequest {
    pub slug: String,
    pub title: Option<String>,
    pub objective: Option<String>,
    pub repository: Option<String>,
    pub controller_cron_id: Option<String>,
}

/// `PUT /api/projects` — create or enrich a project record. Used by the seed
/// and whenever a controller declares its project. Never clears a field: pass
/// only what you mean to set.
pub async fn upsert_project(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpsertProjectRequest>,
) -> Result<Json<ProjectRecord>, (StatusCode, String)> {
    if !is_plain_key(&req.slug) {
        return Err(bad_slug());
    }
    let record = state
        .projects
        .upsert_project(
            &req.slug,
            req.title.as_deref(),
            req.objective.as_deref(),
            req.repository.as_deref(),
            req.controller_cron_id.as_deref(),
        )
        .map_err(store_err)?;
    Ok(Json(record))
}

#[derive(Debug, Deserialize)]
pub struct SetStatusRequest {
    pub mode: String,
    pub next_action: Option<String>,
    pub blocker: Option<String>,
}

/// `POST /api/projects/:slug/status` — the controller's per-tick state report.
/// Replaces the parsed `[CTRL:]` trailer with a structured write; `wait_ticks`
/// is maintained by the store.
pub async fn set_project_status(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<SetStatusRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    let mode = req.mode.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "active" | "blocked" | "paused") {
        return Err((
            StatusCode::BAD_REQUEST,
            "mode must be active, blocked, or paused".to_string(),
        ));
    }
    state
        .projects
        .set_mode(
            &slug,
            &mode,
            req.next_action.as_deref(),
            req.blocker.as_deref(),
        )
        .map_err(|error| (StatusCode::NOT_FOUND, error))?;
    let project = state.projects.get_project(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "project": project })))
}

#[derive(Debug, Deserialize)]
pub struct SetTrackRequest {
    pub track: String,
    pub desired_state: Option<String>,
    pub status: Option<String>,
}

/// `POST /api/projects/:slug/track` — declare/update one workstream.
pub async fn set_project_track(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<SetTrackRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    if req.track.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "track is required".to_string()));
    }
    state
        .projects
        .set_track(
            &slug,
            req.track.trim(),
            req.desired_state.as_deref(),
            req.status.as_deref(),
        )
        .map_err(store_err)?;
    let tracks = state.projects.tracks(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "tracks": tracks })))
}

/// `GET /api/projects/:slug/grant` — the autonomy grant.
pub async fn get_project_grant(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    let grant = state.projects.get_grant(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "slug": slug, "grant": grant })))
}

#[derive(Debug, Deserialize)]
pub struct SetGrantRequest {
    pub merge_authority: Option<String>,
    pub budget_per_tick: Option<String>,
    pub parallel_missions: Option<i64>,
    pub pause_reason: Option<String>,
    pub resume_condition: Option<String>,
    pub material_bar: Option<String>,
}

/// `POST /api/projects/:slug/grant` — set the autonomy grant. The project must
/// exist (the grant FK-references it).
pub async fn set_project_grant(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<SetGrantRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    if state
        .projects
        .get_project(&slug)
        .map_err(store_err)?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, format!("unknown project '{slug}'")));
    }
    state
        .projects
        .set_grant(
            &slug,
            req.merge_authority.as_deref(),
            req.budget_per_tick.as_deref(),
            req.parallel_missions,
            req.pause_reason.as_deref(),
            req.resume_condition.as_deref(),
            req.material_bar.as_deref(),
        )
        .map_err(store_err)?;
    let grant = state.projects.get_grant(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "slug": slug, "grant": grant })))
}

#[derive(Debug, Deserialize)]
pub struct RecordDecisionRequest {
    pub question: String,
    pub rationale: Option<String>,
}

/// `POST /api/projects/:slug/decision` — add to the pending-decision ledger.
pub async fn record_project_decision(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<RecordDecisionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    if req.question.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "question is required".to_string()));
    }
    state
        .projects
        .record_decision(&slug, req.question.trim(), req.rationale.as_deref())
        .map_err(store_err)?;
    let open = state.projects.open_decisions(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "open_decisions": open })))
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/overview", get(projects_overview))
        .route("/", axum::routing::put(upsert_project))
        .route("/:slug", get(get_project))
        .route("/:slug/state", get(project_state))
        .route("/:slug/updates", get(project_updates))
        .route("/:slug/action", axum::routing::post(project_action))
        .route("/:slug/status", axum::routing::post(set_project_status))
        .route("/:slug/track", axum::routing::post(set_project_track))
        .route(
            "/:slug/grant",
            get(get_project_grant).post(set_project_grant),
        )
        .route(
            "/:slug/decision",
            axum::routing::post(record_project_decision),
        )
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

/// Path to Hermes' `state.db`, for callers that need the continuation chain.
pub fn hermes_state_db_path() -> Option<PathBuf> {
    hermes_state_db()
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
    /// Routing key: the FIRST field of the STATE_SIGNATURE trailer. Says which
    /// project the delivery belongs to, not what state it is in.
    signature: Option<String>,
    /// The rest of the trailer — the fields that actually describe the state
    /// (`phase1-stack|7dba916|clean-ready|ci-failures-3-prs|…`). Two deliveries
    /// with the same value reported the same world.
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    /// Controller-reported mode from the `[CTRL: … | mode=… | …]` trailer:
    /// `active`, `blocked[:cause]` or `paused[:reason]`. Absent for controllers
    /// that have not adopted the trailer — the three regimes were previously
    /// indistinguishable, so a quiet tick and a stuck one looked identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    blocker: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProjectRow {
    slug: String,
    /// The roster project's title, when one was set — surfaces render it
    /// instead of the raw slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// The controller's declared next step, from the roster record.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<String>,
    bucket: &'static str,
    /// The operator's board override for this slug (`"paused"` / `"archived"`),
    /// when one is set. This is the provenance bit clients need to distinguish
    /// an operator pause (override present) from a controller that stopped
    /// itself (`mode` paused/blocked without an override) — overrides are only
    /// ever written by the board action endpoint, never by controllers.
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    board_override: Option<String>,
    /// The roster record's controller cron id, when declared — the
    /// controller ↔ project link.
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_cron_id: Option<String>,
    /// The roster record's controller-reported mode (`active` / `blocked` /
    /// `paused`), surfaced directly on the row. Also still rides on
    /// `latest_update.mode` for compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
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
    let mut bindings = state
        .projects
        .bindings()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    // A binding names the conversation the operator declared. That id goes
    // stale as soon as Hermes compresses the conversation and forks a
    // continuation — measured on the Lido audit, four times in one night. The
    // declared value stays the stored fact; the live tip is resolved here, the
    // same way Hermes resolves its own routes.
    if let Some(path) = hermes_state_db() {
        for conversation in bindings.values_mut() {
            let tip = super::session_chain::live_tip(&path, &conversation.session_id);
            if tip != conversation.session_id {
                tracing::debug!(
                    declared = %conversation.session_id,
                    live = %tip,
                    "binding followed a conversation continuation"
                );
                conversation.session_id = tip;
            }
        }
    }
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

    // Delivery-derived facts come from the projects store, which the
    // background ingestor keeps current — the overview never scans the Hermes
    // state DB per request (that scan was a 600-message LIKE over a
    // multi-gigabyte SQLite file, on every board render).
    let latest_states = state
        .projects
        .latest_states()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let update_totals = state
        .projects
        .state_event_totals()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let unrouted_rows = state
        .projects
        .unrouted(20)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;

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
    // Roster projects are rows in their own right: a project created and bound
    // through the API (no tracker file, no tagged mission, no delivery yet)
    // must still appear on every surface, or "create + bind" looks like a
    // silent no-op from the board.
    // Roster mode/blocker enrich `latest_update` below; capture them before
    // the record's fields are moved onto the builder.
    let mut record_signals: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for record in state
        .projects
        .list_projects()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?
    {
        if deleted(&record.slug) {
            continue;
        }
        record_signals.insert(
            record.slug.clone(),
            (record.mode.clone(), record.blocker.clone()),
        );
        let slug = record.slug.clone();
        let builder = rows
            .entry(slug.clone())
            .or_insert_with(|| ProjectRowBuilder::new(slug));
        builder.title = record.title;
        builder.next_action = record.next_action;
        builder.mode = record.mode;
        builder.controller_cron_id = record.controller_cron_id;
    }
    // The latest ingested state per project becomes the row's latest_update —
    // same serialized shape the delivery scan used to produce, now read back
    // from the store the ingestor maintains.
    for (slug, project_state) in &latest_states {
        if deleted(slug) {
            continue;
        }
        let (mode, blocker) = record_signals.get(slug).cloned().unwrap_or_default();
        let update = store_update(slug, project_state, mode, blocker);
        let total = update_totals.get(slug).copied().unwrap_or(0);
        rows.entry(slug.clone())
            .or_insert_with(|| ProjectRowBuilder::new(slug.clone()))
            .attach_store_update(update, project_state.observations, total);
    }
    let unrouted: Vec<DeliveryUpdate> = unrouted_rows
        .into_iter()
        .map(|row| DeliveryUpdate {
            headline: row.headline,
            body: None,
            session_id: row.session_id,
            at: row.at,
            signature: row.signature,
            state: None,
            mode: row.mode,
            blocker: row.blocker,
        })
        .collect();

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

/// The store-held latest state of a project, rendered in the exact serialized
/// shape the per-request delivery scan used to produce — every surface (web
/// dashboard, desktop plugin, iOS) consumes `latest_update` as-is.
///
/// Field mapping: `headline`/`at`(=last_seen_at)/`session_id` come from the
/// state event; `signature` is the canonical slug (the routing key the row is
/// keyed by); `state` is the stored descriptor unless it is a synthetic
/// `ctrl:` marker (CTRL-only deliveries never had a descriptor); `mode` and
/// `blocker` come from the roster record; `body` is not stored — `None`.
fn store_update(
    slug: &str,
    state: &super::projects_store::ProjectState,
    mode: Option<String>,
    blocker: Option<String>,
) -> DeliveryUpdate {
    DeliveryUpdate {
        headline: state.headline.clone().unwrap_or_default(),
        body: None,
        session_id: state.session_id.clone().unwrap_or_default(),
        at: state.last_seen_at.clone(),
        signature: Some(slug.to_string()),
        state: Some(state.signature.clone())
            .filter(|descriptor| !descriptor.starts_with(CTRL_DESCRIPTOR_PREFIX)),
        mode,
        blocker,
    }
}

/// Turn a slug into a readable fallback name: `ec-defensive-research` ->
/// `Ec Defensive Research`. Used only when the roster carries no explicit
/// title, so a raw slug never leaks into a notification or a board row.
pub(crate) fn humanize_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct ProjectRowBuilder {
    slug: String,
    /// Roster title/next-action, attached when the slug has a roster record.
    title: Option<String>,
    next_action: Option<String>,
    /// Roster mode + controller link, attached alongside title/next_action.
    mode: Option<String>,
    controller_cron_id: Option<String>,
    tracker: Option<TrackerInfo>,
    missions: Vec<MissionChip>,
    /// Health inputs, accumulated alongside the display chips.
    health_inputs: Vec<OwnedHealthInput>,
    latest_update: Option<DeliveryUpdate>,
    /// How many consecutive deliveries reported the latest state — the stall
    /// signal's input, read from the store's observation count.
    latest_observations: u32,
    updates_count: usize,
}

impl ProjectRowBuilder {
    fn new(slug: String) -> Self {
        Self {
            slug,
            title: None,
            next_action: None,
            mode: None,
            controller_cron_id: None,
            tracker: None,
            missions: Vec::new(),
            health_inputs: Vec::new(),
            latest_update: None,
            latest_observations: 0,
            updates_count: 0,
        }
    }

    /// Attach the store-derived latest update: the newest state event plus the
    /// roster's mode/blocker, with the observation count driving the stall
    /// signal and the timeline total driving `updates_count`.
    fn attach_store_update(&mut self, update: DeliveryUpdate, observations: u32, total: usize) {
        self.latest_update = Some(update);
        self.latest_observations = observations;
        self.updates_count = total;
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
            // Compare the state descriptor, not the routing key: the store
            // collapses consecutive identical descriptors into one row and
            // counts observations, so "three deliveries reported this state
            // running" is exactly `observations >= 3` on the latest event.
            if latest.state.is_some() && self.latest_observations >= 3 {
                attention.push("same state on 3 consecutive updates".to_string());
            }
        }
        // A parked question always needs the operator: no freshness or mode
        // signal from the controller can answer on the human's behalf.
        let awaiting_user = self
            .missions
            .iter()
            .filter(|chip| chip.status == MissionStatus::AwaitingUser)
            .count();
        if awaiting_user > 0 {
            attention.push(match awaiting_user {
                1 => "1 mission awaiting user input".to_string(),
                count => format!("{count} missions awaiting user input"),
            });
        }

        // Fresh-active suppression: the controller reported recently (silent
        // ticks advance `at` too), says it is active, and reports no blocker —
        // it has seen the failed/interrupted missions and continues, so those
        // mission-derived reasons are noise, not attention. A stale controller,
        // a blocker, a non-active mode, or an awaiting_user mission each keep
        // the reasons. When suppression applies the reasons are not emitted at
        // all: cards and pills count `attention_reasons` as rendered.
        let mode_active = self
            .mode
            .as_deref()
            .or_else(|| {
                self.latest_update
                    .as_ref()
                    .and_then(|update| update.mode.as_deref())
            })
            .is_some_and(|mode| mode == "active");
        let signal_fresh = self
            .latest_update
            .as_ref()
            .and_then(|update| chrono::DateTime::parse_from_rfc3339(&update.at).ok())
            .zip(chrono::DateTime::parse_from_rfc3339(now).ok())
            .is_some_and(|(at, now)| {
                let age = now.signed_duration_since(at);
                age <= chrono::Duration::seconds(attention_fresh_signal_secs())
            });
        let blocker_set = self
            .latest_update
            .as_ref()
            .is_some_and(|update| update.blocker.is_some());
        let suppress_mission_reasons =
            signal_fresh && mode_active && !blocker_set && awaiting_user == 0;

        // One aggregated line instead of one per mission: the detail pane
        // already lists every mission chip.
        let problem_missions: Vec<&MissionChip> = if suppress_mission_reasons {
            Vec::new()
        } else {
            self.missions
                .iter()
                .filter(|chip| {
                    matches!(
                        chip.status,
                        MissionStatus::Failed | MissionStatus::Interrupted
                    )
                })
                .collect()
        };
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

        // Never let a surface fall back to the raw lowercase-hyphenated slug
        // ("ec-security"): when the roster carries no title, present a
        // humanized slug ("Ec Security") so notifications/board rows always
        // read as a name. (A project's true display title still wins when set.)
        let title = self
            .title
            .clone()
            .or_else(|| Some(humanize_slug(&self.slug)));

        ProjectRow {
            slug: self.slug,
            title,
            next_action: self.next_action,
            bucket,
            board_override: forced.map(str::to_string),
            controller_cron_id: self.controller_cron_id,
            mode: self.mode,
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

/// True when every field of a state descriptor is an unfilled `<placeholder>`.
///
/// Deliberately narrow: a descriptor is rejected only when it carries no real
/// content at all. A partially-filled trailer is still a state — the operator
/// learns more from `phase1|<heads>|blocked` than from silence.
fn is_placeholder_descriptor(descriptor: &str) -> bool {
    let mut saw_field = false;
    for field in descriptor.split('|') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        saw_field = true;
        if !(field.starts_with('<') && field.ends_with('>')) {
            return false;
        }
    }
    saw_field
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
    State(state): State<Arc<AppState>>,
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
            // Operator intent is authoritative over a stale controller mode:
            // resuming clears a lingering self-pause/blocked so the project
            // stops being buried while the operator wants it active. The
            // controller re-confirms/adjusts its mode on its next tick. Ignore
            // the "unknown project" error (a project with no roster record).
            let _ = state.projects.set_mode(&slug, "active", None, None);
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
            || trimmed.starts_with("[CTRL:")
        {
            continue;
        }
        headline = trimmed.trim_start_matches('#').trim().to_string();
        break;
    }
    if headline.is_empty() {
        headline = tag_title.unwrap_or_default();
    }

    // `[STATE_SIGNATURE: <routing-key>|<state fields…>]`. The first field says
    // WHICH project; everything after it says WHAT state it is in. Conflating
    // the two is what made the stall signal meaningless — see `state` below.
    let trailer = content.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("[STATE_SIGNATURE:")
            .and_then(|rest| rest.strip_suffix(']'))
    });
    let ctrl = content.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("[CTRL:")
            .and_then(|rest| rest.strip_suffix(']'))
    });
    let signature = trailer
        .and_then(|inner| inner.split('|').next())
        .map(|key| key.trim().to_string())
        // Reject template placeholders like `<project>` and anything that
        // isn't a plain routing key.
        .filter(|key| is_plain_key(key))
        // Fall back to the `[CTRL: <project> | …]` trailer's own first field.
        // Requiring a controller to emit two separate trailers to be routed is
        // a rule it will eventually forget, and the failure is silent: the
        // report simply never reaches its project row. The mode trailer already
        // names the project, so accept it rather than lose the delivery.
        .or_else(|| {
            ctrl.and_then(|inner| inner.split('|').next())
                .map(|key| key.trim().to_string())
                .filter(|key| is_plain_key(key))
        });
    // The state descriptor: the trailer minus its routing key. Empty when the
    // controller emitted a key and nothing else, which is not a state.
    let state = trailer
        .and_then(|inner| inner.split_once('|'))
        .map(|(_, rest)| rest.trim().to_string())
        .filter(|rest| !rest.is_empty())
        // A controller that quotes the trailer's own template back into its
        // report emits `lido|<phase>|<heads>|<blocker>`. The routing key is a
        // real slug so `is_plain_key` above accepts it, and the last trailer
        // in the message wins — so an echoed template landing last would be
        // recorded as a genuine state and sit in the durable timeline forever.
        .filter(|rest| !is_placeholder_descriptor(rest));

    // `[CTRL: <project> | mode=active|blocked|paused | wait=<n> | next=…]`.
    // Emitted on every delivery INCLUDING `[SILENT]`, so a healthy quiet tick is
    // distinguishable from one stuck on the same blocker. Absent for controllers
    // that predate the convention: the field is then omitted entirely rather
    // than defaulted, so the UI can render exactly as it did before.
    let mode = ctrl
        .and_then(|inner| {
            inner
                .split('|')
                .filter_map(|field| field.trim().strip_prefix("mode="))
                .map(|value| value.trim().to_ascii_lowercase())
                .next()
        })
        .filter(|value| {
            // Only the three known regimes, each optionally carrying a cause
            // (`blocked:transport-cap`). Anything else is a malformed trailer
            // and must not reach the UI as a mode chip.
            let base = value
                .split_once(':')
                .map_or(value.as_str(), |(base, _)| base);
            matches!(base, "active" | "blocked" | "paused")
        });

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
        state,
        mode,
        blocker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_slug_makes_a_readable_name() {
        assert_eq!(
            humanize_slug("ec-defensive-research"),
            "Ec Defensive Research"
        );
        assert_eq!(humanize_slug("coldcard"), "Coldcard");
        assert_eq!(humanize_slug("minimax_m3_full263"), "Minimax M3 Full263");
        assert_eq!(humanize_slug("verity-core"), "Verity Core");
        assert_eq!(humanize_slug(""), "");
    }

    const SAMPLE: &str = "[Cron delivery: Verity two-phase Fable/Codex progression]\n\
        Verity — BLOQUÉE PAR LE CONTROL PLANE\n\n\
        **Changé :** l'audit est terminé.\n\
        **Bloqué par :** lease writer fantôme sur #2219.\n\n\
        [STATE_SIGNATURE: verity|phase-1b|abc123|blocked|lease|next]";

    #[test]
    fn ctrl_trailer_yields_mode_and_never_becomes_the_headline() {
        // A quiet tick: the only content is [SILENT] plus the trailer. The
        // trailer must not be mistaken for the headline.
        let quiet = "[Cron delivery: Verity]\n[SILENT]\n[CTRL: verity | mode=active | wait=0 | next=certify #2240]";
        let parsed = parse_delivery("s1", 0.0, quiet);
        assert_eq!(parsed.mode.as_deref(), Some("active"));
        assert_eq!(parsed.headline, "[SILENT]");

        // A cause rides along with the mode.
        let blocked = "[Cron delivery: Bench]\nTransport rejected\n[CTRL: verity-benchmark | mode=blocked:transport-cap | wait=3 | next=shrink tree]";
        assert_eq!(
            parse_delivery("s2", 0.0, blocked).mode.as_deref(),
            Some("blocked:transport-cap")
        );

        // Controllers that never adopted the trailer report no mode at all,
        // rather than defaulting to one: the UI must render as it did before.
        let legacy = "[Cron delivery: Lido]\nSomething happened\n[STATE_SIGNATURE: lido|phase|head|none|next]";
        assert!(parse_delivery("s3", 0.0, legacy).mode.is_none());

        // The CTRL trailer alone routes the delivery: requiring two separate
        // trailers is a rule a controller eventually forgets, and the failure
        // is silent — the report never reaches its project row.
        let ctrl_only =
            "[Cron delivery: Verity]\nDid a thing\n[CTRL: verity | mode=active | wait=0 | next=x]";
        assert_eq!(
            parse_delivery("s5", 0.0, ctrl_only).signature.as_deref(),
            Some("verity")
        );

        // When both are present the explicit routing trailer still wins.
        let both = "[Cron delivery: X]\nhi\n[CTRL: ctrl-key | mode=active | wait=0 | next=x]\n[STATE_SIGNATURE: sig-key|a|b|c|d]";
        assert_eq!(
            parse_delivery("s6", 0.0, both).signature.as_deref(),
            Some("sig-key")
        );

        // A malformed mode is dropped rather than surfaced as a chip.
        let bogus = "[Cron delivery: X]\nhi\n[CTRL: x | mode=confused | wait=0 | next=none]";
        assert!(parse_delivery("s4", 0.0, bogus).mode.is_none());
    }

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

    /// The trailer's first field routes; the rest describes the state. Keeping
    /// them apart is the whole fix — see the stall-signal test below.
    #[test]
    fn the_state_descriptor_is_the_trailer_minus_its_routing_key() {
        let content = "[Cron delivery: x]\nTitre\n\
                       [STATE_SIGNATURE: verity|phase1-stack|7dba916|clean-ready|none]\n";
        let parsed = parse_delivery("sess-1", 1_754_000_000.0, content);
        assert_eq!(parsed.signature.as_deref(), Some("verity"));
        assert_eq!(
            parsed.state.as_deref(),
            Some("phase1-stack|7dba916|clean-ready|none")
        );
    }

    /// Observed on 2026-08-05: the Lido controller quoted the trailer template
    /// from its own instructions back into the report, so the message carried
    /// both the template and the real signature. The last trailer wins, so an
    /// echo landing last would be ingested as a genuine state and kept in the
    /// durable timeline. The routing key is a real slug, so `is_plain_key`
    /// does not catch it.
    #[test]
    fn an_echoed_template_is_not_a_state() {
        let content = "[Cron delivery: x]\nTitre\n\
                       [STATE_SIGNATURE: lido|<phase>|<heads>|<blocker>|<next-action>]\n";
        let parsed = parse_delivery("s", 1_754_000_000.0, content);
        assert_eq!(parsed.signature.as_deref(), Some("lido"), "the key is real");
        assert_eq!(parsed.state, None, "but the descriptor carries no state");
    }

    #[test]
    fn a_partly_filled_descriptor_is_still_a_state() {
        // Rejecting these would lose real information: knowing the phase and
        // that it is blocked beats knowing nothing.
        let content = "[Cron delivery: x]\nTitre\n\
                       [STATE_SIGNATURE: lido|phase3|<heads>|blocked-on-2231]\n";
        let parsed = parse_delivery("s", 1_754_000_000.0, content);
        assert_eq!(
            parsed.state.as_deref(),
            Some("phase3|<heads>|blocked-on-2231")
        );
    }

    #[test]
    fn the_real_signature_wins_when_a_template_precedes_it() {
        let content = "[Cron delivery: x]\nTitre\n\
                       [STATE_SIGNATURE: lido|<phase>|<heads>]\n\
                       [STATE_SIGNATURE: lido|phase3|a3d80673|none]\n";
        let parsed = parse_delivery("s", 1_754_000_000.0, content);
        assert_eq!(parsed.state.as_deref(), Some("phase3|a3d80673|none"));
    }

    #[test]
    fn a_routing_key_with_no_state_fields_is_not_a_state() {
        let content = "[Cron delivery: x]\nTitre\n[STATE_SIGNATURE: verity]\n";
        let parsed = parse_delivery("sess-1", 1_754_000_000.0, content);
        assert_eq!(parsed.signature.as_deref(), Some("verity"));
        assert_eq!(parsed.state, None);

        // A trailing separator with nothing after it is the same absence.
        let content = "[Cron delivery: x]\nTitre\n[STATE_SIGNATURE: verity|  ]\n";
        assert_eq!(parse_delivery("s", 1.0, content).state, None);
    }

    /// The signal used to compare the *routing key*, which is near-constant
    /// within a project row by construction — so it fired for essentially
    /// every project with three updates, flagging steady progress and a real
    /// stall identically. Measured on prod: 6 of 26 rows, i.e. exactly those
    /// with three or more deliveries.
    #[test]
    fn the_stall_signal_tracks_the_state_not_the_routing_key() {
        let delivery = |state: &str| DeliveryUpdate {
            headline: "h".into(),
            body: None,
            session_id: "s".into(),
            at: "2026-08-04T12:00:00Z".into(),
            signature: Some("verity".into()),
            mode: None,
            state: Some(state.into()),
            blocker: None,
        };

        // A project changing state every tick: the latest state has a single
        // observation, however many total updates there were. Not a stall.
        let mut moving = ProjectRowBuilder::new("verity".into());
        moving.attach_store_update(delivery("c|3"), 1, 3);
        let row = moving.finish(&[], None, None, "2026-08-04T20:00:00Z");
        assert!(
            !row.attention_reasons
                .iter()
                .any(|r| r.contains("3 consecutive")),
            "a project changing state every tick must not be flagged: {:?}",
            row.attention_reasons
        );

        // Same state three times running: that is the stall worth reporting.
        let mut stuck = ProjectRowBuilder::new("verity".into());
        stuck.attach_store_update(delivery("blocked|same"), 3, 3);
        let row = stuck.finish(&[], None, None, "2026-08-04T20:00:00Z");
        assert!(
            row.attention_reasons
                .iter()
                .any(|r| r.contains("3 consecutive")),
            "an unchanged state must still be flagged: {:?}",
            row.attention_reasons
        );

        // A synthetic CTRL-only marker never surfaces as a state, so repeated
        // quiet CTRL ticks must not trip the stall signal either.
        let mut quiet = ProjectRowBuilder::new("verity".into());
        let mut update = delivery("ignored");
        update.state = None;
        quiet.attach_store_update(update, 5, 5);
        let row = quiet.finish(&[], None, None, "2026-08-04T20:00:00Z");
        assert!(!row
            .attention_reasons
            .iter()
            .any(|r| r.contains("3 consecutive")));
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
        // Three deliveries of SAMPLE collapse in the store into one state
        // event with observations=3; the builder sees that count.
        builder.attach_store_update(parse_delivery("s", 3.0, SAMPLE), 3, 3);
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

    // ---- fresh-active suppression ----

    fn failed_chip(id: &str) -> MissionChip {
        MissionChip {
            id: id.to_string(),
            status: MissionStatus::Failed,
            title: None,
            updated_at: "2026-08-02T00:00:00Z".to_string(),
            github_pr: None,
        }
    }

    fn active_update(at: &str, blocker: Option<&str>) -> DeliveryUpdate {
        DeliveryUpdate {
            headline: "tick".into(),
            body: None,
            session_id: "s".into(),
            at: at.into(),
            signature: Some("verity".into()),
            state: Some("phase|head|clean".into()),
            mode: Some("active".into()),
            blocker: blocker.map(str::to_string),
        }
    }

    /// The incident shape: 48h-old failed missions, but the controller
    /// reported minutes ago, says active, and reports no blocker. It has seen
    /// those missions and continues — the row must not sit on the attention
    /// shelf, and the suppressed reasons must not be emitted at all.
    #[test]
    fn fresh_active_controller_suppresses_mission_derived_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        builder
            .missions
            .push(failed_chip("00000001-aaaa-bbbb-cccc-dddddddddddd"));
        builder.attach_store_update(active_update("2026-08-04T11:50:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "active");
        assert!(
            row.attention_reasons.is_empty(),
            "suppressed reasons must not be emitted: {:?}",
            row.attention_reasons
        );
    }

    /// A stale controller cannot vouch for its failures: past the freshness
    /// window the failed missions flag the row again.
    #[test]
    fn stale_controller_with_failed_missions_stays_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        builder
            .missions
            .push(failed_chip("00000001-aaaa-bbbb-cccc-dddddddddddd"));
        builder.attach_store_update(active_update("2026-08-02T12:00:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert!(row.attention_reasons.iter().any(|r| r.contains("Failed")));
    }

    /// A reported blocker always wins over freshness: the controller itself
    /// says it is stuck.
    #[test]
    fn fresh_controller_with_blocker_stays_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        builder
            .missions
            .push(failed_chip("00000001-aaaa-bbbb-cccc-dddddddddddd"));
        builder.attach_store_update(
            active_update("2026-08-04T11:50:00Z", Some("waiting on CI runner")),
            1,
            5,
        );
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert!(row.attention_reasons.iter().any(|r| r.contains("blocker")));
        // Suppression is off entirely: the failed mission surfaces too.
        assert!(row.attention_reasons.iter().any(|r| r.contains("Failed")));
    }

    /// A parked question is always attention: no controller signal can answer
    /// on the operator's behalf, and it disables suppression for the row.
    #[test]
    fn awaiting_user_is_attention_regardless_of_freshness() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        builder.missions.push(MissionChip {
            id: "00000002-aaaa-bbbb-cccc-dddddddddddd".to_string(),
            status: MissionStatus::AwaitingUser,
            title: None,
            updated_at: "2026-08-04T11:00:00Z".to_string(),
            github_pr: None,
        });
        builder
            .missions
            .push(failed_chip("00000001-aaaa-bbbb-cccc-dddddddddddd"));
        builder.attach_store_update(active_update("2026-08-04T11:50:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert!(row
            .attention_reasons
            .iter()
            .any(|r| r.contains("awaiting user input")));
        assert!(row.attention_reasons.iter().any(|r| r.contains("Failed")));
    }

    /// Freshness alone is not enough: without an active mode the controller
    /// has not vouched for anything.
    #[test]
    fn fresh_signal_without_active_mode_does_not_suppress() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder
            .missions
            .push(failed_chip("00000001-aaaa-bbbb-cccc-dddddddddddd"));
        let mut update = active_update("2026-08-04T11:50:00Z", None);
        update.mode = None;
        builder.attach_store_update(update, 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert!(row.attention_reasons.iter().any(|r| r.contains("Failed")));
    }

    #[test]
    fn forced_override_silences_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.attach_store_update(parse_delivery("s", 3.0, SAMPLE), 3, 3);
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
        builder.attach_store_update(
            DeliveryUpdate {
                headline: "tick".into(),
                body: None,
                at: "2026-08-04T12:00:00Z".into(),
                session_id: "cron_e594d751447d_20260804_120931".into(),
                signature: None,
                mode: None,
                state: None,
                blocker: None,
            },
            1,
            1,
        );
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
        builder.attach_store_update(
            DeliveryUpdate {
                headline: "tick".into(),
                body: None,
                at: "2026-08-04T12:00:00Z".into(),
                session_id: "cron_e594d751447d_20260804_120931".into(),
                signature: None,
                mode: None,
                state: None,
                blocker: None,
            },
            1,
            1,
        );
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        let conversation = row.conversation.expect("conversation");
        assert_eq!(conversation.source, "latest_update");
        assert_eq!(conversation.bound_at, None);
    }

    /// Roster metadata rides on the row: a set title replaces the slug on cards
    /// and the palette; next_action renders on attention cards. next_action is
    /// optional and absent from the JSON when unset. `title`, however, is never
    /// absent — when the roster carries no title the row humanizes the slug so a
    /// surface never falls back to the raw lowercase-hyphenated slug.
    #[test]
    fn roster_title_and_next_action_ride_on_the_row() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.title = Some("Verity 4.31 convergence".to_string());
        builder.next_action = Some("certify #2240".to_string());
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.title.as_deref(), Some("Verity 4.31 convergence"));
        assert_eq!(row.next_action.as_deref(), Some("certify #2240"));

        let bare = ProjectRowBuilder::new("lido".to_string()).finish(
            &[],
            None,
            None,
            "2026-08-04T12:00:00Z",
        );
        // A bare row still gets a humanized title ("Lido") — never the raw slug,
        // never omitted — so notifications and board rows always read as a name.
        assert_eq!(bare.title.as_deref(), Some("Lido"));
        let json = serde_json::to_value(&bare).expect("serialize");
        assert_eq!(
            json.get("title").and_then(|v| v.as_str()),
            Some("Lido"),
            "unset title humanizes the slug rather than being omitted"
        );
        assert!(
            json.get("next_action").is_none(),
            "unset next_action is omitted"
        );
    }

    /// Provenance rides on the row: `override` is the operator's board action,
    /// `mode` is the controller's own report. An operator pause serializes the
    /// override; a controller self-pause serializes mode without an override —
    /// that difference is what lets clients render "paused by you" vs
    /// "controller stopped itself". All three fields are omitted when unset.
    #[test]
    fn override_mode_and_controller_id_expose_stop_provenance() {
        // Operator pause: board override present, controller still active.
        let mut operator = ProjectRowBuilder::new("verity".to_string());
        operator.mode = Some("active".to_string());
        operator.controller_cron_id = Some("cron-abc123".to_string());
        let row = operator.finish(&[], Some("paused"), None, "2026-08-04T12:00:00Z");
        let json = serde_json::to_value(&row).expect("serialize");
        assert_eq!(json["override"], "paused");
        assert_eq!(json["mode"], "active");
        assert_eq!(json["controller_cron_id"], "cron-abc123");
        assert_eq!(row.bucket, "paused");

        // Controller self-pause: mode says paused, no override.
        let mut cut = ProjectRowBuilder::new("lido".to_string());
        cut.mode = Some("paused".to_string());
        let row = cut.finish(&[], None, None, "2026-08-04T12:00:00Z");
        let json = serde_json::to_value(&row).expect("serialize");
        assert!(json.get("override").is_none(), "no override was set");
        assert_eq!(json["mode"], "paused");

        // Nothing set: all three are omitted from the JSON.
        let bare = ProjectRowBuilder::new("erc".to_string()).finish(
            &[],
            None,
            None,
            "2026-08-04T12:00:00Z",
        );
        let json = serde_json::to_value(&bare).expect("serialize");
        assert!(json.get("override").is_none());
        assert!(json.get("mode").is_none());
        assert!(json.get("controller_cron_id").is_none());
    }

    // ---- store-driven overview (the ingestor is the only delivery reader) ----

    use super::super::projects_store::{ProjectState, ProjectsStore};

    /// A CTRL-only delivery (no STATE_SIGNATURE descriptor) must still land as
    /// the row's latest_update — via the store, with no per-request scan of
    /// HERMES_STATE_DB. The whole path here runs with that env unset.
    #[test]
    fn a_ctrl_only_delivery_headline_lands_on_the_row_via_the_store() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let ctrl_only =
            "[Cron delivery: Verity]\nDid a thing\n[CTRL: verity | mode=active | wait=0 | next=x]";
        ingest_deliveries(
            &store,
            &HashMap::new(),
            &HashMap::new(),
            vec![parse_delivery("sess-1", 1_754_000_000.0, ctrl_only)],
        );

        let latest = store.latest_states().expect("latest");
        let event = latest.get("verity").expect("state event recorded");
        let record = store
            .get_project("verity")
            .expect("read")
            .expect("roster auto-upserted");
        let update = store_update("verity", event, record.mode, record.blocker);
        assert_eq!(update.headline, "Did a thing");
        assert_eq!(update.session_id, "sess-1");
        assert_eq!(update.signature.as_deref(), Some("verity"));
        assert_eq!(update.mode.as_deref(), Some("active"));
        assert_eq!(
            update.state, None,
            "the synthetic ctrl descriptor is not a state"
        );
    }

    /// A `[SILENT]` tick after a real report must advance freshness without
    /// stealing the headline: the card keeps the last meaningful headline,
    /// stamped with the silent delivery's (fresher) time. Observed live on
    /// verity-lido, where cards showed the literal string "[SILENT]".
    #[test]
    fn a_silent_delivery_keeps_the_previous_headline_but_advances_freshness() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let real = "[Cron delivery: Verity]\nCertify #2240 merged\n\
                    [CTRL: verity | mode=active | wait=0 | next=x]";
        let quiet = "[Cron delivery: Verity]\n[SILENT]\n\
                     [CTRL: verity | mode=active | wait=1 | next=x]";
        ingest_deliveries(
            &store,
            &HashMap::new(),
            &HashMap::new(),
            // Newest-first, as read_deliveries returns them.
            vec![
                parse_delivery("sess-2", 1_754_003_600.0, quiet),
                parse_delivery("sess-1", 1_754_000_000.0, real),
            ],
        );

        let latest = store.latest_states().expect("latest");
        let event = latest.get("verity").expect("state event recorded");
        let update = store_update("verity", event, None, None);
        assert_eq!(
            update.headline, "Certify #2240 merged",
            "the [SILENT] tick must not replace the last meaningful headline"
        );
        assert_eq!(
            update.at,
            parse_delivery("sess-2", 1_754_003_600.0, quiet).at,
            "freshness must advance to the silent delivery's time"
        );
        assert_eq!(event.observations, 2, "the quiet tick is still counted");
    }

    /// A controller whose very first delivery is `[SILENT]` must not put the
    /// literal marker on the card: the event lands with no headline at all.
    #[test]
    fn a_silent_first_delivery_records_no_garbage_headline() {
        let store = ProjectsStore::open_in_memory().expect("store");
        let quiet = "[Cron delivery: Lido]\n[SILENT]\n\
                     [CTRL: lido | mode=active | wait=0 | next=x]";
        ingest_deliveries(
            &store,
            &HashMap::new(),
            &HashMap::new(),
            vec![parse_delivery("sess-1", 1_754_000_000.0, quiet)],
        );

        let latest = store.latest_states().expect("latest");
        let event = latest.get("lido").expect("freshness is still recorded");
        assert_eq!(event.headline, None, "no [SILENT] garbage headline");
        let update = store_update("lido", event, None, None);
        assert_eq!(update.headline, "");
    }

    /// The serialized shape of a store-built latest_update is the one every
    /// surface already consumes: body present (null), state/mode omitted when
    /// absent, the rest verbatim.
    #[test]
    fn the_store_built_latest_update_keeps_the_delivery_shape() {
        let event = ProjectState {
            signature: "phase1|abc".into(),
            headline: Some("Verity — stable".into()),
            first_seen_at: "2026-08-04T10:00:00Z".into(),
            last_seen_at: "2026-08-04T12:00:00Z".into(),
            observations: 2,
            session_id: Some("sess-9".into()),
        };
        let full = store_update(
            "verity",
            &event,
            Some("blocked:cap".into()),
            Some("cap".into()),
        );
        let json = serde_json::to_value(&full).expect("serialize");
        assert_eq!(json["headline"], "Verity — stable");
        assert_eq!(json["body"], serde_json::Value::Null);
        assert_eq!(json["session_id"], "sess-9");
        assert_eq!(json["at"], "2026-08-04T12:00:00Z");
        assert_eq!(json["signature"], "verity");
        assert_eq!(json["state"], "phase1|abc");
        assert_eq!(json["mode"], "blocked:cap");
        assert_eq!(json["blocker"], "cap");

        let mut bare_event = event;
        bare_event.signature = "ctrl:report".into();
        bare_event.session_id = None;
        let bare = store_update("verity", &bare_event, None, None);
        let json = serde_json::to_value(&bare).expect("serialize");
        assert!(json.get("state").is_none(), "absent state is omitted");
        assert!(json.get("mode").is_none(), "absent mode is omitted");
        assert_eq!(json["session_id"], "");
        assert_eq!(json["blocker"], serde_json::Value::Null);
    }

    /// An alias whose target is archived — by roster status or board override —
    /// must refuse the route: the delivery surfaces as unrouted instead of
    /// silently feeding a row nobody watches. routes.json is never rewritten.
    #[test]
    fn an_alias_onto_an_archived_target_is_refused_and_surfaces_as_unrouted() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido-audit", None, None, None, None)
            .expect("seed");
        store.set_status("lido-audit", "archived").expect("archive");
        let aliases: HashMap<String, String> =
            [("lido".to_string(), "lido-audit".to_string())].into();

        let routed = "[Cron delivery: Lido]\nAudit tick\n[STATE_SIGNATURE: lido|phase3|abc|none]";
        ingest_deliveries(
            &store,
            &aliases,
            &HashMap::new(),
            vec![parse_delivery("sess-2", 1_754_000_000.0, routed)],
        );
        assert!(
            store
                .latest_states()
                .expect("latest")
                .get("lido-audit")
                .is_none(),
            "nothing may be recorded against the archived target"
        );
        let unrouted = store.unrouted(10).expect("unrouted");
        assert_eq!(unrouted.len(), 1);
        assert_eq!(unrouted[0].signature.as_deref(), Some("lido"));
        assert_eq!(unrouted[0].headline, "Audit tick");

        // Board override archived/deleted refuses the route the same way.
        let aliases2: HashMap<String, String> =
            [("verity".to_string(), "verity-roadmap".to_string())].into();
        let overrides: HashMap<String, String> =
            [("verity-roadmap".to_string(), "archived".to_string())].into();
        let routed2 = "[Cron delivery: V]\nTick\n[STATE_SIGNATURE: verity|p|x|y]";
        ingest_deliveries(
            &store,
            &aliases2,
            &overrides,
            vec![parse_delivery("sess-3", 1_754_000_001.0, routed2)],
        );
        assert!(store
            .latest_states()
            .expect("latest")
            .get("verity-roadmap")
            .is_none());
        assert_eq!(store.unrouted(10).expect("unrouted").len(), 2);

        // A direct (non-aliased) key still routes even when archived: only a
        // stale alias is refused.
        let direct = "[Cron delivery: L]\nDirect\n[STATE_SIGNATURE: lido-audit|p|x|y]";
        ingest_deliveries(
            &store,
            &HashMap::new(),
            &HashMap::new(),
            vec![parse_delivery("sess-4", 1_754_000_002.0, direct)],
        );
        assert!(store
            .latest_states()
            .expect("latest")
            .get("lido-audit")
            .is_some());
    }

    /// A delivery with no routing key at all lands in the triage inbox — this
    /// used to be derived per request by the overview's own scan.
    #[test]
    fn a_keyless_delivery_is_recorded_as_unrouted_by_the_ingestor() {
        let store = ProjectsStore::open_in_memory().expect("store");
        ingest_deliveries(
            &store,
            &HashMap::new(),
            &HashMap::new(),
            vec![parse_delivery(
                "sess-5",
                1_754_000_000.0,
                "[Cron delivery: Mystery]\nNo trailer here.",
            )],
        );
        let unrouted = store.unrouted(10).expect("unrouted");
        assert_eq!(unrouted.len(), 1);
        assert_eq!(unrouted[0].headline, "No trailer here.");
        assert!(
            store.latest_states().expect("latest").is_empty(),
            "no phantom project was fabricated"
        );
    }

    /// Repeated CTRL-only quiet ticks collapse into one state event whose
    /// observation count and mode projection keep working.
    #[test]
    fn repeated_ctrl_only_ticks_collapse_and_project_the_mode() {
        let store = ProjectsStore::open_in_memory().expect("store");
        for i in 0..3 {
            let tick = "[Cron delivery: Lido]\n[SILENT]\n[CTRL: lido | mode=blocked:transport-cap | wait=0 | next=x]";
            ingest_deliveries(
                &store,
                &HashMap::new(),
                &HashMap::new(),
                vec![parse_delivery(
                    &format!("sess-{i}"),
                    1_754_000_000.0 + (i as f64) * 60.0,
                    tick,
                )],
            );
        }
        let latest = store.latest_states().expect("latest");
        let event = latest.get("lido").expect("event");
        assert_eq!(event.observations, 3);
        assert_eq!(event.session_id.as_deref(), Some("sess-2"), "newest wins");
        let record = store.get_project("lido").expect("read").expect("present");
        assert_eq!(record.mode.as_deref(), Some("blocked"));
        assert_eq!(record.blocker.as_deref(), Some("transport-cap"));
        assert_eq!(record.wait_ticks, 2);
        assert_eq!(store.state_event_totals().expect("totals")["lido"], 3);
    }
}
