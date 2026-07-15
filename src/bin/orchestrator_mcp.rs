//! MCP Server for orchestrating parallel agent missions.
//!
//! Provides boss agents with tools to create, monitor, and manage worker missions.
//! Communicates over stdio using JSON-RPC 2.0.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::Command;
use std::sync::Arc;

use tokio::sync::OnceCell;

use chrono::Utc;
use jsonwebtoken::{EncodingKey, Header};
use sandboxed_sh::ai_providers::ProviderType;
use sandboxed_sh::api::ai_providers::{
    default_backends_for_provider, get_all_openai_oauth_accounts,
    get_openai_api_key_for_codex_default, provider_targets_backend, read_oauth_token_entry,
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
    /// Backend to use: "claudecode", "codex", "gemini", "opencode"
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
    /// Workspace to spawn the worker in. If omitted, the worker inherits the
    /// boss mission's workspace so it sees the same container, mounts, and
    /// installed tooling. Pass `"00000000-0000-0000-0000-000000000000"` to
    /// explicitly target the host workspace.
    #[serde(default)]
    workspace_id: Option<String>,
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

/// Snapshot of the worker's history taken when an `ask_worker` question is
/// sent. Returned to the caller on timeout so a continuation re-call can keep
/// waiting for the *same* answer without re-sending the question.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AskWorkerBaseline {
    /// Number of ASSISTANT entries in history at send time. Total history
    /// length is not usable: the question itself appends a user entry, which
    /// would read as "new answer" while the old assistant reply is still the
    /// latest one.
    assistant_count: usize,
    #[serde(default)]
    last_assistant: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AskWorkerParams {
    /// UUID of the (advisor) worker mission to ask
    mission_id: String,
    /// The question to send (ignored on continuation re-calls)
    #[serde(default)]
    question: String,
    /// Maximum seconds to wait for the answer (default 600; internally
    /// clamped per call — re-call with the returned baseline to keep waiting)
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// Continuation token from a previous timed-out ask_worker call
    #[serde(default)]
    baseline: Option<AskWorkerBaseline>,
}

#[derive(Debug, Deserialize)]
struct BackendAuthStatusParams {
    #[serde(default)]
    backend: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DurableJobStartParams {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct DurableJobIdParams {
    job_id: String,
}

#[derive(Debug, Deserialize)]
struct DurableJobLogsParams {
    job_id: String,
    #[serde(default)]
    tail_bytes: Option<usize>,
    #[serde(default)]
    stream: Option<String>,
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
struct PlanWorktreeSpec {
    /// Absolute path where the worktree will be created
    path: String,
    /// Branch name (created from `base` if it doesn't exist)
    branch: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    repo_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlanTaskSpec {
    /// Stable key unique on this board; used in depends_on and digests
    task_key: String,
    title: String,
    /// Full worker prompt: scope, absolute paths, success condition,
    /// verification command. The done/BLOCKED turn contract is appended
    /// automatically by the scheduler.
    prompt: String,
    /// Worker backend (codex, opencode, grok, ...). Never claudecode.
    backend: String,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    model_effort: Option<String>,
    /// Working directory for the worker. Overridden by `worktree.path` when a
    /// worktree spec is given.
    #[serde(default)]
    working_directory: Option<String>,
    /// Task keys (this board) that must settle successfully or be accepted first
    #[serde(default)]
    depends_on: Vec<String>,
    /// Create an isolated git worktree for the worker before registering
    #[serde(default)]
    worktree: Option<PlanWorktreeSpec>,
}

#[derive(Debug, Deserialize)]
struct PlanTasksParams {
    tasks: Vec<PlanTaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskRefParams {
    /// Task key on this board (preferred)
    #[serde(default)]
    task_key: Option<String>,
    /// Task UUID (alternative to task_key)
    #[serde(default)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RejectTaskParams {
    #[serde(default)]
    task_key: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    /// Feedback delivered to the worker (what's wrong, what to change)
    feedback: String,
}

fn default_review_history() -> usize {
    6
}

#[derive(Debug, Deserialize)]
struct ReviewTaskParams {
    #[serde(default)]
    task_key: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    /// How many history entries from the worker mission to include
    #[serde(default = "default_review_history")]
    history_limit: usize,
}

#[derive(Debug, Deserialize)]
struct MergeBranchParams {
    /// Branch to merge from (a worker's worktree branch)
    source_branch: String,
    /// Branch to merge into (e.g. the PR branch)
    target_branch: String,
    #[serde(default)]
    repo_path: Option<String>,
    /// Push the target branch to origin after a clean merge
    #[serde(default)]
    push: bool,
    /// Delete the source branch after a clean merge
    #[serde(default)]
    delete_source: bool,
    /// Backend for the auto-registered conflict-resolver task (default codex)
    #[serde(default)]
    resolver_backend: Option<String>,
    /// Model for the resolver task (default gpt-5.6-sol)
    #[serde(default)]
    resolver_model: Option<String>,
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

#[derive(Deserialize, Default)]
struct DeploySandboxedShParams {
    /// Optional explicit Sandboxed.sh environment to deploy. When omitted, the
    /// tool deploys the API environment this mission is connected to.
    #[serde(default)]
    target_environment: Option<String>,
    /// Bypass self-protection + debounce safety rails.
    #[serde(default)]
    force: bool,
    /// Optional git ref to check out before building.
    #[serde(default)]
    git_ref: Option<String>,
    /// Skip cargo build (binaries assumed current at <repo_path>/target/debug/).
    #[serde(default)]
    skip_build: bool,
    /// Override the server's configured source repo. Required when the
    /// build artifact lives outside the default location (e.g. an ad-hoc
    /// worktree at /opt/sandboxed-sh-<name>/).
    #[serde(default)]
    repo_path: Option<String>,
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
///
/// When `BOSS_USER_ID` is set (forwarded by workspace prep), the token is
/// minted as that user so worker missions created via this MCP land in the
/// boss's per-user mission store. Without it, the SingleTenant auth path
/// would create a synthetic `orchestrator-mcp` user and shard workers into
/// `missions-orchestrator-mcp.db`, where the dashboard can't see them.
fn mint_service_jwt(secret: &str) -> Option<String> {
    let now = Utc::now();
    let exp = now + chrono::Duration::hours(24);
    let (sub, usr) = match std::env::var("BOSS_USER_ID") {
        Ok(id) if !id.trim().is_empty() => (id.clone(), id),
        _ => (
            "orchestrator-mcp".to_string(),
            "orchestrator-mcp".to_string(),
        ),
    };
    let claims = JwtClaims {
        sub,
        usr,
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
    // Cached lookup of the boss mission's workspace_id, so workers default to the
    // same workspace instead of silently falling back to the host (Uuid::nil).
    boss_workspace_id: OnceCell<Option<String>>,
    // Fresh ask_worker questions sent per worker mission this MCP lifetime.
    // Cost guard: a cheap executor looping questions at an expensive advisor
    // is the failure mode this caps. Continuation re-calls don't count.
    ask_counts: tokio::sync::Mutex<HashMap<Uuid, u32>>,
}

/// Max fresh ask_worker questions per worker for one MCP process (≈ one
/// mission). High enough for real consultations, low enough to stop loops.
const MAX_ASKS_PER_WORKER: u32 = 20;

struct DeployTarget {
    api_url: &'static str,
    expected_service: Option<&'static str>,
}

fn resolve_deploy_target(target_environment: Option<&str>) -> Result<DeployTarget, String> {
    match target_environment {
        None => Ok(DeployTarget {
            api_url: "",
            expected_service: None,
        }),
        Some("dev") => Ok(DeployTarget {
            api_url: "http://127.0.0.1:3002",
            expected_service: Some("sandboxed-sh-dev.service"),
        }),
        Some("prod") => Ok(DeployTarget {
            api_url: "http://127.0.0.1:3000",
            expected_service: Some("sandboxed-sh-prod.service"),
        }),
        Some(other) => Err(format!(
            "Invalid target_environment {:?}; expected \"dev\" or \"prod\"",
            other
        )),
    }
}

impl OrchestratorMcp {
    fn new(mission_id: Uuid, api_url: String, api_token: Option<String>) -> Self {
        Self {
            mission_id,
            api_url,
            api_token,
            client: reqwest::Client::new(),
            boss_workspace_id: OnceCell::new(),
            ask_counts: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the workspace_id of the boss mission, or `None` if the lookup
    /// fails (e.g. legacy mission without workspace_id). Caches the result for
    /// the lifetime of the MCP process so we don't re-query on every worker.
    async fn boss_workspace_id(&self) -> Option<String> {
        self.boss_workspace_id
            .get_or_init(|| async {
                let response = self
                    .api_get(&format!("/api/control/missions/{}", self.mission_id))
                    .await
                    .ok()?;
                if !response.status().is_success() {
                    return None;
                }
                let mission: Value = response.json().await.ok()?;
                mission
                    .get("workspace_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .await
            .clone()
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
                            "enum": ["claudecode", "codex", "gemini", "opencode", "grok"],
                            "description": "Optional single backend to inspect. If omitted, returns all common backends."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "durable_job_start".to_string(),
                description: "REQUIRED for any command that may outlive one tool call (long builds, benchmark arms, test suites, image builds). It runs inside the caller's workspace with its CPU/RAM scope, durable status, and file-backed logs. Never use shell `&`, nohup, or setsid for such work: those descendants remain owned by the ephemeral harness scope and are killed when the turn disconnects.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["command"],
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run."},
                        "cwd": {"type": "string", "description": "Working directory inside the caller's workspace. Relative paths resolve from the workspace root."},
                        "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Environment variables to add."}
                    }
                }),
            },
            ToolDefinition {
                name: "durable_job_list".to_string(),
                description: "List durable background jobs with current status, pid, command, cwd, and log paths.".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
            },
            ToolDefinition {
                name: "durable_job_status".to_string(),
                description: "Get current status for a durable background job.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {"job_id": {"type": "string", "description": "Durable job UUID."}}
                }),
            },
            ToolDefinition {
                name: "durable_job_logs".to_string(),
                description: "Read the tail of stdout and/or stderr logs for a durable background job.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {
                        "job_id": {"type": "string", "description": "Durable job UUID."},
                        "tail_bytes": {"type": "integer", "description": "Maximum bytes to return from each selected stream. Defaults to 16384."},
                        "stream": {"type": "string", "enum": ["stdout", "stderr"], "description": "Optional single stream to return. Omit for both."}
                    }
                }),
            },
            ToolDefinition {
                name: "durable_job_cancel".to_string(),
                description: "Cancel a running durable background job by terminating its process group.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["job_id"],
                    "properties": {"job_id": {"type": "string", "description": "Durable job UUID."}}
                }),
            },
            ToolDefinition {
                name: "plan_tasks".to_string(),
                description: "PREFERRED orchestration tool. Register tasks on this mission's task board. The server-side scheduler then owns everything mechanical: it spawns a worker mission for every dependency-satisfied task while capacity allows, retries failures once, and MESSAGES YOU A DIGEST as each task settles. You never create workers, wait, or poll. Each task needs: task_key (stable, unique), title, full prompt (scope, absolute paths, success condition, verification command — the done/BLOCKED turn contract is appended automatically), backend (codex/opencode/grok — never claudecode), and optionally model_override/model_effort, depends_on (keys of tasks that must settle successfully first), and worktree {path, branch, base} for an isolated checkout (created here, before registration). Re-calling with an existing key updates the task while it is still pending. After planning: END YOUR TURN — digests will wake you.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["tasks"],
                    "properties": {
                        "tasks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["task_key", "title", "prompt", "backend"],
                                "properties": {
                                    "task_key": { "type": "string", "description": "Stable unique key on this board (e.g. 'p3-nonce')" },
                                    "title": { "type": "string" },
                                    "prompt": { "type": "string", "description": "Full worker prompt: exact scope, absolute paths, success condition, verification command" },
                                    "backend": { "type": "string", "description": "codex | opencode | grok (never claudecode)" },
                                    "model_override": { "type": "string", "description": "Exact account-supported model ID. For Codex Terra use gpt-5.6-terra with medium effort. Never invent variants such as gpt-5.5-sol." },
                                    "model_effort": { "type": "string", "description": "low | medium | high | xhigh | max (codex)" },
                                    "working_directory": { "type": "string", "description": "Worker cwd; superseded by worktree.path if a worktree is given" },
                                    "depends_on": { "type": "array", "items": { "type": "string" }, "description": "task_keys that must settle successfully or be accepted first" },
                                    "worktree": {
                                        "type": "object",
                                        "required": ["path", "branch"],
                                        "properties": {
                                            "path": { "type": "string", "description": "Absolute worktree path (inside the workspace)" },
                                            "branch": { "type": "string" },
                                            "base": { "type": "string", "description": "Base branch/commit (default HEAD)" },
                                            "repo_path": { "type": "string", "description": "Repo owning the worktree (auto-detected if omitted)" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "board_status".to_string(),
                description: "Current task board: every task with status (pending/running/settled/accepted/failed/cancelled), outcome, worker mission id, digest, plus utilization counters. Cheap snapshot — use instead of list_worker_missions/get_worker_status for board work.".to_string(),
                input_schema: json!({ "type": "object", "properties": {} }),
            },
            ToolDefinition {
                name: "review_task".to_string(),
                description: "Full review payload for a board task: the task record (prompt, digest, notes) plus the tail of the worker mission's conversation. Use before accept_task/reject_task when the digest alone isn't enough to judge.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task_key": { "type": "string", "description": "Task key on this board (preferred)" },
                        "task_id": { "type": "string", "description": "Task UUID (alternative)" },
                        "history_limit": { "type": "integer", "description": "Worker history entries to include (default 6)" }
                    }
                }),
            },
            ToolDefinition {
                name: "accept_task".to_string(),
                description: "Accept a settled task's result (terminal verdict). Unblocks nothing extra — successful settles already unblock dependents — but records your judgment and clears the awaiting-verdict queue.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task_key": { "type": "string" },
                        "task_id": { "type": "string" }
                    }
                }),
            },
            ToolDefinition {
                name: "reject_task".to_string(),
                description: "Reject a settled (or failed) task with feedback. The worker mission is resumed with your feedback so it keeps its context; if the worker is gone the task is re-queued with the feedback appended to its prompt. You'll get a fresh digest when it settles again.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["feedback"],
                    "properties": {
                        "task_key": { "type": "string" },
                        "task_id": { "type": "string" },
                        "feedback": { "type": "string", "description": "What is wrong and what to change — delivered verbatim to the worker" }
                    }
                }),
            },
            ToolDefinition {
                name: "merge_branch".to_string(),
                description: "Merge a worker branch into a target branch (e.g. worker worktree branch → PR branch). Clean merge: optionally push and delete the source branch. Conflict: the merge is aborted safely and a conflict-resolver task is registered on the board automatically — you'll get a digest when it settles. This makes splitting one PR across several worktree workers routine.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["source_branch", "target_branch"],
                    "properties": {
                        "source_branch": { "type": "string" },
                        "target_branch": { "type": "string" },
                        "repo_path": { "type": "string", "description": "Repo directory (auto-detected if omitted). Must be checked out on target_branch or switchable to it." },
                        "push": { "type": "boolean", "description": "Push target to origin after a clean merge" },
                        "delete_source": { "type": "boolean", "description": "Delete the source branch after a clean merge" },
                        "resolver_backend": { "type": "string", "description": "Backend for the auto conflict-resolver task (default codex)" },
                        "resolver_model": { "type": "string", "description": "Model for the resolver task (default gpt-5.6-sol)" }
                    }
                }),
            },
            ToolDefinition {
                name: "create_worker_mission".to_string(),
                description: "LEGACY — prefer plan_tasks, which schedules, retries, and notifies automatically. Create a new worker mission (child of the current boss mission). The worker will start executing immediately and runs in the same workspace as the boss by default, so it sees the boss's container, mounts, and installed tooling. IMPORTANT: You must set the 'backend' field to match the harness you want (claudecode, codex, gemini, grok, opencode). If omitted, defaults to the workspace default (usually claudecode).".to_string(),
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
                            "enum": ["claudecode", "codex", "gemini", "opencode", "grok"],
                            "description": "Backend/harness to use. MUST match the model: claudecode for Claude models, codex for OpenAI/GPT models, gemini for Gemini models, grok for Grok models, opencode for any model via provider routing."
                        },
                        "model_override": {
                            "type": "string",
                            "description": "Exact account-supported model ID. Must match the backend: Claude models (e.g. 'claude-opus-4-7') for claudecode, GPT models (e.g. 'gpt-5.6-terra', recommended with medium effort) for codex, Gemini models for gemini, Grok models for grok, 'provider/model' format for opencode. Never invent variants such as 'gpt-5.5-sol'."
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
                        "workspace_id": {
                            "type": "string",
                            "description": "Workspace UUID to spawn the worker in. Defaults to the boss's workspace so the worker inherits the same container, mounts, and installed tooling. Pass the nil UUID to force the host workspace."
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
                                    "backend": { "type": "string", "enum": ["claudecode", "codex", "gemini", "opencode", "grok"] },
                                    "model_override": { "type": "string", "description": "Exact account-supported model ID. For Codex Terra use gpt-5.6-terra with medium effort; gpt-5.5-sol is unsupported." },
                                    "model_effort": { "type": "string", "enum": ["low", "medium", "high", "xhigh", "max"] },
                                    "agent": { "type": "string" },
                                    "config_profile": { "type": "string" },
                                    "working_directory": { "type": "string" },
                                    "workspace_id": { "type": "string" },
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
                name: "ask_worker".to_string(),
                description: "Ask a worker mission a question and wait for its answer in one \
                    call (send + wait + extract). Designed for consulting a persistent advisor \
                    worker: the worker keeps its session context between questions, so it does \
                    not re-read the repository each time. Returns {answered: true, answer} on \
                    success. If the result has answer_pending: true, the worker is still \
                    thinking — call ask_worker again with the SAME mission_id, an empty \
                    question, and the returned `baseline` object exactly as given (this \
                    continues waiting; it does not re-send the question). Include in every \
                    question: what you tried, the exact error, and relevant file paths."
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "required": ["mission_id"],
                    "properties": {
                        "mission_id": {
                            "type": "string",
                            "description": "UUID of the worker mission to ask"
                        },
                        "question": {
                            "type": "string",
                            "description": "The question to send. Required on a fresh ask; leave empty when passing `baseline` to continue waiting."
                        },
                        "timeout_seconds": {
                            "type": "number",
                            "description": "Max seconds to wait for the answer (default 600; long waits are split into multiple calls via the baseline mechanism)"
                        },
                        "baseline": {
                            "type": "object",
                            "description": "Continuation token returned by a previous ask_worker call that ended with answer_pending: true. Pass it back unchanged."
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
                description: "Block until a single worker mission reaches a settled status (including awaiting_user, the normal end-of-turn state). Prefer wait_for_any_worker to monitor multiple workers simultaneously — blocking on one worker while others sit idle serializes your pipeline. For waits beyond a few minutes, end your turn and schedule a wakeup instead of polling (each ~90s poll costs a model call).".to_string(),
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
                            "description": "Statuses to wait for (default: ['completed', 'awaiting_user', 'failed', 'interrupted', 'not_feasible'])"
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
                description: "Block until ANY of the specified worker missions reaches a settled status (including awaiting_user, the normal end-of-turn state). Returns the first worker that settles. Use this to monitor a pool of workers and react as each completes. PARALLELISM: when this returns, re-dispatch ALL idle workers (resume_worker / create_worker_mission for every independent track) BEFORE doing any deep analysis of the returned result — workers sitting in awaiting_user are wasted capacity while you read logs. TOKEN ECONOMY: the wait is capped at ~90s per call, so polling a long-running worker burns one of YOUR model calls per 90s. If no worker is likely to finish within a couple of polls, END YOUR TURN and schedule a wakeup (ScheduleWakeup, 10-30 min) instead — check workers once per wakeup.".to_string(),
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
                            "description": "Statuses to wait for (default: ['completed', 'awaiting_user', 'failed', 'interrupted', 'not_feasible'])"
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
                name: "deploy_sandboxed_sh".to_string(),
                description:
                    "Build and hot-swap one Sandboxed.sh backend with safety rails.\n\
                     Targeting is explicit:\n\
                     • Omit target_environment to deploy the backend API this mission is connected to.\n\
                     • Set target_environment=\"dev\" to deploy sandboxed-sh-dev via localhost:3002.\n\
                     • Set target_environment=\"prod\" to deploy sandboxed-sh-prod via localhost:3000.\n\
                     The backend refuses if the request reaches a service different from the \
                     requested target.\n\
                     • Self-protection: refuses by default if your own mission lives on the \
                       service being restarted (passing force=true acknowledges that your turn \
                       will be SIGTERM'd).\n\
                     • Debounce: refuses if another deploy fired in the last few minutes \
                       (force=true overrides).\n\
                     • Atomic install with .pre-deploy-<sha> backups + detached restart so the \
                       SSE response flushes before the service dies. If the orchestrator-mcp \
                       install fails, the main binary swap is rolled back so the system is \
                       never in a half-applied state.\n\
                     Replaces calling `systemctl restart` directly from a shell. Returns the \
                     deployed commit sha on success."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "target_environment": {
                            "type": "string",
                            "enum": ["dev", "prod"],
                            "description": "Explicit environment to deploy. Omit to deploy the backend API this mission is connected to. Use dev for sandboxed-sh-dev on localhost:3002 and prod for sandboxed-sh-prod on localhost:3000."
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Bypass self-protection AND debounce. Only set this when you've explicitly decided the restart is worth killing your own turn / breaking the cooldown.",
                            "default": false
                        },
                        "git_ref": {
                            "type": "string",
                            "description": "Optional git ref (tag/branch/sha) to check out before building. Omit to deploy whatever the local repo currently has checked out."
                        },
                        "skip_build": {
                            "type": "boolean",
                            "description": "Skip the cargo build step and assume <repo_path>/target/debug/sandboxed-sh and orchestrator-mcp are already current. Useful when the build was done elsewhere.",
                            "default": false
                        },
                        "repo_path": {
                            "type": "string",
                            "description": "Override the server's configured source repo path. Required when your build artifact lives outside the default location (e.g. a worktree at /opt/sandboxed-sh-<name>/)."
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
        self.api_post_to(&self.api_url, path, body).await
    }

    async fn api_post_to(
        &self,
        api_url: &str,
        path: &str,
        body: Value,
    ) -> Result<reqwest::Response, String> {
        let url = format!("{}{}", api_url, path);
        let mut req = self.client.post(&url).json(&body);
        if let Some((k, v)) = self.auth_header() {
            req = req.header(k, v);
        }
        req.send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))
    }

    async fn create_worker(&self, params: CreateWorkerParams) -> Result<Value, String> {
        // Worker model allowlist: Claude models are reserved for the boss and
        // the oracle; spending them on workers defeats the budget the policy
        // protects. Prompt-level policy proved insufficient (two violations in
        // one campaign), so this is enforced here. Override with
        // SANDBOXED_SH_ALLOW_CLAUDE_WORKERS=1 when the operator explicitly
        // approves a Claude worker.
        let claude_worker = params.backend.as_deref() == Some("claudecode")
            || params
                .model_override
                .as_deref()
                .map(|m| m.to_ascii_lowercase().contains("claude"))
                .unwrap_or(false);
        let claude_allowed = std::env::var("SANDBOXED_SH_ALLOW_CLAUDE_WORKERS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        // Omitted backend/model would fall through to the server-side default
        // (which may be a Claude backend) — require an explicit choice so the
        // policy cannot be bypassed by omission.
        if !claude_allowed && params.backend.as_deref().unwrap_or("").trim().is_empty() {
            return Err(
                "Refused: specify an explicit non-Claude `backend` (and matching                  `model_override`) for worker missions — omitting it falls back to                  the server default, which may be a Claude backend (operator policy)."
                    .to_string(),
            );
        }
        if claude_worker && !claude_allowed {
            return Err(
                "Refused: Claude models/backends are not allowed for worker missions                  (operator policy). Use opencode+builtin/smart, opencode+genius,                  codex+gpt-5.6-sol, or grok instead. If the operator explicitly approved a                  Claude worker, set SANDBOXED_SH_ALLOW_CLAUDE_WORKERS=1."
                    .to_string(),
            );
        }

        // Fail fast if working_directory points outside the boss's container
        // workspace mount: the worker container will not be able to see paths
        // outside the bind-mounted workspace root, so accepting the request
        // would silently produce a worker that fails on first cd/Read.
        if let Some(wd) = params.working_directory.as_deref() {
            validate_working_directory_visible_to_worker(wd)?;
        }

        // If the caller didn't specify a workspace, inherit the boss's so the
        // worker runs in the same container the boss is in. Falling back to the
        // host workspace (Uuid::nil) was surprising: the boss's installed
        // tooling/symlinks aren't visible there, and a stale host environment
        // could leave the worker unable to spawn its CLI.
        let workspace_id = match params.workspace_id {
            Some(ref id) if !id.trim().is_empty() => Some(id.trim().to_string()),
            _ => self.boss_workspace_id().await,
        };

        let body = json!({
            "title": params.title,
            "agent": params.agent,
            "backend": params.backend,
            "model_override": params.model_override,
            "model_effort": params.model_effort,
            "config_profile": params.config_profile,
            "parent_mission_id": self.mission_id.to_string(),
            "working_directory": params.working_directory,
            "workspace_id": workspace_id,
            // Atomic create+start: the API stores the prompt as the mission's
            // deferred goal and the scheduler dispatches it once capacity
            // allows. The old create-then-message pattern could be dropped at
            // capacity, leaving a zombie Pending worker the boss believed was
            // running.
            "prompt": params.prompt,
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
        let mut workers: Vec<Value> = missions
            .into_iter()
            .filter(|m| m["parent_mission_id"].as_str() == Some(&boss_id))
            .collect();

        // Enrich completed workers with push verification.
        for worker in workers.iter_mut() {
            enrich_with_push_claims(worker);
        }

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

        let mut mission: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // Enrich with push verification if the worker is completed and the
        // last assistant message claims to have pushed a branch.
        enrich_with_push_claims(&mut mission);

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

    /// Synchronous question→answer round-trip with a (persistent) worker.
    ///
    /// Sends `question` as a user message, polls until a NEW assistant turn
    /// lands and the worker settles, and returns that answer. On internal
    /// timeout it returns `answer_pending: true` plus a `baseline` token —
    /// the caller re-invokes ask_worker with the same mission_id and that
    /// baseline (question is ignored) to keep waiting without re-asking.
    async fn ask_worker(&self, params: AskWorkerParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.mission_id)
            .map_err(|_| "Invalid mission ID format".to_string())?;

        let baseline = match params.baseline {
            Some(baseline) => baseline,
            None => {
                let question = params.question.trim();
                if question.is_empty() {
                    return Err(
                        "question must not be empty (pass `baseline` only to continue waiting \
                         for a previous ask)"
                            .to_string(),
                    );
                }

                // Cost guard: check the cap up front, but only charge the
                // quota after the message actually sends — a failed fetch or
                // send must not consume one of the allowed questions.
                {
                    let counts = self.ask_counts.lock().await;
                    if counts.get(&id).copied().unwrap_or(0) >= MAX_ASKS_PER_WORKER {
                        return Err(format!(
                            "ask_worker limit reached for this worker ({MAX_ASKS_PER_WORKER} \
                             questions). Consolidate remaining questions or proceed with your \
                             own judgment."
                        ));
                    }
                }

                // Snapshot history BEFORE sending so a new answer is detectable.
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
                let baseline = history_fingerprint(&mission);

                self.send_message(SendMessageParams {
                    mission_id: params.mission_id.clone(),
                    content: params.question.clone(),
                })
                .await?;
                // Question is in flight — charge the quota now.
                *self.ask_counts.lock().await.entry(id).or_insert(0) += 1;
                baseline
            }
        };

        let effective_timeout_secs = params.timeout_seconds.min(MAX_INTERNAL_TIMEOUT_SECS);
        let timeout = std::time::Duration::from_secs(effective_timeout_secs);
        let interval = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();

        loop {
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
            let status = mission["status"].as_str().unwrap_or("").to_string();

            if ask_settled(&status, &baseline, &mission) {
                return Ok(json!({
                    "answered": true,
                    "answer": last_assistant_message(&mission),
                    "status": status,
                    "elapsed_seconds": start.elapsed().as_secs(),
                }));
            }

            // Worker died without producing a new answer — surface it.
            if matches!(status.as_str(), "failed" | "interrupted" | "not_feasible") {
                return Ok(json!({
                    "answered": false,
                    "status": status,
                    "error": format!(
                        "Worker reached status '{status}' without a new answer. \
                         Inspect it with get_worker_status or recover with resume_worker."
                    ),
                }));
            }

            if start.elapsed() > timeout {
                return Ok(json!({
                    "answered": false,
                    "answer_pending": true,
                    "status": status,
                    "elapsed_seconds": start.elapsed().as_secs(),
                    "baseline": baseline,
                    "hint": "The worker is still thinking. Call ask_worker again with the \
                             same mission_id, an empty question, and this exact `baseline` \
                             object to continue waiting (the question is NOT re-sent).",
                }));
            }

            tokio::time::sleep(interval).await;
        }
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
                    "grok".to_string(),
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
                    let codex_auth = codex_auth_status(&workspace_root);
                    let targeted =
                        provider_targets_backend(&workspace_root, ProviderType::OpenAI, "codex");
                    backend_auth_entry(
                        "codex",
                        ProviderType::OpenAI,
                        &workspace_root,
                        targeted,
                        codex_auth.has_credentials(),
                        codex_auth.has_valid_auth_json,
                        Some(json!({
                            "has_api_key": codex_auth.has_api_key,
                            "has_oauth": codex_auth.has_oauth,
                            "has_oauth_account": codex_auth.has_oauth_account,
                            "has_host_auth_json": codex_auth.has_valid_auth_json,
                            "auth_json_path": codex_auth.auth_json_path,
                            "auth_json_mode": codex_auth.auth_json_mode,
                            "auth_json_candidates": codex_auth.auth_json_candidates,
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
                "grok" => json!({
                    "backend": "grok",
                    "ready": true,
                    "provider": "xAI",
                    "provider_targeted": provider_targets_backend(&workspace_root, ProviderType::Xai, "grok"),
                    "reason": "Grok Build can use a targeted xAI provider API key or the CLI's own X login cache.",
                    "default_backends": default_backends_for_provider(ProviderType::Xai),
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

    async fn durable_job_start(&self, params: DurableJobStartParams) -> Result<Value, String> {
        let cwd = params.cwd.or_else(|| {
            std::env::var("WORKING_DIR")
                .ok()
                .or_else(|| std::env::var("SANDBOXED_SH_WORKSPACE").ok())
        });
        let workspace_id = self
            .boss_workspace_id()
            .await
            .ok_or_else(|| "Mission has no resolvable workspace_id; refusing to launch a host-leaking durable job".to_string())?;
        let body = json!({
            "command": params.command,
            "cwd": cwd,
            "env": params.env,
            "started_by_mission_id": self.mission_id,
            "workspace_id": workspace_id,
        });
        let response = self.api_post("/api/durable-jobs", body).await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to start durable job: {}", text));
        }
        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse durable job response: {}", e))
    }

    async fn durable_job_list(&self) -> Result<Value, String> {
        let response = self.api_get("/api/durable-jobs").await?;
        if !response.status().is_success() {
            return Err(format!(
                "Failed to list durable jobs: {}",
                response.status()
            ));
        }
        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse durable job list: {}", e))
    }

    async fn durable_job_status(&self, params: DurableJobIdParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.job_id)
            .map_err(|_| "Invalid durable job ID format".to_string())?;
        let response = self.api_get(&format!("/api/durable-jobs/{}", id)).await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Durable job not found: {}", text));
        }
        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse durable job status: {}", e))
    }

    async fn durable_job_logs(&self, params: DurableJobLogsParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.job_id)
            .map_err(|_| "Invalid durable job ID format".to_string())?;
        let mut path = format!(
            "/api/durable-jobs/{}/logs?tail_bytes={}",
            id,
            params.tail_bytes.unwrap_or(16 * 1024)
        );
        if let Some(stream) = params.stream {
            path.push_str("&stream=");
            path.push_str(&urlencoding::encode(&stream));
        }
        let response = self.api_get(&path).await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to read durable job logs: {}", text));
        }
        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse durable job logs: {}", e))
    }

    async fn durable_job_cancel(&self, params: DurableJobIdParams) -> Result<Value, String> {
        let id = Uuid::parse_str(&params.job_id)
            .map_err(|_| "Invalid durable job ID format".to_string())?;
        let response = self
            .api_post(&format!("/api/durable-jobs/{}/cancel", id), json!({}))
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to cancel durable job: {}", text));
        }
        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse durable job cancellation: {}", e))
    }

    fn create_worktree(&self, params: CreateWorktreeParams) -> Result<Value, String> {
        let path = &params.path;
        let branch = &params.branch;
        let repo_dir = resolve_repo_path(params.repo_path.as_deref());

        // Fail fast if the worktree path is outside the workspace mount: a
        // worker container assigned this worktree as its working_directory
        // would not be able to see it.
        validate_working_directory_visible_to_worker(path)?;

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

    // ---- Task board -------------------------------------------------------

    /// Register tasks on this boss mission's board. The server-side scheduler
    /// spawns workers for ready tasks automatically — no create/wait calls.
    async fn plan_tasks(&self, params: PlanTasksParams) -> Result<Value, String> {
        if params.tasks.is_empty() {
            return Err("tasks is required".to_string());
        }

        let claude_allowed = std::env::var("SANDBOXED_SH_ALLOW_CLAUDE_WORKERS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let mut registered = Vec::with_capacity(params.tasks.len());
        let mut worktrees_created = Vec::new();
        for spec in params.tasks {
            // Same operator policy as create_worker_mission: workers never
            // burn Claude tokens.
            let is_claude = spec.backend.eq_ignore_ascii_case("claudecode")
                || spec
                    .model_override
                    .as_deref()
                    .map(|m| m.to_ascii_lowercase().contains("claude"))
                    .unwrap_or(false);
            if spec.backend.trim().is_empty() || (is_claude && !claude_allowed) {
                return Err(format!(
                    "task `{}`: specify an explicit non-Claude backend (got `{}`) — \
                     use codex, opencode, or grok (operator policy)",
                    spec.task_key, spec.backend
                ));
            }

            let working_directory = if let Some(wt) = &spec.worktree {
                self.create_worktree(CreateWorktreeParams {
                    path: wt.path.clone(),
                    branch: wt.branch.clone(),
                    base: wt.base.clone(),
                    repo_path: wt.repo_path.clone(),
                })
                .map_err(|e| format!("task `{}`: worktree failed: {}", spec.task_key, e))?;
                worktrees_created.push(json!({
                    "task_key": spec.task_key,
                    "path": wt.path,
                    "branch": wt.branch,
                }));
                Some(wt.path.clone())
            } else {
                spec.working_directory.clone()
            };
            let (repository, branch) = spec
                .worktree
                .as_ref()
                .map(|wt| {
                    let repo_path = resolve_repo_path(wt.repo_path.as_deref());
                    (
                        Some(repository_identity_for_scheduler(&repo_path)),
                        Some(wt.branch.clone()),
                    )
                })
                .unwrap_or((None, None));
            if let Some(wd) = working_directory.as_deref() {
                validate_working_directory_visible_to_worker(wd)?;
            }

            registered.push(json!({
                "task_key": spec.task_key,
                "title": spec.title,
                "prompt": spec.prompt,
                "backend": spec.backend,
                "model_override": spec.model_override,
                "model_effort": spec.model_effort,
                "working_directory": working_directory,
                "repository": repository,
                "branch": branch,
                "depends_on": spec.depends_on,
            }));
        }

        let response = self
            .api_post(
                &format!("/api/control/missions/{}/board/tasks", self.mission_id),
                json!({ "tasks": registered }),
            )
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to register board tasks: {}", text));
        }
        let mut board: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse board response: {}", e))?;
        if let Some(obj) = board.as_object_mut() {
            obj.insert("worktrees_created".to_string(), json!(worktrees_created));
            obj.insert(
                "hint".to_string(),
                json!(
                    "Tasks registered. The scheduler spawns workers automatically and will \
                     message you a digest as each task settles. Do NOT wait or poll — end \
                     your turn now."
                ),
            );
        }
        Ok(board)
    }

    /// Current board state with utilization counters.
    async fn board_status(&self) -> Result<Value, String> {
        let response = self
            .api_get(&format!("/api/control/missions/{}/board", self.mission_id))
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to fetch board: {}", text));
        }
        response
            .json::<Value>()
            .await
            .map_err(|e| format!("Failed to parse board: {}", e))
    }

    /// Resolve a task by key (on this board) or by UUID.
    async fn resolve_board_task(
        &self,
        task_key: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<Value, String> {
        let board = self.board_status().await?;
        let tasks = board["tasks"]
            .as_array()
            .ok_or("Malformed board response")?;
        let found = tasks.iter().find(|t| match (task_key, task_id) {
            (Some(k), _) => t["task_key"].as_str() == Some(k),
            (None, Some(id)) => t["id"].as_str() == Some(id),
            (None, None) => false,
        });
        found.cloned().ok_or_else(|| {
            format!(
                "Task not found (key={:?}, id={:?}). Keys on this board: {}",
                task_key,
                task_id,
                tasks
                    .iter()
                    .filter_map(|t| t["task_key"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }

    /// Full review payload for a settled task: the task record plus the tail
    /// of the worker mission's conversation.
    async fn review_task(&self, params: ReviewTaskParams) -> Result<Value, String> {
        let task = self
            .resolve_board_task(params.task_key.as_deref(), params.task_id.as_deref())
            .await?;
        let mut out = json!({ "task": task });
        if let Some(worker_id) = task["worker_mission_id"].as_str() {
            let response = self
                .api_get(&format!("/api/control/missions/{}", worker_id))
                .await?;
            if response.status().is_success() {
                if let Ok(mission) = response.json::<Value>().await {
                    let history = mission["history"].as_array().cloned().unwrap_or_default();
                    let tail: Vec<Value> = history
                        .iter()
                        .rev()
                        .take(params.history_limit.max(1))
                        .rev()
                        .cloned()
                        .collect();
                    out["worker_history_tail"] = json!(tail);
                    out["worker_status"] = mission["status"].clone();
                    out["worker_working_directory"] = mission["working_directory"].clone();
                }
            }
        }
        Ok(out)
    }

    async fn task_verdict(
        &self,
        task_key: Option<&str>,
        task_id: Option<&str>,
        action: &str,
        feedback: Option<&str>,
    ) -> Result<Value, String> {
        let task = self.resolve_board_task(task_key, task_id).await?;
        let id = task["id"].as_str().ok_or("Malformed task record")?;
        let response = self
            .api_post(
                &format!("/api/control/board/tasks/{}/verdict", id),
                json!({ "action": action, "feedback": feedback }),
            )
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Verdict failed: {}", text));
        }
        response
            .json::<Value>()
            .await
            .map_err(|e| format!("Failed to parse verdict response: {}", e))
    }

    /// Merge a worker branch into a target branch. On conflict the merge is
    /// aborted and a resolver task is registered on the board automatically.
    async fn merge_branch(&self, params: MergeBranchParams) -> Result<Value, String> {
        let repo_dir = resolve_repo_path(params.repo_path.as_deref());
        let source = params.source_branch.trim();
        let target = params.target_branch.trim();
        if source.is_empty() || target.is_empty() {
            return Err("source_branch and target_branch are required".to_string());
        }

        let run = |args: &[&str]| -> Result<(bool, String), String> {
            let output = Command::new("git")
                .current_dir(&repo_dir)
                .args(args)
                .output()
                .map_err(|e| format!("Failed to run git {:?}: {}", args, e))?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok((output.status.success(), text))
        };

        // The main checkout must be on the target branch; checking it out
        // under a running worker would yank files out from under it.
        let (ok, head) = run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        if !ok {
            return Err(format!("git rev-parse failed in {}: {}", repo_dir, head));
        }
        let head = head.trim().to_string();
        if head != target {
            let (ok, out) = run(&["checkout", target])?;
            if !ok {
                return Err(format!(
                    "Cannot checkout target branch `{}` (HEAD is `{}`): {}",
                    target, head, out
                ));
            }
        }

        let (merged, merge_out) = run(&["merge", "--no-edit", source])?;
        if merged {
            let mut result = json!({
                "merged": true,
                "source_branch": source,
                "target_branch": target,
                "repo_path": repo_dir,
                "output": merge_out.trim(),
            });
            if params.push {
                let (pushed, push_out) = run(&["push", "origin", target])?;
                result["pushed"] = json!(pushed);
                if !pushed {
                    result["push_error"] = json!(push_out.trim());
                }
            }
            if params.delete_source {
                let (deleted, del_out) = run(&["branch", "-D", source])?;
                result["source_deleted"] = json!(deleted);
                if !deleted {
                    result["delete_error"] = json!(del_out.trim());
                }
            }
            return Ok(result);
        }

        // Conflict (or other merge failure): collect conflicted files, abort,
        // and register a resolver task.
        let (_, conflicted) = run(&["diff", "--name-only", "--diff-filter=U"])?;
        let conflicted_files: Vec<String> = conflicted
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        let _ = run(&["merge", "--abort"]);

        if conflicted_files.is_empty() {
            // Not a content conflict — surface the raw error instead of
            // spawning a resolver that can't do anything.
            return Err(format!(
                "git merge failed (no conflicted files detected): {}",
                merge_out.trim()
            ));
        }

        let stamp = chrono::Utc::now().format("%H%M%S");
        let resolver_key = format!("merge-{}-{}", source.replace('/', "_"), stamp);
        let resolver_prompt = format!(
            "Merge conflict resolution in {repo}.\n\
             Run: `git checkout {target}` then `git merge --no-edit {source}`.\n\
             Conflicted files:\n{files}\n\
             Resolve every conflict so BOTH changes' intent is preserved (read both branch \
             histories with `git log {target} -5 --oneline` and `git log {source} -5 --oneline` \
             for context). After resolving: build/verify, `git add -A`, `git commit --no-edit`. \
             Do NOT push. Success condition: merge commit exists on `{target}` and verification \
             passes.",
            repo = repo_dir,
            target = target,
            source = source,
            files = conflicted_files.join("\n"),
        );
        let resolver = self
            .plan_tasks(PlanTasksParams {
                tasks: vec![PlanTaskSpec {
                    task_key: resolver_key.clone(),
                    title: format!("Resolve merge conflict {} → {}", source, target),
                    prompt: resolver_prompt,
                    backend: params
                        .resolver_backend
                        .unwrap_or_else(|| "codex".to_string()),
                    model_override: Some(
                        params
                            .resolver_model
                            .unwrap_or_else(|| "gpt-5.6-sol".to_string()),
                    ),
                    model_effort: Some("high".to_string()),
                    working_directory: Some(repo_dir.clone()),
                    depends_on: vec![],
                    worktree: None,
                }],
            })
            .await;

        Ok(json!({
            "merged": false,
            "conflict": true,
            "source_branch": source,
            "target_branch": target,
            "repo_path": repo_dir,
            "conflicted_files": conflicted_files,
            "resolver_task": resolver_key,
            "resolver_registered": resolver.is_ok(),
            "resolver_error": resolver.err(),
            "hint": "Merge aborted cleanly. A resolver task was registered on the board; \
                     you'll get a digest when it settles. End your turn.",
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
                "awaiting_user".to_string(),
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
                "awaiting_user".to_string(),
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

    /// Hot-swap the sandboxed.sh binary by hitting the API's
    /// `/api/system/deploy` endpoint. The endpoint enforces self-protection
    /// (refuses to kill the caller) and debounce, so the LLM can't
    /// accidentally chainsaw the host by retrying in a loop.
    ///
    /// We propagate `mission_id` so the server-side self-protection check
    /// can find the calling mission in its store. The SSE stream is
    /// consumed eagerly until we see a `deployed` event or the stream
    /// closes (the new binary takes over and our connection drops).
    async fn deploy_sandboxed_sh(&self, params: DeploySandboxedShParams) -> Result<Value, String> {
        let target = resolve_deploy_target(params.target_environment.as_deref())?;
        let api_url = if target.api_url.is_empty() {
            self.api_url.as_str()
        } else {
            target.api_url
        };
        let body = json!({
            "calling_mission_id": self.mission_id,
            "force": params.force,
            "git_ref": params.git_ref,
            "skip_build": params.skip_build,
            "repo_path": params.repo_path,
            "expected_service": target.expected_service,
        });

        let response = self
            .api_post_to(api_url, "/api/system/deploy", body)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!(
                "Deploy refused (HTTP {}): {}",
                status.as_u16(),
                text
            ));
        }

        // Stream SSE until "deployed" or EOF. We deliberately don't fail
        // on EOF-after-deployed: the service restart we asked for is
        // *expected* to kill our connection.
        use futures::StreamExt;
        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        let mut logs: Vec<String> = Vec::new();
        let mut deployed_message: Option<String> = None;
        let mut error_message: Option<String> = None;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| format!("SSE stream error: {}", e))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buf.find("\n\n") {
                let event_block = buf[..idx].to_string();
                buf.drain(..=idx + 1);
                for line in event_block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            let kind = v.get("event_type").and_then(|x| x.as_str()).unwrap_or("");
                            let msg = v
                                .get("message")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            match kind {
                                "deployed" => {
                                    deployed_message = Some(msg);
                                }
                                "error" => {
                                    error_message = Some(msg);
                                }
                                _ => {
                                    logs.push(msg);
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(err) = error_message {
            return Err(format!("Deploy failed: {}", err));
        }
        Ok(json!({
            "deployed": deployed_message.is_some(),
            "summary": deployed_message,
            "logs": logs,
            "hint": "If the API connection dropped right after 'deployed', that's the expected restart firing.",
        }))
    }

    async fn handle_call(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "get_workspace_layout" => Ok(self.get_workspace_layout()),
            "get_backend_auth_status" => {
                let params: BackendAuthStatusParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                Ok(self.get_backend_auth_status(params))
            }
            "durable_job_start" => {
                let params: DurableJobStartParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.durable_job_start(params).await
            }
            "durable_job_list" => self.durable_job_list().await,
            "durable_job_status" => {
                let params: DurableJobIdParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.durable_job_status(params).await
            }
            "durable_job_logs" => {
                let params: DurableJobLogsParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.durable_job_logs(params).await
            }
            "durable_job_cancel" => {
                let params: DurableJobIdParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.durable_job_cancel(params).await
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
            "ask_worker" => {
                let params: AskWorkerParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.ask_worker(params).await
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
            "plan_tasks" => {
                let params: PlanTasksParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.plan_tasks(params).await
            }
            "board_status" => self.board_status().await,
            "review_task" => {
                let params: ReviewTaskParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.review_task(params).await
            }
            "accept_task" => {
                let params: TaskRefParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.task_verdict(
                    params.task_key.as_deref(),
                    params.task_id.as_deref(),
                    "accept",
                    None,
                )
                .await
            }
            "reject_task" => {
                let params: RejectTaskParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.task_verdict(
                    params.task_key.as_deref(),
                    params.task_id.as_deref(),
                    "reject",
                    Some(&params.feedback),
                )
                .await
            }
            "merge_branch" => {
                let params: MergeBranchParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.merge_branch(params).await
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
            "deploy_sandboxed_sh" => {
                let params: DeploySandboxedShParams =
                    serde_json::from_value(params).map_err(|e| format!("Invalid params: {}", e))?;
                self.deploy_sandboxed_sh(params).await
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

/// Persist a repository identifier the API/control process can use after this
/// container-scoped MCP exits. Container guest paths are not necessarily
/// visible on the host, while an OWNER/REPO identity works with `gh --repo`.
fn repository_identity_for_scheduler(repo_path: &str) -> String {
    if !std::path::Path::new(repo_path).exists() {
        return repo_path.to_string();
    }
    let remote = Command::new("git")
        .current_dir(repo_path)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());

    remote
        .as_deref()
        .and_then(github_repository_identity)
        .unwrap_or_else(|| repo_path.to_string())
}

fn github_repository_identity(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches(".git");
    let slug = trimmed
        .split("github.com/")
        .nth(1)
        .or_else(|| trimmed.rsplit_once("github.com:").map(|(_, tail)| tail))?;
    let slug = slug.trim_start_matches('/');
    (slug.split('/').count() == 2).then(|| slug.to_string())
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

/// Validate that the requested `working_directory` is visible to a worker
/// container.
///
/// Worker missions inherit the boss workspace, but their container bind-mounts
/// only the workspace root. A `working_directory` that resolves outside the
/// workspace mount is invisible to the worker and will cause it to fail on
/// first filesystem access. When we detect this, return an error instructing
/// the caller to use the "clone-yourself" pattern instead.
///
/// The check is best-effort: if no workspace mount is configured (Host
/// workspace) we accept any path. The check uses prefix matching on the
/// canonical path string — symlinks are not resolved because the worker side
/// also operates on string paths.
fn validate_working_directory_visible_to_worker(working_directory: &str) -> Result<(), String> {
    let workspace_type = std::env::var("SANDBOXED_SH_WORKSPACE_TYPE")
        .ok()
        .map(|s| s.to_lowercase());
    if workspace_type.as_deref() != Some("container") {
        return Ok(());
    }

    let workspace_mount = match std::env::var("SANDBOXED_SH_WORKSPACE") {
        Ok(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    if path_is_within(working_directory, &workspace_mount) {
        return Ok(());
    }

    Err(format!(
        "error: working_directory '{}' is outside the workspace mount '{}' and will not be visible to the worker container; clone-yourself pattern required",
        working_directory, workspace_mount
    ))
}

/// Return true if `candidate` is the same as `root` or a descendant of it.
///
/// Pure string comparison on normalised paths so the check works without
/// touching the filesystem. Handles trailing slashes on both arguments.
fn path_is_within(candidate: &str, root: &str) -> bool {
    let cand = candidate.trim_end_matches('/');
    let r = root.trim_end_matches('/');
    if cand == r {
        return true;
    }
    cand.starts_with(&format!("{}/", r))
}

/// Enrich a mission JSON object with a `push_claims` array if the mission is
/// completed AND the last assistant message claims to have pushed a branch.
///
/// Each entry has shape `{ branch, claimed_sha, remote_sha, verified }`.
/// `claimed_sha` is the local SHA at `working_directory` (when readable);
/// `remote_sha` comes from `git ls-remote origin <branch>` (when reachable);
/// `verified` is true iff both are present and equal.
///
/// Best-effort: never fails. Synchronous git operations with a short timeout.
/// If the worker did not push, the field is omitted.
/// Latest assistant message content from a mission JSON's `history` array.
fn last_assistant_message(mission: &Value) -> Option<String> {
    mission
        .get("history")
        .and_then(Value::as_array)
        .and_then(|h| {
            h.iter()
                .rev()
                .find(|entry| entry.get("role").and_then(Value::as_str) == Some("assistant"))
                .and_then(|entry| entry.get("content").and_then(Value::as_str))
                .map(|s| s.to_string())
        })
}

/// Fingerprint of a mission's conversation used by ask_worker to detect that
/// a NEW assistant turn landed after the question was sent.
fn history_fingerprint(mission: &Value) -> AskWorkerBaseline {
    let assistant_count = mission
        .get("history")
        .and_then(Value::as_array)
        .map(|h| {
            h.iter()
                .filter(|entry| entry.get("role").and_then(Value::as_str) == Some("assistant"))
                .count()
        })
        .unwrap_or(0);
    AskWorkerBaseline {
        assistant_count,
        last_assistant: last_assistant_message(mission),
    }
}

/// True once the worker has produced a NEW answer since `baseline` AND its
/// turn has settled (status is no longer mid-turn). Pure for testability.
///
/// "New answer" = an additional assistant entry, or the latest assistant
/// content changed (history rewrites). The user message that ask_worker
/// itself sends grows history without adding an assistant entry and must NOT
/// count — that was returning the previous (stale) answer.
fn ask_settled(status: &str, baseline: &AskWorkerBaseline, mission: &Value) -> bool {
    if matches!(status, "active" | "pending" | "acknowledged") {
        return false;
    }
    let current = history_fingerprint(mission);
    let Some(answer) = current.last_assistant.as_deref() else {
        return false;
    };
    current.assistant_count > baseline.assistant_count
        || baseline.last_assistant.as_deref() != Some(answer)
}

fn enrich_with_push_claims(mission: &mut Value) {
    let status = mission
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if status != "completed" {
        return;
    }

    let Some(content) = last_assistant_message(mission) else {
        return;
    };

    if !looks_like_push_claim(&content) {
        return;
    }

    let branches = extract_branch_candidates(&content);
    if branches.is_empty() {
        return;
    }

    let working_directory = mission
        .get("working_directory")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    let mut claims = Vec::new();
    for branch in branches {
        let claimed_sha = working_directory
            .as_deref()
            .and_then(|wd| git_local_sha(wd, &branch));
        let remote_sha = working_directory
            .as_deref()
            .and_then(|wd| git_remote_sha(wd, "origin", &branch));
        let verified = claimed_sha.is_some() && remote_sha.is_some() && claimed_sha == remote_sha;
        claims.push(json!({
            "branch": branch,
            "claimed_sha": claimed_sha,
            "remote_sha": remote_sha,
            "verified": verified,
        }));
    }

    if let Some(obj) = mission.as_object_mut() {
        obj.insert("push_claims".to_string(), Value::Array(claims));
    }
}

/// Heuristic: did the worker claim to have pushed?
fn looks_like_push_claim(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("pushed branch")
        || lower.contains("pushed to")
        || lower.contains("git push")
        || lower.contains("push successful")
        || lower.contains("`git push")
}

/// Extract plausible branch names from an assistant message.
///
/// Looks for patterns like "Pushed branch `name`", "git push origin name",
/// "fix/...", "feat/...", "feature/...", and backtick-wrapped tokens that
/// look like git refs.
fn extract_branch_candidates(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s
            .trim()
            .trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | ',' | '.' | ')' | '('));
        if !s.is_empty() && is_plausible_branch(s) && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };

    // Pattern: "git push origin <branch>"
    for cap in content.split("git push origin ").skip(1) {
        if let Some(token) = cap.split_whitespace().next() {
            push(token);
        }
    }
    // Pattern: "Pushed branch <branch>" / "Pushed branch `<branch>`"
    for cap in content.split("ushed branch ").skip(1) {
        // matches "pushed branch" and "Pushed branch"
        if let Some(token) = cap.split_whitespace().next() {
            push(token);
        }
    }
    // Pattern: bare ref-like tokens fix/... feat/... feature/...
    for word in content.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            matches!(c, '`' | '"' | '\'' | ',' | '.' | ')' | '(' | ':' | ';')
        });
        if trimmed.starts_with("fix/")
            || trimmed.starts_with("feat/")
            || trimmed.starts_with("feature/")
            || trimmed.starts_with("chore/")
            || trimmed.starts_with("refactor/")
        {
            push(trimmed);
        }
    }

    out
}

/// A plausible git branch ref: ASCII, no whitespace, no leading dash, no
/// double slashes, length 1..=200.
fn is_plausible_branch(s: &str) -> bool {
    if s.is_empty() || s.len() > 200 {
        return false;
    }
    if s.starts_with('-') {
        return false;
    }
    if s.contains("//") {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '+'))
}

/// Read the local SHA for `<branch>` (or `refs/heads/<branch>`) in `repo`.
fn git_local_sha(repo: &str, branch: &str) -> Option<String> {
    if !std::path::Path::new(repo).exists() {
        return None;
    }
    let output = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "--verify", branch])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.len() < 7 {
        None
    } else {
        Some(sha)
    }
}

/// Read the remote SHA for `<branch>` from `<remote>` via git ls-remote.
/// Returns None on any failure or if the remote does not have the branch.
fn git_remote_sha(repo: &str, remote: &str, branch: &str) -> Option<String> {
    if !std::path::Path::new(repo).exists() {
        return None;
    }
    let output = Command::new("git")
        .current_dir(repo)
        .args(["ls-remote", "--heads", remote, branch])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?;
    let sha = first.split_whitespace().next()?;
    if sha.len() < 7 {
        None
    } else {
        Some(sha.to_string())
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

#[derive(Debug, Clone)]
struct CodexAuthStatus {
    has_api_key: bool,
    has_oauth: bool,
    has_oauth_account: bool,
    has_valid_auth_json: bool,
    auth_json_path: Option<String>,
    auth_json_mode: Option<String>,
    auth_json_candidates: Vec<String>,
}

impl CodexAuthStatus {
    fn has_credentials(&self) -> bool {
        self.has_api_key || self.has_oauth || self.has_oauth_account || self.has_valid_auth_json
    }
}

fn codex_auth_status(workspace_root: &std::path::Path) -> CodexAuthStatus {
    let has_api_key = get_openai_api_key_for_codex_default(workspace_root).is_some();
    let has_oauth = read_oauth_token_entry(ProviderType::OpenAI).is_some();
    let has_oauth_account = !get_all_openai_oauth_accounts(workspace_root).is_empty();

    let auth_json_candidates = codex_auth_json_candidates(workspace_root);
    let auth_json_candidates_display = auth_json_candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let valid_auth_json = auth_json_candidates
        .iter()
        .filter_map(|path| codex_auth_json_status(path).map(|mode| (path, mode)))
        .next();

    let (auth_json_path, auth_json_mode) = valid_auth_json
        .map(|(path, mode)| (Some(path.display().to_string()), Some(mode)))
        .unwrap_or((None, None));

    CodexAuthStatus {
        has_api_key,
        has_oauth,
        has_oauth_account,
        has_valid_auth_json: auth_json_path.is_some(),
        auth_json_path,
        auth_json_mode,
        auth_json_candidates: auth_json_candidates_display,
    }
}

fn codex_auth_json_candidates(workspace_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        candidates.push(std::path::PathBuf::from(home).join(".codex/auth.json"));
    }

    // In container workspaces Codex runs as root and reads /root/.codex/auth.json.
    candidates.push(std::path::PathBuf::from("/root/.codex/auth.json"));

    // Host-side view of a container workspace: write_codex_credentials_for_workspace
    // writes <workspace_root>/root/.codex/auth.json.
    candidates.push(workspace_root.join("root/.codex/auth.json"));

    // Legacy isolated OpenCode home fallback.
    candidates.push(std::path::PathBuf::from(
        "/var/lib/opencode/.codex/auth.json",
    ));

    let legacy = codex_auth_json_path();
    candidates.push(legacy);

    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing| existing == &candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn codex_auth_json_status(path: &std::path::Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&contents).ok()?;
    codex_auth_json_mode(&value)
}

fn codex_auth_json_mode(value: &serde_json::Value) -> Option<String> {
    let mode = value.get("auth_mode").and_then(|v| v.as_str())?;
    match mode {
        "apikey" => {
            let has_key = value
                .get("tokens")
                .and_then(|v| v.get("api_key"))
                .or_else(|| value.get("api_key"))
                .or_else(|| value.get("OPENAI_API_KEY"))
                .and_then(|v| v.as_str())
                .is_some_and(|token| !token.trim().is_empty());
            has_key.then(|| mode.to_string())
        }
        "chatgpt" => {
            let tokens = value.get("tokens")?;
            let has_access = tokens
                .get("access_token")
                .and_then(|v| v.as_str())
                .is_some_and(|token| !token.trim().is_empty());
            let has_refresh = tokens
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .is_some_and(|token| !token.trim().is_empty());
            (has_access && has_refresh).then(|| mode.to_string())
        }
        _ => None,
    }
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

#[cfg(test)]
mod working_directory_tests {
    use super::{
        github_repository_identity, path_is_within, validate_working_directory_visible_to_worker,
    };

    fn with_env<F: FnOnce()>(workspace_type: Option<&str>, workspace: Option<&str>, f: F) {
        // SAFETY: tests in this module are run serially via `#[test]` with
        // env var manipulation; cargo test parallelism may interleave but
        // every test sets both vars before calling the function, so as long
        // as each test only uses its own scoped values the result is stable
        // enough for the assertions below. To make this rock-solid we wrap
        // with a global mutex.
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        // Stash existing values
        let prev_type = std::env::var("SANDBOXED_SH_WORKSPACE_TYPE").ok();
        let prev_ws = std::env::var("SANDBOXED_SH_WORKSPACE").ok();
        match workspace_type {
            Some(v) => std::env::set_var("SANDBOXED_SH_WORKSPACE_TYPE", v),
            None => std::env::remove_var("SANDBOXED_SH_WORKSPACE_TYPE"),
        }
        match workspace {
            Some(v) => std::env::set_var("SANDBOXED_SH_WORKSPACE", v),
            None => std::env::remove_var("SANDBOXED_SH_WORKSPACE"),
        }
        f();
        // Restore
        match prev_type {
            Some(v) => std::env::set_var("SANDBOXED_SH_WORKSPACE_TYPE", v),
            None => std::env::remove_var("SANDBOXED_SH_WORKSPACE_TYPE"),
        }
        match prev_ws {
            Some(v) => std::env::set_var("SANDBOXED_SH_WORKSPACE", v),
            None => std::env::remove_var("SANDBOXED_SH_WORKSPACE"),
        }
    }

    #[test]
    fn path_within_root_exact_match() {
        assert!(path_is_within("/workspaces/foo", "/workspaces/foo"));
    }

    #[test]
    fn path_within_root_descendant() {
        assert!(path_is_within("/workspaces/foo/bar/baz", "/workspaces/foo"));
    }

    #[test]
    fn path_within_root_trailing_slash_root() {
        assert!(path_is_within("/workspaces/foo/bar", "/workspaces/foo/"));
    }

    #[test]
    fn path_within_root_rejects_prefix_match_without_separator() {
        // /workspaces/foobar must NOT be considered inside /workspaces/foo
        assert!(!path_is_within("/workspaces/foobar", "/workspaces/foo"));
    }

    #[test]
    fn path_within_root_rejects_sibling() {
        assert!(!path_is_within("/workspaces/other", "/workspaces/foo"));
    }

    #[test]
    fn validate_accepts_when_not_container() {
        with_env(Some("host"), Some("/workspaces/foo"), || {
            assert!(validate_working_directory_visible_to_worker("/tmp/elsewhere").is_ok());
        });
    }

    #[test]
    fn validate_accepts_when_no_mount_configured() {
        with_env(Some("container"), None, || {
            assert!(validate_working_directory_visible_to_worker("/tmp/elsewhere").is_ok());
        });
    }

    #[test]
    fn validate_accepts_path_inside_mount() {
        with_env(Some("container"), Some("/workspaces/foo"), || {
            assert!(
                validate_working_directory_visible_to_worker("/workspaces/foo/worker-1").is_ok()
            );
        });
    }

    #[test]
    fn validate_rejects_path_outside_mount() {
        with_env(Some("container"), Some("/workspaces/foo"), || {
            let err = validate_working_directory_visible_to_worker("/tmp/elsewhere")
                .expect_err("should reject path outside mount");
            assert!(
                err.contains("clone-yourself pattern required"),
                "error message did not mention clone-yourself: {}",
                err
            );
            assert!(err.contains("/tmp/elsewhere"));
            assert!(err.contains("/workspaces/foo"));
        });
    }

    #[test]
    fn validate_rejects_sibling_workspace() {
        with_env(Some("container"), Some("/workspaces/foo"), || {
            let err = validate_working_directory_visible_to_worker("/workspaces/foobar/sub")
                .expect_err("sibling path with prefix match should be rejected");
            assert!(err.contains("not be visible to the worker"));
        });
    }

    #[test]
    fn github_repository_identity_parses_https_and_ssh_remotes() {
        assert_eq!(
            github_repository_identity("https://github.com/lfglabs-dev/verity.git").as_deref(),
            Some("lfglabs-dev/verity")
        );
        assert_eq!(
            github_repository_identity("git@github.com:lfglabs-dev/verity.git").as_deref(),
            Some("lfglabs-dev/verity")
        );
        assert_eq!(
            github_repository_identity("https://example.com/repo.git"),
            None
        );
    }
}

#[cfg(test)]
mod codex_auth_status_tests {
    use super::{codex_auth_json_mode, CodexAuthStatus};
    use serde_json::json;

    #[test]
    fn codex_auth_json_mode_accepts_apikey_payload() {
        let value = json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": "sk-test",
        });

        assert_eq!(codex_auth_json_mode(&value), Some("apikey".to_string()));
    }

    #[test]
    fn codex_auth_json_mode_accepts_chatgpt_payload_with_refresh() {
        let value = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "access",
                "refresh_token": "refresh",
                "id_token": "access",
                "account_id": "acct"
            },
            "last_refresh": "2026-06-04T16:01:00Z",
        });

        assert_eq!(codex_auth_json_mode(&value), Some("chatgpt".to_string()));
    }

    #[test]
    fn codex_auth_json_mode_rejects_chatgpt_payload_without_refresh() {
        let value = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "access"
            }
        });

        assert_eq!(codex_auth_json_mode(&value), None);
    }

    #[test]
    fn codex_auth_status_counts_oauth_account_as_credentials() {
        let status = CodexAuthStatus {
            has_api_key: false,
            has_oauth: false,
            has_oauth_account: true,
            has_valid_auth_json: false,
            auth_json_path: None,
            auth_json_mode: None,
            auth_json_candidates: Vec::new(),
        };

        assert!(status.has_credentials());
    }
}

#[cfg(test)]
mod push_claim_tests {
    use super::{
        ask_settled, enrich_with_push_claims, extract_branch_candidates, history_fingerprint,
        is_plausible_branch, last_assistant_message, looks_like_push_claim, AskWorkerBaseline,
    };
    use serde_json::json;

    #[test]
    fn looks_like_push_claim_recognises_common_phrases() {
        assert!(looks_like_push_claim("Pushed branch feat/foo"));
        assert!(looks_like_push_claim("I ran `git push origin fix/bar`"));
        assert!(looks_like_push_claim("Push successful."));
        assert!(!looks_like_push_claim("Nothing to push."));
        assert!(!looks_like_push_claim("Hello world"));
    }

    #[test]
    fn extract_branch_from_pushed_branch_marker() {
        let s = "Pushed branch fix/session-id-collision-recovery to origin.";
        let v = extract_branch_candidates(s);
        assert!(
            v.contains(&"fix/session-id-collision-recovery".to_string()),
            "got {:?}",
            v
        );
    }

    #[test]
    fn extract_branch_from_git_push_command() {
        let s = "ran: git push origin feat/new-thing";
        let v = extract_branch_candidates(s);
        assert!(v.contains(&"feat/new-thing".to_string()), "got {:?}", v);
    }

    #[test]
    fn extract_branch_dedupes_and_strips_backticks() {
        let s = "Pushed branch `fix/foo` (git push origin fix/foo).";
        let v = extract_branch_candidates(s);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], "fix/foo");
    }

    #[test]
    fn is_plausible_branch_rejects_garbage() {
        assert!(is_plausible_branch("fix/foo"));
        assert!(is_plausible_branch("master"));
        assert!(!is_plausible_branch(""));
        assert!(!is_plausible_branch("-rf"));
        assert!(!is_plausible_branch("foo//bar"));
        assert!(!is_plausible_branch("foo bar"));
    }

    #[test]
    fn enrich_skips_non_completed_status() {
        let mut m = json!({
            "status": "running",
            "history": [
                { "role": "assistant", "content": "Pushed branch fix/foo." }
            ]
        });
        enrich_with_push_claims(&mut m);
        assert!(m.get("push_claims").is_none());
    }

    #[test]
    fn enrich_skips_when_no_push_claim() {
        let mut m = json!({
            "status": "completed",
            "history": [
                { "role": "assistant", "content": "All done." }
            ]
        });
        enrich_with_push_claims(&mut m);
        assert!(m.get("push_claims").is_none());
    }

    #[test]
    fn enrich_adds_unverified_claim_when_repo_missing() {
        // No working_directory, so claimed_sha and remote_sha will be None
        // and verified must be false; the claim entry must still be present.
        let mut m = json!({
            "status": "completed",
            "history": [
                { "role": "assistant", "content": "Pushed branch fix/foo to origin." }
            ]
        });
        enrich_with_push_claims(&mut m);
        let claims = m
            .get("push_claims")
            .and_then(|v| v.as_array())
            .expect("push_claims should be present");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["branch"], "fix/foo");
        assert_eq!(claims[0]["verified"], false);
        assert!(claims[0]["claimed_sha"].is_null());
        assert!(claims[0]["remote_sha"].is_null());
    }

    #[test]
    fn enrich_picks_last_assistant_message() {
        let mut m = json!({
            "status": "completed",
            "history": [
                { "role": "user", "content": "go push something" },
                { "role": "assistant", "content": "Nothing pushed yet." },
                { "role": "user", "content": "retry" },
                { "role": "assistant", "content": "Pushed branch fix/final." }
            ]
        });
        enrich_with_push_claims(&mut m);
        let claims = m
            .get("push_claims")
            .and_then(|v| v.as_array())
            .expect("push_claims should be present");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["branch"], "fix/final");
    }

    #[test]
    fn last_assistant_message_extracts_latest() {
        let m = json!({
            "history": [
                { "role": "assistant", "content": "first" },
                { "role": "user", "content": "follow-up" },
                { "role": "assistant", "content": "second" }
            ]
        });
        assert_eq!(last_assistant_message(&m).as_deref(), Some("second"));
        assert_eq!(last_assistant_message(&json!({})), None);
        assert_eq!(
            last_assistant_message(&json!({ "history": [{ "role": "user", "content": "hi" }] })),
            None
        );
    }

    #[test]
    fn history_fingerprint_detects_new_turn() {
        let before = json!({
            "history": [
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "a1" }
            ]
        });
        let baseline = history_fingerprint(&before);
        assert_eq!(baseline.assistant_count, 1);
        assert_eq!(baseline.last_assistant.as_deref(), Some("a1"));

        // Unchanged history → not settled even when awaiting_user.
        assert!(!ask_settled("awaiting_user", &baseline, &before));

        // The question ask_worker just sent appends a USER entry — history
        // grew but no new assistant answer exists yet. Returning here was
        // the stale-answer bug.
        let question_sent = json!({
            "history": [
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "a1" },
                { "role": "user", "content": "q2" }
            ]
        });
        assert!(!ask_settled("awaiting_user", &baseline, &question_sent));

        // New assistant entry → settled once the status leaves mid-turn.
        let after = json!({
            "history": [
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "a1" },
                { "role": "user", "content": "q2" },
                { "role": "assistant", "content": "a2" }
            ]
        });
        assert!(ask_settled("awaiting_user", &baseline, &after));
        assert!(ask_settled("completed", &baseline, &after));
        // Still actively producing the turn → keep waiting.
        assert!(!ask_settled("active", &baseline, &after));
        assert!(!ask_settled("pending", &baseline, &after));
    }

    #[test]
    fn ask_settled_handles_replaced_answer_without_length_change() {
        // Some backends rewrite history (e.g. canonicalized assistant
        // message) without growing it; a changed last-assistant content
        // still counts as a new answer.
        let baseline = AskWorkerBaseline {
            assistant_count: 1,
            last_assistant: Some("a1".to_string()),
        };
        let after = json!({
            "history": [
                { "role": "user", "content": "q1" },
                { "role": "assistant", "content": "rewritten answer" }
            ]
        });
        assert!(ask_settled("awaiting_user", &baseline, &after));
    }
}
