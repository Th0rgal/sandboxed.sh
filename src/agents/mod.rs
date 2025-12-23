//! Agents module - task execution system.
//!
//! # Agent Types
//! - **SimpleAgent**: Unified agent that directly executes tasks
//! - **TaskExecutor**: Core execution loop with tools (used by SimpleAgent)
//!
//! # Legacy Types (deprecated, will be removed)
//! - **RootAgent**: Complex orchestrator (replaced by SimpleAgent)
//! - **NodeAgent**: Recursive splitter (removed - lost context)
//! - **ComplexityEstimator**: LLM-based estimation (unreliable)
//! - **ModelSelector**: U-curve optimization (over-engineered)
//! - **Verifier**: LLM self-verification (rubber-stamped everything)
//!
//! # Design Principles
//! - Direct execution without orchestration overhead
//! - User controls task granularity (no auto-splitting)
//! - Blocker detection via system prompt rules
//! - Mission completion via complete_mission tool

mod context;
pub mod leaf;
pub mod orchestrator; // TODO: Remove after migration
mod simple;
mod tree;
pub mod tuning;
mod types;

pub use simple::SimpleAgent;

pub use context::AgentContext;
pub use tree::{AgentRef, AgentTree};
pub use tuning::TuningParams;
pub use types::{AgentError, AgentId, AgentResult, AgentType, Complexity};

use crate::task::Task;
use async_trait::async_trait;

/// Base trait for all agents.
///
/// # Invariants
/// - `execute()` returns `Ok` only if the task was actually completed or delegated
/// - `execute()` never panics; all errors are returned as `Err`
///
/// # Design for Provability
/// - Preconditions and postconditions are documented
/// - Pure functions are preferred where possible
#[async_trait]
pub trait Agent: Send + Sync {
    /// Get the unique identifier for this agent.
    fn id(&self) -> &AgentId;

    /// Get the type/role of this agent.
    fn agent_type(&self) -> AgentType;

    /// Execute a task.
    ///
    /// # Preconditions
    /// - `task.budget().remaining_cents() > 0` (has budget)
    /// - `task.status() == Pending || task.status() == Running`
    ///
    /// # Postconditions
    /// - On success: task is completed or delegated appropriately
    /// - `result.cost_cents <= task.budget().total_cents()`
    ///
    /// # Errors
    /// Returns `Err` if:
    /// - Task cannot be executed (insufficient budget, invalid state)
    /// - Execution fails (tool error, LLM error, etc.)
    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult;

    /// Get a human-readable description of this agent.
    fn description(&self) -> &str {
        "Generic agent"
    }
}

/// Trait for orchestrator agents (Root and Node) that can have children.
///
/// # Child Management
/// Orchestrators can delegate work to child agents.
#[async_trait]
pub trait OrchestratorAgent: Agent {
    /// Get references to child agents.
    fn children(&self) -> Vec<AgentRef>;

    /// Find a child agent by capability.
    fn find_child(&self, agent_type: AgentType) -> Option<AgentRef>;

    /// Delegate a task to a specific child.
    ///
    /// # Preconditions
    /// - Child exists and is capable of handling the task
    /// - Task has sufficient budget
    ///
    /// # Postconditions
    /// - Child's execute() is called
    /// - Results are aggregated and returned
    async fn delegate(&self, task: &mut Task, child: AgentRef, ctx: &AgentContext) -> AgentResult;

    /// Delegate multiple tasks to appropriate children.
    ///
    /// # Preconditions
    /// - Sum of task budgets <= available budget
    /// - All tasks can be matched to capable children
    async fn delegate_all(&self, tasks: &mut [Task], ctx: &AgentContext) -> Vec<AgentResult>;
}

/// Trait for leaf agents with specialized capabilities.
pub trait LeafAgent: Agent {
    /// Get the specific capability of this leaf agent.
    fn capability(&self) -> LeafCapability;
}

/// Capabilities of leaf agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeafCapability {
    /// Can estimate task complexity
    ComplexityEstimation,

    /// Can select optimal model for a task
    ModelSelection,

    /// Can execute tasks using tools
    TaskExecution,

    /// Can verify task completion
    Verification,
}

impl LeafCapability {
    /// Get the agent type for this capability.
    pub fn agent_type(&self) -> AgentType {
        match self {
            Self::ComplexityEstimation => AgentType::ComplexityEstimator,
            Self::ModelSelection => AgentType::ModelSelector,
            Self::TaskExecution => AgentType::TaskExecutor,
            Self::Verification => AgentType::Verifier,
        }
    }
}
