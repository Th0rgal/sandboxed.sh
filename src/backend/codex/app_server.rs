//! Codex `app-server` client — drives `codex app-server --enable goals` over
//! stdio using JSON-RPC framing.
//!
//! Replaces the legacy `codex exec` path for missions that need first-class
//! `/goal` support. The exec entrypoint forwards the prompt verbatim to the
//! model and never parses slash commands, so `thread/goal/set` over the v2 RPC
//! is the only way to arm codex's native continuation loop.
//!
//! Protocol notes:
//! - Wire format is "JSON-RPC lite" — newline-delimited JSON, no `jsonrpc`
//!   field on responses, `id` is a JSON number.
//! - All goal RPCs require `capabilities.experimentalApi = true` declared at
//!   `initialize` time. Without it, server rejects with
//!   `"thread/goal/set requires experimentalApi capability"`.
//! - After `thread/goal/set`, codex auto-starts a turn — clients only need to
//!   send `turn/start` for non-goal sessions or follow-up user input.
//! - Goal terminal status arrives as `thread/goal/updated` with
//!   `goal.status ∈ {"complete", "budgetLimited"}`. The model's
//!   `update_goal` tool call also surfaces as a normal `item/started` +
//!   `item/completed`, but the notification is the canonical signal.
//!
//! Empirical reference: see the `reference_codex_app_server_protocol` memory
//! and the `rust-v0.128.0` tag of github.com/openai/codex.
//!
//! This is the **scaffold/transport layer** — the typed API for `initialize`,
//! `thread/start`, `turn/start`, `thread/goal/{set,clear}`, plus a notification
//! stream. Higher-level event translation lives in the parent `mod.rs` once
//! this layer is wired into `CodexBackend`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::workspace_exec::WorkspaceExec;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Settings for spawning a `codex app-server` process.
#[derive(Debug, Clone)]
pub struct AppServerConfig {
    /// Path to the `codex` binary. Defaults to `codex` (resolved via PATH).
    pub cli_path: String,
    /// Comma-separated list of feature flags to pass to `--enable`.
    /// Always includes `goals` for goal-mode sessions.
    pub enabled_features: Vec<String>,
    /// Optional default model override (`thread/start` accepts `model`).
    pub default_model: Option<String>,
    /// Reasoning effort (`thread/start` accepts `reasoningEffort`).
    pub model_effort: Option<String>,
    /// Extra environment variables to inject into the codex child process.
    pub env: HashMap<String, String>,
}

impl Default for AppServerConfig {
    fn default() -> Self {
        Self {
            cli_path: std::env::var("CODEX_CLI_PATH").unwrap_or_else(|_| "codex".to_string()),
            enabled_features: vec!["goals".to_string()],
            default_model: None,
            model_effort: None,
            env: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types — only the request/response shapes we actually drive.
// Everything else stays as `serde_json::Value` so we can forward it.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct InitializeParams {
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
    capabilities: ClientCapabilities,
}

#[derive(Debug, Serialize)]
struct ClientInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct ClientCapabilities {
    #[serde(rename = "experimentalApi")]
    experimental_api: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct InitializeResult {
    #[serde(rename = "userAgent")]
    pub user_agent: String,
    #[serde(rename = "codexHome")]
    pub codex_home: String,
    #[serde(rename = "platformFamily")]
    pub platform_family: String,
    #[serde(rename = "platformOs")]
    pub platform_os: String,
}

/// Subset of `thread/start` params we use. Codex 0.128.0 has many more
/// (experimental-gated) fields; add them as we adopt them.
#[derive(Debug, Serialize, Default)]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "reasoningEffort", skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(rename = "serviceTier", skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Goals require non-ephemeral threads (`thread_goal_handlers.rs:33-42`).
    /// Leave `None` so codex picks the default (non-ephemeral).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
    /// Codex `ApprovalPolicy`. Valid values empirically:
    /// `untrusted`, `on-failure`, `on-request`, `granular`, `never`.
    /// Default in app-server is `on-request` — codex will block on
    /// elicitations for every shell command and tool call. Mirror exec-mode's
    /// `--dangerously-bypass-approvals-and-sandbox` by sending `never` so the
    /// agent runs without prompting.
    #[serde(rename = "approvalPolicy", skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    /// Codex sandbox policy (string form on the wire — codex echoes it back as
    /// a tagged object). Valid values: `read-only`, `workspace-write`,
    /// `danger-full-access`. Default is `read-only`, which blocks shell writes
    /// to `/tmp` and outside `cwd`. We pick `danger-full-access` to match
    /// exec-mode's bypass-sandbox behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadStartResult {
    pub thread: ThreadHandle,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ThreadHandle {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct TurnStartParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub input: Vec<UserInputItem>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInputItem {
    Text { text: String },
}

#[derive(Debug, Serialize)]
pub struct GoalSetParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub objective: String,
    /// Optional token budget — `null` clears, omitted leaves unchanged.
    #[serde(rename = "tokenBudget", skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct GoalClearParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

// ---------------------------------------------------------------------------
// Notification envelope — kept as raw values; the caller does the matching.
// ---------------------------------------------------------------------------

/// One inbound message from app-server: either a response keyed by id, a
/// notification (no id), or a server-side request (id but `method` set, the
/// caller must respond).
#[derive(Debug, Clone)]
pub enum InboundMessage {
    /// Notification from server (`method`, `params`, no `id`).
    Notification { method: String, params: Value },
    /// Server-initiated request (we must reply with matching `id`).
    /// Codex uses this for elicitations — approval prompts, dynamic tool
    /// calls, auth-token refreshes.
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
}

// ---------------------------------------------------------------------------
// JSON-RPC client over stdio
// ---------------------------------------------------------------------------

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Debug, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// A live connection to a `codex app-server` process.
pub struct AppServerSession {
    next_id: Arc<Mutex<i64>>,
    pending: PendingMap,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Receives every inbound notification or server-request.
    inbound_rx: Mutex<Option<mpsc::Receiver<InboundMessage>>>,
    child: Arc<Mutex<Option<Child>>>,
    reader_task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for AppServerSession {
    fn drop(&mut self) {
        // If the caller dropped the session without calling `shutdown()` (e.g.
        // mission_runner panicked, receiver was dropped abruptly, or a
        // mid-flight error path forgot to clean up) make a best-effort kill
        // of the child process. Without this the codex app-server stays
        // alive after the mission row is gone.
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
        if let Ok(mut guard) = self.reader_task.try_lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
    }
}

impl AppServerSession {
    /// Spawn `codex app-server`, wire stdio, and run the JSON-RPC reader loop.
    /// Caller is expected to follow up with [`Self::initialize`].
    pub async fn spawn(
        config: AppServerConfig,
        cwd: &str,
        workspace_exec: Option<&WorkspaceExec>,
    ) -> Result<Self> {
        let mut args = vec!["app-server".to_string()];
        for feat in &config.enabled_features {
            args.push("--enable".to_string());
            args.push(feat.clone());
        }

        info!(
            "Spawning codex app-server: cwd={}, features={:?}",
            cwd, config.enabled_features
        );

        // Two spawn paths mirror the legacy exec client — host vs container
        // workspace exec — so app-server works inside per-mission systemd-nspawn
        // containers without losing stdio.
        let mut child = if let Some(exec) = workspace_exec {
            exec.spawn_streaming(Path::new(cwd), &config.cli_path, &args, config.env.clone())
                .await
                .map_err(|e| {
                    error!("Failed to spawn codex app-server in workspace: {}", e);
                    anyhow!("Failed to spawn codex app-server in workspace: {}", e)
                })?
        } else {
            let mut cmd = Command::new(&config.cli_path);
            cmd.current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .args(&args);
            if !config.env.is_empty() {
                cmd.envs(&config.env);
            }
            cmd.spawn().map_err(|e| {
                anyhow!(
                    "Failed to spawn codex app-server '{}': {}",
                    config.cli_path,
                    e
                )
            })?
        };

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow!("codex app-server child missing stdin pipe — required for JSON-RPC writes")
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex app-server child missing stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("codex app-server child missing stderr pipe"))?;

        // Drain stderr in the background — failure to do so deadlocks codex
        // when its log buffer fills up. Lines are debug-logged so they show
        // up under RUST_LOG=debug without polluting normal logs.
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    debug!("codex app-server stderr: {}", trimmed);
                }
            }
        });

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (inbound_tx, inbound_rx) = mpsc::channel::<InboundMessage>(256);

        // Reader loop: pulls newline-delimited JSON, dispatches responses to
        // pending oneshots and notifications/server-requests onto the inbound
        // channel.
        let pending_for_task = Arc::clone(&pending);
        let reader_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "codex app-server: failed to parse line as JSON: {} — line={}",
                            e, trimmed
                        );
                        continue;
                    }
                };

                // Route based on shape: response (`id` + `result|error`),
                // notification (`method`, no `id`), or server-request
                // (`id` + `method`).
                let has_id = value.get("id").is_some();
                let has_method = value.get("method").is_some();
                let has_result_or_error =
                    value.get("result").is_some() || value.get("error").is_some();

                if has_id && has_result_or_error {
                    if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
                        if let Some(sender) = pending_for_task.lock().await.remove(&id) {
                            if let Some(err) = value.get("error") {
                                let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                                let message = err
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown error")
                                    .to_string();
                                let _ = sender.send(Err(RpcError { code, message }));
                            } else if let Some(result) = value.get("result").cloned() {
                                let _ = sender.send(Ok(result));
                            } else {
                                let _ = sender.send(Ok(Value::Null));
                            }
                        } else {
                            warn!("codex app-server: response for unknown id={}", id);
                        }
                    }
                } else if has_method && has_id {
                    // Server-initiated request (elicitation). Forward up so
                    // the caller can reply.
                    let id = value.get("id").cloned().unwrap_or(Value::Null);
                    let method = value
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let params = value.get("params").cloned().unwrap_or(Value::Null);
                    if inbound_tx
                        .send(InboundMessage::ServerRequest { id, method, params })
                        .await
                        .is_err()
                    {
                        debug!("codex app-server: inbound channel closed");
                        break;
                    }
                } else if has_method {
                    let method = value
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let params = value.get("params").cloned().unwrap_or(Value::Null);
                    if inbound_tx
                        .send(InboundMessage::Notification { method, params })
                        .await
                        .is_err()
                    {
                        debug!("codex app-server: inbound channel closed");
                        break;
                    }
                } else {
                    warn!("codex app-server: unrecognized message shape — {}", trimmed);
                }
            }
            debug!("codex app-server: reader loop exited (EOF)");
            // Drain any still-pending request senders so callers stuck in
            // `rx.await` get a clear error instead of hanging until the
            // session is dropped. Without this drain, `request()` blocks
            // forever when codex closes stdout mid-RPC (process crash,
            // panic, or `kill`).
            let mut pending_guard = pending_for_task.lock().await;
            for (_, sender) in pending_guard.drain() {
                let _ = sender.send(Err(RpcError {
                    code: 0,
                    message: "codex app-server stream closed before responding".to_string(),
                }));
            }
        });

        // `config` was consumed only for its `cli_path`/`enabled_features`/
        // `env` fields when spawning. The session itself doesn't carry it
        // around because `default_model` and `model_effort` are passed
        // through to `thread/start` by the caller.
        let _ = config;

        Ok(Self {
            next_id: Arc::new(Mutex::new(1)),
            pending,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            inbound_rx: Mutex::new(Some(inbound_rx)),
            child: Arc::new(Mutex::new(Some(child))),
            reader_task: Mutex::new(Some(reader_task)),
        })
    }

    /// Take the inbound message stream. Each session yields it exactly once.
    pub async fn take_inbound(&self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.inbound_rx.lock().await.take()
    }

    /// Send a request and await the response.
    async fn request<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R> {
        let id = {
            let mut id_lock = self.next_id.lock().await;
            let id = *id_lock;
            *id_lock += 1;
            id
        };

        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = format!("{}\n", serde_json::to_string(&envelope)?);

        // Insert the pending sender BEFORE writing to stdout so a fast
        // server response (responses can arrive before `write_all` returns
        // on a busy event loop) doesn't get logged as "response for unknown
        // id" by the reader task. If the write fails, we remove the entry
        // so the pending map doesn't grow unbounded across failed RPCs.
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let write_result = {
            let mut stdin_lock = self.stdin.lock().await;
            match stdin_lock.as_mut() {
                Some(stdin) => match stdin.write_all(line.as_bytes()).await {
                    Ok(()) => {
                        stdin.flush().await.ok();
                        Ok(())
                    }
                    Err(e) => Err(anyhow!("writing {} request: {}", method, e)),
                },
                None => Err(anyhow!("codex app-server stdin closed")),
            }
        };

        if let Err(e) = write_result {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        let raw = rx
            .await
            .map_err(|_| {
                anyhow!(
                    "codex app-server reader task dropped before responding to {}",
                    method
                )
            })?
            .map_err(|e| anyhow!("{} failed: {}", method, e))?;
        let parsed: R =
            serde_json::from_value(raw).with_context(|| format!("parsing {} response", method))?;
        Ok(parsed)
    }

    /// Send a server-request response (id mirrored back, `result` payload).
    pub async fn respond_to_server_request(&self, id: Value, result: Value) -> Result<()> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let line = format!("{}\n", serde_json::to_string(&envelope)?);
        let mut stdin_lock = self.stdin.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server stdin closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await.ok();
        Ok(())
    }

    /// Send a JSON-RPC error response to a server-initiated request. Used for
    /// elicitations we can't satisfy (e.g. `account/chatgptAuthTokens/refresh`
    /// — we don't carry refresh credentials in this client). Returning a
    /// typed error tells codex to fail the in-flight turn immediately rather
    /// than wait for a 10-second timeout on a malformed `null` result.
    pub async fn respond_to_server_request_error(
        &self,
        id: Value,
        code: i64,
        message: &str,
    ) -> Result<()> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        });
        let line = format!("{}\n", serde_json::to_string(&envelope)?);
        let mut stdin_lock = self.stdin.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server stdin closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await.ok();
        Ok(())
    }

    /// Send the `notifications/initialized` client notification (LSP-style
    /// handshake completion). Best-effort — the server tolerates clients that
    /// skip it but some experimental gates expect to see it.
    pub async fn send_initialized_notification(&self) -> Result<()> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        });
        let line = format!("{}\n", serde_json::to_string(&envelope)?);
        let mut stdin_lock = self.stdin.lock().await;
        let stdin = stdin_lock
            .as_mut()
            .ok_or_else(|| anyhow!("codex app-server stdin closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await.ok();
        Ok(())
    }

    // -------- Typed RPC methods --------

    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<InitializeResult> {
        self.request(
            "initialize",
            InitializeParams {
                client_info: ClientInfo {
                    name: client_name.to_string(),
                    version: client_version.to_string(),
                },
                capabilities: ClientCapabilities {
                    experimental_api: true,
                },
            },
        )
        .await
    }

    pub async fn thread_start(&self, params: ThreadStartParams) -> Result<ThreadStartResult> {
        // No defaults applied here — the caller (`send_message_streaming_app_server`)
        // already merges `CodexConfig.default_model` / `model_effort` into
        // `params` before invoking, and keeping the fallback in two places
        // would silently disagree if one drifts.
        self.request("thread/start", params).await
    }

    pub async fn login_chatgpt_auth_tokens(
        &self,
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> Result<Value> {
        self.request(
            "account/login/start",
            json!({
                "type": "chatgptAuthTokens",
                "accessToken": access_token,
                "chatgptAccountId": chatgpt_account_id,
                "chatgptPlanType": chatgpt_plan_type,
            }),
        )
        .await
    }

    /// Resume a previously-started thread by id. Codex 0.128.0 accepts
    /// `thread/resume` with `{threadId}` to reattach to a session whose
    /// rollout already lives under `$CODEX_HOME/sessions/...` — used by
    /// the backend to recover from a mid-mission codex crash without
    /// losing the thread state.
    ///
    /// The response is the same `ThreadStartResult` shape (`{thread, ...}`).
    pub async fn thread_resume(&self, thread_id: &str) -> Result<ThreadStartResult> {
        self.request(
            "thread/resume",
            json!({
                "threadId": thread_id,
            }),
        )
        .await
    }

    pub async fn turn_start(&self, params: TurnStartParams) -> Result<Value> {
        // We don't strongly type the response — the caller cares about
        // notifications, not the immediate `{turn: ...}` echo.
        self.request("turn/start", params).await
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: Option<&str>) -> Result<Value> {
        let mut params = json!({ "threadId": thread_id });
        if let Some(tid) = turn_id {
            params["turnId"] = json!(tid);
        }
        self.request("turn/interrupt", params).await
    }

    pub async fn goal_set(&self, params: GoalSetParams) -> Result<Value> {
        self.request("thread/goal/set", params).await
    }

    pub async fn goal_clear(&self, thread_id: &str) -> Result<Value> {
        self.request(
            "thread/goal/clear",
            GoalClearParams {
                thread_id: thread_id.to_string(),
            },
        )
        .await
    }

    /// Hard-stop: kill the child process and drop the reader task.
    pub async fn shutdown(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            if let Err(e) = child.kill().await {
                debug!("codex app-server kill: {}", e);
            }
        }
        if let Some(task) = self.reader_task.lock().await.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_input_serializes_with_camelcase_tag() {
        let item = UserInputItem::Text {
            text: "hi".to_string(),
        };
        let s = serde_json::to_string(&item).unwrap();
        assert!(s.contains("\"type\":\"text\""));
        assert!(s.contains("\"text\":\"hi\""));
    }

    #[test]
    fn turn_start_params_serialize_threadid_camelcase() {
        let p = TurnStartParams {
            thread_id: "abc".to_string(),
            input: vec![UserInputItem::Text {
                text: "hi".to_string(),
            }],
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"threadId\":\"abc\""));
    }

    #[test]
    fn thread_start_params_serialize_fast_service_tier() {
        let params = ThreadStartParams {
            model: Some("gpt-5.6-sol".to_string()),
            reasoning_effort: Some("low".to_string()),
            service_tier: Some("fast".to_string()),
            cwd: Some("/tmp/workspace".to_string()),
            ephemeral: None,
            approval_policy: Some("never".to_string()),
            sandbox: Some("danger-full-access".to_string()),
        };
        let value = serde_json::to_value(params).unwrap();
        assert_eq!(value["serviceTier"], "fast");
        assert_eq!(value["model"], "gpt-5.6-sol");
    }

    #[test]
    fn goal_set_skips_token_budget_when_none() {
        let p = GoalSetParams {
            thread_id: "abc".to_string(),
            objective: "do the thing".to_string(),
            token_budget: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("tokenBudget"));
    }

    #[test]
    fn initialize_params_include_experimental_api_capability() {
        let p = InitializeParams {
            client_info: ClientInfo {
                name: "s".to_string(),
                version: "1".to_string(),
            },
            capabilities: ClientCapabilities {
                experimental_api: true,
            },
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"experimentalApi\":true"));
    }
}
