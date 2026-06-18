pub mod app_server;
pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::CodexConfig;

/// Codex backend that spawns the Codex CLI for mission execution.
pub struct CodexBackend {
    id: String,
    name: String,
    config: Arc<RwLock<CodexConfig>>,
    workspace_exec: Option<crate::workspace_exec::WorkspaceExec>,
}

impl CodexBackend {
    pub fn new() -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(CodexConfig::default())),
            workspace_exec: None,
        }
    }

    pub fn with_config(config: CodexConfig) -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: None,
        }
    }

    pub fn with_config_and_workspace(
        config: CodexConfig,
        workspace_exec: crate::workspace_exec::WorkspaceExec,
    ) -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: Some(workspace_exec),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: CodexConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> CodexConfig {
        self.config.read().await.clone()
    }
}

impl Default for CodexBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn cli_names(&self) -> &'static [&'static str] {
        &["codex"]
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Codex doesn't have separate agent types like Claude Code
        // Return a single general-purpose agent
        Ok(vec![AgentInfo {
            id: "default".to_string(),
            name: "Codex Agent".to_string(),
        }])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        // Codex's app-server protocol creates the actual session id when the
        // client calls `thread/start`. We only need a local handle here so
        // mission_runner can correlate ExecutionEvents to the originating
        // mission; uuid is sufficient.
        Ok(Session {
            id: uuid::Uuid::new_v4().to_string(),
            directory: config.directory,
            model: config.model,
            agent: config.agent,
        })
    }

    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
        // All codex missions go through app-server now (Path A). The legacy
        // `codex exec` branch was removed because it can't parse slash
        // commands, never arms goals.rs, and the new path covers both
        // regular and goal missions.
        let config = self.config.read().await.clone();
        send_message_streaming_app_server(config, session, message, self.workspace_exec.as_ref())
            .await
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (testable without spawning codex)
// ---------------------------------------------------------------------------

/// Strip a leading `/goal ` prefix from a user message.
///
/// Returns `(is_goal_mission, payload)`. Single-pass via `strip_prefix`
/// rather than greedy `trim_start_matches`, so a literal `/goal ` inside
/// the objective survives. Leading whitespace before `/goal ` is tolerated.
fn parse_goal_prefix(message: &str) -> (bool, String) {
    let trimmed = message.trim_start();
    match trimmed.strip_prefix("/goal ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, message.to_string()),
    }
}

/// Mirror exec-mode's model resolution at `client.rs:98`: prefer the
/// per-session override, fall back to the operator-configured default,
/// `None` if both are unset (codex picks its own default).
fn resolve_model(session_model: Option<&str>, default_model: Option<&str>) -> Option<String> {
    session_model
        .map(|s| s.to_string())
        .or_else(|| default_model.map(|s| s.to_string()))
}

/// Fold a codex delta into a per-item buffer.
///
/// All codex `*/delta` notifications (`item/agentMessage/delta`,
/// `item/reasoning/textDelta`, `item/reasoning/summaryTextDelta`) carry
/// incremental token deltas on current CLI builds (verified on 0.128.0:
/// a ~500-char summary arrives as ~94 token-sized deltas). Append by
/// default, but absorb two snapshot-shaped payloads some builds emit
/// through the same methods:
/// - prefix-extension (delta starts with the buffer) → replace, so a full
///   text-so-far resend doesn't duplicate;
/// - echo (buffer starts with the delta) → drop, so a shorter resend
///   doesn't shrink or duplicate.
///
/// `summaryTextDelta` was previously treated as a cumulative snapshot
/// (wholesale replacement per delta). On 0.128.0 that collapsed every
/// reasoning summary to whichever single token chunk was longest — the
/// Thoughts panel showed/stored 35-char fragments (mission 2e8af5f7,
/// 87 identical truncated thinking events).
fn fold_delta_into(buffer: &mut String, delta: &str) {
    if !buffer.is_empty() && delta.starts_with(buffer.as_str()) {
        buffer.clear();
        buffer.push_str(delta);
    } else if !buffer.starts_with(delta) {
        buffer.push_str(delta);
    }
}

// ---------------------------------------------------------------------------
// App-server mode driver (Path A)
// ---------------------------------------------------------------------------

/// Drives a single mission turn via `codex app-server`. Mirrors the exec-mode
/// `send_message_streaming` contract: returns a receiver of ExecutionEvents and
/// a JoinHandle that resolves when the turn (or the goal loop) reaches a
/// terminal state.
///
/// Goal vs non-goal routing:
/// - Message starts with `/goal ` → strip the prefix and call
///   `thread/goal/set` instead of `turn/start`. Codex auto-starts a turn and
///   keeps looping until the model invokes `update_goal { status: "complete" }`
///   (or the optional token budget is hit). We finish the mission when we see
///   a `thread/goal/updated` notification with terminal status.
/// - Otherwise → `turn/start` with a single text input item. We finish the
///   mission on the first `turn/completed` notification.
async fn send_message_streaming_app_server(
    cfg: client::CodexConfig,
    session: &Session,
    message: &str,
    workspace_exec: Option<&crate::workspace_exec::WorkspaceExec>,
) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
    use app_server::{
        AppServerConfig, AppServerSession, GoalSetParams, InboundMessage, ThreadStartParams,
        TurnStartParams, UserInputItem,
    };

    // Note: codex app-server does NOT honor `OPENAI_API_KEY`/`OPENAI_OAUTH_TOKEN`
    // env vars (per `app-server/src/lib.rs:646-647`). For ChatGPT OAuth
    // rotation we use app-server's external token mode instead: the backend
    // supplies an access token at startup and answers refresh requests under
    // our per-account lock.
    let app_cfg = AppServerConfig {
        cli_path: cfg.cli_path.clone(),
        enabled_features: vec!["goals".to_string()],
        default_model: cfg.default_model.clone(),
        model_effort: cfg.model_effort.clone(),
        env: cfg.extra_env.clone(),
    };
    // Keep an owned clone of the spawn config so the driver task can
    // re-spawn the codex process via `thread/resume` if the stdio
    // stream closes mid-mission. The original `app_cfg` is consumed by
    // the first spawn below.
    let app_cfg_for_reconnect = app_cfg.clone();

    let session_arc = AppServerSession::spawn(app_cfg, &session.directory, workspace_exec).await?;
    let session_arc = Arc::new(session_arc);

    // Initialize handshake — without `experimentalApi: true`, every
    // thread/goal/* RPC is rejected.
    if let Err(e) = session_arc
        .initialize("sandboxed-sh", env!("CARGO_PKG_VERSION"))
        .await
    {
        let _ = session_arc.shutdown().await;
        return Err(anyhow::anyhow!("codex app-server initialize failed: {}", e));
    }

    if let Some(external_auth) = cfg.external_chatgpt_auth.as_ref() {
        if let Err(e) = session_arc
            .login_chatgpt_auth_tokens(
                &external_auth.access_token,
                &external_auth.chatgpt_account_id,
                external_auth.chatgpt_plan_type.as_deref(),
            )
            .await
        {
            let _ = session_arc.shutdown().await;
            return Err(anyhow::anyhow!(
                "codex account/login/start chatgptAuthTokens failed: {}",
                e
            ));
        }
        tracing::info!(
            chatgpt_account_id = %external_auth.chatgpt_account_id,
            "Configured Codex app-server external ChatGPT OAuth tokens"
        );
    }
    // Best-effort `notifications/initialized` — codex tolerates clients that
    // skip this but it matches the LSP-style handshake.
    let _ = session_arc.send_initialized_notification().await;

    // Resolve the model the same way exec mode does (`client.rs` ~L98):
    // per-session override wins, otherwise fall back to the operator-configured
    // default. Without this fallback, an operator who sets `default_model`
    // through the codex backend config would silently get codex's built-in
    // default in app-server mode.
    let resolved_model = resolve_model(session.model.as_deref(), cfg.default_model.as_deref());
    let thread_cwd = workspace_exec
        .map(|exec| exec.translate_path_for_container(std::path::Path::new(&session.directory)))
        .unwrap_or_else(|| session.directory.clone());
    let thread_start_params = ThreadStartParams {
        model: resolved_model,
        cwd: Some(thread_cwd),
        reasoning_effort: cfg.model_effort.clone(),
        ephemeral: None,
        // Match exec-mode's `--dangerously-bypass-approvals-and-sandbox`.
        // Without these, codex defaults to `on-request` + `read-only`, which
        // means every shell command pings us for an elicitation and writes
        // outside cwd are rejected — wrong for missions that already run
        // inside per-mission systemd-nspawn containers.
        approval_policy: Some("never".to_string()),
        sandbox: Some("danger-full-access".to_string()),
    };
    let thread = match session_arc.thread_start(thread_start_params).await {
        Ok(t) => t.thread,
        Err(e) => {
            let _ = session_arc.shutdown().await;
            return Err(anyhow::anyhow!("codex thread/start failed: {}", e));
        }
    };

    let (tx, rx) = mpsc::channel::<ExecutionEvent>(256);

    // Take the inbound channel before issuing any further RPC — `goal/set`
    // and `turn/start` start emitting notifications before they return.
    let inbound = match session_arc.take_inbound().await {
        Some(rx) => rx,
        None => {
            let _ = session_arc.shutdown().await;
            return Err(anyhow::anyhow!(
                "codex app-server inbound stream already taken"
            ));
        }
    };

    // Detect /goal prefix server-side. Dashboard does this too, but the
    // backend is the trust boundary — easier to enforce here than rely on
    // every client.
    let (is_goal_mission, user_payload) = parse_goal_prefix(message);

    let thread_id = thread.id.clone();
    let session_for_rpc = Arc::clone(&session_arc);

    // Issue the priming RPC. For goal missions, codex auto-starts the first
    // turn after `goal/set`; for non-goal, we explicitly send `turn/start`.
    if is_goal_mission {
        if user_payload.is_empty() {
            let _ = session_arc.shutdown().await;
            return Err(anyhow::anyhow!(
                "/goal requires an objective — got empty string"
            ));
        }
        // The goals db (`~/.codex/goals_1.sqlite`) is shared by every
        // app-server in the container, and a freshly spawned server can
        // receive goal/set while a sibling is still running the sqlx
        // migrations — surfacing as a transient "no such table:
        // thread_goals". Retry briefly before giving up (observed live:
        // the migration completes within seconds).
        let mut goal_set_result: anyhow::Result<serde_json::Value> = Ok(serde_json::Value::Null);
        for attempt in 1..=3u32 {
            goal_set_result = session_for_rpc
                .goal_set(GoalSetParams {
                    thread_id: thread_id.clone(),
                    objective: user_payload.clone(),
                    token_budget: None,
                })
                .await;
            match &goal_set_result {
                Err(e) if attempt < 3 && e.to_string().contains("no such table") => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "thread/goal/set hit a goals-db migration race; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(1500 * attempt as u64))
                        .await;
                }
                _ => break,
            }
        }
        if let Err(e) = goal_set_result {
            let _ = session_arc.shutdown().await;
            return Err(anyhow::anyhow!("codex thread/goal/set failed: {}", e));
        }
    } else if let Err(e) = session_for_rpc
        .turn_start(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInputItem::Text {
                text: user_payload.clone(),
            }],
        })
        .await
    {
        let _ = session_arc.shutdown().await;
        return Err(anyhow::anyhow!("codex turn/start failed: {}", e));
    }

    let session_id = session.id.clone();
    let initial_objective = if is_goal_mission {
        user_payload.clone()
    } else {
        String::new()
    };
    // State the driver task needs in order to re-spawn the codex
    // app-server process if it crashes mid-mission. Owned clones (not
    // borrowed refs) so the spawned task is `'static`.
    let reconnect_app_cfg = app_cfg_for_reconnect;
    let reconnect_cwd = session.directory.clone();
    let reconnect_workspace_exec = workspace_exec.cloned();
    let reconnect_thread_id = thread.id.clone();

    let handle = tokio::spawn(async move {
        // Seed the cached objective so the first GoalIteration event has
        // it before `thread/goal/updated` arrives. Cleared when not a
        // goal mission (no iteration counters fire then anyway).
        let mut translator = AppServerEventTranslator {
            goal_objective: initial_objective,
            ..Default::default()
        };
        let mut terminal = false;
        let mut stream_closed_unexpectedly = false;

        // Mutable so we can swap in a fresh session after a reconnect.
        let mut session_arc = session_arc;
        let mut inbound = inbound;
        let cancel_token = cfg.cancel_token.clone();

        // Cap the number of automatic reconnects per mission so a
        // systemic codex crash doesn't loop forever. One retry covers
        // the common case (transient OOM kill, network blip) without
        // hiding a persistent failure.
        const MAX_RECONNECTS: u32 = 1;
        let mut reconnect_attempts: u32 = 0;

        'outer: loop {
            loop {
                let msg = tokio::select! {
                    _ = async {
                        if let Some(token) = cancel_token.as_ref() {
                            token.cancelled().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        tracing::info!(
                            thread_id = %thread_id,
                            is_goal_mission,
                            "codex app-server cancellation requested"
                        );
                        if is_goal_mission {
                            if let Err(e) = session_arc.goal_clear(&thread_id).await {
                                tracing::warn!(
                                    thread_id = %thread_id,
                                    "codex thread/goal/clear failed during cancellation: {}",
                                    e
                                );
                            }
                            let _ = tx
                                .send(ExecutionEvent::GoalStatus {
                                    status: "cleared".to_string(),
                                    objective: translator.goal_objective.clone(),
                                })
                                .await;
                        } else if let Err(e) = session_arc.turn_interrupt(&thread_id, None).await {
                            tracing::debug!(
                                thread_id = %thread_id,
                                "codex turn/interrupt failed during cancellation: {}",
                                e
                            );
                        }
                        let _ = tx.send(ExecutionEvent::Cancelled).await;
                        break 'outer;
                    }
                    msg = inbound.recv() => match msg {
                        Some(m) => m,
                        None => break, // inner loop → check whether to reconnect
                    },
                };

                match msg {
                    InboundMessage::Notification { method, params } => {
                        let outcome =
                            translator.handle_notification(&method, &params, is_goal_mission);
                        for ev in outcome.events {
                            if tx.send(ev).await.is_err() {
                                terminal = true;
                                break;
                            }
                        }
                        if outcome.terminal {
                            terminal = true;
                        }
                    }
                    InboundMessage::ServerRequest { id, method, params } => {
                        // Codex elicits permission for command exec, file change,
                        // and dynamic-tool invocations through server-initiated
                        // requests. Exec mode runs with
                        // `--dangerously-bypass-approvals-and-sandbox`; we mirror
                        // that policy here by auto-approving every elicitation.
                        let send_err = if method == "account/chatgptAuthTokens/refresh" {
                            match cfg.external_chatgpt_auth.as_ref() {
                                Some(external_auth) => {
                                    let previous_account_id = params
                                        .get("previousAccountId")
                                        .and_then(|value| value.as_str());
                                    match crate::api::ai_providers::refresh_codex_oauth_account_for_app_server(
                                        &external_auth.working_dir,
                                        previous_account_id,
                                        Some(&external_auth.chatgpt_account_id),
                                    )
                                    .await
                                    {
                                        Ok(account) => {
                                            let result = serde_json::json!({
                                                "accessToken": account.access_token,
                                                "chatgptAccountId": account.chatgpt_account_id,
                                                "chatgptPlanType": external_auth.chatgpt_plan_type,
                                            });
                                            session_arc.respond_to_server_request(id, result).await
                                        }
                                        Err(error) => {
                                            session_arc
                                                .respond_to_server_request_error(
                                                    id,
                                                    -32603,
                                                    &format!(
                                                        "sandboxed-sh: ChatGPT OAuth refresh failed: {}",
                                                        error
                                                    ),
                                                )
                                                .await
                                        }
                                    }
                                }
                                None => {
                                    session_arc
                                        .respond_to_server_request_error(
                                            id,
                                            -32603,
                                            "sandboxed-sh: auth refresh requested without external ChatGPT OAuth context",
                                        )
                                        .await
                                }
                            }
                        } else {
                            let result = elicitation_auto_approve(&method);
                            session_arc.respond_to_server_request(id, result).await
                        };
                        if let Err(e) = send_err {
                            debug!("failed to respond to server request {}: {}", method, e);
                        }
                    }
                }

                if terminal {
                    break 'outer;
                }
            }

            // Inbound closed. If we already terminated, fine — exit.
            // Otherwise codex crashed mid-mission; attempt one reconnect
            // via `thread/resume` before giving up.
            if terminal {
                break 'outer;
            }
            if reconnect_attempts >= MAX_RECONNECTS {
                stream_closed_unexpectedly = true;
                break 'outer;
            }
            reconnect_attempts += 1;

            tracing::warn!(
                "codex app-server stream closed mid-mission; attempting thread/resume (attempt {})",
                reconnect_attempts
            );
            let _ = session_arc.shutdown().await;

            let new_session = match app_server::AppServerSession::spawn(
                reconnect_app_cfg.clone(),
                &reconnect_cwd,
                reconnect_workspace_exec.as_ref(),
            )
            .await
            {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::error!("codex app-server reconnect: spawn failed: {}", e);
                    stream_closed_unexpectedly = true;
                    break 'outer;
                }
            };
            if let Err(e) = new_session
                .initialize("sandboxed-sh", env!("CARGO_PKG_VERSION"))
                .await
            {
                tracing::error!("codex app-server reconnect: initialize failed: {}", e);
                stream_closed_unexpectedly = true;
                break 'outer;
            }
            let _ = new_session.send_initialized_notification().await;
            if let Err(e) = new_session.thread_resume(&reconnect_thread_id).await {
                tracing::error!("codex app-server reconnect: thread/resume failed: {}", e);
                stream_closed_unexpectedly = true;
                break 'outer;
            }
            let new_inbound = match new_session.take_inbound().await {
                Some(rx) => rx,
                None => {
                    tracing::error!("codex app-server reconnect: inbound stream missing");
                    stream_closed_unexpectedly = true;
                    break 'outer;
                }
            };
            session_arc = new_session;
            inbound = new_inbound;
            tracing::info!("codex app-server reconnected via thread/resume");
        }

        if stream_closed_unexpectedly {
            let _ = tx
                .send(ExecutionEvent::Error {
                    message: "codex app-server stream closed before mission terminated".to_string(),
                })
                .await;
        }

        let _ = tx
            .send(ExecutionEvent::MessageComplete {
                session_id: session_id.clone(),
            })
            .await;

        let _ = session_arc.shutdown().await;
    });

    Ok((rx, handle))
}

/// Auto-approve any server-initiated elicitation. Matches exec-mode's
/// `--dangerously-bypass-approvals-and-sandbox` posture. Specific elicitations
/// expect different result shapes; cover the common ones explicitly and fall
/// back to a generic `{decision:"approve"}` for anything else.
///
/// Note: `account/chatgptAuthTokens/refresh` is handled separately at the
/// caller (it needs a JSON-RPC error response, not a result payload).
fn elicitation_auto_approve(method: &str) -> serde_json::Value {
    use serde_json::json;
    match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval" => json!({ "decision": "approve" }),
        _ => json!({ "decision": "approve" }),
    }
}

/// Translates codex app-server notifications into ExecutionEvents and detects
/// terminal state for the mission.
#[derive(Default)]
struct AppServerEventTranslator {
    /// Keep track of which item ids we've already emitted text for, so
    /// repeated `item/agentMessage/delta` events don't duplicate text into
    /// the mission stream beyond what each delta carries.
    delta_buffers: std::collections::HashMap<String, String>,
    /// True once we've emitted a synthetic Usage event for the current turn,
    /// so we don't double-count when codex sends both turn-level and
    /// thread-level token deltas.
    emitted_usage_for_turn: std::collections::HashSet<String>,
    /// 1-based iteration counter for goal missions — incremented on every
    /// `turn/started` we observe while the goal is active. Surfaces as
    /// `GoalIteration` ExecutionEvents that the UI renders as a pill.
    goal_iteration: u32,
    /// Mirror of the goal objective so iteration markers can carry it
    /// without re-parsing every notification. Set when the driver issues
    /// `thread/goal/set`.
    goal_objective: String,
    /// Set of turn ids we've already counted as iterations, so repeated
    /// `turn/started` for the same turn (codex re-emits on resume) doesn't
    /// double-count.
    counted_turn_ids: std::collections::HashSet<String>,
    /// True while a goal-mode turn is still active. A goal can transition to
    /// `complete` before the current turn emits its final assistant message;
    /// ending immediately on the goal update drops that closing response.
    goal_turn_active: bool,
    /// Set after a terminal goal update (`complete` / `budgetLimited`). The
    /// stream becomes terminal once the active turn completes.
    goal_terminal_seen: bool,
}

struct TranslateOutcome {
    events: Vec<ExecutionEvent>,
    terminal: bool,
}

fn usage_tokens(usage: &serde_json::Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(|v| v.as_u64()))
        .unwrap_or(0)
}

fn codex_usage_from_turn_params(params: &serde_json::Value) -> Option<(u64, u64)> {
    let turn = params.get("turn");
    let usage = turn
        .and_then(|turn| turn.get("tokenUsage").or_else(|| turn.get("usage")))
        .or_else(|| params.get("tokenUsage"))
        .or_else(|| params.get("usage"))?;

    let input = usage_tokens(
        usage,
        &[
            "inputTokens",
            "input_tokens",
            "promptTokens",
            "prompt_tokens",
            "totalInputTokens",
            "total_input_tokens",
        ],
    );
    let output = usage_tokens(
        usage,
        &[
            "outputTokens",
            "output_tokens",
            "completionTokens",
            "completion_tokens",
            "totalOutputTokens",
            "total_output_tokens",
        ],
    );
    (input > 0 || output > 0).then_some((input, output))
}

impl AppServerEventTranslator {
    fn handle_notification(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        is_goal_mission: bool,
    ) -> TranslateOutcome {
        let mut events = Vec::new();
        let mut terminal = false;

        match method {
            // ----- Streaming text & reasoning -----
            //
            // mission_runner expects TextDelta and Thinking content to be
            // **per-item snapshots** (full text so far for that item), not
            // raw incremental chunks — exec-mode's `emit_text_snapshot`
            // already followed that contract. Codex's `delta` notifications
            // carry incremental pieces; we accumulate them per item id and
            // emit a snapshot of the running buffer.
            "item/agentMessage/delta" => {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    let item_id = params
                        .get("itemId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("__anon")
                        .to_string();
                    let entry = self.delta_buffers.entry(item_id).or_default();
                    // Agent-message deltas are incremental tokens.
                    fold_delta_into(entry, delta);
                    events.push(ExecutionEvent::TextDelta {
                        content: entry.clone(),
                    });
                }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    let item_id = params
                        .get("itemId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("__anon_reasoning")
                        .to_string();
                    // Both reasoning sub-streams carry incremental token
                    // deltas on current CLI builds (0.128.0 verified for
                    // `summaryTextDelta`; treating it as a cumulative
                    // snapshot collapsed summaries to single token chunks).
                    // They keep separate buffer keys so one stream can't
                    // contaminate the other.
                    let kind = if method == "item/reasoning/summaryTextDelta" {
                        "summary"
                    } else {
                        "reasoning"
                    };
                    let key = format!("{}:{}", kind, item_id);
                    let entry = self.delta_buffers.entry(key.clone()).or_default();
                    fold_delta_into(entry, delta);
                    events.push(ExecutionEvent::Thinking {
                        content: entry.clone(),
                        item_id: Some(key),
                    });
                }
            }

            // ----- Item lifecycle (tool calls, command execution) -----
            "item/started" | "item/completed" => {
                if let Some(item) = params.get("item") {
                    let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    match kind {
                        "toolCall" | "tool_call" | "functionCall" | "function_call" => {
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown_tool")
                                .to_string();
                            if method == "item/started" {
                                let args = item
                                    .get("arguments")
                                    .or_else(|| item.get("args"))
                                    .or_else(|| item.get("input"))
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                events.push(ExecutionEvent::ToolCall { id, name, args });
                            } else {
                                let result = item
                                    .get("result")
                                    .or_else(|| item.get("output"))
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                events.push(ExecutionEvent::ToolResult { id, name, result });
                            }
                        }
                        "commandExecution" => {
                            // Bash-like commands. Surface as a synthetic
                            // tool call named "bash" to match the exec-mode
                            // legacy translator's convention.
                            let command = item
                                .get("command")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            if method == "item/started" {
                                events.push(ExecutionEvent::ToolCall {
                                    id,
                                    name: "bash".to_string(),
                                    args: serde_json::json!({ "command": command }),
                                });
                            } else {
                                let result = item
                                    .get("aggregatedOutput")
                                    .or_else(|| item.get("output"))
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                events.push(ExecutionEvent::ToolResult {
                                    id,
                                    name: "bash".to_string(),
                                    result,
                                });
                            }
                        }
                        // Other item types (assistantMessage, userMessage, etc.)
                        // are surfaced through delta events; nothing to do here.
                        _ => {}
                    }
                }
            }

            // ----- Turn lifecycle -----
            "turn/completed" => {
                if let Some(turn) = params.get("turn") {
                    let turn_id = turn
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !turn_id.is_empty() && !self.emitted_usage_for_turn.contains(&turn_id) {
                        if let Some((input_tokens, output_tokens)) =
                            codex_usage_from_turn_params(params)
                        {
                            events.push(ExecutionEvent::Usage {
                                input_tokens,
                                output_tokens,
                            });
                        }
                        self.emitted_usage_for_turn.insert(turn_id);
                    }

                    let status = turn.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    match status {
                        // `failed` is unconditional — a turn that hard-errors
                        // takes the mission down regardless of goal mode,
                        // because codex's continuation loop won't restart
                        // a failed turn either way.
                        "failed" => {
                            let msg = turn
                                .get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("turn failed");
                            events.push(ExecutionEvent::Error {
                                message: msg.to_string(),
                            });
                            terminal = true;
                        }
                        // For goal missions, turn completion is terminal only
                        // after we have already seen a terminal goal status.
                        // `thread/goal/updated {status: complete}` can arrive
                        // before the current turn emits its closing assistant
                        // message; waiting for turn completion keeps that
                        // final response in the stream.
                        // For non-goal missions, the mission ends when its
                        // single turn ends (whether interrupted or completed).
                        "interrupted" | "completed" if !is_goal_mission => {
                            terminal = true;
                        }
                        "interrupted" | "completed" if is_goal_mission => {
                            self.goal_turn_active = false;
                            if self.goal_terminal_seen {
                                terminal = true;
                            }
                        }
                        _ => {}
                    }
                }
            }

            // ----- Per-turn marker (goal mode iteration counter) -----
            //
            // We only count iterations for goal missions since codex's
            // continuation engine fires `turn/started` for each iteration
            // automatically. For non-goal missions there's only ever one
            // turn, so a counter would be noise.
            "turn/started" if is_goal_mission => {
                self.goal_turn_active = true;
                let turn_id = params
                    .get("turn")
                    .and_then(|t| t.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Codex can re-emit `turn/started` for the same turn after
                // a thread/resume; dedupe by id.
                if !turn_id.is_empty() && self.counted_turn_ids.insert(turn_id) {
                    self.goal_iteration = self.goal_iteration.saturating_add(1);
                    events.push(ExecutionEvent::GoalIteration {
                        iteration: self.goal_iteration,
                        objective: self.goal_objective.clone(),
                    });
                }
            }

            // ----- Goal lifecycle -----
            "thread/goal/updated" => {
                if let Some(goal) = params.get("goal") {
                    let status = goal
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Refresh our cached objective on every update — covers
                    // the initial set, pause/resume, and budget bumps.
                    if let Some(obj) = goal.get("objective").and_then(|v| v.as_str()) {
                        self.goal_objective = obj.to_string();
                    }
                    if !status.is_empty() {
                        events.push(ExecutionEvent::GoalStatus {
                            status: status.clone(),
                            objective: self.goal_objective.clone(),
                        });
                    }
                    if status == "complete" || status == "budgetLimited" {
                        self.goal_terminal_seen = true;
                        if !self.goal_turn_active {
                            terminal = true;
                        }
                    }
                }
            }
            "thread/goal/cleared" if is_goal_mission => {
                events.push(ExecutionEvent::GoalStatus {
                    status: "cleared".to_string(),
                    objective: self.goal_objective.clone(),
                });
                terminal = true;
            }

            // ----- Errors / warnings -----
            "error" => {
                if let Some(err) = params.get("error") {
                    let message = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("codex app-server error")
                        .to_string();
                    let will_retry = params
                        .get("willRetry")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    events.push(ExecutionEvent::Error {
                        message: message.clone(),
                    });
                    if !will_retry {
                        terminal = true;
                    }
                }
            }

            // Notifications we deliberately ignore: thread/started,
            // thread/status/changed, warning, remoteControl/status/changed,
            // turn/started, item/agentMessage/delta we already handled, etc.
            _ => {}
        }

        TranslateOutcome { events, terminal }
    }
}

/// Create a registry entry for the Codex backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(CodexBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn parse_goal_prefix_detects_simple_goal() {
        let (is_goal, payload) = parse_goal_prefix("/goal create file foo");
        assert!(is_goal);
        assert_eq!(payload, "create file foo");
    }

    #[test]
    fn parse_goal_prefix_handles_leading_whitespace() {
        let (is_goal, payload) = parse_goal_prefix("   /goal do the thing");
        assert!(is_goal);
        assert_eq!(payload, "do the thing");
    }

    #[test]
    fn parse_goal_prefix_preserves_inner_goal_literal() {
        // Bugbot regression: trim_start_matches would strip both prefixes.
        // strip_prefix is single-pass — the inner `/goal ` must survive.
        let (is_goal, payload) = parse_goal_prefix("/goal /goal explain why this is a bad idea");
        assert!(is_goal);
        assert_eq!(payload, "/goal explain why this is a bad idea");
    }

    #[test]
    fn parse_goal_prefix_ignores_unprefixed_messages() {
        let (is_goal, payload) = parse_goal_prefix("hello world");
        assert!(!is_goal);
        assert_eq!(payload, "hello world");
    }

    #[test]
    fn parse_goal_prefix_requires_trailing_space() {
        // Bare "/goal" without a trailing space should NOT be treated as
        // a goal — the user might mean a literal `/goal` token.
        let (is_goal, payload) = parse_goal_prefix("/goal");
        assert!(!is_goal);
        assert_eq!(payload, "/goal");
    }

    #[test]
    fn resolve_model_prefers_session_override() {
        let resolved = resolve_model(Some("gpt-5.5"), Some("gpt-4o"));
        assert_eq!(resolved.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn resolve_model_falls_back_to_default() {
        // Bugbot regression: app-server mode used to ignore default_model
        // entirely. With session override unset, the operator-configured
        // default must be picked up.
        let resolved = resolve_model(None, Some("gpt-5.5"));
        assert_eq!(resolved.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn resolve_model_returns_none_when_both_unset() {
        let resolved = resolve_model(None, None);
        assert!(resolved.is_none());
    }

    #[test]
    fn fold_delta_appends_incremental_tokens() {
        let mut buf = String::new();
        for tok in ["P", "O", "N", "G"] {
            fold_delta_into(&mut buf, tok);
        }
        assert_eq!(buf, "PONG");
    }

    #[test]
    fn fold_delta_replaces_cumulative_snapshots() {
        // Snapshot-shaped stream (each payload extends the previous one)
        // must replace, not append — older app-server builds send full
        // text-so-far through delta methods.
        let mut buf = String::new();
        for snapshot in [
            "The",
            "The targeted",
            "The targeted Lean",
            "The targeted Lean module",
        ] {
            fold_delta_into(&mut buf, snapshot);
        }
        assert_eq!(buf, "The targeted Lean module");
    }

    #[test]
    fn fold_delta_drops_echo() {
        // Pure echo of an earlier prefix should not shrink the buffer.
        let mut buf = String::from("The targeted Lean");
        fold_delta_into(&mut buf, "The targeted");
        assert_eq!(buf, "The targeted Lean");
    }

    #[test]
    fn fold_delta_idempotent_on_repeated_snapshot() {
        let mut buf = String::new();
        fold_delta_into(&mut buf, "stable");
        fold_delta_into(&mut buf, "stable");
        assert_eq!(buf, "stable");
    }

    #[test]
    fn fold_delta_accumulates_summary_token_deltas() {
        // Regression for the truncated-thoughts bug (mission 2e8af5f7):
        // `item/reasoning/summaryTextDelta` on CLI 0.128.0 streams token
        // deltas; cumulative-snapshot replacement collapsed the summary to
        // its longest single chunk. Token deltas must accumulate.
        let mut buf = String::new();
        for tok in [
            "**Solving the riddle carefully**\n\nI",
            "'m",
            " working",
            " through",
            " the",
            " sheep",
            " puzzle.",
        ] {
            fold_delta_into(&mut buf, tok);
        }
        assert_eq!(
            buf,
            "**Solving the riddle carefully**\n\nI'm working through the sheep puzzle."
        );
    }

    #[test]
    fn fold_delta_still_appends_true_token_deltas() {
        let mut buf = String::new();
        fold_delta_into(&mut buf, "Hello ");
        fold_delta_into(&mut buf, "world");
        assert_eq!(buf, "Hello world");
    }

    #[test]
    fn reasoning_thinking_events_carry_distinct_item_ids() {
        // Regression for mission dbc8a7e9 seq 6651: the translator must
        // tag each Thinking event with the per-item buffer key so the
        // mission_runner can detect when codex moves to a new reasoning
        // item and finalize the previous thought instead of concatenating
        // unrelated cumulative snapshots into one buffer.
        let mut translator = AppServerEventTranslator {
            delta_buffers: HashMap::new(),
            emitted_usage_for_turn: HashSet::new(),
            counted_turn_ids: HashSet::new(),
            goal_iteration: 0,
            goal_objective: String::new(),
            goal_turn_active: false,
            goal_terminal_seen: false,
        };

        let notify = |item_id: &str, delta: &str| {
            json!({
                "itemId": item_id,
                "delta": delta,
            })
        };

        let collect = |events: Vec<ExecutionEvent>| -> Vec<(Option<String>, String)> {
            events
                .into_iter()
                .filter_map(|e| match e {
                    ExecutionEvent::Thinking { content, item_id } => Some((item_id, content)),
                    _ => None,
                })
                .collect()
        };

        // Item A: cumulative summary snapshots.
        let mut events = Vec::new();
        events.extend(
            translator
                .handle_notification(
                    "item/reasoning/summaryTextDelta",
                    &notify("A", "The recovery log."),
                    false,
                )
                .events,
        );
        events.extend(
            translator
                .handle_notification(
                    "item/reasoning/summaryTextDelta",
                    &notify("A", "The recovery log. Do not duplicate."),
                    false,
                )
                .events,
        );
        // Item B: brand new reasoning, starts at "Work".
        events.extend(
            translator
                .handle_notification(
                    "item/reasoning/summaryTextDelta",
                    &notify("B", "Work"),
                    false,
                )
                .events,
        );
        events.extend(
            translator
                .handle_notification(
                    "item/reasoning/summaryTextDelta",
                    &notify("B", "Worktrees are ready"),
                    false,
                )
                .events,
        );

        let thinkings = collect(events);
        assert_eq!(thinkings.len(), 4);
        // Item A's snapshots share an item_id.
        assert_eq!(thinkings[0].0, Some("summary:A".to_string()));
        assert_eq!(thinkings[1].0, Some("summary:A".to_string()));
        // Item B's snapshots share a *different* item_id.
        assert_eq!(thinkings[2].0, Some("summary:B".to_string()));
        assert_eq!(thinkings[3].0, Some("summary:B".to_string()));
        assert_ne!(thinkings[1].0, thinkings[2].0);
        // Per-item contents are clean cumulative snapshots (the runner is
        // free to REPLACE per item, no overlap merging required).
        assert_eq!(thinkings[1].1, "The recovery log. Do not duplicate.");
        assert_eq!(thinkings[3].1, "Worktrees are ready");
    }

    #[test]
    fn codex_turn_completed_extracts_usage_aliases() {
        let mut translator = AppServerEventTranslator {
            delta_buffers: HashMap::new(),
            emitted_usage_for_turn: HashSet::new(),
            counted_turn_ids: HashSet::new(),
            goal_iteration: 0,
            goal_objective: String::new(),
            goal_turn_active: false,
            goal_terminal_seen: false,
        };

        let outcome = translator.handle_notification(
            "turn/completed",
            &json!({
                "turn": {
                    "id": "turn-1",
                    "status": "completed"
                },
                "usage": {
                    "total_input_tokens": 1234,
                    "total_output_tokens": 56
                }
            }),
            false,
        );

        assert!(outcome.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::Usage {
                input_tokens: 1234,
                output_tokens: 56
            }
        )));
    }

    #[test]
    fn goal_complete_waits_for_active_turn_completion() {
        let mut translator = AppServerEventTranslator::default();

        let started = translator.handle_notification(
            "turn/started",
            &json!({ "turn": { "id": "turn-1" } }),
            true,
        );
        assert!(!started.terminal);

        let complete = translator.handle_notification(
            "thread/goal/updated",
            &json!({
                "goal": {
                    "status": "complete",
                    "objective": "finish the debug task"
                }
            }),
            true,
        );
        assert!(!complete.terminal);
        assert!(complete.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::GoalStatus { status, .. } if status == "complete"
        )));

        let text = translator.handle_notification(
            "item/agentMessage/delta",
            &json!({ "itemId": "msg-1", "delta": "FINAL_RESPONSE_DEBUG_OK" }),
            true,
        );
        assert!(!text.terminal);
        assert!(text.events.iter().any(|event| matches!(
            event,
            ExecutionEvent::TextDelta { content } if content == "FINAL_RESPONSE_DEBUG_OK"
        )));

        let turn_completed = translator.handle_notification(
            "turn/completed",
            &json!({ "turn": { "id": "turn-1", "status": "completed" } }),
            true,
        );
        assert!(turn_completed.terminal);
    }

    #[tokio::test]
    async fn test_list_agents() {
        let backend = CodexBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "default");
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = CodexBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                agent: None,
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.directory, "/tmp");
    }
}
