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
    extract::{Extension, Path, State},
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
use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::workspace;

use super::auth::AuthUser;
use super::library::SharedLibrary;
use super::mission_store::{
    self, create_mission_store, now_string, Mission, MissionHistoryEntry, MissionStore,
    MissionStoreType, StoredEvent,
};
use super::routes::AppState;

/// Returns a safe index to truncate a string at, ensuring we don't cut UTF-8 characters.
fn safe_truncate_index(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    // Find a char boundary at or before max
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Build a simple history context from conversation history.
fn build_history_context(history: &[(String, String)], max_chars: usize) -> String {
    let mut result = String::new();
    let mut total_chars = 0;

    for (role, content) in history.iter().rev() {
        let entry = format!("{}: {}\n\n", role.to_uppercase(), content);
        if total_chars + entry.len() > max_chars && !result.is_empty() {
            break;
        }
        result = format!("{}{}", entry, result);
        total_chars += entry.len();
    }

    result
}

/// Message posted by a user to the control session.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlMessageRequest {
    pub content: String,
    /// Optional agent override for this specific message (e.g., from @agent mention)
    #[serde(default)]
    pub agent: Option<String>,
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

/// A file shared by the agent (images render inline, other files show as download links).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedFile {
    /// Display name for the file
    pub name: String,
    /// Public URL to view/download
    pub url: String,
    /// MIME type (e.g., "image/png", "application/pdf")
    pub content_type: String,
    /// File size in bytes (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// File kind for rendering hints: "image", "document", "archive", "code", "other"
    pub kind: SharedFileKind,
}

/// Kind of shared file (determines how it renders in the UI).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedFileKind {
    /// Images (PNG, JPEG, GIF, WebP, SVG) - rendered inline
    Image,
    /// Documents (PDF, Word, etc.) - shown as download card
    Document,
    /// Archives (ZIP, TAR, etc.) - shown as download card
    Archive,
    /// Code/text files - shown as download card with syntax hint
    Code,
    /// Other files - generic download card
    Other,
}

impl SharedFile {
    /// Create a new SharedFile, inferring kind from content_type.
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        content_type: impl Into<String>,
        size_bytes: Option<u64>,
    ) -> Self {
        let content_type = content_type.into();
        let kind = Self::infer_kind(&content_type);
        Self {
            name: name.into(),
            url: url.into(),
            content_type,
            size_bytes,
            kind,
        }
    }

    /// Infer the file kind from MIME type.
    fn infer_kind(content_type: &str) -> SharedFileKind {
        if content_type.starts_with("image/") {
            SharedFileKind::Image
        } else if content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
        {
            SharedFileKind::Code
        } else if content_type.contains("pdf")
            || content_type.contains("document")
            || content_type.contains("word")
        {
            SharedFileKind::Document
        } else if content_type.contains("zip")
            || content_type.contains("tar")
            || content_type.contains("gzip")
            || content_type.contains("compress")
        {
            SharedFileKind::Archive
        } else {
            SharedFileKind::Other
        }
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
        /// Whether this message is queued (not yet being processed).
        #[serde(default)]
        queued: bool,
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
        /// Files shared in this message (images, documents, etc.)
        #[serde(skip_serializing_if = "Option::is_none")]
        shared_files: Option<Vec<SharedFile>>,
        /// Whether the mission can be resumed after this failure (only relevant when success=false)
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumable: bool,
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
        /// Whether the mission can be resumed after this error
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumable: bool,
    },
    /// Mission status changed (by agent or user)
    MissionStatusChanged {
        mission_id: Uuid,
        status: MissionStatus,
        summary: Option<String>,
    },
    /// Agent phase update (for showing preparation steps)
    AgentPhase {
        /// Phase name: "executing", "delegating", etc.
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
    pub node_type: String, // e.g. "Root", "Worker"
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

    pub fn mission_id(&self) -> Option<Uuid> {
        match self {
            AgentEvent::Status { mission_id, .. } => *mission_id,
            AgentEvent::UserMessage { mission_id, .. } => *mission_id,
            AgentEvent::AssistantMessage { mission_id, .. } => *mission_id,
            AgentEvent::Thinking { mission_id, .. } => *mission_id,
            AgentEvent::ToolCall { mission_id, .. } => *mission_id,
            AgentEvent::ToolResult { mission_id, .. } => *mission_id,
            AgentEvent::Error { mission_id, .. } => *mission_id,
            AgentEvent::MissionStatusChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
            AgentEvent::AgentTree { mission_id, .. } => *mission_id,
            AgentEvent::Progress { mission_id, .. } => *mission_id,
        }
    }
}

/// Internal control commands (queued and processed by the actor).
#[derive(Debug)]
pub enum ControlCommand {
    UserMessage {
        id: Uuid,
        content: String,
        /// Optional agent override for this specific message
        agent: Option<String>,
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
        workspace_id: Option<Uuid>,
        /// Agent name from library (e.g., "code-reviewer")
        agent: Option<String>,
        /// Optional model override (provider/model)
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

// Mission and MissionHistoryEntry are now defined in mission_store module

/// Metadata for a desktop session started during a mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionInfo {
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshots_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The mission that owns this desktop session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<uuid::Uuid>,
    /// Timestamp until which the session should be kept alive (ISO 8601).
    /// User can extend this to prevent auto-close.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_until: Option<String>,
}

/// Request to set mission status.
#[derive(Debug, Clone, Deserialize)]
pub struct SetMissionStatusRequest {
    pub status: MissionStatus,
}

// MissionStore trait and implementations are in mission_store module

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
    /// Mission persistence (in-memory or Supabase-backed)
    pub mission_store: Arc<dyn MissionStore>,
}

/// Control session manager for per-user sessions.
#[derive(Clone)]
pub struct ControlHub {
    sessions: Arc<RwLock<HashMap<String, ControlState>>>,
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
}

impl ControlHub {
    pub fn new(
        config: Config,
        root_agent: AgentRef,
        mcp: Arc<McpRegistry>,
        workspaces: workspace::SharedWorkspaceStore,
        library: SharedLibrary,
    ) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
            root_agent,
            mcp,
            workspaces,
            library,
        }
    }

    pub async fn get_or_spawn(&self, user: &AuthUser) -> ControlState {
        if let Some(existing) = self.sessions.read().await.get(&user.id).cloned() {
            return existing;
        }
        let mut sessions = self.sessions.write().await;
        if let Some(existing) = sessions.get(&user.id).cloned() {
            return existing;
        }

        // Get mission store type from environment (default: SQLite)
        let store_type = std::env::var("MISSION_STORE_TYPE")
            .map(|s| MissionStoreType::from_str(&s))
            .unwrap_or(MissionStoreType::Sqlite);

        let base_dir = self.config.working_dir.join(".openagent").join("missions");
        let mission_store: Arc<dyn MissionStore> =
            match create_mission_store(store_type, base_dir, &user.id).await {
                Ok(store) => Arc::from(store),
                Err(err) => {
                    tracing::warn!(
                        "Failed to initialize {:?} mission store, falling back to memory: {}",
                        store_type,
                        err
                    );
                    Arc::new(mission_store::InMemoryMissionStore::new())
                }
            };

        let state = spawn_control_session(
            self.config.clone(),
            Arc::clone(&self.root_agent),
            Arc::clone(&self.mcp),
            Arc::clone(&self.workspaces),
            Arc::clone(&self.library),
            mission_store,
        );
        sessions.insert(user.id.clone(), state.clone());
        state
    }

    pub async fn all_sessions(&self) -> Vec<ControlState> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// Get a mission store for desktop management.
    /// Uses the default user's store if available, or creates a temporary one.
    pub async fn get_mission_store(&self) -> Arc<dyn MissionStore> {
        // Try to get from the first existing session
        if let Some(session) = self.sessions.read().await.values().next() {
            return Arc::clone(&session.mission_store);
        }

        // No existing sessions, create a temporary store
        let store_type = std::env::var("MISSION_STORE_TYPE")
            .map(|s| MissionStoreType::from_str(&s))
            .unwrap_or(MissionStoreType::Sqlite);

        let base_dir = self.config.working_dir.join(".openagent").join("missions");

        match create_mission_store(store_type, base_dir, "default").await {
            Ok(store) => Arc::from(store),
            Err(err) => {
                tracing::warn!(
                    "Failed to create mission store for desktop management: {}",
                    err
                );
                Arc::new(mission_store::InMemoryMissionStore::new())
            }
        }
    }
}

/// Execution progress for showing overall mission progress
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
    let _ = events.send(AgentEvent::Status {
        state,
        queue_len,
        mission_id: None,
    });
}

async fn control_for_user(state: &Arc<AppState>, user: &AuthUser) -> ControlState {
    state.control.get_or_spawn(user).await
}

/// Enqueue a user message for the global control session.
pub async fn post_message(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<ControlMessageRequest>,
) -> Result<Json<ControlMessageResponse>, (StatusCode, String)> {
    let content = req.content.trim().to_string();
    if content.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "content is required".to_string()));
    }

    let id = Uuid::new_v4();
    let agent = req.agent;
    let control = control_for_user(&state, &user).await;
    let queued = {
        let status = control.status.read().await;
        status.state != ControlRunState::Idle
    };
    tracing::info!(
        user_id = %user.id,
        username = %user.username,
        message_id = %id,
        queued,
        content_len = content.len(),
        agent = ?agent,
        "Received control message"
    );
    control
        .cmd_tx
        .send(ControlCommand::UserMessage { id, content, agent })
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
    Extension(user): Extension<AuthUser>,
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

    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<Mission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let mut missions = control
        .mission_store
        .list_missions(50, 0)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Populate workspace_name for each mission
    for mission in &mut missions {
        if let Some(workspace) = state.workspaces.get(mission.workspace_id).await {
            mission.workspace_name = Some(workspace.name);
        }
    }

    Ok(Json(missions))
}

/// Get a specific mission.
pub async fn get_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    match control
        .mission_store
        .get_mission(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        Some(mut mission) => {
            // Populate workspace_name
            if let Some(workspace) = state.workspaces.get(mission.workspace_id).await {
                mission.workspace_name = Some(workspace.name);
            }
            Ok(Json(mission))
        }
        None => Err((StatusCode::NOT_FOUND, format!("Mission {} not found", id))),
    }
}

/// Create a new mission and switch to it.
/// Request body for creating a mission
#[derive(Debug, Deserialize)]
pub struct CreateMissionRequest {
    pub title: Option<String>,
    /// Workspace ID to run the mission in (defaults to host workspace)
    pub workspace_id: Option<Uuid>,
    /// Agent name from library (e.g., "code-reviewer")
    pub agent: Option<String>,
    /// Optional model override (provider/model)
    pub model_override: Option<String>,
}

pub async fn create_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    body: Option<Json<CreateMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let (title, workspace_id, agent, model_override) = body
        .map(|b| {
            (
                b.title.clone(),
                b.workspace_id,
                b.agent.clone(),
                b.model_override.clone(),
            )
        })
        .unwrap_or((None, None, None, None));

    // Validate agent exists before creating mission (fail fast with clear error)
    if let Some(ref agent_name) = agent {
        super::library::validate_agent_exists(&state, agent_name)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::CreateMission {
            title,
            workspace_id,
            agent,
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
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMissionStatusRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Option<Mission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let current_id = control.current_mission.read().await.clone();

    match current_id {
        Some(id) => {
            let mission = control
                .mission_store
                .get_mission(id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(Json(mission))
        }
        None => Ok(Json(None)),
    }
}

/// Get current agent tree snapshot (for refresh resilience).
/// Returns the last emitted tree state, or null if no tree is active.
pub async fn get_tree(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<Option<AgentTreeNode>> {
    let control = control_for_user(&state, &user).await;
    let tree = control.current_tree.read().await.clone();
    Json(tree)
}

/// Get tree for a specific mission.
/// For currently running mission, returns the live tree from memory.
/// For completed missions, returns the saved final_tree from the database.
pub async fn get_mission_tree(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Option<AgentTreeNode>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    // Check if this is the current active mission
    let current_id = control.current_mission.read().await.clone();
    if current_id == Some(mission_id) {
        // Return live tree from memory
        let tree = control.current_tree.read().await.clone();
        return Ok(Json(tree));
    }
    let tree = control
        .mission_store
        .get_mission_tree(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if tree.is_some() {
        return Ok(Json(tree));
    }

    let mission_exists = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if mission_exists.is_some() {
        Ok(Json(None))
    } else {
        Err((StatusCode::NOT_FOUND, "Mission not found".to_string()))
    }
}

/// Get current execution progress (for progress indicator).
pub async fn get_progress(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<ExecutionProgress> {
    let control = control_for_user(&state, &user).await;
    let progress = control.progress.read().await.clone();
    Json(progress)
}

/// Query params for events endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct GetEventsQuery {
    /// Comma-separated event types to filter (e.g., "tool_call,tool_result")
    #[serde(default)]
    pub types: Option<String>,
    /// Maximum number of events to return
    #[serde(default)]
    pub limit: Option<usize>,
    /// Offset for pagination
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Get events for a mission (for debugging/replay).
pub async fn get_mission_events(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<GetEventsQuery>,
) -> Result<Json<Vec<StoredEvent>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    // Check mission exists
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if mission.is_none() {
        return Err((StatusCode::NOT_FOUND, "Mission not found".to_string()));
    }

    // Parse event types filter
    let types: Option<Vec<&str>> = query
        .types
        .as_ref()
        .map(|s| s.split(',').map(|t| t.trim()).collect());

    let events = control
        .mission_store
        .get_events(mission_id, types.as_deref(), query.limit, query.offset)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(events))
}

// ==================== Diagnostic Endpoints ====================

/// Response for OpenCode diagnostic endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct OpenCodeDiagnostics {
    /// OpenCode base URL
    pub base_url: String,
    /// Current session ID (if active)
    pub session_id: Option<String>,
    /// Session status from OpenCode
    pub session_status: Option<crate::opencode::OpenCodeSessionStatus>,
    /// Error message if status check failed
    pub error: Option<String>,
}

/// Get OpenCode session diagnostics for debugging stuck operations.
pub async fn get_opencode_diagnostics(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<OpenCodeDiagnostics>, (StatusCode, String)> {
    let opencode_url = state.config.opencode_base_url.clone();

    // Create a client to query OpenCode
    let client = crate::opencode::OpenCodeClient::new(&opencode_url, None, false);

    // Try to get the list of sessions from OpenCode
    let sessions_url = format!("{}/session", opencode_url);
    let sessions_result = reqwest::Client::new()
        .get(&sessions_url)
        .header("Accept", "application/json")
        .send()
        .await;

    match sessions_result {
        Ok(resp) if resp.status().is_success() => {
            let sessions: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();

            // Find the most recent session
            let most_recent = sessions.iter().max_by_key(|s| {
                s.get("time")
                    .and_then(|t| t.get("updated"))
                    .and_then(|u| u.as_u64())
                    .unwrap_or(0)
            });

            if let Some(session) = most_recent {
                let session_id = session
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Get detailed status for this session
                match client.get_session_status(&session_id).await {
                    Ok(status) => Ok(Json(OpenCodeDiagnostics {
                        base_url: opencode_url,
                        session_id: Some(session_id),
                        session_status: Some(status),
                        error: None,
                    })),
                    Err(e) => Ok(Json(OpenCodeDiagnostics {
                        base_url: opencode_url,
                        session_id: Some(session_id),
                        session_status: None,
                        error: Some(format!("Failed to get session status: {}", e)),
                    })),
                }
            } else {
                Ok(Json(OpenCodeDiagnostics {
                    base_url: opencode_url,
                    session_id: None,
                    session_status: None,
                    error: Some("No sessions found".to_string()),
                }))
            }
        }
        Ok(resp) => Ok(Json(OpenCodeDiagnostics {
            base_url: opencode_url,
            session_id: None,
            session_status: None,
            error: Some(format!("OpenCode returned status {}", resp.status())),
        })),
        Err(e) => Ok(Json(OpenCodeDiagnostics {
            base_url: opencode_url,
            session_id: None,
            session_status: None,
            error: Some(format!("Failed to connect to OpenCode: {}", e)),
        })),
    }
}

// ==================== Parallel Mission Endpoints ====================

/// List currently running missions.
pub async fn list_running_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<super::mission_runner::RunningMissionInfo>>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
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
}

/// Start a mission in parallel (if capacity allows).
pub async fn start_mission_parallel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<StartParallelRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::StartParallel {
            mission_id,
            content: req.content,
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
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    body: Option<Json<ResumeMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let clean_workspace = body.map(|b| b.clean_workspace).unwrap_or(false);
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
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
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Query actual running count from the control actor
    // (the running state is tracked in the actor loop, not in shared state)
    let (tx, rx) = oneshot::channel();
    let control = control_for_user(&state, &user).await;
    control
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
        "max_parallel_missions": control.max_parallel,
        "running_count": running.len(),
    })))
}

/// Delete a mission by ID.
/// Only allows deleting missions that are not currently running.
pub async fn delete_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Check if mission is currently running by querying the control actor
    // (the actual running state is tracked in the actor loop, not in shared state)
    let (tx, rx) = oneshot::channel();
    let control = control_for_user(&state, &user).await;
    control
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

    let deleted = control
        .mission_store
        .delete_mission(mission_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Get currently running mission IDs to exclude from cleanup
    // (a newly-started mission may have empty history in DB while actively running)
    let (tx, rx) = oneshot::channel();
    let control = control_for_user(&state, &user).await;
    control
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

    let count = control
        .mission_store
        .delete_empty_untitled_missions_excluding(&running_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "deleted_count": count
    })))
}

/// Stream control session events via SSE.
pub async fn stream(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let mut rx = control.events_tx.subscribe();
    let stream_id = Uuid::new_v4();
    tracing::info!(
        stream_id = %stream_id,
        user_id = %user.id,
        username = %user.username,
        "Control SSE stream opened"
    );

    // Emit an initial status snapshot immediately.
    let initial = control.status.read().await.clone();

    struct StreamDropGuard {
        stream_id: Uuid,
        user_id: String,
        username: String,
    }

    impl Drop for StreamDropGuard {
        fn drop(&mut self) {
            tracing::info!(
                stream_id = %self.stream_id,
                user_id = %self.user_id,
                username = %self.username,
                "Control SSE stream closed"
            );
        }
    }

    let drop_guard = StreamDropGuard {
        stream_id,
        user_id: user.id.clone(),
        username: user.username.clone(),
    };

    let stream = async_stream::stream! {
        let _guard = drop_guard;
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
                            let mission_id = ev.mission_id();
                            match &ev {
                                AgentEvent::Thinking { .. } => {
                                    tracing::trace!(
                                        stream_id = %stream_id,
                                        event = %ev.event_name(),
                                        mission_id = ?mission_id,
                                        "Control SSE event"
                                    );
                                }
                                _ => {
                                    tracing::debug!(
                                        stream_id = %stream_id,
                                        event = %ev.event_name(),
                                        mission_id = ?mission_id,
                                        "Control SSE event"
                                    );
                                }
                            }
                            let sse = Event::default().event(ev.event_name()).json_data(&ev).unwrap();
                            yield Ok(sse);
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!(
                                stream_id = %stream_id,
                                "Control SSE stream lagged; events dropped"
                            );
                            let sse = Event::default()
                                .event("error")
                                .json_data(AgentEvent::Error { message: "event stream lagged; some events were dropped".to_string(), mission_id: None, resumable: false })
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
fn spawn_control_session(
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    mission_store: Arc<dyn MissionStore>,
) -> ControlState {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ControlCommand>(256);
    let (events_tx, events_rx) = broadcast::channel::<AgentEvent>(1024);
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
        mission_store: Arc::clone(&mission_store),
    };

    // Spawn the main control actor
    tokio::spawn(control_actor_loop(
        config.clone(),
        root_agent,
        mcp,
        workspaces,
        library,
        cmd_rx,
        mission_cmd_rx,
        mission_cmd_tx,
        events_tx.clone(),
        events_rx,
        tool_hub,
        status,
        current_mission,
        current_tree,
        progress,
        mission_store,
    ));

    // Spawn background stale mission cleanup task (if enabled)
    if config.stale_mission_hours > 0 && state.mission_store.is_persistent() {
        tokio::spawn(stale_mission_cleanup_loop(
            Arc::clone(&state.mission_store),
            config.stale_mission_hours,
            events_tx.clone(),
        ));
    }

    // Spawn event logger task (logs all events to SQLite for debugging/replay)
    if state.mission_store.is_persistent() {
        let store = Arc::clone(&state.mission_store);
        let mut event_rx = events_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => {
                        // Extract mission_id from event
                        if let Some(mid) = event.mission_id() {
                            if let Err(e) = store.log_event(mid, &event).await {
                                tracing::warn!("Failed to log event: {}", e);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Event logger lagged by {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            tracing::info!("Event logger task stopped");
        });
    }

    state
}

/// Background task that periodically closes stale missions.
async fn stale_mission_cleanup_loop(
    mission_store: Arc<dyn MissionStore>,
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

        match mission_store.get_stale_active_missions(stale_hours).await {
            Ok(stale_missions) => {
                for mission in stale_missions {
                    tracing::info!(
                        "Auto-closing stale mission {}: '{}' (inactive since {})",
                        mission.id,
                        mission.title.as_deref().unwrap_or("Untitled"),
                        mission.updated_at
                    );

                    if let Err(e) = mission_store
                        .update_mission_status(mission.id, MissionStatus::Completed)
                        .await
                    {
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
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    mut cmd_rx: mpsc::Receiver<ControlCommand>,
    mut mission_cmd_rx: mpsc::Receiver<crate::tools::mission::MissionControlCommand>,
    mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    mut events_rx: broadcast::Receiver<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    current_mission: Arc<RwLock<Option<Uuid>>>,
    current_tree: Arc<RwLock<Option<AgentTreeNode>>>,
    progress: Arc<RwLock<ExecutionProgress>>,
    mission_store: Arc<dyn MissionStore>,
) {
    // Queue stores (id, content, agent) for the current/primary mission
    let mut queue: VecDeque<(Uuid, String, Option<String>)> = VecDeque::new();
    let mut history: Vec<(String, String)> = Vec::new(); // (role, content) pairs (user/assistant)
    let mut running: Option<tokio::task::JoinHandle<(Uuid, String, crate::agents::AgentResult)>> =
        None;
    let mut running_cancel: Option<CancellationToken> = None;
    // Track which mission the main `running` task is actually working on.
    // This is different from `current_mission` which can change when user creates a new mission.
    let mut running_mission_id: Option<Uuid> = None;
    // Track last activity for the main runner (for stall detection)
    let mut main_runner_last_activity: std::time::Instant = std::time::Instant::now();

    // Parallel mission runners - each runs independently
    let mut parallel_runners: std::collections::HashMap<
        Uuid,
        super::mission_runner::MissionRunner,
    > = std::collections::HashMap::new();

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
        mission_store: &Arc<dyn MissionStore>,
        current_mission: &Arc<RwLock<Option<Uuid>>>,
        history: &[(String, String)],
    ) {
        let mission_id = current_mission.read().await.clone();
        if let Some(mid) = mission_id {
            let entries: Vec<MissionHistoryEntry> = history
                .iter()
                .map(|(role, content)| MissionHistoryEntry {
                    role: role.clone(),
                    content: content.clone(),
                })
                .collect();
            if let Err(e) = mission_store.update_mission_history(mid, &entries).await {
                tracing::warn!("Failed to persist mission history: {}", e);
            }

            // Update title from first user message if not set
            if history.len() == 2 {
                if let Some((role, content)) = history.first() {
                    if role == "user" {
                        let should_update = mission_store
                            .get_mission(mid)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|m| m.title)
                            .map(|t| t.trim().is_empty())
                            .unwrap_or(true);
                        if should_update {
                            let title = if content.len() > 100 {
                                let safe_end = safe_truncate_index(content, 100);
                                format!("{}...", &content[..safe_end])
                            } else {
                                content.clone()
                            };
                            if let Err(e) = mission_store.update_mission_title(mid, &title).await {
                                tracing::warn!("Failed to update mission title: {}", e);
                            }
                        }
                    }
                }
            }
        }
    }

    fn parse_tool_result_object(result: &serde_json::Value) -> Option<serde_json::Value> {
        if result.is_object() {
            return Some(result.clone());
        }
        if let Some(raw) = result.as_str() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
                return Some(parsed);
            }
        }
        None
    }

    // Helper to load a mission and return a Mission struct
    async fn load_mission_record(
        mission_store: &Arc<dyn MissionStore>,
        id: Uuid,
    ) -> Result<Mission, String> {
        mission_store
            .get_mission(id)
            .await?
            .ok_or_else(|| format!("Mission {} not found", id))
    }

    // Helper to create a new mission
    async fn create_new_mission(mission_store: &Arc<dyn MissionStore>) -> Result<Mission, String> {
        create_new_mission_with_title(mission_store, None, None, None, None).await
    }

    // Helper to create a new mission with title
    async fn create_new_mission_with_title(
        mission_store: &Arc<dyn MissionStore>,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<Mission, String> {
        mission_store
            .create_mission(title, workspace_id, agent, model_override)
            .await
    }

    // Helper to build resume context for an interrupted or blocked mission
    async fn resume_mission_impl(
        mission_store: &Arc<dyn MissionStore>,
        config: &Config,
        workspaces: &workspace::SharedWorkspaceStore,
        mission_id: Uuid,
        clean_workspace: bool,
    ) -> Result<(Mission, String), String> {
        let mission = load_mission_record(mission_store, mission_id).await?;

        // Check if mission can be resumed (interrupted or blocked)
        if !matches!(
            mission.status,
            MissionStatus::Interrupted | MissionStatus::Blocked
        ) {
            return Err(format!(
                "Mission {} cannot be resumed (status: {})",
                mission_id, mission.status
            ));
        }

        // Clean workspace if requested
        let workspace_root =
            workspace::resolve_workspace_root(workspaces, config, Some(mission.workspace_id)).await;
        let mission_dir = workspace::mission_workspace_dir_for_root(&workspace_root, mission_id);

        if clean_workspace && mission_dir.exists() {
            tracing::info!(
                "Cleaning workspace for mission {} at {:?}",
                mission_id,
                mission_dir
            );
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
            resume_parts.push(format!(
                "**MISSION RESUMED**{}\nThis mission {} and is now being continued.",
                workspace_note, resume_reason
            ));
        }

        // Add history summary
        if !mission.history.is_empty() {
            resume_parts.push("\n## Previous Conversation Summary".to_string());

            // Include the original user request
            if let Some(first_user) = mission.history.iter().find(|h| h.role == "user") {
                resume_parts.push(format!("\n**Original Request:**\n{}", first_user.content));
            }

            // Include last assistant response (what was being worked on)
            if let Some(last_assistant) =
                mission.history.iter().rev().find(|h| h.role == "assistant")
            {
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
                        if dir_name == "venv"
                            || dir_name == ".venv"
                            || dir_name == ".open_agent"
                            || dir_name == ".openagent"
                            || dir_name == "temp"
                        {
                            continue;
                        }
                        // List files in subdirectory
                        if let Ok(subentries) = std::fs::read_dir(&path) {
                            for subentry in subentries.filter_map(|e| e.ok()) {
                                let subpath = subentry.path();
                                if subpath.is_file() {
                                    let rel_path = subpath
                                        .strip_prefix(&mission_dir)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|_| subpath.display().to_string());
                                    files_found.push(rel_path);
                                }
                            }
                        }
                    } else if path.is_file() {
                        let rel_path = path
                            .strip_prefix(&mission_dir)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| path.display().to_string());
                        files_found.push(rel_path);
                    }
                }
            }

            if !files_found.is_empty() {
                resume_parts.push(format!(
                    "Files created:\n{}",
                    files_found
                        .iter()
                        .map(|f| format!("- {}", f))
                        .collect::<Vec<_>>()
                        .join("\n")
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
                    ControlCommand::UserMessage { id, content, agent: msg_agent } => {
                        // Auto-create mission on first message if none exists
                        {
                            let mission_id = current_mission.read().await.clone();
                            if mission_id.is_none() {
                                if let Ok(new_mission) =
                                    create_new_mission(&mission_store).await
                                {
                                    *current_mission.write().await = Some(new_mission.id);
                                    tracing::info!("Auto-created mission: {}", new_mission.id);
                                }
                            }
                        }

                        let was_running = running.is_some();
                        let content_clone = content.clone();
                        queue.push_back((id, content, msg_agent));
                        set_and_emit_status(
                            &status,
                            &events_tx,
                            if running.is_some() { ControlRunState::Running } else { ControlRunState::Idle },
                            queue.len(),
                        ).await;
                        if was_running {
                            let current_mid = current_mission.read().await.clone();
                            let _ = events_tx.send(AgentEvent::UserMessage {
                                id,
                                content: content_clone,
                                queued: true,
                                mission_id: current_mid,
                            });
                        }
                        if running.is_none() {
                            if let Some((mid, msg, per_msg_agent)) = queue.pop_front() {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                                let current_mid = current_mission.read().await.clone();
                                let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: current_mid });

                                // Immediately persist user message so it's visible when loading mission
                                history.push(("user".to_string(), msg.clone()));
                                persist_mission_history(&mission_store, &current_mission, &history)
                                    .await;

                                let cfg = config.clone();
                                let agent = Arc::clone(&root_agent);
                                let mcp_ref = Arc::clone(&mcp);
                                let workspaces_ref = Arc::clone(&workspaces);
                                let library_ref = Arc::clone(&library);
                                let events = events_tx.clone();
                                let tools_hub = Arc::clone(&tool_hub);
                                let status_ref = Arc::clone(&status);
                                let cancel = CancellationToken::new();
                                let hist_snapshot = history.clone();
                                let mission_ctrl = crate::tools::mission::MissionControl {
                                    current_mission_id: Arc::clone(&current_mission),
                                    cmd_tx: mission_cmd_tx.clone(),
                                };
                                let tree_ref = Arc::clone(&current_tree);
                                let progress_ref = Arc::clone(&progress);
                                // Capture which mission this task is working on
                                let mission_id = current_mission.read().await.clone();
                                let (workspace_id, model_override, mission_agent) = if let Some(mid) = mission_id {
                                    match mission_store.get_mission(mid).await {
                                        Ok(Some(mission)) => (
                                            Some(mission.workspace_id),
                                            mission.model_override.clone(),
                                            mission.agent.clone(),
                                        ),
                                        Ok(None) => {
                                            tracing::warn!(
                                                "Mission {} not found while resolving workspace",
                                                mid
                                            );
                                            (None, None, None)
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to load mission {} for workspace: {}",
                                                mid,
                                                e
                                            );
                                            (None, None, None)
                                        }
                                    }
                                } else {
                                    (None, None, None)
                                };
                                // Per-message agent overrides mission agent
                                let agent_override = per_msg_agent.or(mission_agent);
                                running_cancel = Some(cancel.clone());
                                running_mission_id = mission_id;
                                // Reset activity timer when new task starts to avoid false stall warnings
                                main_runner_last_activity = std::time::Instant::now();
                                running = Some(tokio::spawn(async move {
                                    let result = run_single_control_turn(
                                        cfg,
                                        agent,
                                        mcp_ref,
                                        workspaces_ref,
                                        library_ref,
                                        events,
                                        tools_hub,
                                        status_ref,
                                        cancel,
                                        hist_snapshot,
                                        msg.clone(),
                                        Some(mission_ctrl),
                                        tree_ref,
                                        progress_ref,
                                        mission_id,
                                        workspace_id,
                                        model_override,
                                        agent_override,
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
                            let _ = events_tx.send(AgentEvent::Error { message: format!("Unknown tool_call_id '{}' for tool '{}'", tool_call_id, name), mission_id: None, resumable: false });
                        }
                    }
                    ControlCommand::Cancel => {
                        if let Some(token) = &running_cancel {
                            token.cancel();
                            // Don't send Error event here - the task will complete and send
                            // an AssistantMessage with the cancellation result when it finishes.
                            // Sending both causes duplicate UI messages.
                        } else {
                            let _ = events_tx.send(AgentEvent::Error { message: "No running task to cancel".to_string(), mission_id: None, resumable: false });
                        }
                    }
                    ControlCommand::LoadMission { id, respond } => {
                        // First persist current mission history
                        persist_mission_history(
                            &mission_store,
                            &current_mission,
                            &history,
                        )
                        .await;

                        // Load the new mission
                        match load_mission_record(
                            &mission_store,
                            id,
                        )
                        .await {
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
                    ControlCommand::CreateMission { title, workspace_id, agent, model_override, respond } => {
                        // First persist current mission history
                        persist_mission_history(
                            &mission_store,
                            &current_mission,
                            &history,
                        )
                        .await;

                        // Create a new mission with optional title, workspace, and agent
                        match create_new_mission_with_title(
                            &mission_store,
                            title.as_deref(),
                            workspace_id,
                            agent.as_deref(),
                            model_override.as_deref(),
                        )
                        .await {
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
                        let current_id = current_mission.read().await.clone();
                        if current_id == Some(id) {
                            if let Some(tree) = current_tree.read().await.clone() {
                                if let Err(e) = mission_store.update_mission_tree(id, &tree).await
                                {
                                    tracing::warn!("Failed to save mission tree: {}", e);
                                }
                            }
                        }

                        let result = mission_store
                            .update_mission_status(id, new_status)
                            .await;
                        if result.is_ok() {
                            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                mission_id: id,
                                status: new_status,
                                summary: None,
                            });
                        }
                        let _ = respond.send(result);
                    }
                    ControlCommand::StartParallel { mission_id, content, respond } => {
                        tracing::info!("StartParallel requested for mission {}", mission_id);

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
                            // Load mission to get existing history
                            let mission = match load_mission_record(
                                &mission_store,
                                mission_id,
                            )
                            .await {
                                Ok(m) => m,
                                Err(e) => {
                                    let _ = respond.send(Err(format!("Failed to load mission: {}", e)));
                                    continue;
                                }
                            };

                            // Create a new MissionRunner
                            let mut runner = super::mission_runner::MissionRunner::new(
                                mission_id,
                                mission.workspace_id,
                                mission.agent.clone(),
                            );

                            // Load existing history into runner to preserve conversation context
                            for entry in &mission.history {
                                runner.history.push((entry.role.clone(), entry.content.clone()));
                            }

                            // Queue the initial message (no per-message agent override for parallel start)
                            runner.queue_message(Uuid::new_v4(), content, None);

                            // Start execution
                            let started = runner.start_next(
                                config.clone(),
                                Arc::clone(&root_agent),
                                Arc::clone(&mcp),
                                Arc::clone(&workspaces),
                                library.clone(),
                                events_tx.clone(),
                                Arc::clone(&tool_hub),
                                Arc::clone(&status),
                                mission_cmd_tx.clone(),
                                Arc::new(RwLock::new(Some(mission_id))), // Each runner tracks its own mission
                            );

                            if started {
                                tracing::info!("Mission {} started in parallel", mission_id);
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
                                resumable: true, // Cancelled missions can be resumed
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
                                    // Don't send Error event here - the task will complete and send
                                    // an AssistantMessage with resumable=true when it finishes.
                                    // Sending both causes duplicate UI messages.
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
                                    state: "running".to_string(),
                                    queue_len: queue.len(),
                                    history_len: history.len(),
                                    seconds_since_activity: main_runner_last_activity.elapsed().as_secs(),
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
                        match resume_mission_impl(
                            &mission_store,
                            &config,
                            &workspaces,
                            mission_id,
                            clean_workspace,
                        )
                        .await {
                            Ok((mission, resume_prompt)) => {
                                // First persist current mission history (if any)
                                persist_mission_history(
                                    &mission_store,
                                    &current_mission,
                                    &history,
                                )
                                .await;

                                // Load the mission's history into current state
                                history = mission.history.iter()
                                    .map(|e| (e.role.clone(), e.content.clone()))
                                    .collect();
                                *current_mission.write().await = Some(mission_id);

                                // Update mission status back to active
                                if let Err(e) = mission_store
                                    .update_mission_status(mission_id, MissionStatus::Active)
                                    .await
                                {
                                    tracing::warn!("Failed to resume mission {}: {}", mission_id, e);
                                }

                                // Queue the resume prompt as a message (no per-message agent override)
                                let msg_id = Uuid::new_v4();
                                queue.push_back((msg_id, resume_prompt, None));

                                // Start execution if not already running
                                if running.is_none() {
                                    if let Some((mid, msg, _per_msg_agent)) = queue.pop_front() {
                                        set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                                        let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: Some(mission_id) });
                                        let cfg = config.clone();
                                        let agent = Arc::clone(&root_agent);
                                        let mcp_ref = Arc::clone(&mcp);
                                        let workspaces_ref = Arc::clone(&workspaces);
                                        let library_ref = Arc::clone(&library);
                                        let events = events_tx.clone();
                                        let tools_hub = Arc::clone(&tool_hub);
                                        let status_ref = Arc::clone(&status);
                                        let cancel = CancellationToken::new();
                                        let hist_snapshot = history.clone();
                                        let mission_ctrl = crate::tools::mission::MissionControl {
                                            current_mission_id: Arc::clone(&current_mission),
                                            cmd_tx: mission_cmd_tx.clone(),
                                        };
                                        let tree_ref = Arc::clone(&current_tree);
                                        let progress_ref = Arc::clone(&progress);
                                        let workspace_id = Some(mission.workspace_id);
                                        let model_override = mission.model_override.clone();
                                        // Resume uses mission agent (no per-message override for resumes)
                                        let agent_override = mission.agent.clone();
                                        running_cancel = Some(cancel.clone());
                                        // Capture which mission this task is working on (the resumed mission)
                                        running_mission_id = Some(mission_id);
                                        running = Some(tokio::spawn(async move {
                                            let result = run_single_control_turn(
                                                cfg,
                                                agent,
                                                mcp_ref,
                                                workspaces_ref,
                                                library_ref,
                                                events,
                                                tools_hub,
                                                status_ref,
                                                cancel,
                                                hist_snapshot,
                                                msg.clone(),
                                                Some(mission_ctrl),
                                                tree_ref,
                                                progress_ref,
                                                Some(mission_id),
                                                workspace_id,
                                                model_override,
                                                agent_override,
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
                                    persist_mission_history(
                                        &mission_store,
                                        &current_mission,
                                        &history,
                                    )
                                    .await;
                                }
                                // Note: If missions differ, don't persist - the local history
                                // belongs to current_mission, not running_mission_id

                                if mission_store
                                    .update_mission_status(mission_id, MissionStatus::Interrupted)
                                    .await
                                    .is_ok()
                                {
                                    interrupted_ids.push(mission_id);
                                    tracing::info!("Marked mission {} as interrupted", mission_id);
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
                            let entries: Vec<MissionHistoryEntry> = runner
                                .history
                                .iter()
                                .map(|(role, content)| MissionHistoryEntry {
                                    role: role.clone(),
                                    content: content.clone(),
                                })
                                .collect();
                            if let Err(e) = mission_store
                                .update_mission_history(*mission_id, &entries)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to persist parallel mission history {}: {}",
                                    mission_id,
                                    e
                                );
                            }
                            if mission_store
                                .update_mission_status(*mission_id, MissionStatus::Interrupted)
                                .await
                                .is_ok()
                            {
                                interrupted_ids.push(*mission_id);
                                tracing::info!("Marked parallel mission {} as interrupted", mission_id);
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
                                // Save the final tree before updating status
                                if let Some(tree) = current_tree.read().await.clone() {
                                    if let Err(e) = mission_store.update_mission_tree(id, &tree).await {
                                        tracing::warn!("Failed to save mission tree: {}", e);
                                    } else {
                                        tracing::info!("Saved final tree for mission {}", id);
                                    }
                                }

                                if mission_store
                                    .update_mission_status(id, new_status)
                                    .await
                                    .is_ok()
                                {
                                    // Generate and store mission summary
                                    if let Some(ref summary_text) = summary {
                                        // Extract key files from conversation (look for paths in assistant messages)
                                        let key_files: Vec<String> = history
                                            .iter()
                                            .filter(|(role, _)| role == "assistant")
                                            .flat_map(|(_, content)| extract_file_paths(content))
                                            .take(10)
                                            .collect();

                                        if let Err(e) = mission_store
                                            .insert_mission_summary(id, summary_text, &key_files, success)
                                            .await
                                        {
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
                            // Only append assistant to local history if this mission is still the current mission.
                            // Note: User message was already added before execution started.
                            // If the user created a new mission mid-execution, history was cleared for that new mission,
                            // and we don't want to contaminate it with the old mission's exchange.
                            let current_mid = current_mission.read().await.clone();
                            if completed_mission_id == current_mid {
                                history.push(("assistant".to_string(), agent_result.output.clone()));
                            }

                            // Persist to mission using the actual completed mission ID
                            // (not current_mission, which could have changed)
                            //
                            // IMPORTANT: We fetch existing history from DB and append, rather than
                            // using the local `history` variable, because CreateMission may have
                            // cleared `history` while this task was running. This prevents data loss.
                            // Note: User message was already persisted before execution started.
                            if let Some(mid) = completed_mission_id {
                                match mission_store.get_mission(mid).await {
                                    Ok(Some(mission)) => {
                                        let mut entries = mission.history.clone();
                                        entries.push(MissionHistoryEntry {
                                            role: "assistant".to_string(),
                                            content: agent_result.output.clone(),
                                        });
                                        if let Err(e) =
                                            mission_store.update_mission_history(mid, &entries).await
                                        {
                                            tracing::warn!("Failed to persist mission history: {}", e);
                                        }

                                        let title_empty = mission
                                            .title
                                            .as_ref()
                                            .map(|s| s.trim().is_empty())
                                            .unwrap_or(true);
                                        if title_empty && entries.len() == 2 && entries[0].role == "user"
                                        {
                                            // Use safe_truncate_index for UTF-8 safe truncation
                                            let title = if user_msg.len() > 100 {
                                                let safe_end =
                                                    safe_truncate_index(&user_msg, 100);
                                                format!("{}...", &user_msg[..safe_end])
                                            } else {
                                                user_msg.clone()
                                            };
                                            if let Err(e) =
                                                mission_store.update_mission_title(mid, &title).await
                                            {
                                                tracing::warn!("Failed to update mission title: {}", e);
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        tracing::warn!("Mission {} not found for history append", mid);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to load mission {} for history append: {}",
                                            mid,
                                            e
                                        );
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
                                // Use completed_mission_id (the actual mission that just finished)
                                // instead of current_mission (which can change when user creates a new mission)
                                if let Some(mission_id) = completed_mission_id {
                                    match mission_store.get_mission(mission_id).await {
                                        Ok(Some(mission)) => {
                                            if mission.status == MissionStatus::Active {
                                                let new_status = match agent_result.terminal_reason {
                                                    Some(TerminalReason::Completed) => MissionStatus::Completed,
                                                    Some(TerminalReason::MaxIterations) => MissionStatus::Blocked,
                                                    _ if agent_result.success => MissionStatus::Completed,
                                                    _ => MissionStatus::Failed,
                                                };
                                                tracing::info!(
                                                    "Auto-completing mission {} with status '{:?}' (terminal_reason: {:?})",
                                                    mission_id, new_status, agent_result.terminal_reason
                                                );
                                                if let Err(e) = mission_store
                                                    .update_mission_status(mission_id, new_status)
                                                    .await
                                                {
                                                    tracing::warn!("Failed to auto-complete mission: {}", e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id,
                                                        status: new_status,
                                                        summary: Some(format!(
                                                            "Auto-completed: {}",
                                                            agent_result.output.chars().take(100).collect::<String>()
                                                        )),
                                                    });
                                                }
                                            } else {
                                                tracing::debug!(
                                                    "Skipping auto-complete: mission {} already has status {:?}",
                                                    mission_id, mission.status
                                                );
                                            }
                                        }
                                        Ok(None) => {
                                            tracing::warn!("Mission {} not found for auto-complete", mission_id);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to load mission {} for auto-complete: {}",
                                                mission_id,
                                                e
                                            );
                                        }
                                    }
                                }
                            }

                            // Mark failures as resumable so UI can show a resume button
                            let resumable = !agent_result.success && completed_mission_id.is_some();
                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: Uuid::new_v4(),
                                content: agent_result.output.clone(),
                                success: agent_result.success,
                                cost_cents: agent_result.cost_cents,
                                model: agent_result.model_used,
                                mission_id: completed_mission_id,
                                shared_files: None,
                                resumable,
                            });
                        }
                        Err(e) => {
                            let _ = events_tx.send(AgentEvent::Error {
                                message: format!("Control session task join failed: {}", e),
                                mission_id: completed_mission_id,
                                resumable: completed_mission_id.is_some(), // Can resume if mission exists
                            });
                        }
                    }
                }

                // Start next queued message, if any.
                if let Some((mid, msg, per_msg_agent)) = queue.pop_front() {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Running, queue.len()).await;
                    let current_mid = current_mission.read().await.clone();
                    let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: current_mid });

                    // Immediately persist user message so it's visible when loading mission
                    history.push(("user".to_string(), msg.clone()));
                    persist_mission_history(&mission_store, &current_mission, &history)
                        .await;

                    let cfg = config.clone();
                    let agent = Arc::clone(&root_agent);
                    let mcp_ref = Arc::clone(&mcp);
                    let workspaces_ref = Arc::clone(&workspaces);
                    let library_ref = Arc::clone(&library);
                    let events = events_tx.clone();
                    let tools_hub = Arc::clone(&tool_hub);
                    let status_ref = Arc::clone(&status);
                    let cancel = CancellationToken::new();
                    let hist_snapshot = history.clone();
                    let mission_ctrl = crate::tools::mission::MissionControl {
                        current_mission_id: Arc::clone(&current_mission),
                        cmd_tx: mission_cmd_tx.clone(),
                    };
                    let tree_ref = Arc::clone(&current_tree);
                    let progress_ref = Arc::clone(&progress);
                    running_cancel = Some(cancel.clone());
                    // Capture which mission this task is working on
                    let mission_id = current_mission.read().await.clone();
                    let (workspace_id, model_override, mission_agent) = if let Some(mid) = mission_id {
                        match mission_store.get_mission(mid).await {
                            Ok(Some(mission)) => (
                                Some(mission.workspace_id),
                                mission.model_override.clone(),
                                mission.agent.clone(),
                            ),
                            Ok(None) => {
                                tracing::warn!(
                                    "Mission {} not found while resolving workspace",
                                    mid
                                );
                                (None, None, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to load mission {} for workspace: {}",
                                    mid,
                                    e
                                );
                                (None, None, None)
                            }
                        }
                    } else {
                        (None, None, None)
                    };
                    // Per-message agent overrides mission agent
                    let agent_override = per_msg_agent.or(mission_agent);
                    running_mission_id = mission_id;
                    // Reset activity timer when new task starts to avoid false stall warnings
                    main_runner_last_activity = std::time::Instant::now();
                    running = Some(tokio::spawn(async move {
                        let result = run_single_control_turn(
                            cfg,
                            agent,
                            mcp_ref,
                            workspaces_ref,
                            library_ref,
                            events,
                            tools_hub,
                            status_ref,
                            cancel,
                            hist_snapshot,
                            msg.clone(),
                            Some(mission_ctrl),
                            tree_ref,
                            progress_ref,
                            mission_id,
                            workspace_id,
                            model_override,
                            agent_override,
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
                        if let Some((msg_id, _user_msg, result)) = runner.poll_completion().await {
                            tracing::info!(
                                "Parallel mission {} completed (success: {}, cost: {} cents)",
                                mission_id, result.success, result.cost_cents
                            );

                            // Emit completion event with mission_id
                            // Mark failures as resumable
                            let resumable = !result.success;
                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: msg_id,
                                content: result.output.clone(),
                                success: result.success,
                                cost_cents: result.cost_cents,
                                model: result.model_used.clone(),
                                mission_id: Some(*mission_id),
                                shared_files: None,
                                resumable,
                            });

                            // Persist history for this mission
                            let entries: Vec<MissionHistoryEntry> = runner
                                .history
                                .iter()
                                .map(|(role, content)| MissionHistoryEntry {
                                    role: role.clone(),
                                    content: content.clone(),
                                })
                                .collect();
                            if let Err(e) = mission_store
                                .update_mission_history(*mission_id, &entries)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to persist parallel mission history: {}",
                                    e
                                );
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
            // Update last_activity for runners when we receive events for them
            event = events_rx.recv() => {
                if let Ok(event) = event {
                    // Extract mission_id from event if present
                    let mission_id = match &event {
                        AgentEvent::ToolCall { mission_id, .. } => *mission_id,
                        AgentEvent::ToolResult { mission_id, .. } => *mission_id,
                        AgentEvent::Thinking { mission_id, .. } => *mission_id,
                        AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
                        AgentEvent::AgentTree { mission_id, .. } => *mission_id,
                        AgentEvent::Progress { mission_id, .. } => *mission_id,
                        _ => None,
                    };
                    // Update last_activity for matching runner (main or parallel)
                    if let Some(mid) = mission_id {
                        if running_mission_id == Some(mid) {
                            // Update main runner activity
                            main_runner_last_activity = std::time::Instant::now();
                        } else if let Some(runner) = parallel_runners.get_mut(&mid) {
                            // Update parallel runner activity
                            runner.touch();
                        }
                    }

                    // Track desktop sessions for mission reconnect/resume.
                    if let AgentEvent::ToolResult { name, result, mission_id, .. } = &event {
                        let Some(mid) = mission_id else {
                            continue;
                        };

                        let tool_name = name.as_str();
                        let is_start = matches!(
                            tool_name,
                            "desktop_start_session" | "desktop_desktop_start_session"
                        );
                        let is_stop = matches!(
                            tool_name,
                            "desktop_stop_session"
                                | "desktop_close_session"
                                | "desktop_desktop_stop_session"
                                | "desktop_desktop_close_session"
                        );

                        if !is_start && !is_stop {
                            continue;
                        }

                        let Some(obj) = parse_tool_result_object(result) else {
                            continue;
                        };

                        let Some(display) = obj
                            .get("display")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string())
                        else {
                            continue;
                        };

                        let Ok(Some(mission)) = mission_store.get_mission(*mid).await else {
                            continue;
                        };

                        let mut sessions = mission.desktop_sessions.clone();
                        let now = now_string();

                        if is_start {
                            let resolution = obj
                                .get("resolution")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let screenshots_dir = obj
                                .get("screenshots_dir")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let browser = obj
                                .get("browser")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let url = obj
                                .get("url")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());

                            if let Some(existing) = sessions
                                .iter_mut()
                                .rev()
                                .find(|session| session.display == display && session.stopped_at.is_none())
                            {
                                existing.resolution = resolution;
                                existing.screenshots_dir = screenshots_dir;
                                existing.browser = browser;
                                existing.url = url;
                                existing.started_at = now.clone();
                            } else {
                                sessions.push(DesktopSessionInfo {
                                    display,
                                    resolution,
                                    started_at: now.clone(),
                                    stopped_at: None,
                                    screenshots_dir,
                                    browser,
                                    url,
                                    mission_id: Some(*mid),
                                    keep_alive_until: None,
                                });
                            }
                        } else {
                            if let Some(existing) = sessions
                                .iter_mut()
                                .rev()
                                .find(|session| session.display == display && session.stopped_at.is_none())
                            {
                                existing.stopped_at = Some(now.clone());
                            }
                        }

                        if let Err(err) = mission_store
                            .update_mission_desktop_sessions(*mid, &sessions)
                            .await
                        {
                            tracing::warn!(
                                "Failed to persist desktop session info for mission {}: {}",
                                mid,
                                err
                            );
                        }
                    }
                }
            }
        }
    }
}

async fn run_single_control_turn(
    mut config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    mission_control: Option<crate::tools::mission::MissionControl>,
    tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,
    progress_snapshot: Arc<RwLock<ExecutionProgress>>,
    mission_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    model_override: Option<String>,
    agent_override: Option<String>,
) -> crate::agents::AgentResult {
    if model_override.is_some() {
        config.default_model = model_override;
    }
    if let Some(agent) = agent_override {
        config.opencode_agent = Some(agent);
    }
    // Ensure a workspace directory for this mission (if applicable).
    let (working_dir_path, runtime_workspace) = if let Some(mid) = mission_id {
        let ws = workspace::resolve_workspace(&workspaces, &config, workspace_id).await;
        // Get library for skill syncing
        let lib_guard = library.read().await;
        let lib_ref = lib_guard.as_ref().map(|l| l.as_ref());
        let dir =
            match workspace::prepare_mission_workspace_with_skills(&ws, &mcp, lib_ref, mid).await {
                Ok(dir) => dir,
                Err(e) => {
                    tracing::warn!("Failed to prepare mission workspace: {}", e);
                    ws.path.clone()
                }
            };
        (dir, Some(ws))
    } else {
        (
            config.working_dir.clone(),
            Some(workspace::Workspace::default_host(
                config.working_dir.clone(),
            )),
        )
    };

    if let Some(ws) = runtime_workspace.as_ref() {
        if let Err(e) = workspace::write_runtime_workspace_state(
            &config.working_dir,
            ws,
            &working_dir_path,
            mission_id,
            &config.context.context_dir_name,
        )
        .await
        {
            tracing::warn!("Failed to write runtime workspace state: {}", e);
        }
    }

    // Build a task prompt that includes conversation context with size limits.
    let history_for_prompt = match history.last() {
        Some((role, content)) if role == "user" && content == &user_message => {
            &history[..history.len() - 1]
        }
        _ => history.as_slice(),
    };
    let history_context =
        build_history_context(history_for_prompt, config.context.max_history_total_chars);
    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- Use available tools as needed.\n- For large data processing tasks (>10KB), prefer executing scripts rather than inline processing.\n");
    let mut task = match crate::task::Task::new(convo, Some(1000)) {
        Ok(t) => t,
        Err(e) => {
            let r = crate::agents::AgentResult::failure(format!("Failed to create task: {}", e), 0);
            return r;
        }
    };

    // Context for agent execution.
    let mut ctx = AgentContext::new(config.clone(), working_dir_path);
    ctx.mission_control = mission_control;
    ctx.control_events = Some(events_tx);
    ctx.frontend_tool_hub = Some(tool_hub);
    ctx.control_status = Some(status);
    ctx.cancel_token = Some(cancel);
    ctx.tree_snapshot = Some(tree_snapshot);
    ctx.progress_snapshot = Some(progress_snapshot);
    ctx.mission_id = mission_id;
    ctx.mcp = Some(mcp);

    let result = root_agent.execute(&mut task, &ctx).await;
    result
}
