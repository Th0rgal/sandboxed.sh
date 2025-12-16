//! Node agent - intermediate orchestrator in the agent tree.
//!
//! Node agents are like mini-root agents that can:
//! - Receive delegated tasks from parent
//! - Split complex subtasks further
//! - Delegate to their own children
//! - Aggregate results for parent

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentRef, AgentResult, AgentType, OrchestratorAgent,
    leaf::{TaskExecutor, Verifier},
};
use crate::task::Task;

/// Node agent - intermediate orchestrator.
/// 
/// # Purpose
/// Handles subtasks that may still be complex enough
/// to warrant further splitting.
/// 
/// # Differences from Root
/// - No complexity estimation (parent already decided to split)
/// - Simpler child set (just executor and verifier)
/// - Limited split depth (prevents infinite recursion)
pub struct NodeAgent {
    id: AgentId,
    
    /// Name for identification in logs
    name: String,
    
    // Child agents
    task_executor: Arc<TaskExecutor>,
    verifier: Arc<Verifier>,
    
    // Child node agents (for further splitting)
    child_nodes: Vec<Arc<NodeAgent>>,
}

impl NodeAgent {
    /// Create a new node agent.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            task_executor: Arc::new(TaskExecutor::new()),
            verifier: Arc::new(Verifier::new()),
            child_nodes: Vec::new(),
        }
    }

    /// Create a node with custom executor.
    pub fn with_executor(mut self, executor: Arc<TaskExecutor>) -> Self {
        self.task_executor = executor;
        self
    }

    /// Add a child node for hierarchical delegation.
    pub fn add_child_node(&mut self, child: Arc<NodeAgent>) {
        self.child_nodes.push(child);
    }

    /// Get the node's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Default for NodeAgent {
    fn default() -> Self {
        Self::new("node")
    }
}

#[async_trait]
impl Agent for NodeAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Node
    }

    fn description(&self) -> &str {
        "Intermediate orchestrator for subtask delegation"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        tracing::debug!("NodeAgent '{}' executing task", self.name);

        // Execute the task
        let result = self.task_executor.execute(task, ctx).await;

        if !result.success {
            return result;
        }

        // Verify if criteria specified
        let verification = self.verifier.execute(task, ctx).await;

        if verification.success {
            result
        } else {
            AgentResult::failure(
                format!(
                    "Task completed but verification failed: {}",
                    verification.output
                ),
                result.cost_cents + verification.cost_cents,
            )
            .with_data(json!({
                "execution": result.data,
                "verification": verification.data,
            }))
        }
    }
}

#[async_trait]
impl OrchestratorAgent for NodeAgent {
    fn children(&self) -> Vec<AgentRef> {
        let mut children: Vec<AgentRef> = vec![
            Arc::clone(&self.task_executor) as AgentRef,
            Arc::clone(&self.verifier) as AgentRef,
        ];

        for node in &self.child_nodes {
            children.push(Arc::clone(node) as AgentRef);
        }

        children
    }

    fn find_child(&self, agent_type: AgentType) -> Option<AgentRef> {
        match agent_type {
            AgentType::TaskExecutor => Some(Arc::clone(&self.task_executor) as AgentRef),
            AgentType::Verifier => Some(Arc::clone(&self.verifier) as AgentRef),
            AgentType::Node => self.child_nodes.first().map(|n| Arc::clone(n) as AgentRef),
            _ => None,
        }
    }

    async fn delegate(&self, task: &mut Task, child: AgentRef, ctx: &AgentContext) -> AgentResult {
        child.execute(task, ctx).await
    }

    async fn delegate_all(&self, tasks: &mut [Task], ctx: &AgentContext) -> Vec<AgentResult> {
        let mut results = Vec::with_capacity(tasks.len());

        for task in tasks {
            let result = self.task_executor.execute(task, ctx).await;
            results.push(result);
        }

        results
    }
}

