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
use crate::secrets::SecretsStore;
use crate::workspace;

use super::auth::AuthUser;
use super::desktop;
use super::library::SharedLibrary;
use super::mission_store::{
    self, create_mission_store, now_string, Mission, MissionHistoryEntry, MissionStore,
    MissionStoreType, StoredEvent,
};
use super::routes::AppState;

/// Returns a safe index to truncate a string at, ensuring we don't cut UTF-8 characters.
pub(super) fn safe_truncate_index(s: &str, max: usize) -> usize {
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

/// Derive a human-readable activity label from a tool call.
fn activity_label_from_tool_call(tool_name: &str, args: &serde_json::Value) -> String {
    fn extract_str<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
        for key in keys {
            if let Some(v) = args.get(*key).and_then(|v| v.as_str()) {
                return Some(v);
            }
        }
        None
    }

    fn basename(path: &str) -> &str {
        std::path::Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(path)
    }

    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            let end = safe_truncate_index(s, max);
            format!("{}…", &s[..end])
        }
    }

    match tool_name {
        "Bash" | "bash" => {
            let cmd = extract_str(args, &["command"]).unwrap_or("…");
            let first_line = cmd.lines().next().unwrap_or(cmd);
            format!("Running: {}", truncate(first_line, 60))
        }
        "Read" | "read_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Reading: {}", basename(path))
        }
        "Edit" | "edit_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Editing: {}", basename(path))
        }
        "Write" | "write_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Writing: {}", basename(path))
        }
        "Grep" | "grep" | "search" => {
            let pattern = extract_str(args, &["pattern"]).unwrap_or("…");
            format!("Searching: {}", truncate(pattern, 40))
        }
        "Glob" | "glob" => {
            let pattern = extract_str(args, &["pattern"]).unwrap_or("…");
            format!("Finding: {}", truncate(pattern, 50))
        }
        "WebSearch" | "web_search" => {
            let query = extract_str(args, &["query"]).unwrap_or("…");
            format!("Searching web: {}", truncate(query, 40))
        }
        "WebFetch" | "web_fetch" => "Fetching web page".to_string(),
        "Task" | "delegate_task" => {
            let desc = extract_str(args, &["description", "prompt", "subject"]).unwrap_or("…");
            format!("Subtask: {}", truncate(desc, 80))
        }
        "TaskCreate" => {
            let desc = extract_str(args, &["subject", "description"]).unwrap_or("…");
            format!("Creating task: {}", truncate(desc, 80))
        }
        "Skill" => {
            let skill = extract_str(args, &["skill"]).unwrap_or("…");
            format!("Running skill: {}", skill)
        }
        "AskUserQuestion" => "Waiting for input".to_string(),
        "NotebookEdit" => {
            let path = extract_str(args, &["notebook_path"]).unwrap_or("…");
            format!("Editing notebook: {}", basename(path))
        }
        name if name.starts_with("mcp__") => {
            let parts: Vec<&str> = name.splitn(3, "__").collect();
            if parts.len() == 3 {
                format!("{}: {}", parts[1], parts[2])
            } else {
                format!("Tool: {}", name)
            }
        }
        other => format!("Tool: {}", other),
    }
}

/// Extract a concise title from the assistant's first response.
/// Returns the first substantive line, cleaned of markdown formatting.
fn extract_title_from_assistant(content: &str) -> Option<String> {
    // Find the first non-trivial line that isn't a code fence
    let first_line = content
        .lines()
        .map(|l| l.trim())
        .find(|l| l.len() > 5 && !l.starts_with("```"))?;

    // Strip markdown prefixes
    let cleaned = first_line
        .trim_start_matches(|c: char| c == '#' || c == '*' || c == '-' || c == ' ')
        .trim();

    if cleaned.len() < 5 {
        return None;
    }

    let max_len = cleaned.len().min(100);
    let safe_end = safe_truncate_index(cleaned, max_len);
    if safe_end < cleaned.len() {
        Some(format!("{}...", &cleaned[..safe_end]))
    } else {
        Some(cleaned.to_string())
    }
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

pub(crate) async fn resolve_claudecode_default_model(
    library: &SharedLibrary,
    config_profile: Option<&str>,
) -> Option<String> {
    let lib = {
        let guard = library.read().await;
        guard.clone()
    }?;

    let profile = config_profile.unwrap_or("default");
    match lib.get_claudecode_config_for_profile(profile).await {
        Ok(config) => config.default_model.and_then(|model| {
            let trimmed = model.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }),
        Err(err) => {
            tracing::warn!(
                "Failed to load Claude Code config from library (profile: {}): {}",
                profile,
                err
            );
            None
        }
    }
}

async fn close_mission_desktop_sessions(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    working_dir: &std::path::Path,
) {
    let Ok(Some(mission)) = mission_store.get_mission(mission_id).await else {
        return;
    };

    if mission.desktop_sessions.is_empty() {
        return;
    }

    let mut sessions = mission.desktop_sessions.clone();
    let now = now_string();
    let mut updated = false;

    for session in sessions
        .iter_mut()
        .filter(|session| session.stopped_at.is_none())
    {
        if let Err(err) = desktop::close_desktop_session(&session.display, working_dir).await {
            tracing::warn!(
                mission_id = %mission_id,
                display = %session.display,
                error = %err,
                "Failed to close desktop session"
            );
        }
        session.stopped_at = Some(now.clone());
        updated = true;
    }

    if updated {
        if let Err(err) = mission_store
            .update_mission_desktop_sessions(mission_id, &sessions)
            .await
        {
            tracing::warn!(
                mission_id = %mission_id,
                error = %err,
                "Failed to persist desktop session shutdown"
            );
        }
    }
}

/// Message posted by a user to the control session.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlMessageRequest {
    pub content: String,
    /// Optional agent override for this specific message (e.g., from @agent mention)
    #[serde(default)]
    pub agent: Option<String>,
    /// Target mission ID. If provided and differs from the currently running mission,
    /// the backend will automatically start this mission in parallel (if capacity allows).
    #[serde(default)]
    pub mission_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlMessageResponse {
    pub id: Uuid,
    pub queued: bool,
}

/// A message waiting in the queue
#[derive(Debug, Clone, Serialize)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    pub agent: Option<String>,
    /// Which mission this queued message belongs to
    pub mission_id: Option<Uuid>,
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
    /// Text content delta (streaming assistant response)
    TextDelta {
        /// Accumulated text content so far
        content: String,
        /// Mission this text belongs to (for parallel execution)
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
    /// Session ID update (for backends like Amp that generate their own session IDs)
    SessionIdUpdate {
        /// The new session ID to use for continuation
        session_id: String,
        /// Mission this session ID belongs to
        mission_id: Uuid,
    },
    /// Live activity label derived from the current tool call
    MissionActivity {
        /// Human-readable activity label (e.g., "Reading: main.rs")
        label: String,
        /// Tool name that generated this activity
        tool_name: String,
        /// Mission this activity belongs to
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
            AgentEvent::TextDelta { .. } => "text_delta",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Error { .. } => "error",
            AgentEvent::MissionStatusChanged { .. } => "mission_status_changed",
            AgentEvent::AgentPhase { .. } => "agent_phase",
            AgentEvent::AgentTree { .. } => "agent_tree",
            AgentEvent::Progress { .. } => "progress",
            AgentEvent::SessionIdUpdate { .. } => "session_id_update",
            AgentEvent::MissionActivity { .. } => "mission_activity",
        }
    }

    pub fn mission_id(&self) -> Option<Uuid> {
        match self {
            AgentEvent::Status { mission_id, .. } => *mission_id,
            AgentEvent::UserMessage { mission_id, .. } => *mission_id,
            AgentEvent::AssistantMessage { mission_id, .. } => *mission_id,
            AgentEvent::Thinking { mission_id, .. } => *mission_id,
            AgentEvent::TextDelta { mission_id, .. } => *mission_id,
            AgentEvent::ToolCall { mission_id, .. } => *mission_id,
            AgentEvent::ToolResult { mission_id, .. } => *mission_id,
            AgentEvent::Error { mission_id, .. } => *mission_id,
            AgentEvent::MissionStatusChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
            AgentEvent::AgentTree { mission_id, .. } => *mission_id,
            AgentEvent::Progress { mission_id, .. } => *mission_id,
            AgentEvent::SessionIdUpdate { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionActivity { mission_id, .. } => *mission_id,
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
        /// Target mission ID - if provided and differs from running mission, start in parallel
        target_mission_id: Option<Uuid>,
        /// Respond with whether the message was queued (true = waiting to be processed)
        respond: oneshot::Sender<bool>,
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
        /// Backend to use for this mission ("opencode" or "claudecode")
        backend: Option<String>,
        /// Config profile to use for this mission
        config_profile: Option<String>,
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
        /// If true, only update status without sending the "MISSION RESUMED" message
        skip_message: bool,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Graceful shutdown - mark running missions as interrupted
    GracefulShutdown {
        respond: oneshot::Sender<Vec<Uuid>>,
    },
    /// Get the current message queue
    GetQueue {
        respond: oneshot::Sender<Vec<QueuedMessage>>,
    },
    /// Remove a message from the queue
    RemoveFromQueue {
        message_id: Uuid,
        respond: oneshot::Sender<bool>, // true if removed, false if not found
    },
    /// Clear all messages from the queue
    ClearQueue {
        respond: oneshot::Sender<usize>, // number of messages cleared
    },
}

// ==================== Mission Types ====================

/// Mission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    /// Mission created but hasn't received any messages yet
    Pending,
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
            Self::Pending => write!(f, "pending"),
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
///
/// Supports both orderings:
/// - register-then-resolve (normal flow)
/// - resolve-then-register (frontend submits answer before backend registers)
#[derive(Debug)]
pub struct FrontendToolHub {
    pending: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    early_results: Mutex<HashMap<String, serde_json::Value>>,
}

impl FrontendToolHub {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            early_results: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool call that expects a frontend-provided result.
    /// If the result was already submitted (resolve-before-register), it is
    /// delivered immediately.
    pub async fn register(&self, tool_call_id: String) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();

        {
            let mut early = self.early_results.lock().await;
            if let Some(result) = early.remove(&tool_call_id) {
                let _ = tx.send(result);
                return rx;
            }
        }

        let mut pending = self.pending.lock().await;
        pending.insert(tool_call_id, tx);
        rx
    }

    /// Resolve a pending tool call by id.
    /// If no one has registered yet, the result is cached for later pickup.
    pub async fn resolve(&self, tool_call_id: &str, result: serde_json::Value) -> Result<(), ()> {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(tool_call_id) {
            let _ = tx.send(result);
            return Ok(());
        }
        drop(pending);

        let mut early = self.early_results.lock().await;
        const MAX_EARLY_RESULTS: usize = 256;
        if early.len() >= MAX_EARLY_RESULTS {
            tracing::warn!(
                "FrontendToolHub: early_results cache full ({} entries), dropping an entry",
                early.len()
            );
            if let Some(key) = early.keys().next().cloned() {
                early.remove(&key);
            }
        }
        early.insert(tool_call_id.to_string(), result);
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
    /// Mission persistence (SQLite-backed)
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
    secrets: Option<Arc<SecretsStore>>,
}

impl ControlHub {
    pub fn new(
        config: Config,
        root_agent: AgentRef,
        mcp: Arc<McpRegistry>,
        workspaces: workspace::SharedWorkspaceStore,
        library: SharedLibrary,
        secrets: Option<Arc<SecretsStore>>,
    ) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
            root_agent,
            mcp,
            workspaces,
            library,
            secrets,
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

        let base_dir = self
            .config
            .working_dir
            .join(".sandboxed-sh")
            .join("missions");
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
            self.secrets.clone(),
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

        let base_dir = self
            .config
            .working_dir
            .join(".sandboxed-sh")
            .join("missions");

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
    pub mission_id: Option<Uuid>,
}

async fn set_and_emit_status(
    status: &Arc<RwLock<ControlStatus>>,
    events: &broadcast::Sender<AgentEvent>,
    state: ControlRunState,
    queue_len: usize,
    mission_id: Option<Uuid>,
) {
    {
        let mut s = status.write().await;
        s.state = state;
        s.queue_len = queue_len;
        s.mission_id = mission_id;
    }
    let _ = events.send(AgentEvent::Status {
        state,
        queue_len,
        mission_id,
    });
}

async fn control_for_user(state: &Arc<AppState>, user: &AuthUser) -> ControlState {
    state.control.get_or_spawn(user).await
}

/// Enqueue a user message for the global control session.
/// If mission_id is provided and differs from the currently running mission,
/// the backend will automatically start it in parallel (if capacity allows).
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
    let target_mission_id = req.mission_id;
    let control = control_for_user(&state, &user).await;
    let (queued_tx, queued_rx) = oneshot::channel();
    tracing::info!(
        user_id = %user.id,
        username = %user.username,
        message_id = %id,
        content_len = content.len(),
        agent = ?agent,
        target_mission_id = ?target_mission_id,
        "Received control message"
    );
    control
        .cmd_tx
        .send(ControlCommand::UserMessage {
            id,
            content,
            agent,
            target_mission_id,
            respond: queued_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    let queued = match queued_rx.await {
        Ok(value) => value,
        Err(_) => {
            let status = control.status.read().await;
            status.state != ControlRunState::Idle
        }
    };
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

// ==================== Queue Management Endpoints ====================

/// Get the current message queue.
pub async fn get_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<QueuedMessage>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::GetQueue { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    let queue = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to get queue".to_string(),
        )
    })?;
    Ok(Json(queue))
}

/// Remove a message from the queue.
pub async fn remove_from_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::RemoveFromQueue {
            message_id,
            respond: tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    let removed = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to remove from queue".to_string(),
        )
    })?;
    if removed {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        Err((StatusCode::NOT_FOUND, "message not in queue".to_string()))
    }
}

/// Clear all messages from the queue.
pub async fn clear_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::ClearQueue { respond: tx })
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "control session unavailable".to_string(),
            )
        })?;
    let cleared = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to clear queue".to_string(),
        )
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "cleared": cleared })))
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
    /// Optional model override (provider/model) - deprecated, use config_profile instead
    pub model_override: Option<String>,
    /// Config profile to use for this mission (overrides workspace's default profile)
    pub config_profile: Option<String>,
    /// Backend to use for this mission ("opencode" or "claudecode")
    pub backend: Option<String>,
}

pub async fn create_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    body: Option<Json<CreateMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let (title, workspace_id, agent, model_override, config_profile, mut backend) = body
        .map(|b| {
            (
                b.title.clone(),
                b.workspace_id,
                b.agent.clone(),
                b.model_override.clone(),
                b.config_profile.clone(),
                b.backend.clone(),
            )
        })
        .unwrap_or((None, None, None, None, None, None));

    let mut model_override = model_override;
    if let Some(value) = backend.as_ref() {
        if value.trim().is_empty() {
            backend = None;
        }
    }
    if let Some(value) = model_override.as_ref() {
        if value.trim().is_empty() {
            model_override = None;
        }
    }

    // Validate agent exists before creating mission (fail fast with clear error)
    // Skip validation for Claude Code and Amp - they have their own built-in agents
    if let Some(ref agent_name) = agent {
        let backend_id = backend.as_deref();
        let skip_validation = backend_id == Some("claudecode") || backend_id == Some("amp");
        if !skip_validation {
            super::library::validate_agent_exists(&state, agent_name)
                .await
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        }
    }

    if let Some(ref backend_id) = backend {
        let registry = state.backend_registry.read().await;
        if registry.get(backend_id).is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Unknown backend: {}", backend_id),
            ));
        }
    }

    // Resolve the effective config profile:
    // 1. Use explicit config_profile from request if provided
    // 2. Otherwise use workspace's config_profile
    // 3. Fall back to "default"
    let effective_config_profile = if let Some(ref profile) = config_profile {
        Some(profile.clone())
    } else if let Some(ws_id) = workspace_id {
        state
            .workspaces
            .get(ws_id)
            .await
            .and_then(|ws| ws.config_profile)
    } else {
        None
    };

    // If no model_override specified, resolve from config profile for Claude Code
    if backend.as_deref() == Some("claudecode") && model_override.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&state.library, effective_config_profile.as_deref())
                .await
        {
            model_override = Some(default_model);
        }
    }

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::CreateMission {
            title,
            workspace_id,
            agent,
            model_override,
            backend,
            config_profile: effective_config_profile,
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

/// Get OpenCode diagnostics.
/// Note: With per-mission CLI execution, there's no central server to diagnose.
/// This endpoint now returns information about the execution mode.
pub async fn get_opencode_diagnostics(
    State(_state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<OpenCodeDiagnostics>, (StatusCode, String)> {
    // Per-mission CLI execution doesn't use a central server
    Ok(Json(OpenCodeDiagnostics {
        base_url: "per-mission-cli-mode".to_string(),
        session_id: None,
        session_status: None,
        error: Some(
            "Per-mission CLI mode: No central server. Each mission spawns its own CLI process."
                .to_string(),
        ),
    }))
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
    /// If true, only update the mission status without sending the "MISSION RESUMED" message.
    /// Useful when the user is about to send their own custom message.
    #[serde(default)]
    pub skip_message: bool,
}

/// Resume an interrupted mission.
/// This reconstructs context from history and work directory, then restarts execution.
pub async fn resume_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    body: Option<Json<ResumeMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (clean_workspace, skip_message) = body
        .map(|b| (b.clean_workspace, b.skip_message))
        .unwrap_or((false, false));
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::ResumeMission {
            mission_id,
            clean_workspace,
            skip_message,
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
            .json_data(AgentEvent::Status { state: initial.state, queue_len: initial.queue_len, mission_id: initial.mission_id })
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
    secrets: Option<Arc<SecretsStore>>,
) -> ControlState {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ControlCommand>(256);
    let (events_tx, events_rx) = broadcast::channel::<AgentEvent>(1024);
    let tool_hub = Arc::new(FrontendToolHub::new());
    let status = Arc::new(RwLock::new(ControlStatus {
        state: ControlRunState::Idle,
        queue_len: 0,
        mission_id: None,
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
        secrets,
    ));

    // Recover orphaned missions from previous run.
    // Any mission still marked "active" in the DB cannot be running because
    // we just started — mark them as interrupted.
    if state.mission_store.is_persistent() {
        let store = Arc::clone(&state.mission_store);
        let tx = events_tx.clone();
        tokio::spawn(async move {
            match store.get_all_active_missions().await {
                Ok(orphans) if !orphans.is_empty() => {
                    tracing::info!(
                        "Startup recovery: marking {} orphaned active missions as interrupted",
                        orphans.len()
                    );
                    for mission in orphans {
                        tracing::info!(
                            "  → {} '{}' (last update: {})",
                            mission.id,
                            mission.title.as_deref().unwrap_or("Untitled"),
                            mission.updated_at
                        );
                        if let Err(e) = store
                            .update_mission_status(mission.id, MissionStatus::Interrupted)
                            .await
                        {
                            tracing::warn!(
                                "Failed to mark orphaned mission {} as interrupted: {}",
                                mission.id,
                                e
                            );
                        } else {
                            let _ = tx.send(AgentEvent::MissionStatusChanged {
                                mission_id: mission.id,
                                status: MissionStatus::Interrupted,
                                summary: Some(
                                    "Interrupted: server restarted while mission was active"
                                        .to_string(),
                                ),
                            });
                        }
                    }
                }
                Ok(_) => {
                    tracing::debug!("Startup recovery: no orphaned active missions found");
                }
                Err(e) => {
                    tracing::warn!(
                        "Startup recovery: failed to check for orphaned missions: {}",
                        e
                    );
                }
            }
        });
    }

    // Spawn background stale mission cleanup task (if enabled)
    if config.stale_mission_hours > 0 && state.mission_store.is_persistent() {
        tokio::spawn(stale_mission_cleanup_loop(
            Arc::clone(&state.mission_store),
            config.stale_mission_hours,
            Arc::clone(&running_missions),
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

/// Background task that periodically cleans up missions that are no longer running.
///
/// Two checks on each tick:
/// 1. **Orphan detection**: any mission marked `active` in the DB but not present
///    in the in-memory `running_missions` list is an orphan whose harness process
///    died without updating the DB. These are marked `interrupted` immediately.
/// 2. **Stale timeout**: missions that have been active longer than `stale_hours`
///    without any activity update are marked `completed` as a safety net.
async fn stale_mission_cleanup_loop(
    mission_store: Arc<dyn MissionStore>,
    stale_hours: u64,
    running_missions: Arc<RwLock<Vec<super::mission_runner::RunningMissionInfo>>>,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    // Check every 5 minutes (fast enough to catch orphans promptly).
    let check_interval = std::time::Duration::from_secs(300);

    tracing::info!(
        "Mission cleanup task started: orphan check every 5 min, stale timeout {} hours",
        stale_hours
    );

    loop {
        tokio::time::sleep(check_interval).await;

        // --- Orphan detection: active in DB but not running in-process ---
        match mission_store.get_all_active_missions().await {
            Ok(active_missions) if !active_missions.is_empty() => {
                let running = running_missions.read().await;
                let running_ids: std::collections::HashSet<Uuid> =
                    running.iter().map(|r| r.mission_id).collect();
                drop(running);

                for mission in &active_missions {
                    if !running_ids.contains(&mission.id) {
                        tracing::info!(
                            "Orphan detected: mission {} '{}' is active in DB but has no running process (last update: {})",
                            mission.id,
                            mission.title.as_deref().unwrap_or("Untitled"),
                            mission.updated_at
                        );
                        if let Err(e) = mission_store
                            .update_mission_status(mission.id, MissionStatus::Interrupted)
                            .await
                        {
                            tracing::warn!(
                                "Failed to mark orphaned mission {} as interrupted: {}",
                                mission.id,
                                e
                            );
                        } else {
                            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                mission_id: mission.id,
                                status: MissionStatus::Interrupted,
                                summary: Some(
                                    "Interrupted: harness process is no longer running".to_string(),
                                ),
                            });
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Failed to check for orphaned missions: {}", e);
            }
        }

        // --- Stale timeout: safety net for missions that somehow stay active ---
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
    secrets: Option<Arc<SecretsStore>>,
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
    // Track current activity label for the main runner
    let mut main_runner_activity: Option<String> = None;
    // Track subtasks for the main runner
    let mut main_runner_subtasks: Vec<super::mission_runner::SubtaskInfo> = Vec::new();

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

            // Auto-generate title from conversation if not set
            if history.len() >= 2 {
                let should_update = mission_store
                    .get_mission(mid)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|m| m.title)
                    .map(|t| t.trim().is_empty())
                    .unwrap_or(true);
                if should_update {
                    // Prefer assistant's opening line (often a summary of intent)
                    let title = history
                        .iter()
                        .rev()
                        .find(|(role, _)| role == "assistant")
                        .and_then(|(_, content)| extract_title_from_assistant(content))
                        .unwrap_or_else(|| {
                            // Fall back to user's first message
                            let user_content = &history[0].1;
                            if user_content.len() > 100 {
                                let safe_end = safe_truncate_index(user_content, 100);
                                format!("{}...", &user_content[..safe_end])
                            } else {
                                user_content.clone()
                            }
                        });
                    if let Err(e) = mission_store.update_mission_title(mid, &title).await {
                        tracing::warn!("Failed to update mission title: {}", e);
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
        create_new_mission_with_title(mission_store, None, None, None, None, None, None).await
    }

    // Helper to create a new mission with title
    async fn create_new_mission_with_title(
        mission_store: &Arc<dyn MissionStore>,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
    ) -> Result<Mission, String> {
        mission_store
            .create_mission(
                title,
                workspace_id,
                agent,
                model_override,
                backend,
                config_profile,
            )
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

        // Check if mission can be resumed (interrupted, blocked, or failed)
        // Failed missions can be resumed to retry after transient errors (e.g., 529 overloaded)
        if !matches!(
            mission.status,
            MissionStatus::Interrupted | MissionStatus::Blocked | MissionStatus::Failed
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
            MissionStatus::Failed => "failed due to an error (retrying)",
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
                    let end = safe_truncate_index(&last_assistant.content, 2000);
                    format!("{}...", &last_assistant.content[..end])
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
                            || dir_name == ".sandboxed_sh"
                            || dir_name == ".sandboxed-sh"
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
                    ControlCommand::UserMessage { id, content, agent: msg_agent, target_mission_id, respond } => {
                        // Smart routing: decide where to send this message based on target_mission_id
                        // and what's currently running.

                        let current_mission_id = current_mission.read().await.clone();
                        let running_mid = running_mission_id;
                        let main_mission_id = if running_mid.is_some() {
                            running_mid
                        } else {
                            current_mission_id
                        };
                        let main_is_running = running.is_some();

                        // If no explicit target but current_mission differs from the
                        // running mission (i.e., CreateMission switched the pointer),
                        // infer the target as current_mission so it auto-starts in parallel.
                        let effective_target = target_mission_id.or_else(|| {
                            if main_is_running {
                                if let Some(cid) = current_mission_id {
                                    if running_mid != Some(cid) {
                                        tracing::info!(
                                            "Inferred target mission {} (current differs from running {:?})",
                                            cid, running_mid
                                        );
                                        return Some(cid);
                                    }
                                }
                            }
                            None
                        });

                        // Determine if target is already running somewhere
                        let target_in_parallel = effective_target
                            .map(|tid| parallel_runners.contains_key(&tid))
                            .unwrap_or(false);
                        let target_is_main = effective_target
                            .map(|tid| main_mission_id == Some(tid))
                            .unwrap_or(true); // No target = use main

                        // Case 1: Target is already running in parallel_runners - queue to it
                        if let Some(tid) = effective_target {
                            if target_in_parallel {
                                if let Some(runner) = parallel_runners.get_mut(&tid) {
                                    let was_running = runner.is_running();
                                    runner.queue_message(id, content.clone(), msg_agent);
                                    let _ = events_tx.send(AgentEvent::UserMessage {
                                        id,
                                        content: content.clone(),
                                        queued: was_running,
                                        mission_id: Some(tid),
                                    });
                                    // Try to start if not already running
                                    if !runner.is_running() {
                                        runner.start_next(
                                            config.clone(),
                                            Arc::clone(&root_agent),
                                            Arc::clone(&mcp),
                                            Arc::clone(&workspaces),
                                            library.clone(),
                                            events_tx.clone(),
                                            Arc::clone(&tool_hub),
                                            Arc::clone(&status),
                                            mission_cmd_tx.clone(),
                                            Arc::new(RwLock::new(Some(tid))),
                                            secrets.clone(),
                                        );
                                    }
                                    let _ = respond.send(was_running);
                                    continue;
                                }
                            }
                        }

                        // Case 2: Target differs from main AND main is running → start parallel
                        if let Some(tid) = effective_target {
                            if !target_is_main && main_is_running {
                                // Check capacity
                                let parallel_running = parallel_runners.values().filter(|r| r.is_running()).count();
                                let total_running = parallel_running + 1; // +1 for main
                                let max_parallel = config.max_parallel_missions;

                                if total_running >= max_parallel {
                                    tracing::warn!(
                                        "Cannot start parallel mission {}: max {} reached",
                                        tid, max_parallel
                                    );
                                    // Fall through to queue on main as fallback
                                } else {
                                    // Load mission and start in parallel
                                    match load_mission_record(&mission_store, tid).await {
                                        Ok(mission) => {
                                            // Activate mission: if pending, interrupted, or blocked, update status to active
                                            if matches!(mission.status, MissionStatus::Pending | MissionStatus::Interrupted | MissionStatus::Blocked) {
                                                tracing::info!("Activating parallel mission {} (was {})", tid, mission.status);
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate parallel mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                            let mut runner = super::mission_runner::MissionRunner::new(
                                                tid,
                                                mission.workspace_id,
                                                mission.agent.clone(),
                                                Some(mission.backend.clone()),
                                                mission.session_id.clone(),
                                                mission.config_profile.clone(),
                                            );
                                            // Load existing history
                                            for entry in &mission.history {
                                                runner.history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            // Queue the message
                                            runner.queue_message(id, content.clone(), msg_agent);
                                            // Emit user message event
                                            let _ = events_tx.send(AgentEvent::UserMessage {
                                                id,
                                                content: content.clone(),
                                                queued: false,
                                                mission_id: Some(tid),
                                            });
                                            // Start execution
                                            runner.start_next(
                                                config.clone(),
                                                Arc::clone(&root_agent),
                                                Arc::clone(&mcp),
                                                Arc::clone(&workspaces),
                                                library.clone(),
                                                events_tx.clone(),
                                                Arc::clone(&tool_hub),
                                                Arc::clone(&status),
                                                mission_cmd_tx.clone(),
                                                Arc::new(RwLock::new(Some(tid))),
                                                secrets.clone(),
                                            );
                                            tracing::info!("Auto-started mission {} in parallel", tid);
                                            parallel_runners.insert(tid, runner);
                                            let _ = respond.send(false);
                                            continue;
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to load mission {} for parallel: {}", tid, e);
                                            // Fall through to queue on main as fallback
                                        }
                                    }
                                }
                            }
                        }

                        // Case 3: Queue to main session (default behavior)
                        // Auto-create mission on first message if none exists
                        {
                            let mission_id = current_mission.read().await.clone();
                            if mission_id.is_none() {
                                // Use effective_target if available, otherwise create new
                                if let Some(tid) = effective_target {
                                    // Load mission history from DB so continuation detection
                                    // works correctly (e.g., after server restart when
                                    // current_mission is None but the mission has prior turns).
                                    if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                        if !mission.history.is_empty() {
                                            history.clear();
                                            for entry in &mission.history {
                                                history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            tracing::info!(
                                                "Loaded {} history entries for target mission {} (first message after session start)",
                                                mission.history.len(), tid
                                            );
                                        }
                                        // Activate mission if it was interrupted/blocked
                                        if matches!(mission.status, MissionStatus::Pending | MissionStatus::Interrupted | MissionStatus::Blocked) {
                                            tracing::info!("Activating main mission {} (was {})", tid, mission.status);
                                            if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                tracing::warn!("Failed to activate main mission {}: {}", tid, e);
                                            } else {
                                                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                    mission_id: tid,
                                                    status: MissionStatus::Active,
                                                    summary: None,
                                                });
                                            }
                                        }
                                    }
                                    *current_mission.write().await = Some(tid);
                                    tracing::info!("Set current mission to target: {}", tid);
                                } else if let Ok(new_mission) = create_new_mission(&mission_store).await {
                                    *current_mission.write().await = Some(new_mission.id);
                                    tracing::info!("Auto-created mission: {}", new_mission.id);
                                }
                            } else if let Some(tid) = effective_target {
                                if !main_is_running {
                                    if mission_id != Some(tid) {
                                        // Switch main session to target mission
                                        persist_mission_history(&mission_store, &current_mission, &history).await;
                                        if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                            history.clear();
                                            for entry in &mission.history {
                                                history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            // Activate mission if it was interrupted/blocked
                                            if matches!(mission.status, MissionStatus::Pending | MissionStatus::Interrupted | MissionStatus::Blocked) {
                                                tracing::info!("Activating switched mission {} (was {})", tid, mission.status);
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate switched mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                        }
                                        *current_mission.write().await = Some(tid);
                                        tracing::info!("Switched main session to mission: {}", tid);
                                    } else if !history.iter().any(|(role, _)| role == "assistant") {
                                        // Same mission but no assistant history in memory
                                        // (e.g., after server restart). Reload from database
                                        // so Claude Code continuation detection works correctly.
                                        if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                            if !mission.history.is_empty() {
                                                history.clear();
                                                for entry in &mission.history {
                                                    history.push((entry.role.clone(), entry.content.clone()));
                                                }
                                                tracing::info!(
                                                    "Reloaded {} history entries for mission {} (session continuity)",
                                                    mission.history.len(), tid
                                                );
                                            }
                                            // Activate mission if it was interrupted/blocked (same mission, reloading)
                                            if matches!(mission.status, MissionStatus::Pending | MissionStatus::Interrupted | MissionStatus::Blocked) {
                                                tracing::info!("Activating reloaded mission {} (was {})", tid, mission.status);
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate reloaded mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let was_running = running.is_some();
                        let content_clone = content.clone();
                        queue.push_back((id, content, msg_agent));
                        let status_mission_id = if running.is_some() {
                            running_mission_id
                        } else {
                            current_mission.read().await.clone()
                        };
                        set_and_emit_status(
                            &status,
                            &events_tx,
                            if running.is_some() { ControlRunState::Running } else { ControlRunState::Idle },
                            queue.len(),
                            status_mission_id,
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
                                let current_mid = current_mission.read().await.clone();
                                set_and_emit_status(
                                    &status,
                                    &events_tx,
                                    ControlRunState::Running,
                                    queue.len(),
                                    current_mid,
                                ).await;
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
                                let (workspace_id, model_override, mission_agent, backend_id, session_id, mission_config_profile) = if let Some(mid) = mission_id {
                                    match mission_store.get_mission(mid).await {
                                        Ok(Some(mission)) => {
                                            // Activate mission: if pending, interrupted, or blocked, update status to active
                                            if matches!(mission.status, MissionStatus::Pending | MissionStatus::Interrupted | MissionStatus::Blocked) {
                                                tracing::info!("Activating mission {} (was {})", mid, mission.status);
                                                if let Err(e) = mission_store.update_mission_status(mid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate mission {}: {}", mid, e);
                                                } else {
                                                    // Notify frontend of status change
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: mid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                            (
                                                Some(mission.workspace_id),
                                                mission.model_override.clone(),
                                                mission.agent.clone(),
                                                Some(mission.backend.clone()),
                                                mission.session_id.clone(),
                                                mission.config_profile.clone(),
                                            )
                                        }
                                        Ok(None) => {
                                            tracing::warn!(
                                                "Mission {} not found while resolving workspace",
                                                mid
                                            );
                                            (None, None, None, None, None, None)
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to load mission {} for workspace: {}",
                                                mid,
                                                e
                                            );
                                            (None, None, None, None, None, None)
                                        }
                                    }
                                } else {
                                    (None, None, None, None, None, None)
                                };
                                // Per-message agent overrides mission agent
                                let agent_override = per_msg_agent.or(mission_agent);
                                running_cancel = Some(cancel.clone());
                                running_mission_id = mission_id;
                                // Reset activity tracking when new task starts
                                main_runner_last_activity = std::time::Instant::now();
                                main_runner_activity = None;
                                main_runner_subtasks.clear();
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
                                        backend_id,
                                        model_override,
                                        agent_override,
                                        session_id,
                                        false, // force_session_resume: regular message, not a resume
                                        mission_config_profile,
                                    )
                                    .await;
                                    (mid, msg, result)
                                }));
                            } else {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0, None).await;
                            }
                        }
                        let _ = respond.send(was_running);
                    }
                    ControlCommand::ToolResult { tool_call_id, name, result } => {
                        // Deliver to the tool hub. resolve() caches the result if
                        // no one has registered yet (resolve-before-register).
                        let _ = tool_hub.resolve(&tool_call_id, result).await;
                        tracing::debug!(tool_call_id = %tool_call_id, name = %name, "ToolResult delivered to hub");
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

                                // Write runtime workspace state so file uploads work immediately
                                // (without needing to send a message first)
                                let ws = workspace::resolve_workspace(
                                    &workspaces,
                                    &config,
                                    Some(mission.workspace_id),
                                ).await;
                                if let Err(e) = workspace::write_runtime_workspace_state(
                                    &config.working_dir,
                                    &ws,
                                    &ws.path,
                                    Some(id),
                                    &config.context.context_dir_name,
                                ).await {
                                    tracing::warn!("Failed to write runtime workspace state on load: {}", e);
                                }

                                let _ = respond.send(Ok(mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::CreateMission { title, workspace_id, agent, model_override, backend, config_profile, respond } => {
                        // First persist current mission history
                        persist_mission_history(
                            &mission_store,
                            &current_mission,
                            &history,
                        )
                        .await;

                        // Create a new mission with optional title, workspace, agent, and backend
                        match create_new_mission_with_title(
                            &mission_store,
                            title.as_deref(),
                            workspace_id,
                            agent.as_deref(),
                            model_override.as_deref(),
                            backend.as_deref(),
                            config_profile.as_deref(),
                        )
                        .await {
                            Ok(mission) => {
                                history.clear();
                                *current_mission.write().await = Some(mission.id);

                                // Write runtime workspace state so file uploads work immediately
                                let ws = workspace::resolve_workspace(
                                    &workspaces,
                                    &config,
                                    Some(mission.workspace_id),
                                ).await;
                                if let Err(e) = workspace::write_runtime_workspace_state(
                                    &config.working_dir,
                                    &ws,
                                    &ws.path,
                                    Some(mission.id),
                                    &config.context.context_dir_name,
                                ).await {
                                    tracing::warn!("Failed to write runtime workspace state on create: {}", e);
                                }

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
                                Some(mission.backend.clone()),
                                mission.session_id.clone(),
                                mission.config_profile.clone(),
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
                                secrets.clone(),
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
                            close_mission_desktop_sessions(
                                &mission_store,
                                mission_id,
                                &config.working_dir,
                            )
                            .await;
                            let _ = respond.send(Ok(()));
                        } else {
                            // Check if this is the currently executing mission
                            // Use running_mission_id (the actual mission being executed)
                            // instead of current_mission (which can change when user creates a new mission)
                            if running_mission_id == Some(mission_id) {
                                // Cancel the current execution
                                if let Some(token) = &running_cancel {
                                    token.cancel();
                                    close_mission_desktop_sessions(
                                        &mission_store,
                                        mission_id,
                                        &config.working_dir,
                                    )
                                    .await;
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
                                    current_activity: main_runner_activity.clone(),
                                    subtask_total: main_runner_subtasks.len(),
                                    subtask_completed: main_runner_subtasks.iter().filter(|s| s.completed).count(),
                                });
                            }
                        }

                        // Add all parallel runners
                        for runner in parallel_runners.values() {
                            running_list.push(super::mission_runner::RunningMissionInfo::from(runner));
                        }

                        let _ = respond.send(running_list);
                    }
                    ControlCommand::ResumeMission { mission_id, clean_workspace, skip_message, respond } => {
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
                                } else {
                                    // Send status changed event so UI updates
                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                        mission_id,
                                        status: MissionStatus::Active,
                                        summary: None,
                                    });
                                }

                                // Queue the resume prompt as a message (no per-message agent override)
                                // Skip if the caller just wants to update the status (e.g., before sending a custom message)
                                if !skip_message {
                                    let msg_id = Uuid::new_v4();
                                    queue.push_back((msg_id, resume_prompt, None));
                                }

                                // Start execution if not already running
                                if running.is_none() {
                                    if let Some((mid, msg, _per_msg_agent)) = queue.pop_front() {
                                        set_and_emit_status(
                                            &status,
                                            &events_tx,
                                            ControlRunState::Running,
                                            queue.len(),
                                            Some(mission_id),
                                        ).await;
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
                                        let backend_id = Some(mission.backend.clone());
                                        let model_override = mission.model_override.clone();
                                        // Resume uses mission agent (no per-message override for resumes)
                                        let agent_override = mission.agent.clone();
                                        let session_id = mission.session_id.clone();
                                        let mission_config_profile = mission.config_profile.clone();
                                        running_cancel = Some(cancel.clone());
                                        // Capture which mission this task is working on (the resumed mission)
                                        running_mission_id = Some(mission_id);
                                        main_runner_activity = None;
                                        main_runner_subtasks.clear();
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
                                                backend_id,
                                                model_override,
                                                agent_override,
                                                session_id,
                                                true, // force_session_resume: this is a resume operation
                                                mission_config_profile,
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
                    ControlCommand::GetQueue { respond } => {
                        // Collect queued messages from main runner
                        let mut queued: Vec<QueuedMessage> = queue
                            .iter()
                            .map(|(id, content, agent)| QueuedMessage {
                                id: *id,
                                content: content.clone(),
                                agent: agent.clone(),
                                mission_id: running_mission_id,
                            })
                            .collect();
                        // Also collect queued messages from parallel runners
                        for (mid, runner) in parallel_runners.iter() {
                            for qm in runner.queue.iter() {
                                queued.push(QueuedMessage {
                                    id: qm.id,
                                    content: qm.content.clone(),
                                    agent: qm.agent.clone(),
                                    mission_id: Some(*mid),
                                });
                            }
                        }
                        let _ = respond.send(queued);
                    }
                    ControlCommand::RemoveFromQueue { message_id, respond } => {
                        let before_len = queue.len();
                        queue.retain(|(id, _, _)| *id != message_id);
                        let removed = queue.len() < before_len;
                        if removed {
                            // Emit event to notify frontend
                            let _ = events_tx.send(AgentEvent::Status {
                                state: if running.is_some() {
                                    ControlRunState::Running
                                } else {
                                    ControlRunState::Idle
                                },
                                queue_len: queue.len(),
                                mission_id: current_mission.read().await.clone(),
                            });
                        }
                        let _ = respond.send(removed);
                    }
                    ControlCommand::ClearQueue { respond } => {
                        let cleared = queue.len();
                        queue.clear();
                        // Emit event to notify frontend
                        let _ = events_tx.send(AgentEvent::Status {
                            state: if running.is_some() {
                                ControlRunState::Running
                            } else {
                                ControlRunState::Idle
                            },
                            queue_len: 0,
                            mission_id: current_mission.read().await.clone(),
                        });
                        let _ = respond.send(cleared);
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
                    main_runner_activity = None;
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
                                        if title_empty && entries.len() >= 2 {
                                            // Prefer assistant's opening line, fall back to user message
                                            let title = entries
                                                .iter()
                                                .rev()
                                                .find(|e| e.role == "assistant")
                                                .and_then(|e| extract_title_from_assistant(&e.content))
                                                .unwrap_or_else(|| {
                                                    if user_msg.len() > 100 {
                                                        let safe_end =
                                                            safe_truncate_index(&user_msg, 100);
                                                        format!("{}...", &user_msg[..safe_end])
                                                    } else {
                                                        user_msg.clone()
                                                    }
                                                });
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
                                            // Auto-complete if mission is Active OR Interrupted (resumed missions may
                                            // still have Interrupted status if the status update event was not persisted)
                                            if matches!(mission.status, MissionStatus::Active | MissionStatus::Interrupted) {
                                                let new_status = match agent_result.terminal_reason {
                                                    Some(TerminalReason::Completed) => MissionStatus::Completed,
                                                    Some(TerminalReason::MaxIterations) => MissionStatus::Blocked,
                                                    _ if agent_result.success => MissionStatus::Completed,
                                                    _ => MissionStatus::Failed,
                                                };
                                                // Convert terminal_reason to string for storage
                                                let terminal_reason_str = agent_result.terminal_reason.map(|r| match r {
                                                    TerminalReason::Completed => "completed",
                                                    TerminalReason::Cancelled => "cancelled",
                                                    TerminalReason::LlmError => "llm_error",
                                                    TerminalReason::Stalled => "stalled",
                                                    TerminalReason::InfiniteLoop => "infinite_loop",
                                                    TerminalReason::MaxIterations => "max_iterations",
                                                });
                                                tracing::info!(
                                                    "Auto-completing mission {} with status '{:?}' (terminal_reason: {:?})",
                                                    mission_id, new_status, agent_result.terminal_reason
                                                );
                                                if let Err(e) = mission_store
                                                    .update_mission_status_with_reason(mission_id, new_status, terminal_reason_str)
                                                    .await
                                                {
                                                    tracing::warn!("Failed to auto-complete mission: {}", e);
                                                } else {
                                                    // Send status change event - the actual completion content
                                                    // is already in the assistant_message event, so we just provide
                                                    // a clean summary based on how the mission ended
                                                    let summary = match agent_result.terminal_reason {
                                                        Some(TerminalReason::Completed) => None, // Normal completion, no extra explanation needed
                                                        Some(TerminalReason::MaxIterations) => Some("Reached iteration limit".to_string()),
                                                        Some(TerminalReason::Cancelled) => Some("Cancelled by user".to_string()),
                                                        Some(TerminalReason::Stalled) => Some("No progress detected".to_string()),
                                                        Some(TerminalReason::InfiniteLoop) => Some("Detected repetitive behavior".to_string()),
                                                        Some(TerminalReason::LlmError) => Some("Model error".to_string()),
                                                        None if agent_result.success => None,
                                                        None => Some("Unexpected termination".to_string()),
                                                    };
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id,
                                                        status: new_status,
                                                        summary,
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
                            if let Some(mission_id) = completed_mission_id {
                                close_mission_desktop_sessions(
                                    &mission_store,
                                    mission_id,
                                    &config.working_dir,
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let _ = events_tx.send(AgentEvent::Error {
                                message: format!("Control session task join failed: {}", e),
                                mission_id: completed_mission_id,
                                resumable: completed_mission_id.is_some(), // Can resume if mission exists
                            });
                            if let Some(mission_id) = completed_mission_id {
                                close_mission_desktop_sessions(
                                    &mission_store,
                                    mission_id,
                                    &config.working_dir,
                                )
                                .await;
                            }
                        }
                    }
                }

                // Start next queued message, if any.
                if let Some((mid, msg, per_msg_agent)) = queue.pop_front() {
                    let current_mid = current_mission.read().await.clone();
                    set_and_emit_status(
                        &status,
                        &events_tx,
                        ControlRunState::Running,
                        queue.len(),
                        current_mid,
                    ).await;
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
                    let (workspace_id, model_override, mission_agent, backend_id, session_id, mission_config_profile) = if let Some(mid) = mission_id {
                        match mission_store.get_mission(mid).await {
                            Ok(Some(mission)) => (
                                Some(mission.workspace_id),
                                mission.model_override.clone(),
                                mission.agent.clone(),
                                Some(mission.backend.clone()),
                                mission.session_id.clone(),
                                mission.config_profile.clone(),
                            ),
                            Ok(None) => {
                                tracing::warn!(
                                    "Mission {} not found while resolving workspace",
                                    mid
                                );
                                (None, None, None, None, None, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to load mission {} for workspace: {}",
                                    mid,
                                    e
                                );
                                (None, None, None, None, None, None)
                            }
                        }
                    } else {
                        (None, None, None, None, None, None)
                    };
                    // Per-message agent overrides mission agent
                    let agent_override = per_msg_agent.or(mission_agent);
                    running_mission_id = mission_id;
                    // Reset activity tracking when new task starts
                    main_runner_last_activity = std::time::Instant::now();
                    main_runner_activity = None;
                    main_runner_subtasks.clear();
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
                            backend_id,
                            model_override,
                            agent_override,
                            session_id,
                            false, // force_session_resume: continuation turn, not a resume
                            mission_config_profile,
                        )
                        .await;
                        (mid, msg, result)
                    }));
                } else {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0, None).await;
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

                    // --- Activity tracking & subtask detection ---
                    match &event {
                        AgentEvent::ToolCall { name, args, tool_call_id, mission_id } => {
                            if let Some(mid) = mission_id {
                                let label = activity_label_from_tool_call(name, args);

                                // Update activity on runner
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = Some(label.clone());
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = Some(label.clone());
                                }

                                // Emit activity event for real-time SSE
                                let _ = events_tx.send(AgentEvent::MissionActivity {
                                    label,
                                    tool_name: name.clone(),
                                    mission_id: Some(*mid),
                                });

                                // Subtask detection
                                let is_subtask = matches!(name.as_str(),
                                    "Task" | "delegate_task" | "TaskCreate" | "Skill"
                                );
                                if is_subtask {
                                    let desc: String = args.get("description")
                                        .or_else(|| args.get("subject"))
                                        .or_else(|| args.get("prompt"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Subtask")
                                        .chars().take(120).collect();
                                    let info = super::mission_runner::SubtaskInfo {
                                        tool_call_id: tool_call_id.clone(),
                                        description: desc,
                                        completed: false,
                                    };
                                    let (total, completed) = if running_mission_id == Some(*mid) {
                                        main_runner_subtasks.push(info);
                                        (main_runner_subtasks.len(), main_runner_subtasks.iter().filter(|s| s.completed).count())
                                    } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                        runner.subtasks.push(info);
                                        (runner.subtasks.len(), runner.subtasks.iter().filter(|s| s.completed).count())
                                    } else {
                                        (0, 0)
                                    };
                                    if total > 0 {
                                        let _ = events_tx.send(AgentEvent::Progress {
                                            total_subtasks: total,
                                            completed_subtasks: completed,
                                            current_subtask: None,
                                            depth: 0,
                                            mission_id: Some(*mid),
                                        });
                                    }
                                }

                                // Desktop session detection from ToolCall.
                                // Claude Code and Amp don't emit ToolResult for MCP tools,
                                // so we detect the session start from the ToolCall and
                                // spawn a background task to attribute Xvfb processes.
                                let is_desktop_start = matches!(
                                    name.as_str(),
                                    "desktop_start_session"
                                        | "desktop_desktop_start_session"
                                        | "mcp__desktop__desktop_start_session"
                                );
                                if is_desktop_start {
                                    let store = mission_store.clone();
                                    let mid = *mid;
                                    tokio::spawn(async move {
                                        // Wait for Xvfb to start
                                        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

                                        // Scan for running Xvfb displays
                                        let displays =
                                            super::desktop::get_running_xvfb_displays().await;
                                        if displays.is_empty() {
                                            tracing::debug!(
                                                "No Xvfb displays found for desktop attribution"
                                            );
                                            return;
                                        }

                                        // Load current mission sessions
                                        let Ok(Some(mission)) = store.get_mission(mid).await
                                        else {
                                            return;
                                        };
                                        let mut sessions = mission.desktop_sessions.clone();
                                        let tracked: std::collections::HashSet<String> =
                                            sessions.iter().map(|s| s.display.clone()).collect();

                                        let mut changed = false;
                                        for disp in displays {
                                            if !tracked.contains(&disp) {
                                                sessions.push(DesktopSessionInfo {
                                                    display: disp.clone(),
                                                    resolution: None,
                                                    started_at: now_string(),
                                                    stopped_at: None,
                                                    screenshots_dir: None,
                                                    browser: None,
                                                    url: None,
                                                    mission_id: Some(mid),
                                                    keep_alive_until: None,
                                                });
                                                changed = true;
                                                tracing::info!(
                                                    display_id = %disp,
                                                    mission_id = %mid,
                                                    "Desktop session attributed from ToolCall"
                                                );
                                            }
                                        }

                                        if changed {
                                            if let Err(err) = store
                                                .update_mission_desktop_sessions(mid, &sessions)
                                                .await
                                            {
                                                tracing::warn!(
                                                    "Failed to persist desktop session from ToolCall for mission {}: {}",
                                                    mid,
                                                    err
                                                );
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        AgentEvent::ToolResult { tool_call_id, mission_id, .. } => {
                            if let Some(mid) = mission_id {
                                // Clear activity label (tool finished)
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = None;
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = None;
                                }

                                // Mark subtask complete if applicable
                                let subtasks: Option<&mut Vec<super::mission_runner::SubtaskInfo>> =
                                    if running_mission_id == Some(*mid) {
                                        Some(&mut main_runner_subtasks)
                                    } else {
                                        parallel_runners.get_mut(mid).map(|r| &mut r.subtasks)
                                    };
                                if let Some(subtasks) = subtasks {
                                    let mut changed = false;
                                    for s in subtasks.iter_mut() {
                                        if s.tool_call_id == *tool_call_id && !s.completed {
                                            s.completed = true;
                                            changed = true;
                                            break;
                                        }
                                    }
                                    if changed {
                                        let total = subtasks.len();
                                        let completed = subtasks.iter().filter(|s| s.completed).count();
                                        let _ = events_tx.send(AgentEvent::Progress {
                                            total_subtasks: total,
                                            completed_subtasks: completed,
                                            current_subtask: None,
                                            depth: 0,
                                            mission_id: Some(*mid),
                                        });
                                    }
                                }
                            }
                        }
                        AgentEvent::Thinking { done, mission_id, .. } => {
                            if let Some(mid) = mission_id {
                                let label = if *done { None } else { Some("Thinking…".to_string()) };
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = label;
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = label;
                                }
                            }
                        }
                        _ => {}
                    }

                    // Track desktop sessions for mission reconnect/resume.
                    if let AgentEvent::ToolResult { name, result, mission_id, .. } = &event {
                        let Some(mid) = mission_id else {
                            continue;
                        };

                        let tool_name = name.as_str();
                        let is_start = matches!(
                            tool_name,
                            "desktop_start_session"
                                | "desktop_desktop_start_session"
                                | "mcp__desktop__desktop_start_session"
                        );
                        let is_stop = matches!(
                            tool_name,
                            "desktop_stop_session"
                                | "desktop_close_session"
                                | "desktop_desktop_stop_session"
                                | "desktop_desktop_close_session"
                                | "mcp__desktop__desktop_stop_session"
                                | "mcp__desktop__desktop_close_session"
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

                    // Handle session ID updates (for backends like Amp that generate their own IDs)
                    if let AgentEvent::SessionIdUpdate { mission_id, session_id } = &event {
                        if let Err(err) = mission_store
                            .update_mission_session_id(*mission_id, session_id)
                            .await
                        {
                            tracing::warn!(
                                "Failed to update session ID for mission {}: {}",
                                mission_id,
                                err
                            );
                        } else {
                            tracing::debug!(
                                mission_id = %mission_id,
                                session_id = %session_id,
                                "Updated mission session ID from backend"
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
    backend_id: Option<String>,
    model_override: Option<String>,
    agent_override: Option<String>,
    session_id: Option<String>,
    force_session_resume: bool,
    mission_config_profile: Option<String>,
) -> crate::agents::AgentResult {
    let is_claudecode = backend_id.as_deref() == Some("claudecode");
    // Get config profile: mission's config_profile takes priority over workspace's
    let workspace_config_profile = if let Some(ws_id) = workspace_id {
        workspaces.get(ws_id).await.and_then(|ws| ws.config_profile)
    } else {
        None
    };
    let effective_config_profile = mission_config_profile.or(workspace_config_profile);
    if let Some(model) = model_override {
        config.default_model = Some(model);
    } else if is_claudecode && config.default_model.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&library, effective_config_profile.as_deref()).await
        {
            config.default_model = Some(default_model);
        }
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
        let dir = match workspace::prepare_mission_workspace_with_skills_backend(
            &ws,
            &mcp,
            lib_ref,
            mid,
            backend_id.as_deref().unwrap_or("opencode"),
            None, // custom_providers: TODO integrate with provider store
            effective_config_profile.as_deref(),
        )
        .await
        {
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
    ctx.control_events = Some(events_tx.clone());
    ctx.frontend_tool_hub = Some(tool_hub.clone());
    ctx.control_status = Some(status);
    ctx.cancel_token = Some(cancel.clone());
    ctx.tree_snapshot = Some(tree_snapshot);
    ctx.progress_snapshot = Some(progress_snapshot);
    ctx.mission_id = mission_id;
    ctx.mcp = Some(mcp);

    let fallback_workspace = workspace::Workspace::default_host(config.working_dir.clone());
    let exec_workspace = runtime_workspace.as_ref().unwrap_or(&fallback_workspace);

    // Execute based on backend
    let result = match backend_id.as_deref() {
        Some("claudecode") => {
            let mid = match mission_id {
                Some(id) => id,
                None => {
                    let _ = events_tx.send(AgentEvent::Error {
                        message: "Claude Code backend requires a mission ID".to_string(),
                        mission_id: None,
                        resumable: false,
                    });
                    return crate::agents::AgentResult::failure(
                        "Claude Code backend requires a mission ID".to_string(),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::LlmError);
                }
            };
            // Check if this is a continuation turn (has prior assistant response).
            // Note: history may include the current user message before the turn runs,
            // so we check for assistant messages to determine if this is truly a continuation.
            // Also use --resume if force_session_resume is set (e.g., for mission resume operations
            // where the session exists but history may not have assistant messages yet).
            let is_continuation =
                force_session_resume || history.iter().any(|(role, _)| role == "assistant");
            super::mission_runner::run_claudecode_turn(
                exec_workspace,
                &ctx.working_dir,
                &user_message,
                config.default_model.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel,
                None, // secrets - not available in control context
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                Some(tool_hub.clone()),
            )
            .await
        }
        Some("amp") => {
            let mid = match mission_id {
                Some(id) => id,
                None => {
                    let _ = events_tx.send(AgentEvent::Error {
                        message: "Amp backend requires a mission ID".to_string(),
                        mission_id: None,
                        resumable: false,
                    });
                    return crate::agents::AgentResult::failure(
                        "Amp backend requires a mission ID".to_string(),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::LlmError);
                }
            };
            let is_continuation =
                force_session_resume || history.iter().any(|(role, _)| role == "assistant");
            let api_key = super::mission_runner::get_amp_api_key_from_config();
            super::mission_runner::run_amp_turn(
                exec_workspace,
                &ctx.working_dir,
                &user_message,
                config.opencode_agent.as_deref(), // mode (smart/rush)
                mid,
                events_tx.clone(),
                cancel,
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                api_key.as_deref(),
            )
            .await
        }
        Some(backend) if backend != "opencode" => {
            let _ = events_tx.send(AgentEvent::Error {
                message: format!("Unsupported backend: {}", backend),
                mission_id,
                resumable: mission_id.is_some(),
            });
            crate::agents::AgentResult::failure(format!("Unsupported backend: {}", backend), 0)
                .with_terminal_reason(TerminalReason::LlmError)
        }
        _ => {
            // Default to opencode using per-workspace CLI execution
            let mid = mission_id.unwrap_or_else(Uuid::nil);
            super::mission_runner::run_opencode_turn(
                exec_workspace,
                &ctx.working_dir,
                &user_message,
                config.default_model.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel,
                &config.working_dir,
            )
            .await
        }
    };
    result
}
