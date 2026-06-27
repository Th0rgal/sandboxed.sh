//! MCP server for a standalone Hermes assistant.
//!
//! This is intentionally narrower than `orchestrator-mcp`: it exposes the
//! control-plane tools a personal assistant needs without deployment or
//! durable-job capabilities.

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
}

#[derive(Debug, Deserialize)]
struct SendMessageParams {
    mission_id: String,
    content: String,
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
            client: reqwest::Client::new(),
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
                description: "Get one mission by UUID.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "get_mission_events".to_string(),
                description: "Fetch persisted mission events, usually with view='transcript' for chat history or view='all' for debugging.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {"type": "string"},
                        "limit": {"type": "integer", "description": "Maximum events to return, default 40."},
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
                description: "Create a new sandboxed.sh mission and send its initial prompt.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["title", "prompt"],
                    "properties": {
                        "title": {"type": "string"},
                        "prompt": {"type": "string"},
                        "workspace_id": {"type": "string"},
                        "backend": {"type": "string", "enum": ["opencode", "claudecode", "codex", "gemini", "grok"]},
                        "model_override": {"type": "string"},
                        "model_effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                        "config_profile": {"type": "string"},
                        "agent": {"type": "string"}
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
                name: "cancel_mission".to_string(),
                description: "Cancel a running or pending mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {"mission_id": {"type": "string"}}
                }),
            },
            ToolDefinition {
                name: "list_workspaces".to_string(),
                description: "List sandboxed.sh workspaces so new missions can target the right environment.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "workspace_bash".to_string(),
                description: "Run a bash command inside a sandboxed.sh workspace — the same context missions run in, with the workspace's configured environment variables and (when a GitHub account is connected in the dashboard) GitHub git credentials wired in for `git push`. Prefer this over local bash for git operations and anything needing workspace secrets.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["command"],
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run in the workspace."},
                        "workspace_id": {"type": "string", "description": "Workspace UUID. Defaults to the assistant's default workspace."},
                        "cwd": {"type": "string", "description": "Working directory relative to the workspace root."},
                        "timeout_secs": {"type": "integer", "description": "Timeout in seconds, default 300, max 600."}
                    }
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
                description: "Change a mission's run settings for its NEXT turn: switch backend (claudecode/codex/opencode/gemini/grok), model, reasoning effort, or agent. Applies between turns — the mission must be idle (awaiting_user/acknowledged/interrupted), not actively running. If it is running, cancel_mission first (or wait), then update, then send_message_to_mission or resume_mission to kick the next turn. Note: model_effort only applies to claudecode (low/medium/high/xhigh/max) and codex (low/medium/high).".to_string(),
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
        let id = parse_uuid(&params.mission_id)?;
        let response = self.api_get(&format!("/api/control/missions/{id}")).await?;
        if !response.status().is_success() {
            return Err(format!("Mission not found: {}", response.status()));
        }
        response
            .json()
            .await
            .map_err(|error| format!("Failed to parse mission: {error}"))
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
        let mut path = format!(
            "/api/control/missions/{id}/events?limit={limit}&view={view}&include_counts=false"
        );
        if let Some(before_seq) = params.before_seq {
            path.push_str(&format!("&before_seq={before_seq}"));
        } else if let Some(since_seq) = params.since_seq {
            path.push_str(&format!("&since_seq={since_seq}"));
        }
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
        let body = json!({
            "title": params.title,
            "workspace_id": workspace_id,
            "backend": params.backend,
            "model_override": params.model_override,
            "model_effort": params.model_effort,
            "config_profile": params.config_profile,
            "agent": params.agent,
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
        let Some(mission_id) = mission["id"].as_str() else {
            return Err("Created mission response did not include an id".to_string());
        };
        self.send_message(SendMessageParams {
            mission_id: mission_id.to_string(),
            content: params.prompt,
        })
        .await?;
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
                    "timeout_secs": params.timeout_secs,
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
            "cancel_mission" => {
                let params: MissionIdParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.cancel_mission(params).await
            }
            "list_workspaces" => self.list_workspaces().await,
            "workspace_bash" => {
                let params: WorkspaceBashParams = serde_json::from_value(arguments)
                    .map_err(|error| format!("Invalid params: {error}"))?;
                self.workspace_bash(params).await
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
                if is_sensitive_key(key) {
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
            "model_override": "gpt-5.5",
            "workspace_id": "workspace-1",
            "workspace_name": "assistant",
            "short_description": "Build fix",
            "updated_at": "2026-05-28T12:00:00Z",
            "project": "verity-core",
            "track": "C3-bridge-collapse",
            "github_pr": 2061,
            "tags": ["c3", "sprint-2"],
            "awaiting_kind": "decision",
            "prompt": "secret prompt",
            "api_token": "sk-test",
        }));

        assert_eq!(summary["id"], "mission-1");
        assert_eq!(summary["workspace_name"], "assistant");
        assert_eq!(summary["project"], "verity-core");
        assert_eq!(summary["track"], "C3-bridge-collapse");
        assert_eq!(summary["github_pr"], 2061);
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
                "notes": ["visible", "github_pat_123"]
            },
            "token": "plain-token"
        });

        scrub_sensitive_json(&mut value);

        assert_eq!(value["mission"]["title"], "Hermes");
        assert_eq!(value["mission"]["api_key"], "[redacted]");
        assert_eq!(value["mission"]["notes"][0], "visible");
        assert_eq!(value["mission"]["notes"][1], "[redacted]");
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
}
