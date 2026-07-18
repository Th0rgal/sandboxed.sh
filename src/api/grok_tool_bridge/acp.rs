//! Live ACP↔MCP transport for the connected-account Grok bridge.
//!
//! This is the only part of the bridge that talks to a real `grok agent stdio`
//! child. It is **default-off** (see [`super::capability`]): the pure state
//! machine above it is exhaustively tested via the fake backend, but this live
//! path can only be exercised against a provisioned xAI/Grok account, so it is
//! gated behind an operator-set `SANDBOXED_SH_GROK_ROUTER_BRIDGE_LIVE` flag.
//!
//! ## How caller tools stay caller-owned
//!
//! Caller `tools[]` are rendered as an **ephemeral in-process MCP server** bound
//! to `127.0.0.1:0` and offered to the Grok session via `session/new`'s
//! `mcpServers`. When Grok decides to call one of those tools it issues an MCP
//! `tools/call` over HTTP; our handler **suspends** that HTTP request (parks the
//! JSON-RPC response on a oneshot) and forwards the call to the bridge, which
//! surfaces it to the OpenAI caller as `assistant.tool_calls`. The caller's
//! later `role: "tool"` result is delivered back through the oneshot, unblocking
//! the still-open `tools/call` so the same Grok session continues its turn.
//!
//! Grok therefore never executes the caller's tools itself — it only ever sees
//! an MCP endpoint whose responses the caller supplies.
//!
//! ## Verification status
//!
//! The exact `mcpServers` HTTP descriptor shape a given Grok CLI build accepts
//! is a deployment detail we cannot assert without a live account; it is
//! documented at the `session/new` call below. Until an operator verifies this
//! path against their connected account (and flips the live flag), the route is
//! not advertised. Every failure here is classified as
//! [`BridgeError::upstream`] (infrastructure) or
//! [`BridgeError::provider_configuration`] — never dressed up as a model turn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::error::BridgeError;
use super::transport::{
    BridgeBackend, BridgeConversation, BridgeUsage, CompletedTurn, PromptInput, RequestedToolCall,
    ResolvedToolResult, TurnOutcome,
};

/// JSON-RPC ids for the ACP handshake. Kept distinct so responses are
/// unambiguous on a shared stdout stream.
const ACP_INIT_ID: u64 = 1;
const ACP_SESSION_NEW_ID: u64 = 2;
const ACP_SET_MODEL_ID: u64 = 3;
const ACP_PROMPT_ID: u64 = 4;

/// Deadline for each synchronous handshake step.
const HANDSHAKE_STEP_SECS: u64 = 60;

/// Idle guard for the post-prompt event stream: a silent gap this long means
/// the CLI is wedged (or blocked on something that never arrives headless).
const DRIVER_IDLE_SECS: u64 = 300;

/// After the first suspended tool call arrives, wait this long for additional
/// concurrent calls so a parallel tool batch surfaces as one `tool_calls` turn.
/// A sequential CLI simply yields one call per turn (the window elapses idle),
/// which is equally valid OpenAI semantics.
const TOOL_BATCH_SETTLE_MS: u64 = 75;

/// A suspended MCP `tools/call`: the invocation Grok made, plus the oneshot the
/// HTTP handler is blocked on until the caller supplies a result.
struct InboundToolCall {
    name: String,
    arguments: serde_json::Value,
    responder: oneshot::Sender<String>,
}

/// Shared state for the ephemeral MCP server.
struct McpState {
    /// Caller tool descriptors (`name`/`description`/`inputSchema`).
    tools: Vec<serde_json::Value>,
    /// Channel to hand suspended calls to the bridge coordination loop.
    tool_tx: mpsc::UnboundedSender<InboundToolCall>,
    /// Per-session bearer value required by every request to this endpoint.
    bearer_value: String,
}

/// Owns the ephemeral MCP server during startup. Any early return cancels and
/// aborts it; once the conversation is fully constructed, ownership is
/// transferred into [`AcpConversation`].
struct StartupTaskGuard {
    cancel: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl StartupTaskGuard {
    fn new(cancel: CancellationToken, handle: JoinHandle<()>) -> Self {
        Self {
            cancel,
            handle: Some(handle),
        }
    }

    fn into_handle(mut self) -> JoinHandle<()> {
        self.handle
            .take()
            .expect("startup task guard must own a handle")
    }
}

impl Drop for StartupTaskGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cancel.cancel();
            handle.abort();
        }
    }
}

fn jsonrpc_result(id: Option<serde_json::Value>, result: serde_json::Value) -> Response {
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "result": result,
    }))
    .into_response()
}

fn jsonrpc_error(id: Option<serde_json::Value>, code: i32, message: &str) -> Response {
    Json(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "error": { "code": code, "message": message },
    }))
    .into_response()
}

/// ACP's HTTP MCP-server variant requires a `headers` array. The bearer secret
/// prevents unrelated same-host processes from injecting caller tool calls
/// into the live session.
fn http_mcp_server_descriptor(url: &str, bearer_value: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "http",
        "name": "grok-cli-bridge-tools",
        "url": url,
        "headers": [{
            "name": "Authorization",
            "value": bearer_value,
        }],
    })
}

/// The ephemeral MCP server's single JSON-RPC endpoint. Suspends `tools/call`
/// until the bridge resumes it.
async fn mcp_handler(
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(state.bearer_value.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "initialize" => jsonrpc_result(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "grok-cli-bridge", "version": env!("CARGO_PKG_VERSION") },
            }),
        ),
        // Notifications carry no id and expect no JSON-RPC body.
        m if m.starts_with("notifications/") => StatusCode::ACCEPTED.into_response(),
        "tools/list" => jsonrpc_result(id, serde_json::json!({ "tools": state.tools })),
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return jsonrpc_error(id, -32602, "tools/call missing tool name");
            }
            if !state
                .tools
                .iter()
                .any(|tool| tool.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
            {
                return jsonrpc_error(id, -32602, "tools/call references an unadvertised tool");
            }
            let arguments = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let (responder, rx) = oneshot::channel();
            if state
                .tool_tx
                .send(InboundToolCall {
                    name,
                    arguments,
                    responder,
                })
                .is_err()
            {
                // Bridge coordination loop is gone — session tearing down.
                return jsonrpc_error(id, -32000, "grok bridge session closed");
            }
            // Suspend here: the HTTP response is withheld until the caller's
            // tool result unblocks the oneshot (or the session is dropped).
            match rx.await {
                Ok(content) => jsonrpc_result(
                    id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": content }],
                        "isError": false,
                    }),
                ),
                Err(_) => {
                    jsonrpc_error(id, -32000, "grok bridge session closed before tool result")
                }
            }
        }
        _ => jsonrpc_error(id, -32601, "method not found"),
    }
}

/// Resolve the CLI path (override via env for non-default installs).
fn grok_cli_path() -> String {
    std::env::var("SANDBOXED_SH_GROK_CLI_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "grok".to_string())
}

/// Build the non-interactive auth env for the connected account.
///
/// This bridge deliberately accepts **only a genuine, explicitly configured xAI
/// API key** ([`get_xai_api_key_for_grok`] reads it verbatim from
/// `ai_providers.json` and never falls back to OAuth). We do NOT relabel an
/// OAuth access token as `XAI_API_KEY`: that masquerade is unsupported by the
/// account and would silently break, so instead we fail closed with an exact
/// blocker and the route stays unadvertised (see [`super::capability`]). The
/// credential itself is never logged.
fn grok_auth_env(working_dir: &Path) -> Result<HashMap<String, String>, BridgeError> {
    let key = crate::api::ai_providers::get_xai_api_key_for_grok(working_dir).ok_or_else(|| {
        BridgeError::provider_configuration(
            "the grok-cli bridge requires a genuine xAI API key configured for the grok backend; \
             an OAuth-only connected account cannot be used non-interactively and is never \
             relabeled as an API key",
        )
    })?;

    let mut env = HashMap::new();
    env.insert("XAI_API_KEY".to_string(), key.clone());
    env.insert("GROK_CODE_XAI_API_KEY".to_string(), key);
    Ok(env)
}

async fn send_rpc(stdin: &mut ChildStdin, value: serde_json::Value) -> Result<(), BridgeError> {
    let mut payload = value.to_string();
    payload.push('\n');
    stdin
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| BridgeError::upstream(format!("grok ACP stdin write failed: {e}")))
}

/// Read stdout until the JSON-RPC response for `id` arrives, with a deadline.
async fn await_response<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut Lines<R>,
    id: u64,
) -> Result<serde_json::Value, BridgeError> {
    let deadline = Instant::now() + Duration::from_secs(HANDSHAKE_STEP_SECS);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                BridgeError::upstream(format!("grok ACP timed out awaiting response id {id}"))
            })?;
        let line = tokio::time::timeout(remaining, lines.next_line())
            .await
            .map_err(|_| {
                BridgeError::upstream(format!("grok ACP timed out awaiting response id {id}"))
            })?
            .map_err(|e| BridgeError::upstream(format!("grok ACP stdout read failed: {e}")))?
            .ok_or_else(|| {
                BridgeError::upstream("grok ACP stream closed during handshake".to_string())
            })?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
            if let Some(err) = value.get("error") {
                return Err(BridgeError::upstream(format!(
                    "grok ACP request {id} failed: {err}"
                )));
            }
            return Ok(value);
        }
        // Any notification received mid-handshake is discarded — no tool calls
        // can occur before the prompt is sent.
    }
}

fn extract_tokens(meta: &serde_json::Value, keys: &[&str]) -> u64 {
    for key in keys {
        if let Some(n) = meta.get(key).and_then(|v| v.as_u64()) {
            return n;
        }
    }
    0
}

/// Build a fail-closed response to an ACP `session/request_permission`.
///
/// The bridge never authorizes a built-in host action. We prefer an explicit
/// `reject*` option so the CLI records a genuine denial; if the agent offered no
/// reject option we return the ACP `cancelled` outcome, which is also a refusal.
/// We never select an `allow*` (or arbitrary first) option.
fn deny_permission_outcome(options: Option<&serde_json::Value>) -> serde_json::Value {
    if let Some(opts) = options.and_then(|v| v.as_array()) {
        let reject = opts
            .iter()
            .find(|o| {
                o.get("kind")
                    .and_then(|k| k.as_str())
                    .is_some_and(|k| k.starts_with("reject") || k.starts_with("deny"))
            })
            .and_then(|o| o.get("optionId"))
            .cloned();
        if let Some(option_id) = reject {
            return serde_json::json!({ "outcome": "selected", "optionId": option_id });
        }
    }
    serde_json::json!({ "outcome": "cancelled" })
}

/// Terminal result the driver task produces exactly once per session.
type DriverResult = Result<CompletedTurn, BridgeError>;

/// Drain the child's stderr to EOF so its pipe buffer can never fill and
/// deadlock the CLI. Content is **not** echoed — grok stderr can contain
/// sensitive material, so we only emit a redacted line count at debug level.
async fn drain_stderr(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    let mut count: u64 = 0;
    while let Ok(Some(_line)) = lines.next_line().await {
        count += 1;
    }
    if count > 0 {
        tracing::debug!(
            stderr_lines = count,
            "grok bridge child stderr drained (content redacted)"
        );
    }
}

/// Background task: pump the ACP stdout stream after the prompt is sent,
/// auto-approving permission requests, accumulating assistant text, and
/// resolving the turn when `session/prompt` completes.
async fn run_driver(
    mut stdin: ChildStdin,
    mut lines: Lines<BufReader<tokio::process::ChildStdout>>,
    mut model: String,
    cancel: CancellationToken,
    done_tx: oneshot::Sender<DriverResult>,
) {
    let mut text = String::new();
    let idle = Duration::from_secs(DRIVER_IDLE_SECS);
    let result: DriverResult = loop {
        let read = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break Err(BridgeError::session_state("grok bridge session was shut down"));
            }
            line = tokio::time::timeout(idle, lines.next_line()) => line,
        };
        let line = match read {
            Err(_) => {
                break Err(BridgeError::upstream(format!(
                    "grok ACP produced no events for {DRIVER_IDLE_SECS}s"
                )));
            }
            Ok(Err(e)) => break Err(BridgeError::upstream(format!("grok ACP read failed: {e}"))),
            Ok(Ok(None)) => {
                break Err(BridgeError::upstream(
                    "grok ACP stream closed before the prompt completed",
                ));
            }
            Ok(Ok(Some(line))) => line,
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };

        // Incoming request from the agent. We DENY every ACP permission prompt:
        // the caller's tools run through the ephemeral MCP server (which needs no
        // ACP permission), so any `session/request_permission` here is the CLI
        // asking to run a *built-in* shell/filesystem/network action on the host.
        // Auto-approving those would let a compromised or adversarial model
        // execute arbitrary host actions, so we fail closed.
        if let (Some(req_id), Some(method)) = (value.get("id"), value.get("method")) {
            if method == "session/request_permission" {
                let outcome = deny_permission_outcome(value.pointer("/params/options"));
                let _ = send_rpc(
                    &mut stdin,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": { "outcome": outcome },
                    }),
                )
                .await;
            }
            continue;
        }

        // Prompt completion.
        if value.get("id").and_then(|v| v.as_u64()) == Some(ACP_PROMPT_ID) {
            if let Some(err) = value.get("error") {
                break Err(BridgeError::upstream(format!(
                    "grok ACP prompt failed: {err}"
                )));
            }
            let result = value.get("result").cloned().unwrap_or_default();
            let stop_reason = result
                .get("stopReason")
                .and_then(|v| v.as_str())
                .unwrap_or("stop")
                .to_string();
            let mut usage = BridgeUsage::default();
            if let Some(meta) = result.get("_meta") {
                if let Some(m) = meta.get("modelId").and_then(|v| v.as_str()) {
                    model = m.to_string();
                }
                usage.input_tokens = extract_tokens(meta, &["inputTokens", "input_tokens"]);
                usage.output_tokens = extract_tokens(meta, &["outputTokens", "output_tokens"]);
                if usage.total() == 0 {
                    usage.input_tokens = extract_tokens(meta, &["totalTokens", "total_tokens"]);
                }
            }
            break Ok(CompletedTurn {
                text: text.trim().to_string(),
                model,
                usage,
                stop_reason,
            });
        }

        // Assistant text chunks (both standard and vendor-prefixed envelopes).
        let update = match value.get("method").and_then(|v| v.as_str()) {
            Some("session/update") | Some("_x.ai/session_notification") => {
                value.pointer("/params/update").cloned()
            }
            _ => None,
        };
        if let Some(update) = update {
            if update.get("sessionUpdate").and_then(|v| v.as_str()) == Some("agent_message_chunk") {
                if let Some(chunk) = update.pointer("/content/text").and_then(|v| v.as_str()) {
                    text.push_str(chunk);
                }
            }
        }
    };
    let _ = done_tx.send(result);
}

/// A live Grok conversation: owns the child, the ephemeral MCP server, the
/// stdout driver task, and any currently-suspended tool invocations.
pub struct AcpConversation {
    session_id: String,
    model: String,
    child: Child,
    cancel: CancellationToken,
    driver_handle: Option<JoinHandle<()>>,
    mcp_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
    tool_rx: mpsc::UnboundedReceiver<InboundToolCall>,
    done_rx: oneshot::Receiver<DriverResult>,
    /// provider_call_id → the suspended MCP handler awaiting its result.
    pending: HashMap<String, oneshot::Sender<String>>,
}

impl AcpConversation {
    /// Await the next turn boundary: either Grok invoked caller tools (surface
    /// them) or the turn completed.
    async fn await_outcome(&mut self) -> Result<TurnOutcome, BridgeError> {
        tokio::select! {
            biased;
            done = &mut self.done_rx => {
                match done {
                    Ok(Ok(turn)) => Ok(TurnOutcome::Completed(turn)),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(BridgeError::upstream(
                        "grok ACP driver terminated without a result",
                    )),
                }
            }
            first = self.tool_rx.recv() => {
                match first {
                    Some(call) => {
                        let batch = self.collect_batch(call).await;
                        Ok(self.surface_tool_calls(batch))
                    }
                    None => {
                        // MCP server gone before any completion — fall to done.
                        match (&mut self.done_rx).await {
                            Ok(Ok(turn)) => Ok(TurnOutcome::Completed(turn)),
                            Ok(Err(e)) => Err(e),
                            Err(_) => Err(BridgeError::upstream(
                                "grok bridge closed before the turn completed",
                            )),
                        }
                    }
                }
            }
        }
    }

    /// Gather any additional concurrent tool calls within the settle window.
    async fn collect_batch(&mut self, first: InboundToolCall) -> Vec<InboundToolCall> {
        let mut calls = vec![first];
        let settle = Duration::from_millis(TOOL_BATCH_SETTLE_MS);
        while let Ok(Some(call)) = tokio::time::timeout(settle, self.tool_rx.recv()).await {
            calls.push(call);
        }
        calls
    }

    /// Mint provider ids, park the suspended responders, and build the outcome.
    fn surface_tool_calls(&mut self, calls: Vec<InboundToolCall>) -> TurnOutcome {
        let mut requested = Vec::with_capacity(calls.len());
        for call in calls {
            let provider_call_id = format!("acp-{}", uuid::Uuid::new_v4().simple());
            self.pending
                .insert(provider_call_id.clone(), call.responder);
            requested.push(RequestedToolCall {
                provider_call_id,
                name: call.name,
                arguments: call.arguments,
            });
        }
        TurnOutcome::ToolCalls {
            model: self.model.clone(),
            calls: requested,
        }
    }
}

#[async_trait]
impl BridgeConversation for AcpConversation {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn resume(
        &mut self,
        results: Vec<ResolvedToolResult>,
    ) -> Result<TurnOutcome, BridgeError> {
        for result in results {
            match self.pending.remove(&result.provider_call_id) {
                Some(responder) => {
                    // Unblock the suspended MCP handler; a receive error here
                    // just means Grok already tore that call down.
                    let _ = responder.send(result.content);
                }
                None => {
                    return Err(BridgeError::session_state(format!(
                        "no suspended Grok tool call for provider id '{}'",
                        result.provider_call_id
                    )));
                }
            }
        }
        self.await_outcome().await
    }

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        // Dropping the parked responders errors out any still-suspended MCP
        // handlers so Grok's tool call fails closed rather than hanging.
        self.pending.clear();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        if let Some(handle) = self.driver_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.mcp_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle.abort();
        }
    }
}

/// Safety net for every drop path (e.g. a parked session evicted on TTL sweep,
/// or a panic): cancel the token and abort the background tasks so no ephemeral
/// MCP server or driver task can outlive its conversation. The child carries
/// `kill_on_drop(true)`, so dropping it here reaps the process. This makes
/// teardown deterministic even when async `shutdown` was never awaited.
impl Drop for AcpConversation {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.driver_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.mcp_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_handle.take() {
            handle.abort();
        }
    }
}

/// Opens live Grok conversations mediated through an ephemeral MCP server.
pub struct AcpMcpBackend {
    working_dir: PathBuf,
}

impl AcpMcpBackend {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl BridgeBackend for AcpMcpBackend {
    async fn open(
        &self,
        input: PromptInput,
    ) -> Result<(Box<dyn BridgeConversation>, TurnOutcome), BridgeError> {
        let cancel = CancellationToken::new();

        // 1. Start the ephemeral MCP server before the ACP handshake — the CLI
        //    connects to it (initialize / tools/list) during `session/new`.
        let (tool_tx, tool_rx) = mpsc::unbounded_channel::<InboundToolCall>();
        let mcp_bearer_value = format!("Bearer {}", uuid::Uuid::new_v4().simple());
        let mcp_state = Arc::new(McpState {
            tools: input.tools_mcp.clone(),
            tool_tx,
            bearer_value: mcp_bearer_value.clone(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| {
                BridgeError::upstream(format!("failed to bind ephemeral MCP server: {e}"))
            })?;
        let addr = listener
            .local_addr()
            .map_err(|e| BridgeError::upstream(format!("MCP server has no local addr: {e}")))?;
        let app = Router::new()
            .route("/", post(mcp_handler))
            .with_state(mcp_state);
        let mcp_cancel = cancel.clone();
        let mcp_handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move { mcp_cancel.cancelled().await })
                .await;
        });
        let mcp_guard = StartupTaskGuard::new(cancel.clone(), mcp_handle);
        let mcp_url = format!("http://{addr}/");

        // 2. Spawn `grok agent stdio` on the host with non-interactive auth.
        let env = grok_auth_env(&self.working_dir)?;
        let mut command = tokio::process::Command::new(grok_cli_path());
        command
            .arg("agent")
            .arg("stdio")
            .current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| {
            BridgeError::upstream(format!("failed to spawn `grok agent stdio`: {e}"))
        })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| BridgeError::upstream("failed to capture grok stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BridgeError::upstream("failed to capture grok stdout"))?;
        let mut lines = BufReader::new(stdout).lines();

        // Drain stderr continuously so a full pipe buffer can never deadlock the
        // CLI mid-handshake. The handle is aborted on shutdown/drop.
        let stderr_handle = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(drain_stderr(stderr)));

        // 3. ACP handshake. Any failure here tears the child down and reports a
        //    truthful infrastructure error (never a fabricated turn).
        let handshake = async {
            send_rpc(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": ACP_INIT_ID,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": 1,
                        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } }
                    }
                }),
            )
            .await?;
            await_response(&mut lines, ACP_INIT_ID).await?;

            let cwd = self.working_dir.to_string_lossy().to_string();
            // The ephemeral caller-tool MCP server is offered here. The HTTP
            // descriptor shape below is the ACP `http` MCP-server variant; the
            // precise schema a given CLI build accepts is the operator-verified
            // deployment gate this whole route is flagged behind.
            send_rpc(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": ACP_SESSION_NEW_ID,
                    "method": "session/new",
                    "params": {
                        "cwd": cwd,
                        "mcpServers": [http_mcp_server_descriptor(&mcp_url, &mcp_bearer_value)]
                    }
                }),
            )
            .await?;
            let resp = await_response(&mut lines, ACP_SESSION_NEW_ID).await?;
            let session_id = resp
                .pointer("/result/sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    BridgeError::upstream("grok ACP session/new returned no sessionId")
                })?
                .to_string();

            send_rpc(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": ACP_SET_MODEL_ID,
                    "method": "session/set_model",
                    "params": { "sessionId": session_id, "modelId": input.model.clone().unwrap_or_else(|| "grok-4.5".to_string()) }
                }),
            )
            .await?;
            // A set_model rejection is fatal: we must not silently answer on a
            // different model than the caller routed to.
            await_response(&mut lines, ACP_SET_MODEL_ID).await?;

            send_rpc(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": ACP_PROMPT_ID,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": session_id,
                        "prompt": [{ "type": "text", "text": input.prompt }]
                    }
                }),
            )
            .await?;
            Ok::<String, BridgeError>(session_id)
        }
        .await;

        let session_id = match handshake {
            Ok(sid) => sid,
            Err(e) => {
                cancel.cancel();
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let Some(handle) = stderr_handle {
                    handle.abort();
                }
                return Err(e);
            }
        };

        // 4. Hand stdin + stdout to the driver task; it resolves the turn while
        //    tool calls (if any) flow in over the MCP server.
        let model = input.model.unwrap_or_else(|| "grok-4.5".to_string());
        let (done_tx, done_rx) = oneshot::channel();
        let driver_handle = tokio::spawn(run_driver(
            stdin,
            lines,
            model.clone(),
            cancel.clone(),
            done_tx,
        ));

        let mut conversation = AcpConversation {
            session_id,
            model,
            child,
            cancel,
            driver_handle: Some(driver_handle),
            mcp_handle: Some(mcp_guard.into_handle()),
            stderr_handle,
            tool_rx,
            done_rx,
            pending: HashMap::new(),
        };
        let outcome = conversation.await_outcome().await?;
        Ok((Box::new(conversation), outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_mcp_descriptor_includes_session_authorization() {
        let descriptor = http_mcp_server_descriptor("http://127.0.0.1:1234/", "Bearer test-secret");
        assert_eq!(descriptor["type"], "http");
        assert_eq!(descriptor["url"], "http://127.0.0.1:1234/");
        assert_eq!(
            descriptor["headers"],
            serde_json::json!([{
                "name": "Authorization",
                "value": "Bearer test-secret",
            }])
        );
    }

    #[tokio::test]
    async fn mcp_endpoint_rejects_missing_or_wrong_session_authorization() {
        let (tool_tx, _tool_rx) = mpsc::unbounded_channel();
        let state = Arc::new(McpState {
            tools: Vec::new(),
            tool_tx,
            bearer_value: "Bearer expected-secret".to_string(),
        });
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        });

        let missing = mcp_handler(
            State(state.clone()),
            HeaderMap::new(),
            Json(request.clone()),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let mut wrong_headers = HeaderMap::new();
        wrong_headers.insert(
            header::AUTHORIZATION,
            "Bearer wrong-secret".parse().unwrap(),
        );
        let wrong = mcp_handler(State(state.clone()), wrong_headers, Json(request.clone())).await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            header::AUTHORIZATION,
            "Bearer expected-secret".parse().unwrap(),
        );
        let valid = mcp_handler(State(state), valid_headers, Json(request)).await;
        assert_eq!(valid.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn startup_guard_aborts_server_task_on_early_return() {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            task_cancel.cancelled().await;
        });
        let abort_handle = handle.abort_handle();

        drop(StartupTaskGuard::new(cancel.clone(), handle));

        assert!(cancel.is_cancelled());
        tokio::task::yield_now().await;
        assert!(abort_handle.is_finished());
    }

    #[test]
    fn deny_permission_selects_reject_option_when_offered() {
        let options = serde_json::json!([
            { "optionId": "allow-once", "kind": "allow_once" },
            { "optionId": "reject-once", "kind": "reject_once" }
        ]);
        let outcome = deny_permission_outcome(Some(&options));
        assert_eq!(outcome["outcome"], "selected");
        // Must pick the reject option — never the allow one.
        assert_eq!(outcome["optionId"], "reject-once");
    }

    #[test]
    fn deny_permission_cancels_when_only_allow_options_exist() {
        // A malicious/compromised agent offering only "allow" options for a
        // built-in shell/fs action must still be refused: we cancel rather than
        // select any allow option.
        let options = serde_json::json!([
            { "optionId": "allow-once", "kind": "allow_once" },
            { "optionId": "allow-always", "kind": "allow_always" }
        ]);
        let outcome = deny_permission_outcome(Some(&options));
        assert_eq!(outcome["outcome"], "cancelled");
        assert!(outcome.get("optionId").is_none());
    }

    #[test]
    fn deny_permission_cancels_with_no_options() {
        assert_eq!(deny_permission_outcome(None)["outcome"], "cancelled");
        let empty = serde_json::json!([]);
        assert_eq!(
            deny_permission_outcome(Some(&empty))["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn grok_auth_env_fails_closed_without_genuine_key() {
        // No ai_providers.json under a nonexistent dir ⇒ no genuine API key ⇒
        // provider-configuration error, never an OAuth relabel.
        let err = grok_auth_env(Path::new("/nonexistent-grok-bridge-dir")).unwrap_err();
        assert!(matches!(
            err.class,
            super::super::error::BridgeErrorClass::ProviderConfiguration
        ));
    }
}
