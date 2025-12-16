//! Global control session API (interactive, queued).
//!
//! This module implements a single global "control session" that:
//! - accepts user messages at any time (queued FIFO)
//! - runs a persistent root-agent conversation sequentially
//! - streams structured events via SSE (Tool UI friendly)
//! - supports frontend/interactive tools by accepting tool results

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentContext, AgentRef};
use crate::budget::{Budget, ModelPricing};
use crate::config::Config;
use crate::llm::OpenRouterClient;
use crate::memory::MemorySystem;
use crate::task::VerificationCriteria;
use crate::tools::ToolRegistry;

use super::routes::AppState;

/// Message posted by a user to the control session.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlMessageRequest {
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlMessageResponse {
    pub id: Uuid,
    pub queued: bool,
}

/// Tool result posted by the frontend for an interactive tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlToolResultRequest {
    pub tool_call_id: String,
    pub name: String,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlRunState {
    Idle,
    Running,
    WaitingForTool,
}

impl Default for ControlRunState {
    fn default() -> Self {
        ControlRunState::Idle
    }
}

/// A structured event emitted by the control session.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Status {
        state: ControlRunState,
        queue_len: usize,
    },
    UserMessage {
        id: Uuid,
        content: String,
    },
    AssistantMessage {
        id: Uuid,
        content: String,
        success: bool,
        cost_cents: u64,
        model: Option<String>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl AgentEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            AgentEvent::Status { .. } => "status",
            AgentEvent::UserMessage { .. } => "user_message",
            AgentEvent::AssistantMessage { .. } => "assistant_message",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Error { .. } => "error",
        }
    }
}

/// Internal control commands (queued and processed by the actor).
#[derive(Debug)]
pub enum ControlCommand {
    UserMessage { id: Uuid, content: String },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
    },
    Cancel,
}

/// Shared tool hub used to await frontend tool results.
#[derive(Debug)]
pub struct FrontendToolHub {
    pending: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
}

impl FrontendToolHub {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool call that expects a frontend-provided result.
    pub async fn register(&self, tool_call_id: String) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock().await;
        pending.insert(tool_call_id, tx);
        rx
    }

    /// Resolve a pending tool call by id.
    pub async fn resolve(
        &self,
        tool_call_id: &str,
        result: serde_json::Value,
    ) -> Result<(), ()> {
        let mut pending = self.pending.lock().await;
        let Some(tx) = pending.remove(tool_call_id) else {
            return Err(());
        };
        let _ = tx.send(result);
        Ok(())
    }
}

/// Control session runtime stored in `AppState`.
#[derive(Clone)]
pub struct ControlState {
    pub cmd_tx: mpsc::Sender<ControlCommand>,
    pub events_tx: broadcast::Sender<AgentEvent>,
    pub tool_hub: Arc<FrontendToolHub>,
    pub status: Arc<RwLock<ControlStatus>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ControlStatus {
    pub state: ControlRunState,
    pub queue_len: usize,
}

async fn set_and_emit_status(
    status: &Arc<RwLock<ControlStatus>>,
    events: &broadcast::Sender<AgentEvent>,
    state: ControlRunState,
    queue_len: usize,
) {
    {
        let mut s = status.write().await;
        s.state = state;
        s.queue_len = queue_len;
    }
    let _ = events.send(AgentEvent::Status { state, queue_len });
}

/// Enqueue a user message for the global control session.
pub async fn post_message(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ControlMessageRequest>,
) -> Result<Json<ControlMessageResponse>, (StatusCode, String)> {
    let content = req.content.trim().to_string();
    if content.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "content is required".to_string()));
    }

    let id = Uuid::new_v4();
    let queued = true;
    state
        .control
        .cmd_tx
        .send(ControlCommand::UserMessage { id, content })
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;

    Ok(Json(ControlMessageResponse { id, queued }))
}

/// Submit a frontend tool result to resume the running agent.
pub async fn post_tool_result(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ControlToolResultRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.tool_call_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "tool_call_id is required".to_string(),
        ));
    }
    if req.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name is required".to_string()));
    }

    state
        .control
        .cmd_tx
        .send(ControlCommand::ToolResult {
            tool_call_id: req.tool_call_id,
            name: req.name,
            result: req.result,
        })
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Cancel the currently running control session task.
pub async fn post_cancel(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .control
        .cmd_tx
        .send(ControlCommand::Cancel)
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Stream control session events via SSE.
pub async fn stream(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let mut rx = state.control.events_tx.subscribe();

    // Emit an initial status snapshot immediately.
    let initial = state.control.status.read().await.clone();

    let stream = async_stream::stream! {
        let init_ev = Event::default()
            .event("status")
            .json_data(AgentEvent::Status { state: initial.state, queue_len: initial.queue_len })
            .unwrap();
        yield Ok(init_ev);

        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let sse = Event::default().event(ev.event_name()).json_data(&ev).unwrap();
                    yield Ok(sse);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let sse = Event::default()
                        .event("error")
                        .json_data(AgentEvent::Error { message: "event stream lagged; some events were dropped".to_string() })
                        .unwrap();
                    yield Ok(sse);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream))
}

/// Spawn the global control session actor.
pub fn spawn_control_session(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
) -> ControlState {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ControlCommand>(256);
    let (events_tx, _events_rx) = broadcast::channel::<AgentEvent>(1024);
    let tool_hub = Arc::new(FrontendToolHub::new());
    let status = Arc::new(RwLock::new(ControlStatus {
        state: ControlRunState::Idle,
        queue_len: 0,
    }));

    let state = ControlState {
        cmd_tx,
        events_tx: events_tx.clone(),
        tool_hub: Arc::clone(&tool_hub),
        status: Arc::clone(&status),
    };

    tokio::spawn(control_actor_loop(
        config,
        root_agent,
        memory,
        cmd_rx,
        events_tx,
        tool_hub,
        status,
    ));

    state
}

async fn control_actor_loop(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    mut cmd_rx: mpsc::Receiver<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
) {
    let mut queue: VecDeque<(Uuid, String)> = VecDeque::new();
    let mut history: Vec<(String, String)> = Vec::new(); // (role, content) pairs (user/assistant)
    let pricing = Arc::new(ModelPricing::new());

    let mut running: Option<tokio::task::JoinHandle<(Uuid, String, crate::agents::AgentResult)>> = None;
    let mut running_cancel: Option<CancellationToken> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    ControlCommand::UserMessage { id, content } => {
                        queue.push_back((id, content));
                        set_and_emit_status(
                            &status,
                            &events_tx,
                            if running.is_some() { ControlRunState::Running } else { ControlRunState::Idle },
                            queue.len(),
                        ).await;
                        if running.is_none() {
                            if let Some((mid, msg)) = queue.pop_front() {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                                let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone() });
                                let cfg = config.clone();
                                let agent = Arc::clone(&root_agent);
                                let mem = memory.clone();
                                let events = events_tx.clone();
                                let tools_hub = Arc::clone(&tool_hub);
                                let status_ref = Arc::clone(&status);
                                let cancel = CancellationToken::new();
                                let pricing = Arc::clone(&pricing);
                                let hist_snapshot = history.clone();
                                running_cancel = Some(cancel.clone());
                                running = Some(tokio::spawn(async move {
                                    let result = run_single_control_turn(
                                        cfg,
                                        agent,
                                        mem,
                                        pricing,
                                        events,
                                        tools_hub,
                                        status_ref,
                                        cancel,
                                        hist_snapshot,
                                        msg.clone(),
                                    )
                                    .await;
                                    (mid, msg, result)
                                }));
                            } else {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0).await;
                            }
                        }
                    }
                    ControlCommand::ToolResult { tool_call_id, name, result } => {
                        // Deliver to the tool hub. The executor emits ToolResult events when it receives it.
                        if tool_hub.resolve(&tool_call_id, result).await.is_err() {
                            let _ = events_tx.send(AgentEvent::Error { message: format!("Unknown tool_call_id '{}' for tool '{}'", tool_call_id, name) });
                        }
                    }
                    ControlCommand::Cancel => {
                        if let Some(token) = &running_cancel {
                            token.cancel();
                            let _ = events_tx.send(AgentEvent::Error { message: "Cancellation requested".to_string() });
                        } else {
                            let _ = events_tx.send(AgentEvent::Error { message: "No running task to cancel".to_string() });
                        }
                    }
                }
            }
            finished = async {
                match &mut running {
                    Some(handle) => Some(handle.await),
                    None => None
                }
            }, if running.is_some() => {
                if let Some(res) = finished {
                    running = None;
                    running_cancel = None;
                    match res {
                        Ok((_mid, user_msg, agent_result)) => {
                            // Append to conversation history.
                            history.push(("user".to_string(), user_msg));
                            history.push(("assistant".to_string(), agent_result.output.clone()));

                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: Uuid::new_v4(),
                                content: agent_result.output.clone(),
                                success: agent_result.success,
                                cost_cents: agent_result.cost_cents,
                                model: agent_result.model_used,
                            });
                        }
                        Err(e) => {
                            let _ = events_tx.send(AgentEvent::Error { message: format!("Control session task join failed: {}", e) });
                        }
                    }
                }

                // Start next queued message, if any.
                if let Some((mid, msg)) = queue.pop_front() {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                    let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone() });
                    let cfg = config.clone();
                    let agent = Arc::clone(&root_agent);
                    let mem = memory.clone();
                    let events = events_tx.clone();
                    let tools_hub = Arc::clone(&tool_hub);
                    let status_ref = Arc::clone(&status);
                    let cancel = CancellationToken::new();
                    let pricing = Arc::clone(&pricing);
                    let hist_snapshot = history.clone();
                    running_cancel = Some(cancel.clone());
                    running = Some(tokio::spawn(async move {
                        let result = run_single_control_turn(
                            cfg,
                            agent,
                            mem,
                            pricing,
                            events,
                            tools_hub,
                            status_ref,
                            cancel,
                            hist_snapshot,
                            msg.clone(),
                        )
                        .await;
                        (mid, msg, result)
                    }));
                } else {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0).await;
                }
            }
        }
    }
}

async fn run_single_control_turn(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    pricing: Arc<ModelPricing>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
) -> crate::agents::AgentResult {
    // Build a task prompt that includes lightweight conversation context.
    let mut convo = String::new();
    if !history.is_empty() {
        convo.push_str("Conversation so far:\n");
        for (role, content) in &history {
            convo.push_str(&format!("{}: {}\n", role, content));
        }
        convo.push('\n');
    }
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- You may use tools to gather information or make changes.\n- When appropriate, use Tool UI tools (ui_*) for structured output or to ask for user selections.\n");

    let budget = Budget::new(1000);
    let verification = VerificationCriteria::None;
    let mut task = match crate::task::Task::new(convo, verification, budget) {
        Ok(t) => t,
        Err(e) => {
            let r = crate::agents::AgentResult::failure(format!("Failed to create task: {}", e), 0);
            return r;
        }
    };

    // Context for agent execution.
    let llm = Arc::new(OpenRouterClient::new(config.api_key.clone()));
    let tools = ToolRegistry::new();
    let mut ctx = AgentContext::with_memory(
        config.clone(),
        llm,
        tools,
        pricing,
        config.working_dir.clone(),
        memory,
    );
    ctx.control_events = Some(events_tx);
    ctx.frontend_tool_hub = Some(tool_hub);
    ctx.control_status = Some(status);
    ctx.cancel_token = Some(cancel);

    let result = root_agent.execute(&mut task, &ctx).await;
    result
}

