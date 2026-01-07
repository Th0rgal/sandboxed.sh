//! OpenCode-backed agent - delegates task execution to an external OpenCode server.
//!
//! This agent streams real-time events (thinking, tool calls, results) from OpenCode
//! to the control broadcast channel, enabling live UI updates in the dashboard.

use async_trait::async_trait;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::agents::{Agent, AgentContext, AgentId, AgentResult, AgentType, TerminalReason};
use crate::api::control::{AgentEvent, AgentTreeNode};
use crate::config::Config;
use crate::opencode::{extract_text, OpenCodeClient, OpenCodeEvent};
use crate::task::Task;

/// How long to wait without events before checking if a tool is stuck.
const TOOL_STUCK_CHECK_INTERVAL: Duration = Duration::from_secs(120);

/// Maximum time a tool can be "running" without any output before we consider it stuck.
const TOOL_STUCK_TIMEOUT: Duration = Duration::from_secs(300);

/// Message to send to the agent when a tool appears stuck, asking it to self-diagnose.
const STUCK_TOOL_RECOVERY_PROMPT: &str = r#"IMPORTANT: The previous operation appears to have stalled - there has been no activity for over 2 minutes.

Please check:
1. Is the bash command or tool still running? Use `ps aux | grep` to check
2. If the process has exited or crashed, acknowledge what happened
3. If the command is still running but taking a long time, explain what it's doing
4. If something went wrong, try an alternative approach

Do NOT just retry the same command blindly - first investigate what happened."#;

pub struct OpenCodeAgent {
    id: AgentId,
    client: OpenCodeClient,
    default_agent: Option<String>,
    /// Timeout in seconds after which to auto-abort stuck tools (0 = disabled).
    tool_stuck_abort_timeout_secs: u64,
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
            tool_stuck_abort_timeout_secs: config.tool_stuck_abort_timeout_secs,
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

    /// Send a recovery message to the agent asking it to investigate a stuck tool.
    /// This aborts the current operation and sends a new message.
    async fn send_recovery_message(
        &self,
        session_id: &str,
        directory: &str,
        stuck_tools: &str,
        model: Option<&str>,
        agent: Option<&str>,
        ctx: &AgentContext,
    ) -> anyhow::Result<(
        mpsc::Receiver<OpenCodeEvent>,
        tokio::task::JoinHandle<anyhow::Result<crate::opencode::OpenCodeMessageResponse>>,
    )> {
        // First, abort the current session to free it up
        tracing::info!(
            session_id = %session_id,
            stuck_tools = %stuck_tools,
            "Aborting stuck session and sending recovery message"
        );

        if let Err(e) = self.client.abort_session(session_id, directory).await {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "Failed to abort session (may already be complete)"
            );
        }

        // Small delay to let OpenCode process the abort
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send a recovery message asking the agent to investigate
        let recovery_message = format!(
            "{}\n\nThe tool(s) that appear stuck: {}",
            STUCK_TOOL_RECOVERY_PROMPT, stuck_tools
        );

        // Emit an event so the frontend knows we're trying to recover
        if let Some(events_tx) = &ctx.control_events {
            let _ = events_tx.send(AgentEvent::Thinking {
                content: format!("Asking agent to investigate stuck tool: {}", stuck_tools),
                done: false,
                mission_id: ctx.mission_id,
            });
        }

        self.client
            .send_message_streaming(session_id, directory, &recovery_message, model, agent)
            .await
    }

    /// Check if a tool appears to be stuck by querying OpenCode session status.
    /// Returns the name of the stuck tool if found.
    async fn check_for_stuck_tool(&self, session_id: &str) -> Option<String> {
        match self.client.get_session_status(session_id).await {
            Ok(status) => {
                if !status.running_tools.is_empty() {
                    let tool_names: Vec<_> = status
                        .running_tools
                        .iter()
                        .map(|t| t.name.clone())
                        .collect();
                    tracing::warn!(
                        session_id = %session_id,
                        running_tools = ?tool_names,
                        "Found tools marked as 'running' in OpenCode session"
                    );
                    Some(tool_names.join(", "))
                } else {
                    None
                }
            }
            Err(e) => {
                tracing::debug!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to check OpenCode session status"
                );
                None
            }
        }
    }

    /// Forward an OpenCode event to the control broadcast channel.
    fn forward_event(&self, oc_event: &OpenCodeEvent, ctx: &AgentContext) {
        let Some(events_tx) = &ctx.control_events else {
            tracing::trace!("forward_event: no control_events channel, skipping");
            return;
        };

        let agent_event = match oc_event {
            OpenCodeEvent::Thinking { content } => {
                tracing::debug!(
                    content_len = content.len(),
                    "Forwarding Thinking event to control broadcast"
                );
                AgentEvent::Thinking {
                    content: content.clone(),
                    done: false,
                    mission_id: ctx.mission_id,
                }
            }
            OpenCodeEvent::TextDelta { content } => {
                tracing::trace!(
                    content_len = content.len(),
                    "Forwarding TextDelta as Thinking event"
                );
                AgentEvent::Thinking {
                    content: content.clone(),
                    done: false,
                    mission_id: ctx.mission_id,
                }
            }
            OpenCodeEvent::ToolCall {
                tool_call_id,
                name,
                args,
            } => {
                tracing::info!(
                    tool_call_id = %tool_call_id,
                    name = %name,
                    "Forwarding tool_call event to control broadcast"
                );
                AgentEvent::ToolCall {
                    tool_call_id: tool_call_id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                    mission_id: ctx.mission_id,
                }
            }
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

        // Process streaming events with cancellation support and stuck tool detection
        let response = if let Some(cancel) = ctx.cancel_token.clone() {
            let mut last_event_time = Instant::now();
            let mut last_stuck_check = Instant::now();
            let mut stuck_tool_warned = false;
            let mut current_tool: Option<String> = None;

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
                                last_event_time = Instant::now();
                                stuck_tool_warned = false; // Reset warning on new event

                                // Track current tool state
                                match &oc_event {
                                    OpenCodeEvent::ToolCall { name, .. } => {
                                        current_tool = Some(name.clone());
                                    }
                                    OpenCodeEvent::ToolResult { .. } => {
                                        current_tool = None;
                                    }
                                    _ => {}
                                }

                                self.forward_event(&oc_event, ctx);
                                if matches!(oc_event, OpenCodeEvent::MessageComplete { .. }) {
                                    break;
                                }
                            }
                            None => break, // Channel closed
                        }
                    }
                    _ = tokio::time::sleep(TOOL_STUCK_CHECK_INTERVAL) => {
                        let elapsed = last_event_time.elapsed();
                        let since_last_check = last_stuck_check.elapsed();

                        // Only check periodically to avoid hammering OpenCode
                        if since_last_check >= TOOL_STUCK_CHECK_INTERVAL {
                            last_stuck_check = Instant::now();

                            tracing::info!(
                                session_id = %session.id,
                                elapsed_secs = elapsed.as_secs(),
                                current_tool = ?current_tool,
                                "No OpenCode events received, checking for stuck tools"
                            );

                            // Check if there's a stuck tool in OpenCode
                            if let Some(stuck_tools) = self.check_for_stuck_tool(&session.id).await {
                                if elapsed >= TOOL_STUCK_TIMEOUT && !stuck_tool_warned {
                                    stuck_tool_warned = true;

                                    tracing::warn!(
                                        session_id = %session.id,
                                        stuck_tools = %stuck_tools,
                                        elapsed_secs = elapsed.as_secs(),
                                        "Tool appears stuck - sending recovery message to agent"
                                    );

                                    // Send recovery message asking agent to investigate
                                    match self.send_recovery_message(
                                        &session.id,
                                        &directory,
                                        &stuck_tools,
                                        model_override.as_deref(),
                                        agent_name,
                                        ctx,
                                    ).await {
                                        Ok((new_rx, new_handle)) => {
                                            // Switch to the new event stream
                                            message_handle.abort();
                                            event_rx = new_rx;
                                            // We can't reassign message_handle in this scope,
                                            // so we'll process the new stream inline
                                            drop(new_handle); // Let it run, we'll use the events

                                            // Reset timers for the new message
                                            last_event_time = Instant::now();
                                            last_stuck_check = Instant::now();
                                            stuck_tool_warned = false;
                                            current_tool = None;

                                            tracing::info!(
                                                session_id = %session.id,
                                                "Switched to recovery message event stream"
                                            );
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                session_id = %session.id,
                                                error = %e,
                                                "Failed to send recovery message"
                                            );

                                            // Fall back to emitting a warning
                                            if let Some(events_tx) = &ctx.control_events {
                                                let _ = events_tx.send(AgentEvent::Error {
                                                    message: format!(
                                                        "Tool '{}' may be stuck - no activity for {} seconds. Recovery failed: {}",
                                                        stuck_tools,
                                                        elapsed.as_secs(),
                                                        e
                                                    ),
                                                    mission_id: ctx.mission_id,
                                                });
                                            }
                                        }
                                    }
                                }

                                // Auto-abort if configured and timeout exceeded (as final fallback)
                                if self.tool_stuck_abort_timeout_secs > 0
                                    && elapsed.as_secs() >= self.tool_stuck_abort_timeout_secs
                                {
                                    tracing::warn!(
                                        session_id = %session.id,
                                        stuck_tools = %stuck_tools,
                                        timeout_secs = self.tool_stuck_abort_timeout_secs,
                                        "Auto-aborting stuck session due to TOOL_STUCK_ABORT_TIMEOUT_SECS"
                                    );

                                    let _ = self.client.abort_session(&session.id, &directory).await;
                                    message_handle.abort();
                                    tree.status = "failed".to_string();
                                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                                        node.status = "failed".to_string();
                                    }
                                    ctx.emit_tree(tree);
                                    return AgentResult::failure(
                                        format!("Tool '{}' timed out after {} seconds with no progress", stuck_tools, elapsed.as_secs()),
                                        0
                                    ).with_terminal_reason(TerminalReason::Stalled);
                                }
                            }
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
            // No cancel token - process events with stuck detection
            let mut last_event_time = Instant::now();
            let mut last_stuck_check = Instant::now();
            let mut stuck_tool_warned = false;

            loop {
                tokio::select! {
                    event = event_rx.recv() => {
                        match event {
                            Some(oc_event) => {
                                last_event_time = Instant::now();
                                stuck_tool_warned = false;
                                self.forward_event(&oc_event, ctx);
                                if matches!(oc_event, OpenCodeEvent::MessageComplete { .. }) {
                                    break;
                                }
                            }
                            None => break, // Channel closed
                        }
                    }
                    _ = tokio::time::sleep(TOOL_STUCK_CHECK_INTERVAL) => {
                        let elapsed = last_event_time.elapsed();
                        let since_last_check = last_stuck_check.elapsed();

                        if since_last_check >= TOOL_STUCK_CHECK_INTERVAL {
                            last_stuck_check = Instant::now();

                            if let Some(stuck_tools) = self.check_for_stuck_tool(&session.id).await {
                                if elapsed >= TOOL_STUCK_TIMEOUT && !stuck_tool_warned {
                                    stuck_tool_warned = true;

                                    tracing::warn!(
                                        session_id = %session.id,
                                        stuck_tools = %stuck_tools,
                                        elapsed_secs = elapsed.as_secs(),
                                        "Tool appears stuck - sending recovery message to agent"
                                    );

                                    // Send recovery message asking agent to investigate
                                    match self.send_recovery_message(
                                        &session.id,
                                        &directory,
                                        &stuck_tools,
                                        model_override.as_deref(),
                                        agent_name,
                                        ctx,
                                    ).await {
                                        Ok((new_rx, new_handle)) => {
                                            message_handle.abort();
                                            event_rx = new_rx;
                                            drop(new_handle);

                                            last_event_time = Instant::now();
                                            last_stuck_check = Instant::now();
                                            stuck_tool_warned = false;

                                            tracing::info!(
                                                session_id = %session.id,
                                                "Switched to recovery message event stream"
                                            );
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                session_id = %session.id,
                                                error = %e,
                                                "Failed to send recovery message"
                                            );

                                            if let Some(events_tx) = &ctx.control_events {
                                                let _ = events_tx.send(AgentEvent::Error {
                                                    message: format!(
                                                        "Tool '{}' may be stuck - no activity for {} seconds. Recovery failed: {}",
                                                        stuck_tools,
                                                        elapsed.as_secs(),
                                                        e
                                                    ),
                                                    mission_id: ctx.mission_id,
                                                });
                                            }
                                        }
                                    }
                                }

                                // Auto-abort if configured and timeout exceeded (as final fallback)
                                if self.tool_stuck_abort_timeout_secs > 0
                                    && elapsed.as_secs() >= self.tool_stuck_abort_timeout_secs
                                {
                                    tracing::warn!(
                                        session_id = %session.id,
                                        stuck_tools = %stuck_tools,
                                        timeout_secs = self.tool_stuck_abort_timeout_secs,
                                        "Auto-aborting stuck session due to TOOL_STUCK_ABORT_TIMEOUT_SECS"
                                    );

                                    let _ = self.client.abort_session(&session.id, &directory).await;
                                    message_handle.abort();
                                    tree.status = "failed".to_string();
                                    if let Some(node) = tree.children.iter_mut().find(|n| n.id == "opencode") {
                                        node.status = "failed".to_string();
                                    }
                                    ctx.emit_tree(tree);
                                    return AgentResult::failure(
                                        format!("Tool '{}' timed out after {} seconds with no progress", stuck_tools, elapsed.as_secs()),
                                        0
                                    ).with_terminal_reason(TerminalReason::Stalled);
                                }
                            }
                        }
                    }
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
