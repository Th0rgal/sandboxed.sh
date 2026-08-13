//! MCP server for a standalone Hermes assistant.
//!
//! This is intentionally narrower than `orchestrator-mcp`: it exposes the
//! control-plane tools a personal assistant needs without deployment access.
//! Long workspace commands are exposed as durable jobs so the gateway never
//! has to hold a synchronous MCP request open for a build.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use jsonwebtoken::{EncodingKey, Header};
use serde::de::IntoDeserializer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

const SERVER_VERSION: &str = "0.1.0";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct MissionIdParams {
    mission_id: String,
}

#[derive(Debug, Deserialize)]
struct ProjectSlugParams {
    slug: String,
}

#[derive(Debug, Deserialize)]
struct UpdateProjectStatusParams {
    slug: String,
    mode: String,
    #[serde(default)]
    next_action: Option<String>,
    #[serde(default)]
    blocker: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetProjectTrackParams {
    slug: String,
    track: String,
    #[serde(default)]
    desired_state: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetProjectGrantParams {
    slug: String,
    #[serde(default)]
    merge_authority: Option<String>,
    #[serde(default)]
    budget_per_tick: Option<String>,
    #[serde(default)]
    parallel_missions: Option<i64>,
    #[serde(default)]
    pause_reason: Option<String>,
    #[serde(default)]
    resume_condition: Option<String>,
    #[serde(default)]
    material_bar: Option<String>,
    #[serde(default)]
    autonomy_level: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProposalTaskInput {
    task_key: String,
    title: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlanProjectTasksParams {
    slug: String,
    tasks: Vec<ProposalTaskInput>,
}

#[derive(Debug, Deserialize)]
struct UpdateProjectTaskParams {
    slug: String,
    task_key: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CancelProjectTaskParams {
    slug: String,
    task_key: String,
}

#[derive(Debug, Deserialize)]
struct RecordProjectDecisionParams {
    slug: String,
    question: String,
    #[serde(default)]
    rationale: Option<String>,
    /// merge | dispatch | abandon | pause | resume | scope | budget | retry | …
    #[serde(default)]
    kind: Option<String>,
    /// granted (autonomous act) | escalation (question for the owner, default).
    #[serde(default)]
    authority: Option<String>,
    /// decided | pending_user; defaults follow the authority.
    #[serde(default)]
    status: Option<String>,
    /// {"pr_url": …, "mission_id": …}
    #[serde(default)]
    evidence: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AnswerProjectDecisionParams {
    slug: String,
    /// The decision's `at` key, as returned by record/get_project.
    at: String,
    answer: String,
}

#[derive(Debug, Deserialize)]
struct LinkMissionToProjectParams {
    mission_id: String,
    slug: String,
    #[serde(default)]
    track: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdoptMissionParams {
    mission_id: String,
    /// The Hermes session that should own this mission's callbacks from now
    /// on. The sandboxed-origin-session plugin stamps it exactly as it does
    /// for start_mission; the model itself should not pass it.
    #[serde(default)]
    origin_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListMissionsParams {
    #[serde(default)]
    status: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    project: Option<String>,
    /// Project family: matches `X` and `X-*`.
    #[serde(default)]
    project_prefix: Option<String>,
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    /// The Hermes conversation a mission was launched from.
    #[serde(default)]
    origin_session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MissionEventsParams {
    mission_id: String,
    #[serde(default = "default_event_limit")]
    limit: usize,
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    since_seq: Option<i64>,
    /// Page backwards: return the newest `limit` events with sequence below
    /// this value (ascending order). Takes precedence over `since_seq`.
    /// When neither cursor is provided, the MCP defaults to the newest page.
    #[serde(default)]
    before_seq: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MissionSharedFilesParams {
    mission_id: String,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct DownloadSharedFileParams {
    mission_id: String,
    url: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    output_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StartMissionParams {
    title: String,
    prompt: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    model_effort: Option<String>,
    #[serde(default)]
    fast_mode: Option<bool>,
    #[serde(default)]
    config_profile: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    // Project tagging — forwarded so missions are created WITH structured
    // metadata instead of null (watchdogs then don't have to parse titles).
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    github_pr: Option<String>,
    #[serde(default)]
    writer: Option<bool>,
    /// Model-visible request for merge capability. It is not authority: this
    /// server derives grants only from its operator-owned environment.
    #[serde(default)]
    request_merge_authority: Option<bool>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    desired_state: Option<String>,
    #[serde(default)]
    next_check_at: Option<String>,
    #[serde(default)]
    estimated_disk_gib: Option<u64>,
    /// Hermes session that spawned this mission. Forwarded as the mission's
    /// `origin_session_id` so clients group it as a worker of that session.
    #[serde(default)]
    origin_session_id: Option<String>,
}

/// Deserialize a tool's arguments, naming the offending field when it fails.
///
/// `serde_json::from_value` reports the shape mismatch but not its location:
/// `invalid type: map, expected a string` for a struct with fourteen string
/// fields. An agent that reads that has no way to know which argument to fix.
///
/// Measured 2026-08-05: a controller called `start_mission` seven times in
/// ninety seconds, each time getting exactly that sentence back, until the
/// tool-loop guard cut it off. It was never told which field was wrong, so
/// each retry was a guess.
///
/// `serde_path_to_error` wraps the deserializer and tracks the path, turning
/// the same failure into `desired_state: invalid type: map, expected a
/// string` — actionable on the first read.
fn parse_params<T: serde::de::DeserializeOwned>(arguments: Value) -> Result<T, String> {
    let deserializer = arguments.into_deserializer();
    serde_path_to_error::deserialize(deserializer).map_err(|error| {
        let path = error.path().to_string();
        // The path is "." for a failure on the root value itself (arguments
        // that are not an object at all). Naming "." there would be noise.
        if path.is_empty() || path == "." {
            format!("Invalid params: {}", error.inner())
        } else {
            format!("Invalid params: {path}: {}", error.inner())
        }
    })
}

/// Accept only conservative session identifiers for `origin_session_id`
/// (Hermes session ids look like `20260803_150605_59ab72`; channel-keyed
/// sessions contain `:`). Keeping the shape tight means the value stays
/// machine-routable by the delivery handler.
fn valid_origin_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn native_backend_from_agent(agent: Option<&str>) -> Option<String> {
    match agent?.trim().to_ascii_lowercase().as_str() {
        "codex" => Some("codex".to_string()),
        "claudecode" => Some("claudecode".to_string()),
        "gemini" => Some("gemini".to_string()),
        "grok" => Some("grok".to_string()),
        _ => None,
    }
}

fn mission_start_tags(
    tags: Option<Vec<String>>,
    request_merge_authority: bool,
    writer: bool,
    github_pr: Option<&str>,
    merge_grant: Option<&MergeGrantConfig>,
) -> Result<Vec<String>, String> {
    let mut tags: Vec<String> = tags
        .unwrap_or_default()
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    tags.retain(|tag| {
        tag != "origin:hermes-assistant"
            && tag != "merge-authority:granted"
            && !tag.starts_with("merge-authority-source:")
            && !tag.starts_with("merge-authority-target:")
            && !tag.starts_with("origin-session:")
            && !tag.starts_with("origin-platform:")
    });
    tags.push("origin:hermes-assistant".to_string());

    if !request_merge_authority {
        return Ok(tags);
    }
    if !writer {
        return Err(
            "merge authority requires writer=true so the PR writer lease is held".to_string(),
        );
    }
    let github_pr = github_pr
        .map(str::trim)
        .filter(|value| canonical_github_pr_ref(value))
        .ok_or_else(|| {
            "merge authority requires github_pr in owner/repository#123 form".to_string()
        })?;
    let merge_grant = merge_grant.ok_or_else(|| {
        "merge authority was requested but no operator grant is configured".to_string()
    })?;
    let repository = github_pr.split_once('#').expect("validated PR ref").0;
    if !merge_grant.allows(repository) {
        return Err(format!(
            "merge authority was requested outside the configured repository scope: {repository}"
        ));
    }
    tags.push("merge-authority:granted".to_string());
    tags.push(format!(
        "merge-authority-source:{}",
        merge_grant.authority_source
    ));
    tags.push(format!("merge-authority-target:{github_pr}"));
    Ok(tags)
}

#[derive(Debug)]
struct MergeGrantConfig {
    authority_source: String,
    repositories: Vec<String>,
}

impl MergeGrantConfig {
    fn from_environment() -> Option<Self> {
        let authority_source = std::env::var("HERMES_MERGE_AUTHORITY_SOURCE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric()
                            || matches!(character, '-' | '_' | ':' | '/' | '#' | '.')
                    })
            })?;
        let repositories: Vec<String> = std::env::var("HERMES_MERGE_AUTHORITY_REPOSITORIES")
            .ok()?
            .split(',')
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect();
        if repositories.is_empty() {
            return None;
        }
        Some(Self {
            authority_source,
            repositories,
        })
    }

    fn allows(&self, repository: &str) -> bool {
        let repository = repository.to_ascii_lowercase();
        let owner = repository.split_once('/').map(|(owner, _)| owner);
        self.repositories.iter().any(|allowed| {
            allowed == &repository
                || allowed
                    .strip_suffix("/*")
                    .is_some_and(|allowed_owner| owner == Some(allowed_owner))
        })
    }
}

fn canonical_github_pr_ref(value: &str) -> bool {
    let Some((repository, number)) = value.split_once('#') else {
        return false;
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut parts = repository.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let valid_component = |component: &str| {
        !component.is_empty()
            && component.len() <= 100
            && component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    valid_component(owner) && valid_component(repo)
}

#[derive(Debug, Deserialize)]
struct SendMessageParams {
    mission_id: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AskMissionParams {
    mission_id: String,
    content: String,
    /// Continue an existing Ask thread. Omit to start a new one.
    #[serde(default)]
    thread_id: Option<String>,
    /// Run the Ask bash tool in an isolated copy of the workspace (writes never
    /// touch the live tree). Opt-in.
    #[serde(default)]
    sandbox: bool,
}

#[derive(Debug, Deserialize)]
struct AnswerMissionQuestionParams {
    mission_id: String,
    /// Target a specific AskUserQuestion call. Omit to auto-resolve the
    /// mission's single unanswered question.
    #[serde(default)]
    tool_call_id: Option<String>,
    /// One inner array per question; each entry is an option label or free
    /// text.
    answers: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceBashParams {
    command: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StartWorkspaceJobParams {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
    #[serde(default)]
    workspace_id: Option<String>,
    mission_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    resource_class: Option<String>,
    idempotency_key: String,
}

#[derive(Debug, Deserialize)]
struct WorkspaceJobParams {
    job_id: String,
    #[serde(default = "default_job_tail_bytes")]
    tail_bytes: usize,
}

fn default_job_tail_bytes() -> usize {
    16 * 1024
}

fn shell_quote_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn workspace_job_command(
    command: Option<String>,
    argv: Option<Vec<String>>,
) -> Result<String, String> {
    match (command, argv) {
        (Some(command), None) if !command.trim().is_empty() => Ok(command),
        (None, Some(argv)) if !argv.is_empty() => Ok(argv
            .iter()
            .map(|arg| shell_quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")),
        (Some(_), Some(_)) => Err("Pass exactly one of command or argv, not both".to_string()),
        _ => Err("Pass a non-empty command or argv".to_string()),
    }
}

fn is_heavy_workspace_command(command: &str) -> bool {
    let normalized = command
        .split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    [
        "lake build",
        "lake test",
        "cargo build --release",
        "cargo test --all",
        "npm run build",
        "bun run build",
        "next build",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn compact_workspace_job(job: Value) -> Value {
    json!({
        "id": job.get("id").cloned().unwrap_or(Value::Null),
        "status": job.get("status").cloned().unwrap_or(Value::Null),
        "heartbeat_at": job.get("heartbeat_at").cloned().unwrap_or(Value::Null),
        "deadline_at": job.get("deadline_at").cloned().unwrap_or(Value::Null),
        "scope_unit": job.get("scope_unit").cloned().unwrap_or(Value::Null),
        "pid": job.get("pid").cloned().unwrap_or(Value::Null),
        "exit_code": job.get("exit_code").cloned().unwrap_or(Value::Null),
        "signal": job.get("signal").cloned().unwrap_or(Value::Null),
        "workspace_id": job.get("workspace_id").cloned().unwrap_or(Value::Null),
        "mission_id": job.get("started_by_mission_id").cloned().unwrap_or(Value::Null),
        "resource_class": job.get("resource_class").cloned().unwrap_or(Value::Null),
        "created_at": job.get("created_at").cloned().unwrap_or(Value::Null),
        "updated_at": job.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}

#[derive(Debug, Deserialize)]
struct WorkspaceIdParams {
    workspace_id: String,
}

#[derive(Debug, Deserialize)]
struct DeleteWorkspaceParams {
    workspace_id: String,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
struct CreateWorkspaceParams {
    name: String,
    #[serde(default)]
    workspace_type: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    plugins: Option<Vec<String>>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    distro: Option<String>,
    #[serde(default)]
    env_vars: Option<HashMap<String, String>>,
    #[serde(default)]
    init_script: Option<String>,
    #[serde(default)]
    shared_network: Option<bool>,
    #[serde(default)]
    tailscale_mode: Option<String>,
    #[serde(default)]
    mcps: Option<Vec<String>>,
    #[serde(default)]
    mcps_replace_defaults: Option<bool>,
    #[serde(default)]
    config_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateWorkspaceParams {
    workspace_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    plugins: Option<Vec<String>>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    distro: Option<String>,
    #[serde(default)]
    env_vars: Option<HashMap<String, String>>,
    #[serde(default)]
    init_script: Option<String>,
    #[serde(default)]
    init_scripts: Option<Vec<String>>,
    #[serde(default)]
    shared_network: Option<bool>,
    #[serde(default)]
    tailscale_mode: Option<String>,
    #[serde(default)]
    mcps: Option<Vec<String>>,
    #[serde(default)]
    mcps_replace_defaults: Option<bool>,
    #[serde(default)]
    config_profile: Option<String>,
    #[serde(default)]
    config: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceTemplateNameParams {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SaveWorkspaceTemplateParams {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    distro: Option<String>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    env_vars: Option<HashMap<String, String>>,
    #[serde(default)]
    encrypted_keys: Option<Vec<String>>,
    #[serde(default)]
    init_scripts: Option<Vec<String>>,
    #[serde(default)]
    init_script: Option<String>,
    #[serde(default)]
    shared_network: Option<bool>,
    #[serde(default)]
    tailscale_mode: Option<String>,
    #[serde(default)]
    mcps: Option<Vec<String>>,
    #[serde(default)]
    mcps_replace_defaults: Option<bool>,
    #[serde(default)]
    config_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeleteWorkspaceTemplateParams {
    name: String,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
struct RebuildWorkspaceFromTemplateParams {
    workspace_id: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsParams {
    mission_id: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    model_effort: Option<String>,
    #[serde(default)]
    fast_mode: Option<bool>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    config_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResumeMissionParams {
    mission_id: String,
    /// Optional steering message delivered as the resume turn's prompt instead
    /// of the default "continue where you left off" text.
    #[serde(default)]
    content: Option<String>,
    /// Wipe the mission work directory before resuming. Rarely needed.
    #[serde(default)]
    clean_workspace: bool,
}

#[derive(Debug, Deserialize)]
struct MissionHealthParams {
    mission_id: String,
}

#[derive(Debug, Deserialize)]
struct MissionDiagnosticsParams {
    mission_id: String,
    #[serde(default = "default_diagnostics_limit")]
    limit: usize,
}

fn default_diagnostics_limit() -> usize {
    80
}

#[derive(Debug, Serialize)]
struct JwtClaims {
    sub: String,
    usr: String,
    iat: i64,
    exp: i64,
}

fn default_limit() -> usize {
    50
}

fn default_event_limit() -> usize {
    40
}

fn mission_events_path(
    id: Uuid,
    limit: usize,
    view: &str,
    before_seq: Option<i64>,
    since_seq: Option<i64>,
) -> String {
    let mut path =
        format!("/api/control/missions/{id}/events?limit={limit}&view={view}&include_counts=false");
    if let Some(before_seq) = before_seq {
        path.push_str(&format!("&before_seq={before_seq}"));
    } else if let Some(since_seq) = since_seq {
        path.push_str(&format!("&since_seq={since_seq}"));
    } else {
        // The events endpoint already defaults to the newest page when no
        // cursor is supplied, but Hermes uses this bounded tool for live
        // reconciliation, so we pin the tail explicitly to stay robust against
        // any future change to the server-side default.
        path.push_str(&format!("&before_seq={}", i64::MAX));
    }
    path
}

/// One unanswered AskUserQuestion tool call found in a mission's events.
#[derive(Debug, PartialEq, Eq)]
struct PendingAskQuestion {
    tool_call_id: String,
    sequence: i64,
    /// Question texts parsed from the call args, best-effort.
    questions: Vec<String>,
}

impl PendingAskQuestion {
    fn summary(&self) -> String {
        if self.questions.is_empty() {
            "(question text unavailable)".to_string()
        } else {
            self.questions.join(" | ")
        }
    }
}

/// Unanswered AskUserQuestion calls in `events`, newest first. A call is
/// unanswered when no tool_result event anywhere in the page shares its
/// tool_call_id.
fn pending_ask_user_questions(events: &[Value]) -> Vec<PendingAskQuestion> {
    let mut answered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for event in events {
        if event.get("event_type").and_then(Value::as_str) == Some("tool_result") {
            if let Some(id) = event.get("tool_call_id").and_then(Value::as_str) {
                answered.insert(id);
            }
        }
    }
    let mut pending = Vec::new();
    for event in events {
        if event.get("event_type").and_then(Value::as_str) != Some("tool_call")
            || event.get("tool_name").and_then(Value::as_str) != Some("AskUserQuestion")
        {
            continue;
        }
        let Some(id) = event.get("tool_call_id").and_then(Value::as_str) else {
            continue;
        };
        if answered.contains(id) {
            continue;
        }
        let questions = event
            .get("content")
            .and_then(Value::as_str)
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .as_ref()
            .and_then(|args| args.get("questions"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("question").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        pending.push(PendingAskQuestion {
            tool_call_id: id.to_string(),
            sequence: event.get("sequence").and_then(Value::as_i64).unwrap_or(0),
            questions,
        });
    }
    pending.sort_by_key(|question| std::cmp::Reverse(question.sequence));
    pending
}

fn insert_optional<T: Serialize>(
    body: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<T>,
) -> Result<(), String> {
    if let Some(value) = value {
        body.insert(
            key.to_string(),
            serde_json::to_value(value)
                .map_err(|error| format!("Failed to encode {key}: {error}"))?,
        );
    }
    Ok(())
}

fn require_resource_name(name: &str, resource: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{resource} name cannot be empty"));
    }
    Ok(name.to_string())
}

fn create_workspace_body(params: CreateWorkspaceParams) -> Result<Value, String> {
    let mut body = serde_json::Map::new();
    body.insert(
        "name".to_string(),
        json!(require_resource_name(&params.name, "Workspace")?),
    );
    insert_optional(&mut body, "workspace_type", params.workspace_type)?;
    insert_optional(&mut body, "path", params.path)?;
    insert_optional(&mut body, "skills", params.skills)?;
    insert_optional(&mut body, "plugins", params.plugins)?;
    insert_optional(&mut body, "template", params.template)?;
    insert_optional(&mut body, "distro", params.distro)?;
    insert_optional(&mut body, "env_vars", params.env_vars)?;
    insert_optional(&mut body, "init_script", params.init_script)?;
    insert_optional(&mut body, "shared_network", params.shared_network)?;
    insert_optional(&mut body, "tailscale_mode", params.tailscale_mode)?;
    insert_optional(&mut body, "mcps", params.mcps)?;
    insert_optional(
        &mut body,
        "mcps_replace_defaults",
        params.mcps_replace_defaults,
    )?;
    insert_optional(&mut body, "config_profile", params.config_profile)?;
    Ok(Value::Object(body))
}

fn update_workspace_body(params: UpdateWorkspaceParams) -> Result<(Uuid, Value), String> {
    let id = parse_uuid(&params.workspace_id)?;
    let mut body = serde_json::Map::new();
    insert_optional(&mut body, "name", params.name)?;
    insert_optional(&mut body, "skills", params.skills)?;
    insert_optional(&mut body, "plugins", params.plugins)?;
    insert_optional(&mut body, "template", params.template)?;
    insert_optional(&mut body, "distro", params.distro)?;
    insert_optional(&mut body, "env_vars", params.env_vars)?;
    insert_optional(&mut body, "init_script", params.init_script)?;
    insert_optional(&mut body, "init_scripts", params.init_scripts)?;
    insert_optional(&mut body, "shared_network", params.shared_network)?;
    insert_optional(&mut body, "tailscale_mode", params.tailscale_mode)?;
    insert_optional(&mut body, "mcps", params.mcps)?;
    insert_optional(
        &mut body,
        "mcps_replace_defaults",
        params.mcps_replace_defaults,
    )?;
    insert_optional(&mut body, "config_profile", params.config_profile)?;
    insert_optional(&mut body, "config", params.config)?;
    if body.is_empty() {
        return Err("No workspace fields supplied to update".to_string());
    }
    Ok((id, Value::Object(body)))
}

fn apply_template_patch(
    template: &mut serde_json::Map<String, Value>,
    params: SaveWorkspaceTemplateParams,
) -> Result<(), String> {
    insert_optional(template, "description", params.description)?;
    insert_optional(template, "distro", params.distro)?;
    insert_optional(template, "skills", params.skills)?;
    insert_optional(template, "env_vars", params.env_vars)?;
    insert_optional(template, "encrypted_keys", params.encrypted_keys)?;
    insert_optional(template, "init_scripts", params.init_scripts)?;
    insert_optional(template, "init_script", params.init_script)?;
    insert_optional(template, "shared_network", params.shared_network)?;
    insert_optional(template, "tailscale_mode", params.tailscale_mode)?;
    insert_optional(template, "mcps", params.mcps)?;
    insert_optional(
        template,
        "mcps_replace_defaults",
        params.mcps_replace_defaults,
    )?;
    insert_optional(template, "config_profile", params.config_profile)?;
    Ok(())
}

fn default_artifact_dir() -> PathBuf {
    std::env::var("HERMES_ASSISTANT_ARTIFACT_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/hermes-assistant-artifacts"))
}

fn sanitize_filename(name: &str) -> String {
    let clean = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .trim_start_matches('.')
        .to_string();
    if clean.is_empty() {
        "artifact".to_string()
    } else {
        clean.chars().take(180).collect()
    }
}

fn output_dir_for_shared_file(
    mission_id: &Uuid,
    requested: Option<String>,
) -> Result<PathBuf, String> {
    let base = requested
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_artifact_dir);
    if !base.is_absolute() {
        return Err("output_dir must be an absolute path".to_string());
    }
    // Reject `..` components: `starts_with` is lexical, so `/tmp/../etc` would
    // pass the prefix check below while resolving outside the real /tmp tree.
    if base
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("output_dir must not contain '..' components".to_string());
    }
    if !base.starts_with(Path::new("/tmp")) {
        return Err(
            "output_dir must be under /tmp so Paloma's email attachment policy can allow it"
                .to_string(),
        );
    }
    Ok(base.join(mission_id.to_string()))
}

fn shared_file_name_from_url(url: &str) -> Option<String> {
    let marker = "path=";
    let encoded = url.split(marker).nth(1)?.split('&').next()?;
    let decoded = urlencoding::decode(encoded).ok()?;
    Path::new(decoded.as_ref())
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string)
}

fn shared_file_download_path(url: &str) -> Result<String, String> {
    if url.starts_with("/api/fs/download?") {
        return Ok(url.to_string());
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|error| format!("Invalid shared file URL: {error}"))?;
    if parsed.path() != "/api/fs/download" {
        return Err("Only /api/fs/download shared file URLs can be downloaded".to_string());
    }
    let query = parsed
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    Ok(format!("{}{}", parsed.path(), query))
}

fn mint_service_jwt(secret: &str) -> Option<String> {
    let now = Utc::now();
    let exp = now + chrono::Duration::hours(24);
    let user_id = std::env::var("HERMES_ASSISTANT_USER_ID")
        .or_else(|_| std::env::var("SANDBOXED_ASSISTANT_USER_ID"))
        .or_else(|_| std::env::var("SANDBOXED_SINGLE_TENANT_USER_ID"))
        .or_else(|_| std::env::var("SINGLE_TENANT_USER_ID"))
        .unwrap_or_else(|_| "default".to_string());
    let user_id = user_id.trim();
    let user_id = if user_id.is_empty() {
        "default"
    } else {
        user_id
    };

    let claims = JwtClaims {
        sub: user_id.to_string(),
        usr: user_id.to_string(),
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };
    jsonwebtoken::encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .ok()
}

struct AssistantMcp {
    api_url: String,
    api_token: Option<String>,
    jwt_secret: Option<String>,
    /// Slugs this instance may MUTATE (`SANDBOXED_PROJECT_SCOPE`, comma-list).
    /// None = unrestricted (the owner's interactive Hermes). Reads are never
    /// scoped — cross-project awareness is a feature, not a leak.
    project_scope: Option<std::collections::HashSet<String>>,
    client: reqwest::Client,
}

impl AssistantMcp {
    fn new() -> Self {
        let api_url = std::env::var("HERMES_SANDBOXED_API_URL")
            .or_else(|_| std::env::var("SANDBOXED_API_URL"))
            .or_else(|_| std::env::var("OPEN_AGENT_API_URL"))
            .unwrap_or_else(|_| "http://127.0.0.1:3000".to_string())
            .trim_end_matches('/')
            .to_string();
        let api_token = std::env::var("HERMES_SANDBOXED_API_TOKEN")
            .or_else(|_| std::env::var("SANDBOXED_API_TOKEN"))
            .or_else(|_| std::env::var("OPEN_AGENT_API_TOKEN"))
            .ok()
            .filter(|token| !token.trim().is_empty());
        let jwt_secret = std::env::var("JWT_SECRET")
            .ok()
            .filter(|secret| !secret.trim().is_empty());
        let project_scope = std::env::var("SANDBOXED_PROJECT_SCOPE")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(|slug| slug.trim().to_string())
                    .filter(|slug| !slug.is_empty())
                    .collect::<std::collections::HashSet<_>>()
            })
            .filter(|scope| !scope.is_empty());
        Self {
            api_url,
            api_token,
            jwt_secret,
            project_scope,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Gate for MUTATING project tools. Scoped controllers stay inside their
    /// own project; unscoped instances (owner chat) pass untouched.
    fn assert_project_scope(&self, slug: &str) -> Result<(), String> {
        let slug = slug.trim();
        match &self.project_scope {
            Some(scope) if !scope.contains(slug) => {
                let mut allowed: Vec<&str> = scope.iter().map(String::as_str).collect();
                allowed.sort_unstable();
                Err(format!(
                    "project '{slug}' is outside this controller's scope ({}). \
                     Mutating tools are limited to your own project; reads are unrestricted.",
                    allowed.join(", ")
                ))
            }
            _ => Ok(()),
        }
    }

    fn auth_header(&self) -> Option<(String, String)> {
        // Prefer an explicit static token; otherwise mint a fresh service JWT
        // per request so long-running processes never send an expired token.
        self.api_token
            .clone()
            .or_else(|| self.jwt_secret.as_deref().and_then(mint_service_jwt))
            .map(|token| ("Authorization".to_string(), format!("Bearer {token}")))
    }

    /// Turn whatever the caller has into a mission UUID.
    ///
    /// Dashboards, logs and transcripts all show the 8-character prefix, so a
    /// controller keeping notes across days inevitably feeds one back. A full
    /// UUID resolves locally with no round trip; anything else asks the
    /// server, which is the only party that can tell whether the fragment is
    /// unambiguous. Ambiguity surfaces the candidates instead of guessing.
    async fn resolve_mission_id(&self, raw: &str) -> Result<Uuid, String> {
        let trimmed = raw.trim();
        if let Ok(id) = Uuid::parse_str(trimmed) {
            return Ok(id);
        }
        let response = self
            .api_get(&format!(
                "/api/control/missions/resolve?id={}",
                urlencoding::encode(trimmed)
            ))
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            let value: Value = serde_json::from_str(&body)
                .map_err(|error| format!("Failed to parse mission resolve response: {error}"))?;
            return value
                .get("mission_id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| format!("Mission resolve returned no id for '{trimmed}'"));
        }
        // Name the candidates so the next call can be exact.
        if let Ok(value) = serde_json::from_str::<Value>(&body) {
            if value.get("error").and_then(Value::as_str) == Some("ambiguous") {
                let listed = value
                    .get("candidates")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| {
                                let id = row.get("id")?.as_str()?;
                                let title = row.get("title").and_then(Value::as_str).unwrap_or("");
                                let mission_status =
                                    row.get("status").and_then(Value::as_str).unwrap_or("");
                                Some(format!("{id} ({mission_status}, {title:?})"))
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "Ambiguous mission id '{trimmed}' — candidates: {listed}. Pass the full id."
                ));
            }
        }
        Err(format!(
            "Failed to resolve mission '{trimmed}' ({status}): {body}"
        ))
    }

    async fn api_get(&self, path: &str) -> Result<reqwest::Response, String> {
        let mut req = self.client.get(format!("{}{}", self.api_url, path));
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        req.send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))
    }

    async fn api_get_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let response = self.api_get(path).await?;
        if !response.status().is_success() {
            return Err(format!(
                "Failed to download shared file: {}",
                response.status()
            ));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("Failed to read shared file bytes: {error}"))
    }

    async fn api_post(&self, path: &str, body: Value) -> Result<reqwest::Response, String> {
        self.api_post_with_timeout(path, body, None).await
    }

    /// POST with an optional per-request timeout override. The shared client's
    /// default is 120s; synchronous long-running endpoints (`/ask`) need more
    /// headroom than that but must still resolve before the Hermes-side MCP
    /// request timeout (600s in the generated config) so the caller gets a
    /// clean error instead of a severed stdio call.
    async fn api_post_with_timeout(
        &self,
        path: &str,
        body: Value,
        timeout: Option<std::time::Duration>,
    ) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .post(format!("{}{}", self.api_url, path))
            .json(&body);
        if let Some(timeout) = timeout {
            req = req.timeout(timeout);
        }
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        req.send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))
    }

    async fn api_patch(&self, path: &str, body: Value) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .patch(format!("{}{}", self.api_url, path))
            .json(&body);
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        req.send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))
    }

    async fn api_put(&self, path: &str, body: Value) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .put(format!("{}{}", self.api_url, path))
            .json(&body);
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        req.send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))
    }

    async fn api_delete(&self, path: &str) -> Result<reqwest::Response, String> {
        let mut req = self.client.delete(format!("{}{}", self.api_url, path));
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        req.send()
            .await
            .map_err(|error| format!("HTTP request failed: {error}"))
    }

    async fn response_value(response: reqwest::Response, operation: &str) -> Result<Value, String> {
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("Failed to read {operation} response: {error}"))?;
        if !status.is_success() {
            let detail = String::from_utf8_lossy(&bytes);
            return Err(format!("{operation} failed ({status}): {detail}"));
        }
        if bytes.is_empty() {
            return Ok(json!({"success": true}));
        }
        match serde_json::from_slice(&bytes) {
            Ok(value) => Ok(value),
            Err(_) => Ok(json!({
                "success": true,
                "message": String::from_utf8_lossy(&bytes).trim(),
            })),
        }
    }

    fn tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_active_missions".to_string(),
                description: "List active, pending, blocked, or awaiting-user missions in sandboxed.sh.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "Maximum missions to return, default 50."},
                        "project": {"type": "string", "description": "Optional filter: exact project id."},
                        "project_prefix": {"type": "string", "description": "Optional filter: project FAMILY — matches the id and its `-` suffixed variants (e.g. verity covers verity-core, verity-phase1d). Use this when a project's work is still split across per-phase ids."},
                        "track": {"type": "string", "description": "Optional filter: exact track within a project."},
                        "tag": {"type": "string", "description": "Optional filter: only missions carrying this tag."},
                        "origin_session_id": {"type": "string", "description": "Optional filter: only missions launched from this Hermes conversation."}
                    }
                }),
            },
            ToolDefinition {
                name: "list_missions".to_string(),
                description: "List recent missions, optionally filtered by status, project, or tag.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "status": {"type": "string", "description": "Optional mission status filter."},
                        "limit": {"type": "integer", "description": "Maximum missions to return, default 50."},
                        "project": {"type": "string", "description": "Optional filter: exact project id."},
                        "project_prefix": {"type": "string", "description": "Optional filter: project FAMILY — matches the id and its `-` suffixed variants (e.g. verity covers verity-core, verity-phase1d). Use this when a project's work is still split across per-phase ids."},
                        "track": {"type": "string", "description": "Optional filter: exact track within a project."},
                        "tag": {"type": "string", "description": "Optional filter: only missions carrying this tag."},
                        "origin_session_id": {"type": "string", "description": "Optional filter: only missions launched from this Hermes conversation."}
                    }
                }),
            },
            ToolDefinition {
                name: "get_mission".to_string(),
                description: "Compatibility alias for the compact ~2KB mission digest. It never returns the full history; use get_mission_events with a bounded limit for transcript or trace details.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "get_mission_digest".to_string(),
                description: "Compact ~2KB mission status: state, awaiting_kind, last user/assistant messages (truncated), GitHub PR links, project metadata. Use this instead of get_mission/get_mission_events for recaps and 'where is it?' checks — it avoids pulling whole transcripts into context.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "get_mission_events".to_string(),
                description: "Fetch persisted mission events. Without a cursor, returns the newest bounded page in ascending order; use before_seq to page backwards or since_seq for deltas.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "limit": {"type": "integer", "description": "Maximum events to return, default 40 newest events."},
                        "view": {"type": "string", "enum": ["transcript", "trace", "history", "all"]},
                        "since_seq": {"type": "integer", "description": "Return events with sequence greater than this value."},
                        "before_seq": {"type": "integer", "description": "Page backwards: return the newest events with sequence below this value (takes precedence over since_seq)."}
                    }
                }),
            },
            ToolDefinition {
                name: "get_chatgpt_ui_pool_status".to_string(),
                description: "Get privacy-safe ChatGPT UI slot telemetry and the persistent backend availability gate. Before starting, resuming, or redispatching a chatgpt_ui mission, require availability.state=available; cooldown and probing states must wait for the backend recovery probe.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "list_mission_shared_files".to_string(),
                description: "List files and screenshots shared by assistant messages in a sandboxed.sh mission, including bounded files downloaded from a ChatGPT UI response.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "limit": {"type": "integer", "description": "Maximum mission events to scan, default 40."}
                    }
                }),
            },
            ToolDefinition {
                name: "download_shared_file".to_string(),
                description: "Download a mission shared file URL to a local /tmp artifact path suitable for inspection or attachments. Use list_mission_shared_files first, including after ChatGPT UI missions that generated files.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "url"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "url": {"type": "string", "description": "A shared_files[].url value returned by list_mission_shared_files or get_mission_events."},
                        "filename": {"type": "string", "description": "Optional output filename override."},
                        "output_dir": {"type": "string", "description": "Optional absolute directory under /tmp. Defaults to /tmp/hermes-assistant-artifacts."}
                    }
                }),
            },
            ToolDefinition {
                name: "start_mission".to_string(),
                description: "Create a new sandboxed.sh mission and send its initial prompt. Set backend explicitly when possible. For Codex GPT-5.6/5.5/5.4, set fast_mode=true to request the native fast service tier; this consumes ChatGPT credits faster. Use backend=chatgpt_ui with model_override=gpt-5.6-pro only for exceptionally difficult read-only synthesis, research, or design-conflict questions; keep writer=false, then retrieve any generated files with list_mission_shared_files and download_shared_file. For compatibility, a native agent name (codex/claudecode/gemini/grok) selects the matching backend when backend is omitted; ordinary library agent names do not. Pass project/track/intent/github_pr/tags so the mission carries structured metadata (so watchdogs/dashboards don't have to parse the title). Reviewers and certifiers must use writer=false: the server tags them pr-readonly and blocks git/gh mutations. Any PR-changing mission must use writer=true; the API rejects concurrent writers for the same PR.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["title", "prompt"],
                    "properties": {
                        "title": {"type": "string"},
                        "prompt": {"type": "string"},
                        "workspace_id": {"type": "string"},
                        "backend": {"type": "string", "enum": ["opencode", "claudecode", "codex", "gemini", "grok", "chatgpt_ui"]},
                        "model_override": {"type": "string", "description": "Exact account-supported model ID. For ChatGPT UI Pro use the canonical ID gpt-5.6-pro; the harness verifies the visible Pro picker option. For Codex Terra use gpt-5.6-terra with medium effort. Never invent variants such as gpt-5.5-sol."},
                        "model_effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                        "fast_mode": {"type": "boolean", "description": "Enable Codex fast mode (service tier fast). Requires backend=codex and an explicit GPT-5.6, GPT-5.5, or GPT-5.4 model_override. Uses ChatGPT credits faster."},
                        "config_profile": {"type": "string"},
                        "agent": {"type": "string"},
                        "project": {"type": "string", "description": "Stable project id (e.g. \"verity\")."},
                        "track": {"type": "string", "description": "Track/workstream (e.g. \"core-c3\")."},
                        "intent": {"type": "string", "description": "Intent (e.g. \"review_merge_pr\")."},
                        "github_pr": {"type": "string", "description": "Associated PR ref (e.g. \"owner/repo#123\")."},
                        "writer": {"type": "boolean", "description": "Capability boundary for the associated PR. Use false for every reviewer/certifier (server-enforced read-only git/gh); use true for any branch, comment, thread, approval, or merge mutation. Concurrent writers for one PR are rejected."},
                        "request_merge_authority": {"type": "boolean", "description": "Request final guarded merge capability for a dedicated integrator. This is not self-authorization: the trusted Hermes client grants it only when github_pr matches the operator-configured repository allowlist."},
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "desired_state": {"type": "string", "description": "Track state, e.g. waiting_ci / waiting_review / blocked_external."},
                        "next_check_at": {"type": "string", "description": "When the track should next be checked (RFC3339)."},
                        "estimated_disk_gib": {"type": "integer", "minimum": 1, "maximum": 512, "description": "Expected peak local scratch use. Set this for Lean/build-heavy missions; omit for small/no-build work."},
                        "origin_session_id": {"type": "string", "description": "Hermes session id of the conversation spawning this mission. Dashboards group the mission as a worker of that session, AND the mission-status webhook carries it back so the completion is delivered into that conversation instead of an isolated webhook session — always pass it when starting a mission from a conversation. Injected automatically by the Hermes-side plugin; pass through unchanged, never another session's id."}
                    }
                }),
            },
            ToolDefinition {
                name: "send_message_to_mission".to_string(),
                description: "Send a follow-up message to an existing mission, waking it if it is idle. This is the general way to restart a parked mission and KEEP THE SAME mission id: it activates pending, awaiting_user, acknowledged, waiting_background, interrupted, blocked, completed and failed missions alike. If the mission is already running the message is delivered to the live turn. There is no idle status that requires starting a new mission just to get the agent's attention.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "content"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "content": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "ask_mission".to_string(),
                description: "Ask a read-only question to a mission's Ask copilot (a sidecar with bash + workspace access) WITHOUT disturbing the mission or waking its main agent. Use this to inspect a mission's workspace/state or get analysis. Returns the copilot's answer and a thread_id; pass thread_id back to continue the same conversation. This does NOT send a message to the mission agent — use send_message_to_mission for that.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "content"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "content": {"type": "string", "description": "The question for the copilot."},
                        "thread_id": {"type": "string", "description": "Optional: continue an existing Ask thread."},
                        "sandbox": {"type": "boolean", "description": "Optional: isolate bash writes in a throwaway copy of the workspace (default false)."}
                    }
                }),
            },
            ToolDefinition {
                name: "answer_mission_question".to_string(),
                description: "Answer a mission's pending AskUserQuestion so its blocked turn resumes immediately. A mission parked on a question cannot be unblocked with send_message_to_mission — plain messages QUEUE BEHIND the blocked turn — so this is the way a controller answers instead of leaving the mission in awaiting_user. `answers` is an array of arrays: one inner array per question, each entry an option label (preferred) or free text. Omit tool_call_id to auto-target the mission's single unanswered question; if several are pending you get an error listing them.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "answers"],
                    "properties": {
                        "mission_id": {"type": "string", "description": "Mission UUID or an unambiguous leading fragment."},
                        "tool_call_id": {"type": "string", "description": "Optional: the AskUserQuestion tool_call id to answer. Omit to use the newest unanswered one."},
                        "answers": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}, "description": "One inner array per question, e.g. [[\"Option A\"]]."}
                    }
                }),
            },
            ToolDefinition {
                name: "cancel_mission".to_string(),
                description: "Cancel a running or pending mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "acknowledge_mission".to_string(),
                description: "Acknowledge a mission only after independently verifying its terminal result. This is the safe host-authenticated replacement for asking a workspace container to use the service JWT. It accepts only awaiting_user missions whose awaiting_kind is ack; decision waits and live missions are refused.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "adopt_mission".to_string(),
                description: "Re-point a mission's origin to THIS conversation so its completion callback lands here. Use when you are waiting on a mission that was dispatched from another session (or with no origin): without adoption, its terminal webhook pins into the conversation that started it — or nowhere — and this conversation is never woken. After adopting, do not poll: the mission-complete callback will arrive as a delivery in this conversation.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "get_compute_fleet".to_string(),
                description: "Get a compact live view of sandboxed.sh compute capacity: remote node health, labels, Lean readiness/toolchains, available slots, active/queued jobs, and recent placement receipts. Use this before dispatching parallel compute or choosing a remote validation node. Ordinary CPU/Lean work should prefer non-GPU nodes while they have immediate capacity; request the gpu label only for GPU work.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "list_projects".to_string(),
                description: "List the projects you supervise (the authoritative roster): slug, status, mode (active/blocked/paused), how many consecutive ticks in that mode, and the next action. Read this at the start of a tick instead of scanning tracker files.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "get_project".to_string(),
                description: "Get one project's structured state: record (objective, status, mode, blocker), autonomy grant, tracks, open decisions for the owner, and the bound control conversation. This is the source of truth for your project — prefer it over reading markdown trackers.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug"],
                    "properties": {"slug": {"type": "string", "description": "Project slug, e.g. 'verity' or 'coldcard-rng-cracker'."}}
                }),
            },
            ToolDefinition {
                name: "update_project_status".to_string(),
                description: "Report your project's state for this tick: mode (active | blocked | paused), the next action, and the blocker if any. Replaces the [CTRL:] trailer with a structured write; the store counts how long you have been in this mode, so blocked/paused staleness is visible without parsing your reports.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "mode"],
                    "properties": {
                        "slug": {"type": "string"},
                        "mode": {"type": "string", "enum": ["active", "blocked", "paused"]},
                        "next_action": {"type": "string", "description": "The next concrete step, or the resume/unblock condition."},
                        "blocker": {"type": "string", "description": "What you are blocked on. Set only when mode=blocked."}
                    }
                }),
            },
            ToolDefinition {
                name: "set_project_track".to_string(),
                description: "Declare or update one workstream of your project: its desired_state (what it should reach) and current status. Use instead of editing prose in a tracker file.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "track"],
                    "properties": {
                        "slug": {"type": "string"},
                        "track": {"type": "string"},
                        "desired_state": {"type": "string"},
                        "status": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "get_project_grant".to_string(),
                description: "Read your project's autonomy grant: merge authority (full | repo:… | review-first), budget per tick, parallel missions, and the structured PAUSED(pause_reason; resume_condition). This is the durable source of what you are authorized to do — it survives prompt rewrites. Read it at your first tick.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug"],
                    "properties": {"slug": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "set_project_grant".to_string(),
                description: "Record the owner's autonomy grant for a project after they answer the setup questions: the normalized autonomy level, merge authority, budget, parallel missions, pause reason + machine-checkable resume condition, and the material-report bar. The project must already exist.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug"],
                    "properties": {
                        "slug": {"type": "string"},
                        "autonomy_level": {"type": "string", "enum": ["observe", "propose", "act_reversible", "act_full"], "description": "What the controller may do without asking: observe (report only), propose (escalate every act), act_reversible (act, but irreversible kinds — merge/abandon/delete/publish/deploy/force_push — still escalate), act_full."},
                        "merge_authority": {"type": "string", "description": "full | repo:a,b | review-first"},
                        "budget_per_tick": {"type": "string"},
                        "parallel_missions": {"type": "integer"},
                        "pause_reason": {"type": "string"},
                        "resume_condition": {"type": "string", "description": "A condition you can check yourself, e.g. 'FTDI device enumerates on spark-de79'."},
                        "material_bar": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "record_project_decision".to_string(),
                description: "Write to the project's decision ledger. Two uses: (1) declare an autonomous act BEFORE doing it (authority=granted, status=decided, kind, evidence with pr_url/mission_id) — if the response says coerced=true your grant does not cover it, treat it as an escalation and do NOT execute; (2) ask the owner a question (authority=escalation or just omit the new fields — the legacy shape still means 'ask'). Non-blocking either way.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "question"],
                    "properties": {
                        "slug": {"type": "string"},
                        "question": {"type": "string", "description": "The question for the owner, or the past-tense statement of the act ('Merged verity#2213')."},
                        "rationale": {"type": "string"},
                        "kind": {"type": "string", "description": "merge | dispatch | abandon | pause | resume | scope | budget | retry | ..."},
                        "authority": {"type": "string", "enum": ["granted", "escalation"]},
                        "status": {"type": "string", "enum": ["decided", "pending_user"]},
                        "evidence": {"type": "object", "description": "Supporting links, e.g. {\"pr_url\": ..., \"mission_id\": ...}."}
                    }
                }),
            },
            ToolDefinition {
                name: "answer_project_decision".to_string(),
                description: "Resolve a pending owner escalation in a project's decision ledger with the owner's answer. Use only when relaying a decision the owner actually expressed.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "at", "answer"],
                    "properties": {
                        "slug": {"type": "string"},
                        "at": {"type": "string", "description": "The decision's 'at' timestamp key from get_project/record_project_decision."},
                        "answer": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "get_project_tasks".to_string(),
                description: "The project's roadmap: every board task planned under the project's boss missions, with status, dependencies, result digest, PR link, and worker mission. Read this to see what is done/running/failed across the whole project without walking individual mission boards.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug"],
                    "properties": {"slug": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "plan_project_tasks".to_string(),
                description: "Plan roadmap items for a project from conversation. Creates proposals (status 'proposed') on the project's roadmap — visible to the owner and to the project's controller, which turns them into real board tasks when it dispatches work (a board task under the same task_key supersedes the proposal). Re-planning an existing key updates it.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "tasks"],
                    "properties": {
                        "slug": {"type": "string"},
                        "tasks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["task_key", "title"],
                                "properties": {
                                    "task_key": {"type": "string", "description": "Stable kebab-case key, unique within the project."},
                                    "title": {"type": "string"},
                                    "prompt": {"type": "string", "description": "What a worker should actually do, if known."},
                                    "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
                                    "depends_on": {"type": "array", "items": {"type": "string"}}
                                }
                            }
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "update_project_task".to_string(),
                description: "Edit an open roadmap proposal (title, prompt, acceptance criteria, dependencies). Only proposals are editable — once a boss mission plans the key as a real board task, edits flow through that mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "task_key"],
                    "properties": {
                        "slug": {"type": "string"},
                        "task_key": {"type": "string"},
                        "title": {"type": "string"},
                        "prompt": {"type": "string"},
                        "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
                        "depends_on": {"type": "array", "items": {"type": "string"}}
                    }
                }),
            },
            ToolDefinition {
                name: "cancel_project_task".to_string(),
                description: "Cancel an open roadmap proposal. Board tasks already adopted by a boss mission are not touched — cancel those through the mission's own board.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["slug", "task_key"],
                    "properties": {
                        "slug": {"type": "string"},
                        "task_key": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "link_mission_to_project".to_string(),
                description: "Tag a mission as belonging to your project (and optionally a track), so it appears in the project's inventory. Use this for missions you dispatch that must be grouped under the project — a worker with no project tag is invisible in the roster.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "slug"],
                    "properties": {
                        "mission_id": {"type": "string", "description": "Mission UUID or an unambiguous leading fragment."},
                        "slug": {"type": "string"},
                        "track": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "list_workspaces".to_string(),
                description: "List sandboxed.sh workspaces so new missions can target the right environment.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "get_workspace".to_string(),
                description: "Get the complete configuration and current build status of one sandboxed.sh workspace.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["workspace_id"],
                    "properties": {"workspace_id": {"type": "string", "description": "Workspace UUID."}}
                }),
            },
            ToolDefinition {
                name: "create_workspace".to_string(),
                description: "Create a sandboxed.sh workspace. A template always creates an isolated container and starts its build automatically; a host workspace requires a path inside the server working directory.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string"},
                        "workspace_type": {"type": "string", "enum": ["host", "container"], "description": "Defaults to host unless template is set."},
                        "path": {"type": "string", "description": "Required for host workspaces; omit for the standard container path."},
                        "skills": {"type": "array", "items": {"type": "string"}},
                        "plugins": {"type": "array", "items": {"type": "string"}},
                        "template": {"type": "string", "description": "Workspace template name. Forces container type and starts a build."},
                        "distro": {"type": "string"},
                        "env_vars": {"type": "object", "additionalProperties": {"type": "string"}},
                        "init_script": {"type": "string"},
                        "shared_network": {"type": "boolean"},
                        "tailscale_mode": {"type": "string", "enum": ["exit_node", "tailnet_only"]},
                        "mcps": {"type": "array", "items": {"type": "string"}},
                        "mcps_replace_defaults": {"type": "boolean"},
                        "config_profile": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "update_workspace".to_string(),
                description: "Update only the supplied fields of an existing workspace. Setting template changes the association but does not reapply template contents; use rebuild_workspace_from_template for that.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["workspace_id"],
                    "properties": {
                        "workspace_id": {"type": "string"},
                        "name": {"type": "string"},
                        "skills": {"type": "array", "items": {"type": "string"}},
                        "plugins": {"type": "array", "items": {"type": "string"}},
                        "template": {"type": "string", "description": "Empty string clears the association."},
                        "distro": {"type": "string", "description": "Empty string clears the override."},
                        "env_vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Replaces the workspace env map."},
                        "init_script": {"type": "string"},
                        "init_scripts": {"type": "array", "items": {"type": "string"}},
                        "shared_network": {"type": "boolean"},
                        "tailscale_mode": {"type": "string", "enum": ["exit_node", "tailnet_only"]},
                        "mcps": {"type": "array", "items": {"type": "string"}},
                        "mcps_replace_defaults": {"type": "boolean"},
                        "config_profile": {"type": "string", "description": "Empty string clears the profile."},
                        "config": {"type": "object", "description": "Shallow-merged into freeform workspace config."}
                    }
                }),
            },
            ToolDefinition {
                name: "delete_workspace".to_string(),
                description: "Delete a sandboxed.sh workspace and its managed container data. Requires confirm=true; active missions that reference it are not deleted.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["workspace_id", "confirm"],
                    "properties": {
                        "workspace_id": {"type": "string"},
                        "confirm": {"type": "boolean", "description": "Must be true to perform deletion."}
                    }
                }),
            },
            ToolDefinition {
                name: "list_workspace_templates".to_string(),
                description: "List reusable sandboxed.sh workspace templates from the Library.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "get_workspace_template".to_string(),
                description: "Get a reusable workspace template. Sensitive values are redacted from the tool result.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {"name": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "save_workspace_template".to_string(),
                description: "Create or update a reusable workspace template. Existing templates are patch-updated: omitted fields are preserved, while supplied arrays/maps replace that field.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string"},
                        "description": {"type": "string"},
                        "distro": {"type": "string"},
                        "skills": {"type": "array", "items": {"type": "string"}},
                        "env_vars": {"type": "object", "additionalProperties": {"type": "string"}},
                        "encrypted_keys": {"type": "array", "items": {"type": "string"}, "description": "Env var keys to encrypt at rest."},
                        "init_scripts": {"type": "array", "items": {"type": "string"}},
                        "init_script": {"type": "string"},
                        "shared_network": {"type": "boolean"},
                        "tailscale_mode": {"type": "string", "enum": ["exit_node", "tailnet_only"]},
                        "mcps": {"type": "array", "items": {"type": "string"}},
                        "mcps_replace_defaults": {"type": "boolean"},
                        "config_profile": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "delete_workspace_template".to_string(),
                description: "Delete a reusable workspace template. Requires confirm=true and does not delete workspaces already created from it.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["name", "confirm"],
                    "properties": {
                        "name": {"type": "string"},
                        "confirm": {"type": "boolean", "description": "Must be true to perform deletion."}
                    }
                }),
            },
            ToolDefinition {
                name: "rebuild_workspace_from_template".to_string(),
                description: "Reapply the latest template definition to an existing container workspace, replacing template-controlled fields, then force-rebuild its container. Requires confirm=true because container state may be replaced.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["workspace_id", "confirm"],
                    "properties": {
                        "workspace_id": {"type": "string"},
                        "template": {"type": "string", "description": "Optional template name; defaults to the workspace's current template."},
                        "confirm": {"type": "boolean", "description": "Must be true to force rebuild."}
                    }
                }),
            },
            ToolDefinition {
                name: "workspace_bash".to_string(),
                description: "Run a short diagnostic command inside a sandboxed.sh workspace. Defaults to 60 seconds and never exceeds 120 seconds. Heavy commands such as `lake build` are rejected; use start_workspace_job for long work.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["command"],
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run in the workspace."},
                        "workspace_id": {"type": "string", "description": "Workspace UUID. Defaults to the assistant's default workspace."},
                        "cwd": {"type": "string", "description": "Working directory relative to the workspace root."},
                        "timeout_secs": {"type": "integer", "description": "Timeout in seconds, default 60, max 120."}
                    }
                }),
            },
            ToolDefinition {
                name: "start_workspace_job".to_string(),
                description: "Start a restart-safe long-running command in a workspace and return immediately with a durable job id. Supply exactly one of command or argv. Retries must reuse the same idempotency_key; a reused key cannot submit duplicate work.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "idempotency_key"],
                    "properties": {
                        "command": {"type": "string", "description": "Shell command. Mutually exclusive with argv."},
                        "argv": {"type": "array", "items": {"type": "string"}, "description": "Argument vector, safely shell-quoted. Mutually exclusive with command."},
                        "workspace_id": {"type": "string", "description": "Workspace UUID. Defaults to the assistant default workspace."},
                        "mission_id": {"type": "string", "description": "Owning mission UUID."},
                        "cwd": {"type": "string", "description": "Working directory relative to the workspace root."},
                        "timeout_secs": {"type": "integer", "description": "Absolute runtime limit. Default 7200 seconds, maximum 86400."},
                        "resource_class": {"type": "string", "description": "Scheduling hint such as lean_heavy, cpu, or io."},
                        "idempotency_key": {"type": "string", "description": "Stable retry key for this logical submission."}
                    }
                }),
            },
            ToolDefinition {
                name: "get_workspace_job".to_string(),
                description: "Inspect a durable workspace job by id, including heartbeat, deadline, scope, exit status, and bounded stdout/stderr tails.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {
                        "job_id": {"type": "string"},
                        "tail_bytes": {"type": "integer", "description": "Maximum bytes from each log, default 16384, maximum 65536."}
                    }
                }),
            },
            ToolDefinition {
                name: "cancel_workspace_job".to_string(),
                description: "Idempotently request cancellation of a durable workspace job. Returns after cancellation is recorded; process teardown continues asynchronously.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {"job_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "get_mission_health".to_string(),
                description: "Diagnose where a mission stands: live run state, stall severity, detected error signals (rate limit / auth / capacity / context-limit / network), suspected tool loops, the last assistant message, and a one-line recommendation. Use this first when babysitting a long-running mission — it summarizes 'where it is struggling' instead of making you read raw events.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string", "description": "Mission UUID, or an unambiguous leading fragment of one (the 8-character form dashboards and logs display)."}}
                }),
            },
            ToolDefinition {
                name: "get_mission_diagnostics".to_string(),
                description: "Deep-dive a mission: a compact timeline of the most recent tool calls (with result snippets), per-tool call counts, repeated/looping calls, and full error events. Use when get_mission_health flags a problem and you need to see exactly what the model is doing.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "limit": {"type": "integer", "description": "Trace events to scan from the tail, default 80, max 300."}
                    }
                }),
            },
            ToolDefinition {
                name: "update_mission_settings".to_string(),
                description: "Change a mission's run settings for its NEXT turn: switch backend (claudecode/codex/opencode/gemini/grok/chatgpt_ui), model, reasoning effort, fast mode, or agent. Applies between turns — the mission must be idle (awaiting_user/acknowledged/interrupted), not actively running. If it is running, cancel_mission first (or wait), then update, then send_message_to_mission or resume_mission to kick the next turn. model_effort applies to claudecode/codex; fast_mode applies only to Codex GPT-5.6/5.5/5.4 and consumes ChatGPT credits faster.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "backend": {"type": "string", "enum": ["opencode", "claudecode", "codex", "gemini", "grok", "chatgpt_ui"]},
                        "model_override": {"type": "string", "description": "Model id. Empty string clears it. When backend changes this is reset unless set explicitly."},
                        "model_effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                        "fast_mode": {"type": "boolean", "description": "Enable or disable Codex fast mode for future turns. Only backend=codex with GPT-5.6/5.5/5.4."},
                        "agent": {"type": "string", "description": "Agent name. Empty string clears it."},
                        "config_profile": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "resume_mission".to_string(),
                description: "Restart a mission that ended without finishing — interrupted, blocked or failed — by reconstructing context from history and the work directory, then running the next turn. This is the recovery path, not the only way to wake a mission: for a mission parked in awaiting_user or acknowledged, send_message_to_mission wakes it on the same id and is the normal choice. Pass `content` to steer the resume with a concrete hint (e.g. 'you still have budget — keep going until the build passes; do not stop to ask'). Without `content` it sends the default continue-where-you-left-off prompt.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "content": {"type": "string", "description": "Optional steering message used as the resume turn's prompt."},
                        "clean_workspace": {"type": "boolean", "description": "Wipe the work directory before resuming. Rarely needed; default false."}
                    }
                }),
            },
        ]
    }

    async fn list_missions(&self, params: ListMissionsParams) -> Result<Value, String> {
        let limit = params.limit.clamp(1, 100);
        // Forward filters to the API so it does the (paginated, scan-bounded)
        // matching server-side — filtering only the fetched page here would miss
        // matches outside the window on a larger fleet.
        let mut path = format!("/api/control/missions?limit={limit}&offset=0");
        if let Some(status) = params.status.as_deref() {
            path.push_str(&format!("&status={}", urlencoding::encode(status)));
        }
        if let Some(project) = params.project.as_deref() {
            path.push_str(&format!("&project={}", urlencoding::encode(project)));
        }
        if let Some(prefix) = params.project_prefix.as_deref() {
            path.push_str(&format!("&project_prefix={}", urlencoding::encode(prefix)));
        }
        if let Some(track) = params.track.as_deref() {
            path.push_str(&format!("&track={}", urlencoding::encode(track)));
        }
        if let Some(tag) = params.tag.as_deref() {
            path.push_str(&format!("&tag={}", urlencoding::encode(tag)));
        }
        if let Some(session) = params.origin_session_id.as_deref() {
            path.push_str(&format!(
                "&origin_session_id={}",
                urlencoding::encode(session)
            ));
        }
        let response = self.api_get(&path).await?;
        if !response.status().is_success() {
            return Err(format!("Failed to list missions: {}", response.status()));
        }
        let missions: Vec<Value> = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse missions: {error}"))?;
        let missions = missions
            .into_iter()
            .map(compact_mission_summary)
            .collect::<Vec<_>>();
        Ok(json!({ "missions": missions }))
    }

    /// Takes the whole filter set so an active-mission query can be narrowed
    /// exactly like a full listing (by conversation, family, track, …).
    async fn list_active_missions(&self, params: ListMissionsParams) -> Result<Value, String> {
        let ListMissionsParams {
            limit,
            project,
            project_prefix,
            track,
            tag,
            origin_session_id,
            ..
        } = params;
        let requested = limit.clamp(1, 100);
        // The API returns the most recent missions regardless of status, so a
        // narrow fetch limit can be fully consumed by recent completed missions
        // and starve the active filter below. Fetch a wider window than the
        // caller asked for, then filter and truncate to the requested count.
        let fetch_limit = requested.saturating_mul(4).clamp(50, 100);
        let mut result = self
            .list_missions(ListMissionsParams {
                status: None,
                limit: fetch_limit,
                project,
                project_prefix,
                track,
                tag,
                origin_session_id,
            })
            .await?;
        if let Some(missions) = result["missions"].as_array_mut() {
            missions.retain(|mission| {
                matches!(
                    mission["status"].as_str(),
                    Some("active" | "pending" | "awaiting_user" | "blocked")
                )
            });
            missions.truncate(requested);
        }
        Ok(result)
    }

    async fn get_mission(&self, params: MissionIdParams) -> Result<Value, String> {
        // Keep the legacy tool name safe for autonomous controllers. Models
        // routinely chose `get_mission` despite its former "heavy" warning,
        // pulling entire transcripts into every reconciliation tick. Detailed
        // history remains available through the bounded/paginated events tool.
        self.get_mission_digest(params).await
    }

    async fn get_mission_digest(&self, params: MissionIdParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let response = self
            .api_get(&format!("/api/control/missions/{id}/digest"))
            .await?;
        if !response.status().is_success() {
            return Err(format!("Mission not found: {}", response.status()));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Failed to parse mission digest: {error}"))
    }

    async fn get_mission_events(&self, params: MissionEventsParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let limit = params.limit.clamp(1, 200);
        // Validate against the declared enum rather than interpolating a
        // free-form string into the URL, which would let a caller smuggle
        // extra query parameters (e.g. `all&foo=bar`) into the internal request.
        let view = match params.view.as_deref() {
            None | Some("transcript") => "transcript",
            Some("trace") => "trace",
            Some("history") => "history",
            Some("all") => "all",
            Some(other) => {
                return Err(format!(
                    "Invalid view '{other}'; expected one of: transcript, trace, history, all"
                ))
            }
        };
        let path = mission_events_path(id, limit, view, params.before_seq, params.since_seq);
        let response = self.api_get(&path).await?;
        if !response.status().is_success() {
            return Err(format!(
                "Failed to fetch mission events: {}",
                response.status()
            ));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Failed to parse mission events: {error}"))
    }

    async fn get_chatgpt_ui_pool_status(&self) -> Result<Value, String> {
        let response = self
            .api_get("/api/backends/chatgpt_ui/profile-pool")
            .await?;
        Self::response_value(response, "fetch ChatGPT UI pool status").await
    }

    async fn list_mission_shared_files(
        &self,
        params: MissionSharedFilesParams,
    ) -> Result<Value, String> {
        let mission_id = self.resolve_mission_id(&params.mission_id).await?;
        // Page backwards from the end of the transcript: shared files are
        // "current attachments", so we must scan the NEWEST `limit` events —
        // the default (no cursor) pagination returns the oldest rows and would
        // silently drop recent attachments on long missions.
        let events = self
            .get_mission_events(MissionEventsParams {
                mission_id: mission_id.to_string(),
                limit: params.limit.clamp(1, 200),
                view: Some("transcript".to_string()),
                since_seq: None,
                before_seq: Some(i64::MAX),
            })
            .await?;
        let mut files = Vec::new();
        for event in events.as_array().into_iter().flatten() {
            let Some(shared_files) = event
                .get("metadata")
                .and_then(|metadata| metadata.get("shared_files"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for file in shared_files {
                let mut item = file.clone();
                if let Some(object) = item.as_object_mut() {
                    object.insert("mission_id".to_string(), json!(mission_id.to_string()));
                    if let Some(sequence) = event.get("sequence").cloned() {
                        object.insert("event_sequence".to_string(), sequence);
                    }
                    if let Some(timestamp) = event.get("timestamp").cloned() {
                        object.insert("event_timestamp".to_string(), timestamp);
                    }
                }
                files.push(item);
            }
        }
        Ok(json!({ "mission_id": mission_id.to_string(), "shared_files": files }))
    }

    async fn download_shared_file(
        &self,
        params: DownloadSharedFileParams,
    ) -> Result<Value, String> {
        let mission_id = self.resolve_mission_id(&params.mission_id).await?;
        let path = shared_file_download_path(&params.url)?;
        let filename = params
            .filename
            .or_else(|| shared_file_name_from_url(&params.url))
            .unwrap_or_else(|| "artifact".to_string());
        let filename = sanitize_filename(&filename);
        let output_dir = output_dir_for_shared_file(&mission_id, params.output_dir)?;
        tokio::fs::create_dir_all(&output_dir)
            .await
            .map_err(|error| format!("Failed to create artifact directory: {error}"))?;
        let output_path = output_dir.join(filename);
        let bytes = self.api_get_bytes(&path).await?;
        tokio::fs::write(&output_path, &bytes)
            .await
            .map_err(|error| format!("Failed to write shared file: {error}"))?;
        Ok(json!({
            "mission_id": mission_id.to_string(),
            "path": output_path.to_string_lossy(),
            "bytes": bytes.len(),
        }))
    }

    async fn start_mission(&self, params: StartMissionParams) -> Result<Value, String> {
        let workspace_id = resolve_default_workspace_id(params.workspace_id);
        let backend = params
            .backend
            .or_else(|| native_backend_from_agent(params.agent.as_deref()));
        let tags = mission_start_tags(
            params.tags,
            params.request_merge_authority.unwrap_or(false),
            params.writer.unwrap_or(false),
            params.github_pr.as_deref(),
            MergeGrantConfig::from_environment().as_ref(),
        )?;
        // An explicitly supplied origin session must be usable for routing:
        // silently dropping a blank/malformed id would create exactly the
        // unreachable mission this field exists to prevent. Only omitting the
        // field opts out.
        let origin_session_id = match params.origin_session_id.as_deref().map(str::trim) {
            Some(session) if valid_origin_session_id(session) => Some(session),
            Some(session) => {
                return Err(format!(
                    "origin_session_id `{session}` is invalid: use 1-128 chars of \
                     [A-Za-z0-9._:-]"
                ))
            }
            None => None,
        };
        let body = json!({
            "title": params.title,
            "workspace_id": workspace_id,
            "backend": backend,
            "model_override": params.model_override,
            "model_effort": params.model_effort,
            "fast_mode": params.fast_mode.unwrap_or(false),
            "config_profile": params.config_profile,
            "agent": params.agent,
            // Atomic create+start: the API stores the prompt as the mission's
            // deferred goal and the scheduler dispatches it as soon as
            // capacity allows. The old create-then-send_message pattern could
            // be dropped at capacity, leaving zombie Pending missions.
            "prompt": params.prompt,
            // Project tagging at creation so the mission isn't born with null
            // metadata (Paloma watchdogs then route by these, not titles).
            "project": params.project,
            "track": params.track,
            "intent": params.intent,
            "github_pr": params.github_pr,
            "writer": params.writer,
            "tags": tags,
            "desired_state": params.desired_state,
            "next_check_at": params.next_check_at,
            "estimated_disk_gib": params.estimated_disk_gib,
            // Mission-level provenance. `origin` is fixed by this server (not
            // model-controlled); the session id ties the mission to the Hermes
            // conversation that spawned it, and the mission-status webhook
            // carries it back so the result reaches that conversation.
            "origin": "hermes",
            "origin_session_id": origin_session_id,
        });
        let response = self.api_post("/api/control/missions", body).await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to create mission: {text}"));
        }
        let mission: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse created mission: {error}"))?;
        if mission["id"].as_str().is_none() {
            return Err("Created mission response did not include an id".to_string());
        }
        Ok(json!({ "mission": mission }))
    }

    /// Run a bash command through `POST /api/workspaces/:id/exec`, which
    /// executes in the workspace context with its configured `env_vars`
    /// merged in (host: process env; container: --setenv). This gives the
    /// assistant mission-equivalent access to workspace secrets without
    /// copying them into the gateway's own service environment.
    async fn workspace_bash(&self, params: WorkspaceBashParams) -> Result<Value, String> {
        if params.command.trim().is_empty() {
            return Err("Command is empty".to_string());
        }
        if is_heavy_workspace_command(&params.command) {
            return Err(serde_json::to_string(&json!({
                "error": "heavy_command_requires_durable_job",
                "message": "This command can outlive a synchronous Hermes MCP call. Use start_workspace_job and poll get_workspace_job.",
                "tool": "start_workspace_job"
            })).unwrap_or_else(|_| "heavy command requires start_workspace_job".to_string()));
        }
        let workspace_id = resolve_default_workspace_id(params.workspace_id).ok_or_else(|| {
            "No workspace_id given and no default workspace configured \
             (HERMES_DEFAULT_WORKSPACE_ID / ASSISTANT_DEFAULT_WORKSPACE_ID)"
                .to_string()
        })?;
        let id = parse_uuid(&workspace_id)?;
        let response = self
            .api_post(
                &format!("/api/workspaces/{id}/exec"),
                json!({
                    "command": params.command,
                    "cwd": params.cwd,
                    "timeout_secs": params.timeout_secs.unwrap_or(60).clamp(1, 120),
                }),
            )
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Workspace exec failed ({status}): {text}"));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Failed to parse exec result: {error}"))
    }

    async fn start_workspace_job(&self, params: StartWorkspaceJobParams) -> Result<Value, String> {
        let workspace_id = resolve_default_workspace_id(params.workspace_id).ok_or_else(|| {
            "No workspace_id given and no default workspace configured (HERMES_DEFAULT_WORKSPACE_ID / ASSISTANT_DEFAULT_WORKSPACE_ID)".to_string()
        })?;
        let workspace_id = parse_uuid(&workspace_id)?;
        let mission_id = self.resolve_mission_id(&params.mission_id).await?;
        let command = workspace_job_command(params.command, params.argv)?;
        let key = params.idempotency_key.trim();
        if key.is_empty() {
            return Err("idempotency_key is required".to_string());
        }
        let response = self
            .api_post(
                "/api/durable-jobs",
                json!({
                    "command": command,
                    "cwd": params.cwd,
                    "workspace_id": workspace_id,
                    "started_by_mission_id": mission_id,
                    "timeout_secs": params.timeout_secs.unwrap_or(7200).clamp(1, 86400),
                    "resource_class": params.resource_class,
                    "idempotency_key": key,
                }),
            )
            .await?;
        let job = Self::response_value(response, "Start workspace job").await?;
        Ok(json!({"job": compact_workspace_job(job)}))
    }

    async fn get_workspace_job(&self, params: WorkspaceJobParams) -> Result<Value, String> {
        let id = parse_uuid(&params.job_id)?;
        let response = self.api_get(&format!("/api/durable-jobs/{id}")).await?;
        let job = Self::response_value(response, "Get workspace job").await?;
        let tail_bytes = params.tail_bytes.clamp(1, 64 * 1024);
        let response = self
            .api_get(&format!(
                "/api/durable-jobs/{id}/logs?tail_bytes={tail_bytes}"
            ))
            .await?;
        let logs = Self::response_value(response, "Get workspace job logs").await?;
        Ok(json!({"job": compact_workspace_job(job), "logs": logs}))
    }

    async fn cancel_workspace_job(&self, params: WorkspaceJobParams) -> Result<Value, String> {
        let id = parse_uuid(&params.job_id)?;
        let response = self
            .api_post(&format!("/api/durable-jobs/{id}/cancel"), json!({}))
            .await?;
        let job = Self::response_value(response, "Cancel workspace job").await?;
        Ok(json!({"job": compact_workspace_job(job), "cancellation_acknowledged": true}))
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let response = self
            .api_post(
                "/api/control/message",
                json!({
                    "mission_id": id.to_string(),
                    "content": params.content,
                }),
            )
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to send message: {text}"));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Failed to parse send result: {error}"))
    }

    async fn ask_mission(&self, params: AskMissionParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let mut body = json!({
            "content": params.content,
            "sandbox": params.sandbox,
        });
        if let Some(tid) = params.thread_id.as_deref() {
            let tid = parse_uuid(tid)?;
            body["thread_id"] = json!(tid.to_string());
        }
        // Ask turns make multiple sequential LLM/tool calls, so the shared
        // client default of 120s would abort long asks. But TWO budgets sit
        // above this call, and both are 600s: the Hermes-side MCP request
        // timeout, and the cron scheduler's idle watchdog — which counts from
        // the moment the tool STARTS, so id resolution, connect time and this
        // whole request all spend it. At 570s the margin was 30s; measured
        // 2026-08-06 05:18, a busy mission ate it and the watchdog killed the
        // whole tick ("idle for 600s — last activity: executing tool:
        // ask_mission"), turning one slow answer into a failed controller
        // run. 450s keeps ample room for real asks and returns a clean
        // "mission busy" error while both outer budgets still have 150s left.
        let response = self
            .api_post_with_timeout(
                &format!("/api/control/missions/{id}/ask"),
                body,
                Some(std::time::Duration::from_secs(450)),
            )
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to ask mission ({status}): {text}"));
        }
        let result: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse ask result: {error}"))?;
        // Return just the answer + thread_id (drop the full message history to
        // keep the tool result compact).
        Ok(json!({
            "thread_id": result.get("thread_id").cloned().unwrap_or(Value::Null),
            "answer": result.get("answer").cloned().unwrap_or(Value::Null),
        }))
    }

    async fn answer_mission_question(
        &self,
        params: AnswerMissionQuestionParams,
    ) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        if params.answers.is_empty() || params.answers.iter().any(Vec::is_empty) {
            return Err(
                "answers must contain one non-empty inner array per question, e.g. [[\"Option A\"]]"
                    .to_string(),
            );
        }
        let tool_call_id =
            match params
                .tool_call_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(explicit) => explicit.to_string(),
                None => {
                    // Page backwards from the end of the transcript: the pending
                    // question is by definition among the NEWEST events, and the
                    // default (no cursor) pagination returns the oldest rows.
                    let events = self
                        .get_mission_events(MissionEventsParams {
                            mission_id: id.to_string(),
                            limit: 200,
                            view: Some("transcript".to_string()),
                            since_seq: None,
                            before_seq: Some(i64::MAX),
                        })
                        .await?;
                    let empty = Vec::new();
                    let events = events.as_array().unwrap_or(&empty);
                    let pending = pending_ask_user_questions(events);
                    match pending.as_slice() {
                        [] => return Err(
                            "No unanswered AskUserQuestion found in the mission's recent events. \
                             If the mission is merely idle or awaiting an ack, use \
                             send_message_to_mission / acknowledge_mission instead."
                                .to_string(),
                        ),
                        [only] => only.tool_call_id.clone(),
                        many => {
                            let listing = many
                                .iter()
                                .map(|question| {
                                    format!("{} — {}", question.tool_call_id, question.summary())
                                })
                                .collect::<Vec<_>>()
                                .join("; ");
                            return Err(format!(
                                "Multiple unanswered AskUserQuestion calls are pending; pass \
                             tool_call_id explicitly. Pending (newest first): {listing}"
                            ));
                        }
                    }
                }
            };
        let body = json!({
            "tool_call_id": tool_call_id,
            "name": "AskUserQuestion",
            "result": { "answers": params.answers },
        });
        let response = self.api_post("/api/control/tool_result", body).await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to submit answer ({status}): {text}"));
        }
        let result: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse tool_result response: {error}"))?;
        // `delivered: false` means no live turn was parked on that call (the
        // mission ended or was interrupted) and the answer was dropped.
        let delivered = result
            .get("delivered")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut out = json!({
            "mission_id": id.to_string(),
            "tool_call_id": tool_call_id,
            "delivered": delivered,
        });
        if !delivered {
            out["note"] = json!(
                "The answer did not reach a live parked turn — the mission has likely ended or \
                 been interrupted. Use resume_mission or send_message_to_mission to wake it, \
                 then answer again if it re-asks."
            );
        }
        Ok(out)
    }

    async fn cancel_mission(&self, params: MissionIdParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let response = self
            .api_post(&format!("/api/control/missions/{id}/cancel"), json!({}))
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to cancel mission: {text}"));
        }
        Ok(json!({ "success": true, "cancelled": id.to_string() }))
    }

    async fn adopt_mission(&self, params: AdoptMissionParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        // The session id is stamped by the sandboxed-origin-session plugin,
        // exactly as for start_mission. Refuse to adopt into nothing: an
        // adoption without a session would silently CLEAR the mission's
        // origin, which is the opposite of what the caller wanted.
        let session = params
            .origin_session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "adopt_mission needs the calling session's id; it is normally \
                 injected automatically. If you see this, the origin-session \
                 plugin did not stamp the call — report that rather than \
                 passing a session id by hand."
                    .to_string()
            })?;
        if !valid_origin_session_id(session) {
            return Err(format!(
                "origin_session_id {session:?} is not a valid session id"
            ));
        }
        let response = self
            .api_post(
                &format!("/api/control/missions/{id}/origin"),
                json!({ "origin": "hermes", "origin_session_id": session }),
            )
            .await?;
        let mission = Self::response_value(response, "adopt mission").await?;
        Ok(json!({
            "success": true,
            "adopted": id.to_string(),
            "origin_session_id": session,
            "mission": mission,
            "note": "This conversation now receives the mission's completion callback; do not poll.",
        }))
    }

    async fn acknowledge_mission(&self, params: MissionIdParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let response = self
            .api_get(&format!("/api/control/missions/{id}/digest"))
            .await?;
        let digest = Self::response_value(response, "Read mission before ACK").await?;
        if !mission_requires_acknowledgement(&digest)? {
            return Ok(json!({
                "success": true,
                "acknowledged": id.to_string(),
                "already_acknowledged": true,
            }));
        }

        let response = self
            .api_post(
                &format!("/api/control/missions/{id}/status"),
                json!({"status": "acknowledged"}),
            )
            .await?;
        Self::response_value(response, "Acknowledge mission").await?;

        // The status mutation response is only an operation envelope. Re-read
        // the mission so controllers never mistake a successful HTTP response
        // for a confirmed state transition.
        let response = self
            .api_get(&format!("/api/control/missions/{id}/digest"))
            .await?;
        let digest = Self::response_value(response, "Verify mission ACK").await?;
        let mission = digest.get("mission").unwrap_or(&digest);
        if mission.get("status").and_then(Value::as_str) != Some("acknowledged") {
            return Err(
                "Mission ACK mutation succeeded but readback is not acknowledged".to_string(),
            );
        }

        Ok(json!({
            "success": true,
            "acknowledged": id.to_string(),
            "already_acknowledged": false,
        }))
    }

    async fn get_compute_fleet(&self) -> Result<Value, String> {
        let response = self.api_get("/api/remote-nodes").await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to get compute fleet ({status}): {text}"));
        }
        let fleet: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse compute fleet: {error}"))?;
        Ok(compact_compute_fleet(&fleet))
    }

    // ---- Project roster tools (see projects_store.rs / projects_overview.rs) ----

    async fn list_projects(&self) -> Result<Value, String> {
        // Reuse the overview endpoint, but return only the light roster fields
        // a controller needs at the top of a tick — not the full board payload.
        let response = self.api_get("/api/projects/overview").await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to list projects ({status}): {text}"));
        }
        let overview: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse projects: {error}"))?;
        let rows: Vec<Value> = overview
            .get("projects")
            .and_then(Value::as_array)
            .map(|projects| {
                projects
                    .iter()
                    .map(|p| {
                        let latest = p.get("latest_update");
                        json!({
                            "slug": p.get("slug"),
                            "bucket": p.get("bucket"),
                            "mode": latest.and_then(|u| u.get("mode")),
                            "health": p.get("health").and_then(|h| h.get("tracks_needing_attention")),
                            "latest_at": latest.and_then(|u| u.get("at")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "projects": rows }))
    }

    async fn get_project(&self, params: ProjectSlugParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        let response = self.api_get(&format!("/api/projects/{slug}")).await?;
        Self::response_value(response, "get project").await
    }

    async fn update_project_status(
        &self,
        params: UpdateProjectStatusParams,
    ) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let body = json!({
            "mode": params.mode,
            "next_action": params.next_action,
            "blocker": params.blocker,
        });
        let response = self
            .api_post(&format!("/api/projects/{slug}/status"), body)
            .await?;
        Self::response_value(response, "update project status").await
    }

    async fn set_project_track(&self, params: SetProjectTrackParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let body = json!({
            "track": params.track,
            "desired_state": params.desired_state,
            "status": params.status,
        });
        let response = self
            .api_post(&format!("/api/projects/{slug}/track"), body)
            .await?;
        Self::response_value(response, "set project track").await
    }

    async fn get_project_grant(&self, params: ProjectSlugParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        let response = self.api_get(&format!("/api/projects/{slug}/grant")).await?;
        Self::response_value(response, "get project grant").await
    }

    async fn set_project_grant(&self, params: SetProjectGrantParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let body = json!({
            "merge_authority": params.merge_authority,
            "budget_per_tick": params.budget_per_tick,
            "parallel_missions": params.parallel_missions,
            "pause_reason": params.pause_reason,
            "resume_condition": params.resume_condition,
            "material_bar": params.material_bar,
            "autonomy_level": params.autonomy_level,
        });
        let response = self
            .api_post(&format!("/api/projects/{slug}/grant"), body)
            .await?;
        Self::response_value(response, "set project grant").await
    }

    async fn record_project_decision(
        &self,
        params: RecordProjectDecisionParams,
    ) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let body = json!({
            "question": params.question,
            "rationale": params.rationale,
            "kind": params.kind,
            "authority": params.authority,
            "status": params.status,
            "evidence": params.evidence,
        });
        let response = self
            .api_post(&format!("/api/projects/{slug}/decision"), body)
            .await?;
        Self::response_value(response, "record project decision").await
    }

    async fn answer_project_decision(
        &self,
        params: AnswerProjectDecisionParams,
    ) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let body = json!({ "at": params.at, "answer": params.answer });
        let response = self
            .api_post(&format!("/api/projects/{slug}/decision/answer"), body)
            .await?;
        Self::response_value(response, "answer project decision").await
    }

    async fn get_project_tasks(&self, params: ProjectSlugParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        let response = self.api_get(&format!("/api/projects/{slug}/tasks")).await?;
        Self::response_value(response, "get project tasks").await
    }

    async fn plan_project_tasks(&self, params: PlanProjectTasksParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let tasks: Vec<Value> = params
            .tasks
            .iter()
            .map(|task| {
                json!({
                    "task_key": task.task_key,
                    "title": task.title,
                    "prompt": task.prompt,
                    "acceptance_criteria": task.acceptance_criteria,
                    "depends_on": task.depends_on,
                })
            })
            .collect();
        let response = self
            .api_post(
                &format!("/api/projects/{slug}/tasks"),
                json!({ "tasks": tasks }),
            )
            .await?;
        Self::response_value(response, "plan project tasks").await
    }

    async fn update_project_task(&self, params: UpdateProjectTaskParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let task_key = params.task_key.trim();
        let response = self
            .api_patch(
                &format!("/api/projects/{slug}/tasks/{task_key}"),
                json!({
                    "title": params.title,
                    "prompt": params.prompt,
                    "acceptance_criteria": params.acceptance_criteria,
                    "depends_on": params.depends_on,
                }),
            )
            .await?;
        Self::response_value(response, "update project task").await
    }

    async fn cancel_project_task(&self, params: CancelProjectTaskParams) -> Result<Value, String> {
        let slug = params.slug.trim();
        self.assert_project_scope(slug)?;
        let task_key = params.task_key.trim();
        let response = self
            .api_delete(&format!("/api/projects/{slug}/tasks/{task_key}"))
            .await?;
        Self::response_value(response, "cancel project task").await
    }

    async fn link_mission_to_project(
        &self,
        params: LinkMissionToProjectParams,
    ) -> Result<Value, String> {
        self.assert_project_scope(&params.slug)?;
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let mut body = serde_json::Map::new();
        body.insert("project".to_string(), json!(params.slug.trim()));
        if let Some(track) = params
            .track
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            body.insert("track".to_string(), json!(track));
        }
        let response = self
            .api_post(
                &format!("/api/control/missions/{id}/project"),
                Value::Object(body),
            )
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to link mission to project ({status}): {text}"
            ));
        }
        Ok(json!({ "success": true, "mission_id": id.to_string(), "project": params.slug.trim() }))
    }

    async fn list_workspaces(&self) -> Result<Value, String> {
        let response = self.api_get("/api/workspaces").await?;
        if !response.status().is_success() {
            return Err(format!("Failed to list workspaces: {}", response.status()));
        }
        let workspaces: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse workspaces: {error}"))?;
        Ok(json!({ "workspaces": workspaces }))
    }

    async fn get_workspace(&self, params: WorkspaceIdParams) -> Result<Value, String> {
        let id = parse_uuid(&params.workspace_id)?;
        let response = self.api_get(&format!("/api/workspaces/{id}")).await?;
        let workspace = Self::response_value(response, "Get workspace").await?;
        Ok(json!({"workspace": workspace}))
    }

    async fn create_workspace(&self, params: CreateWorkspaceParams) -> Result<Value, String> {
        let body = create_workspace_body(params)?;
        let response = self.api_post("/api/workspaces", body).await?;
        let workspace = Self::response_value(response, "Create workspace").await?;
        Ok(json!({"workspace": workspace}))
    }

    async fn update_workspace(&self, params: UpdateWorkspaceParams) -> Result<Value, String> {
        let (id, body) = update_workspace_body(params)?;
        let response = self.api_put(&format!("/api/workspaces/{id}"), body).await?;
        let workspace = Self::response_value(response, "Update workspace").await?;
        Ok(json!({"workspace": workspace}))
    }

    async fn delete_workspace(&self, params: DeleteWorkspaceParams) -> Result<Value, String> {
        if !params.confirm {
            return Err("Workspace deletion requires confirm=true".to_string());
        }
        let id = parse_uuid(&params.workspace_id)?;
        let response = self.api_delete(&format!("/api/workspaces/{id}")).await?;
        Self::response_value(response, "Delete workspace").await?;
        Ok(json!({"deleted": true, "workspace_id": id.to_string()}))
    }

    async fn list_workspace_templates(&self) -> Result<Value, String> {
        let response = self.api_get("/api/library/workspace-template").await?;
        let templates = Self::response_value(response, "List workspace templates").await?;
        Ok(json!({"templates": templates}))
    }

    async fn get_workspace_template(
        &self,
        params: WorkspaceTemplateNameParams,
    ) -> Result<Value, String> {
        let name = require_resource_name(&params.name, "Workspace template")?;
        let encoded = urlencoding::encode(&name);
        let response = self
            .api_get(&format!("/api/library/workspace-template/{encoded}"))
            .await?;
        let template = Self::response_value(response, "Get workspace template").await?;
        Ok(json!({"template": template}))
    }

    async fn save_workspace_template(
        &self,
        params: SaveWorkspaceTemplateParams,
    ) -> Result<Value, String> {
        let name = require_resource_name(&params.name, "Workspace template")?;
        let encoded = urlencoding::encode(&name);
        let path = format!("/api/library/workspace-template/{encoded}");
        let response = self.api_get(&path).await?;
        let has_env_override = params.env_vars.is_some();
        let mut template = if response.status().is_success() {
            let current = Self::response_value(response, "Get workspace template").await?;
            if !has_env_override
                && current
                    .get("env_vars")
                    .and_then(Value::as_object)
                    .is_some_and(|env| {
                        env.values().any(|value| {
                            value
                                .as_str()
                                .is_some_and(|raw| raw.starts_with("[DECRYPTION_FAILED]"))
                        })
                    })
            {
                return Err(
                    "Template contains env vars that could not be decrypted; supply a complete env_vars replacement before saving"
                        .to_string(),
                );
            }
            current.as_object().cloned().ok_or_else(|| {
                "Existing workspace template response was not an object".to_string()
            })?
        } else if response.status() == reqwest::StatusCode::NOT_FOUND {
            serde_json::Map::new()
        } else {
            return match Self::response_value(response, "Get workspace template").await {
                Err(error) => Err(error),
                Ok(_) => Err("Unexpected successful template response".to_string()),
            };
        };
        template.remove("name");
        template.remove("path");
        apply_template_patch(&mut template, params)?;

        let response = self.api_put(&path, Value::Object(template)).await?;
        Self::response_value(response, "Save workspace template").await?;
        let response = self.api_get(&path).await?;
        let saved = Self::response_value(response, "Read saved workspace template").await?;
        Ok(json!({"saved": true, "template": saved}))
    }

    async fn delete_workspace_template(
        &self,
        params: DeleteWorkspaceTemplateParams,
    ) -> Result<Value, String> {
        if !params.confirm {
            return Err("Template deletion requires confirm=true".to_string());
        }
        let name = require_resource_name(&params.name, "Workspace template")?;
        let encoded = urlencoding::encode(&name);
        let response = self
            .api_delete(&format!("/api/library/workspace-template/{encoded}"))
            .await?;
        Self::response_value(response, "Delete workspace template").await?;
        Ok(json!({"deleted": true, "name": name}))
    }

    async fn rebuild_workspace_from_template(
        &self,
        params: RebuildWorkspaceFromTemplateParams,
    ) -> Result<Value, String> {
        if !params.confirm {
            return Err("Forced workspace rebuild requires confirm=true".to_string());
        }
        let id = parse_uuid(&params.workspace_id)?;
        let apply_response = self
            .api_post(
                &format!("/api/workspaces/{id}/apply-template"),
                json!({"template": params.template}),
            )
            .await?;
        let applied = Self::response_value(apply_response, "Apply workspace template").await?;

        let build_response = self
            .api_post(
                &format!("/api/workspaces/{id}/build"),
                json!({"rebuild": true}),
            )
            .await?;
        let build = Self::response_value(build_response, "Rebuild workspace").await?;
        Ok(json!({
            "workspace_id": id.to_string(),
            "template_applied": applied.get("template").cloned().unwrap_or(Value::Null),
            "status": build.get("status").cloned().unwrap_or(Value::Null),
            "rebuild_started": true,
        }))
    }

    async fn update_mission_settings(&self, params: UpdateSettingsParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let mut body = serde_json::Map::new();
        if let Some(backend) = params
            .backend
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body.insert("backend".to_string(), json!(backend));
        }
        // model_override / model_effort / agent / config_profile use a "patch"
        // deserializer on the server: present (incl. empty string) = set/clear,
        // omitted = leave unchanged. So only insert what the caller provided.
        if let Some(model_override) = params.model_override {
            body.insert("model_override".to_string(), json!(model_override));
        }
        if let Some(model_effort) = params.model_effort {
            body.insert("model_effort".to_string(), json!(model_effort));
        }
        if let Some(fast_mode) = params.fast_mode {
            body.insert("fast_mode".to_string(), json!(fast_mode));
        }
        if let Some(agent) = params.agent {
            body.insert("agent".to_string(), json!(agent));
        }
        if let Some(config_profile) = params.config_profile {
            body.insert("config_profile".to_string(), json!(config_profile));
        }
        if body.is_empty() {
            return Err("No settings provided. Set at least one of: backend, \
                        model_override, model_effort, fast_mode, agent, config_profile."
                .to_string());
        }
        let response = self
            .api_patch(
                &format!("/api/control/missions/{id}/settings"),
                Value::Object(body),
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 409 {
                return Err(format!(
                    "Mission is running, so settings cannot change mid-turn ({text}). \
                     Cancel it with cancel_mission (or wait for it to reach awaiting_user), \
                     update settings, then resume_mission or send_message_to_mission to start \
                     the next turn on the new backend."
                ));
            }
            return Err(format!(
                "Failed to update mission settings ({status}): {text}"
            ));
        }
        let mission: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse updated mission: {error}"))?;
        Ok(json!({ "mission": compact_mission_summary(mission) }))
    }

    async fn resume_mission(&self, params: ResumeMissionParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let hint = params
            .content
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let has_hint = hint.is_some();
        // With a steering hint we suppress the default resume prompt and deliver
        // our own message as the next turn instead.
        let response = self
            .api_post(
                &format!("/api/control/missions/{id}/resume"),
                json!({ "clean_workspace": params.clean_workspace, "skip_message": has_hint }),
            )
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to resume mission ({status}): {text}. \
                 Only interrupted, blocked, or failed missions can be resumed."
            ));
        }
        let mission: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse resumed mission: {error}"))?;
        // If we have a steering hint, deliver it as the next turn. If the post
        // fails, the mission is already active — surface that as a soft warning
        // (not an error) so the caller knows resume succeeded but the hint did
        // not land. They can retry the hint without re-resuming.
        let steer_warning = if let Some(content) = hint {
            match self
                .send_message(SendMessageParams {
                    mission_id: id.to_string(),
                    content,
                })
                .await
            {
                Ok(_) => None,
                Err(error) => Some(format!(
                    "Mission resumed, but steering hint could not be delivered: {error}. \
                     The mission is already active; retry send_message_to_mission to land \
                     the hint."
                )),
            }
        } else {
            None
        };
        let response_body = json!({
            "mission": compact_mission_summary(mission),
            "steered": has_hint && steer_warning.is_none(),
            "steer_warning": steer_warning,
        });
        Ok(response_body)
    }

    /// Fetch the live runner entry for one mission from `/api/control/running`,
    /// or `Value::Null` if the mission is not currently running (idle/finished).
    ///
    /// Network failures and 5xx responses are propagated as `Err` so callers can
    /// surface them — masking them as "not running" would make a stalled but
    /// still-active mission look healthy.
    async fn find_running_info(&self, mission_id: &Uuid) -> Result<Value, String> {
        let response = self.api_get("/api/control/running").await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to fetch live runner state ({status}): {text}"
            ));
        }
        let running: Value = response
            .json()
            .await
            .map_err(|error| format!("Failed to parse running missions: {error}"))?;
        let needle = mission_id.to_string();
        let found = running
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry.get("mission_id").and_then(Value::as_str) == Some(needle.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(found)
    }

    /// Most recent assistant message content (truncated), for judging whether a
    /// mission gave up early or finished cleanly.
    async fn last_assistant_message(&self, mission_id: &Uuid) -> Option<String> {
        let events = self
            .get_mission_events(MissionEventsParams {
                mission_id: mission_id.to_string(),
                limit: 12,
                view: Some("transcript".to_string()),
                since_seq: None,
                before_seq: Some(i64::MAX),
            })
            .await
            .ok()?;
        events
            .as_array()?
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.get("event_type").and_then(Value::as_str),
                    Some("assistant_message" | "assistant_message_canonical")
                )
            })
            .and_then(|event| event.get("content").and_then(Value::as_str))
            .map(|content| truncate_snippet(content, 600))
    }

    async fn get_mission_health(&self, params: MissionHealthParams) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let mission = self
            .get_mission(MissionIdParams {
                mission_id: id.to_string(),
            })
            .await?;
        let status = mission
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        // Live runner state is best-effort: a transient API error should not
        // blind the babysitter to the rest of the health picture. Surface the
        // error inline so the caller (and the recommendation) can see that
        // stall/health data is unavailable, rather than silently reporting
        // "no problems".
        let (live, live_warning) = match self.find_running_info(&id).await {
            Ok(live) => (live, None),
            Err(error) => (Value::Null, Some(error)),
        };
        let events = self
            .get_mission_events(MissionEventsParams {
                mission_id: id.to_string(),
                limit: 60,
                view: Some("trace".to_string()),
                since_seq: None,
                before_seq: Some(i64::MAX),
            })
            .await?;
        let empty = Vec::new();
        let events = events.as_array().unwrap_or(&empty);
        let analysis = analyze_trace_events(events);
        let last_assistant = self.last_assistant_message(&id).await;
        let backend = mission.get("backend").and_then(Value::as_str);
        let mut recommendation = build_recommendation(&status, backend, &live, &analysis);
        if let Some(warning) = &live_warning {
            recommendation =
                format!("{recommendation} (Note: live runner state unavailable — {warning})");
        }
        Ok(json!({
            "mission_id": id.to_string(),
            "title": mission.get("title").cloned().unwrap_or(Value::Null),
            "status": status,
            "backend": mission.get("backend").cloned().unwrap_or(Value::Null),
            "model_override": mission.get("model_override").cloned().unwrap_or(Value::Null),
            "model_effort": mission.get("model_effort").cloned().unwrap_or(Value::Null),
            "live": live,
            "live_warning": live_warning,
            "signals": analysis.signals_json(),
            "recent_errors": analysis.recent_errors,
            "suspected_loop": analysis.loop_json(),
            "trace_tool_calls": analysis.tool_call_count,
            "last_assistant_message": last_assistant,
            "recommendation": recommendation,
        }))
    }

    async fn get_mission_diagnostics(
        &self,
        params: MissionDiagnosticsParams,
    ) -> Result<Value, String> {
        let id = self.resolve_mission_id(&params.mission_id).await?;
        let limit = params.limit.clamp(10, 300);
        let events = self
            .get_mission_events(MissionEventsParams {
                mission_id: id.to_string(),
                limit,
                view: Some("trace".to_string()),
                since_seq: None,
                before_seq: Some(i64::MAX),
            })
            .await?;
        let empty = Vec::new();
        let events = events.as_array().unwrap_or(&empty);

        let mut timeline = Vec::new();
        let mut tool_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut repeat_counts: std::collections::BTreeMap<(String, String), usize> =
            std::collections::BTreeMap::new();
        let mut errors = Vec::new();

        for event in events {
            let event_type = event
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or("");
            match event_type {
                "tool_call" => {
                    let tool = event
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("(unknown)")
                        .to_string();
                    let args = event.get("content").and_then(Value::as_str).unwrap_or("");
                    *tool_counts.entry(tool.clone()).or_insert(0) += 1;
                    *repeat_counts
                        .entry((tool.clone(), args.trim().to_string()))
                        .or_insert(0) += 1;
                    timeline.push(json!({
                        "sequence": event.get("sequence").cloned().unwrap_or(Value::Null),
                        "tool": tool,
                        "args": truncate_snippet(args, 200),
                    }));
                }
                "error" => {
                    let content = event.get("content").and_then(Value::as_str).unwrap_or("");
                    errors.push(json!({
                        "sequence": event.get("sequence").cloned().unwrap_or(Value::Null),
                        "timestamp": event.get("timestamp").cloned().unwrap_or(Value::Null),
                        "content": truncate_snippet(content, 800),
                        "signals": error_signals_in(content),
                    }));
                }
                _ => {}
            }
        }

        // Keep the most recent slice of the tool timeline to bound output.
        let timeline_tail: Vec<Value> = timeline.iter().rev().take(30).rev().cloned().collect();
        let repeated: Vec<Value> = repeat_counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|((tool, args), count)| {
                json!({ "tool": tool, "repeats": count, "args": truncate_snippet(&args, 160) })
            })
            .collect();
        let tool_counts: Vec<Value> = tool_counts
            .into_iter()
            .map(|(tool, count)| json!({ "tool": tool, "count": count }))
            .collect();

        Ok(json!({
            "mission_id": id.to_string(),
            "events_scanned": events.len(),
            "tool_timeline": timeline_tail,
            "tool_counts": tool_counts,
            "repeated_calls": repeated,
            "errors": errors,
        }))
    }

    async fn handle_call(&self, name: &str, arguments: Value) -> Result<Value, String> {
        match name {
            "list_active_missions" => {
                let params: ListMissionsParams = parse_params(arguments)?;
                self.list_active_missions(params).await
            }
            "list_missions" => {
                let params: ListMissionsParams = parse_params(arguments)?;
                self.list_missions(params).await
            }
            "get_mission" => {
                let params: MissionIdParams = parse_params(arguments)?;
                self.get_mission(params).await
            }
            "get_mission_digest" => {
                let params: MissionIdParams = parse_params(arguments)?;
                self.get_mission_digest(params).await
            }
            "get_mission_events" => {
                let params: MissionEventsParams = parse_params(arguments)?;
                self.get_mission_events(params).await
            }
            "get_chatgpt_ui_pool_status" => self.get_chatgpt_ui_pool_status().await,
            "list_mission_shared_files" => {
                let params: MissionSharedFilesParams = parse_params(arguments)?;
                self.list_mission_shared_files(params).await
            }
            "download_shared_file" => {
                let params: DownloadSharedFileParams = parse_params(arguments)?;
                self.download_shared_file(params).await
            }
            "start_mission" => {
                let params: StartMissionParams = parse_params(arguments)?;
                self.start_mission(params).await
            }
            "send_message_to_mission" => {
                let params: SendMessageParams = parse_params(arguments)?;
                self.send_message(params).await
            }
            "ask_mission" => {
                let params: AskMissionParams = parse_params(arguments)?;
                self.ask_mission(params).await
            }
            "answer_mission_question" => {
                let params: AnswerMissionQuestionParams = parse_params(arguments)?;
                self.answer_mission_question(params).await
            }
            "cancel_mission" => {
                let params: MissionIdParams = parse_params(arguments)?;
                self.cancel_mission(params).await
            }
            "acknowledge_mission" => {
                let params: MissionIdParams = parse_params(arguments)?;
                self.acknowledge_mission(params).await
            }
            "adopt_mission" => {
                let params: AdoptMissionParams = parse_params(arguments)?;
                self.adopt_mission(params).await
            }
            "get_compute_fleet" => self.get_compute_fleet().await,
            "list_projects" => self.list_projects().await,
            "get_project" => {
                let params: ProjectSlugParams = parse_params(arguments)?;
                self.get_project(params).await
            }
            "update_project_status" => {
                let params: UpdateProjectStatusParams = parse_params(arguments)?;
                self.update_project_status(params).await
            }
            "set_project_track" => {
                let params: SetProjectTrackParams = parse_params(arguments)?;
                self.set_project_track(params).await
            }
            "get_project_grant" => {
                let params: ProjectSlugParams = parse_params(arguments)?;
                self.get_project_grant(params).await
            }
            "set_project_grant" => {
                let params: SetProjectGrantParams = parse_params(arguments)?;
                self.set_project_grant(params).await
            }
            "record_project_decision" => {
                let params: RecordProjectDecisionParams = parse_params(arguments)?;
                self.record_project_decision(params).await
            }
            "answer_project_decision" => {
                let params: AnswerProjectDecisionParams = parse_params(arguments)?;
                self.answer_project_decision(params).await
            }
            "get_project_tasks" => {
                let params: ProjectSlugParams = parse_params(arguments)?;
                self.get_project_tasks(params).await
            }
            "plan_project_tasks" => {
                let params: PlanProjectTasksParams = parse_params(arguments)?;
                self.plan_project_tasks(params).await
            }
            "update_project_task" => {
                let params: UpdateProjectTaskParams = parse_params(arguments)?;
                self.update_project_task(params).await
            }
            "cancel_project_task" => {
                let params: CancelProjectTaskParams = parse_params(arguments)?;
                self.cancel_project_task(params).await
            }
            "link_mission_to_project" => {
                let params: LinkMissionToProjectParams = parse_params(arguments)?;
                self.link_mission_to_project(params).await
            }
            "list_workspaces" => self.list_workspaces().await,
            "get_workspace" => {
                let params: WorkspaceIdParams = parse_params(arguments)?;
                self.get_workspace(params).await
            }
            "create_workspace" => {
                let params: CreateWorkspaceParams = parse_params(arguments)?;
                self.create_workspace(params).await
            }
            "update_workspace" => {
                let params: UpdateWorkspaceParams = parse_params(arguments)?;
                self.update_workspace(params).await
            }
            "delete_workspace" => {
                let params: DeleteWorkspaceParams = parse_params(arguments)?;
                self.delete_workspace(params).await
            }
            "list_workspace_templates" => self.list_workspace_templates().await,
            "get_workspace_template" => {
                let params: WorkspaceTemplateNameParams = parse_params(arguments)?;
                self.get_workspace_template(params).await
            }
            "save_workspace_template" => {
                let params: SaveWorkspaceTemplateParams = parse_params(arguments)?;
                self.save_workspace_template(params).await
            }
            "delete_workspace_template" => {
                let params: DeleteWorkspaceTemplateParams = parse_params(arguments)?;
                self.delete_workspace_template(params).await
            }
            "rebuild_workspace_from_template" => {
                let params: RebuildWorkspaceFromTemplateParams = parse_params(arguments)?;
                self.rebuild_workspace_from_template(params).await
            }
            "workspace_bash" => {
                let params: WorkspaceBashParams = parse_params(arguments)?;
                self.workspace_bash(params).await
            }
            "start_workspace_job" => {
                let params: StartWorkspaceJobParams = parse_params(arguments)?;
                self.start_workspace_job(params).await
            }
            "get_workspace_job" => {
                let params: WorkspaceJobParams = parse_params(arguments)?;
                self.get_workspace_job(params).await
            }
            "cancel_workspace_job" => {
                let params: WorkspaceJobParams = parse_params(arguments)?;
                self.cancel_workspace_job(params).await
            }
            "get_mission_health" => {
                let params: MissionHealthParams = parse_params(arguments)?;
                self.get_mission_health(params).await
            }
            "get_mission_diagnostics" => {
                let params: MissionDiagnosticsParams = parse_params(arguments)?;
                self.get_mission_diagnostics(params).await
            }
            "update_mission_settings" => {
                let params: UpdateSettingsParams = parse_params(arguments)?;
                self.update_mission_settings(params).await
            }
            "resume_mission" => {
                let params: ResumeMissionParams = parse_params(arguments)?;
                self.resume_mission(params).await
            }
            other => Err(format!("Unknown tool: {other}")),
        }
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => JsonRpcResponse::success(
                req.id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "sandboxed-hermes-assistant", "version": SERVER_VERSION},
                    "capabilities": {"tools": {}}
                }),
            ),
            "tools/list" => JsonRpcResponse::success(req.id, json!({ "tools": Self::tools() })),
            "tools/call" => {
                let Some(params) = req.params.as_object() else {
                    return JsonRpcResponse::error(req.id, -32602, "Invalid params");
                };
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return JsonRpcResponse::error(req.id, -32602, "Missing tool name");
                };
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.handle_call(name, arguments).await {
                    Ok(mut value) => {
                        scrub_sensitive_json(&mut value);
                        JsonRpcResponse::success(
                            req.id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                                }]
                            }),
                        )
                    }
                    Err(error) => JsonRpcResponse::error(req.id, -32000, error),
                }
            }
            _ => JsonRpcResponse::error(req.id, -32601, "Method not found"),
        }
    }
}

fn parse_uuid(raw: &str) -> Result<Uuid, String> {
    Uuid::parse_str(raw.trim()).map_err(|_| format!("Invalid UUID: {raw}"))
}

fn resolve_default_workspace_id(explicit_workspace_id: Option<String>) -> Option<String> {
    explicit_workspace_id
        .or_else(|| std::env::var("HERMES_DEFAULT_WORKSPACE_ID").ok())
        .or_else(|| std::env::var("ASSISTANT_DEFAULT_WORKSPACE_ID").ok())
        .filter(|value| !value.trim().is_empty())
}

fn compact_compute_fleet(fleet: &Value) -> Value {
    let nodes = fleet
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let compact_nodes: Vec<Value> = nodes
        .iter()
        .map(|node| {
            json!({
                "id": node.get("id").cloned().unwrap_or(Value::Null),
                "status": node.get("status").cloned().unwrap_or(Value::Null),
                "labels": node.get("labels").cloned().unwrap_or_else(|| json!([])),
                "capacity_total": node.get("capacity_total").cloned().unwrap_or(Value::Null),
                "capacity_available": node.get("capacity_available").cloned().unwrap_or(Value::Null),
                "active_jobs": node.get("active_jobs").cloned().unwrap_or(Value::Null),
                "queued_jobs": node.get("queued_jobs").cloned().unwrap_or(Value::Null),
                "cpu_total": node.get("cpu_total").cloned().unwrap_or(Value::Null),
                "mem_available_bytes": node.get("mem_available_bytes").cloned().unwrap_or(Value::Null),
                "disk_available_bytes": node.get("disk_available_bytes").cloned().unwrap_or(Value::Null),
                "cached_toolchains": node.get("cached_toolchains").cloned().unwrap_or_else(|| json!([])),
                "lean_runtime_ready": node.get("lean_runtime_ready").cloned().unwrap_or(Value::Null),
                "error": node.get("error").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    let online = nodes
        .iter()
        .filter(|node| node.get("status").and_then(Value::as_str) == Some("online"));
    let online_nodes = online.clone().count();
    let lean_nodes: Vec<&Value> = online
        .filter(|node| {
            node.get("lean_runtime_ready").and_then(Value::as_bool) != Some(false)
                && node
                    .get("labels")
                    .and_then(Value::as_array)
                    .is_some_and(|labels| labels.iter().any(|label| label.as_str() == Some("lean")))
        })
        .collect();
    let lean_slots_total = lean_nodes
        .iter()
        .filter_map(|node| node.get("capacity_total").and_then(Value::as_u64))
        .sum::<u64>();
    let lean_slots_available = lean_nodes
        .iter()
        .filter_map(|node| node.get("capacity_available").and_then(Value::as_u64))
        .sum::<u64>();
    let active_jobs = nodes
        .iter()
        .filter_map(|node| node.get("active_jobs").and_then(Value::as_u64))
        .sum::<u64>();
    let queued_jobs = nodes
        .iter()
        .filter_map(|node| node.get("queued_jobs").and_then(Value::as_u64))
        .sum::<u64>();

    json!({
        "enabled": fleet.get("enabled").cloned().unwrap_or(Value::Bool(false)),
        "summary": {
            "nodes_total": nodes.len(),
            "nodes_online": online_nodes,
            "lean_nodes_online": lean_nodes.len(),
            "lean_slots_total": lean_slots_total,
            "lean_slots_available": lean_slots_available,
            "active_remote_jobs": active_jobs,
            "queued_remote_jobs": queued_jobs,
        },
        "placement_policy": {
            "ordinary_cpu_work": "prefer online non-GPU nodes with immediate capacity, then lowest normalized utilization",
            "gpu_work": "request the gpu label explicitly",
            "parallel_clean_builds": "use distinct missions and verify distinct node/job/head receipts",
            "lean_builds": "workspaces listed under spark_offload.enabled_workspaces can also offload Lean builds to the DGX Spark lane (separate from these nodes)",
        },
        "nodes": compact_nodes,
        "recent_jobs": fleet.get("recent_jobs").cloned().unwrap_or_else(|| json!([])),
        "spark_offload": fleet.get("spark_offload").cloned().unwrap_or(Value::Null),
    })
}

fn compact_mission_summary(mission: Value) -> Value {
    json!({
        "id": mission.get("id").cloned().unwrap_or(Value::Null),
        "title": mission.get("title").cloned().unwrap_or(Value::Null),
        "status": mission.get("status").cloned().unwrap_or(Value::Null),
        "mission_mode": mission.get("mission_mode").cloned().unwrap_or(Value::Null),
        "backend": mission.get("backend").cloned().unwrap_or(Value::Null),
        "model_override": mission.get("model_override").cloned().unwrap_or(Value::Null),
        "model_effort": mission.get("model_effort").cloned().unwrap_or(Value::Null),
        "fast_mode": mission.get("fast_mode").cloned().unwrap_or(json!(false)),
        "workspace_id": mission.get("workspace_id").cloned().unwrap_or(Value::Null),
        "workspace_name": mission.get("workspace_name").cloned().unwrap_or(Value::Null),
        "short_description": mission.get("short_description").cloned().unwrap_or(Value::Null),
        "updated_at": mission.get("updated_at").cloned().unwrap_or(Value::Null),
        // Project tagging + awaiting classification + staleness anchors so
        // consumers can group/route/triage missions without parsing titles or
        // replaying events.
        "project": mission.get("project").cloned().unwrap_or(Value::Null),
        "track": mission.get("track").cloned().unwrap_or(Value::Null),
        "intent": mission.get("intent").cloned().unwrap_or(Value::Null),
        "github_pr": mission.get("github_pr").cloned().unwrap_or(Value::Null),
        "tags": mission.get("tags").cloned().unwrap_or_else(|| json!([])),
        "desired_state": mission.get("desired_state").cloned().unwrap_or(Value::Null),
        "next_check_at": mission.get("next_check_at").cloned().unwrap_or(Value::Null),
        // Creation provenance. Hermes is what SETS this, and until now could
        // not read it back — so it could not tell which of its own
        // conversations a mission belonged to.
        "origin": mission.get("origin").cloned().unwrap_or(Value::Null),
        "origin_session_id": mission.get("origin_session_id").cloned().unwrap_or(Value::Null),
        "awaiting_kind": mission.get("awaiting_kind").cloned().unwrap_or(Value::Null),
        "last_activity_at": mission.get("last_activity_at").cloned().unwrap_or(Value::Null),
        "last_status_change_at": mission.get("last_status_change_at").cloned().unwrap_or(Value::Null),
    })
}

fn truncate_snippet(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        out.push('…');
    }
    out
}

/// Classify a free-form error/content string into the failure modes a mission
/// babysitter cares about. Mirrors the server's `is_rate_limited_error` /
/// `is_auth_error` / `is_capacity_limited_error` families (see
/// src/api/mission_runner.rs) but works on text we can see from the event
/// stream.
fn error_signals_in(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut signals = Vec::new();
    if lower.contains("429")
        || lower.contains("529")
        || lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
        || lower.contains("overloaded")
        || lower.contains("overloaded_error")
        || lower.contains("resource_exhausted")
        || lower.contains("status code: 429")
        || lower.contains("status code: 529")
        || lower.contains("error: 429")
        || lower.contains("error: 529")
        || lower.contains("hit your limit")
        || lower.contains("hit your usage limit")
        || lower.contains("out of extra usage")
        || lower.contains("out of regular usage")
        || lower.contains("purchase more credits")
        || lower.contains("settings/usage")
    {
        signals.push("rate_limited");
    }
    if lower.contains(" 401")
        || lower.contains(" 403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("authentication")
        || lower.contains("credential")
        || lower.contains("refresh token was already used")
        || lower.contains("refresh_token was already used")
    {
        signals.push("auth_error");
    }
    if lower.contains("capacity")
        || lower.contains("503")
        || lower.contains("service unavailable")
        || lower.contains("no capacity")
        || lower.contains("already have five missions running")
        || lower.contains("already have 5 missions running")
        || lower.contains("concurrent mission limit")
        || lower.contains("selected model is at capacity")
        || lower.contains("model is at capacity")
    {
        signals.push("capacity_limited");
    }
    if lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("context_length_exceeded")
        || lower.contains("token limit")
        || lower.contains("prompt is too long")
    {
        signals.push("context_limit");
    }
    // Network / edge errors: be specific. Bare "timeout" or "idle timeout"
    // (e.g. OpenCode "idle timeout: the model stopped producing output") are
    // harness-level problems, not routing/edge issues. Only tag network_error
    // for clear transport indicators.
    let has_edge_code = lower.contains("502")
        || lower.contains("520")
        || lower.contains("521")
        || lower.contains("522");
    let is_transport_timeout = lower.contains("connection timed out")
        || lower.contains("request timed out")
        || lower.contains("read timeout")
        || lower.contains("write timeout")
        || (lower.contains("timed out")
            && (lower.contains("connection")
                || lower.contains("reset")
                || lower.contains("cloudflare")
                || lower.contains("peer")
                || lower.contains("dns")))
        || (lower.contains("timeout")
            && (lower.contains("cloudflare")
                || lower.contains("econn")
                || lower.contains("reset by peer")
                || lower.contains("dns")));
    if lower.contains("cloudflare")
        || lower.contains("connection reset")
        || lower.contains("econnreset")
        || lower.contains("reset by peer")
        || is_transport_timeout
        || has_edge_code
        || lower.contains("dns")
    {
        signals.push("network_error");
    }
    signals
}

#[derive(Default)]
struct TraceAnalysis {
    signals: std::collections::BTreeSet<&'static str>,
    recent_errors: Vec<Value>,
    loop_tool: Option<String>,
    loop_repeats: usize,
    loop_snippet: Option<String>,
    tool_call_count: usize,
}

impl TraceAnalysis {
    fn signals_json(&self) -> Value {
        json!({
            "rate_limited": self.signals.contains("rate_limited"),
            "auth_error": self.signals.contains("auth_error"),
            "capacity_limited": self.signals.contains("capacity_limited"),
            "context_limit": self.signals.contains("context_limit"),
            "network_error": self.signals.contains("network_error"),
            "suspected_loop": self.loop_tool.is_some(),
        })
    }

    fn loop_json(&self) -> Value {
        match &self.loop_tool {
            Some(tool) => json!({
                "tool": tool,
                "repeats": self.loop_repeats,
                "args": self.loop_snippet.clone().unwrap_or_default(),
            }),
            None => Value::Null,
        }
    }
}

/// Scan trace events (ascending) for error signals and looping tool calls.
fn analyze_trace_events(events: &[Value]) -> TraceAnalysis {
    let mut analysis = TraceAnalysis::default();
    let mut repeat_counts: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();

    for event in events {
        match event.get("event_type").and_then(Value::as_str) {
            Some("error") => {
                let content = event.get("content").and_then(Value::as_str).unwrap_or("");
                for signal in error_signals_in(content) {
                    analysis.signals.insert(signal);
                }
                // Keep only the last few error snippets to bound output.
                if analysis.recent_errors.len() >= 5 {
                    analysis.recent_errors.remove(0);
                }
                analysis.recent_errors.push(json!({
                    "sequence": event.get("sequence").cloned().unwrap_or(Value::Null),
                    "timestamp": event.get("timestamp").cloned().unwrap_or(Value::Null),
                    "snippet": truncate_snippet(content, 400),
                }));
            }
            Some("tool_call") => {
                analysis.tool_call_count += 1;
                let tool = event
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)")
                    .to_string();
                let args = event
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let count = repeat_counts
                    .entry((tool.clone(), args.clone()))
                    .or_insert(0);
                *count += 1;
                // 3+ identical calls (same tool + same args) within the window
                // is a strong loop signal.
                if *count >= 3 && *count > analysis.loop_repeats {
                    analysis.loop_repeats = *count;
                    analysis.loop_tool = Some(tool);
                    analysis.loop_snippet = Some(truncate_snippet(&args, 160));
                }
            }
            _ => {}
        }
    }
    analysis
}

/// Synthesize a single actionable next-step hint for the babysitter.
fn build_recommendation(
    status: &str,
    backend: Option<&str>,
    live: &Value,
    analysis: &TraceAnalysis,
) -> String {
    let live_state = live.get("state").and_then(Value::as_str);
    if backend == Some("chatgpt_ui") && live_state == Some("running") {
        return "ChatGPT UI Pro is still generating. The web UI may expose only a generic \
                `Pro thinking` marker until the final answer begins, so event silence is not \
                evidence of a stall. The driver has its own absolute timeout and the durable \
                run heartbeat proves ownership. Do not cancel, resume, or submit another \
                ChatGPT UI mission while this run is non-terminal; wait for its result or \
                explicit timeout."
            .to_string();
    }
    if analysis.signals.contains("rate_limited") || analysis.signals.contains("capacity_limited") {
        return "Provider is rate-limiting or at capacity. Switch to a different backend/provider \
                with update_mission_settings, or wait and resume_mission."
            .to_string();
    }
    if analysis.signals.contains("auth_error") {
        return "Auth/credential failure for this backend. Verify backend auth; switching backend \
                via update_mission_settings may unblock it."
            .to_string();
    }
    if analysis.signals.contains("context_limit") {
        return "Hit the model context limit. Switch to a larger-context backend/model with \
                update_mission_settings, then resume_mission."
            .to_string();
    }
    if analysis.signals.contains("network_error") {
        return "Network/edge errors (e.g. Cloudflare drops, resets, timeouts). Usually transient \
                routing — resume_mission, and if it recurs switch backend with update_mission_settings."
            .to_string();
    }
    if let Some(tool) = &analysis.loop_tool {
        return format!(
            "Agent looks stuck looping on `{tool}` ({}× identical calls). Send a concrete hint \
             with send_message_to_mission, or switch backend/model with update_mission_settings.",
            analysis.loop_repeats
        );
    }

    let health_status = live
        .get("health")
        .and_then(|health| health.get("status"))
        .and_then(Value::as_str);
    let severity = live
        .get("health")
        .and_then(|health| health.get("severity"))
        .and_then(Value::as_str);
    if health_status == Some("stalled") {
        let seconds = live
            .get("seconds_since_activity")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if severity == Some("severe") {
            return format!(
                "Mission appears severely stalled (no activity for {seconds}s and no live tool). \
                 Consider cancel_mission then resume_mission, or send_message_to_mission with a \
                 concrete next step."
            );
        }
        return format!(
            "Mission is quiet ({seconds}s since last activity) but a tool may still be running. \
             Watch it; only intervene if it stays stalled."
        );
    }

    // Only interrupted/blocked/failed are resumable server-side (see
    // `mission_can_be_resumed` in src/api/control.rs). The other idle
    // statuses need a different intervention.
    if matches!(status, "interrupted" | "blocked" | "failed") {
        return format!(
            "Mission is idle in status '{status}'. If the goal isn't done, resume_mission with a \
             hint to keep going (e.g. 'you still have budget — continue until done, don't stop to ask')."
        );
    }
    if status == "not_feasible" {
        return "Mission concluded the goal is not feasible as specified. This status is not \
                resumable — review the last assistant message, adjust the prompt/goal, and start \
                a new mission or send_message_to_mission once the task is reframed."
            .to_string();
    }
    if matches!(status, "awaiting_user" | "acknowledged") {
        return "Mission finished its turn and is waiting. If the goal isn't fully done, nudge it \
                with send_message_to_mission to continue rather than letting it idle."
            .to_string();
    }

    "No problems detected; mission appears healthy.".to_string()
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("authkey")
        || key.contains("private_key")
        || key.contains("credential")
}

fn is_sensitive_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("ghp_")
        || trimmed.starts_with("github_pat_")
        || trimmed.starts_with("sk-")
        || trimmed.starts_with("tskey-")
        || trimmed.contains("BEGIN OPENSSH PRIVATE KEY")
        || trimmed.contains("BEGIN PGP PRIVATE KEY")
        || trimmed.contains("<encrypted")
}

fn scrub_sensitive_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key.eq_ignore_ascii_case("env_vars") {
                    if let Value::Object(env_vars) = child {
                        for env_value in env_vars.values_mut() {
                            *env_value = Value::String("[redacted]".to_string());
                        }
                    } else {
                        *child = Value::String("[redacted]".to_string());
                    }
                } else if is_sensitive_key(key) {
                    *child = Value::String("[redacted]".to_string());
                } else {
                    scrub_sensitive_json(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub_sensitive_json(item);
            }
        }
        Value::String(raw) if is_sensitive_value(raw) => {
            *value = Value::String("[redacted]".to_string());
        }
        _ => {}
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("assistant-mcp {SERVER_VERSION}");
        return;
    }

    let server = AssistantMcp::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in BufReader::new(stdin.lock()).lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {error}"));
                if let Ok(serialized) = serde_json::to_string(&response) {
                    let _ = writeln!(stdout, "{serialized}");
                    let _ = stdout.flush();
                }
                continue;
            }
        };

        // Notifications (no id), e.g. the `notifications/initialized` the MCP
        // client sends after `initialize`, expect no reply per JSON-RPC.
        // Returning a "-32601 Method not found" error here breaks the handshake
        // with stricter clients.
        if request.id.is_null() && request.method.starts_with("notifications/") {
            continue;
        }

        let response = server.handle_request(request).await;
        if let Ok(serialized) = serde_json::to_string(&response) {
            let _ = writeln!(stdout, "{serialized}");
            let _ = stdout.flush();
        }
    }
}

/// Return true when a mission is an ACK-only wait that may transition to
/// `acknowledged`, false when it is already acknowledged, and reject every
/// live or decision-bearing state.
fn mission_requires_acknowledgement(digest: &Value) -> Result<bool, String> {
    let mission = digest.get("mission").unwrap_or(digest);
    let status = mission
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "Mission digest did not include a status".to_string())?;
    if status == "acknowledged" {
        return Ok(false);
    }

    let awaiting_kind = mission.get("awaiting_kind").and_then(Value::as_str);
    if status == "awaiting_user" && awaiting_kind == Some("ack") {
        return Ok(true);
    }

    Err(format!(
        "Mission cannot be ACKed from status={status}, awaiting_kind={}",
        awaiting_kind.unwrap_or("none")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `adopt_mission` without a stamped session must refuse, not clear.
    #[test]
    fn adopt_without_a_session_is_refused() {
        let params: AdoptMissionParams =
            parse_params(json!({"mission_id": "57c1dfb4"})).expect("parse");
        assert!(params.origin_session_id.is_none());
        // The handler itself needs a live API to run; pin the refusal message
        // contract here instead: an empty stamp must not silently clear.
        let session = params
            .origin_session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        assert!(session.is_none(), "an absent stamp must read as absent");
    }

    #[test]
    fn adopt_params_accept_the_stamped_shape() {
        let params: AdoptMissionParams = parse_params(json!({
            "mission_id": "57c1dfb4-a782-4010-9f8b-aaa7f88afa7e",
            "origin_session_id": "20260805_190902_361101",
        }))
        .expect("parse");
        assert_eq!(
            params.origin_session_id.as_deref(),
            Some("20260805_190902_361101")
        );
        assert!(valid_origin_session_id(
            params.origin_session_id.as_deref().unwrap()
        ));
    }

    /// The 2026-08-05 incident: seven identical retries in ninety seconds,
    /// because the error named no field.
    #[test]
    fn a_params_error_names_the_offending_field() {
        let arguments = json!({
            "title": "t",
            "prompt": "p",
            "desired_state": {"status": "running"},
        });
        let error = parse_params::<StartMissionParams>(arguments)
            .expect_err("a map where a string belongs must not deserialize");
        assert!(
            error.contains("desired_state"),
            "the field must be named, got: {error}"
        );
        assert!(error.contains("invalid type: map"), "got: {error}");
    }

    #[test]
    fn a_nested_field_reports_its_path() {
        let arguments = json!({"title": "t", "prompt": "p", "tags": ["ok", {"a": 1}]});
        let error = parse_params::<StartMissionParams>(arguments).expect_err("a map is not a tag");
        assert!(error.contains("tags[1]"), "got: {error}");
    }

    #[test]
    fn a_missing_required_field_is_named_too() {
        let error = parse_params::<StartMissionParams>(json!({"title": "t"}))
            .expect_err("prompt is required");
        assert!(error.contains("prompt"), "got: {error}");
    }

    #[test]
    fn valid_params_still_deserialize() {
        let params: StartMissionParams =
            parse_params(json!({"title": "t", "prompt": "p", "tags": ["a"]}))
                .expect("valid arguments must parse");
        assert_eq!(params.title, "t");
        assert_eq!(params.tags.as_deref(), Some(&["a".to_string()][..]));
    }

    /// Arguments that are not an object at all have no field to name, and a
    /// bare "." would be noise rather than information.
    #[test]
    fn a_root_level_failure_reports_no_path() {
        let error = parse_params::<StartMissionParams>(json!("not an object"))
            .expect_err("a string is not a params object");
        assert!(
            error.starts_with("Invalid params: invalid type"),
            "got: {error}"
        );
    }

    /// A tool description is the only thing an autonomous agent knows about
    /// what a tool can do. When it understates the tool, the agent reasons
    /// correctly from wrong premises and gives up.
    ///
    /// That is not hypothetical. `resume_mission` said "interrupted, blocked,
    /// or failed" and `send_message_to_mission` said only "Send a follow-up
    /// message", so a controller holding an `acknowledged` mission concluded
    /// it could not be woken at all and reported the benchmark campaign
    /// blocked — while the server would have activated it on the same id.
    #[test]
    fn the_wake_tool_advertises_every_status_it_actually_wakes() {
        let tools = AssistantMcp::tools();
        let send = tools
            .iter()
            .find(|t| t.name == "send_message_to_mission")
            .expect("send_message_to_mission is registered");

        // These are exactly the statuses `message_activates_mission` accepts.
        // If that list grows, this description has to grow with it.
        for status in [
            "pending",
            "awaiting_user",
            "acknowledged",
            "waiting_background",
            "interrupted",
            "blocked",
            "completed",
            "failed",
        ] {
            assert!(
                send.description.contains(status),
                "send_message_to_mission wakes `{status}` but does not say so; \
                 an agent reading this will believe it cannot: {}",
                send.description
            );
        }

        // And the recovery tool must not read as the only way to wake a
        // mission, which is the inference that cost the benchmark track.
        let resume = tools
            .iter()
            .find(|t| t.name == "resume_mission")
            .expect("resume_mission is registered");
        assert!(
            resume.description.contains("send_message_to_mission"),
            "resume_mission should point at the normal wake path: {}",
            resume.description
        );
    }

    /// The binary's tool table IS the curated Hermes surface; the generated
    /// config allowlists are pinned to `HERMES_ASSISTANT_TOOL_ALLOWLIST`.
    /// Adding/removing a tool here must go through the canonical list.
    /// A full UUID must resolve without a round trip: no HTTP client is
    /// configured in tests, so any network attempt would fail here.
    #[tokio::test]
    async fn full_uuid_resolves_without_calling_the_server() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let mcp = AssistantMcp::new();
        let id = Uuid::new_v4();
        assert_eq!(
            mcp.resolve_mission_id(&id.to_string())
                .await
                .expect("resolve"),
            id
        );
        // Mixed case with surrounding whitespace is the shape humans paste.
        assert_eq!(
            mcp.resolve_mission_id(&format!("  {}  ", id.to_string().to_uppercase()))
                .await
                .expect("resolve"),
            id
        );
    }

    #[test]
    fn compact_summary_exposes_creation_origin() {
        let summary = compact_mission_summary(json!({
            "id": "11111111-2222-3333-4444-555555555555",
            "status": "active",
            "origin": "hermes",
            "origin_session_id": "20260804_103847_86ca5c",
            "prompt": "secret",
            "api_token": "secret",
        }));
        assert_eq!(summary["origin"], "hermes");
        assert_eq!(summary["origin_session_id"], "20260804_103847_86ca5c");
        // The summary stays a projection, not a passthrough.
        assert!(summary.get("prompt").is_none());
        assert!(summary.get("api_token").is_none());
    }

    fn seeded_event(
        sequence: i64,
        event_type: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        content: &str,
    ) -> Value {
        json!({
            "sequence": sequence,
            "event_type": event_type,
            "tool_name": tool_name,
            "tool_call_id": tool_call_id,
            "content": content,
        })
    }

    #[test]
    fn pending_question_lookup_finds_unanswered_call() {
        let events = vec![
            seeded_event(1, "user_message", None, None, "go"),
            // An older, already-answered question must NOT be pending.
            seeded_event(
                2,
                "tool_call",
                Some("AskUserQuestion"),
                Some("q-old"),
                r#"{"questions":[{"question":"Old?"}]}"#,
            ),
            seeded_event(
                3,
                "tool_result",
                Some("AskUserQuestion"),
                Some("q-old"),
                r#"{"answers":[["A"]]}"#,
            ),
            // An ordinary in-flight tool call is not a question.
            seeded_event(4, "tool_call", Some("Bash"), Some("b-1"), r#"{"cmd":"x"}"#),
            seeded_event(
                5,
                "tool_call",
                Some("AskUserQuestion"),
                Some("q-new"),
                r#"{"questions":[{"question":"Deploy to prod?"},{"question":"Which region?"}]}"#,
            ),
        ];
        let pending = pending_ask_user_questions(&events);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_call_id, "q-new");
        assert_eq!(pending[0].sequence, 5);
        assert_eq!(pending[0].summary(), "Deploy to prod? | Which region?");
    }

    #[test]
    fn pending_question_lookup_orders_newest_first_and_survives_bad_args() {
        let events = vec![
            // Unparseable args must still surface the pending call.
            seeded_event(
                1,
                "tool_call",
                Some("AskUserQuestion"),
                Some("q-1"),
                "not json",
            ),
            seeded_event(
                7,
                "tool_call",
                Some("AskUserQuestion"),
                Some("q-2"),
                r#"{"questions":[{"question":"Second?"}]}"#,
            ),
        ];
        let pending = pending_ask_user_questions(&events);
        assert_eq!(
            pending
                .iter()
                .map(|q| q.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["q-2", "q-1"]
        );
        assert_eq!(pending[1].summary(), "(question text unavailable)");
    }

    #[test]
    fn pending_question_lookup_empty_when_all_answered() {
        let events = vec![
            seeded_event(
                1,
                "tool_call",
                Some("AskUserQuestion"),
                Some("q-1"),
                r#"{"questions":[{"question":"Only?"}]}"#,
            ),
            seeded_event(
                2,
                "tool_result",
                None,
                Some("q-1"),
                r#"{"answers":[["A"]]}"#,
            ),
        ];
        assert!(pending_ask_user_questions(&events).is_empty());
    }

    #[test]
    fn answer_mission_question_params_parse_with_and_without_tool_call_id() {
        let params: AnswerMissionQuestionParams = parse_params(json!({
            "mission_id": "abc",
            "answers": [["Option A"], ["free text"]],
        }))
        .expect("parse without tool_call_id");
        assert!(params.tool_call_id.is_none());
        assert_eq!(params.answers, vec![vec!["Option A"], vec!["free text"]]);

        let params: AnswerMissionQuestionParams = parse_params(json!({
            "mission_id": "abc",
            "tool_call_id": "q-1",
            "answers": [["Yes"]],
        }))
        .expect("parse with tool_call_id");
        assert_eq!(params.tool_call_id.as_deref(), Some("q-1"));
    }

    #[test]
    fn tool_table_matches_canonical_allowlist() {
        let names: Vec<String> = AssistantMcp::tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            names,
            sandboxed_sh::hermes_tools::HERMES_ASSISTANT_TOOL_ALLOWLIST,
            "assistant-mcp tool table diverged from \
             src/hermes_tools.rs::HERMES_ASSISTANT_TOOL_ALLOWLIST — update both together"
        );
    }

    const ENV_KEYS: &[&str] = &[
        "HERMES_SANDBOXED_API_URL",
        "SANDBOXED_API_URL",
        "OPEN_AGENT_API_URL",
        "HERMES_SANDBOXED_API_TOKEN",
        "SANDBOXED_API_TOKEN",
        "OPEN_AGENT_API_TOKEN",
        "JWT_SECRET",
        "HERMES_DEFAULT_WORKSPACE_ID",
        "ASSISTANT_DEFAULT_WORKSPACE_ID",
    ];

    fn clear_env() {
        for key in ENV_KEYS {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn compact_mission_summary_keeps_only_hermes_safe_fields() {
        let summary = compact_mission_summary(json!({
            "id": "mission-1",
            "title": "Fix the build",
            "status": "active",
            "mission_mode": "default",
            "backend": "codex",
            "model_override": "gpt-5.6-sol",
            "workspace_id": "workspace-1",
            "workspace_name": "assistant",
            "short_description": "Build fix",
            "updated_at": "2026-05-28T12:00:00Z",
            "project": "verity-core",
            "track": "C3-bridge-collapse",
            "github_pr": "lfglabs-dev/verity#2070",
            "tags": ["c3", "sprint-2"],
            "desired_state": "waiting_ci",
            "awaiting_kind": "decision",
            "prompt": "secret prompt",
            "api_token": "sk-test",
        }));

        assert_eq!(summary["id"], "mission-1");
        assert_eq!(summary["workspace_name"], "assistant");
        assert_eq!(summary["project"], "verity-core");
        assert_eq!(summary["track"], "C3-bridge-collapse");
        assert_eq!(summary["github_pr"], "lfglabs-dev/verity#2070");
        assert_eq!(summary["desired_state"], "waiting_ci");
        assert_eq!(summary["tags"][1], "sprint-2");
        assert_eq!(summary["awaiting_kind"], "decision");
        // Missions without tags get an empty array, not null.
        let bare = compact_mission_summary(json!({"id": "m2"}));
        assert_eq!(bare["tags"], json!([]));
        assert!(summary.get("prompt").is_none());
        assert!(summary.get("api_token").is_none());
    }

    #[test]
    fn scrub_sensitive_json_redacts_nested_keys_and_token_values() {
        let mut value = json!({
            "mission": {
                "title": "Hermes",
                "api_key": "sk-test",
                "notes": ["visible", "github_pat_123"],
                "env_vars": {
                    "DATABASE_URL": "postgres://user:password@example.test/db",
                    "AWS_ACCESS_KEY_ID": "not-matched-by-value-heuristics",
                    "PATH": "/usr/local/bin:/usr/bin"
                }
            },
            "token": "plain-token"
        });

        scrub_sensitive_json(&mut value);

        assert_eq!(value["mission"]["title"], "Hermes");
        assert_eq!(value["mission"]["api_key"], "[redacted]");
        assert_eq!(value["mission"]["notes"][0], "visible");
        assert_eq!(value["mission"]["notes"][1], "[redacted]");
        assert_eq!(value["mission"]["env_vars"]["DATABASE_URL"], "[redacted]");
        assert_eq!(
            value["mission"]["env_vars"]["AWS_ACCESS_KEY_ID"],
            "[redacted]"
        );
        assert_eq!(value["mission"]["env_vars"]["PATH"], "[redacted]");
        assert_eq!(value["token"], "[redacted]");
    }

    #[test]
    fn hermes_connection_env_takes_precedence_over_legacy_names() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OPEN_AGENT_API_URL", "https://open-agent.example");
        std::env::set_var("SANDBOXED_API_URL", "https://sandboxed.example");
        std::env::set_var("HERMES_SANDBOXED_API_URL", "https://hermes.example/");
        std::env::set_var("OPEN_AGENT_API_TOKEN", "open-agent-token");
        std::env::set_var("SANDBOXED_API_TOKEN", "sandboxed-token");
        std::env::set_var("HERMES_SANDBOXED_API_TOKEN", "hermes-token");

        let server = AssistantMcp::new();

        assert_eq!(server.api_url, "https://hermes.example");
        assert_eq!(server.api_token.as_deref(), Some("hermes-token"));
        clear_env();
    }

    #[test]
    fn legacy_connection_envs_remain_supported_for_compatibility() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OPEN_AGENT_API_URL", "https://open-agent.example");
        std::env::set_var("SANDBOXED_API_URL", "https://sandboxed.example/");
        std::env::set_var("OPEN_AGENT_API_TOKEN", "open-agent-token");
        std::env::set_var("SANDBOXED_API_TOKEN", "sandboxed-token");

        let server = AssistantMcp::new();

        assert_eq!(server.api_url, "https://sandboxed.example");
        assert_eq!(server.api_token.as_deref(), Some("sandboxed-token"));
        clear_env();
    }

    #[test]
    fn explicit_workspace_id_takes_precedence_over_default_envs() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("HERMES_DEFAULT_WORKSPACE_ID", "hermes-workspace");
        std::env::set_var("ASSISTANT_DEFAULT_WORKSPACE_ID", "assistant-workspace");

        let workspace_id = resolve_default_workspace_id(Some("tool-workspace".to_string()));

        assert_eq!(workspace_id.as_deref(), Some("tool-workspace"));
        clear_env();
    }

    #[test]
    fn hermes_default_workspace_env_takes_precedence_over_legacy_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("HERMES_DEFAULT_WORKSPACE_ID", "hermes-workspace");
        std::env::set_var("ASSISTANT_DEFAULT_WORKSPACE_ID", "assistant-workspace");

        let workspace_id = resolve_default_workspace_id(None);

        assert_eq!(workspace_id.as_deref(), Some("hermes-workspace"));
        clear_env();
    }

    #[test]
    fn legacy_default_workspace_env_remains_supported_for_compatibility() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("ASSISTANT_DEFAULT_WORKSPACE_ID", "assistant-workspace");

        let workspace_id = resolve_default_workspace_id(None);

        assert_eq!(workspace_id.as_deref(), Some("assistant-workspace"));
        clear_env();
    }

    #[test]
    fn native_agent_names_select_the_matching_backend() {
        assert_eq!(
            native_backend_from_agent(Some("codex")).as_deref(),
            Some("codex")
        );
        assert_eq!(
            native_backend_from_agent(Some(" ClaudeCode ")).as_deref(),
            Some("claudecode")
        );
        assert_eq!(native_backend_from_agent(Some("build")), None);
        assert_eq!(native_backend_from_agent(None), None);
    }

    #[test]
    fn error_signals_classify_known_failure_modes() {
        assert_eq!(
            error_signals_in("HTTP 429 Too Many Requests"),
            vec!["rate_limited"]
        );
        assert_eq!(
            error_signals_in("Error: 401 Unauthorized invalid api key"),
            vec!["auth_error"]
        );
        assert!(
            error_signals_in("context_length_exceeded: prompt is too long")
                .contains(&"context_limit")
        );
        assert!(error_signals_in("cloudflare 520: connection reset").contains(&"network_error"));
        assert!(error_signals_in("all good here").is_empty());

        // 529 and "hit your limit" family (server-side rate limit markers)
        assert!(error_signals_in("error: 529 overloaded").contains(&"rate_limited"));
        assert!(error_signals_in("status code: 529").contains(&"rate_limited"));
        assert!(
            error_signals_in("You've hit your limit · resets Jun 10, 5pm (Europe/Berlin)")
                .contains(&"rate_limited")
        );
        assert!(error_signals_in(
            "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage"
        )
        .contains(&"rate_limited"));

        // Harness-level idle timeouts are NOT network errors (they are local model/harness stalls)
        let idle = error_signals_in("OpenCode idle timeout: the model stopped producing output before finishing the turn. Partial output was discarded because it was incomplete.");
        assert!(
            !idle.contains(&"network_error"),
            "idle timeout must not be misclassified as network_error: {idle:?}"
        );
        // But real transport timeouts still are
        assert!(
            error_signals_in("connection timed out while talking to provider")
                .contains(&"network_error")
        );
    }

    #[test]
    fn analyze_trace_detects_repeated_tool_loop() {
        let events: Vec<Value> = (0..4)
            .map(|i| {
                json!({
                    "sequence": i,
                    "event_type": "tool_call",
                    "tool_name": "read_file",
                    "content": "{\"path\":\"main.rs\"}"
                })
            })
            .collect();
        let analysis = analyze_trace_events(&events);
        assert_eq!(analysis.loop_tool.as_deref(), Some("read_file"));
        assert_eq!(analysis.loop_repeats, 4);
        assert_eq!(analysis.tool_call_count, 4);
    }

    #[test]
    fn analyze_trace_collects_error_signals() {
        let events = vec![
            json!({"sequence": 1, "event_type": "error", "content": "provider 429 rate limit"}),
            json!({"sequence": 2, "event_type": "tool_call", "tool_name": "run_command", "content": "ls"}),
        ];
        let analysis = analyze_trace_events(&events);
        assert!(analysis.signals.contains("rate_limited"));
        assert_eq!(analysis.recent_errors.len(), 1);
        assert!(analysis.loop_tool.is_none());
    }

    #[test]
    fn recommendation_prioritizes_rate_limit_over_loop() {
        let mut analysis = TraceAnalysis::default();
        analysis.signals.insert("rate_limited");
        analysis.loop_tool = Some("read_file".to_string());
        analysis.loop_repeats = 5;
        let rec = build_recommendation("active", None, &Value::Null, &analysis);
        assert!(rec.contains("rate-limiting") || rec.contains("capacity"));
    }

    #[test]
    fn recommendation_flags_severe_stall() {
        let analysis = TraceAnalysis::default();
        let live = json!({
            "seconds_since_activity": 600,
            "health": {"status": "stalled", "severity": "severe"}
        });
        let rec = build_recommendation("active", None, &live, &analysis);
        assert!(rec.contains("stalled"));
    }

    #[test]
    fn recommendation_does_not_cancel_running_chatgpt_ui_for_event_silence() {
        let mut analysis = TraceAnalysis::default();
        // A previous generation's terminal transport error can remain in the
        // bounded trace window. It must not preempt a healthy replacement run.
        analysis.signals.insert("network_error");
        let live = json!({
            "state": "running",
            "seconds_since_activity": 686,
            "heartbeat_at": "2026-07-25T10:45:00Z",
            "health": {"status": "stalled", "severity": "severe"}
        });
        let rec = build_recommendation("active", Some("chatgpt_ui"), &live, &analysis);
        assert!(rec.contains("Pro thinking"));
        assert!(rec.contains("Do not cancel"));
        assert!(rec.contains("explicit timeout"));
    }

    #[test]
    fn recommendation_does_not_recommend_resume_for_not_feasible() {
        // not_feasible is not resumable server-side (see mission_can_be_resumed
        // in src/api/control.rs). The recommendation must steer the babysitter
        // away from calling resume_mission, otherwise it gets a hard failure.
        let analysis = TraceAnalysis::default();
        let rec = build_recommendation("not_feasible", None, &Value::Null, &analysis);
        assert!(
            !rec.contains("resume_mission with a hint"),
            "recommendation must not suggest resume_mission for not_feasible, got: {rec}"
        );
        assert!(
            rec.contains("not feasible") || rec.contains("not_feasible"),
            "recommendation should explain the status, got: {rec}"
        );
    }

    #[test]
    fn recommendation_still_recommends_resume_for_resumable_statuses() {
        for status in ["interrupted", "blocked", "failed"] {
            let analysis = TraceAnalysis::default();
            let rec = build_recommendation(status, None, &Value::Null, &analysis);
            assert!(
                rec.contains("resume_mission"),
                "expected resume_mission recommendation for status {status}, got: {rec}"
            );
        }
    }

    #[test]
    fn output_dir_accepts_paths_under_tmp() {
        let mission_id = Uuid::nil();
        let dir = output_dir_for_shared_file(&mission_id, Some("/tmp/artifacts".to_string()))
            .expect("plain /tmp path is allowed");
        assert!(dir.starts_with("/tmp/artifacts"));
    }

    #[test]
    fn output_dir_rejects_parent_traversal_and_non_tmp_paths() {
        let mission_id = Uuid::nil();
        // `/tmp/../etc` passes a lexical starts_with("/tmp") but resolves
        // outside the real /tmp tree — must be rejected.
        assert!(output_dir_for_shared_file(&mission_id, Some("/tmp/../etc".to_string())).is_err());
        assert!(
            output_dir_for_shared_file(&mission_id, Some("/tmp/a/../../etc".to_string())).is_err()
        );
        // Sibling prefixes and plainly foreign roots are rejected too.
        assert!(output_dir_for_shared_file(&mission_id, Some("/tmpdir/x".to_string())).is_err());
        assert!(output_dir_for_shared_file(&mission_id, Some("/var/tmp".to_string())).is_err());
        // Relative paths are rejected.
        assert!(output_dir_for_shared_file(&mission_id, Some("tmp/x".to_string())).is_err());
    }

    #[test]
    fn workspace_management_tools_are_exposed_with_destructive_confirmations() {
        let tools = AssistantMcp::tools();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(names.contains(&"get_compute_fleet"));
        for expected in [
            "get_workspace",
            "create_workspace",
            "update_workspace",
            "delete_workspace",
            "list_workspace_templates",
            "get_workspace_template",
            "save_workspace_template",
            "delete_workspace_template",
            "rebuild_workspace_from_template",
            "start_workspace_job",
            "get_workspace_job",
            "cancel_workspace_job",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }

        for destructive in [
            "delete_workspace",
            "delete_workspace_template",
            "rebuild_workspace_from_template",
        ] {
            let schema = &tools
                .iter()
                .find(|tool| tool.name == destructive)
                .unwrap()
                .input_schema;
            assert!(schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("confirm")));
        }
    }

    #[test]
    fn project_roster_tools_are_exposed() {
        let tools = AssistantMcp::tools();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        for expected in [
            "list_projects",
            "get_project",
            "update_project_status",
            "set_project_track",
            "get_project_grant",
            "set_project_grant",
            "record_project_decision",
            "answer_project_decision",
            "get_project_tasks",
            "plan_project_tasks",
            "update_project_task",
            "cancel_project_task",
            "link_mission_to_project",
        ] {
            assert!(names.contains(&expected), "missing project tool {expected}");
        }
        // The ledger tool exposes the authority split, and the grant tool the
        // normalized autonomy level — controllers discover both from schema.
        let decision = tools
            .iter()
            .find(|tool| tool.name == "record_project_decision")
            .unwrap();
        let authorities = decision.input_schema["properties"]["authority"]["enum"]
            .as_array()
            .unwrap();
        assert!(authorities.contains(&json!("granted")));
        let grant = tools
            .iter()
            .find(|tool| tool.name == "set_project_grant")
            .unwrap();
        let levels = grant.input_schema["properties"]["autonomy_level"]["enum"]
            .as_array()
            .unwrap();
        assert!(levels.contains(&json!("act_reversible")));
        // The status tool constrains mode to the three known regimes.
        let status = tools
            .iter()
            .find(|tool| tool.name == "update_project_status")
            .unwrap();
        let modes = status.input_schema["properties"]["mode"]["enum"]
            .as_array()
            .unwrap();
        assert!(modes.contains(&json!("blocked")));
    }

    #[test]
    fn mission_tools_expose_codex_fast_mode() {
        let tools = AssistantMcp::tools();
        assert!(tools
            .iter()
            .any(|tool| tool.name == "get_chatgpt_ui_pool_status"));
        for name in ["start_mission", "update_mission_settings"] {
            let schema = &tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("missing tool {name}"))
                .input_schema;
            assert_eq!(schema["properties"]["fast_mode"]["type"], "boolean");
        }
    }

    #[test]
    fn workspace_bash_rejects_heavy_commands_and_job_argv_is_quoted() {
        assert!(is_heavy_workspace_command("lake build Verity"));
        assert!(is_heavy_workspace_command("cd repo && cargo test --all"));
        assert!(!is_heavy_workspace_command("git status --short"));
        assert_eq!(
            workspace_job_command(None, Some(vec!["printf".into(), "%s".into(), "a'b".into()]))
                .unwrap(),
            "'printf' '%s' 'a'\\''b'"
        );
        assert!(workspace_job_command(Some("true".into()), Some(vec!["true".into()])).is_err());
    }

    #[test]
    fn compute_fleet_summary_is_compact_and_capacity_aware() {
        let raw = json!({
            "enabled": true,
            "nodes": [
                {
                    "id": "cpu",
                    "status": "online",
                    "labels": ["lean"],
                    "capacity_total": 2,
                    "capacity_available": 1,
                    "active_jobs": 1,
                    "queued_jobs": 0,
                    "cpu_total": 8,
                    "mem_available_bytes": 32_u64 << 30,
                    "disk_available_bytes": 100_u64 << 30,
                    "cached_toolchains": ["leanprover--lean4---v4.24.0"],
                    "lean_runtime_ready": true,
                    "base_url": "must-not-leak"
                },
                {
                    "id": "general",
                    "status": "online",
                    "labels": ["general"],
                    "capacity_total": 1,
                    "capacity_available": 1,
                    "active_jobs": 0,
                    "queued_jobs": 0,
                    "lean_runtime_ready": false
                },
                {
                    "id": "gpu-only",
                    "status": "online",
                    "labels": ["gpu"],
                    "capacity_total": 4,
                    "capacity_available": 4,
                    "active_jobs": 0,
                    "queued_jobs": 0,
                    "lean_runtime_ready": true,
                    "error": "probe degraded"
                },
                {
                    "id": "legacy-lean",
                    "status": "online",
                    "labels": ["lean"],
                    "capacity_total": 1,
                    "capacity_available": 1,
                    "active_jobs": 0,
                    "queued_jobs": 0
                }
            ],
            "recent_jobs": [{"job_id": "job", "node_id": "cpu", "state": "succeeded"}]
        });

        let compact = compact_compute_fleet(&raw);
        assert_eq!(compact["summary"]["nodes_online"], 4);
        assert_eq!(compact["summary"]["lean_nodes_online"], 2);
        assert_eq!(compact["summary"]["lean_slots_total"], 3);
        assert_eq!(compact["summary"]["lean_slots_available"], 2);
        assert_eq!(compact["summary"]["active_remote_jobs"], 1);
        assert!(compact["nodes"][0].get("base_url").is_none());
        assert_eq!(compact["nodes"][2]["error"], "probe degraded");
        assert_eq!(compact["recent_jobs"][0]["node_id"], "cpu");
    }

    #[test]
    fn mission_events_default_to_the_newest_bounded_page() {
        let id = Uuid::nil();
        let newest = mission_events_path(id, 40, "all", None, None);
        assert!(newest.ends_with(&format!("&before_seq={}", i64::MAX)));

        let delta = mission_events_path(id, 40, "all", None, Some(17));
        assert!(delta.ends_with("&since_seq=17"));
        assert!(!delta.contains("before_seq"));

        let backwards = mission_events_path(id, 40, "all", Some(99), Some(17));
        assert!(backwards.ends_with("&before_seq=99"));
        assert!(!backwards.contains("since_seq"));
    }

    #[test]
    fn project_scope_gates_mutations_only() {
        let scoped = AssistantMcp {
            api_url: "http://127.0.0.1:3000".to_string(),
            api_token: None,
            jwt_secret: None,
            project_scope: Some(["verity".to_string()].into_iter().collect()),
            client: reqwest::Client::new(),
        };
        assert!(scoped.assert_project_scope("verity").is_ok());
        assert!(
            scoped.assert_project_scope(" verity ").is_ok(),
            "slugs are trimmed"
        );
        let err = scoped.assert_project_scope("lido").unwrap_err();
        assert!(err.contains("outside this controller's scope"), "{err}");

        let open = AssistantMcp {
            api_url: "http://127.0.0.1:3000".to_string(),
            api_token: None,
            jwt_secret: None,
            project_scope: None,
            client: reqwest::Client::new(),
        };
        assert!(open.assert_project_scope("anything").is_ok());
    }

    #[test]
    fn mission_start_tags_record_hermes_merge_authority() {
        let grant = MergeGrantConfig {
            authority_source: "owner-standing-grant-2026-07-29".to_string(),
            repositories: vec!["lfglabs-dev/*".to_string()],
        };
        let tags = mission_start_tags(
            Some(vec![
                "verity".to_string(),
                " origin:hermes-assistant ".to_string(),
            ]),
            true,
            true,
            Some("lfglabs-dev/verity#2209"),
            Some(&grant),
        )
        .unwrap();

        assert_eq!(
            tags,
            vec![
                "verity",
                "origin:hermes-assistant",
                "merge-authority:granted",
                "merge-authority-source:owner-standing-grant-2026-07-29",
                "merge-authority-target:lfglabs-dev/verity#2209"
            ]
        );
    }

    #[test]
    fn mission_start_tags_reject_ambiguous_merge_authority() {
        let grant = MergeGrantConfig {
            authority_source: "owner-grant".to_string(),
            repositories: vec!["owner/repo".to_string()],
        };
        assert!(mission_start_tags(None, true, true, Some("owner/repo#1"), None).is_err());
        assert!(mission_start_tags(None, true, true, Some("owner/repo"), Some(&grant)).is_err());
        assert!(mission_start_tags(None, true, true, Some("other/repo#1"), Some(&grant)).is_err());
        assert!(mission_start_tags(None, true, false, Some("owner/repo#1"), Some(&grant)).is_err());
        let tags = mission_start_tags(
            Some(vec![
                "merge-authority:granted".to_string(),
                "merge-authority-source:spoofed".to_string(),
            ]),
            false,
            false,
            Some("owner/repo#1"),
            Some(&grant),
        )
        .unwrap();
        assert!(!tags.iter().any(|tag| tag.starts_with("merge-authority:")));
    }

    #[test]
    fn origin_session_ids_are_validated_not_silently_dropped() {
        assert!(valid_origin_session_id("20260803_150605_59ab72"));
        assert!(valid_origin_session_id("agent:main:telegram:dm:1139694048"));
        // A blank or malformed id must not pass as "no origin": the mission
        // would complete with nowhere to report back to.
        assert!(!valid_origin_session_id(""));
        assert!(!valid_origin_session_id("   ".trim()));
        assert!(!valid_origin_session_id("bad session id"));
        assert!(!valid_origin_session_id(&"x".repeat(129)));
    }

    #[test]
    fn mission_acknowledgement_accepts_only_ack_waits_and_is_idempotent() {
        assert!(mission_requires_acknowledgement(&json!({
            "status": "awaiting_user",
            "awaiting_kind": "ack"
        }))
        .unwrap());
        assert!(!mission_requires_acknowledgement(&json!({
            "mission": {
                "status": "acknowledged",
                "awaiting_kind": null
            }
        }))
        .unwrap());
        assert!(mission_requires_acknowledgement(&json!({
            "status": "awaiting_user",
            "awaiting_kind": "decision"
        }))
        .is_err());
        assert!(mission_requires_acknowledgement(&json!({
            "status": "active",
            "awaiting_kind": null
        }))
        .is_err());
    }

    #[test]
    fn create_workspace_body_omits_unspecified_defaults_and_keeps_false() {
        let params: CreateWorkspaceParams = serde_json::from_value(json!({
            "name": "  hermes-test  ",
            "shared_network": false,
            "mcps": []
        }))
        .unwrap();

        let body = create_workspace_body(params).unwrap();

        assert_eq!(body["name"], "hermes-test");
        assert_eq!(body["shared_network"], false);
        assert_eq!(body["mcps"], json!([]));
        assert!(body.get("workspace_type").is_none());
        assert!(body.get("skills").is_none());
    }

    #[test]
    fn update_workspace_body_requires_a_change_and_preserves_explicit_empty_values() {
        let id = Uuid::new_v4();
        let empty: UpdateWorkspaceParams =
            serde_json::from_value(json!({"workspace_id": id})).unwrap();
        assert!(update_workspace_body(empty).is_err());

        let params: UpdateWorkspaceParams = serde_json::from_value(json!({
            "workspace_id": id,
            "skills": [],
            "shared_network": false,
            "config_profile": ""
        }))
        .unwrap();
        let (parsed_id, body) = update_workspace_body(params).unwrap();
        assert_eq!(parsed_id, id);
        assert_eq!(body["skills"], json!([]));
        assert_eq!(body["shared_network"], false);
        assert_eq!(body["config_profile"], "");
    }

    #[test]
    fn template_patch_preserves_omitted_fields_and_replaces_supplied_fields() {
        let mut current = json!({
            "description": "existing",
            "skills": ["old"],
            "env_vars": {"TOKEN": "secret"},
            "mcps_replace_defaults": true
        })
        .as_object()
        .unwrap()
        .clone();
        let params: SaveWorkspaceTemplateParams = serde_json::from_value(json!({
            "name": "lean",
            "skills": ["lean", "github"],
            "mcps_replace_defaults": false
        }))
        .unwrap();

        apply_template_patch(&mut current, params).unwrap();

        assert_eq!(current["description"], "existing");
        assert_eq!(current["env_vars"]["TOKEN"], "secret");
        assert_eq!(current["skills"], json!(["lean", "github"]));
        assert_eq!(current["mcps_replace_defaults"], false);
    }
}
