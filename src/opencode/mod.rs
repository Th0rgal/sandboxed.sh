//! OpenCode API client with SSE streaming support.
//!
//! Provides the OpenCode HTTP API client needed to run tasks via an external
//! OpenCode server, with real-time event streaming.
//!
//! ## Timeout Philosophy
//!
//! Open Agent acts as a **pure pass-through frontend** to OpenCode. We intentionally
//! do NOT impose any timeouts on the SSE event stream. All timeout handling is
//! delegated to OpenCode, which manages tool execution timeouts internally.
//!
//! This design ensures:
//! - Long-running tools (vision analysis, large file operations) complete naturally
//! - Users can abort missions manually via the dashboard if needed
//! - No artificial timeout mismatches between Open Agent and OpenCode
//! - OpenCode remains the single source of truth for execution limits
//!
//! The only timeout we apply is `DEFAULT_REQUEST_TIMEOUT` for initial HTTP connections,
//! not for ongoing SSE streaming.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

/// Default timeout for OpenCode HTTP requests (10 minutes).
/// This is just for the initial HTTP connection - SSE streaming has no timeout.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Number of retries for transient network failures.
const NETWORK_RETRY_COUNT: u32 = 3;

/// Delay between retries (with exponential backoff).
const NETWORK_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

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

        let mut last_error = None;
        for attempt in 0..NETWORK_RETRY_COUNT {
            if attempt > 0 {
                let delay = NETWORK_RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
                tracing::warn!(
                    attempt = attempt + 1,
                    max_attempts = NETWORK_RETRY_COUNT,
                    delay_ms = delay.as_millis(),
                    "Retrying OpenCode session creation after transient failure"
                );
                tokio::time::sleep(delay).await;
            }

            match self.client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        anyhow::bail!("OpenCode /session failed: {} - {}", status, text);
                    }

                    let session: OpenCodeSession =
                        serde_json::from_str(&text).with_context(|| {
                            format!("Failed to parse OpenCode session response: {}", text)
                        })?;
                    return Ok(session);
                }
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %e,
                        "OpenCode session creation failed"
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .map(|e| anyhow::anyhow!(e))
            .unwrap_or_else(|| anyhow::anyhow!("Unknown error"))
            .context("Failed to call OpenCode /session after retries"))
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

        // Subscribe to SSE events (scoped to the session directory, filter by session ID locally)
        let mut event_url = format!("{}/event", self.base_url);
        if !directory.is_empty() {
            event_url.push_str("?directory=");
            event_url.push_str(&urlencoding::encode(&directory));
        }
        tracing::debug!(url = %event_url, "Connecting to OpenCode SSE endpoint");

        let session_id_clone = session_id.clone();

        // Spawn SSE event consumer task using a subprocess curl for reliable SSE streaming
        // This is necessary because reqwest's async streaming has issues with SSE in tokio
        let sse_handle = tokio::spawn(async move {
            let mut event_count = 0u64;
            let mut sse_state = SseState::default();

            tracing::info!(session_id = %session_id_clone, url = %event_url, "Starting SSE consumer with subprocess curl");

            // Use tokio::process to spawn curl for SSE
            let mut child = match tokio::process::Command::new("curl")
                .args([
                    "-N", // No buffering
                    "-s", // Silent
                    "-H",
                    "Accept: text/event-stream",
                    "-H",
                    "Cache-Control: no-cache",
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
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return;
                }
            };

            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut current_event: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();

            tracing::info!(session_id = %session_id_clone, "SSE curl process started, reading lines");

            // No timeout - we let OpenCode handle all timeouts internally.
            // The SSE stream will run until OpenCode sends MessageComplete or closes the connection.
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

                                let parsed = parse_sse_event(
                                    &data,
                                    event_name,
                                    &session_id_clone,
                                    &mut sse_state,
                                );

                                if let Some(ref event) = parsed {
                                    event_count += 1;
                                    let is_complete =
                                        matches!(event, OpenCodeEvent::MessageComplete { .. });

                                    if event_tx.send(event.clone()).await.is_err() {
                                        tracing::debug!(
                                            session_id = %session_id_clone,
                                            "SSE receiver dropped"
                                        );
                                        let _ = child.kill().await;
                                        let _ = child.wait().await;
                                        return;
                                    }
                                    if is_complete {
                                        tracing::info!(
                                            session_id = %session_id_clone,
                                            event_count = event_count,
                                            "OpenCode message completed"
                                        );
                                        let _ = child.kill().await;
                                        let _ = child.wait().await;
                                        return;
                                    }
                                }
                            }

                            current_event = None;
                            data_lines.clear();
                            continue;
                        }

                        if let Some(rest) = trimmed.strip_prefix("event:") {
                            current_event = Some(rest.trim_start().to_string());
                            continue;
                        }

                        if let Some(rest) = trimmed.strip_prefix("data:") {
                            data_lines.push(rest.trim_start().to_string());
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
            let _ = child.wait().await;
        });

        // Spawn message sending task
        let session_id_for_message = session_id.clone();
        let message_handle = tokio::spawn(async move {
            // Delay to ensure SSE subscription is ready and connection is established
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

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
        directory: &str,
        content: &str,
        model: Option<&str>,
        agent: Option<&str>,
    ) -> anyhow::Result<OpenCodeMessageResponse> {
        let mut url = format!("{}/session/{}/message", self.base_url, session_id);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

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

        let mut last_error = None;
        for attempt in 0..NETWORK_RETRY_COUNT {
            if attempt > 0 {
                let delay = NETWORK_RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempt + 1,
                    max_attempts = NETWORK_RETRY_COUNT,
                    delay_ms = delay.as_millis(),
                    "Retrying OpenCode message send after transient failure"
                );
                tokio::time::sleep(delay).await;
            }

            match self.client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if !status.is_success() {
                        anyhow::bail!("OpenCode message failed: {} - {}", status, text);
                    }
                    return self.parse_message_response(&text);
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        attempt = attempt + 1,
                        error = %e,
                        "OpenCode message send failed"
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .map(|e| anyhow::anyhow!(e))
            .unwrap_or_else(|| anyhow::anyhow!("Unknown error"))
            .context(format!(
                "Failed to call OpenCode /session/{}/message after retries",
                session_id
            )))
    }

    /// Parse a message response from OpenCode, handling various response shapes.
    fn parse_message_response(&self, text: &str) -> anyhow::Result<OpenCodeMessageResponse> {
        if text.trim().is_empty() {
            // Empty body indicates the model was never invoked - treat as error
            anyhow::bail!(
                "OpenCode returned an empty response. This usually means the request failed silently \
                (e.g., provider auth issue, rate limit, or session problem). Check OpenCode logs for details."
            );
        }

        // Try the legacy response shape first.
        if let Ok(message) = serde_json::from_str::<OpenCodeMessageResponse>(text) {
            return Ok(message);
        }

        // Fallback to wrapped response shapes (e.g., { "message": { ... } } or { "data": { ... } }).
        let value: serde_json::Value = serde_json::from_str(text)
            .with_context(|| format!("Failed to parse OpenCode message response: {}", text))?;
        let maybe_message = value
            .get("message")
            .or_else(|| value.get("data"))
            .or_else(|| value.get("result"));
        if let Some(inner) = maybe_message {
            let message: OpenCodeMessageResponse = serde_json::from_value(inner.clone())
                .with_context(|| {
                    format!(
                        "Failed to parse wrapped OpenCode message response: {}",
                        text
                    )
                })?;
            return Ok(message);
        }

        Err(anyhow::anyhow!(
            "Failed to parse OpenCode message response: {}",
            text
        ))
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
    pub async fn get_session_status(
        &self,
        session_id: &str,
    ) -> anyhow::Result<OpenCodeSessionStatus> {
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

    /// Fetch all messages for a session (newest last).
    pub async fn get_session_messages(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let messages_url = format!("{}/session/{}/message", self.base_url, session_id);
        let resp = self
            .client
            .get(&messages_url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get OpenCode messages")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode messages query failed: {}", text);
        }

        let messages: Vec<serde_json::Value> = resp
            .json()
            .await
            .context("Failed to parse OpenCode messages")?;
        Ok(messages)
    }

    pub async fn list_questions(&self, directory: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let mut url = format!("{}/question", self.base_url);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get OpenCode questions")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode question list failed: {}", text);
        }

        let questions: Vec<serde_json::Value> = resp
            .json()
            .await
            .context("Failed to parse OpenCode questions")?;
        Ok(questions)
    }

    pub async fn reply_question(
        &self,
        directory: &str,
        request_id: &str,
        answers: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut url = format!("{}/question/{}/reply", self.base_url, request_id);
        if !directory.is_empty() {
            url.push_str("?directory=");
            url.push_str(&urlencoding::encode(directory));
        }

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&json!({ "answers": answers }))
            .send()
            .await
            .context("Failed to reply to OpenCode question")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode question reply failed: {}", text);
        }
        Ok(())
    }

    /// List all sessions in OpenCode.
    /// Returns session metadata including id, title, directory, and timestamps.
    pub async fn list_sessions(&self) -> anyhow::Result<Vec<OpenCodeSessionInfo>> {
        let url = format!("{}/session", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to list OpenCode sessions")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode session list failed: {}", text);
        }

        let sessions: Vec<OpenCodeSessionInfo> = resp
            .json()
            .await
            .context("Failed to parse OpenCode sessions list")?;
        Ok(sessions)
    }

    /// Delete an OpenCode session by ID.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/session/{}", self.base_url, session_id);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .context("Failed to delete OpenCode session")?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenCode session delete failed: {}", text);
        }

        tracing::info!(session_id = %session_id, "Deleted OpenCode session");
        Ok(())
    }

    /// Clean up old sessions that are older than the specified duration.
    /// Returns the number of sessions deleted.
    pub async fn cleanup_old_sessions(&self, max_age: Duration) -> anyhow::Result<usize> {
        let sessions = self.list_sessions().await?;
        let now = chrono::Utc::now();
        let mut deleted = 0;

        for session in sessions {
            // Parse the updated_at timestamp to determine age
            if let Some(updated_at) = &session.updated_at {
                if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(updated_at) {
                    let age = now.signed_duration_since(updated.with_timezone(&chrono::Utc));
                    if age
                        > chrono::Duration::from_std(max_age).unwrap_or(chrono::Duration::hours(1))
                    {
                        if let Err(e) = self.delete_session(&session.id).await {
                            tracing::warn!(
                                session_id = %session.id,
                                error = %e,
                                "Failed to delete old session"
                            );
                        } else {
                            deleted += 1;
                        }
                    }
                }
            }
        }

        tracing::info!(
            deleted = deleted,
            max_age_secs = max_age.as_secs(),
            "Cleaned up old OpenCode sessions"
        );
        Ok(deleted)
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
///
/// The concrete event type lives in the backend module so it can be shared
/// across backends. We keep this alias for backwards compatibility.
pub type OpenCodeEvent = crate::backend::events::ExecutionEvent;

#[derive(Debug, Default)]
struct SseState {
    message_roles: HashMap<String, String>,
    part_buffers: HashMap<String, String>,
    emitted_tool_calls: HashMap<String, ()>,
    emitted_tool_results: HashMap<String, ()>,
    response_tool_args: HashMap<String, String>,
    response_tool_names: HashMap<String, String>,
    /// Track last emitted thinking/text content to deduplicate identical events
    last_emitted_thinking: Option<String>,
    last_emitted_text: Option<String>,
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
    if matches!(
        part_type,
        "thinking" | "reasoning" | "step-start" | "step-finish"
    ) {
        extract_str(part, &["thinking", "reasoning", "text", "content"])
    } else {
        extract_str(part, &["text", "content", "output_text"])
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

    if !matches!(
        part_type,
        "text" | "output_text" | "reasoning" | "thinking" | "step-start" | "step-finish"
    ) {
        tracing::debug!(
            part_type = %part_type,
            "Unhandled part type in handle_part_update"
        );
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

    if role.is_none()
        && matches!(part_type, "text" | "output_text")
        && looks_like_user_prompt(&content)
    {
        return None;
    }

    if matches!(
        part_type,
        "reasoning" | "thinking" | "step-start" | "step-finish"
    ) {
        // Skip if content is identical to last emitted thinking
        if state.last_emitted_thinking.as_ref() == Some(&content) {
            tracing::debug!(
                content_len = content.len(),
                "Skipping duplicate Thinking event"
            );
            return None;
        }
        state.last_emitted_thinking = Some(content.clone());
        tracing::info!(
            part_type = %part_type,
            content_len = content.len(),
            content_preview = %content.chars().take(100).collect::<String>(),
            "Emitting Thinking event from SSE"
        );
        Some(OpenCodeEvent::Thinking { content })
    } else {
        // Skip if content is identical to last emitted text
        if state.last_emitted_text.as_ref() == Some(&content) {
            tracing::debug!(
                content_len = content.len(),
                "Skipping duplicate TextDelta event"
            );
            return None;
        }
        state.last_emitted_text = Some(content.clone());
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
                id: tool_call_id,
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
                id: tool_call_id,
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
                id: tool_call_id,
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
    let json: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(value) => value,
        Err(err) => {
            if data_str.contains('\n') {
                let compact = data_str.replace('\n', "");
                match serde_json::from_str(&compact) {
                    Ok(value) => value,
                    Err(second_err) => {
                        tracing::warn!(
                            error = %err,
                            secondary_error = %second_err,
                            data_preview = %data_str.chars().take(200).collect::<String>(),
                            "Failed to parse OpenCode SSE JSON payload"
                        );
                        return None;
                    }
                }
            } else {
                tracing::warn!(
                    error = %err,
                    data_preview = %data_str.chars().take(200).collect::<String>(),
                    "Failed to parse OpenCode SSE JSON payload"
                );
                return None;
            }
        }
    };

    let event_type = json.get("type").and_then(|v| v.as_str()).or(event_name)?;
    let props = json
        .get("properties")
        .cloned()
        .unwrap_or_else(|| json.clone());

    tracing::debug!(
        event_type = %event_type,
        session_id = %session_id,
        "OpenCode SSE event received"
    );

    // Filter by session ID if the event has one
    let event_session_id = props
        .get("sessionID")
        .or_else(|| props.get("info").and_then(|v| v.get("sessionID")))
        .or_else(|| props.get("part").and_then(|v| v.get("sessionID")))
        .and_then(|v| v.as_str());

    if let Some(event_session_id) = event_session_id {
        if event_session_id != session_id {
            tracing::debug!(
                event_session_id = %event_session_id,
                our_session_id = %session_id,
                event_type = %event_type,
                "Skipping event - session ID mismatch"
            );
            return None;
        }
    }

    match event_type {
        // OpenAI Responses-style streaming
        "response.output_text.delta" => {
            let delta = props
                .get("delta")
                .or_else(|| props.get("text"))
                .or_else(|| props.get("output_text_delta"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if delta.is_empty() {
                None
            } else {
                let response_id = props
                    .get("response")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str());
                let key = response_id.unwrap_or("response.output_text").to_string();
                let buffer = state.part_buffers.entry(key).or_default();
                buffer.push_str(delta);
                Some(OpenCodeEvent::TextDelta {
                    content: buffer.clone(),
                })
            }
        }
        "response.completed" | "response.incomplete" => Some(OpenCodeEvent::MessageComplete {
            session_id: session_id.to_string(),
        }),
        "response.output_item.added" => {
            if let Some(item) = props.get("item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    state.response_tool_names.insert(call_id.clone(), name);
                    if let Some(args) = item.get("arguments").and_then(|v| v.as_str()) {
                        if !args.is_empty() {
                            state
                                .response_tool_args
                                .insert(call_id.clone(), args.to_string());
                        }
                    }
                }
            }
            None
        }
        "response.function_call_arguments.delta" => {
            let call_id = props
                .get("item_id")
                .or_else(|| props.get("call_id"))
                .or_else(|| props.get("id"))
                .and_then(|v| v.as_str());
            let delta = props.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if let (Some(call_id), false) = (call_id, delta.is_empty()) {
                let entry = state
                    .response_tool_args
                    .entry(call_id.to_string())
                    .or_default();
                entry.push_str(delta);
            }
            None
        }
        "response.output_item.done" => {
            if let Some(item) = props.get("item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if state.emitted_tool_calls.contains_key(&call_id) {
                        None
                    } else {
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| state.response_tool_names.get(&call_id).cloned())
                            .unwrap_or_else(|| "unknown".to_string());
                        let args_str = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| state.response_tool_args.get(&call_id).cloned())
                            .unwrap_or_default();
                        let args = if args_str.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&args_str)
                                .unwrap_or_else(|_| json!({ "arguments": args_str }))
                        };
                        state.emitted_tool_calls.insert(call_id.clone(), ());
                        Some(OpenCodeEvent::ToolCall {
                            id: call_id,
                            name,
                            args,
                        })
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
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
            let part_type = props
                .get("part")
                .and_then(|p| p.get("type"))
                .and_then(|v| v.as_str());
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

        // Session idle signals — emitted when OpenCode finishes all work.
        // Treat as completion so we don't hang when response.completed is
        // not emitted (e.g. after response.incomplete from GLM models).
        "session.idle" => {
            tracing::info!(
                session_id = %session_id,
                "session.idle received — treating as message complete"
            );
            Some(OpenCodeEvent::MessageComplete {
                session_id: session_id.to_string(),
            })
        }
        "session.status" => {
            let is_idle = props
                .get("type")
                .or_else(|| props.get("status"))
                .and_then(|v| v.as_str())
                == Some("idle");
            if is_idle {
                tracing::info!(
                    session_id = %session_id,
                    "session.status idle received — treating as message complete"
                );
                Some(OpenCodeEvent::MessageComplete {
                    session_id: session_id.to_string(),
                })
            } else {
                None
            }
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

/// Extended session info returned from listing sessions.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenCodeSessionInfo {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenCodeMessageResponse {
    pub info: OpenCodeAssistantInfo,
    pub parts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
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
        let part_type = part.get("type").and_then(|v| v.as_str());
        if matches!(part_type, Some("text" | "output_text")) {
            if let Some(text) = extract_str(part, &["text", "content", "output_text"]) {
                out.push(text.to_string());
            }
        }
    }
    out.join("\n")
}

fn is_opencode_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("starting opencode server") {
        return true;
    }
    if lower.starts_with("opencode server started") {
        return true;
    }
    if lower.starts_with("sending prompt") {
        return true;
    }
    if lower.starts_with("waiting for completion") {
        return true;
    }
    if lower.starts_with("all tasks completed") {
        return true;
    }
    if lower.starts_with("session ended with error") {
        return true;
    }
    if lower.starts_with("[session.error]") {
        return true;
    }
    if lower.starts_with("session:") || lower.contains("session: ses_") {
        return true;
    }
    if lower.contains("starting opencode server") {
        return true;
    }
    false
}

fn strip_opencode_status_lines(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        if is_opencode_status_line(line) {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// Extract reasoning/thinking content from message parts.
/// This handles both "reasoning" and "thinking" part types.
pub fn extract_reasoning(parts: &[serde_json::Value]) -> Option<String> {
    let mut out = Vec::new();
    for part in parts {
        let part_type = part.get("type").and_then(|v| v.as_str());
        if matches!(part_type, Some("reasoning") | Some("thinking")) {
            if let Some(text) = extract_part_text(part, part_type.unwrap_or("thinking")) {
                if !text.is_empty() {
                    out.push(text.to_string());
                }
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        let combined = out.join("\n");
        let cleaned = strip_opencode_status_lines(&combined);
        if cleaned.trim().is_empty() {
            None
        } else {
            Some(cleaned)
        }
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
