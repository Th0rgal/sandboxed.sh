//! Project controller view for desktop clients (Orb).
//!
//! A project's *controller* is a Hermes cron job that wakes on a schedule,
//! reads the project's grant and roadmap, dispatches missions and reports.
//! Hermes owns the job; this module only exposes a read model and lifecycle
//! actions so a client can show the controller inside its project:
//!
//! - `GET  /api/projects/:slug/controller`         — job, settings, recent runs
//! - `PUT  /api/projects/:slug/controller`         — edit the job's settings
//! - `POST /api/projects/:slug/controller/action`  — `pause` | `resume` | `run` | `archive` | `restore`
//!
//! The data is read straight from the Hermes cron store that lives on the
//! same host (`<hermes home>/cron/jobs.json`, `executions.db`, and one
//! markdown file per run under `cron/output/<job_id>/`). Actions go through
//! the Hermes scheduler API (settings use its CLI), keeping Hermes the single writer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

type ApiError = (StatusCode, String);

const DEFAULT_RUNS: usize = 30;
const MAX_RUNS: usize = 100;
const CONTROLLER_WAKE_WINDOW: Duration = Duration::from_secs(90);
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
    pub archived: bool,
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

/// Everything that defines what the cron does, as Hermes stores it. A cron
/// is a prompt run on a schedule by a fresh agent: the prompt is prefixed
/// with the attached skills, optionally with a script's stdout and with the
/// job's previous output (continuity), run with a pinned or default model,
/// and its answer is delivered to a target.
#[derive(Debug, Serialize, Clone, PartialEq, Default)]
pub struct ControllerSettings {
    pub prompt: String,
    pub prompt_chars: usize,
    /// Skills preloaded into the prompt on every run.
    pub skills: Vec<String>,
    /// Where the answer goes: `origin`, `local`, `project:<slug>`, a platform…
    pub deliver: Option<String>,
    pub failure_deliver: Option<String>,
    /// `None` = repeat forever.
    pub repeat_times: Option<i64>,
    pub repeat_completed: i64,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub reasoning_effort: Option<String>,
    pub workdir: Option<String>,
    /// Script whose stdout is injected into the prompt (or IS the job).
    pub script: Option<String>,
    pub no_agent: bool,
    /// Each run sees the job's own previous output.
    pub continuity: bool,
    pub monitor_url: Option<String>,
    pub monitor_script: Option<String>,
    pub enabled_toolsets: Vec<String>,
    pub created_at: Option<String>,
    /// Scope binding (project, permissions, mode). Read-only here.
    pub binding: Option<serde_json::Value>,
    /// Hard cap on the whole initial prompt (skills included) that Hermes
    /// enforces for scope-bound controllers; `None` when unbound.
    pub prompt_budget: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ControllerView {
    pub slug: String,
    pub job: Option<ControllerJob>,
    pub settings: Option<ControllerSettings>,
    pub runs: Vec<ControllerRun>,
}

/// Mirrors `CONTROLLER_PROMPT_MAX_CHARS` in Hermes' `cron/controller_scope.py`.
const BOUND_CONTROLLER_PROMPT_BUDGET: usize = 16_000;

/// Editable settings. Absent fields are left untouched; an empty string
/// clears an optional pin (model, provider, effort, workdir, failure target).
#[derive(Debug, Deserialize, Default)]
pub struct UpdateRequest {
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub prompt: Option<String>,
    pub skills: Option<Vec<String>>,
    pub deliver: Option<String>,
    pub failure_deliver: Option<String>,
    /// `Some(0)` or negative = forever.
    pub repeat: Option<i64>,
    pub workdir: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub reasoning_effort: Option<String>,
    pub continuity: Option<bool>,
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
        archived: false,
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

fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
        Some(serde_json::Value::String(one)) if !one.trim().is_empty() => vec![one.clone()],
        _ => Vec::new(),
    }
}

fn settings_view(job: &serde_json::Value) -> ControllerSettings {
    let id = str_field(job, "id").unwrap_or_default();
    let prompt = job
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut skills = string_list(job.get("skills"));
    if skills.is_empty() {
        skills = string_list(job.get("skill"));
    }
    let binding = job.get("controller").filter(|v| v.is_object()).cloned();
    ControllerSettings {
        prompt_chars: prompt.chars().count(),
        prompt,
        skills,
        deliver: str_field(job, "deliver"),
        failure_deliver: str_field(job, "failure_deliver"),
        repeat_times: job
            .get("repeat")
            .and_then(|r| r.get("times"))
            .and_then(|v| v.as_i64()),
        repeat_completed: job
            .get("repeat")
            .and_then(|r| r.get("completed"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        model: str_field(job, "model"),
        provider: str_field(job, "provider"),
        reasoning_effort: str_field(job, "reasoning_effort"),
        workdir: str_field(job, "workdir"),
        script: str_field(job, "script"),
        no_agent: job
            .get("no_agent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        continuity: string_list(job.get("context_from"))
            .iter()
            .any(|c| c == &id),
        monitor_url: str_field(job, "monitor_url"),
        monitor_script: str_field(job, "monitor_script"),
        enabled_toolsets: string_list(job.get("enabled_toolsets")),
        created_at: str_field(job, "created_at"),
        prompt_budget: binding.as_ref().map(|_| BOUND_CONTROLLER_PROMPT_BUDGET),
        binding,
    }
}

const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

fn plain_token(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.len() <= 120
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '/'))
}

/// Translate an update into `hermes cron edit` arguments. Pure so the
/// mapping (and its validation) is testable without a Hermes install.
fn edit_args(req: &UpdateRequest) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    // `--flag=value`, never `--flag value`: argparse must not be able to read
    // a user-supplied value that starts with `-` as another option.
    let mut push = |flag: &str, value: &str| args.push(format!("{flag}={value}"));
    if let Some(name) = req.name.as_deref().map(str::trim) {
        if name.is_empty() || name.chars().count() > 120 || name.contains('\n') {
            return Err("name must be 1 to 120 characters on one line".into());
        }
        push("--name", name);
    }
    if let Some(schedule) = req.schedule.as_deref().map(str::trim) {
        if schedule.is_empty() || schedule.chars().count() > 120 || schedule.contains('\n') {
            return Err(
                "schedule must be 1 to 120 characters, e.g. 'every 45m' or '0 9 * * 1-5'".into(),
            );
        }
        push("--schedule", schedule);
    }
    if let Some(prompt) = req.prompt.as_deref() {
        if prompt.trim().is_empty() {
            return Err("prompt cannot be empty".into());
        }
        if prompt.len() > 96 * 1024 {
            return Err("prompt is larger than 96 KB".into());
        }
        push("--prompt", prompt);
    }
    if let Some(deliver) = req.deliver.as_deref().map(str::trim) {
        if !plain_token(deliver) {
            return Err(
                "deliver must be a plain target such as origin, local or project:<slug>".into(),
            );
        }
        push("--deliver", deliver);
    }
    if let Some(target) = req.failure_deliver.as_deref().map(str::trim) {
        if !target.is_empty() && !plain_token(target) {
            return Err("failure_deliver must be a plain target, or empty to clear".into());
        }
        push("--failure-deliver", target);
    }
    if let Some(workdir) = req.workdir.as_deref().map(str::trim) {
        if !workdir.is_empty() && (!workdir.starts_with('/') || workdir.contains("..")) {
            return Err("workdir must be an absolute path, or empty to clear".into());
        }
        push("--workdir", workdir);
    }
    if let Some(model) = req.model.as_deref().map(str::trim) {
        if !model.is_empty() && !plain_token(model) {
            return Err("model is not a valid model id".into());
        }
        push("--model", model);
    }
    if let Some(provider) = req.provider.as_deref().map(str::trim) {
        if !provider.is_empty() && !plain_token(provider) {
            return Err("provider is not a valid provider id".into());
        }
        push("--provider", provider);
    }
    if let Some(effort) = req.reasoning_effort.as_deref().map(str::trim) {
        if !effort.is_empty() && !EFFORTS.contains(&effort) {
            return Err(format!(
                "reasoning_effort must be one of {}",
                EFFORTS.join(", ")
            ));
        }
        push("--reasoning-effort", effort);
    }
    if let Some(times) = req.repeat {
        push(
            "--repeat",
            &if times <= 0 {
                "forever".to_string()
            } else {
                times.to_string()
            },
        );
    }
    if let Some(skills) = req.skills.as_ref() {
        if skills.iter().any(|s| !plain_token(s.trim())) {
            return Err(
                "skill names may only contain letters, digits, '-', '_', ':' and '/'".into(),
            );
        }
        if skills.is_empty() {
            args.push("--clear-skills".to_string());
        } else {
            for skill in skills {
                args.push(format!("--skill={}", skill.trim()));
            }
        }
    }
    match req.continuity {
        Some(true) => args.push("--continuity".to_string()),
        Some(false) => args.push("--no-continuity".to_string()),
        None => {}
    }
    if args.is_empty() {
        return Err("no changes to apply".into());
    }
    Ok(args)
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

/// A tick that ran fine and had nothing new to say. A failed tick is never
/// silent, however empty its output: it is the thing the operator must see.
fn is_silent(report: &str, delivery_outcome: Option<&str>, failed: bool) -> bool {
    if failed {
        return false;
    }
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
    stems.sort_by_key(|entry| std::cmp::Reverse(entry.0));
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
        let failed = run.status.as_deref() == Some("failed") || run.error.is_some();
        run.silent = is_silent(&run.report, run.delivery_outcome.as_deref(), failed);
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
            settings: None,
            runs: Vec::new(),
        };
    };
    let jobs = load_jobs(&home);
    let raw = find_job(&jobs, &project_keys(slug), recorded_id.as_deref());
    let job = raw.and_then(job_view);
    let settings = job.as_ref().and(raw).map(settings_view);
    let runs = job
        .as_ref()
        .map(|j| build_runs(&home, &j.id, limit))
        .unwrap_or_default();
    ControllerView {
        slug: slug.to_string(),
        job,
        settings,
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

pub(crate) async fn snapshot_view(
    state: &super::routes::AppState,
    slug: &str,
) -> Option<ControllerView> {
    let recorded = recorded_controller_id(state, slug);
    let view_slug = slug.to_string();
    let mut view =
        tokio::task::spawn_blocking(move || controller_view_sync(&view_slug, recorded, MAX_RUNS))
            .await
            .ok()?;
    annotate_archive(&state.projects, slug, &mut view).ok()?;
    Some(view)
}

fn annotate_archive(
    store: &super::projects_store::ProjectsStore,
    slug: &str,
    view: &mut ControllerView,
) -> Result<(), ApiError> {
    if let Some(job) = view.job.as_mut() {
        job.archived = store.controller_archived(slug, &job.id).map_err(internal)?;
    }
    Ok(())
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
    let view_slug = slug.clone();
    let mut view =
        tokio::task::spawn_blocking(move || controller_view_sync(&slug, recorded, limit))
            .await
            .map_err(internal)?;
    annotate_archive(&state.projects, &view_slug, &mut view)?;
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

/// Resolve the project's controller job id and the Hermes home it lives in.
async fn resolve_controller(
    slug: String,
    recorded: Option<String>,
) -> Result<(PathBuf, String), ApiError> {
    tokio::task::spawn_blocking(move || {
        let home = hermes_home()?;
        let jobs = load_jobs(&home);
        let id = find_job(&jobs, &project_keys(&slug), recorded.as_deref())
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
    })
}

/// Run `hermes cron <args…>` against `home`. Hermes stays the only writer of
/// its cron store; this process never edits `jobs.json` itself.
async fn run_hermes_cron(home: &Path, args: &[String]) -> Result<(), ApiError> {
    let mut command = tokio::process::Command::new(hermes_cli());
    command
        .arg("cron")
        .args(args)
        .env("HERMES_HOME", home)
        .kill_on_drop(true);
    // The Hermes CLI expects the assistant's HOME (skills, secrets helper).
    if let Ok(cli_home) = std::env::var("HERMES_CLI_HOME") {
        command.env("HOME", cli_home);
    } else if Path::new("/var/lib/hermes-assistant").is_dir() {
        command.env("HOME", "/var/lib/hermes-assistant");
    }
    let verb = args.first().cloned().unwrap_or_default();
    let output = tokio::time::timeout(std::time::Duration::from_secs(90), command.output())
        .await
        .map_err(|_| internal("hermes cron command timed out"))?
        .map_err(|e| internal(format!("failed to run hermes cron {verb}: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .rfind(|l| !l.is_empty() && !l.contains("Bitwarden Secrets Manager"))
        .unwrap_or("unknown error")
        .to_string();
    Err((
        StatusCode::BAD_GATEWAY,
        format!("hermes cron {verb} failed: {detail}"),
    ))
}

async fn update_controller(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<UpdateRequest>,
) -> Result<Json<ControllerView>, ApiError> {
    if !super::projects_overview::is_plain_key(&slug) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let edit = edit_args(&req).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let slug = super::projects_overview::canonicalize_project_slug(&slug);
    let recorded = recorded_controller_id(&state, &slug);
    let (home, job_id) = resolve_controller(slug.clone(), recorded.clone()).await?;

    let mut args = vec!["edit".to_string()];
    args.extend(edit);
    args.push(job_id);
    run_hermes_cron(&home, &args).await?;

    let view_slug = slug.clone();
    let mut view =
        tokio::task::spawn_blocking(move || controller_view_sync(&slug, recorded, DEFAULT_RUNS))
            .await
            .map_err(internal)?;
    annotate_archive(&state.projects, &view_slug, &mut view)?;
    Ok(Json(view))
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
        "pause" | "archive" | "restore" => "pause",
        "resume" => "resume",
        "run" | "trigger" => "run",
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown action '{other}'; expected pause, resume, run, archive or restore"
                ),
            ))
        }
    };
    let slug = super::projects_overview::canonicalize_project_slug(&slug);
    let recorded = recorded_controller_id(&state, &slug);
    let (_, job_id) = resolve_controller(slug.clone(), recorded.clone()).await?;
    if matches!(verb, "resume" | "run")
        && state
            .projects
            .controller_archived(&slug, &job_id)
            .map_err(internal)?
    {
        return Err((
            StatusCode::CONFLICT,
            "Restore this controller before running it".into(),
        ));
    }

    // The CLI's `run` executes synchronously and can return exit 0 after
    // skipping a paused job. The gateway queues the explicit wake atomically,
    // resumes paused jobs, and owns execution independently of this request.
    if let Err(response) = super::project_crons::hermes(
        &state,
        reqwest::Method::POST,
        &format!("/api/jobs/{job_id}/{verb}"),
        None,
    )
    .await
    {
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .map_err(internal)?;
        return Err((status, String::from_utf8_lossy(&body).into_owned()));
    }

    if matches!(req.action.as_str(), "archive" | "restore") {
        state
            .projects
            .set_controller_archived(&slug, &job_id, req.action == "archive")
            .map_err(internal)?;
    }

    let view_slug = slug.clone();
    let mut view =
        tokio::task::spawn_blocking(move || controller_view_sync(&slug, recorded, DEFAULT_RUNS))
            .await
            .map_err(internal)?;
    annotate_archive(&state.projects, &view_slug, &mut view)?;
    Ok(Json(view))
}

fn controller_wake_guard() -> &'static Mutex<HashMap<String, Instant>> {
    static GUARD: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(HashMap::new()))
}

/// In-window dedupe so a burst of terminal webhooks collapses to one run.
pub fn claim_controller_wake(slug: &str, now: Instant) -> bool {
    let Ok(mut map) = controller_wake_guard().lock() else {
        return false;
    };
    map.retain(|_, previous| now.saturating_duration_since(*previous) < CONTROLLER_WAKE_WINDOW);
    if let Some(prev) = map.get(slug) {
        if now.saturating_duration_since(*prev) < CONTROLLER_WAKE_WINDOW {
            return false;
        }
    }
    map.insert(slug.to_string(), now);
    true
}

// Automatic wake has stricter eligibility than the operator's explicit Run
// action: never infer ownership from a matching delivery target or job order.
fn automatic_wake_job(
    project: &super::projects_store::ProjectRecord,
    jobs: &[serde_json::Value],
) -> Option<String> {
    if project.status != "active"
        || project
            .mode
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("paused"))
    {
        return None;
    }
    let recorded = project.controller_cron_id.as_deref()?.trim();
    let job = jobs
        .iter()
        .find(|job| job.get("id").and_then(|v| v.as_str()) == Some(recorded))?;
    if job.get("enabled").and_then(|v| v.as_bool()) == Some(false)
        || matches!(
            job.get("state").and_then(|v| v.as_str()),
            Some("paused" | "disabled" | "running")
        )
    {
        return None;
    }
    str_field(job, "id")
}

/// Best-effort automatic wake; explicit operator Run now is a separate action.
pub async fn wake_controller_for_slug(state: Arc<super::routes::AppState>, slug: &str) {
    if std::env::var("SANDBOXED_SH_CONTROLLER_TERMINAL_WAKE").as_deref() != Ok("1")
        || !super::projects_overview::is_plain_key(slug)
    {
        return;
    }
    let slug = super::projects_overview::canonicalize_project_slug(slug);
    let lookup_slug = slug.clone();
    let target = tokio::task::spawn_blocking(move || {
        let project = state.projects.get_project(&lookup_slug).ok().flatten()?;
        let home = hermes_home()?;
        let id = automatic_wake_job(&project, &load_jobs(&home))?;
        if state.projects.controller_archived(&lookup_slug, &id).ok()? {
            return None;
        }
        Some((home, id))
    })
    .await
    .ok()
    .flatten();
    let Some((home, job_id)) = target else { return };
    if !claim_controller_wake(&slug, Instant::now()) {
        return;
    }
    if let Err((_, error)) =
        run_hermes_cron(&home, &["run".into(), job_id, "--accept-hooks".into()]).await
    {
        tracing::warn!(project = %slug, %error, "controller wake after mission terminal failed");
    }
}

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route(
            "/:slug/controller",
            get(get_controller).put(update_controller),
        )
        .route("/:slug/controller/action", post(controller_action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_wake_respects_pause_and_only_the_registered_controller() {
        let store = super::super::projects_store::ProjectsStore::open_in_memory().unwrap();
        let mut project = store
            .upsert_project("lido", None, None, None, Some("canonical"))
            .unwrap();
        let mut jobs = vec![
            serde_json::json!({"id":"duplicate", "enabled":true, "state":"scheduled", "controller":{"project":"lido"}}),
            serde_json::json!({"id":"canonical", "enabled":true, "state":"scheduled", "controller":{"project":"lido"}}),
        ];
        assert_eq!(
            automatic_wake_job(&project, &jobs).as_deref(),
            Some("canonical")
        );
        for status in ["paused", "archived"] {
            project.status = status.into();
            assert!(automatic_wake_job(&project, &jobs).is_none());
        }
        project.status = "active".into();
        store.set_mode("lido", "paused", None, None).unwrap();
        project = store.get_project("lido").unwrap().unwrap();
        assert!(automatic_wake_job(&project, &jobs).is_none());
        project.mode = Some("active".into());
        for state in ["paused", "disabled", "running"] {
            jobs[1]["state"] = serde_json::json!(state);
            assert!(automatic_wake_job(&project, &jobs).is_none());
        }
        jobs[1]["state"] = serde_json::json!("scheduled");
        jobs[1]["enabled"] = serde_json::json!(false);
        assert!(automatic_wake_job(&project, &jobs).is_none());
        // Explicit operator resolution still finds the disabled job for Run.
        assert_eq!(
            str_field(
                find_job(&jobs, &["lido".into()], Some("canonical")).unwrap(),
                "id"
            )
            .as_deref(),
            Some("canonical")
        );
        jobs.remove(1);
        assert!(
            automatic_wake_job(&project, &jobs).is_none(),
            "missing registered job cannot fall back to a duplicate"
        );
        project.controller_cron_id = None;
        assert!(automatic_wake_job(&project, &jobs).is_none());
    }

    #[test]
    fn controller_wake_dedupes_inside_the_window() {
        let slug = format!("wake-{}", uuid::Uuid::new_v4());
        let t0 = Instant::now();
        assert!(claim_controller_wake(&slug, t0));
        assert!(!claim_controller_wake(&slug, t0 + Duration::from_secs(10)));
        assert!(claim_controller_wake(
            &slug,
            t0 + CONTROLLER_WAKE_WINDOW + Duration::from_secs(1)
        ));
    }

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
    fn settings_come_from_the_job_record() {
        let job: serde_json::Value = serde_json::from_str(
            r#"{"id":"j1","name":"c","prompt":"Do the thing.","skills":["controllers-policy","github-workflow"],
                "deliver":"project:verity-lido","repeat":{"times":null,"completed":12},
                "model":"builtin/assistant","provider":"sandboxed","context_from":["j1"],
                "enabled_toolsets":["mcp"],"controller":{"project":"verity-lido","mode":"operator"}}"#,
        )
        .unwrap();
        let s = settings_view(&job);
        assert_eq!(s.prompt, "Do the thing.");
        assert_eq!(s.prompt_chars, 13);
        assert_eq!(s.skills, vec!["controllers-policy", "github-workflow"]);
        assert_eq!(s.repeat_times, None);
        assert_eq!(s.repeat_completed, 12);
        assert!(s.continuity);
        assert_eq!(s.prompt_budget, Some(BOUND_CONTROLLER_PROMPT_BUDGET));
        // No binding → no budget; legacy single `skill` still listed.
        let plain: serde_json::Value =
            serde_json::from_str(r#"{"id":"j2","prompt":"x","skill":"controllers-policy"}"#)
                .unwrap();
        let s = settings_view(&plain);
        assert_eq!(s.skills, vec!["controllers-policy"]);
        assert_eq!(s.prompt_budget, None);
        assert!(!s.continuity);
    }

    #[test]
    fn updates_map_to_hermes_cron_edit_arguments() {
        let req = UpdateRequest {
            name: Some(" lido controller ".into()),
            schedule: Some("every 30m".into()),
            skills: Some(vec![]),
            model: Some("".into()),
            reasoning_effort: Some("high".into()),
            continuity: Some(true),
            repeat: Some(0),
            ..UpdateRequest::default()
        };
        let args = edit_args(&req).unwrap();
        assert_eq!(
            args,
            vec![
                "--name=lido controller",
                "--schedule=every 30m",
                "--model=",
                "--reasoning-effort=high",
                "--repeat=forever",
                "--clear-skills",
                "--continuity",
            ]
        );
        let skills = UpdateRequest {
            skills: Some(vec!["a".into(), "b:c".into()]),
            ..Default::default()
        };
        assert_eq!(
            edit_args(&skills).unwrap(),
            vec!["--skill=a", "--skill=b:c"]
        );
        // A value that looks like a flag stays a value.
        let sneaky = UpdateRequest {
            name: Some("--clear-skills".into()),
            ..Default::default()
        };
        assert_eq!(edit_args(&sneaky).unwrap(), vec!["--name=--clear-skills"]);

        // Rejected before anything reaches the CLI.
        assert!(edit_args(&UpdateRequest::default()).is_err());
        let bad = |r: UpdateRequest| edit_args(&r).is_err();
        assert!(bad(UpdateRequest {
            prompt: Some("   ".into()),
            ..Default::default()
        }));
        assert!(bad(UpdateRequest {
            reasoning_effort: Some("extreme".into()),
            ..Default::default()
        }));
        assert!(bad(UpdateRequest {
            workdir: Some("relative/path".into()),
            ..Default::default()
        }));
        assert!(bad(UpdateRequest {
            skills: Some(vec!["--prompt".into()]),
            ..Default::default()
        }));
        assert!(bad(UpdateRequest {
            skills: Some(vec!["x; rm".into()]),
            ..Default::default()
        }));
        assert!(bad(UpdateRequest {
            deliver: Some("project:x --clear-skills".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn silent_ticks_are_detected() {
        assert!(is_silent("", Some("delivered"), false));
        assert!(is_silent("[SILENT]", None, false));
        assert!(is_silent("Same as before.", Some("suppressed"), false));
        assert!(!is_silent("Merged #2406.", Some("delivered"), false));
        // A failed tick with no output is a failure, not a quiet tick.
        assert!(!is_silent("", Some("delivered"), true));
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
