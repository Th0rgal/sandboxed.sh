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

    /// Parse LLM response into SubtaskPlan.
    fn parse_subtask_plan(
        &self,
        response: &str,
        parent_id: crate::task::TaskId,
    ) -> Result<SubtaskPlan, AgentResult> {
        let json: serde_json::Value = serde_json::from_str(response)
            .map_err(|e| AgentResult::failure(format!("Failed to parse subtasks: {}", e), 0))?;

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

    /// Execute subtasks and aggregate results.
    async fn execute_subtasks(
        &self,
        subtask_plan: SubtaskPlan,
        parent_budget: &Budget,
        ctx: &AgentContext,
    ) -> AgentResult {
        // Convert plan to tasks
        let mut tasks = match subtask_plan.into_tasks(parent_budget) {
            Ok(t) => t,
            Err(e) => return AgentResult::failure(format!("Failed to create subtasks: {}", e), 0),
        };

        let mut results = Vec::new();
        let mut total_cost = 0u64;

        // Execute each subtask with planning + verification.
        for task in &mut tasks {
            // 1) Estimate complexity (for token estimate) for this subtask.
            let est = self.complexity_estimator.execute(task, ctx).await;
            total_cost += est.cost_cents;

            // 2) Select model based on complexity + subtask budget.
            let sel = self.model_selector.execute(task, ctx).await;
            total_cost += sel.cost_cents;

            // 3) Execute.
            let exec = self.task_executor.execute(task, ctx).await;
            total_cost += exec.cost_cents;

            // 4) Verify.
            let ver = self.verifier.execute(task, ctx).await;
            total_cost += ver.cost_cents;

            let success = exec.success && ver.success;

            results.push(
                AgentResult {
                    success,
                    output: if ver.success {
                        exec.output.clone()
                    } else {
                        format!("{}\n\nVerification failed: {}", exec.output, ver.output)
                    },
                    cost_cents: est.cost_cents + sel.cost_cents + exec.cost_cents + ver.cost_cents,
                    model_used: exec.model_used.clone(),
                    data: Some(json!({
                        "complexity_estimate": est.data,
                        "model_selection": sel.data,
                        "execution": exec.data,
                        "verification": ver.data,
                    })),
                }
            );
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
                "results": results.iter().map(|r| &r.output).collect::<Vec<_>>(),
            }))
        } else {
            AgentResult::failure(
                format!("{}/{} subtasks succeeded", successes, total),
                total_cost,
            )
            .with_data(json!({
                "subtasks_total": total,
                "subtasks_succeeded": successes,
                "results": results.iter().map(|r| json!({
                    "success": r.success,
                    "output": &r.output,
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
        let mut total_cost = 0u64;

        // Step 1: Estimate complexity
        let complexity = self.estimate_complexity(task, ctx).await;
        // Cost already tracked inside ComplexityEstimator; we keep a small constant for now.
        total_cost += 1;

        tracing::info!(
            "Task complexity: {:.2} (should_split: {})",
            complexity.score(),
            complexity.should_split()
        );

        // Step 2: Decide execution strategy
        if complexity.should_split() && ctx.can_split() {
            // Complex task: split and delegate
            match self.split_task(task, ctx).await {
                Ok(plan) => {
                    total_cost += 2; // Splitting cost
                    
                    // Execute subtasks
                    let child_ctx = ctx.child_context();
                    let result = self.execute_subtasks(plan, task.budget(), &child_ctx).await;
                    
                    return AgentResult {
                        success: result.success,
                        output: result.output,
                        cost_cents: total_cost + result.cost_cents,
                        model_used: result.model_used,
                        data: result.data,
                    };
                }
                Err(e) => {
                    // Couldn't split, fall back to direct execution
                    tracing::warn!("Couldn't split task, executing directly: {}", e.output);
                }
            }
        }

        // Simple task or failed to split: execute directly
        // Use ModelSelector when we have benchmark data, otherwise use default model
        let has_benchmarks = if let Some(b) = &ctx.benchmarks {
            let registry = b.read().await;
            registry.benchmark_count() > 0
        } else {
            false
        };
        
        if has_benchmarks {
            // Use benchmark-informed model selection
            let sel_result = self.model_selector.execute(task, ctx).await;
            total_cost += sel_result.cost_cents;
            
            tracing::info!(
                "ModelSelector used (has benchmarks): {}",
                task.analysis().selected_model.as_ref().unwrap_or(&"none".to_string())
            );
        } else {
            // Fall back to default model when no benchmarks available
            let a = task.analysis_mut();
            a.selected_model = Some(ctx.config.default_model.clone());
            
            tracing::info!(
                "Using default model (no benchmarks): {}",
                ctx.config.default_model
            );
        }

        let result = self.task_executor.execute(task, ctx).await;

        // Step 3: Verify (if verification criteria specified)
        let verification = self.verifier.execute(task, ctx).await;
        total_cost += verification.cost_cents;

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

