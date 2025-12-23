//! Simple agent - streamlined single-agent executor.
//!
//! Replaces the complex RootAgent → NodeAgent → ComplexityEstimator → ModelSelector → TaskExecutor → Verifier
//! hierarchy with a single agent that directly executes tasks.
//!
//! # Why SimpleAgent?
//! The multi-agent hierarchy added overhead without reliable benefits:
//! - ComplexityEstimator: LLM-based estimation was unreliable
//! - ModelSelector: U-curve optimization rarely matched simple "use default" strategy
//! - NodeAgent: Recursive splitting lost context and produced worse results
//! - Verifier: Rubber-stamped everything (LLMs are bad at self-verification)
//!
//! # Design
//! - Direct model selection: mission override > config default
//! - No automatic task splitting (user controls granularity)
//! - Built-in blocker detection via system prompt
//! - Mission completion via complete_mission tool

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType,
    leaf::TaskExecutor,
};
use crate::api::control::AgentTreeNode;
use crate::task::Task;

/// Simple agent - unified executor without orchestration overhead.
///
/// # Execution Flow
/// 1. Resolve model (mission override or config default)
/// 2. Build tree for visualization
/// 3. Execute task via TaskExecutor
/// 4. Return result (no verification layer)
pub struct SimpleAgent {
    id: AgentId,
    task_executor: Arc<TaskExecutor>,
}

impl SimpleAgent {
    /// Create a new simple agent.
    pub fn new() -> Self {
        Self {
            id: AgentId::new(),
            task_executor: Arc::new(TaskExecutor::new()),
        }
    }

    /// Resolve the model to use for execution.
    ///
    /// Priority:
    /// 1. Task's requested model (from mission override)
    /// 2. Config default model (auto-upgraded via resolver)
    async fn resolve_model(&self, task: &Task, ctx: &AgentContext) -> String {
        // Check for explicit model request (from mission override)
        if let Some(requested) = &task.analysis().requested_model {
            // Resolve to latest version if using resolver
            if let Some(resolver) = &ctx.resolver {
                let resolver = resolver.read().await;
                let resolved = resolver.resolve(requested);
                if resolved.upgraded {
                    tracing::info!(
                        "SimpleAgent: requested model auto-upgraded: {} → {}",
                        resolved.original, resolved.resolved
                    );
                }
                return resolved.resolved;
            }
            return requested.clone();
        }

        // Fall back to config default, resolved to latest version
        if let Some(resolver) = &ctx.resolver {
            let resolver = resolver.read().await;
            let resolved = resolver.resolve(&ctx.config.default_model);
            if resolved.upgraded {
                tracing::info!(
                    "SimpleAgent: default model auto-upgraded: {} → {}",
                    resolved.original, resolved.resolved
                );
            }
            resolved.resolved
        } else {
            ctx.config.default_model.clone()
        }
    }

    /// Build a simple agent tree for visualization.
    fn build_tree(&self, task_desc: &str, budget_cents: u64, model: &str) -> AgentTreeNode {
        let mut root = AgentTreeNode::new("root", "Simple", "Simple Agent", task_desc)
            .with_budget(budget_cents, 0)
            .with_status("running");

        // Add executor node
        root.add_child(
            AgentTreeNode::new("executor", "TaskExecutor", "Task Executor", "Executing task")
                .with_budget(budget_cents, 0)
                .with_status("running")
                .with_model(model)
        );

        root
    }
}

impl Default for SimpleAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for SimpleAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Root // Presents as Root for compatibility with tree visualization
    }

    fn description(&self) -> &str {
        "Simple agent: direct task execution without orchestration overhead"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        let task_desc = task.description().chars().take(60).collect::<String>();
        let budget_cents = task.budget().total_cents();

        // Step 1: Resolve model
        let model = self.resolve_model(task, ctx).await;
        
        // Update task analysis with selected model
        task.analysis_mut().selected_model = Some(model.clone());

        tracing::info!(
            "SimpleAgent executing task with model '{}': {}...",
            model,
            task_desc
        );

        // Step 2: Build and emit tree
        let mut tree = self.build_tree(&task_desc, budget_cents, &model);
        ctx.emit_tree(tree.clone());

        // Step 3: Emit phase (for frontend progress indicator)
        ctx.emit_phase("executing", Some("Running task..."), Some("SimpleAgent"));

        // Step 4: Execute via TaskExecutor
        let result = self.task_executor.execute(task, ctx).await;

        // Step 5: Update tree with result
        if let Some(node) = tree.children.iter_mut().find(|n| n.id == "executor") {
            node.status = if result.success { "completed".to_string() } else { "failed".to_string() };
            node.budget_spent = result.cost_cents;
        }
        tree.status = if result.success { "completed".to_string() } else { "failed".to_string() };
        tree.budget_spent = result.cost_cents;
        ctx.emit_tree(tree);

        // Step 6: Return result with metadata
        AgentResult {
            success: result.success,
            output: result.output,
            cost_cents: result.cost_cents,
            model_used: result.model_used.or(Some(model)),
            data: Some(json!({
                "agent": "SimpleAgent",
                "execution": result.data,
            })),
        }
    }
}
