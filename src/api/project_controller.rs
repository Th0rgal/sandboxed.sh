//! Project controller view for desktop clients (Orb).
//!
//! A project's *controller* is a Hermes cron job that wakes on a schedule,
//! reads the project's grant and roadmap, dispatches missions and reports.
//! Hermes owns the job; this module only exposes a read model and three safe
//! actions so a client can show the controller inside its project:
//!
//! - `GET  /api/projects/:slug/controller`         — job + recent runs
//! - `POST /api/projects/:slug/controller/action`  — `pause` | `resume` | `run`
//!
//! The data is read straight from the Hermes cron store that lives on the
//! same host (`<hermes home>/cron/jobs.json`, `executions.db`, and one
//! markdown file per run under `cron/output/<job_id>/`). Actions go through
//! the `hermes cron` CLI so Hermes stays the single writer of its own store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

type ApiError = (StatusCode, String);

const DEFAULT_RUNS: usize = 30;
const MAX_RUNS: usize = 100;
/// Run outputs start with the full prompt (tens of KB of skill text); the
/// controller's answer is the last section, so only the tail is read.
const OUTPUT_TAIL_BYTES: u64 = 48 * 1024;
const RESPONSE_MARKER: &str = "\n## Response\n";

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ControllerJob {
    pub id: String,
    pub name: String,
    pub schedule: Option<String>,
    pub enabled: bool,
    /// Hermes job state: `scheduled`, `running`, `paused`, …
    pub state: Option<String>,
    pub paused_reason: Option<String>,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub failure_streak: i64,
    pub deliver: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Default)]
pub struct ControllerRun {
    /// Stable id: the execution id when matched, else the output file stem.
    pub id: String,
    /// When the run produced its output (RFC 3339 when known).
    pub at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_secs: Option<i64>,
    /// `completed`, `failed`, `running`, `claimed`, `unknown`.
    pub status: Option<String>,
    /// `builtin` (schedule), `manual`, `callback`, …
    pub source: Option<String>,
    /// `delivered` or `suppressed` (nothing new to say).
    pub delivery_outcome: Option<String>,
    /// True when the tick changed nothing worth reporting.
    pub silent: bool,
    /// The controller's report, markdown, without its machine trailers.
    pub report: String,
    /// `[CTRL: …]` trailer, when present.
    pub ctrl: Option<String>,
    /// `[STATE_SIGNATURE: …]` trailer, when present.
    pub signature: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControllerView {
    pub slug: String,
    pub job: Option<ControllerJob>,
    pub runs: Vec<ControllerRun>,
}

#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: String,
}

fn internal(msg: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string())
}

/// Hermes home: `HERMES_HOME`, else the directory holding `HERMES_STATE_DB`.
fn hermes_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HERMES_HOME") {
        let path = PathBuf::from(home.trim());
        if path.join("cron").is_dir() {
            return Some(path);
        }
    }
    super::projects_overview::hermes_state_db_path()
        .and_then(|db| db.parent().map(Path::to_path_buf))
        .filter(|home| home.join("cron").is_dir())
}

fn load_jobs(home: &Path) -> Vec<serde_json::Value> {
    let Ok(raw) = std::fs::read_to_string(home.join("cron/jobs.json")) else {
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Array(jobs)) => jobs,
        Ok(serde_json::Value::Object(map)) => map
            .get("jobs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn str_field(job: &serde_json::Value, key: &str) -> Option<String> {
    job.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Pick the project's controller: the explicitly recorded cron id wins, then
/// a job bound to the project (`controller.project`), then a job delivering
/// into the project (`deliver: project:<slug>`).
fn find_job<'a>(
    jobs: &'a [serde_json::Value],
    keys: &[String],
    recorded_id: Option<&str>,
) -> Option<&'a serde_json::Value> {
    if let Some(id) = recorded_id.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(job) = jobs
            .iter()
            .find(|j| j.get("id").and_then(|v| v.as_str()) == Some(id))
        {
            return Some(job);
        }
    }
    let matches_key = |value: Option<&str>| {
        value.is_some_and(|v| keys.iter().any(|k| k.eq_ignore_ascii_case(v.trim())))
    };
    jobs.iter()
        .find(|j| {
            matches_key(
                j.get("controller")
                    .and_then(|c| c.get("project"))
                    .and_then(|v| v.as_str()),
            )
        })
        .or_else(|| {
            jobs.iter().find(|j| {
                matches_key(
                    j.get("deliver")
                        .and_then(|v| v.as_str())
                        .and_then(|d| d.strip_prefix("project:")),
                )
            })
        })
}

fn job_view(job: &serde_json::Value) -> Option<ControllerJob> {
    let id = str_field(job, "id")?;
    Some(ControllerJob {
        name: str_field(job, "name").unwrap_or_else(|| id.clone()),
        id,
        schedule: str_field(job, "schedule_display").or_else(|| {
            job.get("schedule")
                .and_then(|s| s.get("display"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }),
        enabled: job.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        state: str_field(job, "state"),
        paused_reason: str_field(job, "paused_reason"),
        next_run_at: str_field(job, "next_run_at"),
        last_run_at: str_field(job, "last_run_at"),
        last_status: str_field(job, "last_status"),
        last_error: str_field(job, "last_error"),
        failure_streak: job
            .get("failure_streak")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        deliver: str_field(job, "deliver"),
    })
}

/// Split a run's response into report text and its machine trailers.
fn parse_response(response: &str) -> (String, Option<String>, Option<String>) {
    let mut ctrl = None;
    let mut signature = None;
    let mut kept: Vec<&str> = Vec::new();
    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[CTRL:") && trimmed.ends_with(']') {
            ctrl = Some(trimmed[6..trimmed.len() - 1].trim().to_string());
        } else if trimmed.starts_with("[STATE_SIGNATURE:") && trimmed.ends_with(']') {
            signature = Some(trimmed[17..trimmed.len() - 1].trim().to_string());
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n").trim().to_string(), ctrl, signature)
}

/// The controller's answer: everything after the last `## Response` heading.
fn response_section(tail: &str) -> Option<&str> {
    tail.rfind(RESPONSE_MARKER)
        .map(|idx| &tail[idx + RESPONSE_MARKER.len()..])
}

fn read_tail(path: &Path, max: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `2026-09-19_22-49-35` → naive local timestamp.
fn parse_output_stamp(stem: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(stem, "%Y-%m-%d_%H-%M-%S").ok()
}

#[derive(Debug, Clone)]
struct Execution {
    id: String,
    source: Option<String>,
    status: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    error: Option<String>,
    delivery_outcome: Option<String>,
}

fn load_executions(home: &Path, job_id: &str, limit: usize) -> Vec<Execution> {
    let path = home.join("cron/executions.db");
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, source, status, started_at, finished_at, error, delivery_outcome \
         FROM executions WHERE job_id = ?1 ORDER BY claimed_at DESC, id DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map(rusqlite::params![job_id, limit as i64], |row| {
        Ok(Execution {
            id: row.get(0)?,
            source: row.get(1)?,
            status: row.get(2)?,
            started_at: row.get(3)?,
            finished_at: row.get(4)?,
            error: row.get(5)?,
            delivery_outcome: row.get(6)?,
        })
    });
    match rows {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

fn parse_rfc3339(value: Option<&str>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    value.and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
}

/// An output file belongs to the execution whose window contains its stamp
/// (the file is written between start and finish; allow finish + 90 s).
fn execution_matches(exec: &Execution, stamp: chrono::NaiveDateTime) -> bool {
    let Some(started) = parse_rfc3339(exec.started_at.as_deref()) else {
        return false;
    };
    let start = started.naive_local();
    let end = parse_rfc3339(exec.finished_at.as_deref())
        .map(|f| f.naive_local())
        .unwrap_or(start + chrono::Duration::hours(6))
        + chrono::Duration::seconds(90);
    stamp >= start - chrono::Duration::seconds(5) && stamp <= end
}

fn is_silent(report: &str, delivery_outcome: Option<&str>) -> bool {
    delivery_outcome == Some("suppressed")
        || report.is_empty()
        || report.trim().eq_ignore_ascii_case("[SILENT]")
}

fn build_runs(home: &Path, job_id: &str, limit: usize) -> Vec<ControllerRun> {
    let executions = load_executions(home, job_id, limit * 2 + 4);
    let mut used = vec![false; executions.len()];
    let mut runs: Vec<ControllerRun> = Vec::new();

    let dir = home.join("cron/output").join(job_id);
    let mut stems: Vec<(chrono::NaiveDateTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            let stamp = parse_output_stamp(path.file_stem()?.to_str()?)?;
            Some((stamp, path))
        })
        .collect();
    stems.sort_by(|a, b| b.0.cmp(&a.0));
    stems.truncate(limit);

    for (stamp, path) in stems {
        let tail = read_tail(&path, OUTPUT_TAIL_BYTES).unwrap_or_default();
        let (report, ctrl, signature) = response_section(&tail)
            .map(parse_response)
            .unwrap_or_default();
        let matched = executions
            .iter()
            .enumerate()
            .find(|(i, e)| !used[*i] && execution_matches(e, stamp));
        let mut run = ControllerRun {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            report,
            ctrl,
            signature,
            ..ControllerRun::default()
        };
        if let Some((idx, exec)) = matched {
            used[idx] = true;
            run.id = exec.id.clone();
            run.started_at = exec.started_at.clone();
            run.finished_at = exec.finished_at.clone();
            run.at = exec.finished_at.clone().or(exec.started_at.clone());
            run.status = exec.status.clone();
            run.source = exec.source.clone();
            run.delivery_outcome = exec.delivery_outcome.clone();
            run.error = exec.error.clone().filter(|e| !e.trim().is_empty());
            run.duration_secs = match (
                parse_rfc3339(exec.started_at.as_deref()),
                parse_rfc3339(exec.finished_at.as_deref()),
            ) {
                (Some(s), Some(f)) => Some((f - s).num_seconds().max(0)),
                _ => None,
            };
        }
        if run.at.is_none() {
            // No ledger row: fall back to the file stamp in the host's zone.
            run.at = stamp
                .and_local_timezone(chrono::Local)
                .single()
                .map(|t| t.to_rfc3339());
        }
        run.silent = is_silent(&run.report, run.delivery_outcome.as_deref());
        runs.push(run);
    }

    // A tick in flight has a ledger row but no output file yet: show it first.
    if let Some((_, exec)) = executions.iter().enumerate().find(|(i, e)| {
        !used[*i] && matches!(e.status.as_deref(), Some("running") | Some("claimed"))
    }) {
        runs.insert(
            0,
            ControllerRun {
                id: exec.id.clone(),
                at: exec.started_at.clone(),
                started_at: exec.started_at.clone(),
                status: exec.status.clone(),
                source: exec.source.clone(),
                ..ControllerRun::default()
            },
        );
    }
    runs
}

fn project_keys(slug: &str) -> Vec<String> {
    let mut keys = super::projects_overview::project_tag_keys(slug);
    if !keys.iter().any(|k| k == slug) {
        keys.push(slug.to_string());
    }
    keys
}

fn controller_view_sync(slug: &str, recorded_id: Option<String>, limit: usize) -> ControllerView {
    let Some(home) = hermes_home() else {
        return ControllerView {
            slug: slug.to_string(),
            job: None,
            runs: Vec::new(),
        };
    };
    let jobs = load_jobs(&home);
    let job = find_job(&jobs, &project_keys(slug), recorded_id.as_deref()).and_then(job_view);
    let runs = job
        .as_ref()
        .map(|j| build_runs(&home, &j.id, limit))
        .unwrap_or_default();
    ControllerView {
        slug: slug.to_string(),
        job,
        runs,
    }
}

fn recorded_controller_id(state: &super::routes::AppState, slug: &str) -> Option<String> {
    state
        .projects
        .get_project(slug)
        .ok()
        .flatten()
        .and_then(|p| p.controller_cron_id)
}

async fn get_controller(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<ControllerView>, ApiError> {
    if !super::projects_overview::is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let slug = super::projects_overview::canonicalize_project_slug(&slug);
    let recorded = recorded_controller_id(&state, &slug);
    let limit = query.limit.unwrap_or(DEFAULT_RUNS).clamp(1, MAX_RUNS);
    let view = tokio::task::spawn_blocking(move || controller_view_sync(&slug, recorded, limit))
        .await
        .map_err(internal)?;
    Ok(Json(view))
}

fn hermes_cli() -> String {
    std::env::var("HERMES_CLI_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| {
            if Path::new("/usr/local/bin/hermes").exists() {
                "/usr/local/bin/hermes".to_string()
            } else {
                "hermes".to_string()
            }
        })
}

async fn controller_action(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<ActionRequest>,
) -> Result<Json<ControllerView>, ApiError> {
    if !super::projects_overview::is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let verb = match req.action.as_str() {
        "pause" => "pause",
        "resume" => "resume",
        "run" | "trigger" => "run",
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown action '{other}'; expected pause, resume or run"),
            ))
        }
    };
    let slug = super::projects_overview::canonicalize_project_slug(&slug);
    let recorded = recorded_controller_id(&state, &slug);
    let lookup_slug = slug.clone();
    let lookup_recorded = recorded.clone();
    let (home, job_id) = tokio::task::spawn_blocking(move || {
        let home = hermes_home()?;
        let jobs = load_jobs(&home);
        let id = find_job(
            &jobs,
            &project_keys(&lookup_slug),
            lookup_recorded.as_deref(),
        )
        .and_then(|j| str_field(j, "id"))?;
        Some((home, id))
    })
    .await
    .map_err(internal)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            "this project has no controller cron".to_string(),
        )
    })?;

    let mut command = tokio::process::Command::new(hermes_cli());
    command
        .args(["cron", verb, &job_id])
        .env("HERMES_HOME", &home)
        .kill_on_drop(true);
    if verb == "run" {
        command.arg("--accept-hooks");
    }
    // The Hermes CLI expects the assistant's HOME (skills, secrets helper).
    if let Ok(cli_home) = std::env::var("HERMES_CLI_HOME") {
        command.env("HOME", cli_home);
    } else if Path::new("/var/lib/hermes-assistant").is_dir() {
        command.env("HOME", "/var/lib/hermes-assistant");
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(90), command.output())
        .await
        .map_err(|_| internal("hermes cron command timed out"))?
        .map_err(|e| internal(format!("failed to run hermes cron {verb}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "hermes cron {verb} failed: {}",
                stderr.trim().lines().last().unwrap_or(stdout.trim())
            ),
        ));
    }

    let view =
        tokio::task::spawn_blocking(move || controller_view_sync(&slug, recorded, DEFAULT_RUNS))
            .await
            .map_err(internal)?;
    Ok(Json(view))
}

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/:slug/controller", get(get_controller))
        .route("/:slug/controller/action", post(controller_action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_is_the_last_section_without_trailers() {
        let tail = "## Prompt\n\n## Response\nold\n\n## Response\n\nWriter blocked on a lease.\n\n[CTRL: verity-pareto | mode=active | wait=46]\n[STATE_SIGNATURE: verity-pareto|gaps|x]\n";
        let (report, ctrl, signature) = parse_response(response_section(tail).unwrap());
        assert_eq!(report, "Writer blocked on a lease.");
        assert_eq!(
            ctrl.as_deref(),
            Some("verity-pareto | mode=active | wait=46")
        );
        assert_eq!(signature.as_deref(), Some("verity-pareto|gaps|x"));
        assert!(response_section("no heading here").is_none());
    }

    #[test]
    fn silent_ticks_are_detected() {
        assert!(is_silent("", Some("delivered")));
        assert!(is_silent("[SILENT]", None));
        assert!(is_silent("Same as before.", Some("suppressed")));
        assert!(!is_silent("Merged #2406.", Some("delivered")));
    }

    #[test]
    fn job_lookup_prefers_recorded_id_then_binding_then_delivery() {
        let jobs: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"id":"aaa","name":"by-delivery","deliver":"project:verity-lido"},
              {"id":"bbb","name":"by-binding","controller":{"project":"verity-core"}},
              {"id":"ccc","name":"recorded","deliver":"origin"}
            ]"#,
        )
        .unwrap();
        let keys = |s: &str| vec![s.to_string()];
        let id = |j: Option<&serde_json::Value>| j.and_then(|j| str_field(j, "id"));
        assert_eq!(
            id(find_job(&jobs, &keys("verity-pareto"), Some("ccc"))).as_deref(),
            Some("ccc")
        );
        assert_eq!(
            id(find_job(&jobs, &keys("verity-core"), None)).as_deref(),
            Some("bbb")
        );
        assert_eq!(
            id(find_job(&jobs, &keys("verity-lido"), None)).as_deref(),
            Some("aaa")
        );
        // A stale recorded id falls through to the binding.
        assert_eq!(
            id(find_job(&jobs, &keys("verity-core"), Some("gone"))).as_deref(),
            Some("bbb")
        );
        assert!(find_job(&jobs, &keys("unknown"), None).is_none());
    }

    #[test]
    fn output_files_match_their_execution_window() {
        let exec = Execution {
            id: "e1".into(),
            source: Some("builtin".into()),
            status: Some("completed".into()),
            started_at: Some("2026-09-19T22:47:36.281629+02:00".into()),
            finished_at: Some("2026-09-19T22:50:16.396797+02:00".into()),
            error: None,
            delivery_outcome: Some("delivered".into()),
        };
        assert!(execution_matches(
            &exec,
            parse_output_stamp("2026-09-19_22-49-35").unwrap()
        ));
        assert!(!execution_matches(
            &exec,
            parse_output_stamp("2026-09-19_22-01-41").unwrap()
        ));
        assert!(parse_output_stamp("not-a-stamp").is_none());
    }

    #[test]
    fn runs_are_built_from_output_files_without_a_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("cron/output/job1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("2026-09-19_10-00-00.md"),
            "# Cron Job\n\n## Prompt\nlong\n\n## Response\n\nMerged #12.\n\n[CTRL: p | mode=active]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("2026-09-19_11-00-00.md"),
            "# Cron Job\n\n## Response\n\n[SILENT]\n",
        )
        .unwrap();
        let runs = build_runs(temp.path(), "job1", 10);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "2026-09-19_11-00-00");
        assert!(runs[0].silent);
        assert_eq!(runs[1].report, "Merged #12.");
        assert_eq!(runs[1].ctrl.as_deref(), Some("p | mode=active"));
        assert!(!runs[1].silent);
    }
}
