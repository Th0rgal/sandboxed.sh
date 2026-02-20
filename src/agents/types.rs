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
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self(Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v4()))
    }
}

impl std::str::FromStr for AgentId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    Actual,
    Estimated,
    #[default]
    Unknown,
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

    /// Cost source provenance
    pub cost_source: CostSource,

    /// Token usage when available
    pub usage: Option<crate::cost::TokenUsage>,

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
            cost_source: CostSource::Unknown,
            usage: None,
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
            cost_source: CostSource::Unknown,
            usage: None,
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

    /// Add usage information.
    pub fn with_usage(mut self, usage: crate::cost::TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Add cost source metadata.
    pub fn with_cost_source(mut self, source: CostSource) -> Self {
        self.cost_source = source;
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
    /// Provider rate-limited all retry attempts
    RateLimited,
    /// Provider rejected turn due to concurrent mission capacity exhaustion
    CapacityLimited,
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
