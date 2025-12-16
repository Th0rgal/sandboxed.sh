//! Task executor agent - the main worker that uses tools.
//!
//! This is a refactored version of the original agent loop,
//! now as a leaf agent in the hierarchical tree.

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType, LeafAgent, LeafCapability,
};
use crate::api::control::{AgentEvent, ControlRunState};
use crate::budget::ExecutionSignals;
use crate::llm::{ChatMessage, Role, ToolCall};
use crate::task::{Task, TokenUsageSummary};
use crate::tools::ToolRegistry;

/// Result from running the agent loop with detailed signals for failure analysis.
#[derive(Debug)]
pub struct ExecutionLoopResult {
    /// Final output text
    pub output: String,
    /// Total cost in cents
    pub cost_cents: u64,
    /// Log of tool calls made
    pub tool_log: Vec<String>,
    /// Token usage summary
    pub usage: Option<TokenUsageSummary>,
    /// Detailed signals for failure analysis
    pub signals: ExecutionSignals,
    /// Whether execution succeeded
    pub success: bool,
}

/// Agent that executes tasks using tools.
/// 
/// # Algorithm
/// 1. Build system prompt with available tools
/// 2. Call LLM with task description
/// 3. If LLM requests tool call: execute, feed back result
/// 4. Repeat until LLM produces final response or max iterations
/// 
/// # Budget Management
/// - Tracks token usage and costs
/// - Stops if budget is exhausted
pub struct TaskExecutor {
    id: AgentId,
}

impl TaskExecutor {
    /// Create a new task executor.
    pub fn new() -> Self {
        Self { id: AgentId::new() }
    }

    /// Build the system prompt for task execution.
    fn build_system_prompt(&self, working_dir: &str, tools: &ToolRegistry) -> String {
        let tool_descriptions = tools
            .list_tools()
            .iter()
            .map(|t| format!("- **{}**: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"You are an autonomous task executor with **full system access**.
You can read/write any file, execute any command, and search anywhere on the machine.
Your default working directory is: {working_dir}
You can use absolute paths (e.g., /etc/hosts, /var/log) to access any location.

## Available Tools
{tool_descriptions}

## Rules
1. Use tools to accomplish the task - don't just describe what to do
2. Read files before editing them
3. Prefer working in {working_dir} for relative paths; create reusable helper scripts in {working_dir}/tools/ (production convention: /root/tools/)
4. For large filesystem searches, scope `grep_search` / `search_files` with an appropriate `path`. If you'll search the same large tree repeatedly, build an index with `index_files` then query it with `search_file_index`.
5. Verify your work when possible
6. If stuck, explain what's blocking you
7. When done, summarize what you accomplished
8. For structured output (tables, lists of choices), prefer calling UI tools (ui_*) so the dashboard can render rich components.
9. If you call an interactive UI tool (e.g. ui_optionList), wait for the tool result and continue based on the user's selection.

## Response
When task is complete, provide a clear summary of:
- What you did
- Files created/modified (with full paths)
- How to verify the result"#,
            working_dir = working_dir,
            tool_descriptions = tool_descriptions
        )
    }

    /// Execute a single tool call.
    async fn execute_tool_call(
        &self,
        tool_call: &ToolCall,
        ctx: &AgentContext,
    ) -> anyhow::Result<String> {
        let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
            .unwrap_or(serde_json::Value::Null);

        ctx.tools
            .execute(&tool_call.function.name, args, &ctx.working_dir)
            .await
    }

    /// Run the agent loop for a task.
    async fn run_loop(
        &self,
        task: &Task,
        model: &str,
        ctx: &AgentContext,
    ) -> ExecutionLoopResult {
        let mut total_cost_cents = 0u64;
        let mut tool_log = Vec::new();
        let mut usage: Option<TokenUsageSummary> = None;
        
        // Track execution signals for failure analysis
        let mut successful_tool_calls = 0u32;
        let mut failed_tool_calls = 0u32;
        let mut files_modified = false;
        let mut last_tool_calls: Vec<String> = Vec::new();
        let mut repetitive_actions = false;
        let mut has_error_messages = false;
        let mut iterations_completed = 0u32;

        // If we can fetch pricing, compute real costs from token usage.
        let pricing = ctx.pricing.get_pricing(model).await;

        // Build initial messages
        let system_prompt = self.build_system_prompt(&ctx.working_dir_str(), &ctx.tools);
        let mut messages = vec![
            ChatMessage {
                role: Role::System,
                content: Some(system_prompt),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::User,
                content: Some(task.description().to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        // Get tool schemas
        let tool_schemas = ctx.tools.get_tool_schemas();

        // Agent loop
        for iteration in 0..ctx.max_iterations {
            iterations_completed = iteration as u32 + 1;
            tracing::debug!("TaskExecutor iteration {}", iteration + 1);

            // Cooperative cancellation (control session).
            if let Some(token) = &ctx.cancel_token {
                if token.is_cancelled() {
                    has_error_messages = true;
                    let signals = ExecutionSignals {
                        iterations: iterations_completed,
                        max_iterations: ctx.max_iterations as u32,
                        successful_tool_calls,
                        failed_tool_calls,
                        files_modified,
                        repetitive_actions,
                        has_error_messages,
                        partial_progress: files_modified || successful_tool_calls > 0,
                        cost_spent_cents: total_cost_cents,
                        budget_total_cents: task.budget().total_cents(),
                        final_output: "Cancelled".to_string(),
                        model_used: model.to_string(),
                    };
                    return ExecutionLoopResult {
                        output: "Cancelled".to_string(),
                        cost_cents: total_cost_cents,
                        tool_log,
                        usage,
                        signals,
                        success: false,
                    };
                }
            }

            // Check budget
            let remaining = task.budget().remaining_cents();
            if remaining == 0 && total_cost_cents > 0 {
                let signals = ExecutionSignals {
                    iterations: iterations_completed,
                    max_iterations: ctx.max_iterations as u32,
                    successful_tool_calls,
                    failed_tool_calls,
                    files_modified,
                    repetitive_actions,
                    has_error_messages,
                    partial_progress: files_modified || successful_tool_calls > 0,
                    cost_spent_cents: total_cost_cents,
                    budget_total_cents: task.budget().total_cents(),
                    final_output: "Budget exhausted before task completion".to_string(),
                    model_used: model.to_string(),
                };
                return ExecutionLoopResult {
                    output: "Budget exhausted before task completion".to_string(),
                    cost_cents: total_cost_cents,
                    tool_log,
                    usage,
                    signals,
                    success: false,
                };
            }

            // Call LLM
            let response = match ctx.llm.chat_completion(model, &messages, Some(&tool_schemas)).await {
                Ok(r) => r,
                Err(e) => {
                    has_error_messages = true;
                    let error_msg = format!("LLM error: {}", e);
                    let signals = ExecutionSignals {
                        iterations: iterations_completed,
                        max_iterations: ctx.max_iterations as u32,
                        successful_tool_calls,
                        failed_tool_calls,
                        files_modified,
                        repetitive_actions,
                        has_error_messages,
                        partial_progress: files_modified || successful_tool_calls > 0,
                        cost_spent_cents: total_cost_cents,
                        budget_total_cents: task.budget().total_cents(),
                        final_output: error_msg.clone(),
                        model_used: model.to_string(),
                    };
                    return ExecutionLoopResult {
                        output: error_msg,
                        cost_cents: total_cost_cents,
                        tool_log,
                        usage,
                        signals,
                        success: false,
                    };
                }
            };

            // Cost + usage accounting.
            if let Some(u) = &response.usage {
                let u_sum = TokenUsageSummary::new(u.prompt_tokens, u.completion_tokens);
                usage = Some(match &usage {
                    Some(acc) => acc.add(&u_sum),
                    None => u_sum,
                });

                if let Some(p) = &pricing {
                    total_cost_cents = total_cost_cents.saturating_add(
                        p.calculate_cost_cents(u.prompt_tokens, u.completion_tokens),
                    );
                } else {
                    // Fallback heuristic when usage exists but pricing doesn't.
                    total_cost_cents = total_cost_cents.saturating_add(2);
                }
            } else {
                // Legacy heuristic if upstream doesn't return usage.
                total_cost_cents = total_cost_cents.saturating_add(2);
            }

            // Check for tool calls
            if let Some(tool_calls) = &response.tool_calls {
                if !tool_calls.is_empty() {
                    // Add assistant message with tool calls
                    messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: response.content.clone(),
                        tool_calls: Some(tool_calls.clone()),
                        tool_call_id: None,
                    });

                    // Check for repetitive actions
                    let current_calls: Vec<String> = tool_calls
                        .iter()
                        .map(|tc| format!("{}:{}", tc.function.name, tc.function.arguments))
                        .collect();
                    if current_calls == last_tool_calls && !current_calls.is_empty() {
                        repetitive_actions = true;
                    }
                    last_tool_calls = current_calls;

                    // Execute each tool call
                    for tool_call in tool_calls {
                        let tool_name = tool_call.function.name.clone();
                        let args_json: serde_json::Value =
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or(serde_json::Value::Null);

                        // For interactive frontend tools, register the tool_call_id before notifying the UI,
                        // so a fast tool_result POST can't race ahead of registration.
                        let mut pending_frontend_rx: Option<tokio::sync::oneshot::Receiver<serde_json::Value>> = None;
                        if tool_name == "ui_optionList" {
                            if let Some(hub) = &ctx.frontend_tool_hub {
                                pending_frontend_rx = Some(hub.register(tool_call.id.clone()).await);
                            }
                        }

                        if let Some(events) = &ctx.control_events {
                            let _ = events.send(AgentEvent::ToolCall {
                                tool_call_id: tool_call.id.clone(),
                                name: tool_name.clone(),
                                args: args_json.clone(),
                            });
                        }

                        tool_log.push(format!(
                            "Tool: {} Args: {}",
                            tool_call.function.name,
                            tool_call.function.arguments
                        ));

                        // Track file modifications
                        if tool_name == "write_file" || tool_name == "delete_file" {
                            files_modified = true;
                        }

                        // UI tools are handled by the frontend. We emit events and (optionally) wait for a user result.
                        let (tool_message_content, tool_result_json): (String, serde_json::Value) =
                            if tool_name.starts_with("ui_") {
                                // Interactive tool: wait for frontend to POST result.
                                if tool_name == "ui_optionList" {
                                    if let Some(rx) = pending_frontend_rx {
                                        if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                            let mut s = status.write().await;
                                            s.state = ControlRunState::WaitingForTool;
                                            let q = s.queue_len;
                                            drop(s);
                                            let _ = events.send(AgentEvent::Status { state: ControlRunState::WaitingForTool, queue_len: q });
                                        }

                                        let recv = if let Some(token) = &ctx.cancel_token {
                                            tokio::select! {
                                                v = rx => v,
                                                _ = token.cancelled() => {
                                                    has_error_messages = true;
                                                    let signals = ExecutionSignals {
                                                        iterations: iterations_completed,
                                                        max_iterations: ctx.max_iterations as u32,
                                                        successful_tool_calls,
                                                        failed_tool_calls,
                                                        files_modified,
                                                        repetitive_actions,
                                                        has_error_messages,
                                                        partial_progress: files_modified || successful_tool_calls > 0,
                                                        cost_spent_cents: total_cost_cents,
                                                        budget_total_cents: task.budget().total_cents(),
                                                        final_output: "Cancelled".to_string(),
                                                        model_used: model.to_string(),
                                                    };
                                                    return ExecutionLoopResult {
                                                        output: "Cancelled".to_string(),
                                                        cost_cents: total_cost_cents,
                                                        tool_log,
                                                        usage,
                                                        signals,
                                                        success: false,
                                                    };
                                                }
                                            }
                                        } else {
                                            rx.await
                                        };

                                        match recv {
                                            Ok(v) => {
                                                successful_tool_calls += 1;
                                                let msg = serde_json::to_string(&v)
                                                    .unwrap_or_else(|_| v.to_string());
                                                if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                                    let mut s = status.write().await;
                                                    s.state = ControlRunState::Running;
                                                    let q = s.queue_len;
                                                    drop(s);
                                                    let _ = events.send(AgentEvent::Status { state: ControlRunState::Running, queue_len: q });
                                                }
                                                (msg, v)
                                            }
                                            Err(_) => {
                                                has_error_messages = true;
                                                failed_tool_calls += 1;
                                                if let (Some(status), Some(events)) = (&ctx.control_status, &ctx.control_events) {
                                                    let mut s = status.write().await;
                                                    s.state = ControlRunState::Running;
                                                    let q = s.queue_len;
                                                    drop(s);
                                                    let _ = events.send(AgentEvent::Status { state: ControlRunState::Running, queue_len: q });
                                                }
                                                ("Error: tool result channel closed".to_string(), serde_json::Value::Null)
                                            }
                                        }
                                    } else {
                                        has_error_messages = true;
                                        failed_tool_calls += 1;
                                        ("Error: frontend tool hub not configured".to_string(), serde_json::Value::Null)
                                    }
                                } else {
                                    // Non-interactive UI render: echo args as the tool result payload.
                                    let msg = serde_json::to_string(&args_json)
                                        .unwrap_or_else(|_| args_json.to_string());
                                    successful_tool_calls += 1;
                                    (msg, args_json.clone())
                                }
                            } else {
                                // Regular server tool.
                                match self.execute_tool_call(tool_call, ctx).await {
                                    Ok(output) => {
                                        successful_tool_calls += 1;
                                        (output.clone(), serde_json::Value::String(output))
                                    }
                                    Err(e) => {
                                        failed_tool_calls += 1;
                                        has_error_messages = true;
                                        let s = format!("Error: {}", e);
                                        (s.clone(), serde_json::Value::String(s))
                                    }
                                }
                            };

                        if let Some(events) = &ctx.control_events {
                            let _ = events.send(AgentEvent::ToolResult {
                                tool_call_id: tool_call.id.clone(),
                                name: tool_name.clone(),
                                result: tool_result_json.clone(),
                            });
                        }

                        // Add tool result
                        messages.push(ChatMessage {
                            role: Role::Tool,
                            content: Some(tool_message_content),
                            tool_calls: None,
                            tool_call_id: Some(tool_call.id.clone()),
                        });
                    }

                    continue;
                }
            }

            // No tool calls - final response
            if let Some(content) = response.content {
                let signals = ExecutionSignals {
                    iterations: iterations_completed,
                    max_iterations: ctx.max_iterations as u32,
                    successful_tool_calls,
                    failed_tool_calls,
                    files_modified,
                    repetitive_actions,
                    has_error_messages,
                    partial_progress: true, // Completed successfully
                    cost_spent_cents: total_cost_cents,
                    budget_total_cents: task.budget().total_cents(),
                    final_output: content.clone(),
                    model_used: model.to_string(),
                };
                return ExecutionLoopResult {
                    output: content,
                    cost_cents: total_cost_cents,
                    tool_log,
                    usage,
                    signals,
                    success: true,
                };
            }

            // Empty response
            has_error_messages = true;
            let signals = ExecutionSignals {
                iterations: iterations_completed,
                max_iterations: ctx.max_iterations as u32,
                successful_tool_calls,
                failed_tool_calls,
                files_modified,
                repetitive_actions,
                has_error_messages,
                partial_progress: files_modified || successful_tool_calls > 0,
                cost_spent_cents: total_cost_cents,
                budget_total_cents: task.budget().total_cents(),
                final_output: "LLM returned empty response".to_string(),
                model_used: model.to_string(),
            };
            return ExecutionLoopResult {
                output: "LLM returned empty response".to_string(),
                cost_cents: total_cost_cents,
                tool_log,
                usage,
                signals,
                success: false,
            };
        }

        // Max iterations reached
        let signals = ExecutionSignals {
            iterations: iterations_completed,
            max_iterations: ctx.max_iterations as u32,
            successful_tool_calls,
            failed_tool_calls,
            files_modified,
            repetitive_actions,
            has_error_messages,
            partial_progress: files_modified || successful_tool_calls > 0,
            cost_spent_cents: total_cost_cents,
            budget_total_cents: task.budget().total_cents(),
            final_output: format!("Max iterations ({}) reached", ctx.max_iterations),
            model_used: model.to_string(),
        };
        ExecutionLoopResult {
            output: format!("Max iterations ({}) reached", ctx.max_iterations),
            cost_cents: total_cost_cents,
            tool_log,
            usage,
            signals,
            success: false,
        }
    }
}

impl Default for TaskExecutor {
    fn default() -> Self {
        Self::new()
    }
}


#[async_trait]
impl Agent for TaskExecutor {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::TaskExecutor
    }

    fn description(&self) -> &str {
        "Executes tasks using tools (file ops, terminal, search, etc.)"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        // Use model selected during planning, otherwise fall back to default.
        let selected = task
            .analysis()
            .selected_model
            .clone()
            .unwrap_or_else(|| ctx.config.default_model.clone());
        let model = selected.as_str();

        let result = self.run_loop(task, model, ctx).await;

        // Record telemetry
        task.analysis_mut().selected_model = Some(model.to_string());
        task.analysis_mut().actual_usage = result.usage.clone();

        // Update task budget
        let _ = task.budget_mut().try_spend(result.cost_cents);

        let mut agent_result = if result.success {
            AgentResult::success(&result.output, result.cost_cents)
        } else {
            AgentResult::failure(&result.output, result.cost_cents)
        };

        agent_result = agent_result
            .with_model(model)
            .with_data(json!({
                "tool_calls": result.tool_log.len(),
                "tools_used": result.tool_log,
                "usage": result.usage.as_ref().map(|u| json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens
                })),
                "execution_signals": {
                    "iterations": result.signals.iterations,
                    "max_iterations": result.signals.max_iterations,
                    "successful_tool_calls": result.signals.successful_tool_calls,
                    "failed_tool_calls": result.signals.failed_tool_calls,
                    "files_modified": result.signals.files_modified,
                    "repetitive_actions": result.signals.repetitive_actions,
                    "partial_progress": result.signals.partial_progress,
                }
            }));

        agent_result
    }
}

impl TaskExecutor {
    /// Execute a task and return detailed execution result for retry analysis.
    pub async fn execute_with_signals(&self, task: &mut Task, ctx: &AgentContext) -> (AgentResult, ExecutionSignals) {
        let selected = task
            .analysis()
            .selected_model
            .clone()
            .unwrap_or_else(|| ctx.config.default_model.clone());
        let model = selected.as_str();

        let result = self.run_loop(task, model, ctx).await;

        // Record telemetry
        task.analysis_mut().selected_model = Some(model.to_string());
        task.analysis_mut().actual_usage = result.usage.clone();

        // Update task budget
        let _ = task.budget_mut().try_spend(result.cost_cents);

        let mut agent_result = if result.success {
            AgentResult::success(&result.output, result.cost_cents)
        } else {
            AgentResult::failure(&result.output, result.cost_cents)
        };

        agent_result = agent_result
            .with_model(model)
            .with_data(json!({
                "tool_calls": result.tool_log.len(),
                "tools_used": result.tool_log,
                "usage": result.usage.as_ref().map(|u| json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens
                })),
            }));

        (agent_result, result.signals)
    }
}

impl LeafAgent for TaskExecutor {
    fn capability(&self) -> LeafCapability {
        LeafCapability::TaskExecution
    }
}

