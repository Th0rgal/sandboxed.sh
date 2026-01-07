//! API request and response types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request to submit a new task.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskRequest {
    /// The task description / user prompt
    pub task: String,

    /// Optional model override (uses default if not specified)
    pub model: Option<String>,

    /// Optional working directory for relative paths (agent has full system access regardless)
    pub working_dir: Option<String>,

    /// Optional budget limit in cents (default: 1000 = $10, tracking only)
    pub budget_cents: Option<u64>,
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

/// Full task state including results.
#[derive(Debug, Clone, Serialize)]
pub struct TaskState {
    /// Unique task identifier
    pub id: Uuid,

    /// Current status
    pub status: TaskStatus,

    /// Original task description
    pub task: String,

    /// Model used for this task
    pub model: String,

    /// Number of iterations completed
    pub iterations: usize,

    /// Final result or error message
    pub result: Option<String>,

    /// Detailed execution log
    pub log: Vec<TaskLogEntry>,
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
