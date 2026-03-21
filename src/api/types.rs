//! API request and response types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request to submit a new task.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskRequest {
    /// The task description / user prompt (displayed as label in dashboard)
    pub task: String,

    /// Optional model override (uses default if not specified, agent mode only)
    pub model: Option<String>,

    /// Optional working directory for relative paths (agent has full system access regardless)
    pub working_dir: Option<String>,

    /// Optional budget limit in cents (default: 1000 = $10, tracking only, agent mode only)
    pub budget_cents: Option<u64>,

    /// Shell command to run in a workspace container (command mode).
    /// When present, runs the command directly instead of an OpenCode agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Workspace to run the command in (required when command is set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,

    /// Timeout in seconds for command mode (default: 1800 = 30 min; 0 or absent → default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Statistics response.
#[derive(Debug, Clone, Serialize)]
pub struct StatsResponse {
    /// Total number of tasks ever created
    pub total_tasks: usize,

    /// Number of currently running tasks
    pub active_tasks: usize,

    /// Number of completed tasks
    pub completed_tasks: usize,

    /// Number of failed tasks
    pub failed_tasks: usize,

    /// Total cost spent in cents
    pub total_cost_cents: u64,

    /// Cost breakdown by source provenance
    pub actual_cost_cents: u64,
    pub estimated_cost_cents: u64,
    pub unknown_cost_cents: u64,

    /// Success rate (0.0 - 1.0)
    pub success_rate: f64,
}

/// Response after creating a task.
#[derive(Debug, Clone, Serialize)]
pub struct CreateTaskResponse {
    /// Unique task identifier
    pub id: Uuid,

    /// Current task status
    pub status: TaskStatus,
}

/// Task execution mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    Agent,
    Command,
}

/// Task status enumeration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is queued, waiting to start
    Pending,
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed with an error
    Failed,
    /// Task was cancelled
    Cancelled,
}

/// A structured step annotation emitted by a command-mode script.
/// Scripts can print {"step": "name", "status": "started"|"completed"|"failed", ...}
/// JSON lines to stdout and the dashboard will render them as a timeline.
#[derive(Debug, Clone, Serialize)]
pub struct TaskStep {
    /// Step name (e.g. "generate_draft", "llm_judge")
    pub name: String,
    /// Iteration index (for looping eval scripts)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
    /// "started" | "completed" | "failed"
    pub status: String,
    /// ISO 8601 timestamp when step started
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// ISO 8601 timestamp when step finished
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Duration in seconds (from script annotation or computed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    /// Arbitrary metadata (score, pass_rate, feedback, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Full task state including results.
#[derive(Debug, Serialize)]
pub struct TaskState {
    /// Unique task identifier
    pub id: Uuid,

    /// Current status
    pub status: TaskStatus,

    /// Original task description / label
    pub task: String,

    /// Task execution mode
    pub mode: TaskMode,

    /// Model used for this task (agent mode only)
    pub model: String,

    /// Number of iterations completed (agent mode)
    pub iterations: usize,

    /// Workspace the command runs in (command mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,

    /// Workspace name resolved for display (command mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,

    /// Final result or error message
    pub result: Option<String>,

    /// Detailed execution log
    pub log: Vec<TaskLogEntry>,

    /// Structured steps parsed from stdout JSON annotations (command mode)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<TaskStep>,

    /// When the task was created (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,

    /// When the task started running (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// When the task reached a terminal state (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,

    /// Wall-clock seconds from started_at to completed_at
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,

    /// Cancel signal sender — used by stop_task to abort command-mode tasks.
    /// Not serialized; consumed once when cancel is requested.
    #[serde(skip)]
    pub cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// A single entry in the task execution log.
#[derive(Debug, Clone, Serialize)]
pub struct TaskLogEntry {
    /// Timestamp (ISO 8601)
    pub timestamp: String,

    /// Entry type
    pub entry_type: LogEntryType,

    /// Content of the entry
    pub content: String,
}

/// Types of log entries.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEntryType {
    /// Agent is thinking / planning
    Thinking,
    /// Tool is being called
    ToolCall,
    /// Tool returned a result
    ToolResult,
    /// Agent produced final response
    Response,
    /// An error occurred
    Error,
}

/// Server-Sent Event for streaming task progress.
#[derive(Debug, Clone, Serialize)]
pub struct TaskEvent {
    /// Event type
    pub event: String,

    /// Event data (JSON serialized)
    pub data: serde_json::Value,
}

/// Health check response.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    /// Service status
    pub status: String,

    /// Service version
    pub version: String,

    /// Whether the server is running in dev mode (auth disabled)
    pub dev_mode: bool,

    /// Whether auth is required for API requests (dev_mode=false)
    pub auth_required: bool,

    /// Authentication mode ("disabled", "single_tenant", "multi_user")
    pub auth_mode: String,

    /// Maximum iterations per agent (from MAX_ITERATIONS env var)
    pub max_iterations: usize,

    /// Configured library remote URL (from LIBRARY_REMOTE env var)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_remote: Option<String>,
}

/// Login request for dashboard auth.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
}

/// Login response containing a JWT for API authentication.
#[derive(Debug, Clone, Serialize)]
pub struct LoginResponse {
    pub token: String,
    /// Expiration as unix seconds.
    pub exp: i64,
}
