//! Control-session event and command types.
//!
//! Moved verbatim from `control.rs` (Phase 6 of the decomposition):
//! [`AgentEvent`] (the unified event stream), [`TextOp`], [`AgentTreeNode`],
//! [`ControlCommand`], [`UserMessageAck`], and [`MissionStatus`].

#[allow(unused_imports)]
use super::*;

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
        /// Origin of this message for audit/attribution (e.g. `api:<user_id>`,
        /// `task-board`, `telegram`, `relay`). None for internally-generated
        /// control messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    AssistantMessage {
        id: Uuid,
        content: String,
        success: bool,
        cost_cents: u64,
        cost_source: crate::agents::CostSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<crate::cost::TokenUsage>,
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_normalized: Option<String>,
        /// Mission this message belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
        /// Files shared in this message (images, documents, etc.)
        #[serde(skip_serializing_if = "Option::is_none")]
        shared_files: Option<Vec<SharedFile>>,
        /// Whether the mission can be resumed after this failure (only relevant when success=false)
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumable: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        completion_evidence: Option<crate::agents::CompletionEvidence>,
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
    /// CRDT-style text operations for streaming assistant content.
    TextOp {
        mission_id: Uuid,
        bubble_id: String,
        ops: Vec<TextOp>,
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
    /// Goal-mode iteration marker — fired once per turn while a codex
    /// `/goal` continuation loop is active. UI renders as "iter N" pill.
    GoalIteration {
        iteration: u32,
        objective: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Goal status transitioned. Carries the canonical status string from
    /// codex's `thread/goal/updated`: `active`, `paused`, `budgetLimited`,
    /// `complete`, or `cleared` when the goal was explicitly aborted.
    GoalStatus {
        status: String,
        objective: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Mission status changed (by agent or user)
    MissionStatusChanged {
        mission_id: Uuid,
        status: MissionStatus,
        summary: Option<String>,
    },
    /// Mission title changed (by user)
    MissionTitleChanged { mission_id: Uuid, title: String },
    /// Mission metadata changed (title/short description refresh)
    MissionMetadataUpdated {
        mission_id: Uuid,
        title: Option<String>,
        short_description: Option<String>,
        metadata_updated_at: Option<String>,
        updated_at: Option<String>,
        metadata_source: Option<String>,
        metadata_model: Option<String>,
        metadata_version: Option<String>,
    },
    /// Mission run settings changed (backend/model/agent/config profile)
    MissionSettingsUpdated {
        mission_id: Uuid,
        backend: String,
        agent: Option<String>,
        model_override: Option<String>,
        model_effort: Option<String>,
        config_profile: Option<String>,
        session_id: Option<String>,
        updated_at: String,
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
    /// Session ID update (for backends that generate their own session IDs)
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
    /// FIDO signing approval request forwarded to the mobile app
    FidoSignRequest {
        request_id: Uuid,
        key_type: String,
        key_fingerprint: String,
        origin: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        expires_at: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextOp {
    Insert { pos: usize, text: String },
    Replace { range: (usize, usize), text: String },
    Finalize,
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
            AgentEvent::TextOp { .. } => "text_op",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Error { .. } => "error",
            AgentEvent::MissionStatusChanged { .. } => "mission_status_changed",
            AgentEvent::AgentPhase { .. } => "agent_phase",
            AgentEvent::AgentTree { .. } => "agent_tree",
            AgentEvent::Progress { .. } => "progress",
            AgentEvent::SessionIdUpdate { .. } => "session_id_update",
            AgentEvent::MissionActivity { .. } => "mission_activity",
            AgentEvent::MissionTitleChanged { .. } => "mission_title_changed",
            AgentEvent::MissionMetadataUpdated { .. } => "mission_metadata_updated",
            AgentEvent::MissionSettingsUpdated { .. } => "mission_settings_updated",
            AgentEvent::FidoSignRequest { .. } => "fido_sign_request",
            AgentEvent::GoalIteration { .. } => "goal_iteration",
            AgentEvent::GoalStatus { .. } => "goal_status",
        }
    }

    pub fn mission_id(&self) -> Option<Uuid> {
        match self {
            AgentEvent::Status { mission_id, .. } => *mission_id,
            AgentEvent::UserMessage { mission_id, .. } => *mission_id,
            AgentEvent::AssistantMessage { mission_id, .. } => *mission_id,
            AgentEvent::Thinking { mission_id, .. } => *mission_id,
            AgentEvent::TextDelta { mission_id, .. } => *mission_id,
            AgentEvent::TextOp { mission_id, .. } => Some(*mission_id),
            AgentEvent::ToolCall { mission_id, .. } => *mission_id,
            AgentEvent::ToolResult { mission_id, .. } => *mission_id,
            AgentEvent::Error { mission_id, .. } => *mission_id,
            AgentEvent::MissionStatusChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
            AgentEvent::AgentTree { mission_id, .. } => *mission_id,
            AgentEvent::Progress { mission_id, .. } => *mission_id,
            AgentEvent::SessionIdUpdate { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionActivity { mission_id, .. } => *mission_id,
            AgentEvent::MissionTitleChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionMetadataUpdated { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionSettingsUpdated { mission_id, .. } => Some(*mission_id),
            AgentEvent::FidoSignRequest { .. } => None,
            AgentEvent::GoalIteration { mission_id, .. } => *mission_id,
            AgentEvent::GoalStatus { mission_id, .. } => *mission_id,
        }
    }
}

/// Outcome of a [`ControlCommand::UserMessage`], acknowledged on its
/// `respond` channel. Distinguishes "delivered, a turn is starting" from
/// "dropped" — both used to be `false`, which made drops invisible to
/// callers that need delivery guarantees (e.g. the Copilot's steering tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageAck {
    /// The target is busy mid-turn; the message was queued and will be
    /// picked up at the next turn boundary.
    Queued,
    /// The message was delivered and a turn is starting now.
    Delivered,
    /// The message was dropped (parallel cap reached, mission load failure,
    /// rejected goal kickoff, …). An `AgentEvent::Error` with details was
    /// emitted on the event stream.
    Dropped,
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
        /// Control-plane delivery: when true this is a system-generated message
        /// (board wake / worker dispatch) that MUST go only to `target_mission_id`.
        /// The actor skips user-message heuristics for it — no `/goal` rewriting,
        /// no untargeted inference, and no fall-through to the main session: if it
        /// can't reach its exact target it is dropped, never delivered elsewhere.
        /// This keeps the control plane from leaking into unrelated missions.
        strict: bool,
        /// Origin of this message for audit/attribution (e.g. `api:<user_id>`,
        /// `task-board`, `telegram`, `relay`). None for internally-generated
        /// control messages.
        source: Option<String>,
        /// Respond with the delivery outcome (queued / delivered / dropped).
        respond: oneshot::Sender<UserMessageAck>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
        /// Reports whether the result reached a live waiter (`true`) or had to
        /// be cached because no mission was registered for it (`false`).
        respond: oneshot::Sender<bool>,
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
        /// Optional model effort override (e.g. low/medium/high/xhigh/max)
        model_effort: Option<String>,
        /// Backend to use for this mission ("opencode" or "claudecode")
        backend: Option<String>,
        /// Config profile to use for this mission
        config_profile: Option<String>,
        /// Parent mission ID (for orchestrated worker missions)
        parent_mission_id: Option<Uuid>,
        /// Working directory override (for git worktrees etc.)
        working_directory: Option<String>,
        /// FLEET-001 scheduling metadata (priority, not_before, deadline).
        scheduling: crate::api::mission_store::MissionScheduling,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Update mission status
    SetMissionStatus {
        id: Uuid,
        status: MissionStatus,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Update mission title
    SetMissionTitle {
        id: Uuid,
        title: String,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Update mission run settings
    UpdateMissionSettings {
        id: Uuid,
        backend: Option<String>,
        agent: Option<Option<String>>,
        model_override: Option<Option<String>>,
        model_effort: Option<Option<String>>,
        config_profile: Option<Option<String>>,
        session_id: String,
        respond: oneshot::Sender<Result<Mission, String>>,
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
        /// If `Some(d)`, only cancel when the runner has been idle for at
        /// least `d`. Race-protects watchdog/cleanup from killing a
        /// mission that has already resumed activity in the time between
        /// the caller's "stalled" observation and the actor processing
        /// this command. User-initiated cancels pass `None`.
        min_idle: Option<std::time::Duration>,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Pause a mission (FLEET-004). Stops any in-flight runner (main or
    /// parallel) for the mission, then sets its status to `Paused`. Unlike
    /// `CancelMission` (which lands on `Interrupted`), this lands on `Paused`
    /// so the dispatcher skips it until an explicit resume. Routing through the
    /// control loop guarantees the active turn is actually signalled to stop —
    /// a bare store update would leave the runner executing and let its
    /// completion overwrite the paused status.
    PauseMission {
        mission_id: Uuid,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// List currently running missions
    ListRunning {
        respond: oneshot::Sender<Vec<crate::api::mission_runner::RunningMissionInfo>>,
    },
    /// Resume an interrupted mission
    ResumeMission {
        mission_id: Uuid,
        /// If true, clean the mission's work directory before resuming
        clean_workspace: bool,
        /// If true, only update status without sending the automatic resume message
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
    /// Clear queued messages. When `mission_id` is set, only messages
    /// targeting that mission are cleared (main queue entries targeting it +
    /// that mission's parallel-runner queue); otherwise every queue is wiped.
    ClearQueue {
        mission_id: Option<Uuid>,
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
    /// Agent's turn / automation cycle finished cleanly with no follow-up
    /// queued; mission is parked waiting for the user to read it.
    AwaitingUser,
    /// User opened the mission while it was AwaitingUser and the ack grace
    /// period elapsed without a new message — mission is auto-archived.
    Acknowledged,
    Completed,
    Failed,
    /// Mission was interrupted (server shutdown, cancellation, etc.)
    Interrupted,
    /// Mission blocked by external factors (type mismatch, access denied, etc.)
    Blocked,
    /// Mission not feasible as specified (wrong assumptions in request)
    NotFeasible,
    /// Mission explicitly paused by an operator. Unlike `Blocked` this is
    /// NOT terminal: the dispatcher skips it while paused but can resume it
    /// back to `Pending` on demand (see FLEET-004).
    Paused,
}

impl std::fmt::Display for MissionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Active => write!(f, "active"),
            Self::AwaitingUser => write!(f, "awaiting_user"),
            Self::Acknowledged => write!(f, "acknowledged"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Blocked => write!(f, "blocked"),
            Self::NotFeasible => write!(f, "not_feasible"),
            Self::Interrupted => write!(f, "interrupted"),
            Self::Paused => write!(f, "paused"),
        }
    }
}

impl MissionStatus {
    /// Whether this status is terminal — the mission has reached a final
    /// resting state and the dispatcher will never auto-run it again.
    ///
    /// NOTE: `Paused` is deliberately NOT terminal. A paused mission is parked
    /// by an operator and can be resumed back to `Pending` (FLEET-004); the
    /// dispatcher simply skips it while paused. `Blocked` is treated as
    /// terminal here because, unlike `Paused`, it requires external resolution.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Interrupted | Self::Blocked | Self::NotFeasible
        )
    }
}

/// Free-function alias for [`MissionStatus::is_terminal`], kept for call sites
/// that read more clearly as `mission_status_is_terminal(status)`.
pub fn mission_status_is_terminal(status: MissionStatus) -> bool {
    status.is_terminal()
}
