//! OpenCode-backed agent - delegates task execution to an external OpenCode server.
//!
//! This agent streams real-time events (thinking, tool calls, results) from OpenCode
//! to the control broadcast channel, enabling live UI updates in the dashboard.

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{Agent, AgentContext, AgentId, AgentResult, AgentType, TerminalReason};
use crate::api::control::{AgentEvent, AgentTreeNode};
use crate::config::Config;
use crate::opencode::{extract_text, OpenCodeClient, OpenCodeEvent};
use crate::task::Task;

pub struct OpenCodeAgent {
    id: AgentId,
    client: OpenCodeClient,
    default_agent: Option<String>,
}

impl OpenCodeAgent {
    pub fn new(config: Config) -> Self {
        let client = OpenCodeClient::new(
            config.opencode_base_url.clone(),
            config.opencode_agent.clone(),
            config.opencode_permissive,
        );
        Self {
            id: AgentId::new(),
            client,
            default_agent: config.opencode_agent,
        }
    }

    fn build_tree(&self, task_desc: &str, budget_cents: u64) -> AgentTreeNode {
        let mut root = AgentTreeNode::new("root", "OpenCode", "OpenCode Agent", task_desc)
            .with_budget(budget_cents, 0)
            .with_status("running");

        root.add_child(
            AgentTreeNode::new(
                "opencode",
                "OpenCodeSession",
                "OpenCode Session",
                "Delegating to OpenCode",
            )
            .with_budget(budget_cents, 0)
            .with_status("running"),
        );

        root
    }

    /// Forward an OpenCode event to the control broadcast channel.
    fn forward_event(&self, oc_event: &OpenCodeEvent, ctx: &AgentContext) {
        let Some(events_tx) = &ctx.control_events else {
            return;
        };

        let agent_event = match oc_event {
            OpenCodeEvent::Thinking { content } => AgentEvent::Thinking {
                content: content.clone(),
                done: false,
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::TextDelta { content } => AgentEvent::Thinking {
                content: content.clone(),
                done: false,
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::ToolCall {
                tool_call_id,
                name,
                args,
            } => AgentEvent::ToolCall {
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                args: args.clone(),
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::ToolResult {
                tool_call_id,
                name,
                result,
            } => AgentEvent::ToolResult {
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                result: result.clone(),
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::Error { message } => AgentEvent::Error {
                message: message.clone(),
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::MessageComplete { .. } => return, // Don't forward completion marker
        };

        let _ = events_tx.send(agent_event);
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Root
    }

    fn description(&self) -> &str {
        "OpenCode agent: delegates task execution to an OpenCode server"
    }

    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        let task_desc = task.description().chars().take(60).collect::<String>();
        let budget_cents = task.budget().total_cents();

        let mut tree = self.build_tree(&task_desc, budget_cents);
        ctx.emit_tree(tree.clone());
        ctx.emit_phase(
            "executing",
            Some("Delegating to OpenCode server"),
            Some("OpenCodeAgent"),
        );

        if ctx.is_cancelled() {
            return AgentResult::failure("Task cancelled", 0)
                .with_terminal_reason(TerminalReason::Cancelled);
        }

        // OpenCode requires an absolute path
        let directory = std::fs::canonicalize(&ctx.working_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ctx.working_dir_str());
        let title = Some(task_desc.as_str());

        let session = match self.client.create_session(&directory, title).await {
            Ok(s) => s,
            Err(e) => {
                tree.status = "failed".to_string();
                ctx.emit_tree(tree);
                return AgentResult::failure(format!("OpenCode session error: {}", e), 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        // Choose model: requested override > config default
        let model_override: Option<String> = task
            .analysis()
            .requested_model
            .clone()
            .or_else(|| Some(ctx.config.default_model.clone()));
        if let Some(ref model) = model_override {
            task.analysis_mut().selected_model = Some(model.clone());
        }

        let agent_name = self.default_agent.as_deref();

        // Use streaming to get real-time events
        let streaming_result = self
            .client
            .send_message_streaming(
                &session.id,
                &directory,
                task.description(),
                model_override.as_deref(),
                agent_name,
            )
            .await;

        let (mut event_rx, message_handle) = match streaming_result {
            Ok((rx, handle)) => (rx, handle),
            Err(e) => {
                // Fall back to non-streaming if SSE fails
                tracing::warn!(
                    "OpenCode SSE streaming failed, falling back to blocking: {}",
                    e
                );
                return self
                    .execute_blocking(
                        task,
                        ctx,
                        &session.id,
                        &directory,
                        model_override.as_deref(),
                        agent_name,
                        tree,
                    )
                    .await;
            }
        };

        // Process streaming events with cancellation support
        let response = if let Some(cancel) = ctx.cancel_token.clone() {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        let _ = self.client.abort_session(&session.id, &directory).await;
                        message_handle.abort();
                        return AgentResult::failure("Task cancelled", 0).with_terminal_reason(TerminalReason::Cancelled);
                    }
                    event = event_rx.recv() => {
                        match event {
                            Some(oc_event) => {
                                self.forward_event(&oc_event, ctx);
                                if matches!(oc_event, OpenCodeEvent::MessageComplete { .. }) {
                                    break;
                                }
                            }
                            None => break, // Channel closed
                        }
                    }
                }
            }

            // Wait for the final response
            match message_handle.await {
                Ok(Ok(response)) => response,
                Ok(Err(e)) => {
                    tree.status = "failed".to_string();
                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                        node.status = "failed".to_string();
                    }
                    ctx.emit_tree(tree);
                    return AgentResult::failure(format!("OpenCode message error: {}", e), 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
                Err(e) => {
                    tree.status = "failed".to_string();
                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                        node.status = "failed".to_string();
                    }
                    ctx.emit_tree(tree);
                    return AgentResult::failure(format!("OpenCode task error: {}", e), 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
            }
        } else {
            // No cancel token - just process events
            while let Some(oc_event) = event_rx.recv().await {
                self.forward_event(&oc_event, ctx);
                if matches!(oc_event, OpenCodeEvent::MessageComplete { .. }) {
                    break;
                }
            }

            match message_handle.await {
                Ok(Ok(response)) => response,
                Ok(Err(e)) => {
                    tree.status = "failed".to_string();
                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                        node.status = "failed".to_string();
                    }
                    ctx.emit_tree(tree);
                    return AgentResult::failure(format!("OpenCode message error: {}", e), 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
                Err(e) => {
                    tree.status = "failed".to_string();
                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                        node.status = "failed".to_string();
                    }
                    ctx.emit_tree(tree);
                    return AgentResult::failure(format!("OpenCode task error: {}", e), 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
            }
        };

        // Emit final thinking done marker
        if let Some(events_tx) = &ctx.control_events {
            let _ = events_tx.send(AgentEvent::Thinking {
                content: String::new(),
                done: true,
                mission_id: ctx.mission_id,
            });
        }

        if let Some(error) = &response.info.error {
            tree.status = "failed".to_string();
            if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                node.status = "failed".to_string();
            }
            ctx.emit_tree(tree);
            // Extract error message from the error value
            let error_msg = if let Some(msg) = error.get("message").and_then(|v| v.as_str()) {
                msg.to_string()
            } else if let Some(s) = error.as_str() {
                s.to_string()
            } else {
                error.to_string()
            };
            return AgentResult::failure(format!("OpenCode error: {}", error_msg), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }

        let output = extract_text(&response.parts);

        if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
            node.status = "completed".to_string();
        }
        tree.status = "completed".to_string();
        ctx.emit_tree(tree);

        let model_used = match (&response.info.provider_id, &response.info.model_id) {
            (Some(provider), Some(model)) => Some(format!("{}/{}", provider, model)),
            _ => None,
        };

        AgentResult {
            success: true,
            output,
            cost_cents: 0,
            model_used,
            data: Some(json!({
                "agent": "OpenCodeAgent",
                "session_id": session.id,
            })),
            terminal_reason: Some(TerminalReason::Completed),
        }
    }
}

impl OpenCodeAgent {
    /// Fallback blocking execution without streaming.
    async fn execute_blocking(
        &self,
        task: &mut Task,
        ctx: &AgentContext,
        session_id: &str,
        directory: &str,
        model: Option<&str>,
        agent: Option<&str>,
        mut tree: AgentTreeNode,
    ) -> AgentResult {
        let response = if let Some(cancel) = ctx.cancel_token.clone() {
            tokio::select! {
                res = self.client.send_message(session_id, directory, task.description(), model, agent) => res,
                _ = cancel.cancelled() => {
                    let _ = self.client.abort_session(session_id, directory).await;
                    return AgentResult::failure("Task cancelled", 0).with_terminal_reason(TerminalReason::Cancelled);
                }
            }
        } else {
            self.client
                .send_message(session_id, directory, task.description(), model, agent)
                .await
        };

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                tree.status = "failed".to_string();
                if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                    node.status = "failed".to_string();
                }
                ctx.emit_tree(tree);
                return AgentResult::failure(format!("OpenCode message error: {}", e), 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        if let Some(error) = &response.info.error {
            tree.status = "failed".to_string();
            if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                node.status = "failed".to_string();
            }
            ctx.emit_tree(tree);
            // Extract error message from the error value
            let error_msg = if let Some(msg) = error.get("message").and_then(|v| v.as_str()) {
                msg.to_string()
            } else if let Some(s) = error.as_str() {
                s.to_string()
            } else {
                error.to_string()
            };
            return AgentResult::failure(format!("OpenCode error: {}", error_msg), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }

        let output = extract_text(&response.parts);

        if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
            node.status = "completed".to_string();
        }
        tree.status = "completed".to_string();
        ctx.emit_tree(tree);

        let model_used = match (&response.info.provider_id, &response.info.model_id) {
            (Some(provider), Some(model)) => Some(format!("{}/{}", provider, model)),
            _ => None,
        };

        AgentResult {
            success: true,
            output,
            cost_cents: 0,
            model_used,
            data: Some(json!({
                "agent": "OpenCodeAgent",
                "session_id": session_id,
            })),
            terminal_reason: Some(TerminalReason::Completed),
        }
    }
}
