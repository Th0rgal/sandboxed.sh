pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{CodexClient, CodexConfig, CodexEvent};

/// Codex backend that spawns the Codex CLI for mission execution.
pub struct CodexBackend {
    id: String,
    name: String,
    config: Arc<RwLock<CodexConfig>>,
    workspace_exec: Option<crate::workspace_exec::WorkspaceExec>,
}

impl CodexBackend {
    pub fn new() -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(CodexConfig::default())),
            workspace_exec: None,
        }
    }

    pub fn with_config(config: CodexConfig) -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: None,
        }
    }

    pub fn with_config_and_workspace(
        config: CodexConfig,
        workspace_exec: crate::workspace_exec::WorkspaceExec,
    ) -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: Some(workspace_exec),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: CodexConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> CodexConfig {
        self.config.read().await.clone()
    }
}

impl Default for CodexBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Codex doesn't have separate agent types like Claude Code
        // Return a single general-purpose agent
        Ok(vec![AgentInfo {
            id: "default".to_string(),
            name: "Codex Agent".to_string(),
        }])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let client = CodexClient::new();
        Ok(Session {
            id: client.create_session_id(),
            directory: config.directory,
            model: config.model,
            agent: config.agent,
        })
    }

    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
        let config = self.config.read().await.clone();
        let client = CodexClient::with_config(config);
        let workspace_exec = self.workspace_exec.as_ref();

        let (mut codex_rx, codex_handle) = client
            .execute_message(
                &session.directory,
                message,
                session.model.as_deref(),
                Some(&session.id),
                session.agent.as_deref(),
                workspace_exec,
            )
            .await?;

        let (tx, rx) = mpsc::channel(256);
        let session_id = session.id.clone();

        // Spawn event conversion task
        let handle = tokio::spawn(async move {
            // Track last seen content for each item to avoid duplication on ItemUpdated
            let mut item_content_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            'outer: while let Some(event) = codex_rx.recv().await {
                let exec_events = convert_codex_event(event, &mut item_content_cache);

                for exec_event in exec_events {
                    if tx.send(exec_event).await.is_err() {
                        debug!("ExecutionEvent receiver dropped");
                        break 'outer;
                    }
                }
            }

            // Ensure MessageComplete is sent
            let _ = tx
                .send(ExecutionEvent::MessageComplete {
                    session_id: session_id.clone(),
                })
                .await;

            // Drop the codex handle to clean up
            drop(codex_handle);
        });

        Ok((rx, handle))
    }
}

/// Convert a Codex event to backend-agnostic ExecutionEvents.
/// The cache parameter tracks last seen content for each item to avoid duplication on ItemUpdated.
fn convert_codex_event(
    event: CodexEvent,
    item_content_cache: &mut std::collections::HashMap<String, String>,
) -> Vec<ExecutionEvent> {
    fn emit_text_delta(
        results: &mut Vec<ExecutionEvent>,
        item_content_cache: &mut std::collections::HashMap<String, String>,
        item_id: &str,
        text: &str,
    ) {
        let last_content = item_content_cache.get(item_id);
        let new_content = if let Some(last) = last_content {
            if text.starts_with(last) {
                text[last.len()..].to_string()
            } else {
                text.to_string()
            }
        } else {
            text.to_string()
        };

        if !new_content.is_empty() {
            results.push(ExecutionEvent::TextDelta {
                content: new_content,
            });
        }

        item_content_cache.insert(item_id.to_string(), text.to_string());
    }

    fn emit_thinking_if_changed(
        results: &mut Vec<ExecutionEvent>,
        item_content_cache: &mut std::collections::HashMap<String, String>,
        item_id: &str,
        text: &str,
    ) {
        if item_content_cache.get(item_id).map(|v| v.as_str()) == Some(text) {
            return;
        }

        results.push(ExecutionEvent::Thinking {
            content: text.to_string(),
        });
        item_content_cache.insert(item_id.to_string(), text.to_string());
    }

    fn mark_tool_call_emitted(
        item_content_cache: &mut std::collections::HashMap<String, String>,
        item_id: &str,
    ) -> bool {
        let key = format!("tool_call:{}", item_id);
        if item_content_cache.contains_key(&key) {
            true
        } else {
            item_content_cache.insert(key, "1".to_string());
            false
        }
    }

    let mut results = vec![];

    fn mcp_tool_name(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<String> {
        let server = data.get("server")?.as_str()?;
        let tool = data.get("tool")?.as_str()?;
        Some(format!("mcp__{}__{}", server, tool))
    }

    fn mcp_tool_args(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        data.get("arguments")
            .cloned()
            .or_else(|| data.get("args").cloned())
    }

    fn mcp_tool_result(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let result = data
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let error = data.get("error").cloned();
        let status = data.get("status").cloned();

        let has_error = error
            .as_ref()
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or_else(|| error.as_ref().is_some_and(|v| !v.is_null()));
        let has_status = status.as_ref().is_some_and(|v| !v.is_null());

        if has_error || has_status {
            Some(serde_json::json!({
                "result": result,
                "error": error,
                "status": status,
            }))
        } else if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    match event {
        CodexEvent::ThreadStarted { thread_id } => {
            debug!("Codex thread started: thread_id={}", thread_id);
        }

        CodexEvent::TurnStarted => {
            debug!("Codex turn started");
        }

        CodexEvent::TurnCompleted { summary } => {
            if let Some(summary_text) = summary {
                if !summary_text.trim().is_empty() {
                    results.push(ExecutionEvent::TurnSummary {
                        content: summary_text.clone(),
                    });
                }
                debug!("Codex turn completed: {}", summary_text);
            } else {
                debug!("Codex turn completed");
            }
        }

        CodexEvent::TurnFailed { error } => {
            results.push(ExecutionEvent::Error {
                message: error.message,
            });
        }

        CodexEvent::ItemCreated { item } | CodexEvent::ItemUpdated { item } => {
            // Handle different item types
            match item.item_type.as_str() {
                "message" | "agent_message" | "assistant_message" => {
                    // Extract message content
                    if let Some(text) = extract_text_field(&item.data) {
                        emit_text_delta(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                "reasoning" | "thinking" => {
                    // Extract thinking/reasoning content
                    if let Some(text) = extract_text_field(&item.data) {
                        emit_thinking_if_changed(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                "command" | "tool" => {
                    // Extract tool/command execution
                    if let Some(name) = item.data.get("name").and_then(|v| v.as_str()) {
                        if let Some(args) = item.data.get("args") {
                            results.push(ExecutionEvent::ToolCall {
                                id: item.id.clone(),
                                name: name.to_string(),
                                args: args.clone(),
                            });
                        }
                    }
                }
                "mcp_tool_call" => {
                    if let Some(name) = mcp_tool_name(&item.data) {
                        if let Some(args) = mcp_tool_args(&item.data) {
                            results.push(ExecutionEvent::ToolCall {
                                id: item.id.clone(),
                                name,
                                args,
                            });
                            mark_tool_call_emitted(item_content_cache, &item.id);
                        }
                    }
                }
                _ => {
                    debug!("Unknown Codex item type: {}", item.item_type);
                }
            }
        }

        CodexEvent::ItemCompleted { item } => {
            match item.item_type.as_str() {
                "command" | "tool" => {
                    // Extract tool result if available
                    if let Some(result) = item.data.get("result") {
                        if let Some(name) = item.data.get("name").and_then(|v| v.as_str()) {
                            results.push(ExecutionEvent::ToolResult {
                                id: item.id.clone(),
                                name: name.to_string(),
                                result: result.clone(),
                            });
                        }
                    }
                }
                "mcp_tool_call" => {
                    if let Some(name) = mcp_tool_name(&item.data) {
                        if let Some(args) = mcp_tool_args(&item.data) {
                            if !mark_tool_call_emitted(item_content_cache, &item.id) {
                                results.push(ExecutionEvent::ToolCall {
                                    id: item.id.clone(),
                                    name: name.clone(),
                                    args,
                                });
                            }
                        }
                        if let Some(result) = mcp_tool_result(&item.data) {
                            results.push(ExecutionEvent::ToolResult {
                                id: item.id.clone(),
                                name,
                                result,
                            });
                        }
                    }
                }
                "message" | "agent_message" | "assistant_message" => {
                    if let Some(text) = extract_text_field(&item.data) {
                        emit_text_delta(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                "reasoning" | "thinking" => {
                    if let Some(text) = extract_text_field(&item.data) {
                        emit_thinking_if_changed(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                _ => {}
            }
        }

        CodexEvent::Error { message } => {
            results.push(ExecutionEvent::Error { message });
        }

        CodexEvent::Unknown => {
            debug!("Unknown Codex event type");
        }
    }

    results
}

fn extract_text_field(
    data: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<String> {
    fn extract_str(value: Option<&serde_json::Value>) -> Option<String> {
        value
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .filter(|value| !value.is_empty())
    }

    fn extract_from_content(value: &serde_json::Value) -> Option<String> {
        let mut out = String::new();
        let items = value.as_array()?;
        for item in items {
            if let Some(text) = extract_str(item.get("text")) {
                out.push_str(&text);
                continue;
            }
            if let Some(text) = extract_str(item.get("content")) {
                out.push_str(&text);
                continue;
            }
            if let Some(text) = extract_str(item.get("output_text")) {
                out.push_str(&text);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    extract_str(data.get("text"))
        .or_else(|| extract_str(data.get("content")))
        .or_else(|| extract_str(data.get("output_text")))
        .or_else(|| data.get("content").and_then(extract_from_content))
}

/// Create a registry entry for the Codex backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(CodexBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_agents() {
        let backend = CodexBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "default");
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = CodexBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                agent: None,
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.directory, "/tmp");
    }
}
