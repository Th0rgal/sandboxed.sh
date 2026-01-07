//! Core Task type with lightweight cost tracking.
//!
//! # Invariants
//! - `id` is unique within an execution context

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Analysis and telemetry for a task.
///
/// This is mutable, but only via explicit `analysis_mut()` accessor on `Task`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskAnalysis {
    /// User-requested model (if specified)
    pub requested_model: Option<String>,
    /// Model chosen for execution (if selected)
    pub selected_model: Option<String>,
    /// Estimated cost in cents (if computed)
    pub estimated_cost_cents: Option<u64>,
}

/// Lightweight cost tracking for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCost {
    /// Optional budget limit in cents (None = uncapped)
    budget_cents: Option<u64>,
    /// Total spent so far in cents
    spent_cents: u64,
}

impl TaskCost {
    /// Create a new cost tracker with an optional budget cap.
    pub fn new(budget_cents: Option<u64>) -> Self {
        Self {
            budget_cents,
            spent_cents: 0,
        }
    }

    pub fn budget_cents(&self) -> Option<u64> {
        self.budget_cents
    }

    pub fn spent_cents(&self) -> u64 {
        self.spent_cents
    }

    pub fn remaining_cents(&self) -> Option<u64> {
        self.budget_cents
            .map(|budget| budget.saturating_sub(self.spent_cents))
    }

    /// Record additional spend (saturating).
    pub fn record_spend(&mut self, cents: u64) {
        self.spent_cents = self.spent_cents.saturating_add(cents);
    }

    /// Set total spent explicitly (overwrites).
    pub fn set_spent(&mut self, cents: u64) {
        self.spent_cents = cents;
    }
}

/// Unique identifier for a task.
///
/// # Properties
/// - Globally unique within an execution context
/// - Immutable once created
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(Uuid);

impl TaskId {
    /// Create a new unique task ID.
    ///
    /// # Postcondition
    /// Returns a fresh ID that has never been used before in this process.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Get the inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Status of a task in its lifecycle.
///
/// # State Machine
/// ```text
/// Pending -> Running -> Completed
///                   \-> Failed
///        \-> Cancelled
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Task is waiting to be executed
    Pending,
    /// Task is currently being executed
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed with an error
    Failed { reason: String },
    /// Task was cancelled before completion
    Cancelled,
}

impl TaskStatus {
    /// Check if the task is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed { .. } | TaskStatus::Cancelled
        )
    }

    /// Check if the task is still active (can make progress).
    pub fn is_active(&self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::Running)
    }
}

/// A task to be executed by an agent.
///
/// # Design Notes
/// - All fields are immutable after construction (except status via explicit transitions)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier for this task
    id: TaskId,

    /// Human-readable description of what to accomplish
    description: String,

    /// Cost tracking for this task
    cost: TaskCost,

    /// Analysis and telemetry (optional)
    analysis: TaskAnalysis,

    /// Parent task ID if this is a subtask
    parent_id: Option<TaskId>,

    /// Current status
    status: TaskStatus,
}

impl Task {
    /// Create a new task with the given parameters.
    ///
    /// # Preconditions
    /// - `description` is non-empty
    ///
    /// # Postconditions
    /// - Returns a task with `status == Pending`
    /// - `task.id` is a fresh unique identifier
    ///
    /// # Errors
    /// Returns `Err` if preconditions are violated.
    pub fn new(description: String, budget_cents: Option<u64>) -> Result<Self, TaskError> {
        if description.is_empty() {
            return Err(TaskError::EmptyDescription);
        }

        Ok(Self {
            id: TaskId::new(),
            description,
            cost: TaskCost::new(budget_cents),
            analysis: TaskAnalysis::default(),
            parent_id: None,
            status: TaskStatus::Pending,
        })
    }

    // Getters - all return references to preserve immutability semantics

    pub fn id(&self) -> TaskId {
        self.id
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn cost(&self) -> &TaskCost {
        &self.cost
    }

    pub fn cost_mut(&mut self) -> &mut TaskCost {
        &mut self.cost
    }

    pub fn analysis(&self) -> &TaskAnalysis {
        &self.analysis
    }

    pub fn analysis_mut(&mut self) -> &mut TaskAnalysis {
        &mut self.analysis
    }

    pub fn parent_id(&self) -> Option<TaskId> {
        self.parent_id
    }

    pub fn status(&self) -> &TaskStatus {
        &self.status
    }

    /// Check if this task is a subtask (has a parent).
    pub fn is_subtask(&self) -> bool {
        self.parent_id.is_some()
    }

    // State transitions - explicit and validated

    /// Transition the task to Running state.
    pub fn start(&mut self) -> Result<(), TaskError> {
        match &self.status {
            TaskStatus::Pending => {
                self.status = TaskStatus::Running;
                Ok(())
            }
            other => Err(TaskError::InvalidTransition {
                from: format!("{:?}", other),
                to: "Running".to_string(),
            }),
        }
    }

    /// Transition the task to Completed state.
    pub fn complete(&mut self) -> Result<(), TaskError> {
        match &self.status {
            TaskStatus::Running => {
                self.status = TaskStatus::Completed;
                Ok(())
            }
            other => Err(TaskError::InvalidTransition {
                from: format!("{:?}", other),
                to: "Completed".to_string(),
            }),
        }
    }

    /// Transition the task to Failed state.
    pub fn fail(&mut self, reason: String) -> Result<(), TaskError> {
        match &self.status {
            TaskStatus::Running => {
                self.status = TaskStatus::Failed { reason };
                Ok(())
            }
            other => Err(TaskError::InvalidTransition {
                from: format!("{:?}", other),
                to: "Failed".to_string(),
            }),
        }
    }

    /// Transition the task to Cancelled state.
    pub fn cancel(&mut self) -> Result<(), TaskError> {
        if self.status.is_active() {
            self.status = TaskStatus::Cancelled;
            Ok(())
        } else {
            Err(TaskError::InvalidTransition {
                from: format!("{:?}", self.status),
                to: "Cancelled".to_string(),
            })
        }
    }
}

/// Errors that can occur during task operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TaskError {
    #[error("Task description cannot be empty")]
    EmptyDescription,

    #[error("Invalid state transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
}
