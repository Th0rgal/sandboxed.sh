//! OpenCode-backed agent - delegates task execution to an external OpenCode server.
//!
//! This agent streams real-time events (thinking, tool calls, results) from OpenCode
//! to the control broadcast channel, enabling live UI updates in the dashboard.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use crate::agents::{Agent, AgentContext, AgentId, AgentResult, AgentType, TerminalReason};
use crate::api::control::{AgentEvent, AgentTreeNode, ControlRunState};
use crate::config::Config;
use crate::opencode::{extract_reasoning, extract_text, OpenCodeClient, OpenCodeEvent};
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
        tracing::debug!(
            event_type = ?std::mem::discriminant(oc_event),
            has_control_events = ctx.control_events.is_some(),
            mission_id = ?ctx.mission_id,
            "forward_event called"
        );

        let Some(events_tx) = &ctx.control_events else {
            tracing::debug!("forward_event: no control_events channel, skipping");
            return;
        };

        let agent_event = match oc_event {
            OpenCodeEvent::Thinking { content } => {
                tracing::info!(
                    content_len = content.len(),
                    content_preview = %content.chars().take(100).collect::<String>(),
                    "Forwarding Thinking event to control broadcast"
                );
                AgentEvent::Thinking {
                    content: content.clone(),
                    done: false,
                    mission_id: ctx.mission_id,
                }
            }
            OpenCodeEvent::TextDelta { content } => {
                tracing::info!(
                    content_len = content.len(),
                    content_preview = %content.chars().take(100).collect::<String>(),
                    "Forwarding TextDelta as Thinking event"
                );
                AgentEvent::Thinking {
                    content: content.clone(),
                    done: false,
                    mission_id: ctx.mission_id,
                }
            }
            OpenCodeEvent::ToolCall { id, name, args } => {
                tracing::info!(
                    tool_call_id = %id,
                    name = %name,
                    "Forwarding tool_call event to control broadcast"
                );
                AgentEvent::ToolCall {
                    tool_call_id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    mission_id: ctx.mission_id,
                }
            }
            OpenCodeEvent::ToolResult { id, name, result } => AgentEvent::ToolResult {
                tool_call_id: id.clone(),
                name: name.clone(),
                result: result.clone(),
                mission_id: ctx.mission_id,
            },
            OpenCodeEvent::Error { message } => AgentEvent::Error {
                message: message.clone(),
                mission_id: ctx.mission_id,
                resumable: ctx.mission_id.is_some(), // Can resume if within a mission
            },
            OpenCodeEvent::MessageComplete { .. } => return, // Don't forward completion marker
            OpenCodeEvent::TurnSummary { .. } => return,     // Summary is handled elsewhere
        };

        match events_tx.send(agent_event) {
            Ok(receiver_count) => {
                tracing::debug!(
                    receiver_count = receiver_count,
                    "Successfully sent event to broadcast channel"
                );
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "Failed to send event to broadcast channel (no receivers?)"
                );
            }
        }
    }

    fn latest_assistant_parts(messages: &[serde_json::Value]) -> Option<Vec<serde_json::Value>> {
        messages
            .iter()
            .rev()
            .find(|m| {
                m.get("info")
                    .and_then(|i| i.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("assistant")
            })
            .and_then(|m| {
                m.get("parts")
                    .or_else(|| m.get("content"))
                    .and_then(|p| p.as_array())
                    .map(|parts| parts.to_vec())
            })
    }

    fn emit_tool_events_from_parts(&self, parts: &[serde_json::Value], ctx: &AgentContext) {
        let Some(events_tx) = &ctx.control_events else {
            return;
        };

        for part in parts {
            if part.get("type").and_then(|v| v.as_str()) != Some("tool") {
                continue;
            }

            let tool_call_id = part
                .get("callID")
                .or_else(|| part.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let name = part
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let args = part
                .get("state")
                .and_then(|s| s.get("input"))
                .cloned()
                .unwrap_or_else(|| json!({}));

            let _ = events_tx.send(AgentEvent::ToolCall {
                tool_call_id: tool_call_id.clone(),
                name: name.clone(),
                args,
                mission_id: ctx.mission_id,
            });

            if let Some(state) = part.get("state") {
                let status = state
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                if status != "running" {
                    let result = state
                        .get("output")
                        .cloned()
                        .or_else(|| state.get("error").cloned())
                        .unwrap_or_else(|| json!({}));
                    let _ = events_tx.send(AgentEvent::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        name: name.clone(),
                        result,
                        mission_id: ctx.mission_id,
                    });
                }
            }
        }
    }

    fn handle_frontend_tool_call(
        &self,
        tool_call_id: &str,
        name: &str,
        session_id: &str,
        directory: &str,
        ctx: &AgentContext,
    ) {
        if name != "question" {
            return;
        }
        let Some(tool_hub) = &ctx.frontend_tool_hub else {
            return;
        };
        let tool_hub = Arc::clone(tool_hub);

        let client = self.client.clone();
        let tool_call_id = tool_call_id.to_string();
        let session_id = session_id.to_string();
        let directory = directory.to_string();
        let events_tx = ctx.control_events.clone();
        let control_status = ctx.control_status.clone();
        let mission_id = ctx.mission_id;
        let resumable = ctx.mission_id.is_some();

        tokio::spawn(async move {
            if let (Some(status), Some(events), Some(mid)) =
                (&control_status, &events_tx, mission_id)
            {
                let (queue_len, mission_id_opt) = {
                    let mut guard = status.write().await;
                    if let Some(existing) = guard.mission_id {
                        if existing != mid {
                            (guard.queue_len, guard.mission_id)
                        } else {
                            guard.state = ControlRunState::WaitingForTool;
                            (guard.queue_len, guard.mission_id)
                        }
                    } else {
                        guard.mission_id = Some(mid);
                        guard.state = ControlRunState::WaitingForTool;
                        (guard.queue_len, guard.mission_id)
                    }
                };
                if mission_id_opt == Some(mid) {
                    let _ = events.send(AgentEvent::Status {
                        state: ControlRunState::WaitingForTool,
                        queue_len,
                        mission_id: mission_id_opt,
                    });
                }
            }
            let rx = tool_hub.register(tool_call_id.clone()).await;
            let Ok(result) = rx.await else {
                return;
            };
            if let (Some(status), Some(events), Some(mid)) =
                (&control_status, &events_tx, mission_id)
            {
                let (queue_len, mission_id_opt) = {
                    let mut guard = status.write().await;
                    if let Some(existing) = guard.mission_id {
                        if existing != mid {
                            (guard.queue_len, guard.mission_id)
                        } else {
                            guard.state = ControlRunState::Running;
                            (guard.queue_len, guard.mission_id)
                        }
                    } else {
                        guard.mission_id = Some(mid);
                        guard.state = ControlRunState::Running;
                        (guard.queue_len, guard.mission_id)
                    }
                };
                if mission_id_opt == Some(mid) {
                    let _ = events.send(AgentEvent::Status {
                        state: ControlRunState::Running,
                        queue_len,
                        mission_id: mission_id_opt,
                    });
                }
            }

            let answers = result
                .get("answers")
                .cloned()
                .unwrap_or_else(|| result.clone());

            let request_id = match client.list_questions(&directory).await {
                Ok(list) => list
                    .iter()
                    .find(|q| {
                        q.get("sessionID").and_then(|v| v.as_str()) == Some(session_id.as_str())
                            && q.get("tool")
                                .and_then(|t| t.get("callID"))
                                .and_then(|v| v.as_str())
                                == Some(tool_call_id.as_str())
                    })
                    .and_then(|q| q.get("id").and_then(|v| v.as_str()).map(|v| v.to_string())),
                Err(e) => {
                    if let Some(tx) = &events_tx {
                        let _ = tx.send(AgentEvent::Error {
                            message: format!("Failed to list OpenCode questions: {}", e),
                            mission_id,
                            resumable,
                        });
                    }
                    None
                }
            };

            let Some(request_id) = request_id else {
                if let Some(tx) = &events_tx {
                    let _ = tx.send(AgentEvent::Error {
                        message: format!(
                            "No pending question found for tool_call_id {}",
                            tool_call_id
                        ),
                        mission_id,
                        resumable,
                    });
                }
                return;
            };

            if let Err(e) = client
                .reply_question(&directory, &request_id, answers)
                .await
            {
                if let Some(tx) = &events_tx {
                    let _ = tx.send(AgentEvent::Error {
                        message: format!("Failed to reply to question: {}", e),
                        mission_id,
                        resumable,
                    });
                }
            }
        });
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
        let budget_cents = task.cost().budget_cents().unwrap_or(0);

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

        // Use the configured default model (if any)
        let selected_model: Option<String> = ctx.config.default_model.clone();
        if let Some(ref model) = selected_model {
            task.analysis_mut().selected_model = Some(model.clone());
        }

        let agent_name = ctx
            .config
            .opencode_agent
            .as_deref()
            .or(self.default_agent.as_deref());

        // Use streaming to get real-time events
        let streaming_result = self
            .client
            .send_message_streaming(
                &session.id,
                &directory,
                task.description(),
                selected_model.as_deref(),
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
                        selected_model.as_deref(),
                        agent_name,
                        tree,
                    )
                    .await;
            }
        };

        // Process streaming events with cancellation support
        let mut saw_sse_event = false;
        let mut sse_text_buffer = String::new();
        let mut message_handle = message_handle;
        let mut response_result = None;
        let response = if let Some(cancel) = ctx.cancel_token.clone() {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        let _ = self.client.abort_session(&session.id, &directory).await;
                        message_handle.abort();
                        return AgentResult::failure("Task cancelled", 0).with_terminal_reason(TerminalReason::Cancelled);
                    }
                    res = &mut message_handle => {
                        response_result = Some(res);
                        break;
                    }
                    event = event_rx.recv() => {
                        match event {
                            Some(oc_event) => {
                                saw_sse_event = true;
                                tracing::debug!(
                                    event_type = ?std::mem::discriminant(&oc_event),
                                    "Received event from OpenCode SSE channel"
                                );

                                if let OpenCodeEvent::TextDelta { content } = &oc_event {
                                    if !content.trim().is_empty() {
                                        sse_text_buffer = content.clone();
                                    }
                                }

                                if let OpenCodeEvent::ToolCall { id, name, .. } = &oc_event {
                                    self.handle_frontend_tool_call(
                                        id,
                                        name,
                                        &session.id,
                                        &directory,
                                        ctx,
                                    );
                                }
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

            let result = match response_result.take() {
                Some(result) => result,
                None => message_handle.await,
            };
            match result {
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
            loop {
                tokio::select! {
                    res = &mut message_handle => {
                        response_result = Some(res);
                        break;
                    }
                    event = event_rx.recv() => {
                        match event {
                            Some(oc_event) => {
                                saw_sse_event = true;
                                if let OpenCodeEvent::TextDelta { content } = &oc_event {
                                    if !content.trim().is_empty() {
                                        sse_text_buffer = content.clone();
                                    }
                                }
                                if let OpenCodeEvent::ToolCall { id, name, .. } = &oc_event {
                                    self.handle_frontend_tool_call(
                                        id,
                                        name,
                                        &session.id,
                                        &directory,
                                        ctx,
                                    );
                                }
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

            let result = match response_result.take() {
                Some(result) => result,
                None => message_handle.await,
            };
            match result {
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

        let mut response = response;
        if response.parts.is_empty() || !saw_sse_event {
            match self.client.get_session_messages(&session.id).await {
                Ok(messages) => {
                    if let Some(parts) = Self::latest_assistant_parts(&messages) {
                        if response.parts.is_empty() {
                            response.parts = parts.clone();
                        }
                        if !saw_sse_event {
                            self.emit_tool_events_from_parts(&parts, ctx);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session.id,
                        error = %e,
                        "Failed to backfill OpenCode message parts"
                    );
                }
            }
        }

        // Extract and emit any reasoning content from the final response
        // This ensures extended thinking content is captured even if not streamed via SSE
        if let Some(events_tx) = &ctx.control_events {
            if let Some(reasoning_content) = extract_reasoning(&response.parts) {
                tracing::info!(
                    reasoning_len = reasoning_content.len(),
                    "Emitting reasoning content from final response"
                );
                let _ = events_tx.send(AgentEvent::Thinking {
                    content: reasoning_content,
                    done: false,
                    mission_id: ctx.mission_id,
                });
            }
            // Emit final thinking done marker
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

        let mut output = extract_text(&response.parts);
        if output.trim().is_empty() && !sse_text_buffer.trim().is_empty() {
            tracing::info!(
                session_id = %session.id,
                output_len = sse_text_buffer.len(),
                "Using SSE text buffer as final output"
            );
            output = sse_text_buffer.clone();
        }
        if output.trim().is_empty() {
            let part_types: Vec<String> = response
                .parts
                .iter()
                .filter_map(|part| {
                    part.get("type")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            tracing::warn!(
                session_id = %session.id,
                part_count = response.parts.len(),
                part_types = ?part_types,
                "OpenCode response contained no text output"
            );
        }

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
            cost_source: crate::agents::types::CostSource::Unknown,
            usage: None,
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
    #[allow(clippy::too_many_arguments)]
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
            cost_source: crate::agents::types::CostSource::Unknown,
            usage: None,
            model_used,
            data: Some(json!({
                "agent": "OpenCodeAgent",
                "session_id": session_id,
            })),
            terminal_reason: Some(TerminalReason::Completed),
        }
    }
}
