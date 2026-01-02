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
    /// Intermediate orchestrator (can have children)
    Node,
    /// Estimates task complexity
    ComplexityEstimator,
    /// Selects optimal model
    ModelSelector,
    /// Executes tasks using tools
    TaskExecutor,
    /// Verifies task completion
    Verifier,
}

impl AgentType {
    /// Check if this is an orchestrator type (can have children).
    pub fn is_orchestrator(&self) -> bool {
        matches!(self, Self::Root | Self::Node)
    }

    /// Check if this is a leaf type.
    pub fn is_leaf(&self) -> bool {
        !self.is_orchestrator()
    }
}

/// Result of an agent executing a task.
///
/// # Invariants
/// - If `success == true`, the task was completed
/// - `cost_cents` reflects actual cost incurred
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

/// Complexity estimation for a task.
///
/// # Invariants
/// - `score` is in range [0.0, 1.0]
/// - `should_split` is derived from score threshold
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Complexity {
    /// Complexity score: 0.0 = trivial, 1.0 = extremely complex
    score: f64,

    /// Human-readable explanation
    reasoning: String,

    /// Whether the task should be split into subtasks
    should_split: bool,

    /// Estimated token count for this task
    estimated_tokens: u64,
}

impl Complexity {
    /// Create a new complexity estimate.
    ///
    /// # Preconditions
    /// - `score` is in [0.0, 1.0] (will be clamped if not)
    ///
    /// # Postconditions
    /// - `self.score` is in [0.0, 1.0]
    /// - `self.should_split` is true if score > threshold (0.6)
    pub fn new(score: f64, reasoning: impl Into<String>, estimated_tokens: u64) -> Self {
        let clamped_score = score.clamp(0.0, 1.0);
        Self {
            score: clamped_score,
            reasoning: reasoning.into(),
            should_split: clamped_score > Self::SPLIT_THRESHOLD,
            estimated_tokens,
        }
    }

    /// Threshold above which tasks should be split.
    pub const SPLIT_THRESHOLD: f64 = 0.6;

    /// Get the complexity score.
    pub fn score(&self) -> f64 {
        self.score
    }

    /// Get the reasoning explanation.
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// Check if the task should be split.
    pub fn should_split(&self) -> bool {
        self.should_split
    }

    /// Get estimated token count.
    pub fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    /// Create a simple (low complexity) estimate.
    pub fn simple(reasoning: impl Into<String>) -> Self {
        Self::new(0.2, reasoning, 500)
    }

    /// Create a moderate complexity estimate.
    pub fn moderate(reasoning: impl Into<String>) -> Self {
        Self::new(0.5, reasoning, 2000)
    }

    /// Create a complex estimate that should be split.
    pub fn complex(reasoning: impl Into<String>) -> Self {
        Self::new(0.8, reasoning, 5000)
    }

    /// Override the should_split decision.
    pub fn with_split(mut self, should_split: bool) -> Self {
        self.should_split = should_split;
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
    /// Budget was exhausted
    BudgetExhausted,
    /// LLM API error
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

    #[error("Budget exhausted: needed {needed} cents, had {available} cents")]
    BudgetExhausted { needed: u64, available: u64 },

    #[error("No capable agent found for task")]
    NoCapableAgent,

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("Tool error: {0}")]
    ToolError(String),

    #[error("Verification failed: {0}")]
    VerificationFailed(String),

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

impl From<crate::budget::BudgetError> for AgentError {
    fn from(_e: crate::budget::BudgetError) -> Self {
        Self::BudgetExhausted {
            needed: 0,
            available: 0,
        }
    }
}
