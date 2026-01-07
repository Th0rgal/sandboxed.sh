//! OpenCode API client with SSE streaming support.
//!
//! Provides the OpenCode HTTP API client needed to run tasks via an external
//! OpenCode server, with real-time event streaming.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

/// Default timeout for OpenCode HTTP requests (10 minutes).
/// This is intentionally long to allow for extended tool executions.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Interval for logging heartbeat while waiting for SSE events (30 seconds).
const HEARTBEAT_LOG_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct OpenCodeClient {
    base_url: String,
    client: reqwest::Client,
    default_agent: Option<String>,
    permissive: bool,
}

impl OpenCodeClient {
    pub fn new(
        base_url: impl Into<String>,
        default_agent: Option<String>,
        permissive: bool,
    ) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }

        // Create client with default timeout
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            base_url,
            client,
            default_agent,
            permissive,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn create_session(
        &self,
        directory: &str,
        title: Option<&str>,
    ) -> anyhow::Result<OpenCodeSession> {
        let mut url = format!("{}/session", self.base_url);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        let mut body = serde_json::Map::new();
        if let Some(t) = title {
            body.insert("title".to_string(), json!(t));
        }
        if self.permissive {
            body.insert(
                "permission".to_string(),
                json!([{
                    "permission": "*",
                    "pattern": "*",
                    "action": "allow"
                }]),
            );
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to call OpenCode /session")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("OpenCode /session failed: {} - {}", status, text);
        }

        let session: OpenCodeSession = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse OpenCode session response: {}", text))?;
        Ok(session)
    }

    /// Send a message and stream events in real-time.
    /// Returns a channel receiver for events and a handle to await the final response.
    pub async fn send_message_streaming(
        &self,
        session_id: &str,
        directory: &str,
        content: &str,
        model: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<(
        mpsc::Receiver<OpenCodeEvent>,
        tokio::task::JoinHandle<anyhow::Result<OpenCodeMessageResponse>>,
    )> {
        let session_id = session_id.to_string();
        let directory = directory.to_string();
        let content = content.to_string();
        let model = model.map(|s| s.to_string());
        let agent = agent.map(|s| s.to_string());
        let client = self.clone();

        // Log the message being sent for debugging
        let content_preview: String = content.chars().take(100).collect();
        tracing::info!(
            session_id = %session_id,
            directory = %directory,
            model = ?model,
            agent = ?agent,
            content_preview = %content_preview,
            "Sending message to OpenCode"
        );

        let (event_tx, event_rx) = mpsc::channel::<OpenCodeEvent>(256);

        // Subscribe to SSE events (global stream, filter by session ID locally)
        let event_url = format!("{}/event", self.base_url);
        tracing::debug!(url = %event_url, "Connecting to OpenCode SSE endpoint");

        let session_id_clone = session_id.clone();

        // Spawn SSE event consumer task using a subprocess curl for reliable SSE streaming
        // This is necessary because reqwest's async streaming has issues with SSE in tokio
        let sse_handle = tokio::spawn(async move {
            let mut event_count = 0u64;
            let mut sse_state = SseState::default();

            tracing::warn!(session_id = %session_id_clone, url = %event_url, "Starting SSE consumer with subprocess curl");

            // Use tokio::process to spawn curl for SSE
            let mut child = match tokio::process::Command::new("curl")
                .args([
                    "-N",                      // No buffering
                    "-s",                      // Silent
                    "-H", "Accept: text/event-stream",
                    "-H", "Cache-Control: no-cache",
                    &event_url,
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(session_id = %session_id_clone, error = %e, "Failed to spawn curl for SSE");
                    return;
                }
            };

            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    tracing::error!(session_id = %session_id_clone, "Failed to get curl stdout");
                    return;
                }
            };

            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut current_event: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();

            tracing::warn!(session_id = %session_id_clone, "SSE curl process started, reading lines");

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        tracing::debug!(session_id = %session_id_clone, "SSE curl stdout closed");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end();

                        if trimmed.is_empty() {
                            if !data_lines.is_empty() {
                                let data = data_lines.join("\n");
                                let event_name = current_event.as_deref();
                                tracing::debug!(
                                    session_id = %session_id_clone,
                                    event = ?event_name,
                                    data_preview = %data.chars().take(100).collect::<String>(),
                                    "SSE event block received"
                                );

                                if let Some(event) = parse_sse_event(&data, event_name, &session_id_clone, &mut sse_state) {
                                    event_count += 1;
                                    let is_complete = matches!(event, OpenCodeEvent::MessageComplete { .. });

                                    if event_tx.send(event).await.is_err() {
                                        tracing::debug!(session_id = %session_id_clone, "SSE receiver dropped");
                                        let _ = child.kill().await;
                                        return;
                                    }
                                    if is_complete {
                                        tracing::info!(
                                            session_id = %session_id_clone,
                                            event_count = event_count,
                                            "OpenCode message completed"
                                        );
                                        let _ = child.kill().await;
                                        return;
                                    }
                                }
                            }

                            current_event = None;
                            data_lines.clear();
                            continue;
                        }

                        if let Some(rest) = trimmed.strip_prefix("event: ") {
                            current_event = Some(rest.to_string());
                            continue;
                        }

                        if let Some(rest) = trimmed.strip_prefix("data: ") {
                            data_lines.push(rest.to_string());
                            continue;
                        }

                        if trimmed.starts_with(':') {
                            continue;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(session_id = %session_id_clone, error = %e, "SSE read error");
                        break;
                    }
                }
            }

            let _ = child.kill().await;
        });

        // Spawn message sending task
        let session_id_for_message = session_id.clone();
        let message_handle = tokio::spawn(async move {
            // Delay to ensure SSE subscription is ready and connection is established
            tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

            tracing::debug!(session_id = %session_id_for_message, "Sending HTTP POST to OpenCode");
            let start = std::time::Instant::now();

            let result = client
                .send_message_internal(
                    &session_id,
                    &directory,
                    &content,
                    model.as_deref(),
                    agent.as_deref(),
                )
                .await;

            let elapsed = start.elapsed();
            match &result {
                Ok(_) => {
                    tracing::info!(
                        session_id = %session_id_for_message,
                        elapsed_secs = elapsed.as_secs(),
                        "OpenCode HTTP POST completed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id_for_message,
                        elapsed_secs = elapsed.as_secs(),
                        error = %e,
                        "OpenCode HTTP POST failed"
                    );
                }
            }

            // Cancel SSE task after message completes
            sse_handle.abort();
            result
        });

        Ok((event_rx, message_handle))
    }

    /// Internal method to send message (blocking, waits for response).
    async fn send_message_internal(
        &self,
        session_id: &str,
        _directory: &str,
        content: &str,
        model: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<OpenCodeMessageResponse> {
        let url = format!("{}/session/{}/message", self.base_url, session_id);

        let mut body = serde_json::Map::new();
        body.insert(
            "parts".to_string(),
            json!([{
                "type": "text",
                "text": content
            }]),
        );

        let agent_value = agent
            .map(|s| s.to_string())
            .or_else(|| self.default_agent.clone());
        if let Some(agent_name) = agent_value {
            body.insert("agent".to_string(), json!(agent_name));
        }

        if let Some(model_str) = model {
            if let Some((provider_id, model_id)) = split_model(model_str) {
                body.insert(
                    "model".to_string(),
                    json!({
                        "providerID": provider_id,
                        "modelID": model_id
                    }),
                );
            }
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to call OpenCode /session/{id}/message")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("OpenCode message failed: {} - {}", status, text);
        }

        let message: OpenCodeMessageResponse = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse OpenCode message response: {}", text))?;
        Ok(message)
    }

    /// Legacy non-streaming send_message for backwards compatibility.
    pub async fn send_message(
        &self,
        session_id: &str,
        directory: &str,
        content: &str,
        model: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<OpenCodeMessageResponse> {
        self.send_message_internal(session_id, directory, content, model, agent)
            .await
    }

    pub async fn abort_session(&self, session_id: &str, directory: &str) -> anyhow::Result<()> {
        let mut url = format!("{}/session/{}/abort", self.base_url, session_id);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        tracing::info!(session_id = %session_id, "Aborting OpenCode session");

        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .context("Failed to call OpenCode /session/{id}/abort")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode abort failed: {} - {}", status, text);
        }

        tracing::info!(session_id = %session_id, "OpenCode session aborted successfully");
        Ok(())
    }

    /// Get the status of an OpenCode session for debugging.
    /// Returns session info and the latest messages with their tool states.
    pub async fn get_session_status(&self, session_id: &str) -> anyhow::Result<OpenCodeSessionStatus> {
        // Get session info
        let session_url = format!("{}/session/{}", self.base_url, session_id);
        let session_resp = self
            .client
            .get(&session_url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get OpenCode session")?;

        if !session_resp.status().is_success() {
            let text = session_resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode session query failed: {}", text);
        }

        let session_info: serde_json::Value = session_resp
            .json()
            .await
            .context("Failed to parse session info")?;

        // Get session messages
        let messages_url = format!("{}/session/{}/message", self.base_url, session_id);
        let messages_resp = self
            .client
            .get(&messages_url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get OpenCode messages")?;

        let messages: Vec<serde_json::Value> = if messages_resp.status().is_success() {
            messages_resp.json().await.unwrap_or_default()
        } else {
            Vec::new()
        };

        // Analyze tool states from the latest assistant message
        let mut running_tools = Vec::new();
        let mut completed_tools = Vec::new();

        if let Some(last_assistant_msg) = messages.iter().rev().find(|m| {
            m.get("info")
                .and_then(|i| i.get("role"))
                .and_then(|r| r.as_str())
                == Some("assistant")
        }) {
            if let Some(parts) = last_assistant_msg.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) == Some("tool") {
                        let tool_name = part
                            .get("tool")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let call_id = part
                            .get("callID")
                            .and_then(|c| c.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let status = part
                            .get("state")
                            .and_then(|s| s.get("status"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let tool_info = ToolStatusInfo {
                            name: tool_name,
                            call_id,
                            status: status.clone(),
                        };

                        if status == "running" {
                            running_tools.push(tool_info);
                        } else {
                            completed_tools.push(tool_info);
                        }
                    }
                }
            }
        }

        Ok(OpenCodeSessionStatus {
            session_id: session_id.to_string(),
            session_info,
            message_count: messages.len(),
            running_tools,
            completed_tools,
        })
    }
}

/// Status information about an OpenCode session for debugging.
#[derive(Debug, Clone, Serialize)]
pub struct OpenCodeSessionStatus {
    pub session_id: String,
    pub session_info: serde_json::Value,
    pub message_count: usize,
    pub running_tools: Vec<ToolStatusInfo>,
    pub completed_tools: Vec<ToolStatusInfo>,
}

/// Information about a tool call's status.
#[derive(Debug, Clone, Serialize)]
pub struct ToolStatusInfo {
    pub name: String,
    pub call_id: String,
    pub status: String,
}

/// Events emitted by OpenCode during execution.
#[derive(Debug, Clone)]
pub enum OpenCodeEvent {
    /// Agent is thinking/reasoning
    Thinking { content: String },
    /// Agent is calling a tool
    ToolCall {
        tool_call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// Tool execution completed
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
    },
    /// Text content being streamed
    TextDelta { content: String },
    /// Message execution completed
    MessageComplete { session_id: String },
    /// Error occurred
    Error { message: String },
}

#[derive(Debug, Default)]
struct SseState {
    message_roles: HashMap<String, String>,
    part_buffers: HashMap<String, String>,
    emitted_tool_calls: HashMap<String, ()>,
    emitted_tool_results: HashMap<String, ()>,
}

fn extract_str<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(v) = value.get(*key).and_then(|v| v.as_str()) {
            return Some(v);
        }
    }
    None
}

fn extract_part_text<'a>(part: &'a serde_json::Value, part_type: &str) -> Option<&'a str> {
    if part_type == "thinking" {
        part.get("thinking")
            .and_then(|v| v.as_str())
            .or_else(|| part.get("text").and_then(|v| v.as_str()))
    } else {
        part.get("text").and_then(|v| v.as_str())
    }
}

fn looks_like_user_prompt(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("Conversation so far:\n")
        || trimmed.starts_with("User:\n")
        || trimmed.contains("\nInstructions:\n")
}

fn handle_part_update(props: &serde_json::Value, state: &mut SseState) -> Option<OpenCodeEvent> {
    let part = props.get("part")?;
    let part_type = part.get("type").and_then(|v| v.as_str())?;

    // Handle tool parts - extract tool call/result events from state changes
    if part_type == "tool" {
        return handle_tool_part_update(part, state);
    }

    if !matches!(part_type, "text" | "reasoning" | "thinking") {
        return None;
    }

    let part_id = extract_str(part, &["id", "partID", "partId"]);
    let message_id = extract_str(part, &["messageID", "messageId", "message_id"])
        .or_else(|| extract_str(props, &["messageID", "messageId", "message_id"]));
    let role = message_id
        .and_then(|id| state.message_roles.get(id))
        .map(|s| s.as_str());
    if matches!(role, Some(r) if r != "assistant") {
        return None;
    }

    let delta = props.get("delta").and_then(|v| v.as_str());
    let full_text = extract_part_text(part, part_type);
    let buffer_key = part_id.or(message_id).unwrap_or(part_type).to_string();
    let buffer = state.part_buffers.entry(buffer_key).or_default();

    let content = if let Some(delta) = delta {
        if !delta.is_empty() || full_text.is_none() {
            buffer.push_str(delta);
            buffer.clone()
        } else if let Some(full) = full_text {
            *buffer = full.to_string();
            buffer.clone()
        } else {
            return None;
        }
    } else if let Some(full) = full_text {
        *buffer = full.to_string();
        buffer.clone()
    } else {
        return None;
    };

    if role.is_none() && part_type == "text" && looks_like_user_prompt(&content) {
        return None;
    }

    if matches!(part_type, "reasoning" | "thinking") {
        tracing::info!(
            part_type = %part_type,
            content_len = content.len(),
            content_preview = %content.chars().take(100).collect::<String>(),
            "Emitting Thinking event from SSE"
        );
        Some(OpenCodeEvent::Thinking { content })
    } else {
        tracing::info!(
            part_type = %part_type,
            content_len = content.len(),
            content_preview = %content.chars().take(100).collect::<String>(),
            "Emitting TextDelta event from SSE"
        );
        Some(OpenCodeEvent::TextDelta { content })
    }
}

/// Handle tool part updates from message.part.updated events.
/// OpenCode sends tool calls/results via message.part.updated with part.type = "tool"
fn handle_tool_part_update(
    part: &serde_json::Value,
    state: &mut SseState,
) -> Option<OpenCodeEvent> {
    tracing::debug!(part = ?part, "Handling tool part update");

    let state_obj = part.get("state")?;
    let status = state_obj.get("status").and_then(|v| v.as_str())?;

    tracing::debug!(status = %status, "Tool part status");

    // Extract common fields
    let tool_call_id = part
        .get("callID")
        .or_else(|| part.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let tool_name = part
        .get("tool")
        .or_else(|| part.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    match status {
        // Tool is starting to run - emit ToolCall event
        "running" => {
            if state.emitted_tool_calls.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_calls.insert(tool_call_id.clone(), ());
            let args = state_obj
                .get("input")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            tracing::info!(
                tool_call_id = %tool_call_id,
                name = %tool_name,
                "OpenCode tool_call event from message.part.updated"
            );

            Some(OpenCodeEvent::ToolCall {
                tool_call_id,
                name: tool_name,
                args,
            })
        }
        // Tool completed - emit ToolResult event
        "completed" => {
            if state.emitted_tool_results.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_results.insert(tool_call_id.clone(), ());
            let result = state_obj
                .get("output")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            tracing::info!(
                tool_call_id = %tool_call_id,
                name = %tool_name,
                "OpenCode tool_result event from message.part.updated"
            );

            Some(OpenCodeEvent::ToolResult {
                tool_call_id,
                name: tool_name,
                result,
            })
        }
        // Tool errored - emit ToolResult with error
        "error" => {
            let error_msg = state_obj
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            if state.emitted_tool_results.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_results.insert(tool_call_id.clone(), ());
            let result = serde_json::json!({ "error": error_msg });

            tracing::info!(
                tool_call_id = %tool_call_id,
                name = %tool_name,
                error = %error_msg,
                "OpenCode tool error from message.part.updated"
            );

            Some(OpenCodeEvent::ToolResult {
                tool_call_id,
                name: tool_name,
                result,
            })
        }
        // pending or other states - don't emit events yet
        _ => None,
    }
}

/// Parse an SSE event line into an OpenCodeEvent.
fn parse_sse_event(
    data_str: &str,
    event_name: Option<&str>,
    session_id: &str,
    state: &mut SseState,
) -> Option<OpenCodeEvent> {
    let json: serde_json::Value = serde_json::from_str(data_str).ok()?;

    let event_type = json
        .get("type")
        .and_then(|v| v.as_str())
        .or(event_name)?;
    let props = json.get("properties").cloned().unwrap_or(json!({}));

    // Log all event types for debugging
    tracing::warn!(
        event_type = %event_type,
        session_id = %session_id,
        props_keys = ?props.as_object().map(|o| o.keys().collect::<Vec<_>>()),
        "OpenCode SSE event received"
    );

    // Filter by session ID if the event has one
    let event_session_id = props
        .get("sessionID")
        .or_else(|| props.get("info").and_then(|v| v.get("sessionID")))
        .or_else(|| props.get("part").and_then(|v| v.get("sessionID")))
        .and_then(|v| v.as_str());

    tracing::warn!(
        event_session_id = ?event_session_id,
        our_session_id = %session_id,
        event_type = %event_type,
        "Checking session ID filter"
    );

    if let Some(event_session_id) = event_session_id {
        if event_session_id != session_id {
            tracing::warn!(
                event_session_id = %event_session_id,
                our_session_id = %session_id,
                event_type = %event_type,
                "SKIPPING event - session ID mismatch"
            );
            return None;
        }
    }

    match event_type {
        // Message info updates
        "message.updated" => {
            if let Some(info) = props.get("info") {
                if let (Some(id), Some(role)) = (
                    info.get("id").and_then(|v| v.as_str()),
                    info.get("role").and_then(|v| v.as_str()),
                ) {
                    state.message_roles.insert(id.to_string(), role.to_string());
                }
            }
            if props.get("part").is_some() {
                handle_part_update(&props, state)
            } else {
                None
            }
        }

        // Message part streaming events
        "message.part.updated" => {
            let part_type = props.get("part").and_then(|p| p.get("type")).and_then(|v| v.as_str());
            tracing::info!(
                part_type = ?part_type,
                has_delta = props.get("delta").is_some(),
                delta_len = props.get("delta").and_then(|v| v.as_str()).map(|s| s.len()),
                "message.part.updated event received"
            );
            handle_part_update(&props, state)
        }

        // Tool call events
        // Message completion
        "message.completed" | "assistant.message.completed" => {
            Some(OpenCodeEvent::MessageComplete {
                session_id: session_id.to_string(),
            })
        }

        // Error events
        "error" | "message.error" => {
            let message = props
                .get("message")
                .or(props.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Some(OpenCodeEvent::Error { message })
        }

        _ => {
            // Log unknown event types to help debug which events OpenCode sends
            tracing::debug!(
                event_type = %event_type,
                props = ?props,
                "Unknown OpenCode SSE event type"
            );
            None
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeSession {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeMessageResponse {
    pub info: OpenCodeAssistantInfo,
    pub parts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeAssistantInfo {
    #[serde(default)]
    #[serde(rename = "providerID")]
    pub provider_id: Option<String>,
    #[serde(default)]
    #[serde(rename = "modelID")]
    pub model_id: Option<String>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

pub fn extract_text(parts: &[serde_json::Value]) -> String {
    let mut out = Vec::new();
    for part in parts {
        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                out.push(text.to_string());
            }
        }
    }
    out.join("\n")
}

/// Extract reasoning/thinking content from message parts.
/// This handles both "reasoning" and "thinking" part types.
pub fn extract_reasoning(parts: &[serde_json::Value]) -> Option<String> {
    let mut out = Vec::new();
    for part in parts {
        let part_type = part.get("type").and_then(|v| v.as_str());
        if matches!(part_type, Some("reasoning") | Some("thinking")) {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    out.push(text.to_string());
                }
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

fn split_model(model: &str) -> Option<(String, String)> {
    let trimmed = model.trim();
    let mut parts = trimmed.splitn(2, '/');
    let provider = parts.next()?.trim();
    let model_id = parts.next()?.trim();
    if provider.is_empty() || model_id.is_empty() {
        None
    } else {
        Some((provider.to_string(), model_id.to_string()))
    }
}
