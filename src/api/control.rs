//! Global control session API (interactive, queued).
//!
//! This module implements a single global "control session" that:
//! - accepts user messages at any time (queued FIFO)
//! - runs a persistent root-agent conversation sequentially
//! - streams structured events via SSE (Tool UI friendly)
//! - supports frontend/interactive tools by accepting tool results
//! - supports persistent missions (goal-oriented sessions)

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
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
use crate::memory::{MissionMessage, MemorySystem};
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
    /// Agent thinking/reasoning (streaming)
    Thinking {
        /// Incremental thinking content
        content: String,
        /// Whether this is the final thinking chunk
        done: bool,
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
    /// Mission status changed (by agent or user)
    MissionStatusChanged {
        mission_id: Uuid,
        status: MissionStatus,
        summary: Option<String>,
    },
}

impl AgentEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            AgentEvent::Status { .. } => "status",
            AgentEvent::UserMessage { .. } => "user_message",
            AgentEvent::AssistantMessage { .. } => "assistant_message",
            AgentEvent::Thinking { .. } => "thinking",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Error { .. } => "error",
            AgentEvent::MissionStatusChanged { .. } => "mission_status_changed",
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
    /// Load a mission (switch to it)
    LoadMission { id: Uuid, respond: oneshot::Sender<Result<Mission, String>> },
    /// Create a new mission
    CreateMission { respond: oneshot::Sender<Result<Mission, String>> },
    /// Update mission status
    SetMissionStatus { id: Uuid, status: MissionStatus, respond: oneshot::Sender<Result<(), String>> },
}

// ==================== Mission Types ====================

/// Mission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissionStatus {
    Active,
    Completed,
    Failed,
}

impl std::fmt::Display for MissionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// A mission (persistent goal-oriented session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: Uuid,
    pub status: MissionStatus,
    pub title: Option<String>,
    pub history: Vec<MissionHistoryEntry>,
    pub created_at: String,
    pub updated_at: String,
}

/// A single entry in the mission history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionHistoryEntry {
    pub role: String,
    pub content: String,
}

/// Request to set mission status.
#[derive(Debug, Clone, Deserialize)]
pub struct SetMissionStatusRequest {
    pub status: MissionStatus,
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
    /// Current mission ID (if any)
    pub current_mission: Arc<RwLock<Option<Uuid>>>,
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

// ==================== Mission Endpoints ====================

/// List all missions.
pub async fn list_missions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Mission>>, (StatusCode, String)> {
    let mem = state.memory.as_ref().ok_or_else(|| {
        (StatusCode::SERVICE_UNAVAILABLE, "Memory not configured".to_string())
    })?;
    
    let db_missions = mem.supabase.list_missions(50, 0).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    
    let missions: Vec<Mission> = db_missions.into_iter().map(|m| {
        let history: Vec<MissionHistoryEntry> = serde_json::from_value(m.history.clone())
            .unwrap_or_default();
        let status = match m.status.as_str() {
            "completed" => MissionStatus::Completed,
            "failed" => MissionStatus::Failed,
            _ => MissionStatus::Active,
        };
        Mission {
            id: m.id,
            status,
            title: m.title,
            history,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }).collect();
    
    Ok(Json(missions))
}

/// Get a specific mission.
pub async fn get_mission(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let mem = state.memory.as_ref().ok_or_else(|| {
        (StatusCode::SERVICE_UNAVAILABLE, "Memory not configured".to_string())
    })?;
    
    let db_mission = mem.supabase.get_mission(id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Mission {} not found", id)))?;
    
    let history: Vec<MissionHistoryEntry> = serde_json::from_value(db_mission.history.clone())
        .unwrap_or_default();
    let status = match db_mission.status.as_str() {
        "completed" => MissionStatus::Completed,
        "failed" => MissionStatus::Failed,
        _ => MissionStatus::Active,
    };
    
    Ok(Json(Mission {
        id: db_mission.id,
        status,
        title: db_mission.title,
        history,
        created_at: db_mission.created_at,
        updated_at: db_mission.updated_at,
    }))
}

/// Create a new mission and switch to it.
pub async fn create_mission(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state.control.cmd_tx
        .send(ControlCommand::CreateMission { respond: tx })
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;
    
    rx.await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to receive response".to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load/switch to a mission.
pub async fn load_mission(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state.control.cmd_tx
        .send(ControlCommand::LoadMission { id, respond: tx })
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;
    
    rx.await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to receive response".to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Set mission status (completed/failed).
pub async fn set_mission_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMissionStatusRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state.control.cmd_tx
        .send(ControlCommand::SetMissionStatus { id, status: req.status, respond: tx })
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "control session unavailable".to_string()))?;
    
    rx.await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to receive response".to_string()))?
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get the current mission (if any).
pub async fn get_current_mission(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Option<Mission>>, (StatusCode, String)> {
    let current_id = state.control.current_mission.read().await.clone();
    
    match current_id {
        Some(id) => {
            let mem = state.memory.as_ref().ok_or_else(|| {
                (StatusCode::SERVICE_UNAVAILABLE, "Memory not configured".to_string())
            })?;
            
            let db_mission = mem.supabase.get_mission(id).await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            
            match db_mission {
                Some(m) => {
                    let history: Vec<MissionHistoryEntry> = serde_json::from_value(m.history.clone())
                        .unwrap_or_default();
                    let status = match m.status.as_str() {
                        "completed" => MissionStatus::Completed,
                        "failed" => MissionStatus::Failed,
                        _ => MissionStatus::Active,
                    };
                    Ok(Json(Some(Mission {
                        id: m.id,
                        status,
                        title: m.title,
                        history,
                        created_at: m.created_at,
                        updated_at: m.updated_at,
                    })))
                }
                None => Ok(Json(None)),
            }
        }
        None => Ok(Json(None)),
    }
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

        // Keepalive interval to prevent connection timeouts during long LLM calls
        let mut keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
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
                _ = keepalive_interval.tick() => {
                    // Send SSE comment as keepalive (: comment\n\n)
                    let sse = Event::default().comment("keepalive");
                    yield Ok(sse);
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive")
    ))
}

/// Spawn the global control session actor.
pub fn spawn_control_session(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
) -> ControlState {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ControlCommand>(256);
    let (events_tx, _events_rx) = broadcast::channel::<AgentEvent>(1024);
    let tool_hub = Arc::new(FrontendToolHub::new());
    let status = Arc::new(RwLock::new(ControlStatus {
        state: ControlRunState::Idle,
        queue_len: 0,
    }));
    let current_mission = Arc::new(RwLock::new(None));
    
    // Channel for agent-initiated mission control commands
    let (mission_cmd_tx, mission_cmd_rx) = mpsc::channel::<crate::tools::mission::MissionControlCommand>(64);

    let state = ControlState {
        cmd_tx,
        events_tx: events_tx.clone(),
        tool_hub: Arc::clone(&tool_hub),
        status: Arc::clone(&status),
        current_mission: Arc::clone(&current_mission),
    };

    tokio::spawn(control_actor_loop(
        config,
        root_agent,
        memory,
        benchmarks,
        cmd_rx,
        mission_cmd_rx,
        mission_cmd_tx,
        events_tx,
        tool_hub,
        status,
        current_mission,
    ));

    state
}

async fn control_actor_loop(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
    mut cmd_rx: mpsc::Receiver<ControlCommand>,
    mut mission_cmd_rx: mpsc::Receiver<crate::tools::mission::MissionControlCommand>,
    mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    current_mission: Arc<RwLock<Option<Uuid>>>,
) {
    let mut queue: VecDeque<(Uuid, String)> = VecDeque::new();
    let mut history: Vec<(String, String)> = Vec::new(); // (role, content) pairs (user/assistant)
    let pricing = Arc::new(ModelPricing::new());

    let mut running: Option<tokio::task::JoinHandle<(Uuid, String, crate::agents::AgentResult)>> = None;
    let mut running_cancel: Option<CancellationToken> = None;

    // Helper to persist history to current mission
    async fn persist_mission_history(
        memory: &Option<MemorySystem>,
        current_mission: &Arc<RwLock<Option<Uuid>>>,
        history: &[(String, String)],
    ) {
        let mission_id = current_mission.read().await.clone();
        if let (Some(mem), Some(mid)) = (memory, mission_id) {
            let messages: Vec<MissionMessage> = history.iter()
                .map(|(role, content)| MissionMessage {
                    role: role.clone(),
                    content: content.clone(),
                })
                .collect();
            if let Err(e) = mem.supabase.update_mission_history(mid, &messages).await {
                tracing::warn!("Failed to persist mission history: {}", e);
            }
            
            // Update title from first user message if not set
            if history.len() == 2 {
                if let Some((role, content)) = history.first() {
                    if role == "user" {
                        let title = if content.len() > 100 {
                            format!("{}...", &content[..100])
                        } else {
                            content.clone()
                        };
                        if let Err(e) = mem.supabase.update_mission_title(mid, &title).await {
                            tracing::warn!("Failed to update mission title: {}", e);
                        }
                    }
                }
            }
        }
    }

    // Helper to load a mission and return a Mission struct
    async fn load_mission_from_db(
        memory: &Option<MemorySystem>,
        id: Uuid,
    ) -> Result<Mission, String> {
        let mem = memory.as_ref().ok_or("Memory not configured")?;
        let db_mission = mem.supabase.get_mission(id).await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Mission {} not found", id))?;
        
        let history: Vec<MissionHistoryEntry> = serde_json::from_value(db_mission.history.clone())
            .unwrap_or_default();
        let status = match db_mission.status.as_str() {
            "completed" => MissionStatus::Completed,
            "failed" => MissionStatus::Failed,
            _ => MissionStatus::Active,
        };
        
        Ok(Mission {
            id: db_mission.id,
            status,
            title: db_mission.title,
            history,
            created_at: db_mission.created_at,
            updated_at: db_mission.updated_at,
        })
    }

    // Helper to create a new mission
    async fn create_new_mission(
        memory: &Option<MemorySystem>,
    ) -> Result<Mission, String> {
        let mem = memory.as_ref().ok_or("Memory not configured")?;
        let db_mission = mem.supabase.create_mission(None).await
            .map_err(|e| e.to_string())?;
        
        Ok(Mission {
            id: db_mission.id,
            status: MissionStatus::Active,
            title: db_mission.title,
            history: vec![],
            created_at: db_mission.created_at,
            updated_at: db_mission.updated_at,
        })
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    ControlCommand::UserMessage { id, content } => {
                        // Auto-create mission on first message if none exists
                        {
                            let mission_id = current_mission.read().await.clone();
                            if mission_id.is_none() {
                                if let Ok(new_mission) = create_new_mission(&memory).await {
                                    *current_mission.write().await = Some(new_mission.id);
                                    tracing::info!("Auto-created mission: {}", new_mission.id);
                                }
                            }
                        }
                        
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
                                let bench = Arc::clone(&benchmarks);
                                let events = events_tx.clone();
                                let tools_hub = Arc::clone(&tool_hub);
                                let status_ref = Arc::clone(&status);
                                let cancel = CancellationToken::new();
                                let pricing = Arc::clone(&pricing);
                                let hist_snapshot = history.clone();
                                let mission_ctrl = crate::tools::mission::MissionControl {
                                    current_mission_id: Arc::clone(&current_mission),
                                    cmd_tx: mission_cmd_tx.clone(),
                                };
                                running_cancel = Some(cancel.clone());
                                running = Some(tokio::spawn(async move {
                                    let result = run_single_control_turn(
                                        cfg,
                                        agent,
                                        mem,
                                        bench,
                                        pricing,
                                        events,
                                        tools_hub,
                                        status_ref,
                                        cancel,
                                        hist_snapshot,
                                        msg.clone(),
                                        Some(mission_ctrl),
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
                    ControlCommand::LoadMission { id, respond } => {
                        // First persist current mission history
                        persist_mission_history(&memory, &current_mission, &history).await;
                        
                        // Load the new mission
                        match load_mission_from_db(&memory, id).await {
                            Ok(mission) => {
                                // Update history from loaded mission
                                history = mission.history.iter()
                                    .map(|e| (e.role.clone(), e.content.clone()))
                                    .collect();
                                *current_mission.write().await = Some(id);
                                let _ = respond.send(Ok(mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::CreateMission { respond } => {
                        // First persist current mission history
                        persist_mission_history(&memory, &current_mission, &history).await;
                        
                        // Create a new mission
                        match create_new_mission(&memory).await {
                            Ok(mission) => {
                                history.clear();
                                *current_mission.write().await = Some(mission.id);
                                let _ = respond.send(Ok(mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::SetMissionStatus { id, status: new_status, respond } => {
                        if let Some(mem) = &memory {
                            let result = mem.supabase.update_mission_status(id, &new_status.to_string()).await
                                .map_err(|e| e.to_string());
                            if result.is_ok() {
                                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                    mission_id: id,
                                    status: new_status,
                                    summary: None,
                                });
                            }
                            let _ = respond.send(result);
                        } else {
                            let _ = respond.send(Err("Memory not configured".to_string()));
                        }
                    }
                }
            }
            // Handle agent-initiated mission status changes (from complete_mission tool)
            mission_cmd = mission_cmd_rx.recv() => {
                if let Some(cmd) = mission_cmd {
                    match cmd {
                        crate::tools::mission::MissionControlCommand::SetStatus { status, summary } => {
                            let mission_id = current_mission.read().await.clone();
                            if let Some(id) = mission_id {
                                let new_status = match status {
                                    crate::tools::mission::MissionStatusValue::Completed => MissionStatus::Completed,
                                    crate::tools::mission::MissionStatusValue::Failed => MissionStatus::Failed,
                                };
                                if let Some(mem) = &memory {
                                    if let Ok(()) = mem.supabase.update_mission_status(id, &new_status.to_string()).await {
                                        let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                            mission_id: id,
                                            status: new_status,
                                            summary,
                                        });
                                        tracing::info!("Mission {} marked as {} by agent", id, new_status);
                                    }
                                }
                            }
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

                            // Persist to mission
                            persist_mission_history(&memory, &current_mission, &history).await;

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
                    let bench = Arc::clone(&benchmarks);
                    let events = events_tx.clone();
                    let tools_hub = Arc::clone(&tool_hub);
                    let status_ref = Arc::clone(&status);
                    let cancel = CancellationToken::new();
                    let pricing = Arc::clone(&pricing);
                    let hist_snapshot = history.clone();
                    let mission_ctrl = crate::tools::mission::MissionControl {
                        current_mission_id: Arc::clone(&current_mission),
                        cmd_tx: mission_cmd_tx.clone(),
                    };
                    running_cancel = Some(cancel.clone());
                    running = Some(tokio::spawn(async move {
                        let result = run_single_control_turn(
                            cfg,
                            agent,
                            mem,
                            bench,
                            pricing,
                            events,
                            tools_hub,
                            status_ref,
                            cancel,
                            hist_snapshot,
                            msg.clone(),
                            Some(mission_ctrl),
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

/// Truncate a string to a maximum character count, adding an ellipsis if truncated.
fn truncate_message(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        content.to_string()
    } else {
        format!("{}... [truncated, {} chars total]", &content[..max_chars], content.len())
    }
}

/// Build a conversation context from history with size limits.
/// 
/// This prevents context overflow by:
/// 1. Limiting total history to last N messages
/// 2. Truncating individual messages that are too long
/// 3. Capping total context size
fn build_conversation_context(history: &[(String, String)], max_messages: usize, max_message_chars: usize, max_total_chars: usize) -> String {
    let mut context = String::new();
    
    if history.is_empty() {
        return context;
    }
    
    // Take only the most recent messages
    let start_idx = history.len().saturating_sub(max_messages);
    let recent_history = &history[start_idx..];
    
    // Build context with truncation
    context.push_str("Conversation so far:\n");
    
    for (role, content) in recent_history {
        let truncated_content = truncate_message(content, max_message_chars);
        let entry = format!("{}: {}\n", role, truncated_content);
        
        // Check if adding this would exceed total limit
        if context.len() + entry.len() > max_total_chars {
            context.push_str("... [earlier messages omitted due to size limits]\n");
            break;
        }
        
        context.push_str(&entry);
    }
    
    context.push('\n');
    context
}

async fn run_single_control_turn(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
    pricing: Arc<ModelPricing>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    mission_control: Option<crate::tools::mission::MissionControl>,
) -> crate::agents::AgentResult {
    // Build a task prompt that includes conversation context with size limits.
    // This prevents context overflow when history gets large.
    const MAX_HISTORY_MESSAGES: usize = 10;      // Keep only last 10 messages
    const MAX_MESSAGE_CHARS: usize = 5000;       // Truncate individual messages to 5K chars
    const MAX_TOTAL_CONTEXT_CHARS: usize = 30000; // Cap total context at 30K chars
    
    let history_context = build_conversation_context(
        &history, 
        MAX_HISTORY_MESSAGES, 
        MAX_MESSAGE_CHARS, 
        MAX_TOTAL_CONTEXT_CHARS
    );
    
    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- You may use tools to gather information or make changes.\n- When appropriate, use Tool UI tools (ui_*) for structured output or to ask for user selections.\n- For large data processing tasks (>10KB), use run_command to execute Python scripts rather than processing inline.\n- When you have fully completed the user's goal or determined it cannot be completed, use the complete_mission tool to mark the mission status.\n");

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
    let tools = ToolRegistry::with_mission_control(mission_control.clone());
    let mut ctx = AgentContext::with_memory(
        config.clone(),
        llm,
        tools,
        pricing,
        config.working_dir.clone(),
        memory,
    );
    ctx.mission_control = mission_control;
    ctx.control_events = Some(events_tx);
    ctx.frontend_tool_hub = Some(tool_hub);
    ctx.control_status = Some(status);
    ctx.cancel_token = Some(cancel);
    ctx.benchmarks = Some(benchmarks);

    let result = root_agent.execute(&mut task, &ctx).await;
    result
}

