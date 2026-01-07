//! Core types for the agent system.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(Uuid);

impl AgentId {
    /// Create a new unique agent ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create an agent ID from a string (for testing).
    pub fn from_str(s: &str) -> Self {
        Self(Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v4()))
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Type of agent in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentType {
    /// Root orchestrator (top of tree)
    Root,
    /// Worker agent (delegated execution)
    Worker,
}

impl AgentType {
    /// Check if this is an orchestrator type (can have children).
    pub fn is_orchestrator(&self) -> bool {
        matches!(self, Self::Root)
    }
}

/// Result of an agent executing a task.
///
/// # Invariants
/// - If `success == true`, the task was completed
/// - `cost_cents` reflects actual cost incurred (if known)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// Whether the task was successful
    pub success: bool,

    /// Output or response from the agent
    pub output: String,

    /// Cost incurred in cents
    pub cost_cents: u64,

    /// Model used (if any)
    pub model_used: Option<String>,

    /// Detailed result data (type-specific)
    pub data: Option<serde_json::Value>,

    /// Reason why execution terminated (if not successful completion)
    pub terminal_reason: Option<TerminalReason>,
}

impl AgentResult {
    /// Create a successful result.
    pub fn success(output: impl Into<String>, cost_cents: u64) -> Self {
        Self {
            success: true,
            output: output.into(),
            cost_cents,
            model_used: None,
            data: None,
            terminal_reason: None,
        }
    }

    /// Create a failure result.
    pub fn failure(error: impl Into<String>, cost_cents: u64) -> Self {
        Self {
            success: false,
            output: error.into(),
            cost_cents,
            model_used: None,
            data: None,
            terminal_reason: None,
        }
    }

    /// Add model information to the result.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model_used = Some(model.into());
        self
    }

    /// Add additional data to the result.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Add terminal reason to the result.
    pub fn with_terminal_reason(mut self, reason: TerminalReason) -> Self {
        self.terminal_reason = Some(reason);
        self
    }
}

/// Reason why agent execution terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalReason {
    /// Task completed successfully
    Completed,
    /// Task was cancelled by user
    Cancelled,
    /// LLM/OpenCode API error
    LlmError,
    /// Agent stalled (no progress)
    Stalled,
    /// Detected infinite loop
    InfiniteLoop,
    /// Hit maximum iterations limit
    MaxIterations,
}

/// Errors that can occur in agent operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("Task error: {0}")]
    TaskError(String),

    #[error("No capable agent found for task")]
    NoCapableAgent,

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("Tool error: {0}")]
    ToolError(String),

    #[error("Max iterations reached: {0}")]
    MaxIterations(usize),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<crate::task::TaskError> for AgentError {
    fn from(e: crate::task::TaskError) -> Self {
        Self::TaskError(e.to_string())
    }
}
