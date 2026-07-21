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
struct ListMissionsParams {
    #[serde(default)]
    status: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    tag: Option<String>,
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
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    desired_state: Option<String>,
    #[serde(default)]
    next_check_at: Option<String>,
    #[serde(default)]
    estimated_disk_gib: Option<u64>,
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
        // The public events endpoint intentionally starts at the oldest row
        // when no cursor is supplied. Hermes uses this bounded tool for live
        // reconciliation, so its useful default is the newest page.
        path.push_str(&format!("&before_seq={}", i64::MAX));
    }
    path
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
        Self {
            api_url,
            api_token,
            jwt_secret,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
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
        let mut req = self
            .client
            .post(format!("{}{}", self.api_url, path))
            .json(&body);
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
                        "project": {"type": "string", "description": "Optional filter: only missions with this project."},
                        "tag": {"type": "string", "description": "Optional filter: only missions carrying this tag."}
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
                        "project": {"type": "string", "description": "Optional filter: only missions with this project."},
                        "tag": {"type": "string", "description": "Optional filter: only missions carrying this tag."}
                    }
                }),
            },
            ToolDefinition {
                name: "get_mission".to_string(),
                description: "Compatibility alias for the compact ~2KB mission digest. It never returns the full history; use get_mission_events with a bounded limit for transcript or trace details.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "get_mission_digest".to_string(),
                description: "Compact ~2KB mission status: state, awaiting_kind, last user/assistant messages (truncated), GitHub PR links, project metadata. Use this instead of get_mission/get_mission_events for recaps and 'where is it?' checks — it avoids pulling whole transcripts into context.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
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
                name: "list_mission_shared_files".to_string(),
                description: "List files and screenshots shared by assistant messages in a sandboxed.sh mission.".to_string(),
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
                description: "Download a mission shared file URL to a local /tmp artifact path suitable for email attachments.".to_string(),
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
                description: "Create a new sandboxed.sh mission and send its initial prompt. Set backend explicitly when possible. For compatibility, a native agent name (codex/claudecode/gemini/grok) selects the matching backend when backend is omitted; ordinary library agent names do not. Pass project/track/intent/github_pr/tags so the mission carries structured metadata (so watchdogs/dashboards don't have to parse the title). Mark PR-changing work with writer=true; the API rejects concurrent writers for the same PR.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["title", "prompt"],
                    "properties": {
                        "title": {"type": "string"},
                        "prompt": {"type": "string"},
                        "workspace_id": {"type": "string"},
                        "backend": {"type": "string", "enum": ["opencode", "claudecode", "codex", "gemini", "grok"]},
                        "model_override": {"type": "string", "description": "Exact account-supported model ID. For Codex Terra use gpt-5.6-terra with medium effort. Never invent variants such as gpt-5.5-sol."},
                        "model_effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                        "config_profile": {"type": "string"},
                        "agent": {"type": "string"},
                        "project": {"type": "string", "description": "Stable project id (e.g. \"verity\")."},
                        "track": {"type": "string", "description": "Track/workstream (e.g. \"core-c3\")."},
                        "intent": {"type": "string", "description": "Intent (e.g. \"review_merge_pr\")."},
                        "github_pr": {"type": "string", "description": "Associated PR ref (e.g. \"owner/repo#123\")."},
                        "writer": {"type": "boolean", "description": "Whether this mission may modify the associated PR branch. Concurrent writers for one PR are rejected."},
                        "tags": {"type": "array", "items": {"type": "string"}},
                        "desired_state": {"type": "string", "description": "Track state, e.g. waiting_ci / waiting_review / blocked_external."},
                        "next_check_at": {"type": "string", "description": "When the track should next be checked (RFC3339)."},
                        "estimated_disk_gib": {"type": "integer", "minimum": 1, "maximum": 512, "description": "Expected peak local scratch use. Set this for Lean/build-heavy missions; omit for small/no-build work."}
                    }
                }),
            },
            ToolDefinition {
                name: "send_message_to_mission".to_string(),
                description: "Send a follow-up message to an existing mission.".to_string(),
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
                name: "cancel_mission".to_string(),
                description: "Cancel a running or pending mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "acknowledge_mission".to_string(),
                description: "Acknowledge a mission only after independently verifying its terminal result. This is the safe host-authenticated replacement for asking a workspace container to use the service JWT. It accepts only awaiting_user missions whose awaiting_kind is ack; decision waits and live missions are refused.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "get_compute_fleet".to_string(),
                description: "Get a compact live view of sandboxed.sh compute capacity: remote node health, labels, Lean readiness/toolchains, available slots, active/queued jobs, and recent placement receipts. Use this before dispatching parallel compute or choosing a remote validation node. Ordinary CPU/Lean work should prefer non-GPU nodes while they have immediate capacity; request the gpu label only for GPU work.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
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
                    "properties": {"mission_id": {"type": "string"}}
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
                description: "Change a mission's run settings for its NEXT turn: switch backend (claudecode/codex/opencode/gemini/grok), model, reasoning effort, or agent. Applies between turns — the mission must be idle (awaiting_user/acknowledged/interrupted), not actively running. If it is running, cancel_mission first (or wait), then update, then send_message_to_mission or resume_mission to kick the next turn. Note: model_effort only applies to claudecode and codex (low/medium/high/xhigh/max).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "backend": {"type": "string", "enum": ["opencode", "claudecode", "codex", "gemini", "grok"]},
                        "model_override": {"type": "string", "description": "Model id. Empty string clears it. When backend changes this is reset unless set explicitly."},
                        "model_effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                        "agent": {"type": "string", "description": "Agent name. Empty string clears it."},
                        "config_profile": {"type": "string"}
                    }
                }),
            },
            ToolDefinition {
                name: "resume_mission".to_string(),
                description: "Restart an interrupted, blocked, or failed mission. Reconstructs context from history and the work directory, then runs the next turn. Pass `content` to steer the resume with a concrete hint (e.g. 'you still have budget — keep going until the build passes; do not stop to ask'). Without `content` it sends the default continue-where-you-left-off prompt.".to_string(),
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
        if let Some(tag) = params.tag.as_deref() {
            path.push_str(&format!("&tag={}", urlencoding::encode(tag)));
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

    async fn list_active_missions(
        &self,
        limit: usize,
        project: Option<String>,
        tag: Option<String>,
    ) -> Result<Value, String> {
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
                tag,
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
        let id = parse_uuid(&params.mission_id)?;
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
        let id = parse_uuid(&params.mission_id)?;
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

    async fn list_mission_shared_files(
        &self,
        params: MissionSharedFilesParams,
    ) -> Result<Value, String> {
        let mission_id = parse_uuid(&params.mission_id)?;
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
        let mission_id = parse_uuid(&params.mission_id)?;
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
        let body = json!({
            "title": params.title,
            "workspace_id": workspace_id,
            "backend": backend,
            "model_override": params.model_override,
            "model_effort": params.model_effort,
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
            "tags": params.tags,
            "desired_state": params.desired_state,
            "next_check_at": params.next_check_at,
            "estimated_disk_gib": params.estimated_disk_gib,
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
        let mission_id = parse_uuid(&params.mission_id)?;
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
        let id = parse_uuid(&params.mission_id)?;
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
        let id = parse_uuid(&params.mission_id)?;
        let mut body = json!({
            "content": params.content,
            "sandbox": params.sandbox,
        });
        if let Some(tid) = params.thread_id.as_deref() {
            let tid = parse_uuid(tid)?;
            body["thread_id"] = json!(tid.to_string());
        }
        let response = self
            .api_post(&format!("/api/control/missions/{id}/ask"), body)
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

    async fn cancel_mission(&self, params: MissionIdParams) -> Result<Value, String> {
        let id = parse_uuid(&params.mission_id)?;
        let response = self
            .api_post(&format!("/api/control/missions/{id}/cancel"), json!({}))
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to cancel mission: {text}"));
        }
        Ok(json!({ "success": true, "cancelled": id.to_string() }))
    }

    async fn acknowledge_mission(&self, params: MissionIdParams) -> Result<Value, String> {
        let id = parse_uuid(&params.mission_id)?;
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
        let id = parse_uuid(&params.mission_id)?;
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
        if let Some(agent) = params.agent {
            body.insert("agent".to_string(), json!(agent));
        }
        if let Some(config_profile) = params.config_profile {
            body.insert("config_profile".to_string(), json!(config_profile));
        }
        if body.is_empty() {
            return Err("No settings provided. Set at least one of: backend, \
                        model_override, model_effort, agent, config_profile."
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
        let id = parse_uuid(&params.mission_id)?;
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
        let id = parse_uuid(&params.mission_id)?;
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
        let mut recommendation = build_recommendation(&status, &live, &analysis);
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
        let id = parse_uuid(&params.mission_id)?;
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
                let params: ListMissionsParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.list_active_missions(params.limit, params.project, params.tag)
                    .await
            }
            "list_missions" => {
                let params: ListMissionsParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.list_missions(params).await
            }
            "get_mission" => {
                let params: MissionIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_mission(params).await
            }
            "get_mission_digest" => {
                let params: MissionIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_mission_digest(params).await
            }
            "get_mission_events" => {
                let params: MissionEventsParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_mission_events(params).await
            }
            "list_mission_shared_files" => {
                let params: MissionSharedFilesParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.list_mission_shared_files(params).await
            }
            "download_shared_file" => {
                let params: DownloadSharedFileParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.download_shared_file(params).await
            }
            "start_mission" => {
                let params: StartMissionParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.start_mission(params).await
            }
            "send_message_to_mission" => {
                let params: SendMessageParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.send_message(params).await
            }
            "ask_mission" => {
                let params: AskMissionParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.ask_mission(params).await
            }
            "cancel_mission" => {
                let params: MissionIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.cancel_mission(params).await
            }
            "acknowledge_mission" => {
                let params: MissionIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.acknowledge_mission(params).await
            }
            "get_compute_fleet" => self.get_compute_fleet().await,
            "list_workspaces" => self.list_workspaces().await,
            "get_workspace" => {
                let params: WorkspaceIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_workspace(params).await
            }
            "create_workspace" => {
                let params: CreateWorkspaceParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.create_workspace(params).await
            }
            "update_workspace" => {
                let params: UpdateWorkspaceParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.update_workspace(params).await
            }
            "delete_workspace" => {
                let params: DeleteWorkspaceParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.delete_workspace(params).await
            }
            "list_workspace_templates" => self.list_workspace_templates().await,
            "get_workspace_template" => {
                let params: WorkspaceTemplateNameParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_workspace_template(params).await
            }
            "save_workspace_template" => {
                let params: SaveWorkspaceTemplateParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.save_workspace_template(params).await
            }
            "delete_workspace_template" => {
                let params: DeleteWorkspaceTemplateParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.delete_workspace_template(params).await
            }
            "rebuild_workspace_from_template" => {
                let params: RebuildWorkspaceFromTemplateParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.rebuild_workspace_from_template(params).await
            }
            "workspace_bash" => {
                let params: WorkspaceBashParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.workspace_bash(params).await
            }
            "start_workspace_job" => {
                let params: StartWorkspaceJobParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.start_workspace_job(params).await
            }
            "get_workspace_job" => {
                let params: WorkspaceJobParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_workspace_job(params).await
            }
            "cancel_workspace_job" => {
                let params: WorkspaceJobParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.cancel_workspace_job(params).await
            }
            "get_mission_health" => {
                let params: MissionHealthParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_mission_health(params).await
            }
            "get_mission_diagnostics" => {
                let params: MissionDiagnosticsParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.get_mission_diagnostics(params).await
            }
            "update_mission_settings" => {
                let params: UpdateSettingsParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.update_mission_settings(params).await
            }
            "resume_mission" => {
                let params: ResumeMissionParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
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
        },
        "nodes": compact_nodes,
        "recent_jobs": fleet.get("recent_jobs").cloned().unwrap_or_else(|| json!([])),
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
fn build_recommendation(status: &str, live: &Value, analysis: &TraceAnalysis) -> String {
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
        let rec = build_recommendation("active", &Value::Null, &analysis);
        assert!(rec.contains("rate-limiting") || rec.contains("capacity"));
    }

    #[test]
    fn recommendation_flags_severe_stall() {
        let analysis = TraceAnalysis::default();
        let live = json!({
            "seconds_since_activity": 600,
            "health": {"status": "stalled", "severity": "severe"}
        });
        let rec = build_recommendation("active", &live, &analysis);
        assert!(rec.contains("stalled"));
    }

    #[test]
    fn recommendation_does_not_recommend_resume_for_not_feasible() {
        // not_feasible is not resumable server-side (see mission_can_be_resumed
        // in src/api/control.rs). The recommendation must steer the babysitter
        // away from calling resume_mission, otherwise it gets a hard failure.
        let analysis = TraceAnalysis::default();
        let rec = build_recommendation("not_feasible", &Value::Null, &analysis);
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
            let rec = build_recommendation(status, &Value::Null, &analysis);
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
