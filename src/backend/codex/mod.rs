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
        if let std::collections::hash_map::Entry::Vacant(entry) = item_content_cache.entry(key) {
            entry.insert("1".to_string());
            false
        } else {
            true
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

    fn normalize_tool_result(
        result: serde_json::Value,
        error: Option<serde_json::Value>,
        status: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
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

    fn mcp_tool_result(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let result = data
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let error = data.get("error").cloned();
        let status = data.get("status").cloned();
        normalize_tool_result(result, error, status)
    }

    fn tool_name(data: &std::collections::HashMap<String, serde_json::Value>) -> Option<String> {
        fn name_from_object(value: &serde_json::Value) -> Option<String> {
            let obj = value.as_object()?;
            obj.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    obj.get("tool_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    obj.get("command")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
        }

        data.get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                data.get("tool")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                data.get("tool_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                data.get("command")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| data.get("tool").and_then(name_from_object))
            .or_else(|| data.get("function").and_then(name_from_object))
            .or_else(|| data.get("call").and_then(name_from_object))
            .or_else(|| data.get("tool_call").and_then(name_from_object))
            .or_else(|| data.get("function_call").and_then(name_from_object))
            .or_else(|| data.get("toolCall").and_then(name_from_object))
    }

    fn parse_json_str(value: &serde_json::Value) -> Option<serde_json::Value> {
        let s = value.as_str()?;
        if s.trim().is_empty() {
            return None;
        }
        serde_json::from_str::<serde_json::Value>(s).ok()
    }

    fn tool_args(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        fn args_from_object(value: &serde_json::Value) -> Option<serde_json::Value> {
            let obj = value.as_object()?;
            if let Some(value) = obj.get("args") {
                return Some(value.clone());
            }
            if let Some(value) = obj.get("arguments") {
                return parse_json_str(value).or_else(|| Some(value.clone()));
            }
            if let Some(value) = obj.get("input") {
                return Some(value.clone());
            }
            if let Some(value) = obj.get("params") {
                return Some(value.clone());
            }
            if let Some(value) = obj.get("payload") {
                return Some(value.clone());
            }
            None
        }

        if let Some(value) = data.get("args") {
            return Some(value.clone());
        }
        if let Some(value) = data.get("arguments") {
            return parse_json_str(value).or_else(|| Some(value.clone()));
        }
        if let Some(value) = data.get("input") {
            return Some(value.clone());
        }
        if let Some(value) = data.get("params") {
            return Some(value.clone());
        }
        if let Some(value) = data.get("payload") {
            return Some(value.clone());
        }
        data.get("tool")
            .and_then(args_from_object)
            .or_else(|| data.get("function").and_then(args_from_object))
            .or_else(|| data.get("call").and_then(args_from_object))
            .or_else(|| data.get("tool_call").and_then(args_from_object))
            .or_else(|| data.get("function_call").and_then(args_from_object))
            .or_else(|| data.get("toolCall").and_then(args_from_object))
    }

    fn tool_result(
        data: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        fn result_from_object(value: &serde_json::Value) -> Option<serde_json::Value> {
            let obj = value.as_object()?;
            obj.get("result")
                .or_else(|| obj.get("output"))
                .or_else(|| obj.get("response"))
                .or_else(|| obj.get("content"))
                .or_else(|| obj.get("data"))
                .cloned()
        }

        let result = data
            .get("result")
            .or_else(|| data.get("output"))
            .or_else(|| data.get("response"))
            .or_else(|| data.get("content"))
            .or_else(|| data.get("data"))
            .cloned()
            .or_else(|| data.get("tool").and_then(result_from_object))
            .or_else(|| data.get("function").and_then(result_from_object))
            .or_else(|| data.get("call").and_then(result_from_object))
            .or_else(|| data.get("tool_call").and_then(result_from_object))
            .or_else(|| data.get("function_call").and_then(result_from_object))
            .or_else(|| data.get("toolCall").and_then(result_from_object))
            .unwrap_or(serde_json::Value::Null);
        let error = data.get("error").cloned();
        let status = data.get("status").cloned();
        normalize_tool_result(result, error, status)
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
                "command" | "tool" | "tool_call" | "function_call" => {
                    // Extract tool/command execution
                    if let Some(name) = tool_name(&item.data) {
                        let args = tool_args(&item.data)
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                        results.push(ExecutionEvent::ToolCall {
                            id: item.id.clone(),
                            name,
                            args,
                        });
                    }
                }
                "mcp_tool_call" => {
                    if let Some(name) = mcp_tool_name(&item.data) {
                        let args = mcp_tool_args(&item.data)
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                        results.push(ExecutionEvent::ToolCall {
                            id: item.id.clone(),
                            name,
                            args,
                        });
                        mark_tool_call_emitted(item_content_cache, &item.id);
                    }
                }
                _ => {
                    debug!("Unknown Codex item type: {}", item.item_type);
                }
            }
        }

        CodexEvent::ItemCompleted { item } => {
            match item.item_type.as_str() {
                "command" | "tool" | "tool_call" | "function_call" | "tool_result"
                | "function_result" => {
                    // Extract tool result - always emit event even for null results
                    // to prevent pending_tools leak in mission_runner
                    if let Some(name) = tool_name(&item.data) {
                        let result = tool_result(&item.data).unwrap_or(serde_json::Value::Null);
                        results.push(ExecutionEvent::ToolResult {
                            id: item.id.clone(),
                            name,
                            result,
                        });
                    }
                }
                "mcp_tool_call" => {
                    if let Some(name) = mcp_tool_name(&item.data) {
                        let args = mcp_tool_args(&item.data)
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                        if !mark_tool_call_emitted(item_content_cache, &item.id) {
                            results.push(ExecutionEvent::ToolCall {
                                id: item.id.clone(),
                                name: name.clone(),
                                args,
                            });
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

    // ── convert_codex_event tests ─────────────────────────────────────

    use client::{CodexEvent, ErrorInfo, Item};
    use serde_json::json;
    use std::collections::HashMap;

    fn make_item(id: &str, item_type: &str, data: serde_json::Value) -> Item {
        let mut map: HashMap<String, serde_json::Value> = if let Some(obj) = data.as_object() {
            obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        } else {
            HashMap::new()
        };
        // Ensure id and type are not duplicated in data
        map.remove("id");
        map.remove("type");
        Item {
            id: id.to_string(),
            item_type: item_type.to_string(),
            data: map,
        }
    }

    #[test]
    fn convert_codex_event_thread_started_no_events() {
        let event = CodexEvent::ThreadStarted {
            thread_id: "t1".to_string(),
        };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_turn_started_no_events() {
        let event = CodexEvent::TurnStarted;
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_turn_completed_with_summary() {
        let event = CodexEvent::TurnCompleted {
            summary: Some("Task done".to_string()),
        };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::TurnSummary { content } if content == "Task done")
        );
    }

    #[test]
    fn convert_codex_event_turn_completed_no_summary() {
        let event = CodexEvent::TurnCompleted { summary: None };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_turn_completed_blank_summary() {
        let event = CodexEvent::TurnCompleted {
            summary: Some("   ".to_string()),
        };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_turn_failed() {
        let event = CodexEvent::TurnFailed {
            error: ErrorInfo {
                message: "timeout".to_string(),
            },
        };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::Error { message } if message == "timeout"));
    }

    #[test]
    fn convert_codex_event_error() {
        let event = CodexEvent::Error {
            message: "rate limited".to_string(),
        };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Error { message } if message == "rate limited")
        );
    }

    #[test]
    fn convert_codex_event_unknown_no_events() {
        let event = CodexEvent::Unknown;
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_item_created_message() {
        let item = make_item("msg1", "message", json!({"text": "Hello world"}));
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::TextDelta { content } if content == "Hello world")
        );
    }

    #[test]
    fn convert_codex_event_item_created_thinking() {
        let item = make_item("t1", "reasoning", json!({"text": "Let me think..."}));
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Thinking { content } if content == "Let me think...")
        );
    }

    #[test]
    fn convert_codex_event_item_created_tool_call() {
        let item = make_item(
            "tc1",
            "tool_call",
            json!({"name": "bash", "arguments": "{\"cmd\": \"ls\"}"}),
        );
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::ToolCall { id, name, .. }
            if id == "tc1" && name == "bash")
        );
    }

    #[test]
    fn convert_codex_event_item_created_command() {
        let item = make_item(
            "cmd1",
            "command",
            json!({"command": "grep", "args": {"pattern": "foo"}}),
        );
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::ToolCall { name, .. }
            if name == "grep"));
    }

    #[test]
    fn convert_codex_event_item_created_mcp_tool_call() {
        let item = make_item(
            "mcp1",
            "mcp_tool_call",
            json!({"server": "myserver", "tool": "query", "arguments": {"q": "test"}}),
        );
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::ToolCall { name, .. }
            if name == "mcp__myserver__query"));
    }

    #[test]
    fn convert_codex_event_item_completed_tool_result() {
        let item = make_item(
            "tc1",
            "tool_call",
            json!({"name": "bash", "result": "file1.txt\nfile2.txt"}),
        );
        let event = CodexEvent::ItemCompleted { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::ToolResult { id, name, .. }
            if id == "tc1" && name == "bash")
        );
    }

    #[test]
    fn convert_codex_event_item_completed_mcp_first_time() {
        // When mcp_tool_call is completed but was NOT previously created,
        // it should emit both ToolCall and ToolResult
        let item = make_item(
            "mcp1",
            "mcp_tool_call",
            json!({"server": "s", "tool": "t", "arguments": {}, "result": "ok"}),
        );
        let event = CodexEvent::ItemCompleted { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 2);
        assert!(matches!(&results[0], ExecutionEvent::ToolCall { .. }));
        assert!(matches!(&results[1], ExecutionEvent::ToolResult { .. }));
    }

    #[test]
    fn convert_codex_event_item_completed_mcp_already_created() {
        // When mcp_tool_call was already created (mark_tool_call_emitted),
        // it should only emit ToolResult on completion
        let item_created = make_item(
            "mcp1",
            "mcp_tool_call",
            json!({"server": "s", "tool": "t", "arguments": {}}),
        );
        let item_completed = make_item(
            "mcp1",
            "mcp_tool_call",
            json!({"server": "s", "tool": "t", "arguments": {}, "result": "done"}),
        );
        let mut cache = HashMap::new();
        // First, create the item
        convert_codex_event(CodexEvent::ItemCreated { item: item_created }, &mut cache);
        // Now complete it — should only get ToolResult
        let results = convert_codex_event(
            CodexEvent::ItemCompleted {
                item: item_completed,
            },
            &mut cache,
        );
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::ToolResult { .. }));
    }

    #[test]
    fn convert_codex_event_text_delta_deduplication() {
        // When ItemUpdated is received with growing text, only the new delta is emitted
        let mut cache = HashMap::new();

        let item1 = make_item("msg1", "message", json!({"text": "Hello"}));
        let results1 = convert_codex_event(CodexEvent::ItemCreated { item: item1 }, &mut cache);
        assert_eq!(results1.len(), 1);
        assert!(
            matches!(&results1[0], ExecutionEvent::TextDelta { content } if content == "Hello")
        );

        let item2 = make_item("msg1", "message", json!({"text": "Hello world"}));
        let results2 = convert_codex_event(CodexEvent::ItemUpdated { item: item2 }, &mut cache);
        assert_eq!(results2.len(), 1);
        // Only the new " world" part should be emitted
        assert!(
            matches!(&results2[0], ExecutionEvent::TextDelta { content } if content == " world")
        );
    }

    #[test]
    fn convert_codex_event_thinking_dedup_skips_unchanged() {
        let mut cache = HashMap::new();

        let item1 = make_item("r1", "thinking", json!({"text": "analyzing"}));
        let results1 = convert_codex_event(CodexEvent::ItemCreated { item: item1 }, &mut cache);
        assert_eq!(results1.len(), 1);

        // Same text — should be skipped
        let item2 = make_item("r1", "thinking", json!({"text": "analyzing"}));
        let results2 = convert_codex_event(CodexEvent::ItemUpdated { item: item2 }, &mut cache);
        assert!(results2.is_empty());
    }

    #[test]
    fn convert_codex_event_item_completed_message() {
        // ItemCompleted for a message should also emit remaining text delta
        let mut cache = HashMap::new();

        let item1 = make_item("msg1", "message", json!({"text": "Part 1"}));
        convert_codex_event(CodexEvent::ItemCreated { item: item1 }, &mut cache);

        let item2 = make_item("msg1", "message", json!({"text": "Part 1 and Part 2"}));
        let results = convert_codex_event(CodexEvent::ItemCompleted { item: item2 }, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::TextDelta { content } if content == " and Part 2")
        );
    }

    #[test]
    fn convert_codex_event_tool_result_with_error() {
        let item = make_item(
            "tc1",
            "tool_call",
            json!({"name": "bash", "result": "", "error": "command not found"}),
        );
        let event = CodexEvent::ItemCompleted { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        if let ExecutionEvent::ToolResult { result, .. } = &results[0] {
            // With error present, result should be a structured object
            assert!(result.get("error").is_some());
        } else {
            panic!("Expected ToolResult");
        }
    }

    #[test]
    fn convert_codex_event_unknown_item_type() {
        let item = make_item("x1", "unknown_type", json!({"text": "something"}));
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_codex_event_tool_name_fallback_to_tool_object() {
        let item = make_item(
            "tc1",
            "tool_call",
            json!({"tool": {"name": "read_file", "args": {"path": "/tmp"}}}),
        );
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::ToolCall { name, .. }
            if name == "read_file"));
    }

    #[test]
    fn convert_codex_event_tool_args_from_input_field() {
        let item = make_item(
            "tc1",
            "tool_call",
            json!({"name": "edit", "input": {"file": "main.rs", "content": "code"}}),
        );
        let event = CodexEvent::ItemCreated { item };
        let mut cache = HashMap::new();
        let results = convert_codex_event(event, &mut cache);
        assert_eq!(results.len(), 1);
        if let ExecutionEvent::ToolCall { args, .. } = &results[0] {
            assert_eq!(args.get("file").and_then(|v| v.as_str()), Some("main.rs"));
        } else {
            panic!("Expected ToolCall");
        }
    }
}
