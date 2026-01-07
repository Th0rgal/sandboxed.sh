//! Agents module - task execution via OpenCode.
//!
//! # Agent Types
//! - **OpenCodeAgent**: Delegates task execution to an OpenCode server

mod context;
mod opencode;
mod types;

use std::sync::Arc;

pub use opencode::OpenCodeAgent;

pub use context::AgentContext;
pub use types::{AgentError, AgentId, AgentResult, AgentType, TerminalReason};

use crate::task::Task;
use async_trait::async_trait;

/// Reference to an agent (thread-safe shared pointer).
pub type AgentRef = Arc<dyn Agent>;

/// Base trait for all agents.
///
/// # Invariants
/// - `execute()` returns `Ok` only if the task was actually completed or delegated
/// - `execute()` never panics; all errors are returned as `Err`
#[async_trait]
pub trait Agent: Send + Sync {
    /// Get the unique identifier for this agent.
    fn id(&self) -> &AgentId;

    /// Get the type/role of this agent.
    fn agent_type(&self) -> AgentType;

    /// Execute a task.
    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult;

    /// Get a human-readable description of this agent.
    fn description(&self) -> &str {
        "Generic agent"
    }
}
