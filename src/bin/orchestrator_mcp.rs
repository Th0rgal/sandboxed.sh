//! MCP Server for orchestrating parallel agent missions.
//!
//! Provides boss agents with tools to create, monitor, and manage worker missions.
//! Communicates over stdio using JSON-RPC 2.0.

use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use jsonwebtoken::{EncodingKey, Header};
use sandboxed_sh::ai_providers::ProviderType;
use sandboxed_sh::api::ai_providers::{
    default_backends_for_provider, get_openai_api_key_for_codex_default, provider_targets_backend,
    read_oauth_token_entry,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

// =============================================================================
// JSON-RPC Types (same pattern as automation-manager-mcp)
// =============================================================================

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
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
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
                data: None,
            }),
        }
    }
}

// =============================================================================
// MCP Types
// =============================================================================

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

// =============================================================================
// Tool Params
// =============================================================================

#[derive(Debug, Deserialize)]
struct CreateWorkerParams {
    title: String,
    #[serde(default)]
    agent: Option<String>,
    /// Backend to use: "claudecode", "codex", "gemini", "opencode", "amp"
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    model_effort: Option<String>,
    #[serde(default)]
    config_profile: Option<String>,
    #[serde(default)]
    working_directory: Option<String>,
    /// Initial prompt to send to the worker after creation.
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchCreateWorkersParams {
    /// Array of worker definitions
    workers: Vec<CreateWorkerParams>,
}

#[derive(Debug, Deserialize)]
struct WaitForAnyWorkerParams {
    /// UUIDs of the worker missions to wait for
    mission_ids: Vec<String>,
    /// Target statuses to wait for (default: completed, failed, interrupted)
    #[serde(default)]
    target_statuses: Vec<String>,
    /// Maximum seconds to wait (default: 600 = 10 minutes)
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// Poll interval in seconds (default: 10)
    #[serde(default = "default_poll_interval")]
    poll_interval_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct GetWorkerStatusParams {
    mission_id: String,
}

#[derive(Debug, Deserialize)]
struct CancelWorkerParams {
    mission_id: String,
}

#[derive(Debug, Deserialize)]
struct SendMessageParams {
    mission_id: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct BackendAuthStatusParams {
    #[serde(default)]
    backend: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateWorktreeParams {
    /// Path relative to the workspace root where the worktree will be created
    path: String,
    /// Branch name (will be created if it doesn't exist)
    branch: String,
    /// Optional: base branch to create from (defaults to HEAD)
    #[serde(default)]
    base: Option<String>,
    /// Optional: path to the git repo directory. If omitted, auto-detects by
    /// searching for .git within the workspace root (2 levels deep).
    #[serde(default)]
    repo_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoveWorktreeParams {
    /// Path of the worktree to remove
    path: String,
    /// Optional: path to the git repo directory. If omitted, auto-detects by
    /// searching for .git within the workspace root (2 levels deep).
    #[serde(default)]
    repo_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WaitForWorkerParams {
    /// UUID of the worker mission to wait for
    mission_id: String,
    /// Target statuses to wait for (default: completed, failed, interrupted)
    #[serde(default)]
    target_statuses: Vec<String>,
    /// Maximum seconds to wait (default: 600 = 10 minutes)
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// Poll interval in seconds (default: 10)
    #[serde(default = "default_poll_interval")]
    poll_interval_seconds: u64,
}

fn default_timeout() -> u64 {
    600
}

fn default_poll_interval() -> u64 {
    10
}

// =============================================================================
// JWT helpers (lightweight – mirrors auth.rs Claims)
// =============================================================================

#[derive(Debug, Serialize)]
struct JwtClaims {
    sub: String,
    usr: String,
    iat: i64,
    exp: i64,
}

/// Mint a short-lived service JWT using the shared secret.
fn mint_service_jwt(secret: &str) -> Option<String> {
    let now = Utc::now();
    let exp = now + chrono::Duration::hours(24);
    let claims = JwtClaims {
        sub: "orchestrator-mcp".to_string(),
        usr: "orchestrator-mcp".to_string(),
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

// =============================================================================
// Orchestrator MCP Server
// =============================================================================

struct OrchestratorMcp {
    mission_id: Uuid,
    api_url: String,
    api_token: Option<String>,
    client: reqwest::Client,
}

impl OrchestratorMcp {
    fn new(mission_id: Uuid, api_url: String, api_token: Option<String>) -> Self {
        Self {
            mission_id,
            api_url,
            api_token,
            client: reqwest::Client::new(),
        }
    }

    fn auth_header(&self) -> Option<(String, String)> {
        self.api_token
            .as_ref()
            .map(|t| ("Authorization".to_string(), format!("Bearer {}", t)))
    }

    fn get_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "get_workspace_layout".to_string(),
                description: "Return the boss mission's workspace paths so you can stop guessing where the real project root is before delegating.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDefinition {
                name: "get_backend_auth_status".to_string(),
                description: "Report which backends are actually usable from the backend's credential store and provider targeting. Use this instead of guessing from shell env vars or CLI login checks.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "backend": {
                            "type": "string",
                            "enum": ["claudecode", "codex", "gemini", "opencode", "amp"],
                            "description": "Optional single backend to inspect. If omitted, returns all common backends."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "create_worker_mission".to_string(),
                description: "Create a new worker mission (child of the current boss mission). The worker will start executing immediately. IMPORTANT: You must set the 'backend' field to match the harness you want (claudecode, codex, gemini, opencode). If omitted, defaults to the workspace default (usually claudecode).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["title", "prompt"],
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "Descriptive title for the worker mission"
                        },
                        "backend": {
                            "type": "string",
                            "enum": ["claudecode", "codex", "gemini", "opencode", "amp"],
                            "description": "Backend/harness to use. MUST match the model: claudecode for Claude models, codex for OpenAI/GPT models, gemini for Gemini models, opencode for any model via provider routing, amp for Amp."
                        },
                        "model_override": {
                            "type": "string",
                            "description": "Model to use. Must match the backend: Claude models (e.g. 'claude-opus-4-7') for claudecode, GPT models (e.g. 'gpt-5.5') for codex, Gemini models for gemini, 'provider/model' format for opencode."
                        },
                        "model_effort": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "xhigh", "max"],
                            "description": "Effort level. Supported by codex and claudecode backends."
                        },
                        "agent": {
                            "type": "string",
                            "description": "Agent name from library (optional, for opencode backend)."
                        },
                        "config_profile": {
                            "type": "string",
                            "description": "Config profile name to use for this worker"
                        },
                        "working_directory": {
                            "type": "string",
                            "description": "Working directory for the worker (e.g. a git worktree path). If omitted, uses the boss mission's repo directory."
                        },
                        "prompt": {
                            "type": "string",
                            "description": "Initial prompt/instructions to send to the worker after creation. Must be self-contained."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "batch_create_workers".to_string(),
                description: "Create multiple worker missions at once. Each worker is created independently — if one fails, others still succeed. The batch is automatically capped based on container resource limits (PIDs, memory) to prevent resource exhaustion. Check the 'warning' field in the response if your batch was reduced.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["workers"],
                    "properties": {
                        "workers": {
                            "type": "array",
                            "description": "Array of worker definitions. Each has the same schema as create_worker_mission.",
                            "items": {
                                "type": "object",
                                "required": ["title", "prompt"],
                                "properties": {
                                    "title": { "type": "string" },
                                    "backend": { "type": "string", "enum": ["claudecode", "codex", "gemini", "opencode", "amp"] },
                                    "model_override": { "type": "string" },
                                    "model_effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh", "max"] },
                                    "agent": { "type": "string" },
                                    "config_profile": { "type": "string" },
                                    "working_directory": { "type": "string" },
                                    "prompt": { "type": "string" }
                                }
                            }
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "list_worker_missions".to_string(),
                description: "List all worker missions spawned by this boss mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDefinition {
                name: "get_worker_status".to_string(),
                description: "Get the current status and details of a specific worker mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "cancel_worker".to_string(),
                description: "Cancel a specific worker mission.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission to cancel"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "cancel_all_workers".to_string(),
                description: "Cancel all active worker missions.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDefinition {
                name: "send_message_to_worker".to_string(),
                description: "Send a follow-up message to a worker mission. This can continue a running worker and can also reactivate an interrupted, failed, completed, or pending worker by queuing new targeted work.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "content"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission"
                        },
                        "content": {
                            "type": "string",
                            "description": "Message content to send to the worker"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "resume_worker".to_string(),
                description: "Resume or retry a worker by sending it a targeted follow-up message. Use this when a worker was interrupted, failed, blocked, or needs corrective guidance.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "content"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission"
                        },
                        "content": {
                            "type": "string",
                            "description": "Recovery or retry instructions for the worker"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "retask_worker".to_string(),
                description: "Change a worker's assignment by sending it a new targeted prompt. Use this instead of abandoning a worker when its scope should change.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id", "content"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission"
                        },
                        "content": {
                            "type": "string",
                            "description": "Message content to send to the worker"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "create_worktree".to_string(),
                description: "Create a git worktree for a worker to use as an isolated working directory. The worktree will be on its own branch so workers don't conflict.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["path", "branch"],
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path where the worktree will be created (e.g. /workspaces/mission-xxx/verity-worker-1)"
                        },
                        "branch": {
                            "type": "string",
                            "description": "Branch name for the worktree (will be created if it doesn't exist)"
                        },
                        "base": {
                            "type": "string",
                            "description": "Base branch/commit to create from (defaults to HEAD)"
                        },
                        "repo_path": {
                            "type": "string",
                            "description": "Path to the git repo to create the worktree from. If omitted, auto-detects by searching for .git within the workspace root (2 levels deep). Use this when the repo is in a subdirectory (e.g. /workspaces/mission-xxx/verity)."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "remove_worktree".to_string(),
                description: "Remove a git worktree that is no longer needed.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path of the worktree to remove"
                        },
                        "repo_path": {
                            "type": "string",
                            "description": "Path to the git repo that owns the worktree. If omitted, auto-detects by searching for .git within the workspace root (2 levels deep)."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "wait_for_worker".to_string(),
                description: "Block until a single worker mission reaches a terminal status. Use wait_for_any_worker to monitor multiple workers simultaneously.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission to wait for"
                        },
                        "target_statuses": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Statuses to wait for (default: ['completed', 'failed', 'interrupted'])"
                        },
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "Maximum seconds to wait (default: 600)"
                        },
                        "poll_interval_seconds": {
                            "type": "integer",
                            "description": "Seconds between status checks (default: 10)"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "wait_for_any_worker".to_string(),
                description: "Block until ANY of the specified worker missions reaches a terminal status. Returns the first worker that finishes. Use this to monitor a pool of workers and react as each completes.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_ids"],
                    "properties": {
                        "mission_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "UUIDs of worker missions to monitor"
                        },
                        "target_statuses": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Statuses to wait for (default: ['completed', 'failed', 'interrupted'])"
                        },
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "Maximum seconds to wait (default: 600)"
                        },
                        "poll_interval_seconds": {
                            "type": "integer",
                            "description": "Seconds between status checks (default: 10)"
                        }
                    }
                }),
            },
        ]
    }

    async fn api_get(&self, path: &str) -> Result<reqwest::Response, String> {
        let url = format!("{}{}", self.api_url, path);
        let mut req = self.client.get(&url);
        if let Some((k, v)) = self.auth_header() {
            req = req.header(k, v);
        }
        req.send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))
    }

    async fn api_post(&self, path: &str, body: Value) -> Result<reqwest::Response, String> {
        let url = format!("{}{}", self.api_url, path);
        let mut req = self.client.post(&url).json(&body);
        if let Some((k, v)) = self.auth_header() {
            req = req.header(k, v);
        }
        req.send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))
    }

    async fn create_worker(&self, params: CreateWorkerParams) -> Result<Value, String> {
        let body = json!({
            "title": params.title,
            "agent": params.agent,
            "backend": params.backend,
            "model_override": params.model_override,
            "model_effort": params.model_effort,
            "config_profile": params.config_profile,
            "parent_mission_id": self.mission_id.to_string(),
            "working_directory": params.working_directory,
        });

        let response = self.api_post("/api/control/missions", body).await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to create worker mission: {}", text));
        }

        let mission: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let worker_id = mission["id"].as_str().unwrap_or("");

        // If a prompt was provided, send it as the first message
        if let Some(prompt) = params.prompt {
            if !prompt.trim().is_empty() && !worker_id.is_empty() {
                let msg_body = json!({
                    "content": prompt,
                    "mission_id": worker_id,
                });
                if let Err(e) = self.api_post("/api/control/message", msg_body).await {
                    eprintln!("[orchestrator-mcp] Warning: created mission but failed to send initial prompt: {}", e);
                }
            }
        }

        Ok(mission)
    }

    async fn list_workers(&self) -> Result<Value, String> {
        // List all missions and filter for children of this boss mission
        let response = self
            .api_get("/api/control/missions?limit=100&offset=0")
            .await?;

        if !response.status().is_success() {
            return Err(format!("API returned error: {}", response.status()));
        }

        let missions: Vec<Value> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Filter to only child missions of this boss
        let boss_id = self.mission_id.to_string();
        let workers: Vec<&Value> = missions
            .iter()
            .filter(|m| m["parent_mission_id"].as_str() == Some(&boss_id))
            .collect();

        Ok(json!({
            "boss_mission_id": boss_id,
            "worker_count": workers.len(),
            "workers": workers,
        }))
    }

    async fn get_worker_status(&self, params: GetWorkerStatusParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.mission_id)
            .map_err(|_| "Invalid mission ID format".to_string())?;

        let response = self
            .api_get(&format!("/api/control/missions/{}", id))
            .await?;

        if !response.status().is_success() {
            return Err(format!("Worker mission not found: {}", response.status()));
        }

        let mission: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(mission)
    }

    async fn cancel_worker(&self, params: CancelWorkerParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.mission_id)
            .map_err(|_| "Invalid mission ID format".to_string())?;

        let response = self
            .api_post(&format!("/api/control/missions/{}/cancel", id), json!({}))
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to cancel worker: {}", text));
        }

        Ok(json!({"success": true, "cancelled": id.to_string()}))
    }

    async fn cancel_all_workers(&self) -> Result<Value, String> {
        let list = self.list_workers().await?;
        let workers = list["workers"].as_array().cloned().unwrap_or_default();
        let mut cancelled = Vec::new();
        let mut errors = Vec::new();

        for worker in &workers {
            let status = worker["status"].as_str().unwrap_or("");
            if status == "completed"
                || status == "failed"
                || status == "interrupted"
                || status == "not_feasible"
            {
                continue;
            }
            let id = worker["id"].as_str().unwrap_or("");
            if id.is_empty() {
                continue;
            }
            match self
                .cancel_worker(CancelWorkerParams {
                    mission_id: id.to_string(),
                })
                .await
            {
                Ok(_) => cancelled.push(id.to_string()),
                Err(e) => errors.push(format!("{}: {}", id, e)),
            }
        }

        Ok(json!({
            "cancelled": cancelled,
            "errors": errors,
        }))
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.mission_id)
            .map_err(|_| "Invalid mission ID format".to_string())?;

        let body = json!({
            "content": params.content,
            "mission_id": id.to_string(),
        });

        let response = self.api_post("/api/control/message", body).await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to send message: {}", text));
        }

        let result: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(result)
    }

    async fn resume_worker(&self, params: SendMessageParams) -> Result<Value, String> {
        let mission_id = params.mission_id.clone();
        let result = self.send_message(params).await?;
        Ok(json!({
            "success": true,
            "action": "resume_worker",
            "mission_id": mission_id,
            "result": result,
        }))
    }

    async fn retask_worker(&self, params: SendMessageParams) -> Result<Value, String> {
        let mission_id = params.mission_id.clone();
        let result = self.send_message(params).await?;
        Ok(json!({
            "success": true,
            "action": "retask_worker",
            "mission_id": mission_id,
            "result": result,
        }))
    }

    fn get_workspace_layout(&self) -> Value {
        let workspace_dir = std::env::var("WORKING_DIR").ok();
        let workspace_root = std::env::var("SANDBOXED_SH_WORKSPACE_ROOT").ok();
        let workspace_mount = std::env::var("SANDBOXED_SH_WORKSPACE").ok();
        let workspace_type = std::env::var("SANDBOXED_SH_WORKSPACE_TYPE").ok();
        let runtime_workspace_file = std::env::var("SANDBOXED_SH_RUNTIME_WORKSPACE_FILE").ok();

        let git_root = workspace_dir.as_deref().and_then(find_git_root);

        json!({
            "workspace_dir": workspace_dir,
            "workspace_root": workspace_root,
            "workspace_mount": workspace_mount,
            "workspace_type": workspace_type,
            "runtime_workspace_file": runtime_workspace_file,
            "git_root": git_root,
        })
    }

    fn get_backend_auth_status(&self, params: BackendAuthStatusParams) -> Value {
        let backends = params
            .backend
            .map(|backend| vec![backend])
            .unwrap_or_else(|| {
                vec![
                    "claudecode".to_string(),
                    "codex".to_string(),
                    "gemini".to_string(),
                    "opencode".to_string(),
                ]
            });

        let data_dir = openagent_data_dir();
        // provider_targets_backend / load_ai_providers expect a workspace root
        // and internally append `.sandboxed-sh/...`. The data_dir already IS
        // `.sandboxed-sh`, so we use its parent as the workspace root.
        let workspace_root = data_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| data_dir.clone());
        let auth_json_path = opencode_auth_json_path();
        let codex_auth_path = codex_auth_json_path();

        let statuses: Vec<Value> = backends
            .into_iter()
            .map(|backend| match backend.as_str() {
                "claudecode" => backend_auth_entry(
                    "claudecode",
                    ProviderType::Anthropic,
                    &workspace_root,
                    provider_targets_backend(&workspace_root, ProviderType::Anthropic, "claudecode"),
                    read_oauth_token_entry(ProviderType::Anthropic).is_some(),
                    false,
                    None,
                ),
                "codex" => {
                    let has_oauth = read_oauth_token_entry(ProviderType::OpenAI).is_some();
                    let has_api_key =
                        get_openai_api_key_for_codex_default(&workspace_root).is_some();
                    let has_host_auth = looks_like_json_file(&codex_auth_path);
                    let targeted =
                        provider_targets_backend(&workspace_root, ProviderType::OpenAI, "codex");
                    backend_auth_entry(
                        "codex",
                        ProviderType::OpenAI,
                        &workspace_root,
                        targeted,
                        has_oauth || has_api_key || has_host_auth,
                        has_host_auth,
                        Some(json!({
                            "has_api_key": has_api_key,
                            "has_oauth": has_oauth,
                            "has_host_auth_json": has_host_auth,
                        })),
                    )
                }
                "gemini" => backend_auth_entry(
                    "gemini",
                    ProviderType::Google,
                    &workspace_root,
                    provider_targets_backend(&workspace_root, ProviderType::Google, "gemini"),
                    read_oauth_token_entry(ProviderType::Google).is_some()
                        || opencode_auth_has_provider(&auth_json_path, "google")
                        || opencode_auth_has_provider(&auth_json_path, "gemini"),
                    false,
                    Some(json!({
                        "default_backends": default_backends_for_provider(ProviderType::Google),
                    })),
                ),
                "opencode" => json!({
                    "backend": "opencode",
                    "ready": true,
                    "reason": "OpenCode routes through configured providers; inspect provider selection separately.",
                }),
                "amp" => json!({
                    "backend": "amp",
                    "ready": true,
                    "reason": "Amp auth is handled by AMP_API_KEY or provider store, not this tool.",
                }),
                other => json!({
                    "backend": other,
                    "ready": false,
                    "reason": "Unknown backend",
                }),
            })
            .collect();

        json!({
            "openagent_data_dir": data_dir,
            "statuses": statuses,
            "note": "Shell env vars and CLI login status inside a worker shell are not authoritative for backend auth.",
        })
    }

    fn create_worktree(&self, params: CreateWorktreeParams) -> Result<Value, String> {
        let path = &params.path;
        let branch = &params.branch;
        let repo_dir = resolve_repo_path(params.repo_path.as_deref());

        // Check if branch exists
        let branch_exists = Command::new("git")
            .current_dir(&repo_dir)
            .args(["rev-parse", "--verify", branch])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let output = if branch_exists {
            // Branch exists, just create worktree on it
            Command::new("git")
                .current_dir(&repo_dir)
                .args(["worktree", "add", path, branch])
                .output()
                .map_err(|e| format!("Failed to run git worktree add: {}", e))?
        } else {
            // Create new branch from base
            let base = params.base.as_deref().unwrap_or("HEAD");
            Command::new("git")
                .current_dir(&repo_dir)
                .args(["worktree", "add", "-b", branch, path, base])
                .output()
                .map_err(|e| format!("Failed to run git worktree add: {}", e))?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree add failed: {}", stderr));
        }

        Ok(json!({
            "success": true,
            "path": path,
            "branch": branch,
            "repo_path": repo_dir,
            "message": format!("Worktree created at {} on branch {} (repo: {})", path, branch, repo_dir),
        }))
    }

    fn remove_worktree(&self, params: RemoveWorktreeParams) -> Result<Value, String> {
        let repo_dir = resolve_repo_path(params.repo_path.as_deref());

        let output = Command::new("git")
            .current_dir(&repo_dir)
            .args(["worktree", "remove", "--force", &params.path])
            .output()
            .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree remove failed: {}", stderr));
        }

        Ok(json!({
            "success": true,
            "path": params.path,
            "repo_path": repo_dir,
            "message": format!("Worktree removed at {}", params.path),
        }))
    }

    async fn batch_create_workers(
        &self,
        params: BatchCreateWorkersParams,
    ) -> Result<Value, String> {
        let requested = params.workers.len();
        let resource_cap = estimate_max_workers();
        let cap = resource_cap.max_workers;
        let capped = requested > cap;

        let mut results = Vec::new();
        let mut errors = Vec::new();
        let mut skipped = 0usize;

        let mut created_count = 0usize;
        for (i, worker_params) in params.workers.into_iter().enumerate() {
            if created_count >= cap {
                skipped += 1;
                errors.push(json!({
                    "index": i,
                    "success": false,
                    "error": format!(
                        "Skipped: resource cap reached ({} max workers). {}",
                        cap, resource_cap.reason
                    ),
                }));
                continue;
            }
            match self.create_worker(worker_params).await {
                Ok(mission) => {
                    created_count += 1;
                    results.push(json!({
                        "index": i,
                        "success": true,
                        "mission": mission,
                    }));
                }
                Err(e) => {
                    errors.push(json!({
                        "index": i,
                        "success": false,
                        "error": e,
                    }));
                }
            }
        }

        let mut resp = json!({
            "created": results.len(),
            "failed": errors.len(),
            "results": results,
            "errors": errors,
        });

        if capped {
            resp["warning"] = json!(format!(
                "Requested {} workers but capped to {} based on container resources. {}. \
                 {} workers were skipped. Consider running workers sequentially or \
                 reducing parallelism.",
                requested, cap, resource_cap.reason, skipped
            ));
            resp["resource_cap"] = json!({
                "max_workers": cap,
                "pids_available": resource_cap.pids_available,
                "pids_max": resource_cap.pids_max,
                "memory_available_mb": resource_cap.memory_available_mb,
                "memory_max_mb": resource_cap.memory_max_mb,
            });
        }

        Ok(resp)
    }

    async fn wait_for_any_worker(&self, params: WaitForAnyWorkerParams) -> Result<Value, String> {
        let mut ids = Vec::new();
        let mut invalid_ids = Vec::new();
        for s in &params.mission_ids {
            match Uuid::parse_str(s) {
                Ok(id) => ids.push(id),
                Err(_) => invalid_ids.push(s.clone()),
            }
        }

        if !invalid_ids.is_empty() {
            return Err(format!(
                "Invalid mission ID format: {}",
                invalid_ids.join(", ")
            ));
        }

        if ids.is_empty() {
            return Err("No mission IDs provided".to_string());
        }

        let target_statuses = if params.target_statuses.is_empty() {
            vec![
                "completed".to_string(),
                "failed".to_string(),
                "interrupted".to_string(),
                "not_feasible".to_string(),
            ]
        } else {
            params.target_statuses
        };

        // Clamp timeout to MAX_INTERNAL_TIMEOUT_SECS to stay under Codex CLI's 120s limit
        let effective_timeout_secs = params.timeout_seconds.min(MAX_INTERNAL_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(effective_timeout_secs);
        let interval = std::time::Duration::from_secs(params.poll_interval_seconds);
        let start = std::time::Instant::now();
        let mut error_counts: std::collections::HashMap<Uuid, u32> =
            ids.iter().map(|id| (*id, 0u32)).collect();
        // Track latest known status per worker for the timeout snapshot
        let mut last_statuses: std::collections::HashMap<Uuid, Value> =
            std::collections::HashMap::new();

        loop {
            for id in &ids {
                let response = self.api_get(&format!("/api/control/missions/{}", id)).await;
                let errors = error_counts.get_mut(id).unwrap();

                match response {
                    Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                        Ok(mission) => {
                            *errors = 0;
                            let status = mission["status"].as_str().unwrap_or("");
                            last_statuses.insert(
                                *id,
                                json!({
                                    "mission_id": id.to_string(),
                                    "status": status,
                                    "title": mission["title"].as_str().unwrap_or(""),
                                }),
                            );
                            if target_statuses.iter().any(|s| s == status) {
                                return Ok(json!({
                                    "reached_target": true,
                                    "internal_timeout": false,
                                    "mission_id": id.to_string(),
                                    "status": status,
                                    "elapsed_seconds": start.elapsed().as_secs(),
                                    "mission": mission,
                                }));
                            }
                        }
                        Err(e) => {
                            *errors += 1;
                            if *errors >= 3 {
                                return Err(format!(
                                    "Mission {} returned invalid JSON: {} ({} consecutive errors)",
                                    id, e, errors
                                ));
                            }
                        }
                    },
                    Ok(resp) => {
                        *errors += 1;
                        if *errors >= 3 {
                            return Err(format!(
                                "Mission {} returned HTTP {} ({} consecutive errors)",
                                id,
                                resp.status(),
                                errors
                            ));
                        }
                    }
                    Err(e) => {
                        *errors += 1;
                        if *errors >= 3 {
                            return Err(format!(
                                "API request failed for mission {}: {} ({} consecutive errors)",
                                id, e, errors
                            ));
                        }
                    }
                }
            }

            if start.elapsed() > timeout {
                let worker_snapshots: Vec<Value> = ids
                    .iter()
                    .map(|id| {
                        last_statuses.get(id).cloned().unwrap_or_else(|| {
                            json!({
                                "mission_id": id.to_string(),
                                "status": "unknown",
                            })
                        })
                    })
                    .collect();
                let was_clamped = params.timeout_seconds > MAX_INTERNAL_TIMEOUT_SECS;
                return Ok(json!({
                    "reached_target": false,
                    "internal_timeout": was_clamped,
                    "timeout": true,
                    "elapsed_seconds": start.elapsed().as_secs(),
                    "effective_timeout_seconds": effective_timeout_secs,
                    "requested_timeout_seconds": params.timeout_seconds,
                    "worker_statuses": worker_snapshots,
                    "hint": "No worker reached a target status within the time limit. Call wait_for_any_worker again to continue waiting.",
                }));
            }

            tokio::time::sleep(interval).await;
        }
    }

    async fn wait_for_worker(&self, params: WaitForWorkerParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.mission_id)
            .map_err(|_| "Invalid mission ID format".to_string())?;

        let target_statuses = if params.target_statuses.is_empty() {
            vec![
                "completed".to_string(),
                "failed".to_string(),
                "interrupted".to_string(),
                "not_feasible".to_string(),
            ]
        } else {
            params.target_statuses
        };

        // Clamp timeout to MAX_INTERNAL_TIMEOUT_SECS to stay under Codex CLI's 120s limit
        let effective_timeout_secs = params.timeout_seconds.min(MAX_INTERNAL_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(effective_timeout_secs);
        let interval = std::time::Duration::from_secs(params.poll_interval_seconds);
        let start = std::time::Instant::now();

        loop {
            // Check status
            let response = self
                .api_get(&format!("/api/control/missions/{}", id))
                .await?;

            if !response.status().is_success() {
                return Err(format!("Worker mission not found: {}", response.status()));
            }

            let mission: Value = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse response: {}", e))?;

            let status = mission["status"].as_str().unwrap_or("");
            if target_statuses.iter().any(|s| s == status) {
                return Ok(json!({
                    "reached_target": true,
                    "internal_timeout": false,
                    "status": status,
                    "elapsed_seconds": start.elapsed().as_secs(),
                    "mission": mission,
                }));
            }

            // Check timeout
            if start.elapsed() > timeout {
                let was_clamped = params.timeout_seconds > MAX_INTERNAL_TIMEOUT_SECS;
                return Ok(json!({
                    "reached_target": false,
                    "internal_timeout": was_clamped,
                    "timeout": true,
                    "status": status,
                    "elapsed_seconds": start.elapsed().as_secs(),
                    "effective_timeout_seconds": effective_timeout_secs,
                    "requested_timeout_seconds": params.timeout_seconds,
                    "mission": mission,
                    "hint": "Worker has not reached a target status yet. Call wait_for_worker again to continue waiting.",
                }));
            }

            tokio::time::sleep(interval).await;
        }
    }

    async fn handle_call(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "get_workspace_layout" => Ok(self.get_workspace_layout()),
            "get_backend_auth_status" => {
                let params: BackendAuthStatusParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                Ok(self.get_backend_auth_status(params))
            }
            "create_worker_mission" => {
                let params: CreateWorkerParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.create_worker(params).await
            }
            "batch_create_workers" => {
                let params: BatchCreateWorkersParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.batch_create_workers(params).await
            }
            "list_worker_missions" => self.list_workers().await,
            "get_worker_status" => {
                let params: GetWorkerStatusParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.get_worker_status(params).await
            }
            "cancel_worker" => {
                let params: CancelWorkerParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.cancel_worker(params).await
            }
            "cancel_all_workers" => self.cancel_all_workers().await,
            "send_message_to_worker" => {
                let params: SendMessageParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.send_message(params).await
            }
            "resume_worker" => {
                let params: SendMessageParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.resume_worker(params).await
            }
            "retask_worker" => {
                let params: SendMessageParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.retask_worker(params).await
            }
            "create_worktree" => {
                let params: CreateWorktreeParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.create_worktree(params)
            }
            "remove_worktree" => {
                let params: RemoveWorktreeParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.remove_worktree(params)
            }
            "wait_for_worker" => {
                let params: WaitForWorkerParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.wait_for_worker(params).await
            }
            "wait_for_any_worker" => {
                let params: WaitForAnyWorkerParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.wait_for_any_worker(params).await
            }
            _ => Err(format!("Unknown method: {}", method)),
        }
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => {
                let info = ServerInfo {
                    name: "orchestrator".to_string(),
                    version: "0.1.0".to_string(),
                };
                JsonRpcResponse::success(
                    req.id,
                    json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": info,
                        "capabilities": {
                            "tools": {}
                        }
                    }),
                )
            }
            "tools/list" => {
                let tools = Self::get_tools();
                JsonRpcResponse::success(req.id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let params = match req.params.as_object() {
                    Some(p) => p,
                    None => {
                        return JsonRpcResponse::error(req.id, -32602, "Invalid params");
                    }
                };
                let method = match params.get("name").and_then(|n| n.as_str()) {
                    Some(m) => m,
                    None => {
                        return JsonRpcResponse::error(req.id, -32602, "Missing tool name");
                    }
                };
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

                match self.handle_call(method, arguments).await {
                    Ok(result) => JsonRpcResponse::success(
                        req.id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&result).unwrap()
                            }]
                        }),
                    ),
                    Err(e) => JsonRpcResponse::error(req.id, -32000, e),
                }
            }
            "notifications/initialized" => {
                // Notification, no response needed but we return empty for safety
                JsonRpcResponse::success(req.id, json!(null))
            }
            _ => JsonRpcResponse::error(req.id, -32601, format!("Unknown method: {}", req.method)),
        }
    }
}

/// Hard cap on how long any wait_for_* poll loop may run, to stay well under
/// Codex CLI's 120-second MCP tool-call timeout.
const MAX_INTERNAL_TIMEOUT_SECS: u64 = 90;

/// Try to locate the git repository root that worktree commands should target.
///
/// Resolution order:
/// 1. If `explicit` is `Some`, use it directly.
/// 2. Walk the workspace root looking for `.git` entries up to 2 levels deep.
///    If exactly one repo is found, use it.
/// 3. Fall back to the workspace root (original behaviour).
fn resolve_repo_path(explicit: Option<&str>) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }

    let workspace_root = std::env::var("WORKING_DIR")
        .or_else(|_| std::env::var("SANDBOXED_SH_WORKSPACE_ROOT"))
        .unwrap_or_else(|_| ".".to_string());

    // Search up to 2 levels deep for .git dirs/files
    let mut repos: Vec<String> = Vec::new();

    // Level 0: workspace root itself
    let root_path = std::path::Path::new(&workspace_root);
    if root_path.join(".git").exists() {
        return workspace_root;
    }

    // Level 1
    if let Ok(entries) = std::fs::read_dir(root_path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if !child.is_dir() {
                continue;
            }
            if child.join(".git").exists() {
                if let Some(s) = child.to_str() {
                    repos.push(s.to_string());
                }
                continue;
            }
            // Level 2
            if let Ok(sub_entries) = std::fs::read_dir(&child) {
                for sub_entry in sub_entries.flatten() {
                    let grandchild = sub_entry.path();
                    if grandchild.is_dir() && grandchild.join(".git").exists() {
                        if let Some(s) = grandchild.to_str() {
                            repos.push(s.to_string());
                        }
                    }
                }
            }
        }
    }

    if repos.len() == 1 {
        return repos.into_iter().next().unwrap();
    }

    // Multiple or zero repos found – fall back to workspace root
    workspace_root
}

// =============================================================================
// Container resource estimation
// =============================================================================

/// Per-worker resource estimates. Each Claude Code / Codex worker spawns
/// node.js + bun + the CLI itself + child processes (LSP, git, etc.).
const PIDS_PER_WORKER: u64 = 200;
const MEMORY_PER_WORKER_MB: u64 = 800;
/// Headroom reserved for the boss process, system daemons, and the orchestrator MCP.
const PIDS_HEADROOM: u64 = 300;
const MEMORY_HEADROOM_MB: u64 = 1024;
/// Absolute cap even if resources appear plentiful (avoids thrashing).
const ABSOLUTE_MAX_WORKERS: usize = 4;

struct ResourceCap {
    max_workers: usize,
    reason: String,
    pids_available: Option<u64>,
    pids_max: Option<u64>,
    memory_available_mb: Option<u64>,
    memory_max_mb: Option<u64>,
}

/// Read a cgroup v2 file and return its value (or None if unreadable / "max").
fn read_cgroup_u64(path: &str) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed == "max" {
        None // unlimited
    } else {
        trimmed.parse::<u64>().ok()
    }
}

/// Estimate how many concurrent workers this container can support
/// by reading cgroup v2 PID and memory limits.
fn estimate_max_workers() -> ResourceCap {
    let pids_max = read_cgroup_u64("/sys/fs/cgroup/pids.max");
    let pids_current = read_cgroup_u64("/sys/fs/cgroup/pids.current").unwrap_or(0);
    let mem_max = read_cgroup_u64("/sys/fs/cgroup/memory.max");
    let mem_current = read_cgroup_u64("/sys/fs/cgroup/memory.current").unwrap_or(0);

    let mem_current_mb = mem_current / (1024 * 1024);

    // PID-based cap
    let pid_cap = if let Some(max) = pids_max {
        let available = max
            .saturating_sub(pids_current)
            .saturating_sub(PIDS_HEADROOM);
        Some(available / PIDS_PER_WORKER)
    } else {
        None
    };

    // Memory-based cap
    let mem_cap = if let Some(max) = mem_max {
        let max_mb = max / (1024 * 1024);
        let available_mb = max_mb
            .saturating_sub(mem_current_mb)
            .saturating_sub(MEMORY_HEADROOM_MB);
        Some(available_mb / MEMORY_PER_WORKER_MB)
    } else {
        None
    };

    // Take the minimum of all caps
    let mut effective = ABSOLUTE_MAX_WORKERS as u64;
    let mut reasons = Vec::new();

    if let Some(pc) = pid_cap {
        if pc < effective {
            effective = pc;
        }
        reasons.push(format!(
            "PIDs: {}/{} used, ~{} available for workers",
            pids_current,
            pids_max.unwrap_or(0),
            pc
        ));
    }
    if let Some(mc) = mem_cap {
        if mc < effective {
            effective = mc;
        }
        reasons.push(format!(
            "Memory: {}MB/{}MB used, ~{} workers fit",
            mem_current_mb,
            mem_max.map(|m| m / (1024 * 1024)).unwrap_or(0),
            mc
        ));
    }
    if reasons.is_empty() {
        reasons.push("No cgroup limits detected, using absolute cap".to_string());
    }

    // Allow 0 when resources are exhausted — spawning even one worker
    // can tip the container into OOM / PID-exhaustion.
    let max_workers = (effective as usize).min(ABSOLUTE_MAX_WORKERS);

    ResourceCap {
        max_workers,
        reason: reasons.join("; "),
        pids_available: pid_cap.map(|p| p * PIDS_PER_WORKER),
        pids_max,
        memory_available_mb: mem_cap.map(|m| m * MEMORY_PER_WORKER_MB),
        memory_max_mb: mem_max.map(|m| m / (1024 * 1024)),
    }
}

fn find_git_root(path: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(root)
    }
}

fn openagent_data_dir() -> std::path::PathBuf {
    std::env::var("OPENAGENT_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/root/.sandboxed-sh"))
}

fn opencode_auth_json_path() -> std::path::PathBuf {
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        return std::path::PathBuf::from(data_home)
            .join("opencode")
            .join("auth.json");
    }

    std::path::PathBuf::from("/var/lib/opencode/.local/share/opencode/auth.json")
}

fn codex_auth_json_path() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let candidate = std::path::PathBuf::from(&home)
            .join(".codex")
            .join("auth.json");
        if candidate.exists() {
            return candidate;
        }
    }

    std::path::PathBuf::from("/var/lib/opencode/.codex/auth.json")
}

fn looks_like_json_file(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .is_some()
}

fn opencode_auth_has_provider(path: &std::path::Path, provider: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        .and_then(|value| value.get(provider).cloned())
        .is_some()
}

fn backend_auth_entry(
    backend: &str,
    provider_type: ProviderType,
    data_dir: &std::path::Path,
    targeted: bool,
    has_credentials: bool,
    has_cli_auth: bool,
    extra: Option<Value>,
) -> Value {
    let ready = targeted && (has_credentials || has_cli_auth);
    let reason = if ready {
        "backend is targeted and credentials are available"
    } else if has_credentials || has_cli_auth {
        "credentials exist, but this provider is not targeted to that backend"
    } else if targeted {
        "backend is targeted, but no credentials were found"
    } else {
        "backend is not targeted and no credentials were found"
    };

    let mut value = json!({
        "backend": backend,
        "provider": provider_type.id(),
        "ready": ready,
        "targeted": targeted,
        "has_credentials": has_credentials,
        "has_cli_auth": has_cli_auth,
        "default_backends": default_backends_for_provider(provider_type),
        "data_dir": data_dir,
        "reason": reason,
    });

    if let Some(extra) = extra {
        if let (Some(map), Some(extra_map)) = (value.as_object_mut(), extra.as_object()) {
            for (key, entry) in extra_map {
                map.insert(key.clone(), entry.clone());
            }
        }
    }

    value
}

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() {
    let mission_id = std::env::var("MISSION_ID")
        .or_else(|_| std::env::var("SANDBOXED_SH_MISSION_ID"))
        .ok()
        .and_then(|id| Uuid::parse_str(&id).ok())
        .expect("MISSION_ID environment variable not set or invalid");

    let api_url = std::env::var("API_URL")
        .or_else(|_| std::env::var("SANDBOXED_SH_API_URL"))
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_token = std::env::var("API_TOKEN")
        .or_else(|_| std::env::var("SANDBOXED_SH_API_TOKEN"))
        .ok()
        .or_else(|| {
            // Mint a service JWT from the shared secret when no explicit token is set.
            std::env::var("JWT_SECRET")
                .ok()
                .and_then(|s| mint_service_jwt(&s))
        });

    let server = Arc::new(OrchestratorMcp::new(mission_id, api_url, api_token));

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let error_resp =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {}", e));
                if let Ok(json) = serde_json::to_string(&error_resp) {
                    writeln!(stdout, "{}", json).ok();
                }
                stdout.flush().ok();
                continue;
            }
        };

        // Skip notifications (id is null)
        if request.id.is_null() && request.method.starts_with("notifications/") {
            continue;
        }

        let response = server.handle_request(request).await;
        if let Ok(json) = serde_json::to_string(&response) {
            writeln!(stdout, "{}", json).ok();
        }
        stdout.flush().ok();
    }
}
