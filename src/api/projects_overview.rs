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

use std::collections::{HashMap, HashSet};
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
            let live = live_writer_project_slugs(&state).await;
            ingest_deliveries_with_live(&state.projects, &aliases, &overrides, deliveries, &live);
            if let Err(error) = state
                .projects
                .expire_pending_decisions(super::controller_honesty::PENDING_DECISION_TTL)
            {
                tracing::warn!("state ingest expire decisions: {error}");
            }
        }
    });
}

/// Roster slugs that currently have an executing writer. Used so ingest can
/// refuse a lease-writer headline while work is live.
async fn live_writer_project_slugs(state: &super::routes::AppState) -> HashSet<String> {
    let Ok(projects) = state.projects.list_projects() else {
        return HashSet::new();
    };
    let mut live = HashSet::new();
    for project in projects {
        let Ok(missions) = state
            .control
            .collect_attention_missions_for_project(&project.slug)
            .await
        else {
            continue;
        };
        if missions
            .iter()
            .any(|mission| super::controller_honesty::is_live_writer_status(mission.status))
        {
            live.insert(project.slug);
        }
    }
    live
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
    ingest_deliveries_with_live(projects, aliases, overrides, deliveries, &HashSet::new());
}

fn ingest_deliveries_with_live(
    projects: &super::projects_store::ProjectsStore,
    aliases: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
    deliveries: Vec<DeliveryUpdate>,
    live_projects: &HashSet<String>,
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
        let has_live_writer = live_projects.contains(slug);
        let (gated_mode, _) = super::controller_honesty::coerce_mode_against_live(
            delivery.mode.as_deref(),
            delivery.blocker.as_deref(),
            has_live_writer,
        );
        let headline = Some(delivery.headline.trim()).filter(|h| !h.is_empty());
        let session = Some(delivery.session_id.as_str()).filter(|s| !s.is_empty());
        let descriptor = delivery.state.clone().unwrap_or_else(|| {
            format!("{CTRL_DESCRIPTOR_PREFIX}{}", gated_mode.unwrap_or("report"))
        });
        // `[SILENT]` is the controllers-policy convention for "nothing to
        // report". It must advance freshness (a quiet tick is proof of life),
        // but it must never become the headline the card shows — fold it onto
        // the previous state event so `latest_update` keeps surfacing the last
        // meaningful headline with the fresh timestamp.
        //
        // A relaunch-after-cancel-timeout or a lease-writer claim while a
        // writer is live is the same class: infra / a lie, not a chapter.
        let silence = headline.is_none_or(|text| {
            super::controller_honesty::should_silence_headline(text, has_live_writer)
        });
        let recorded = if silence {
            projects.record_silent_observation(slug, &descriptor, &delivery.at, session)
        } else {
            projects.record_state(slug, &descriptor, headline, &delivery.at, session)
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
        if let Some(mode) = gated_mode {
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
        // A `[DECISION: …]` trailer reaches the ledger through the same
        // enforcement gate as the HTTP endpoint: a claimed autonomous act is
        // coerced to an owner escalation unless the grant's autonomy level
        // covers acting. Keyed by the delivery's timestamp (INSERT OR IGNORE),
        // so overlapping ingest windows record it once and never reopen an
        // answered row.
        let mut recorded_decided = false;
        if let Some(trailer) = delivery.decision.as_ref() {
            let grant = projects.get_grant(slug).ok().flatten();
            let autonomy = grant.as_ref().and_then(|g| g.autonomy_level.clone());
            let merge_authority = grant.as_ref().and_then(|g| g.merge_authority.clone());
            match resolve_decision_disposition_for_grant(
                autonomy.as_deref(),
                merge_authority.as_deref(),
                trailer.authority.as_deref(),
                trailer.status.as_deref(),
                trailer.kind.as_deref(),
            ) {
                Ok(disposition) => {
                    recorded_decided = disposition.status == "decided";
                    let decision = super::projects_store::NewDecision {
                        question: trailer.question.clone(),
                        rationale: trailer.rationale.clone(),
                        kind: trailer.kind.clone(),
                        authority: disposition.authority,
                        status: disposition.status,
                        evidence: trailer.evidence.clone(),
                    };
                    if let Err(error) =
                        projects.record_decision_from_delivery(slug, &delivery.at, &decision)
                    {
                        tracing::warn!("state ingest decision: {slug}: {error}");
                    }
                }
                Err(error) => {
                    tracing::warn!("state ingest decision trailer rejected: {slug}: {error}");
                }
            }
        }
        // A merge announcement (headline or a grant-allowed decided act)
        // retires older "merge #N?" questions so the card stops asking
        // about a PR that already landed. The current delivery's own
        // coerced trailer is skipped — it stays the escalation.
        let needles = merge_announcement_needles(&delivery, recorded_decided);
        if !needles.is_empty() {
            if let Err(error) = projects.close_pending_decisions_referencing(
                slug,
                &needles,
                "closed: referenced PR merged",
                Some(delivery.at.as_str()),
            ) {
                tracing::warn!("state ingest close merged decisions: {slug}: {error}");
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
    let recent = state
        .projects
        .recent_decisions(&slug, 20)
        .map_err(store_err)?;
    let conversation = state.projects.binding(&slug).map_err(store_err)?;
    let proposals = state
        .projects
        .list_open_proposals(&slug)
        .map_err(store_err)?;
    let missions = match state
        .control
        .collect_attention_missions_for_project(&slug)
        .await
    {
        Ok(missions) => missions,
        Err(error) => {
            tracing::warn!(project = %slug, %error, "get_project: attention collect failed");
            Vec::new()
        }
    };
    let items = super::mission_horizon::project_items(&tracks, &proposals, &missions);
    Ok(Json(serde_json::json!({
        "project": project,
        "grant": grant,
        "tracks": tracks,
        "items": items,
        "open_decisions": decisions,
        "recent_decisions": recent,
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
pub struct RenameProjectRequest {
    pub new_slug: String,
}

/// `POST /api/projects/:slug/rename` — move a project to a new slug.
///
/// A rename is "move the canonical + leave a forwarding pointer": the store
/// rows move in one transaction, then the alias map gains `old → new` (and any
/// alias that pointed at `old` is flattened onto `new` — `resolve_alias` is
/// single-hop, so a chain would silently stop resolving). External references
/// — mission project tags, cron `deliver: project:<old>`, `[CTRL: old | …]`
/// signatures, tracker files — are deliberately not rewritten: the alias
/// covers them indefinitely.
///
/// Renaming onto an existing project or an established alias key is refused:
/// the first is a merge (explicit, via routes.json), the second would shadow
/// whatever that key already routes to.
pub async fn rename_project(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<RenameProjectRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let new_slug = req.new_slug.trim();
    if !is_plain_key(&slug) || !is_plain_key(new_slug) {
        return Err(bad_slug());
    }
    if new_slug == slug {
        return Err((
            StatusCode::BAD_REQUEST,
            "new slug is identical to the current one".to_string(),
        ));
    }
    let dir = hermes_projects_dir();
    if let Some(dir) = &dir {
        let aliases = read_alias_map(dir);
        if aliases.contains_key(new_slug) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "'{new_slug}' is already an alias for '{}' — pick another name or remove the alias first",
                    aliases[new_slug]
                ),
            ));
        }
    }
    let record = state
        .projects
        .rename_project(&slug, new_slug)
        .map_err(|error| {
            if error.contains("not found") {
                (StatusCode::NOT_FOUND, error)
            } else if error.contains("already exists") {
                (StatusCode::CONFLICT, error)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, error)
            }
        })?;
    let mut aliases_flattened = 0usize;
    if let Some(dir) = &dir {
        aliases_flattened = rewrite_aliases_for_rename(dir, &slug, new_slug).map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "project rows moved to '{new_slug}' but routes.json update failed ({error}); \
                     add \"{slug}\": \"{new_slug}\" to it manually or deliveries keyed '{slug}' will unroute"
                ),
            )
        })?;
        // A board override (paused/archived) follows the project it describes.
        let mut overrides = read_overrides(dir);
        if let Some(value) = overrides.remove(&slug) {
            overrides.insert(new_slug.to_string(), value);
            write_overrides(dir, &overrides).map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("write board-overrides.json: {error}"),
                )
            })?;
        }
    }
    Ok(Json(serde_json::json!({
        "project": record,
        "old_slug": slug,
        "alias_written": dir.is_some(),
        "aliases_flattened": aliases_flattened,
    })))
}

/// Rewrite routes.json for a rename: every alias pointing at `old` is
/// flattened onto `new` (single-hop resolution — a chain through `old` would
/// dead-end), then `old → new` itself is added. Returns how many existing
/// entries were flattened. Written atomically (tmp + rename), like the
/// overrides file.
fn rewrite_aliases_for_rename(dir: &Path, old: &str, new: &str) -> std::io::Result<usize> {
    let mut aliases = read_alias_map(dir);
    let mut flattened = 0usize;
    for target in aliases.values_mut() {
        if target == old {
            *target = new.to_string();
            flattened += 1;
        }
    }
    aliases.insert(old.to_string(), new.to_string());
    let serialized = serde_json::to_string_pretty(&aliases)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let path = dir.join("routes.json");
    let tmp = dir.join(".routes.json.tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, &path)?;
    Ok(flattened)
}

fn write_overrides(dir: &Path, overrides: &HashMap<String, String>) -> std::io::Result<()> {
    let serialized = serde_json::to_string_pretty(overrides)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let tmp = dir.join(".board-overrides.json.tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, overrides_path(dir))?;
    Ok(())
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
    let mut mode = req.mode.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "active" | "blocked" | "paused") {
        return Err((
            StatusCode::BAD_REQUEST,
            "mode must be active, blocked, or paused".to_string(),
        ));
    }
    let mut blocker = req.blocker.clone();
    if mode == "blocked" && super::controller_honesty::is_lease_blocker(blocker.as_deref()) {
        let has_live = state
            .control
            .collect_attention_missions_for_project(&slug)
            .await
            .ok()
            .is_some_and(|missions| {
                missions
                    .iter()
                    .any(|mission| super::controller_honesty::is_live_writer_status(mission.status))
            });
        if has_live {
            mode = "active".to_string();
            blocker = None;
        }
    }
    state
        .projects
        .set_mode(&slug, &mode, req.next_action.as_deref(), blocker.as_deref())
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
    pub autonomy_level: Option<String>,
}

pub(crate) const AUTONOMY_LEVELS: [&str; 4] = ["observe", "propose", "act_reversible", "act_full"];

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
    let autonomy_level = match req.autonomy_level.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(level) if AUTONOMY_LEVELS.contains(&level) => Some(level.to_string()),
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "autonomy_level must be one of {} (got '{other}')",
                    AUTONOMY_LEVELS.join(", ")
                ),
            ));
        }
    };
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
            autonomy_level.as_deref(),
        )
        .map_err(store_err)?;
    let grant = state.projects.get_grant(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({ "slug": slug, "grant": grant })))
}

#[derive(Debug, Deserialize)]
pub struct RecordDecisionRequest {
    pub question: String,
    pub rationale: Option<String>,
    /// merge | dispatch | scope | budget | … (free-form label)
    pub kind: Option<String>,
    /// granted (autonomous act) | escalation (question for the owner).
    /// Legacy callers omit it: default escalation.
    pub authority: Option<String>,
    /// decided | pending_user. Defaults follow the authority.
    pub status: Option<String>,
    /// Supporting links: {"pr_url": …, "mission_id": …}.
    pub evidence: Option<serde_json::Value>,
}

/// How a decision request lands in the ledger after the grant is applied.
pub(crate) struct DecisionDisposition {
    pub authority: String,
    pub status: String,
    /// Set when the grant downgraded a claimed autonomous act to an
    /// escalation — surfaced to the caller so the controller learns.
    pub coerced_reason: Option<String>,
}

/// Decision kinds that are irreversible once executed: under `act_reversible`
/// these coerce to an owner escalation exactly like any act under `propose`.
/// Free-form kinds outside this list pass — the list is the deny set for the
/// reversible tier, not a taxonomy.
pub(crate) const IRREVERSIBLE_KINDS: [&str; 6] = [
    "merge",
    "abandon",
    "delete",
    "publish",
    "deploy",
    "force_push",
];

/// The enforcement point shared by the HTTP endpoint and the delivery-trailer
/// ingestor: a controller may only *record* an act as autonomous when its
/// grant actually allows acting. `observe`/`propose` (or an unset level)
/// coerce granted+decided into an owner escalation instead of failing, so a
/// mis-calibrated controller degrades to asking rather than erroring — and
/// `act_reversible` additionally escalates the irreversible kinds.
pub(crate) fn resolve_decision_disposition(
    autonomy_level: Option<&str>,
    authority: Option<&str>,
    status: Option<&str>,
    kind: Option<&str>,
) -> Result<DecisionDisposition, String> {
    resolve_decision_disposition_for_grant(autonomy_level, None, authority, status, kind)
}

/// Same gate as [`resolve_decision_disposition`], honoring `merge_authority`.
/// `act_reversible` still escalates destroy/publish/deploy/force_push, but a
/// `merge` is allowed when the grant says `full` — otherwise controllers with
/// `merge_authority=full` keep asking Thomas (Verity/Lido, 2026-08-14).
pub(crate) fn resolve_decision_disposition_for_grant(
    autonomy_level: Option<&str>,
    merge_authority: Option<&str>,
    authority: Option<&str>,
    status: Option<&str>,
    kind: Option<&str>,
) -> Result<DecisionDisposition, String> {
    let authority = match authority.map(str::trim).filter(|a| !a.is_empty()) {
        None => "escalation",
        Some(a @ ("granted" | "escalation")) => a,
        Some(other) => {
            return Err(format!(
                "authority must be granted or escalation (got '{other}')"
            ))
        }
    };
    let status = match status.map(str::trim).filter(|s| !s.is_empty()) {
        None => {
            if authority == "granted" {
                "decided"
            } else {
                "pending_user"
            }
        }
        Some(s @ ("decided" | "pending_user")) => s,
        Some(other) => {
            return Err(format!(
                "status must be decided or pending_user (got '{other}')"
            ));
        }
    };
    if authority == "granted" && status == "decided" {
        // Controllers-policy default is *act*. An unset grant is not observe:
        // treating it as observe made every `[DECISION: authority=granted]`
        // bounce back to Thomas (Lido #66 / Verity #2332, 2026-08-13).
        let effective = match autonomy_level.map(str::trim).filter(|s| !s.is_empty()) {
            None => "act_reversible",
            Some(level) => level,
        };
        let may_act = matches!(effective, "act_reversible" | "act_full");
        if !may_act {
            return Ok(DecisionDisposition {
                authority: "escalation".to_string(),
                status: "pending_user".to_string(),
                coerced_reason: Some(format!("autonomy_level={effective}")),
            });
        }
        if effective == "act_reversible" {
            let kind_norm = kind.map(str::trim).map(str::to_ascii_lowercase);
            let merge_ok = kind_norm.as_deref() == Some("merge")
                && merge_authority
                    .map(str::trim)
                    .is_some_and(|a| a.eq_ignore_ascii_case("full"));
            let irreversible = kind_norm
                .as_deref()
                .is_some_and(|k| IRREVERSIBLE_KINDS.contains(&k))
                && !merge_ok;
            if irreversible {
                return Ok(DecisionDisposition {
                    authority: "escalation".to_string(),
                    status: "pending_user".to_string(),
                    coerced_reason: Some(format!(
                        "autonomy_level=act_reversible kind={}",
                        kind.unwrap_or_default().trim()
                    )),
                });
            }
        }
    }
    Ok(DecisionDisposition {
        authority: authority.to_string(),
        status: status.to_string(),
        coerced_reason: None,
    })
}

/// `POST /api/projects/:slug/decision` — add to the decision ledger: an owner
/// escalation, or (grant permitting) a declared autonomous act.
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
    let grant = state.projects.get_grant(&slug).map_err(store_err)?;
    let disposition = resolve_decision_disposition_for_grant(
        grant.as_ref().and_then(|g| g.autonomy_level.as_deref()),
        grant.as_ref().and_then(|g| g.merge_authority.as_deref()),
        req.authority.as_deref(),
        req.status.as_deref(),
        req.kind.as_deref(),
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let decision = super::projects_store::NewDecision {
        question: req.question.trim().to_string(),
        rationale: req.rationale.clone(),
        kind: req
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string),
        authority: disposition.authority,
        status: disposition.status,
        evidence: req.evidence.clone(),
    };
    let at = state
        .projects
        .record_decision(&slug, &decision)
        .map_err(store_err)?;
    let open = state.projects.open_decisions(&slug).map_err(store_err)?;
    Ok(Json(serde_json::json!({
        "at": at,
        "authority": decision.authority,
        "status": decision.status,
        "coerced": disposition.coerced_reason.is_some(),
        "coerced_reason": disposition.coerced_reason,
        "open_decisions": open,
    })))
}

/// Extract the first GitHub PR link from free text (result digests and notes
/// routinely quote one). Read-time extraction, nothing stored.
pub(crate) fn extract_pr_url(text: &str) -> Option<String> {
    // Scan every GitHub URL in the text, not just the first: digests routinely
    // mention the repo before the PR ("Repo https://github.com/x/y; opened
    // https://github.com/x/y/pull/48"), and locking onto the first hit would
    // reject the repo link and never reach the PR.
    text.match_indices("https://github.com/")
        .find_map(|(start, _)| {
            let candidate = &text[start..];
            let end = candidate
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | ',')
                })
                .unwrap_or(candidate.len());
            let url = candidate[..end].trim_end_matches(['.', ';', ':']);
            // Only PR links qualify: /owner/repo/pull/N
            let path: Vec<&str> = url
                .strip_prefix("https://github.com/")?
                .split('/')
                .collect();
            match path.as_slice() {
                [_, _, kind, number, ..]
                    if *kind == "pull" && number.chars().all(|c| c.is_ascii_digit()) =>
                {
                    Some(url.to_string())
                }
                _ => None,
            }
        })
}

/// `#N` hashes and GitHub PR URLs a later merge announcement can match
/// against pending ledger rows. `#1` is kept distinct from `#10`.
pub(crate) fn extract_pr_needles(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(url) = extract_pr_url(text) {
        out.push(url.clone());
        if let Some(number) = url.rsplit('/').next() {
            let hash = format!("#{number}");
            if !out.contains(&hash) {
                out.push(hash);
            }
        }
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let hash = format!("#{}", &text[start..end]);
            if !out.contains(&hash) {
                out.push(hash);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn delivery_looks_merged(headline: &str) -> bool {
    let lower = headline.to_ascii_lowercase();
    lower.contains("merged") || lower.contains("mergé")
}

fn merge_announcement_needles(delivery: &DeliveryUpdate, recorded_decided: bool) -> Vec<String> {
    if !recorded_decided && !delivery_looks_merged(&delivery.headline) {
        return Vec::new();
    }
    let mut blobs: Vec<&str> = vec![delivery.headline.as_str()];
    if let Some(body) = delivery.body.as_deref() {
        blobs.push(body);
    }
    if let Some(decision) = delivery.decision.as_ref() {
        blobs.push(decision.question.as_str());
        if let Some(rationale) = decision.rationale.as_deref() {
            blobs.push(rationale);
        }
        if let Some(url) = decision
            .evidence
            .as_ref()
            .and_then(|value| value.get("pr_url"))
            .and_then(|value| value.as_str())
        {
            blobs.push(url);
        }
    }
    let mut needles = Vec::new();
    for blob in blobs {
        for needle in extract_pr_needles(blob) {
            if !needles.contains(&needle) {
                needles.push(needle);
            }
        }
    }
    needles
}

/// `GET /api/projects/:slug/tasks` — the project's roadmap: every board task
/// planned under a boss mission of this project family, in planning order,
/// with the per-item detail (digest, PR link, worker mission) the drawer's
/// checklist expands into.
pub async fn project_tasks(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    let tasks = state
        .control
        .collect_project_board_tasks(&slug)
        .await
        .map_err(store_err)?;
    let mut done = 0usize;
    let mut running = 0usize;
    let mut failed = 0usize;
    let mut rows: Vec<serde_json::Value> = tasks
        .iter()
        .map(|task| {
            let status = task.status.to_string();
            match status.as_str() {
                "accepted" => done += 1,
                "running" | "settled" => running += 1,
                "failed" => failed += 1,
                _ => {}
            }
            let pr_url = task
                .result_digest
                .as_deref()
                .and_then(extract_pr_url)
                .or_else(|| task.notes.as_deref().and_then(extract_pr_url));
            serde_json::json!({
                "id": task.id,
                "task_key": task.task_key,
                "title": task.title,
                "status": status,
                "outcome": task.outcome,
                "depends_on": task.depends_on,
                "acceptance_criteria": task.acceptance_criteria,
                "result_digest": task.result_digest,
                "pr_url": pr_url,
                "worker_mission_id": task.worker_mission_id,
                "boss_mission_id": task.boss_mission_id,
                "attempts": task.attempts,
                "updated_at": task.updated_at,
            })
        })
        .collect();
    // Chat-planned proposals ride the same list as `status: "proposed"`. A
    // board task under the same key supersedes its proposal — the plan became
    // real work, so the proposal row disappears from the read.
    let planned: std::collections::HashSet<&str> =
        tasks.iter().map(|task| task.task_key.as_str()).collect();
    let proposals = state
        .projects
        .list_open_proposals(&slug)
        .map_err(store_err)?;
    rows.extend(
        proposals
            .iter()
            .filter(|proposal| !planned.contains(proposal.task_key.as_str()))
            .map(|proposal| {
                serde_json::json!({
                    "id": null,
                    "task_key": proposal.task_key,
                    "title": proposal.title,
                    "prompt": proposal.prompt,
                    "status": "proposed",
                    "depends_on": proposal.depends_on,
                    "acceptance_criteria": proposal.acceptance_criteria,
                    "updated_at": proposal.updated_at,
                })
            }),
    );
    Ok(Json(serde_json::json!({
        "slug": slug,
        "tasks": rows,
        "summary": {
            "total": rows.len(),
            "done": done,
            "running": running,
            "failed": failed,
        },
    })))
}

#[derive(Debug, Deserialize)]
pub struct ProposalInput {
    pub task_key: String,
    pub title: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlanTasksRequest {
    pub tasks: Vec<ProposalInput>,
}

/// `POST /api/projects/:slug/tasks` — plan roadmap items from chat. Proposals
/// only: real board tasks stay writable solely by their boss mission, so a
/// conversation can shape the plan without reaching into a running board.
pub async fn plan_project_tasks(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<PlanTasksRequest>,
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
    if req.tasks.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "tasks is required".to_string()));
    }
    let mut proposals = Vec::with_capacity(req.tasks.len());
    for task in &req.tasks {
        let task_key = task.task_key.trim();
        if !is_plain_key(task_key) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("invalid task_key '{}'", task.task_key),
            ));
        }
        if task.title.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("task '{task_key}' needs a title"),
            ));
        }
        proposals.push(crate::api::projects_store::NewProposal {
            task_key: task_key.to_string(),
            title: task.title.trim().to_string(),
            prompt: task.prompt.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            depends_on: task.depends_on.clone(),
        });
    }
    state
        .projects
        .upsert_proposals(&slug, &proposals)
        .map_err(store_err)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "proposed": proposals.len() }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpdateProposalRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
}

/// A proposal whose key a boss mission has planned as a real board task is
/// *adopted*: the roadmap read hides it, and editing/cancelling the hidden row
/// would silently succeed while changing nothing the caller can see. Surface
/// that as a conflict instead.
async fn assert_not_adopted(
    state: &Arc<AppState>,
    slug: &str,
    task_key: &str,
) -> Result<(), (StatusCode, String)> {
    let tasks = state
        .control
        .collect_project_board_tasks(slug)
        .await
        .map_err(store_err)?;
    if tasks.iter().any(|task| task.task_key == task_key) {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "proposal '{task_key}' was adopted as a board task — steer the boss mission instead"
            ),
        ));
    }
    Ok(())
}

/// `PATCH /api/projects/:slug/tasks/:task_key` — edit an open proposal.
/// 404 covers "never proposed" and "cancelled"; an adopted key is 409 — once a
/// board task owns the key, edits belong to the boss mission's flow.
pub async fn update_project_task(
    State(state): State<Arc<AppState>>,
    AxumPath((slug, task_key)): AxumPath<(String, String)>,
    Json(req): Json<UpdateProposalRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) || !is_plain_key(&task_key) {
        return Err(bad_slug());
    }
    assert_not_adopted(&state, &slug, &task_key).await?;
    let updated = state
        .projects
        .update_proposal(
            &slug,
            &task_key,
            req.title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty()),
            req.prompt.as_deref(),
            req.acceptance_criteria.as_deref(),
            req.depends_on.as_deref(),
        )
        .map_err(store_err)?;
    if !updated {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no open proposal '{task_key}' for '{slug}'"),
        ));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /api/projects/:slug/tasks/:task_key` — cancel an open proposal.
pub async fn cancel_project_task(
    State(state): State<Arc<AppState>>,
    AxumPath((slug, task_key)): AxumPath<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) || !is_plain_key(&task_key) {
        return Err(bad_slug());
    }
    assert_not_adopted(&state, &slug, &task_key).await?;
    let cancelled = state
        .projects
        .cancel_proposal(&slug, &task_key)
        .map_err(store_err)?;
    if !cancelled {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no open proposal '{task_key}' for '{slug}'"),
        ));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct AnswerDecisionRequest {
    /// The decision's `at` key.
    pub at: String,
    pub answer: String,
}

/// `POST /api/projects/:slug/decision/answer` — resolve a pending escalation.
/// Delivery of the answer into the control conversation is the caller's job
/// (the board relay knows how to address the bound Hermes session); this
/// endpoint owns only the ledger transition.
pub async fn answer_project_decision(
    State(state): State<Arc<AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<AnswerDecisionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !is_plain_key(&slug) {
        return Err(bad_slug());
    }
    if req.answer.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "answer is required".to_string()));
    }
    let answered = state
        .projects
        .answer_decision(&slug, &req.at, req.answer.trim())
        .map_err(store_err)?;
    if !answered {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no pending decision at '{}' for '{slug}'", req.at),
        ));
    }
    let open = state.projects.open_decisions(&slug).map_err(store_err)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "open_decisions": open }),
    ))
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/overview", get(projects_overview))
        .route("/", axum::routing::put(upsert_project))
        .route("/:slug", get(get_project))
        .route("/:slug/state", get(project_state))
        .route("/:slug/tasks", get(project_tasks).post(plan_project_tasks))
        .route(
            "/:slug/tasks/:task_key",
            axum::routing::patch(update_project_task).delete(cancel_project_task),
        )
        .route("/:slug/updates", get(project_updates))
        .route("/:slug/action", axum::routing::post(project_action))
        .route("/:slug/rename", axum::routing::post(rename_project))
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
            "/:slug/decision/answer",
            axum::routing::post(answer_project_decision),
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

/// The Hermes cron scheduler's jobs file, next to `state.db` in the Hermes
/// home (`<home>/cron/jobs.json`). Overridable for tests and non-standard
/// layouts via `HERMES_CRON_JOBS`.
fn hermes_cron_jobs_path() -> Option<PathBuf> {
    std::env::var("HERMES_CRON_JOBS")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            hermes_state_db().and_then(|db| db.parent().map(|home| home.join("cron/jobs.json")))
        })
        .filter(|path| path.is_file())
}

/// Scheduler-side controller heartbeats: job id → last successful run
/// (RFC3339). Read from the Hermes cron jobs file; only enabled jobs whose
/// last run succeeded count — a job erroring every tick is not a heartbeat.
/// Best-effort: a missing or malformed file yields an empty map, never an
/// error (the board must render without Hermes on disk).
fn read_controller_heartbeats(path: Option<PathBuf>) -> HashMap<String, String> {
    let Some(path) = path else {
        return HashMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return HashMap::new();
    };
    let jobs = match &value {
        serde_json::Value::Object(map) => match map.get("jobs") {
            Some(serde_json::Value::Array(jobs)) => jobs.as_slice(),
            _ => return HashMap::new(),
        },
        serde_json::Value::Array(jobs) => jobs.as_slice(),
        _ => return HashMap::new(),
    };
    jobs.iter()
        .filter_map(|job| {
            let id = job.get("id")?.as_str()?;
            if !job
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return None;
            }
            if job.get("last_status").and_then(|v| v.as_str()) != Some("ok") {
                return None;
            }
            let last_run = job.get("last_run_at")?.as_str()?;
            // Normalize to RFC3339 UTC so `finish()` parses it uniformly.
            let at = chrono::DateTime::parse_from_rfc3339(last_run).ok()?;
            Some((id.to_string(), at.with_timezone(&chrono::Utc).to_rfc3339()))
        })
        .collect()
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
    /// A `[DECISION: …]` trailer, when the delivery carried one. Ingest-only:
    /// the ledger is served through the decision endpoints, never re-emitted
    /// on delivery payloads.
    #[serde(skip)]
    decision: Option<DecisionTrailer>,
}

/// The parsed `[DECISION: {json}]` (or `[DECISION: plain question]`) trailer —
/// the MCP-less fallback for controllers to reach the decision ledger.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecisionTrailer {
    question: String,
    rationale: Option<String>,
    kind: Option<String>,
    authority: Option<String>,
    status: Option<String>,
    evidence: Option<serde_json::Value>,
}

impl DecisionTrailer {
    /// `{json}` form → full shape; anything else → a plain owner escalation.
    /// Malformed JSON (starts with `{` but does not parse, or lacks a
    /// question) is dropped rather than guessed at.
    fn parse(inner: &str) -> Option<Self> {
        let inner = inner.trim();
        if inner.is_empty() {
            return None;
        }
        if inner.starts_with('{') {
            let value: serde_json::Value = serde_json::from_str(inner).ok()?;
            let text = |key: &str| {
                value
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let question = text("question")?;
            return Some(Self {
                question,
                rationale: text("rationale"),
                kind: text("kind"),
                authority: text("authority"),
                status: text("status"),
                evidence: value.get("evidence").filter(|v| v.is_object()).cloned(),
            });
        }
        Some(Self {
            question: inner.to_string(),
            rationale: None,
            kind: None,
            authority: None,
            status: None,
            evidence: None,
        })
    }
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
    /// When the linked controller job last ran successfully (from the Hermes
    /// cron scheduler), regardless of whether it delivered anything. A
    /// controller that answers `[SILENT]` for hours is quiet, not dead — this
    /// is the signal that lets the board tell the two apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_heartbeat_at: Option<String>,
    /// The roster record's controller-reported mode (`active` / `blocked` /
    /// `paused`), surfaced directly on the row. Also still rides on
    /// `latest_update.mode` for compatibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    /// Honesty read-model — derived health axes, computed read-only in
    /// `finish()` so the board can show "active but its engine is gone" instead
    /// of a lying `active`. Absent (None) when the project makes no activity
    /// claim, so a dormant project stays quiet.
    ///
    /// `controller_health`: healthy | stale | missing. A project that claims to
    /// be active but carries no `controller_cron_id` is `missing`; one whose
    /// controller has not signalled within the stale window is `stale`.
    #[serde(skip_serializing_if = "Option::is_none")]
    controller_health: Option<&'static str>,
    /// `delivery_health`: reaching_user | misrouted | dropped. Whether the
    /// controller's output actually reaches a durable conversation, or lands in
    /// a throwaway per-tick session (the "engine runs but nobody receives" blind
    /// spot). Coarse in P0 (binding present vs guessed session); refined in P2.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_health: Option<&'static str>,
    /// `progress_state`: working | waiting_external | blocked. What the project
    /// is actually doing, separate from the operator's desired state.
    #[serde(skip_serializing_if = "Option::is_none")]
    progress_state: Option<&'static str>,
    tracker: Option<TrackerInfo>,
    missions: Vec<MissionChip>,
    latest_update: Option<DeliveryUpdate>,
    updates_count: usize,
    /// The grant's normalized autonomy level (observe | propose |
    /// act_reversible | act_full), surfaced on the row so the card can show it
    /// without a detail fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    autonomy_level: Option<String>,
    /// Decisions waiting on the owner (`status = pending_user`) — the card
    /// badge and a standing attention reason.
    pending_decisions: u32,
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
    let autonomy_levels = state
        .projects
        .autonomy_levels()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let pending_decisions = state
        .projects
        .pending_decision_counts()
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
        // An alias's tracker file (`verity.md`, `verity-roadmap.md`) folds onto
        // the canonical row too — otherwise a markdown file alone re-forks the
        // phantom card the roster/mission loops just collapsed. The canonical's
        // own tracker wins; an alias tracker only fills the gap.
        let key = resolve_alias(&aliases, &tracker.slug);
        if deleted(&key) {
            continue;
        }
        let is_canonical = key == tracker.slug;
        let builder = rows
            .entry(key.clone())
            .or_insert_with(|| ProjectRowBuilder::new(key.clone()));
        if is_canonical || builder.tracker.is_none() {
            builder.tracker = Some(tracker);
        }
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
        // Fold an alias record onto its canonical row, the same way tagged
        // missions are resolved above — otherwise every alias slug (`lido-audit`
        // → `verity-lido`, `verity`/`verity-roadmap` → `verity-core`, …) forks a
        // phantom card next to the real one. The canonical record is
        // authoritative; an alias record only fills gaps so a merged card never
        // shows the alias's stale title/mode over the canonical's.
        let key = resolve_alias(&aliases, &record.slug);
        if deleted(&key) {
            continue;
        }
        let is_canonical = key == record.slug;
        if is_canonical || !record_signals.contains_key(&key) {
            record_signals.insert(key.clone(), (record.mode.clone(), record.blocker.clone()));
        }
        let builder = rows
            .entry(key.clone())
            .or_insert_with(|| ProjectRowBuilder::new(key.clone()));
        if is_canonical {
            builder.title = record.title;
            builder.next_action = record.next_action;
            builder.mode = record.mode;
            builder.controller_cron_id = record.controller_cron_id;
        } else {
            builder.title = builder.title.take().or(record.title);
            builder.next_action = builder.next_action.take().or(record.next_action);
            builder.mode = builder.mode.take().or(record.mode);
            builder.controller_cron_id = builder
                .controller_cron_id
                .take()
                .or(record.controller_cron_id);
        }
    }
    // The latest ingested state per project becomes the row's latest_update —
    // same serialized shape the delivery scan used to produce, now read back
    // from the store the ingestor maintains.
    //
    // Collapse alias slugs onto their canonical FIRST, keeping only the newest
    // state per canonical (last_seen_at is an ISO8601 string, so it sorts
    // chronologically). `attach_store_update` is last-writer-wins, so without
    // this a stale alias state (e.g. `lido-srv3` @ 08-03) clobbers the
    // canonical's fresh one (`verity-lido` @ 08-12) and the card's
    // latest_update goes backwards in time — which reads as "no updates".
    let mut newest_state: HashMap<String, (&String, &super::projects_store::ProjectState)> =
        HashMap::new();
    for (slug, project_state) in &latest_states {
        let key = resolve_alias(&aliases, slug);
        if deleted(&key) {
            continue;
        }
        match newest_state.get(&key) {
            Some((_, existing)) if existing.last_seen_at >= project_state.last_seen_at => {}
            _ => {
                newest_state.insert(key, (slug, project_state));
            }
        }
    }
    for (key, (slug, project_state)) in newest_state {
        let (mode, blocker) = record_signals
            .get(&key)
            .or_else(|| record_signals.get(slug))
            .cloned()
            .unwrap_or_default();
        let update = store_update(&key, project_state, mode, blocker);
        let total = update_totals.get(slug).copied().unwrap_or(0);
        rows.entry(key.clone())
            .or_insert_with(|| ProjectRowBuilder::new(key.clone()))
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
            decision: None,
        })
        .collect();

    // Grant levels and pending-decision counts are keyed by the roster slug the
    // controller wrote them under; fold them onto canonical rows like every
    // other source (an alias never hides, only fills).
    for (slug, level) in &autonomy_levels {
        let key = resolve_alias(&aliases, slug);
        if let Some(builder) = rows.get_mut(&key) {
            if builder.autonomy_level.is_none() || key == *slug {
                builder.autonomy_level = Some(level.clone());
            }
        }
    }
    for (slug, count) in &pending_decisions {
        let key = resolve_alias(&aliases, slug);
        if let Some(builder) = rows.get_mut(&key) {
            builder.pending_decisions += count;
        }
    }

    // Scheduler-side heartbeats, resolved once per request: a controller that
    // ran successfully but delivered nothing ([SILENT] ticks) still proves it
    // is alive.
    let heartbeats = read_controller_heartbeats(hermes_cron_jobs_path());
    let mut projects: Vec<ProjectRow> = rows
        .into_values()
        .map(|mut builder| {
            builder.controller_heartbeat_at = builder
                .controller_cron_id
                .as_ref()
                .and_then(|id| heartbeats.get(id))
                .cloned();
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
        decision: None,
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

/// Roster/CTRL mode is the default; live work and parked decisions win.
/// `paused` is never overridden — the operator (or controller) parked it.
pub(crate) fn honest_controller_mode(
    store_mode: Option<&str>,
    has_live_mission: bool,
    awaiting_user: bool,
    pending_decisions: u32,
) -> Option<String> {
    let base = store_mode
        .map(|mode| mode.split_once(':').map_or(mode, |(head, _)| head))
        .unwrap_or("");
    if base.eq_ignore_ascii_case("paused") {
        return store_mode.map(str::to_string);
    }
    if has_live_mission && !awaiting_user {
        return Some("active".to_string());
    }
    if pending_decisions > 0 {
        return Some("blocked:decision".to_string());
    }
    store_mode.map(str::to_string)
}

struct ProjectRowBuilder {
    slug: String,
    /// Roster title/next-action, attached when the slug has a roster record.
    title: Option<String>,
    next_action: Option<String>,
    /// Roster mode + controller link, attached alongside title/next_action.
    mode: Option<String>,
    controller_cron_id: Option<String>,
    /// Last successful run of the linked controller job (scheduler-side
    /// heartbeat), resolved by the handler from the Hermes cron jobs file.
    controller_heartbeat_at: Option<String>,
    tracker: Option<TrackerInfo>,
    missions: Vec<MissionChip>,
    /// Health inputs, accumulated alongside the display chips.
    health_inputs: Vec<OwnedHealthInput>,
    latest_update: Option<DeliveryUpdate>,
    /// How many consecutive deliveries reported the latest state — the stall
    /// signal's input, read from the store's observation count.
    latest_observations: u32,
    updates_count: usize,
    autonomy_level: Option<String>,
    pending_decisions: u32,
}

impl ProjectRowBuilder {
    fn new(slug: String) -> Self {
        Self {
            slug,
            title: None,
            next_action: None,
            mode: None,
            controller_cron_id: None,
            controller_heartbeat_at: None,
            tracker: None,
            missions: Vec::new(),
            health_inputs: Vec::new(),
            latest_update: None,
            latest_observations: 0,
            updates_count: 0,
            autonomy_level: None,
            pending_decisions: 0,
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
        let has_live_mission = self
            .missions
            .iter()
            .any(|chip| !chip.status.is_terminal() && chip.status != MissionStatus::Acknowledged);
        // Live work and parked decisions beat the last CTRL trailer. A
        // writer already running is `active` even if the last cron said
        // `blocked:cannot-merge`; a pending question with no writer is
        // `blocked:decision`. Operator `paused` is never overridden.
        self.mode = honest_controller_mode(
            self.mode.as_deref().or_else(|| {
                self.latest_update
                    .as_ref()
                    .and_then(|update| update.mode.as_deref())
            }),
            has_live_mission,
            awaiting_user > 0,
            self.pending_decisions,
        );
        if awaiting_user > 0 {
            attention.push(match awaiting_user {
                1 => "1 mission awaiting user input".to_string(),
                count => format!("{count} missions awaiting user input"),
            });
        }
        // Same rule for ledger escalations: a pending_user decision is a
        // question only the owner can answer, so freshness never silences it.
        if self.pending_decisions > 0 {
            attention.push(match self.pending_decisions {
                1 => "1 decision awaiting you".to_string(),
                count => format!("{count} decisions awaiting you"),
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

        // ── Honesty read-model: derive controller_health + progress_state
        //    read-only, BEFORE the bucket so a lying `active` becomes an honest
        //    `attention`. A project "claims to be active" when its controller
        //    mode or its tracker says so; only then does a missing/stale
        //    controller become worth surfacing. A dormant project (no claim, no
        //    controller link) stays quiet (axes None). delivery_health is
        //    computed after `conversation` below.
        let claims_active = mode_active || tracker_active;
        // Use the single `now` the handler stamped for the whole response (also
        // what the tests fix), NOT wall-clock — otherwise two rows in one
        // payload could disagree, and unit tests with a fixed `now` would read
        // an 8-day-old signal.
        let now_parsed = chrono::DateTime::parse_from_rfc3339(now).ok();
        let signal_within_stale_window = self
            .latest_update
            .as_ref()
            .map(|u| u.at.clone())
            .or_else(|| self.tracker.as_ref().and_then(|t| t.updated_at.clone()))
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
            .zip(now_parsed)
            .map(|(at, now)| {
                now.signed_duration_since(at) <= chrono::Duration::hours(STALE_ACTIVE_HOURS)
            })
            .unwrap_or(false);
        // Scheduler-side heartbeat: the linked job ran successfully recently,
        // even if it delivered nothing ([SILENT] ticks produce no state event).
        // A quiet controller is not a dead one.
        let heartbeat_within_stale_window = self
            .controller_heartbeat_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .zip(now_parsed)
            .map(|(at, now)| {
                now.signed_duration_since(at) <= chrono::Duration::hours(STALE_ACTIVE_HOURS)
            })
            .unwrap_or(false);
        let controller_health: Option<&'static str> =
            if claims_active || self.controller_cron_id.is_some() {
                if signal_within_stale_window || has_live_mission || heartbeat_within_stale_window {
                    // Something is clearly driving it (fresh delivery or live
                    // work) — don't cry wolf over a missing link when the engine
                    // is demonstrably running. The link mismatch is a P2 concern.
                    Some("healthy")
                } else if claims_active && self.controller_cron_id.is_none() {
                    // Active, but no recent signal, no live work, and no
                    // controller link at all: the genuine zombie.
                    Some("missing")
                } else {
                    // Has a link but has gone quiet past the stale window.
                    Some("stale")
                }
            } else {
                None
            };
        // The zombie the board used to render as a healthy `active`: an active
        // project whose engine link is gone. Surface it as attention so the
        // bucket stops lying. (`stale` is already covered by the tracker block
        // above; the field alone carries it without forcing attention.)
        if controller_health == Some("missing") {
            attention.push("active project has no controller".to_string());
        }
        let effective_mode = self
            .mode
            .as_deref()
            .or_else(|| self.latest_update.as_ref().and_then(|u| u.mode.as_deref()));
        let progress_state: Option<&'static str> =
            if blocker_set || effective_mode == Some("blocked") {
                Some("blocked")
            } else if effective_mode == Some("active") && (has_live_mission || signal_fresh) {
                Some("working")
            } else if signal_within_stale_window && !has_live_mission && claims_active {
                Some("waiting_external")
            } else {
                None
            };

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

        // Coarse in P0: a real binding means the controller's reports have a
        // durable home; a merely-guessed session (`latest_update` fallback, a
        // throwaway per-tick cron session) means the output is not reaching a
        // stable conversation; nothing at all while active means it is dropped.
        // Refined in P2 once the route has a single authoritative owner.
        let delivery_health: Option<&'static str> =
            if claims_active || self.controller_cron_id.is_some() {
                match conversation.as_ref() {
                    None => Some("dropped"),
                    Some(c) if c.source == "latest_update" => Some("misrouted"),
                    Some(_) => Some("reaching_user"),
                }
            } else {
                None
            };

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
            controller_heartbeat_at: self.controller_heartbeat_at,
            mode: self.mode,
            controller_health,
            delivery_health,
            progress_state,
            tracker: self.tracker,
            missions: self.missions,
            latest_update: self.latest_update,
            updates_count: self.updates_count,
            autonomy_level: self.autonomy_level,
            pending_decisions: self.pending_decisions,
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

/// Fold a mission/controller project tag through `routes.json`.
///
/// An inverted alias (canonical slug pointing at a phantom name) is how
/// Coldcard reports signed `ec-defensive-research` and never reached the
/// bound session. Callers that persist a project tag must store the
/// canonical roster slug, not a nickname.
pub fn canonicalize_project_slug(slug: &str) -> String {
    canonicalize_project_slug_with(
        &hermes_projects_dir()
            .map(|dir| read_alias_map(&dir))
            .unwrap_or_default(),
        slug,
    )
}

/// Test seam: same fold as [`canonicalize_project_slug`] with an explicit map.
pub fn canonicalize_project_slug_with(aliases: &HashMap<String, String>, slug: &str) -> String {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    resolve_alias(aliases, trimmed)
}

/// Mission `project` tags that belong on this project's item view.
///
/// Creates persist the canonical slug, but historical rows and some
/// controllers still stamp an alias (`coldcard`, `ec-defensive-research`).
/// The item inventory has to gather every tag that folds onto the same
/// canonical, otherwise `get_project("coldcard")` looks empty while
/// `coldcard-rng-cracker` / `ec-defensive-research` hold the attempts.
pub fn project_tag_keys(slug: &str) -> Vec<String> {
    project_tag_keys_with(
        &hermes_projects_dir()
            .map(|dir| read_alias_map(&dir))
            .unwrap_or_default(),
        slug,
    )
}

pub fn project_tag_keys_with(aliases: &HashMap<String, String>, slug: &str) -> Vec<String> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let canonical = canonicalize_project_slug_with(aliases, trimmed);
    let mut keys = vec![canonical.clone()];
    if trimmed != canonical && !keys.iter().any(|key| key == trimmed) {
        keys.push(trimmed.to_string());
    }
    let mut extras: Vec<String> = aliases
        .keys()
        .filter(|alias| {
            canonicalize_project_slug_with(aliases, alias) == canonical
                && !keys.iter().any(|key| key == *alias)
        })
        .cloned()
        .collect();
    extras.sort();
    keys.extend(extras);
    keys
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
    /// Only used by `delete`: `keep_missions` removes project-owned state but
    /// preserves mission history; `delete_missions` also deletes every mission
    /// tagged with the exact project slug.
    #[serde(default)]
    delete_mode: Option<String>,
}

/// Apply a board action to a project. Delete is a real project deletion, with
/// an explicit mission-retention policy; its board override remains as a
/// tombstone so surviving missions or tracker markdown cannot recreate it.
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
    let mut deleted_mission_ids = Vec::new();
    let mut project_record_deleted = false;
    match request.action.as_str() {
        "pause" => {
            overrides.insert(slug.clone(), "paused".to_string());
        }
        "archive" => {
            overrides.insert(slug.clone(), "archived".to_string());
        }
        "delete" => {
            match request.delete_mode.as_deref().unwrap_or("keep_missions") {
                "keep_missions" => {}
                "delete_missions" => {
                    deleted_mission_ids = state
                        .control
                        .delete_project_missions(&slug)
                        .await
                        .map_err(|error| (StatusCode::CONFLICT, error))?;
                }
                other => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!(
                            "unknown delete_mode '{other}'; expected keep_missions or delete_missions"
                        ),
                    ));
                }
            }
            project_record_deleted = state.projects.delete_project(&slug).map_err(store_err)?;
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
        "project_record_deleted": project_record_deleted,
        "deleted_mission_ids": deleted_mission_ids,
        "deleted_mission_count": deleted_mission_ids.len(),
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
            || trimmed.starts_with("[DECISION:")
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
    // `[DECISION: {json}]` / `[DECISION: plain question]` — the ledger's
    // trailer fallback. Unlike the state/ctrl trailers (which only route and
    // describe), this one CREATES a durable ledger row, so it is only honored
    // inside the trailing control block: consecutive trailer/empty lines at
    // the end of the message. A `[DECISION:` line quoted mid-prose (a
    // controller echoing its own instructions) never reaches the ledger.
    let decision = content
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty() || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        })
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("[DECISION:")
                .and_then(|rest| rest.strip_suffix(']'))
        })
        .and_then(DecisionTrailer::parse);
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
        decision,
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

    /// A rename flattens every alias that pointed at the old slug and adds the
    /// forwarding entry — `resolve_alias` is single-hop, so `x → old → new`
    /// would dead-end at a slug that no longer exists.
    #[test]
    fn rename_alias_rewrite_flattens_chains_and_adds_forwarding() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("routes.json"),
            r#"{"coldcard": "old-name", "coldcard-rng": "old-name", "lido": "verity-lido"}"#,
        )
        .expect("seed");

        let flattened =
            rewrite_aliases_for_rename(dir.path(), "old-name", "new-name").expect("rewrite");
        assert_eq!(flattened, 2);

        let aliases = read_alias_map(dir.path());
        assert_eq!(
            aliases.get("coldcard").map(String::as_str),
            Some("new-name")
        );
        assert_eq!(
            aliases.get("coldcard-rng").map(String::as_str),
            Some("new-name")
        );
        assert_eq!(
            aliases.get("old-name").map(String::as_str),
            Some("new-name")
        );
        assert_eq!(aliases.get("lido").map(String::as_str), Some("verity-lido"));
        // Single-hop resolution now lands every historical key on the new slug.
        assert_eq!(resolve_alias(&aliases, "coldcard"), "new-name");
        assert_eq!(resolve_alias(&aliases, "old-name"), "new-name");
    }

    /// With no routes.json yet, a rename creates one containing only the
    /// forwarding entry.
    #[test]
    fn rename_alias_rewrite_creates_the_map_when_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flattened =
            rewrite_aliases_for_rename(dir.path(), "old-name", "new-name").expect("rewrite");
        assert_eq!(flattened, 0);
        let aliases = read_alias_map(dir.path());
        assert_eq!(
            aliases.get("old-name").map(String::as_str),
            Some("new-name")
        );
    }

    #[test]
    fn resolve_alias_folds_known_keys_and_passes_through_the_rest() {
        let mut aliases = HashMap::new();
        aliases.insert("lido-audit".to_string(), "verity-lido".to_string());
        aliases.insert("verity-roadmap".to_string(), "verity-core".to_string());
        // Verity and Lido are kept distinct: an alias only ever resolves to the
        // canonical its own family declares, never across families.
        assert_eq!(resolve_alias(&aliases, "lido-audit"), "verity-lido");
        assert_eq!(resolve_alias(&aliases, "verity-roadmap"), "verity-core");
        // The canonical resolves to itself, and an unknown slug is untouched.
        assert_eq!(resolve_alias(&aliases, "verity-lido"), "verity-lido");
        assert_eq!(resolve_alias(&aliases, "sandboxed-sh"), "sandboxed-sh");
    }

    #[test]
    fn canonicalize_project_slug_folds_nicknames_and_keeps_the_canonical() {
        let mut aliases = HashMap::new();
        aliases.insert("coldcard".to_string(), "coldcard-rng-cracker".to_string());
        aliases.insert(
            "coldcard-rng".to_string(),
            "coldcard-rng-cracker".to_string(),
        );
        aliases.insert(
            "ec-defensive-research".to_string(),
            "coldcard-rng-cracker".to_string(),
        );
        assert_eq!(
            canonicalize_project_slug_with(&aliases, "coldcard"),
            "coldcard-rng-cracker"
        );
        assert_eq!(
            canonicalize_project_slug_with(&aliases, "  ec-defensive-research  "),
            "coldcard-rng-cracker"
        );
        assert_eq!(
            canonicalize_project_slug_with(&aliases, "coldcard-rng-cracker"),
            "coldcard-rng-cracker"
        );
        assert_eq!(canonicalize_project_slug_with(&aliases, "verity"), "verity");
        assert_eq!(canonicalize_project_slug_with(&aliases, "   "), "");
    }

    #[test]
    fn project_tag_keys_include_canonical_and_every_alias() {
        let mut aliases = HashMap::new();
        aliases.insert("coldcard".to_string(), "coldcard-rng-cracker".to_string());
        aliases.insert(
            "coldcard-rng".to_string(),
            "coldcard-rng-cracker".to_string(),
        );
        aliases.insert(
            "ec-defensive-research".to_string(),
            "coldcard-rng-cracker".to_string(),
        );
        aliases.insert("lido-audit".to_string(), "verity-lido".to_string());

        let from_nick = project_tag_keys_with(&aliases, "coldcard");
        assert!(from_nick.contains(&"coldcard-rng-cracker".to_string()));
        assert!(from_nick.contains(&"coldcard".to_string()));
        assert!(from_nick.contains(&"ec-defensive-research".to_string()));
        assert!(from_nick.contains(&"coldcard-rng".to_string()));
        assert!(!from_nick.contains(&"verity-lido".to_string()));

        let mut from_canonical = project_tag_keys_with(&aliases, "coldcard-rng-cracker");
        let mut from_nick_sorted = from_nick.clone();
        from_nick_sorted.sort();
        from_canonical.sort();
        assert_eq!(from_nick_sorted, from_canonical);

        assert_eq!(
            project_tag_keys_with(&aliases, "verity"),
            vec!["verity".to_string()]
        );
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
            decision: None,
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

    // ---- [DECISION:] trailer ----

    #[test]
    fn a_decision_trailer_parses_json_and_plain_forms_and_never_becomes_the_headline() {
        let content = "[Cron delivery: verity]\nReal headline\n\
            [DECISION: {\"kind\":\"merge\",\"authority\":\"granted\",\"status\":\"decided\",\
             \"question\":\"Merged verity#2213\",\"evidence\":{\"pr_url\":\"https://github.com/x/y/pull/2213\"}}]\n\
            [STATE_SIGNATURE: verity|phase|head|clean]\n";
        let parsed = parse_delivery("s", 1_754_000_000.0, content);
        assert_eq!(parsed.headline, "Real headline");
        let decision = parsed.decision.expect("decision");
        assert_eq!(decision.question, "Merged verity#2213");
        assert_eq!(decision.authority.as_deref(), Some("granted"));
        assert_eq!(decision.kind.as_deref(), Some("merge"));
        assert_eq!(
            decision.evidence.unwrap()["pr_url"],
            "https://github.com/x/y/pull/2213"
        );

        // Plain-text form = owner escalation (fields defaulted downstream).
        let plain = parse_delivery(
            "s",
            1.0,
            "[Cron delivery: x]\nTitle\n[DECISION: Ship v2 now or wait for audit?]\n",
        );
        let decision = plain.decision.expect("decision");
        assert_eq!(decision.question, "Ship v2 now or wait for audit?");
        assert_eq!(decision.authority, None);

        // A trailer alone must not become the headline (tag title fallback).
        let only = parse_delivery("s", 1.0, "[Cron delivery: x]\n[DECISION: Question?]\n");
        assert_eq!(only.headline, "x");

        // A [DECISION:] line quoted mid-prose (followed by ordinary text) is
        // an example, not a trailer — it must never reach the ledger.
        let quoted = parse_delivery(
            "s",
            1.0,
            "[Cron delivery: x]\nTitle\n[DECISION: use this format]\nas documented, \
             append the trailer to your report.\n[STATE_SIGNATURE: verity|phase]\n",
        );
        assert_eq!(quoted.decision, None, "mid-prose DECISION must be ignored");

        // Malformed JSON is dropped, not guessed at.
        assert_eq!(
            parse_delivery("s", 1.0, "t\n[DECISION: {\"kind\":]\n").decision,
            None
        );
        // JSON without a question is dropped too.
        assert_eq!(
            parse_delivery("s", 1.0, "t\n[DECISION: {\"kind\":\"merge\"}]\n").decision,
            None
        );
    }

    #[test]
    fn ingested_decision_trailers_are_coerced_and_idempotent() {
        let store = super::super::projects_store::ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("verity", None, None, None, None)
            .expect("seed");
        // Unset grant defaults to act_reversible, so a *merge* (irreversible)
        // still lands as a pending owner escalation.
        let content = "[Cron delivery: verity]\nHeadline\n\
            [DECISION: {\"kind\":\"merge\",\"authority\":\"granted\",\"status\":\"decided\",\"question\":\"Merged #1\"}]\n\
            [STATE_SIGNATURE: verity|phase|head]\n";
        let delivery = parse_delivery("s1", 1_754_000_000.0, content);
        let aliases = HashMap::new();
        let overrides = HashMap::new();
        ingest_deliveries(&store, &aliases, &overrides, vec![delivery.clone()]);
        let open = store.open_decisions("verity").expect("open");
        assert_eq!(open.len(), 1, "coerced to escalation");
        assert_eq!(open[0].status.as_deref(), Some("pending_user"));

        // Replaying the same delivery window must not duplicate the row —
        // and must not reopen it once answered.
        ingest_deliveries(&store, &aliases, &overrides, vec![delivery.clone()]);
        assert_eq!(store.open_decisions("verity").expect("open").len(), 1);
        let at = store.open_decisions("verity").expect("open")[0].at.clone();
        assert!(store.answer_decision("verity", &at, "ok").expect("answer"));
        ingest_deliveries(&store, &aliases, &overrides, vec![delivery]);
        assert!(
            store.open_decisions("verity").expect("open").is_empty(),
            "an answered decision must stay answered across ingest replays"
        );

        // With an acting grant, the same trailer records an autonomous act.
        store
            .set_grant(
                "verity",
                None,
                None,
                None,
                None,
                None,
                None,
                Some("act_full"),
            )
            .expect("grant");
        let content2 = content.replace("Merged #1", "Merged #2");
        let delivery2 = parse_delivery("s1", 1_754_000_100.0, &content2);
        ingest_deliveries(&store, &aliases, &overrides, vec![delivery2]);
        assert!(store.open_decisions("verity").expect("open").is_empty());
        let recent = store.recent_decisions("verity", 10).expect("recent");
        assert!(recent
            .iter()
            .any(|d| d.question == "Merged #2" && d.status.as_deref() == Some("decided")));
    }

    #[test]
    fn pr_links_are_extracted_from_digests_and_nothing_else() {
        assert_eq!(
            extract_pr_url("Opened https://github.com/lfglabs-dev/verity/pull/2213 for review."),
            Some("https://github.com/lfglabs-dev/verity/pull/2213".to_string())
        );
        assert_eq!(
            extract_pr_url("(see https://github.com/x/y/pull/48)"),
            Some("https://github.com/x/y/pull/48".to_string())
        );
        // A repo link BEFORE the PR link must not shadow it.
        assert_eq!(
            extract_pr_url("Repo https://github.com/x/y; opened https://github.com/x/y/pull/48"),
            Some("https://github.com/x/y/pull/48".to_string())
        );
        // Repo links, issues, and bare mentions are not PR links.
        assert_eq!(extract_pr_url("https://github.com/x/y"), None);
        assert_eq!(extract_pr_url("https://github.com/x/y/issues/12"), None);
        assert_eq!(extract_pr_url("no links here"), None);
    }

    #[test]
    fn pr_needles_include_hashes_and_urls_without_prefix_collisions() {
        let needles = extract_pr_needles(
            "Certify #2240 merged via https://github.com/lfglabs-dev/verity/pull/2240",
        );
        assert!(needles.contains(&"https://github.com/lfglabs-dev/verity/pull/2240".to_string()));
        assert!(needles.contains(&"#2240".to_string()));
        assert!(!needles.iter().any(|n| n == "#224" || n == "#2"));
    }

    #[test]
    fn a_merge_headline_closes_older_pending_questions_for_that_pr() {
        let store = super::super::projects_store::ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("lido", None, None, None, None)
            .expect("seed");
        store
            .record_decision(
                "lido",
                &super::super::projects_store::NewDecision {
                    question: "merge #66?".to_string(),
                    rationale: Some("blocks A.2".to_string()),
                    kind: Some("merge".to_string()),
                    authority: "escalation".to_string(),
                    status: "pending_user".to_string(),
                    evidence: None,
                },
            )
            .expect("pending");
        let aliases = HashMap::new();
        let overrides = HashMap::new();
        let delivery = parse_delivery(
            "s",
            1_754_000_500.0,
            "[Cron delivery: lido]\n#66 MERGED\n[STATE_SIGNATURE: lido|done|02a0da1]\n",
        );
        ingest_deliveries(&store, &aliases, &overrides, vec![delivery]);
        assert!(
            store.open_decisions("lido").expect("open").is_empty(),
            "the merge headline must retire the older merge #66? question"
        );
    }

    #[test]
    fn a_coerced_merge_trailer_does_not_close_itself() {
        let store = super::super::projects_store::ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("verity", None, None, None, None)
            .expect("seed");
        let content = "[Cron delivery: verity]\nHeadline\n\
            [DECISION: {\"kind\":\"merge\",\"authority\":\"granted\",\"status\":\"decided\",\"question\":\"Merged #2333\"}]\n\
            [STATE_SIGNATURE: verity|phase|head]\n";
        let delivery = parse_delivery("s1", 1_754_000_000.0, content);
        ingest_deliveries(&store, &HashMap::new(), &HashMap::new(), vec![delivery]);
        let open = store.open_decisions("verity").expect("open");
        assert_eq!(open.len(), 1, "coerced merge stays pending");
        assert_eq!(open[0].question, "Merged #2333");
    }

    #[test]
    fn relaunch_and_stale_lease_deliveries_are_folded_silent() {
        let store = super::super::projects_store::ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("verity-lido", None, None, None, None)
            .expect("seed lido");
        store
            .upsert_project("verity-core", None, None, None, None)
            .expect("seed verity");
        let aliases = HashMap::new();
        let overrides = HashMap::new();
        let first = parse_delivery(
            "s",
            1_754_000_000.0,
            "[Cron delivery: verity-lido]\nLido closure-v2 — re-pin started\n\
             [CTRL: verity-lido | mode=active | wait=0 | next=repin]\n\
             [STATE_SIGNATURE: verity-lido|closure|04729a9|none|repin]\n",
        );
        ingest_deliveries(&store, &aliases, &overrides, vec![first]);
        let relaunch = parse_delivery(
            "s",
            1_754_000_100.0,
            "[Cron delivery: verity-lido]\nLido audit — CAMPAGNE RELANCÉE\n\
             [CTRL: verity-lido | mode=active | wait=0 | next=repin]\n\
             [STATE_SIGNATURE: verity-lido|closure|04729a9|none|repin]\n",
        );
        ingest_deliveries(&store, &aliases, &overrides, vec![relaunch]);
        let lido = &store.latest_states().expect("latest")["verity-lido"];
        assert_eq!(
            lido.headline.as_deref(),
            Some("Lido closure-v2 — re-pin started"),
            "relaunch prose must not become the card headline"
        );
        assert!(lido.observations >= 2);

        let live = HashSet::from(["verity-core".to_string()]);
        let lease = parse_delivery(
            "s",
            1_754_000_200.0,
            "[Cron delivery: verity-core]\nVerity #2332 — BLOQUÉE PAR LEASE WRITER\n\
             [CTRL: verity-core | mode=blocked:lease-writer | wait=3 | next=wait]\n\
             [STATE_SIGNATURE: verity-core|c5|#2332|lease|wait]\n",
        );
        ingest_deliveries_with_live(&store, &aliases, &overrides, vec![lease], &live);
        let verity = store
            .get_project("verity-core")
            .expect("row")
            .expect("exists");
        assert_eq!(
            verity.mode.as_deref(),
            Some("active"),
            "a live writer cannot stay blocked:lease"
        );
        assert!(
            store
                .latest_states()
                .expect("latest")
                .get("verity-core")
                .and_then(|s| s.headline.as_deref())
                .is_none(),
            "lease-writer lie must not open a headline while a writer is live"
        );
    }

    #[test]
    fn honest_mode_prefers_live_work_and_parked_decisions_over_cron_prose() {
        assert_eq!(
            honest_controller_mode(Some("blocked:cannot-merge"), true, false, 1).as_deref(),
            Some("active")
        );
        assert_eq!(
            honest_controller_mode(Some("active"), false, false, 2).as_deref(),
            Some("blocked:decision")
        );
        assert_eq!(
            honest_controller_mode(Some("paused:owner"), true, false, 1).as_deref(),
            Some("paused:owner")
        );
        assert_eq!(
            honest_controller_mode(Some("blocked:transport-cap"), false, false, 0).as_deref(),
            Some("blocked:transport-cap")
        );
    }

    #[test]
    fn a_live_writer_makes_the_card_mode_active_despite_a_blocked_ctrl() {
        let mut builder = ProjectRowBuilder::new("verity".into());
        builder.mode = Some("blocked:cannot-merge".into());
        builder.pending_decisions = 1;
        builder.missions.push(MissionChip {
            id: "abcd1234".into(),
            status: MissionStatus::Active,
            title: Some("rebase #2332".into()),
            updated_at: "2026-08-14T06:00:00Z".into(),
            github_pr: None,
        });
        let row = builder.finish(&[], None, None, "2026-08-14T06:05:00Z");
        assert_eq!(row.mode.as_deref(), Some("active"));
    }

    // ---- decision disposition (the autonomy enforcement point) ----

    #[test]
    fn an_unearned_autonomous_act_is_coerced_into_an_escalation() {
        // observe and propose deny acting. An unset grant follows the
        // controllers-policy default (act_reversible), not observe.
        for level in [Some("observe"), Some("propose")] {
            let d = resolve_decision_disposition(level, Some("granted"), Some("decided"), None)
                .expect("valid");
            assert_eq!(d.authority, "escalation");
            assert_eq!(d.status, "pending_user");
            assert!(d.coerced_reason.is_some(), "level {level:?} must coerce");
        }
        let unset = resolve_decision_disposition(None, Some("granted"), Some("decided"), None)
            .expect("valid");
        assert_eq!(unset.authority, "granted");
        assert_eq!(unset.status, "decided");
        assert!(unset.coerced_reason.is_none());
        for level in ["act_reversible", "act_full"] {
            let d =
                resolve_decision_disposition(Some(level), Some("granted"), Some("decided"), None)
                    .expect("valid");
            assert_eq!(d.authority, "granted");
            assert_eq!(d.status, "decided");
            assert!(d.coerced_reason.is_none());
        }
    }

    #[test]
    fn act_reversible_escalates_the_irreversible_kinds() {
        for kind in ["merge", "Abandon", " deploy "] {
            let d = resolve_decision_disposition(
                Some("act_reversible"),
                Some("granted"),
                Some("decided"),
                Some(kind),
            )
            .expect("valid");
            assert_eq!(d.status, "pending_user", "kind {kind:?} must escalate");
            assert!(d.coerced_reason.as_deref().unwrap_or("").contains("kind="));
        }
        // Reversible work passes at act_reversible; everything passes at act_full.
        for (level, kind) in [
            ("act_reversible", Some("dispatch")),
            ("act_reversible", None),
            ("act_full", Some("merge")),
        ] {
            let d =
                resolve_decision_disposition(Some(level), Some("granted"), Some("decided"), kind)
                    .expect("valid");
            assert_eq!(d.status, "decided", "{level}/{kind:?} must pass");
        }
    }

    #[test]
    fn merge_authority_full_lets_act_reversible_record_a_merge() {
        let denied = resolve_decision_disposition_for_grant(
            Some("act_reversible"),
            Some("review-first"),
            Some("granted"),
            Some("decided"),
            Some("merge"),
        )
        .expect("valid");
        assert_eq!(denied.status, "pending_user");

        let allowed = resolve_decision_disposition_for_grant(
            Some("act_reversible"),
            Some("full"),
            Some("granted"),
            Some("decided"),
            Some("merge"),
        )
        .expect("valid");
        assert_eq!(allowed.authority, "granted");
        assert_eq!(allowed.status, "decided");
        assert!(allowed.coerced_reason.is_none());

        // force_push stays banned even with merge_authority=full
        let force = resolve_decision_disposition_for_grant(
            Some("act_reversible"),
            Some("full"),
            Some("granted"),
            Some("decided"),
            Some("force_push"),
        )
        .expect("valid");
        assert_eq!(force.status, "pending_user");
    }

    #[test]
    fn legacy_decision_bodies_default_to_owner_escalations() {
        // The pre-ledger callers send only question+rationale: no authority,
        // no status. They must keep meaning "ask the owner".
        let d = resolve_decision_disposition(Some("act_full"), None, None, None).expect("valid");
        assert_eq!(d.authority, "escalation");
        assert_eq!(d.status, "pending_user");
        assert!(d.coerced_reason.is_none());

        assert!(resolve_decision_disposition(None, Some("sovereign"), None, None).is_err());
        assert!(resolve_decision_disposition(None, None, Some("expired"), None).is_err());
    }

    #[test]
    fn pending_decisions_are_a_standing_attention_reason() {
        let mut builder = ProjectRowBuilder::new("verity".into());
        builder.pending_decisions = 2;
        builder.autonomy_level = Some("propose".into());
        let row = builder.finish(&[], None, None, "2026-08-04T20:00:00Z");
        assert_eq!(row.bucket, "attention");
        assert_eq!(row.pending_decisions, 2);
        assert_eq!(row.autonomy_level.as_deref(), Some("propose"));
        assert!(row
            .attention_reasons
            .iter()
            .any(|r| r == "2 decisions awaiting you"));
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
            decision: None,
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

    /// A controller that runs on schedule but delivers nothing ([SILENT]
    /// ticks) is quiet, not dead: a fresh scheduler heartbeat keeps
    /// `controller_health=healthy` even when the last state event is old.
    #[test]
    fn silent_controller_with_fresh_heartbeat_is_healthy() {
        let mut builder = ProjectRowBuilder::new("lean-silicon".to_string());
        builder.mode = Some("active".to_string());
        builder.controller_cron_id = Some("job42".to_string());
        // Last delivered state is 2 days old …
        builder.attach_store_update(active_update("2026-08-02T12:00:00Z", None), 1, 5);
        // … but the job itself ran successfully 10 minutes ago.
        builder.controller_heartbeat_at = Some("2026-08-04T11:50:00+00:00".to_string());
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.controller_health, Some("healthy"));
        assert_eq!(
            row.controller_heartbeat_at.as_deref(),
            Some("2026-08-04T11:50:00+00:00")
        );
    }

    /// Without a heartbeat the same silent controller is `stale` — the field
    /// is what separates the two regimes.
    #[test]
    fn silent_controller_without_heartbeat_stays_stale() {
        let mut builder = ProjectRowBuilder::new("lean-silicon".to_string());
        builder.mode = Some("active".to_string());
        builder.controller_cron_id = Some("job42".to_string());
        builder.attach_store_update(active_update("2026-08-02T12:00:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.controller_health, Some("stale"));
    }

    /// jobs.json → heartbeat map: only enabled jobs whose last run succeeded
    /// count, and timestamps normalize to UTC.
    #[test]
    fn heartbeats_read_only_successful_enabled_jobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("jobs.json");
        std::fs::write(
            &path,
            r#"{"jobs": [
                {"id": "ok1", "enabled": true, "last_status": "ok", "last_run_at": "2026-08-13T12:33:20.283248+02:00"},
                {"id": "err1", "enabled": true, "last_status": "error", "last_run_at": "2026-08-13T12:14:43+02:00"},
                {"id": "off1", "enabled": false, "last_status": "ok", "last_run_at": "2026-08-13T12:14:43+02:00"},
                {"id": "new1", "enabled": true, "last_status": null, "last_run_at": null}
            ]}"#,
        )
        .expect("seed");
        let map = read_controller_heartbeats(Some(path));
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get("ok1").map(String::as_str),
            Some("2026-08-13T10:33:20.283248+00:00")
        );
        assert!(read_controller_heartbeats(None).is_empty());
    }

    /// Honesty read-model: an active project whose engine is gone (no fresh
    /// signal, no live mission, no controller link) is a zombie — surfaced as
    /// `controller_health=missing` and pushed to the attention bucket instead
    /// of a lying `active`.
    #[test]
    fn zombie_active_project_is_controller_missing_and_attention() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        // Stale signal (2 days old), no controller_cron_id, no live mission.
        builder.attach_store_update(active_update("2026-08-02T12:00:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.controller_health, Some("missing"));
        assert_eq!(row.bucket, "attention");
        assert!(row
            .attention_reasons
            .iter()
            .any(|r| r.contains("no controller")));
    }

    /// A fresh-signalling active controller is `healthy` even without a linked
    /// cron id — something is demonstrably driving it, so P0 does not cry wolf
    /// (the link mismatch is a P2 concern). Bucket stays `active`.
    #[test]
    fn fresh_signal_is_controller_healthy_not_missing() {
        let mut builder = ProjectRowBuilder::new("verity".to_string());
        builder.mode = Some("active".to_string());
        builder.attach_store_update(active_update("2026-08-04T11:50:00Z", None), 1, 5);
        let row = builder.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.controller_health, Some("healthy"));
        assert_eq!(row.progress_state, Some("working"));
        assert_eq!(row.bucket, "active");
    }

    /// delivery_health: a real binding means the reports have a durable home
    /// (`reaching_user`); with only a guessed per-tick session it is
    /// `misrouted` — the "engine runs but nobody receives" blind spot.
    #[test]
    fn delivery_health_tracks_binding_presence() {
        let mut misrouted = ProjectRowBuilder::new("verity".to_string());
        misrouted.mode = Some("active".to_string());
        misrouted.attach_store_update(active_update("2026-08-04T11:50:00Z", None), 1, 5);
        let row = misrouted.finish(&[], None, None, "2026-08-04T12:00:00Z");
        assert_eq!(row.delivery_health, Some("misrouted"));

        let mut bound = ProjectRowBuilder::new("verity".to_string());
        bound.mode = Some("active".to_string());
        bound.attach_store_update(active_update("2026-08-04T11:50:00Z", None), 1, 5);
        let row = bound.finish(
            &[],
            None,
            Some(ProjectConversation {
                session_id: "20260806_172248_c520d0".into(),
                source: "binding",
                bound_at: Some("2026-08-08T12:00:00Z".into()),
            }),
            "2026-08-04T12:00:00Z",
        );
        assert_eq!(row.delivery_health, Some("reaching_user"));
    }

    /// A dormant project (no activity claim, no controller link) stays quiet:
    /// all three honesty axes are absent from the payload.
    #[test]
    fn dormant_project_has_no_health_axes() {
        let row = ProjectRowBuilder::new("collatz-research".to_string()).finish(
            &[],
            None,
            None,
            "2026-08-04T12:00:00Z",
        );
        assert_eq!(row.controller_health, None);
        assert_eq!(row.delivery_health, None);
        assert_eq!(row.progress_state, None);
        let json = serde_json::to_value(&row).expect("serialize");
        assert!(json.get("controller_health").is_none());
        assert!(json.get("delivery_health").is_none());
        assert!(json.get("progress_state").is_none());
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
                decision: None,
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
                decision: None,
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
