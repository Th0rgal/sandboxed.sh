//! Root agent - top-level orchestrator of the agent tree.
//!
//! # Responsibilities
//! 1. Receive tasks from the API
//! 2. Estimate complexity
//! 3. Decide: execute directly or split into subtasks
//! 4. Delegate to appropriate children
//! 5. Aggregate results

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentRef, AgentResult, AgentType, Complexity,
    OrchestratorAgent,
    leaf::{ComplexityEstimator, ModelSelector, TaskExecutor, Verifier},
};
use crate::agents::tuning::TuningParams;
use crate::budget::Budget;
use crate::task::{Task, Subtask, SubtaskPlan, VerificationCriteria};

/// Root agent - the top of the agent tree.
/// 
/// # Task Processing Flow
/// ```text
/// 1. Estimate complexity (ComplexityEstimator)
/// 2. If simple: execute directly (TaskExecutor)
/// 3. If complex: 
///    a. Split into subtasks (LLM-based)
///    b. Select model for each subtask (ModelSelector)
///    c. Execute subtasks (TaskExecutor)
///    d. Verify results (Verifier)
/// 4. Return aggregated result
/// ```
pub struct RootAgent {
    id: AgentId,
    
    // Child agents
    complexity_estimator: Arc<ComplexityEstimator>,
    model_selector: Arc<ModelSelector>,
    task_executor: Arc<TaskExecutor>,
    verifier: Arc<Verifier>,
}

impl RootAgent {
    /// Create a new root agent with default children.
    pub fn new() -> Self {
        Self::new_with_tuning(&TuningParams::default())
    }

    /// Create a new root agent using empirically tuned parameters.
    pub fn new_with_tuning(tuning: &TuningParams) -> Self {
        Self {
            id: AgentId::new(),
            complexity_estimator: Arc::new(ComplexityEstimator::with_params(
                tuning.complexity.prompt_variant,
                tuning.complexity.split_threshold,
                tuning.complexity.token_multiplier,
            )),
            model_selector: Arc::new(ModelSelector::with_params(
                tuning.model_selector.retry_multiplier,
                tuning.model_selector.inefficiency_scale,
                tuning.model_selector.max_failure_probability,
            )),
            task_executor: Arc::new(TaskExecutor::new()),
            verifier: Arc::new(Verifier::new()),
        }
    }

    /// Estimate complexity of a task.
    async fn estimate_complexity(&self, task: &mut Task, ctx: &AgentContext) -> Complexity {
        let result = self.complexity_estimator.execute(task, ctx).await;
        
        if let Some(data) = result.data {
            let score = data["score"].as_f64().unwrap_or(0.5);
            let reasoning = data["reasoning"].as_str().unwrap_or("").to_string();
            let estimated_tokens = data["estimated_tokens"].as_u64().unwrap_or(2000);
            let should_split = data["should_split"].as_bool().unwrap_or(false);
            
            Complexity::new(score, reasoning, estimated_tokens)
                .with_split(should_split)
        } else {
            Complexity::moderate("Could not estimate complexity")
        }
    }

    /// Split a complex task into subtasks.
    /// 
    /// Uses LLM to analyze the task and propose subtasks.
    async fn split_task(&self, task: &Task, ctx: &AgentContext) -> Result<SubtaskPlan, AgentResult> {
        let prompt = format!(
            r#"You are a task planner. Break down this task into smaller, manageable subtasks.

Task: {}

Respond with a JSON object:
{{
    "subtasks": [
        {{
            "description": "What to do",
            "verification": "How to verify it's done",
            "weight": 1.0
        }}
    ],
    "reasoning": "Why this breakdown makes sense"
}}

Guidelines:
- Each subtask should be independently executable
- Include verification for each subtask
- Weight indicates relative effort (higher = more work)
- Keep subtasks focused and specific

Respond ONLY with the JSON object."#,
            task.description()
        );

        let messages = vec![
            crate::llm::ChatMessage::new(crate::llm::Role::System, "You are a precise task planner. Respond only with JSON."),
            crate::llm::ChatMessage::new(crate::llm::Role::User, prompt),
        ];

        let response = ctx.llm
            .chat_completion("openai/gpt-4.1-mini", &messages, None)
            .await
            .map_err(|e| AgentResult::failure(format!("LLM error: {}", e), 1))?;

        let content = response.content.unwrap_or_default();
        self.parse_subtask_plan(&content, task.id())
    }

    /// Extract JSON from LLM response (handles markdown code blocks).
    fn extract_json(response: &str) -> String {
        let trimmed = response.trim();
        
        // Check for markdown code block
        if trimmed.starts_with("```") {
            // Find the end of the opening fence
            if let Some(start_idx) = trimmed.find('\n') {
                let after_fence = &trimmed[start_idx + 1..];
                // Find the closing fence
                if let Some(end_idx) = after_fence.rfind("```") {
                    return after_fence[..end_idx].trim().to_string();
                }
            }
        }
        
        // Try to find JSON object in the response
        if let Some(start) = trimmed.find('{') {
            if let Some(end) = trimmed.rfind('}') {
                if end > start {
                    return trimmed[start..=end].to_string();
                }
            }
        }
        
        // Return as-is if no extraction needed
        trimmed.to_string()
    }

    /// Parse LLM response into SubtaskPlan.
    fn parse_subtask_plan(
        &self,
        response: &str,
        parent_id: crate::task::TaskId,
    ) -> Result<SubtaskPlan, AgentResult> {
        let extracted = Self::extract_json(response);
        let json: serde_json::Value = serde_json::from_str(&extracted)
            .map_err(|e| AgentResult::failure(format!("Failed to parse subtasks: {} (raw: {}...)", e, response.chars().take(100).collect::<String>()), 0))?;

        let reasoning = json["reasoning"]
            .as_str()
            .unwrap_or("No reasoning provided")
            .to_string();

        let subtasks: Vec<Subtask> = json["subtasks"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|s| {
                        let desc = s["description"].as_str().unwrap_or("").to_string();
                        let verification = s["verification"].as_str().unwrap_or("");
                        let weight = s["weight"].as_f64().unwrap_or(1.0);
                        
                        Subtask::new(
                            desc,
                            VerificationCriteria::llm_based(verification),
                            weight,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        if subtasks.is_empty() {
            return Err(AgentResult::failure("No subtasks generated", 1));
        }

        SubtaskPlan::new(parent_id, subtasks, reasoning)
            .map_err(|e| AgentResult::failure(format!("Invalid subtask plan: {}", e), 0))
    }

    /// Execute subtasks using NodeAgents for recursive processing.
    /// 
    /// Each subtask is handled by a NodeAgent which can:
    /// - Estimate complexity of the subtask
    /// - Recursively split if the subtask is still too complex
    /// - Execute directly if simple enough
    async fn execute_subtasks(
        &self,
        subtask_plan: SubtaskPlan,
        parent_budget: &Budget,
        ctx: &AgentContext,
    ) -> AgentResult {
        use super::NodeAgent;
        
        // Convert plan to tasks
        let mut tasks = match subtask_plan.into_tasks(parent_budget) {
            Ok(t) => t,
            Err(e) => return AgentResult::failure(format!("Failed to create subtasks: {}", e), 0),
        };

        let mut results = Vec::new();
        let mut total_cost = 0u64;

        // Create a child context with reduced split depth for subtasks
        let child_ctx = ctx.child_context();

        let total_subtasks = tasks.len();
        
        tracing::info!(
            "RootAgent executing {} subtasks (child depth: {})",
            total_subtasks,
            child_ctx.max_split_depth
        );

        // Execute each subtask through a NodeAgent (which can recursively split)
        for (i, task) in tasks.iter_mut().enumerate() {
            tracing::info!(
                "RootAgent delegating subtask {}/{}: {}",
                i + 1,
                total_subtasks,
                task.description().chars().take(80).collect::<String>()
            );

            // Create a NodeAgent for this subtask
            let node_agent = NodeAgent::new(format!("subtask-{}", i + 1));
            
            // Execute through the NodeAgent (which may split further if complex)
            let result = node_agent.execute(task, &child_ctx).await;
            total_cost += result.cost_cents;

            tracing::info!(
                "Subtask {}/{} {}: {}",
                i + 1,
                total_subtasks,
                if result.success { "succeeded" } else { "failed" },
                result.output.chars().take(100).collect::<String>()
            );

            results.push(result);
        }

        // Aggregate results
        let successes = results.iter().filter(|r| r.success).count();
        let total = results.len();

        if successes == total {
            AgentResult::success(
                format!("All {} subtasks completed successfully", total),
                total_cost,
            )
            .with_data(json!({
                "subtasks_total": total,
                "subtasks_succeeded": successes,
                "recursive_execution": true,
                "results": results.iter().map(|r| json!({
                    "success": r.success,
                    "output": &r.output,
                    "data": &r.data,
                })).collect::<Vec<_>>(),
            }))
        } else {
            AgentResult::failure(
                format!("{}/{} subtasks succeeded", successes, total),
                total_cost,
            )
            .with_data(json!({
                "subtasks_total": total,
                "subtasks_succeeded": successes,
                "recursive_execution": true,
                "results": results.iter().map(|r| json!({
                    "success": r.success,
                    "output": &r.output,
                    "data": &r.data,
                })).collect::<Vec<_>>(),
            }))
        }
    }

    /// Execute subtasks with tree updates for visualization.
    /// Uses wave-based parallel execution for independent tasks.
    async fn execute_subtasks_with_tree(
        &self,
        subtask_plan: SubtaskPlan,
        parent_budget: &Budget,
        child_ctx: &AgentContext,
        root_tree: &mut crate::api::control::AgentTreeNode,
        ctx: &AgentContext,
    ) -> AgentResult {
        use super::NodeAgent;
        use std::sync::Arc;
        use tokio::sync::Mutex;
        
        // Get execution waves for parallel processing
        let waves = match subtask_plan.execution_waves() {
            Ok(w) => w,
            Err(e) => return AgentResult::failure(format!("Invalid subtask dependencies: {}", e), 0),
        };
        
        let mut tasks = match subtask_plan.into_tasks(parent_budget) {
            Ok(t) => t,
            Err(e) => return AgentResult::failure(format!("Failed to create subtasks: {}", e), 0),
        };

        let total_subtasks = tasks.len();
        let num_waves = waves.len();
        
        tracing::info!(
            "RootAgent executing {} subtasks in {} wave(s) (child depth: {})",
            total_subtasks,
            num_waves,
            child_ctx.max_split_depth
        );

        // Wrap tree in Arc<Mutex> for thread-safe parallel updates
        let tree = Arc::new(Mutex::new(root_tree.clone()));
        let mut all_results = Vec::new();
        let mut total_cost = 0u64;

        // Execute each wave in parallel
        for (wave_idx, wave) in waves.iter().enumerate() {
            let is_parallel = wave.len() > 1;
            
            tracing::info!(
                "RootAgent wave {}/{}: {} task(s) {}",
                wave_idx + 1,
                num_waves,
                wave.len(),
                if is_parallel { "(parallel)" } else { "(sequential)" }
            );

            // Mark all tasks in this wave as running
            {
                let mut tree_guard = tree.lock().await;
                for &idx in wave {
                    let subtask_id = format!("subtask-{}", idx + 1);
                    if let Some(node) = tree_guard.children.iter_mut().find(|n| n.id == subtask_id) {
                        node.status = "running".to_string();
                    }
                }
                ctx.emit_tree(tree_guard.clone());
            }

            // Execute tasks in this wave in parallel
            let wave_futures: Vec<_> = wave.iter().map(|&idx| {
                let subtask_id = format!("subtask-{}", idx + 1);
                let task = tasks[idx].clone();
                let child_ctx = child_ctx.clone();
                let tree = Arc::clone(&tree);
                let ctx = ctx.clone();
                
                async move {
                    let mut task = task;
                    let node_agent = NodeAgent::new(subtask_id.clone());
                    
                    tracing::info!(
                        "RootAgent delegating subtask {}: {}",
                        subtask_id,
                        task.description().chars().take(80).collect::<String>()
                    );
                    
                    // Execute (without tree updates for parallel - update after)
                    let result = node_agent.execute(&mut task, &child_ctx).await;
                    
                    // Update tree with result
                    {
                        let mut tree_guard = tree.lock().await;
                        if let Some(node) = tree_guard.children.iter_mut().find(|n| n.id == subtask_id) {
                            node.status = if result.success { "completed".to_string() } else { "failed".to_string() };
                            node.budget_spent = result.cost_cents;
                        }
                        ctx.emit_tree(tree_guard.clone());
                    }
                    
                    tracing::info!(
                        "Subtask {} {}: {}",
                        subtask_id,
                        if result.success { "succeeded" } else { "failed" },
                        result.output.chars().take(100).collect::<String>()
                    );
                    
                    (idx, result)
                }
            }).collect();

            // Wait for all tasks in wave to complete
            let wave_results = futures::future::join_all(wave_futures).await;
            
            for (idx, result) in wave_results {
                total_cost += result.cost_cents;
                // Store result at correct index
                while all_results.len() <= idx {
                    all_results.push(None);
                }
                all_results[idx] = Some(result);
            }
        }

        // Collect results in order
        let results: Vec<AgentResult> = all_results.into_iter().filter_map(|r| r).collect();

        // Update the original tree from our Arc<Mutex> version
        *root_tree = tree.lock().await.clone();

        // Update verifier to running
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "verifier") {
            node.status = "running".to_string();
        }
        ctx.emit_tree(root_tree.clone());

        // Aggregate results
        let successes = results.iter().filter(|r| r.success).count();
        let total = results.len();

        // Update verifier to completed
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "verifier") {
            node.status = if successes == total { "completed".to_string() } else { "failed".to_string() };
            node.budget_spent = 5;
        }
        ctx.emit_tree(root_tree.clone());

        if successes == total {
            AgentResult::success(
                format!("All {} subtasks completed successfully ({} waves)", total, num_waves),
                total_cost,
            )
            .with_data(json!({
                "subtasks_total": total,
                "subtasks_succeeded": successes,
                "recursive_execution": true,
                "parallel_waves": num_waves,
                "results": results.iter().map(|r| json!({
                    "success": r.success,
                    "output": &r.output,
                    "data": &r.data,
                })).collect::<Vec<_>>(),
            }))
        } else {
            AgentResult::failure(
                format!("{}/{} subtasks succeeded ({} waves)", successes, total, num_waves),
                total_cost,
            )
            .with_data(json!({
                "subtasks_total": total,
                "subtasks_succeeded": successes,
                "recursive_execution": true,
                "parallel_waves": num_waves,
                "results": results.iter().map(|r| json!({
                    "success": r.success,
                    "output": &r.output,
                    "data": &r.data,
                })).collect::<Vec<_>>(),
            }))
        }
    }
}

impl Default for RootAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for RootAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Root
    }

    fn description(&self) -> &str {
        "Root orchestrator: estimates complexity, splits tasks, delegates execution"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        use crate::api::control::AgentTreeNode;
        
        let mut total_cost = 0u64;
        let task_desc = task.description().chars().take(60).collect::<String>();
        let budget_cents = task.budget().total_cents();

        // Build initial tree structure
        let mut root_tree = AgentTreeNode::new("root", "Root", "Root Agent", &task_desc)
            .with_budget(budget_cents, 0)
            .with_status("running");

        // Add child agent nodes
        root_tree.add_child(
            AgentTreeNode::new("complexity", "ComplexityEstimator", "Complexity Estimator", "Analyzing task difficulty")
                .with_budget(10, 0)
                .with_status("running")
        );
        ctx.emit_tree(root_tree.clone());

        // Step 1: Estimate complexity
        ctx.emit_phase("estimating_complexity", Some("Analyzing task difficulty..."), Some("RootAgent"));
        let complexity = self.estimate_complexity(task, ctx).await;
        total_cost += 1;

        // Update complexity node
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "complexity") {
            node.status = "completed".to_string();
            node.complexity = Some(complexity.score());
            node.budget_spent = 5;
        }
        ctx.emit_tree(root_tree.clone());

        tracing::info!(
            "Task complexity: {:.2} (should_split: {})",
            complexity.score(),
            complexity.should_split()
        );

        // Step 2: Decide execution strategy
        if complexity.should_split() && ctx.can_split() {
            ctx.emit_phase("splitting_task", Some("Decomposing into subtasks..."), Some("RootAgent"));
            match self.split_task(task, ctx).await {
                Ok(plan) => {
                    total_cost += 2;
                    
                    // Add subtask nodes to tree
                    for (i, subtask) in plan.subtasks().iter().enumerate() {
                        let subtask_node = AgentTreeNode::new(
                            &format!("subtask-{}", i + 1),
                            "Node",
                            &format!("Subtask {}", i + 1),
                            &subtask.description.chars().take(50).collect::<String>(),
                        )
                        .with_budget(budget_cents / plan.subtasks().len() as u64, 0)
                        .with_status("pending");
                        root_tree.add_child(subtask_node);
                    }
                    
                    // Add verifier node
                    root_tree.add_child(
                        AgentTreeNode::new("verifier", "Verifier", "Verifier", "Verify task completion")
                            .with_budget(80, 0)
                            .with_status("pending")
                    );
                    ctx.emit_tree(root_tree.clone());
                    
                    // Execute subtasks with tree updates
                    let child_ctx = ctx.child_context();
                    let result = self.execute_subtasks_with_tree(plan, task.budget(), &child_ctx, &mut root_tree, ctx).await;
                    
                    // Update root status
                    root_tree.status = if result.success { "completed".to_string() } else { "failed".to_string() };
                    root_tree.budget_spent = total_cost + result.cost_cents;
                    ctx.emit_tree(root_tree);
                    
                    return AgentResult {
                        success: result.success,
                        output: result.output,
                        cost_cents: total_cost + result.cost_cents,
                        model_used: result.model_used,
                        data: result.data,
                    };
                }
                Err(e) => {
                    tracing::warn!("Couldn't split task, executing directly: {}", e.output);
                }
            }
        }

        // Simple task: add remaining nodes
        root_tree.add_child(
            AgentTreeNode::new("model-selector", "ModelSelector", "Model Selector", "Selecting optimal model")
                .with_budget(10, 0)
                .with_status("running")
        );
        ctx.emit_tree(root_tree.clone());

        ctx.emit_phase("selecting_model", Some("Choosing optimal model..."), Some("RootAgent"));
        
        let has_benchmarks = if let Some(b) = &ctx.benchmarks {
            let registry = b.read().await;
            registry.benchmark_count() > 0
        } else {
            false
        };
        
        let selected_model = if has_benchmarks {
            let sel_result = self.model_selector.execute(task, ctx).await;
            total_cost += sel_result.cost_cents;
            task.analysis().selected_model.clone().unwrap_or_else(|| ctx.config.default_model.clone())
        } else {
            let a = task.analysis_mut();
            a.selected_model = Some(ctx.config.default_model.clone());
            ctx.config.default_model.clone()
        };

        // Update model selector node
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "model-selector") {
            node.status = "completed".to_string();
            node.selected_model = Some(selected_model);
            node.budget_spent = 3;
        }

        // Add executor and verifier nodes
        root_tree.add_child(
            AgentTreeNode::new("executor", "TaskExecutor", "Task Executor", "Executing task")
                .with_budget(budget_cents - 100, 0)
                .with_status("running")
        );
        root_tree.add_child(
            AgentTreeNode::new("verifier", "Verifier", "Verifier", "Verify task completion")
                .with_budget(80, 0)
                .with_status("pending")
        );
        ctx.emit_tree(root_tree.clone());

        ctx.emit_phase("executing", Some("Running task..."), Some("RootAgent"));
        let result = self.task_executor.execute(task, ctx).await;

        // Update executor node
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "executor") {
            node.status = if result.success { "completed".to_string() } else { "failed".to_string() };
            node.budget_spent = result.cost_cents;
        }
        ctx.emit_tree(root_tree.clone());

        // Step 3: Verify
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "verifier") {
            node.status = "running".to_string();
        }
        ctx.emit_tree(root_tree.clone());

        ctx.emit_phase("verifying", Some("Checking results..."), Some("RootAgent"));
        let verification = self.verifier.execute(task, ctx).await;
        total_cost += verification.cost_cents;

        // Update verifier node
        if let Some(node) = root_tree.children.iter_mut().find(|n| n.id == "verifier") {
            node.status = if verification.success { "completed".to_string() } else { "failed".to_string() };
            node.budget_spent = verification.cost_cents;
        }

        // Update root status
        root_tree.status = if result.success && verification.success { "completed".to_string() } else { "failed".to_string() };
        root_tree.budget_spent = total_cost + result.cost_cents;
        ctx.emit_tree(root_tree);

        AgentResult {
            success: result.success && verification.success,
            output: if verification.success {
                result.output
            } else {
                format!("{}\n\nVerification failed: {}", result.output, verification.output)
            },
            cost_cents: total_cost + result.cost_cents,
            model_used: result.model_used,
            data: json!({
                "complexity": complexity.score(),
                "was_split": false,
                "verification": verification.data,
                "execution": result.data,
            }).into(),
        }
    }
}

#[async_trait]
impl OrchestratorAgent for RootAgent {
    fn children(&self) -> Vec<AgentRef> {
        vec![
            Arc::clone(&self.complexity_estimator) as AgentRef,
            Arc::clone(&self.model_selector) as AgentRef,
            Arc::clone(&self.task_executor) as AgentRef,
            Arc::clone(&self.verifier) as AgentRef,
        ]
    }

    fn find_child(&self, agent_type: AgentType) -> Option<AgentRef> {
        match agent_type {
            AgentType::ComplexityEstimator => Some(Arc::clone(&self.complexity_estimator) as AgentRef),
            AgentType::ModelSelector => Some(Arc::clone(&self.model_selector) as AgentRef),
            AgentType::TaskExecutor => Some(Arc::clone(&self.task_executor) as AgentRef),
            AgentType::Verifier => Some(Arc::clone(&self.verifier) as AgentRef),
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

