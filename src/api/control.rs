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

use crate::agents::{AgentContext, AgentRef, TerminalReason};
use crate::budget::{Budget, ModelPricing};
use crate::config::{AgentBackend, Config};
use crate::llm::OpenRouterClient;
use crate::mcp::McpRegistry;
use crate::memory::{ContextBuilder, MemorySystem, MissionMessage};
use crate::task::VerificationCriteria;
use crate::tools::ToolRegistry;

use super::routes::AppState;

/// Message posted by a user to the control session.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlMessageRequest {
    pub content: String,
    /// Optional model override for this message.
    /// If not specified, uses the server's default model.
    #[serde(default)]
    pub model: Option<String>,
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
        /// Mission this status applies to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    UserMessage {
        id: Uuid,
        content: String,
        /// Mission this message belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    AssistantMessage {
        id: Uuid,
        content: String,
        success: bool,
        cost_cents: u64,
        model: Option<String>,
        /// Mission this message belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Agent thinking/reasoning (streaming)
    Thinking {
        /// Incremental thinking content
        content: String,
        /// Whether this is the final thinking chunk
        done: bool,
        /// Mission this thinking belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        args: serde_json::Value,
        /// Mission this tool call belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
        /// Mission this result belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    Error {
        message: String,
        /// Mission this error belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Mission status changed (by agent or user)
    MissionStatusChanged {
        mission_id: Uuid,
        status: MissionStatus,
        summary: Option<String>,
    },
    /// Agent phase update (for showing preparation steps)
    AgentPhase {
        /// Phase name: "estimating_complexity", "selecting_model", "splitting_task", "executing", "verifying"
        phase: String,
        /// Optional details about what's happening
        detail: Option<String>,
        /// Agent name (for hierarchical display)
        agent: Option<String>,
        /// Mission this phase belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Agent tree update (for real-time tree visualization)
    AgentTree {
        /// The full agent tree structure
        tree: AgentTreeNode,
        /// Mission this tree belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Execution progress update (for progress indicator)
    Progress {
        /// Total number of subtasks
        total_subtasks: usize,
        /// Number of completed subtasks
        completed_subtasks: usize,
        /// Currently executing subtask description (if any)
        current_subtask: Option<String>,
        /// Current depth level (0=root, 1=subtask, 2=sub-subtask)
        depth: u8,
        /// Mission this progress belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
}

/// A node in the agent tree (for visualization)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTreeNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String, // "Root", "Node", "ComplexityEstimator", "ModelSelector", "TaskExecutor", "Verifier"
    pub name: String,
    pub description: String,
    pub status: String, // "pending", "running", "completed", "failed"
    pub budget_allocated: u64,
    pub budget_spent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub children: Vec<AgentTreeNode>,
}

impl AgentTreeNode {
    pub fn new(id: &str, node_type: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            status: "pending".to_string(),
            budget_allocated: 0,
            budget_spent: 0,
            complexity: None,
            selected_model: None,
            children: Vec::new(),
        }
    }

    pub fn with_budget(mut self, allocated: u64, spent: u64) -> Self {
        self.budget_allocated = allocated;
        self.budget_spent = spent;
        self
    }

    pub fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub fn with_complexity(mut self, complexity: f64) -> Self {
        self.complexity = Some(complexity);
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.selected_model = Some(model.to_string());
        self
    }

    pub fn add_child(&mut self, child: AgentTreeNode) {
        self.children.push(child);
    }
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
            AgentEvent::AgentPhase { .. } => "agent_phase",
            AgentEvent::AgentTree { .. } => "agent_tree",
            AgentEvent::Progress { .. } => "progress",
        }
    }
}

/// Internal control commands (queued and processed by the actor).
#[derive(Debug)]
pub enum ControlCommand {
    UserMessage {
        id: Uuid,
        content: String,
        /// Optional model override for this message
        model: Option<String>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
    },
    Cancel,
    /// Load a mission (switch to it)
    LoadMission {
        id: Uuid,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Create a new mission
    CreateMission {
        title: Option<String>,
        model_override: Option<String>,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Update mission status
    SetMissionStatus {
        id: Uuid,
        status: MissionStatus,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Start a mission in parallel (if slots available)
    StartParallel {
        mission_id: Uuid,
        content: String,
        /// Model override from API request (takes priority over DB)
        model: Option<String>,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Cancel a specific mission
    CancelMission {
        mission_id: Uuid,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// List currently running missions
    ListRunning {
        respond: oneshot::Sender<Vec<super::mission_runner::RunningMissionInfo>>,
    },
    /// Resume an interrupted mission
    ResumeMission {
        mission_id: Uuid,
        /// If true, clean the mission's work directory before resuming
        clean_workspace: bool,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Graceful shutdown - mark running missions as interrupted
    GracefulShutdown {
        respond: oneshot::Sender<Vec<Uuid>>,
    },
}

// ==================== Mission Types ====================

/// Mission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Active,
    Completed,
    Failed,
    /// Mission was interrupted (server shutdown, cancellation, etc.)
    Interrupted,
    /// Mission blocked by external factors (type mismatch, access denied, etc.)
    Blocked,
    /// Mission not feasible as specified (wrong assumptions in request)
    NotFeasible,
}

impl std::fmt::Display for MissionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Blocked => write!(f, "blocked"),
            Self::NotFeasible => write!(f, "not_feasible"),
            Self::Interrupted => write!(f, "interrupted"),
        }
    }
}

/// A mission (persistent goal-oriented session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: Uuid,
    pub status: MissionStatus,
    pub title: Option<String>,
    /// Model override requested for this mission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    pub history: Vec<MissionHistoryEntry>,
    pub created_at: String,
    pub updated_at: String,
    /// When this mission was interrupted (if status is Interrupted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
    /// Whether this mission can be resumed
    #[serde(default)]
    pub resumable: bool,
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
    pub async fn resolve(&self, tool_call_id: &str, result: serde_json::Value) -> Result<(), ()> {
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
    /// Current mission ID (if any) - primary mission in the old sequential model
    pub current_mission: Arc<RwLock<Option<Uuid>>>,
    /// Current agent tree snapshot (for refresh resilience)
    pub current_tree: Arc<RwLock<Option<AgentTreeNode>>>,
    /// Current execution progress (for progress indicator)
    pub progress: Arc<RwLock<ExecutionProgress>>,
    /// Running missions (for parallel execution)
    pub running_missions: Arc<RwLock<Vec<super::mission_runner::RunningMissionInfo>>>,
    /// Max parallel missions allowed
    pub max_parallel: usize,
}

/// Execution progress for showing "Subtask X of Y"
#[derive(Debug, Clone, Serialize, Default)]
pub struct ExecutionProgress {
    /// Total number of subtasks
    pub total_subtasks: usize,
    /// Number of completed subtasks
    pub completed_subtasks: usize,
    /// Currently executing subtask description (if any)
    pub current_subtask: Option<String>,
    /// Current depth level (0=root, 1=subtask, 2=sub-subtask)
    pub current_depth: u8,
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
    let _ = events.send(AgentEvent::Status { state, queue_len, mission_id: None });
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
        .send(ControlCommand::UserMessage {
            id,
            content,
            model: req.model,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

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
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

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
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ==================== Mission Endpoints ====================

/// List all missions.
pub async fn list_missions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Mission>>, (StatusCode, String)> {
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let db_missions = mem
        .supabase
        .list_missions(50, 0)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let missions: Vec<Mission> = db_missions
        .into_iter()
        .map(|m| {
            let history: Vec<MissionHistoryEntry> =
                serde_json::from_value(m.history.clone()).unwrap_or_default();
            let status = match m.status.as_str() {
                "completed" => MissionStatus::Completed,
                "failed" => MissionStatus::Failed,
                "interrupted" => MissionStatus::Interrupted,
                "blocked" => MissionStatus::Blocked,
                "not_feasible" => MissionStatus::NotFeasible,
                _ => MissionStatus::Active,
            };
            Mission {
                id: m.id,
                status,
                title: m.title,
                model_override: m.model_override,
                history,
                created_at: m.created_at.clone(),
                updated_at: m.updated_at.clone(),
                interrupted_at: if matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked) { Some(m.updated_at) } else { None },
                resumable: matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked),
            }
        })
        .collect();

    Ok(Json(missions))
}

/// Get a specific mission.
pub async fn get_mission(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;

    let db_mission = mem
        .supabase
        .get_mission(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Mission {} not found", id)))?;

    let history: Vec<MissionHistoryEntry> =
        serde_json::from_value(db_mission.history.clone()).unwrap_or_default();
    let status = match db_mission.status.as_str() {
        "completed" => MissionStatus::Completed,
        "failed" => MissionStatus::Failed,
        "interrupted" => MissionStatus::Interrupted,
        "blocked" => MissionStatus::Blocked,
        "not_feasible" => MissionStatus::NotFeasible,
        _ => MissionStatus::Active,
    };

    Ok(Json(Mission {
        id: db_mission.id,
        status,
        title: db_mission.title,
        model_override: db_mission.model_override,
        history,
        created_at: db_mission.created_at.clone(),
        updated_at: db_mission.updated_at.clone(),
        interrupted_at: if matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked) { Some(db_mission.updated_at) } else { None },
        resumable: matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked),
    }))
}

/// Create a new mission and switch to it.
/// Request body for creating a mission
#[derive(Debug, Deserialize)]
pub struct CreateMissionRequest {
    pub title: Option<String>,
    pub model_override: Option<String>,
}

pub async fn create_mission(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let (title, model_override) = body
        .map(|b| (b.title.clone(), b.model_override.clone()))
        .unwrap_or((None, None));

    state
        .control
        .cmd_tx
        .send(ControlCommand::CreateMission {
            title,
            model_override,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load/switch to a mission.
pub async fn load_mission(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    state
        .control
        .cmd_tx
        .send(ControlCommand::LoadMission { id, respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
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

    state
        .control
        .cmd_tx
        .send(ControlCommand::SetMissionStatus {
            id,
            status: req.status,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
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
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Memory not configured".to_string(),
                )
            })?;

            let db_mission = mem
                .supabase
                .get_mission(id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            match db_mission {
                Some(m) => {
                    let history: Vec<MissionHistoryEntry> =
                        serde_json::from_value(m.history.clone()).unwrap_or_default();
                    let status = match m.status.as_str() {
                        "completed" => MissionStatus::Completed,
                        "failed" => MissionStatus::Failed,
                        "interrupted" => MissionStatus::Interrupted,
                        _ => MissionStatus::Active,
                    };
                    Ok(Json(Some(Mission {
                        id: m.id,
                        status,
                        title: m.title,
                        model_override: m.model_override,
                        history,
                        created_at: m.created_at.clone(),
                        updated_at: m.updated_at.clone(),
                        interrupted_at: if status == MissionStatus::Interrupted { Some(m.updated_at) } else { None },
                        resumable: matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked),
                    })))
                }
                None => Ok(Json(None)),
            }
        }
        None => Ok(Json(None)),
    }
}

/// Get current agent tree snapshot (for refresh resilience).
/// Returns the last emitted tree state, or null if no tree is active.
pub async fn get_tree(State(state): State<Arc<AppState>>) -> Json<Option<AgentTreeNode>> {
    let tree = state.control.current_tree.read().await.clone();
    Json(tree)
}

/// Get tree for a specific mission.
/// For currently running mission, returns the live tree from memory.
/// For completed missions, returns the saved final_tree from the database.
pub async fn get_mission_tree(
    State(state): State<Arc<AppState>>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Option<AgentTreeNode>>, (StatusCode, String)> {
    // Check if this is the current active mission
    let current_id = state.control.current_mission.read().await.clone();
    if current_id == Some(mission_id) {
        // Return live tree from memory
        let tree = state.control.current_tree.read().await.clone();
        return Ok(Json(tree));
    }
    
    // Otherwise, fetch from database
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory not configured".to_string(),
        )
    })?;
    
    let db_mission = mem
        .supabase
        .get_mission(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    
    match db_mission {
        Some(m) => {
            // Parse final_tree from JSON if it exists
            let tree: Option<AgentTreeNode> = m.final_tree
                .and_then(|v| serde_json::from_value(v).ok());
            Ok(Json(tree))
        }
        None => Err((StatusCode::NOT_FOUND, "Mission not found".to_string())),
    }
}

/// Get current execution progress (for progress indicator).
pub async fn get_progress(State(state): State<Arc<AppState>>) -> Json<ExecutionProgress> {
    let progress = state.control.progress.read().await.clone();
    Json(progress)
}

// ==================== Parallel Mission Endpoints ====================

/// List currently running missions.
pub async fn list_running_missions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<super::mission_runner::RunningMissionInfo>>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state
        .control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    
    let running = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to receive response".to_string(),
        )
    })?;
    
    Ok(Json(running))
}

/// Request body for starting a mission in parallel.
#[derive(Debug, Deserialize)]
pub struct StartParallelRequest {
    pub content: String,
    /// Optional model override for this parallel mission
    #[serde(default)]
    pub model: Option<String>,
}

/// Start a mission in parallel (if capacity allows).
pub async fn start_mission_parallel(
    State(state): State<Arc<AppState>>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<StartParallelRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state
        .control
        .cmd_tx
        .send(ControlCommand::StartParallel {
            mission_id,
            content: req.content,
            model: req.model,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    
    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
        .map(|_| Json(serde_json::json!({ "ok": true, "mission_id": mission_id })))
        .map_err(|e| (StatusCode::CONFLICT, e))
}

/// Cancel a specific mission.
pub async fn cancel_mission(
    State(state): State<Arc<AppState>>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    
    state
        .control
        .cmd_tx
        .send(ControlCommand::CancelMission {
            mission_id,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    
    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
        .map(|_| Json(serde_json::json!({ "ok": true, "cancelled": mission_id })))
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

/// Request body for resuming a mission
#[derive(Debug, Deserialize, Default)]
pub struct ResumeMissionRequest {
    /// If true, clean the mission's work directory before resuming
    #[serde(default)]
    pub clean_workspace: bool,
}

/// Resume an interrupted mission.
/// This reconstructs context from history and work directory, then restarts execution.
pub async fn resume_mission(
    State(state): State<Arc<AppState>>,
    Path(mission_id): Path<Uuid>,
    body: Option<Json<ResumeMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let clean_workspace = body.map(|b| b.clean_workspace).unwrap_or(false);
    let (tx, rx) = oneshot::channel();
    
    state
        .control
        .cmd_tx
        .send(ControlCommand::ResumeMission {
            mission_id,
            clean_workspace,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    
    rx.await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to receive response".to_string(),
            )
        })?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Get parallel execution configuration.
pub async fn get_parallel_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Query actual running count from the control actor
    // (the running state is tracked in the actor loop, not in shared state)
    let (tx, rx) = oneshot::channel();
    state
        .control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    let running = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to get running missions".to_string(),
        )
    })?;

    Ok(Json(serde_json::json!({
        "max_parallel_missions": state.control.max_parallel,
        "running_count": running.len(),
    })))
}

/// Delete a mission by ID.
/// Only allows deleting missions that are not currently running.
pub async fn delete_mission(
    State(state): State<Arc<AppState>>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Check if mission is currently running by querying the control actor
    // (the actual running state is tracked in the actor loop, not in shared state)
    let (tx, rx) = oneshot::channel();
    state
        .control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    let running = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to check running missions".to_string(),
        )
    })?;

    if running.iter().any(|m| m.mission_id == mission_id) {
        return Err((
            StatusCode::CONFLICT,
            "Cannot delete a running mission. Cancel it first.".to_string(),
        ));
    }

    // Get memory system
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory system not available".to_string(),
        )
    })?;

    // Delete the mission
    let deleted = mem
        .supabase
        .delete_mission(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted {
        Ok(Json(serde_json::json!({
            "ok": true,
            "deleted": mission_id
        })))
    } else {
        Err((StatusCode::NOT_FOUND, "Mission not found".to_string()))
    }
}

/// Delete all empty "Untitled" missions.
/// Returns the count of deleted missions.
/// Note: This excludes any currently running missions to prevent data loss.
pub async fn cleanup_empty_missions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Get currently running mission IDs to exclude from cleanup
    // (a newly-started mission may have empty history in DB while actively running)
    let (tx, rx) = oneshot::channel();
    state
        .control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;

    let running = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to check running missions".to_string(),
        )
    })?;

    let running_ids: Vec<Uuid> = running.iter().map(|m| m.mission_id).collect();

    // Get memory system
    let mem = state.memory.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Memory system not available".to_string(),
        )
    })?;

    // Delete empty untitled missions, excluding running ones
    let count = mem
        .supabase
        .delete_empty_untitled_missions_excluding(&running_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "deleted_count": count
    })))
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
            .json_data(AgentEvent::Status { state: initial.state, queue_len: initial.queue_len, mission_id: None })
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
                                .json_data(AgentEvent::Error { message: "event stream lagged; some events were dropped".to_string(), mission_id: None })
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
            .text("keepalive"),
    ))
}

/// Spawn the global control session actor.
pub fn spawn_control_session(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
    resolver: crate::budget::SharedModelResolver,
    mcp: Arc<McpRegistry>,
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
    let (mission_cmd_tx, mission_cmd_rx) =
        mpsc::channel::<crate::tools::mission::MissionControlCommand>(64);

    let current_tree = Arc::new(RwLock::new(None));
    let progress = Arc::new(RwLock::new(ExecutionProgress::default()));
    let running_missions = Arc::new(RwLock::new(Vec::new()));
    let max_parallel = config.max_parallel_missions;

    let state = ControlState {
        cmd_tx,
        events_tx: events_tx.clone(),
        tool_hub: Arc::clone(&tool_hub),
        status: Arc::clone(&status),
        current_mission: Arc::clone(&current_mission),
        current_tree: Arc::clone(&current_tree),
        progress: Arc::clone(&progress),
        running_missions: Arc::clone(&running_missions),
        max_parallel,
    };

    // Spawn the main control actor
    tokio::spawn(control_actor_loop(
        config.clone(),
        root_agent,
        memory.clone(),
        benchmarks,
        resolver,
        mcp,
        cmd_rx,
        mission_cmd_rx,
        mission_cmd_tx,
        events_tx.clone(),
        tool_hub,
        status,
        current_mission,
        current_tree,
        progress,
    ));

    // Spawn background stale mission cleanup task (if enabled)
    if config.stale_mission_hours > 0 {
        if let Some(mem) = memory {
            tokio::spawn(stale_mission_cleanup_loop(
                mem,
                config.stale_mission_hours,
                events_tx,
            ));
        }
    }

    state
}

/// Background task that periodically closes stale missions.
async fn stale_mission_cleanup_loop(
    memory: MemorySystem,
    stale_hours: u64,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    // Check every hour
    let check_interval = std::time::Duration::from_secs(3600);
    
    tracing::info!(
        "Stale mission cleanup task started: closing missions inactive for {} hours",
        stale_hours
    );
    
    loop {
        tokio::time::sleep(check_interval).await;
        
        match memory.supabase.get_stale_active_missions(stale_hours).await {
            Ok(stale_missions) => {
                for mission in stale_missions {
                    tracing::info!(
                        "Auto-closing stale mission {}: '{}' (inactive since {})",
                        mission.id,
                        mission.title.as_deref().unwrap_or("Untitled"),
                        mission.updated_at
                    );
                    
                    if let Err(e) = memory.supabase.update_mission_status(mission.id, "completed").await {
                        tracing::warn!("Failed to auto-close stale mission {}: {}", mission.id, e);
                    } else {
                        // Notify listeners
                        let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                            mission_id: mission.id,
                            status: MissionStatus::Completed,
                            summary: Some(format!(
                                "Auto-closed after {} hours of inactivity",
                                stale_hours
                            )),
                        });
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to check for stale missions: {}", e);
            }
        }
    }
}

async fn control_actor_loop(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
    resolver: crate::budget::SharedModelResolver,
    mcp: Arc<McpRegistry>,
    mut cmd_rx: mpsc::Receiver<ControlCommand>,
    mut mission_cmd_rx: mpsc::Receiver<crate::tools::mission::MissionControlCommand>,
    mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    current_mission: Arc<RwLock<Option<Uuid>>>,
    current_tree: Arc<RwLock<Option<AgentTreeNode>>>,
    progress: Arc<RwLock<ExecutionProgress>>,
) {
    // Queue stores (id, content, model_override) for the current/primary mission
    let mut queue: VecDeque<(Uuid, String, Option<String>)> = VecDeque::new();
    let mut history: Vec<(String, String)> = Vec::new(); // (role, content) pairs (user/assistant)
    let pricing = Arc::new(ModelPricing::new());

    let mut running: Option<tokio::task::JoinHandle<(Uuid, String, crate::agents::AgentResult)>> =
        None;
    let mut running_cancel: Option<CancellationToken> = None;
    // Track which mission the main `running` task is actually working on.
    // This is different from `current_mission` which can change when user creates a new mission.
    let mut running_mission_id: Option<Uuid> = None;

    // Parallel mission runners - each runs independently
    let mut parallel_runners: std::collections::HashMap<Uuid, super::mission_runner::MissionRunner> =
        std::collections::HashMap::new();

    // Helper to extract file paths from text (for mission summaries)
    fn extract_file_paths(text: &str) -> Vec<String> {
        let mut paths = Vec::new();
        // Match common file path patterns
        for word in text.split_whitespace() {
            let word =
                word.trim_matches(|c| c == '`' || c == '\'' || c == '"' || c == ',' || c == ':');
            if (word.starts_with('/') || word.starts_with("./"))
                && word.len() > 3
                && !word.contains("http")
                && word.chars().filter(|c| *c == '/').count() >= 1
            {
                // Likely a file path
                paths.push(word.to_string());
            }
        }
        paths
    }

    // Helper to persist history to current mission
    async fn persist_mission_history(
        memory: &Option<MemorySystem>,
        current_mission: &Arc<RwLock<Option<Uuid>>>,
        history: &[(String, String)],
    ) {
        let mission_id = current_mission.read().await.clone();
        if let (Some(mem), Some(mid)) = (memory, mission_id) {
            let messages: Vec<MissionMessage> = history
                .iter()
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
                            let safe_end = crate::memory::safe_truncate_index(content, 100);
                            format!("{}...", &content[..safe_end])
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
        let db_mission = mem
            .supabase
            .get_mission(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Mission {} not found", id))?;

        let history: Vec<MissionHistoryEntry> =
            serde_json::from_value(db_mission.history.clone()).unwrap_or_default();
        let status = match db_mission.status.as_str() {
            "completed" => MissionStatus::Completed,
            "failed" => MissionStatus::Failed,
            "interrupted" => MissionStatus::Interrupted,
            "blocked" => MissionStatus::Blocked,
            "not_feasible" => MissionStatus::NotFeasible,
            _ => MissionStatus::Active,
        };

        Ok(Mission {
            id: db_mission.id,
            status,
            title: db_mission.title,
            model_override: db_mission.model_override,
            history,
            created_at: db_mission.created_at.clone(),
            updated_at: db_mission.updated_at.clone(),
            interrupted_at: if matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked) { Some(db_mission.updated_at) } else { None },
            resumable: matches!(status, MissionStatus::Interrupted | MissionStatus::Blocked),
        })
    }

    // Helper to create a new mission
    async fn create_new_mission(memory: &Option<MemorySystem>, model_override: Option<&str>) -> Result<Mission, String> {
        create_new_mission_with_title(memory, None, model_override).await
    }

    // Helper to create a new mission with title
    async fn create_new_mission_with_title(
        memory: &Option<MemorySystem>,
        title: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<Mission, String> {
        let mem = memory.as_ref().ok_or("Memory not configured")?;
        let db_mission = mem
            .supabase
            .create_mission(title, model_override)
            .await
            .map_err(|e| e.to_string())?;

        Ok(Mission {
            id: db_mission.id,
            status: MissionStatus::Active,
            title: db_mission.title,
            model_override: db_mission.model_override,
            history: vec![],
            created_at: db_mission.created_at,
            updated_at: db_mission.updated_at,
            interrupted_at: None,
            resumable: false,
        })
    }

    // Helper to build resume context for an interrupted or blocked mission
    async fn resume_mission_impl(
        memory: &Option<MemorySystem>,
        config: &Config,
        mission_id: Uuid,
        clean_workspace: bool,
    ) -> Result<(Mission, String), String> {
        let mission = load_mission_from_db(memory, mission_id).await?;
        
        // Check if mission can be resumed (interrupted or blocked)
        if !matches!(mission.status, MissionStatus::Interrupted | MissionStatus::Blocked) {
            return Err(format!(
                "Mission {} cannot be resumed (status: {})",
                mission_id, mission.status
            ));
        }
        
        // Clean workspace if requested
        let short_id = &mission_id.to_string()[..8];
        let mission_dir = config.working_dir.join("work").join(format!("mission-{}", short_id));
        
        if clean_workspace && mission_dir.exists() {
            tracing::info!("Cleaning workspace for mission {} at {:?}", mission_id, mission_dir);
            if let Err(e) = std::fs::remove_dir_all(&mission_dir) {
                tracing::warn!("Failed to clean workspace: {}", e);
            }
            // Recreate the directory
            let _ = std::fs::create_dir_all(&mission_dir);
        }
        
        // Build resume context
        let mut resume_parts = Vec::new();
        
        // Add resumption notice based on status
        let resume_reason = match mission.status {
            MissionStatus::Blocked => "reached its iteration limit",
            _ => "was interrupted",
        };
        
        let workspace_note = if clean_workspace {
            " (workspace cleaned)"
        } else {
            ""
        };
        
        if let Some(interrupted_at) = &mission.interrupted_at {
            resume_parts.push(format!(
                "**MISSION RESUMED**{}\nThis mission {} at {} and is now being continued.",
                workspace_note, resume_reason, interrupted_at
            ));
        } else {
            resume_parts.push(format!("**MISSION RESUMED**{}\nThis mission {} and is now being continued.", workspace_note, resume_reason));
        }
        
        // Add history summary
        if !mission.history.is_empty() {
            resume_parts.push("\n## Previous Conversation Summary".to_string());
            
            // Include the original user request
            if let Some(first_user) = mission.history.iter().find(|h| h.role == "user") {
                resume_parts.push(format!("\n**Original Request:**\n{}", first_user.content));
            }
            
            // Include last assistant response (what was being worked on)
            if let Some(last_assistant) = mission.history.iter().rev().find(|h| h.role == "assistant") {
                let truncated = if last_assistant.content.len() > 2000 {
                    format!("{}...", &last_assistant.content[..2000])
                } else {
                    last_assistant.content.clone()
                };
                resume_parts.push(format!("\n**Last Progress:**\n{}", truncated));
            }
        }
        
        // Scan work directory for artifacts (use mission_dir defined earlier)
        if mission_dir.exists() {
            resume_parts.push("\n## Work Directory Contents".to_string());
            
            let mut files_found = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&mission_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        // Skip common non-artifact directories
                        if dir_name == "venv" || dir_name == ".venv" || dir_name == ".open_agent" || dir_name == "temp" {
                            continue;
                        }
                        // List files in subdirectory
                        if let Ok(subentries) = std::fs::read_dir(&path) {
                            for subentry in subentries.filter_map(|e| e.ok()) {
                                let subpath = subentry.path();
                                if subpath.is_file() {
                                    let rel_path = subpath.strip_prefix(&mission_dir)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|_| subpath.display().to_string());
                                    files_found.push(rel_path);
                                }
                            }
                        }
                    } else if path.is_file() {
                        let rel_path = path.strip_prefix(&mission_dir)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| path.display().to_string());
                        files_found.push(rel_path);
                    }
                }
            }
            
            if !files_found.is_empty() {
                resume_parts.push(format!("Files created:\n{}", 
                    files_found.iter().map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n")
                ));
            } else {
                resume_parts.push("No output files created yet.".to_string());
            }
        }
        
        // Add instructions
        resume_parts.push("\n## Instructions".to_string());
        resume_parts.push(
            "Please continue from where you left off. Review the previous progress and work directory contents, \
            then continue working towards completing the original request. Do not repeat work that was already done."
                .to_string()
        );
        
        let resume_prompt = resume_parts.join("\n");
        
        Ok((mission, resume_prompt))
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    ControlCommand::UserMessage { id, content, model } => {
                        // Auto-create mission on first message if none exists
                        {
                            let mission_id = current_mission.read().await.clone();
                            if mission_id.is_none() {
                                if let Ok(new_mission) = create_new_mission(&memory, model.as_deref()).await {
                                    *current_mission.write().await = Some(new_mission.id);
                                    tracing::info!("Auto-created mission: {} (model: {:?})", new_mission.id, model);
                                }
                            }
                        }

                        // Use explicit model from message, or fall back to mission's model_override
                        let effective_model = if model.is_some() {
                            model
                        } else {
                            // Get current mission's model_override
                            let mission_id = current_mission.read().await.clone();
                            if let Some(mid) = mission_id {
                                if let Ok(mission) = load_mission_from_db(&memory, mid).await {
                                    mission.model_override
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        };

                        queue.push_back((id, content, effective_model));
                        set_and_emit_status(
                            &status,
                            &events_tx,
                            if running.is_some() { ControlRunState::Running } else { ControlRunState::Idle },
                            queue.len(),
                        ).await;
                        if running.is_none() {
                            if let Some((mid, msg, model_override)) = queue.pop_front() {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                                let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), mission_id: None });
                                let cfg = config.clone();
                                let agent = Arc::clone(&root_agent);
                                let mem = memory.clone();
                                let bench = Arc::clone(&benchmarks);
                                let res = Arc::clone(&resolver);
                                let mcp_ref = Arc::clone(&mcp);
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
                                let tree_ref = Arc::clone(&current_tree);
                                let progress_ref = Arc::clone(&progress);
                                running_cancel = Some(cancel.clone());
                                // Capture which mission this task is working on
                                running_mission_id = current_mission.read().await.clone();
                                running = Some(tokio::spawn(async move {
                                    let result = run_single_control_turn(
                                        cfg,
                                        agent,
                                        mem,
                                        bench,
                                        res,
                                        mcp_ref,
                                        pricing,
                                        events,
                                        tools_hub,
                                        status_ref,
                                        cancel,
                                        hist_snapshot,
                                        msg.clone(),
                                        model_override,
                                        Some(mission_ctrl),
                                        tree_ref,
                                        progress_ref,
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
                            let _ = events_tx.send(AgentEvent::Error { message: format!("Unknown tool_call_id '{}' for tool '{}'", tool_call_id, name), mission_id: None });
                        }
                    }
                    ControlCommand::Cancel => {
                        if let Some(token) = &running_cancel {
                            token.cancel();
                            let _ = events_tx.send(AgentEvent::Error { message: "Cancellation requested".to_string(), mission_id: None });
                        } else {
                            let _ = events_tx.send(AgentEvent::Error { message: "No running task to cancel".to_string(), mission_id: None });
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
                    ControlCommand::CreateMission { title, model_override, respond } => {
                        // First persist current mission history
                        persist_mission_history(&memory, &current_mission, &history).await;

                        // Create a new mission with optional title and model override
                        match create_new_mission_with_title(&memory, title.as_deref(), model_override.as_deref()).await {
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
                            // Save the final tree before updating status (if this is the current mission)
                            let current_id = current_mission.read().await.clone();
                            if current_id == Some(id) {
                                if let Some(tree) = current_tree.read().await.clone() {
                                    if let Ok(tree_json) = serde_json::to_value(&tree) {
                                        if let Err(e) = mem.supabase.update_mission_tree(id, &tree_json).await {
                                            tracing::warn!("Failed to save mission tree: {}", e);
                                        }
                                    }
                                }
                            }
                            
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
                    ControlCommand::StartParallel { mission_id, content, model, respond } => {
                        tracing::info!("StartParallel requested for mission {} with model {:?}", mission_id, model);

                        // Count currently running parallel missions
                        let parallel_running = parallel_runners.values().filter(|r| r.is_running()).count();
                        let main_running = if running.is_some() { 1 } else { 0 };
                        let total_running = parallel_running + main_running;
                        let max_parallel = config.max_parallel_missions;

                        if total_running >= max_parallel {
                            let _ = respond.send(Err(format!(
                                "Maximum parallel missions ({}) reached. {} running.",
                                max_parallel, total_running
                            )));
                        } else if parallel_runners.contains_key(&mission_id) {
                            let _ = respond.send(Err(format!(
                                "Mission {} is already running in parallel",
                                mission_id
                            )));
                        } else {
                            // Load mission to get DB model_override as fallback
                            let db_model = match load_mission_from_db(&memory, mission_id).await {
                                Ok(m) => m.model_override,
                                Err(e) => {
                                    let _ = respond.send(Err(format!("Failed to load mission: {}", e)));
                                    continue;
                                }
                            };
                            
                            // Request model takes priority over DB model
                            let model_override = model.or(db_model);

                            // Create a new MissionRunner
                            let mut runner = super::mission_runner::MissionRunner::new(
                                mission_id,
                                model_override.clone(),
                            );
                            
                            // Queue the initial message
                            runner.queue_message(Uuid::new_v4(), content, model_override);
                            
                            // Start execution
                            let started = runner.start_next(
                                config.clone(),
                                Arc::clone(&root_agent),
                                memory.clone(),
                                Arc::clone(&benchmarks),
                                Arc::clone(&resolver),
                                Arc::clone(&mcp),
                                Arc::clone(&pricing),
                                events_tx.clone(),
                                Arc::clone(&tool_hub),
                                Arc::clone(&status),
                                mission_cmd_tx.clone(),
                                Arc::new(RwLock::new(Some(mission_id))), // Each runner tracks its own mission
                            );
                            
                            if started {
                                tracing::info!("Mission {} started in parallel (model: {:?})", mission_id, runner.model_override);
                                parallel_runners.insert(mission_id, runner);
                                let _ = respond.send(Ok(()));
                            } else {
                                let _ = respond.send(Err("Failed to start mission execution".to_string()));
                            }
                        }
                    }
                    ControlCommand::CancelMission { mission_id, respond } => {
                        // First check parallel runners
                        if let Some(runner) = parallel_runners.get_mut(&mission_id) {
                            runner.cancel();
                            let _ = events_tx.send(AgentEvent::Error {
                                message: format!("Parallel mission {} cancelled", mission_id),
                                mission_id: Some(mission_id),
                            });
                            parallel_runners.remove(&mission_id);
                            let _ = respond.send(Ok(()));
                        } else {
                            // Check if this is the currently executing mission
                            // Use running_mission_id (the actual mission being executed)
                            // instead of current_mission (which can change when user creates a new mission)
                            if running_mission_id == Some(mission_id) {
                                // Cancel the current execution
                                if let Some(token) = &running_cancel {
                                    token.cancel();
                                    let _ = events_tx.send(AgentEvent::Error { 
                                        message: format!("Mission {} cancelled", mission_id),
                                        mission_id: Some(mission_id),
                                    });
                                    let _ = respond.send(Ok(()));
                                } else {
                                    let _ = respond.send(Err("Mission not currently executing".to_string()));
                                }
                            } else {
                                let _ = respond.send(Err(format!("Mission {} not found", mission_id)));
                            }
                        }
                    }
                    ControlCommand::ListRunning { respond } => {
                        // Return info about currently running missions
                        let mut running_list = Vec::new();

                        // Add main mission if running - use running_mission_id (the actual mission being executed)
                        // instead of current_mission (which can change when user creates a new mission)
                        if running.is_some() {
                            if let Some(mission_id) = running_mission_id {
                                running_list.push(super::mission_runner::RunningMissionInfo {
                                    mission_id,
                                    model_override: None,
                                    state: "running".to_string(),
                                    queue_len: queue.len(),
                                    history_len: history.len(),
                                    seconds_since_activity: 0, // Main runner doesn't track this yet
                                    expected_deliverables: 0,
                                });
                            }
                        }

                        // Add all parallel runners
                        for runner in parallel_runners.values() {
                            running_list.push(super::mission_runner::RunningMissionInfo::from(runner));
                        }

                        let _ = respond.send(running_list);
                    }
                    ControlCommand::ResumeMission { mission_id, clean_workspace, respond } => {
                        // Resume an interrupted mission by building resume context
                        match resume_mission_impl(&memory, &config, mission_id, clean_workspace).await {
                            Ok((mission, resume_prompt)) => {
                                // First persist current mission history (if any)
                                persist_mission_history(&memory, &current_mission, &history).await;
                                
                                // Load the mission's history into current state
                                history = mission.history.iter()
                                    .map(|e| (e.role.clone(), e.content.clone()))
                                    .collect();
                                *current_mission.write().await = Some(mission_id);

                                // Update mission status back to active
                                if let Some(mem) = &memory {
                                    let _ = mem.supabase.update_mission_status(mission_id, "active").await;
                                }
                                
                                // Queue the resume prompt as a message
                                let msg_id = Uuid::new_v4();
                                queue.push_back((msg_id, resume_prompt, mission.model_override.clone()));
                                
                                // Start execution if not already running
                                if running.is_none() {
                                    if let Some((mid, msg, model_override)) = queue.pop_front() {
                                        set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                                        let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), mission_id: Some(mission_id) });
                                        let cfg = config.clone();
                                        let agent = Arc::clone(&root_agent);
                                        let mem = memory.clone();
                                        let bench = Arc::clone(&benchmarks);
                                        let res = Arc::clone(&resolver);
                                        let mcp_ref = Arc::clone(&mcp);
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
                                        let tree_ref = Arc::clone(&current_tree);
                                        let progress_ref = Arc::clone(&progress);
                                        running_cancel = Some(cancel.clone());
                                        // Capture which mission this task is working on (the resumed mission)
                                        running_mission_id = Some(mission_id);
                                        running = Some(tokio::spawn(async move {
                                            let result = run_single_control_turn(
                                                cfg,
                                                agent,
                                                mem,
                                                bench,
                                                res,
                                                mcp_ref,
                                                pricing,
                                                events,
                                                tools_hub,
                                                status_ref,
                                                cancel,
                                                hist_snapshot,
                                                msg.clone(),
                                                model_override,
                                                Some(mission_ctrl),
                                                tree_ref,
                                                progress_ref,
                                            )
                                            .await;
                                            (mid, msg, result)
                                        }));
                                    }
                                }

                                // Return the updated mission
                                let mut updated_mission = mission;
                                updated_mission.status = MissionStatus::Active;
                                updated_mission.resumable = false;
                                updated_mission.interrupted_at = None;
                                let _ = respond.send(Ok(updated_mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::GracefulShutdown { respond } => {
                        // Mark all running missions as interrupted
                        let mut interrupted_ids = Vec::new();

                        // Handle main mission - use running_mission_id (the actual mission being executed)
                        // Note: We DON'T persist history here because:
                        // 1. If current_mission == running_mission_id, history is correct
                        // 2. If current_mission != running_mission_id (user created new mission),
                        //    history was cleared and doesn't belong to running_mission_id
                        // The running mission's history is already in DB from previous exchanges,
                        // and any in-progress exchange will be lost (acceptable for shutdown).
                        if running.is_some() {
                            if let Some(mission_id) = running_mission_id {
                                // Only persist if the running mission is still current mission
                                // (i.e., user didn't create a new mission while this one was running)
                                let current_mid = current_mission.read().await.clone();
                                if current_mid == Some(mission_id) {
                                    persist_mission_history(&memory, &current_mission, &history).await;
                                }
                                // Note: If missions differ, don't persist - the local history
                                // belongs to current_mission, not running_mission_id

                                if let Some(mem) = &memory {
                                    if let Ok(()) = mem.supabase.update_mission_status(mission_id, "interrupted").await {
                                        interrupted_ids.push(mission_id);
                                        tracing::info!("Marked mission {} as interrupted", mission_id);
                                    }
                                }

                                // Cancel execution
                                if let Some(token) = &running_cancel {
                                    token.cancel();
                                }
                            }
                        }
                        
                        // Handle parallel missions
                        for (mission_id, runner) in parallel_runners.iter_mut() {
                            // Persist history for parallel mission
                            if let Some(mem) = &memory {
                                let messages: Vec<MissionMessage> = runner.history.iter()
                                    .map(|(role, content)| MissionMessage {
                                        role: role.clone(),
                                        content: content.clone(),
                                    })
                                    .collect();
                                let _ = mem.supabase.update_mission_history(*mission_id, &messages).await;
                                
                                if let Ok(()) = mem.supabase.update_mission_status(*mission_id, "interrupted").await {
                                    interrupted_ids.push(*mission_id);
                                    tracing::info!("Marked parallel mission {} as interrupted", mission_id);
                                }
                            }
                            
                            runner.cancel();
                        }
                        
                        let _ = respond.send(interrupted_ids);
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
                                    crate::tools::mission::MissionStatusValue::Blocked => MissionStatus::Blocked,
                                    crate::tools::mission::MissionStatusValue::NotFeasible => MissionStatus::NotFeasible,
                                };
                                let success = matches!(status, crate::tools::mission::MissionStatusValue::Completed);

                                if let Some(mem) = &memory {
                                    // Save the final tree before updating status
                                    if let Some(tree) = current_tree.read().await.clone() {
                                        if let Ok(tree_json) = serde_json::to_value(&tree) {
                                            if let Err(e) = mem.supabase.update_mission_tree(id, &tree_json).await {
                                                tracing::warn!("Failed to save mission tree: {}", e);
                                            } else {
                                                tracing::info!("Saved final tree for mission {}", id);
                                            }
                                        }
                                    }
                                    
                                    if let Ok(()) = mem.supabase.update_mission_status(id, &new_status.to_string()).await {
                                        // Generate and store mission summary
                                        if let Some(ref summary_text) = summary {
                                            // Extract key files from conversation (look for paths in assistant messages)
                                            let key_files: Vec<String> = history.iter()
                                                .filter(|(role, _)| role == "assistant")
                                                .flat_map(|(_, content)| extract_file_paths(content))
                                                .take(10)
                                                .collect();

                                            // Generate embedding for the summary
                                            let embedding = mem.embedder.embed(summary_text).await.ok();

                                            // Store mission summary
                                            if let Err(e) = mem.supabase.insert_mission_summary(
                                                id,
                                                summary_text,
                                                &key_files,
                                                &[], // tools_used - could track this
                                                success,
                                                embedding.as_deref(),
                                            ).await {
                                                tracing::warn!("Failed to store mission summary: {}", e);
                                            } else {
                                                tracing::info!("Stored mission summary for {}", id);
                                            }
                                        }

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
                    // Save the running mission ID before clearing it - we need it for persist and auto-complete
                    // (current_mission can change if user clicks "New Mission" while task was running)
                    let completed_mission_id = running_mission_id;
                    running = None;
                    running_cancel = None;
                    running_mission_id = None;
                    match res {
                        Ok((_mid, user_msg, agent_result)) => {
                            // Only append to local history if this mission is still the current mission.
                            // If the user created a new mission mid-execution, history was cleared for that new mission,
                            // and we don't want to contaminate it with the old mission's exchange.
                            let current_mid = current_mission.read().await.clone();
                            if completed_mission_id == current_mid {
                                history.push(("user".to_string(), user_msg.clone()));
                                history.push(("assistant".to_string(), agent_result.output.clone()));
                            }

                            // Persist to mission using the actual completed mission ID
                            // (not current_mission, which could have changed)
                            //
                            // IMPORTANT: We fetch existing history from DB and append, rather than
                            // using the local `history` variable, because CreateMission may have
                            // cleared `history` while this task was running. This prevents data loss.
                            if let (Some(mem), Some(mid)) = (&memory, completed_mission_id) {
                                // Fetch existing history from DB
                                let existing_history: Vec<MissionHistoryEntry> = match mem.supabase.get_mission(mid).await {
                                    Ok(Some(mission)) => {
                                        serde_json::from_value(mission.history).unwrap_or_default()
                                    }
                                    _ => Vec::new(),
                                };

                                // Append new messages to existing history
                                let mut messages: Vec<MissionMessage> = existing_history
                                    .iter()
                                    .map(|e| MissionMessage {
                                        role: e.role.clone(),
                                        content: e.content.clone(),
                                    })
                                    .collect();
                                messages.push(MissionMessage { role: "user".to_string(), content: user_msg.clone() });
                                messages.push(MissionMessage { role: "assistant".to_string(), content: agent_result.output.clone() });

                                if let Err(e) = mem.supabase.update_mission_history(mid, &messages).await {
                                    tracing::warn!("Failed to persist mission history: {}", e);
                                }

                                // Update title from first user message if not set (only on first exchange)
                                if existing_history.is_empty() {
                                    // Use safe_truncate_index for UTF-8 safe truncation, matching persist_mission_history
                                    let title = if user_msg.len() > 100 {
                                        let safe_end = crate::memory::safe_truncate_index(&user_msg, 100);
                                        format!("{}...", &user_msg[..safe_end])
                                    } else {
                                        user_msg.clone()
                                    };
                                    if let Err(e) = mem.supabase.update_mission_title(mid, &title).await {
                                        tracing::warn!("Failed to update mission title: {}", e);
                                    }
                                }
                            }

                            // P1 FIX: Auto-complete mission if agent execution ended in a terminal state
                            // without an explicit complete_mission call.
                            // This prevents missions from staying "active" forever after max iterations, stalls, etc.
                            //
                            // We use terminal_reason (structured enum) instead of substring matching to avoid
                            // false positives when agent output legitimately contains words like "infinite loop".
                            // We also check the current mission status from DB to handle:
                            // - Explicit complete_mission calls (which update DB status)
                            // - Parallel missions (each has its own DB status)
                            if agent_result.terminal_reason.is_some() {
                                if let Some(mem) = &memory {
                                    // Use completed_mission_id (the actual mission that just finished)
                                    // instead of current_mission (which can change when user creates a new mission)
                                    if let Some(mission_id) = completed_mission_id {
                                        // Check current mission status from DB - only auto-complete if still "active"
                                        let current_status = mem.supabase.get_mission(mission_id).await
                                            .ok()
                                            .flatten()
                                            .map(|m| m.status);
                                        
                                        if current_status.as_deref() == Some("active") {
                                            // Determine status based on terminal reason
                                            let (status, new_status) = match agent_result.terminal_reason {
                                                Some(TerminalReason::Completed) => {
                                                    ("completed", MissionStatus::Completed)
                                                }
                                                // MaxIterations -> blocked (resumable) instead of failed
                                                Some(TerminalReason::MaxIterations) => {
                                                    ("blocked", MissionStatus::Blocked)
                                                }
                                                _ if agent_result.success => {
                                                    ("completed", MissionStatus::Completed)
                                                }
                                                _ => {
                                                    ("failed", MissionStatus::Failed)
                                                }
                                            };
                                            
                                            tracing::info!(
                                                "Auto-completing mission {} with status '{}' (terminal_reason: {:?})",
                                                mission_id, status, agent_result.terminal_reason
                                            );
                                            if let Err(e) = mem.supabase.update_mission_status(mission_id, status).await {
                                                tracing::warn!("Failed to auto-complete mission: {}", e);
                                            } else {
                                                // Emit status change event
                                                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                    mission_id,
                                                    status: new_status,
                                                    summary: Some(format!("Auto-completed: {}", 
                                                        agent_result.output.chars().take(100).collect::<String>())),
                                                });
                                            }
                                        } else {
                                            tracing::debug!(
                                                "Skipping auto-complete: mission {} already has status {:?}",
                                                mission_id, current_status
                                            );
                                        }
                                    }
                                }
                            }

                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: Uuid::new_v4(),
                                content: agent_result.output.clone(),
                                success: agent_result.success,
                                cost_cents: agent_result.cost_cents,
                                model: agent_result.model_used,
                                mission_id: None,
                            });
                        }
                        Err(e) => {
                            let _ = events_tx.send(AgentEvent::Error { message: format!("Control session task join failed: {}", e), mission_id: None });
                        }
                    }
                }

                // Start next queued message, if any.
                if let Some((mid, msg, model_override)) = queue.pop_front() {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                    let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), mission_id: None });
                    let cfg = config.clone();
                    let agent = Arc::clone(&root_agent);
                    let mem = memory.clone();
                    let bench = Arc::clone(&benchmarks);
                    let res = Arc::clone(&resolver);
                    let mcp_ref = Arc::clone(&mcp);
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
                    let tree_ref = Arc::clone(&current_tree);
                    let progress_ref = Arc::clone(&progress);
                    running_cancel = Some(cancel.clone());
                    // Capture which mission this task is working on
                    running_mission_id = current_mission.read().await.clone();
                    running = Some(tokio::spawn(async move {
                        let result = run_single_control_turn(
                            cfg,
                            agent,
                            mem,
                            bench,
                            res,
                            mcp_ref,
                            pricing,
                            events,
                            tools_hub,
                            status_ref,
                            cancel,
                            hist_snapshot,
                            msg.clone(),
                            model_override,
                            Some(mission_ctrl),
                            tree_ref,
                            progress_ref,
                        )
                        .await;
                        (mid, msg, result)
                    }));
                } else {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0).await;
                }
            }
            // Poll parallel runners for completion
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                let mut completed_missions = Vec::new();
                
                for (mission_id, runner) in parallel_runners.iter_mut() {
                    if runner.check_finished() {
                        if let Some((msg_id, user_msg, result)) = runner.poll_completion().await {
                            tracing::info!(
                                "Parallel mission {} completed (success: {}, cost: {} cents)",
                                mission_id, result.success, result.cost_cents
                            );
                            
                            // Emit completion event with mission_id
                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: msg_id,
                                content: result.output.clone(),
                                success: result.success,
                                cost_cents: result.cost_cents,
                                model: result.model_used.clone(),
                                mission_id: Some(*mission_id),
                            });
                            
                            // Persist history for this mission
                            if let Some(mem) = &memory {
                                let messages: Vec<MissionMessage> = runner.history.iter()
                                    .map(|(role, content)| MissionMessage {
                                        role: role.clone(),
                                        content: content.clone(),
                                    })
                                    .collect();
                                if let Err(e) = mem.supabase.update_mission_history(*mission_id, &messages).await {
                                    tracing::warn!("Failed to persist parallel mission history: {}", e);
                                }
                            }
                            
                            // If runner has no more queued messages, mark for cleanup
                            if runner.queue.is_empty() && !runner.is_running() {
                                completed_missions.push(*mission_id);
                            }
                        }
                    }
                }
                
                // Remove completed runners
                for mid in completed_missions {
                    parallel_runners.remove(&mid);
                    tracing::info!("Parallel mission {} removed from runners", mid);
                }
            }
        }
    }
}

async fn run_single_control_turn(
    config: Config,
    root_agent: AgentRef,
    memory: Option<MemorySystem>,
    benchmarks: crate::budget::SharedBenchmarkRegistry,
    resolver: crate::budget::SharedModelResolver,
    mcp: Arc<McpRegistry>,
    pricing: Arc<ModelPricing>,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    model_override: Option<String>,
    mission_control: Option<crate::tools::mission::MissionControl>,
    tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,
    progress_snapshot: Arc<RwLock<ExecutionProgress>>,
) -> crate::agents::AgentResult {
    // Build a task prompt that includes conversation context with size limits.
    // Uses ContextBuilder with config-driven limits to prevent context overflow.
    let working_dir = config.working_dir.to_string_lossy().to_string();
    let context_builder = ContextBuilder::new(&config.context, &working_dir);
    let history_context = context_builder.build_history_context(&history);

    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    let instructions = if config.agent_backend == AgentBackend::OpenCode {
        "\n\nInstructions:\n- Continue the conversation helpfully.\n- Use available tools as needed.\n- For large data processing tasks (>10KB), prefer executing scripts rather than inline processing.\n"
    } else {
        "\n\nInstructions:\n- Continue the conversation helpfully.\n- You may use tools to gather information or make changes.\n- When appropriate, use Tool UI tools (ui_*) for structured output or to ask for user selections.\n- For large data processing tasks (>10KB), use run_command to execute Python scripts rather than processing inline.\n- When you have fully completed the user's goal or determined it cannot be completed, use the complete_mission tool to mark the mission status.\n"
    };
    convo.push_str(instructions);

    let budget = Budget::new(1000);
    let verification = VerificationCriteria::None;
    let mut task = match crate::task::Task::new(convo, verification, budget) {
        Ok(t) => t,
        Err(e) => {
            let r = crate::agents::AgentResult::failure(format!("Failed to create task: {}", e), 0);
            return r;
        }
    };

    // Apply model override if specified
    if let Some(model) = model_override {
        tracing::info!("Using model override: {}", model);
        task.analysis_mut().requested_model = Some(model);
    }

    // Context for agent execution.
    let llm = Arc::new(OpenRouterClient::new(config.api_key.clone()));

    // Create shared memory reference for memory tools
    let shared_memory: Option<crate::tools::memory::SharedMemory> = memory
        .as_ref()
        .map(|m| Arc::new(tokio::sync::RwLock::new(Some(m.clone()))));

    let tools = ToolRegistry::with_options(mission_control.clone(), shared_memory);
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
    ctx.resolver = Some(resolver);
    ctx.tree_snapshot = Some(tree_snapshot);
    ctx.progress_snapshot = Some(progress_snapshot);
    ctx.mcp = Some(mcp);

    let result = root_agent.execute(&mut task, &ctx).await;
    result
}
