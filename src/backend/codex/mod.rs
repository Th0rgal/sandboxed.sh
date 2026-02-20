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
    fn emit_text_snapshot(
        results: &mut Vec<ExecutionEvent>,
        item_content_cache: &mut std::collections::HashMap<String, String>,
        item_id: &str,
        text: &str,
    ) {
        // Codex message items can represent multiple assistant updates within one turn.
        // Emit the full per-item snapshot so the caller can treat each item as a standalone
        // assistant message and avoid concatenating progress updates into the final output.
        if !text.is_empty() && item_content_cache.get(item_id).map(|v| v.as_str()) != Some(text) {
            results.push(ExecutionEvent::TextDelta {
                content: text.to_string(),
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
                        emit_text_snapshot(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                "reasoning" | "thinking" => {
                    // Extract thinking/reasoning content
                    if let Some(text) = extract_text_field_with_reasoning(&item.data) {
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
                        emit_text_snapshot(&mut results, item_content_cache, &item.id, &text);
                    }
                }
                "reasoning" | "thinking" => {
                    if let Some(text) = extract_text_field_with_reasoning(&item.data) {
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
    extract_text_field_internal(data, false)
}

fn extract_text_field_with_reasoning(
    data: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<String> {
    extract_text_field_internal(data, true)
}

fn extract_text_field_internal(
    data: &std::collections::HashMap<String, serde_json::Value>,
    include_reasoning_blocks: bool,
) -> Option<String> {
    fn extract_str(value: Option<&serde_json::Value>) -> Option<String> {
        value
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .filter(|value| !value.is_empty())
    }

    fn extract_from_content(
        value: &serde_json::Value,
        include_reasoning_blocks: bool,
    ) -> Option<String> {
        let mut out = String::new();
        let items = value.as_array()?;
        for item in items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if !include_reasoning_blocks && matches!(item_type, "reasoning" | "thinking") {
                continue;
            }
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
        .or_else(|| {
            data.get("content")
                .and_then(|content| extract_from_content(content, include_reasoning_blocks))
        })
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

    // ---------------------------------------------------------------
    // Tests for convert_codex_event
    // ---------------------------------------------------------------

    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn convert_codex_event_thread_started_no_events() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "thread.started",
            "thread_id": "t1"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(events.is_empty(), "ThreadStarted should produce no events");
    }

    #[test]
    fn convert_codex_event_turn_started_no_events() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "turn.started"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(events.is_empty(), "TurnStarted should produce no events");
    }

    #[test]
    fn convert_codex_event_turn_completed_with_summary() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "turn.completed",
            "summary": "All tasks done"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::TurnSummary { content } => {
                assert_eq!(content, "All tasks done");
            }
            other => panic!("Expected TurnSummary, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_turn_completed_no_summary() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "turn.completed"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(events.is_empty(), "None summary should produce no events");
    }

    #[test]
    fn convert_codex_event_turn_completed_blank_summary() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "turn.completed",
            "summary": "   "
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(events.is_empty(), "Blank summary should produce no events");
    }

    #[test]
    fn convert_codex_event_turn_failed() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "turn.failed",
            "error": { "message": "something went wrong" }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Error { message } => {
                assert_eq!(message, "something went wrong");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_error() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "error",
            "message": "fatal error"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Error { message } => {
                assert_eq!(message, "fatal error");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_unknown_no_events() {
        // Use an unrecognized type string to trigger the Unknown variant
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "some.unknown.event"
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(events.is_empty(), "Unknown event should produce nothing");
    }

    #[test]
    fn convert_codex_event_item_created_message() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "msg1",
                "type": "message",
                "text": "Hello world"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::TextDelta { content } => {
                assert_eq!(content, "Hello world");
            }
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_created_thinking() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "think1",
                "type": "thinking",
                "text": "Let me consider..."
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Thinking { content } => {
                assert_eq!(content, "Let me consider...");
            }
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_created_tool_call() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "tc1",
                "type": "tool_call",
                "name": "read_file",
                "arguments": "{\"path\": \"/tmp/test.txt\"}"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "tc1");
                assert_eq!(name, "read_file");
                // arguments was a JSON string, should be parsed
                assert_eq!(args["path"], "/tmp/test.txt");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_created_command() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "cmd1",
                "type": "command",
                "name": "shell",
                "args": {"cmd": "ls -la"}
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "cmd1");
                assert_eq!(name, "shell");
                assert_eq!(args["cmd"], "ls -la");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_created_mcp_tool_call() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "mcp1",
                "type": "mcp_tool_call",
                "server": "my_server",
                "tool": "my_tool",
                "arguments": {"key": "value"}
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "mcp1");
                assert_eq!(name, "mcp__my_server__my_tool");
                assert_eq!(args["key"], "value");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_completed_message() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.completed",
            "item": {
                "id": "msg2",
                "type": "message",
                "text": "Final answer"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::TextDelta { content } => {
                assert_eq!(content, "Final answer");
            }
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_completed_tool_result() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.completed",
            "item": {
                "id": "tc2",
                "type": "tool_call",
                "name": "read_file",
                "result": "file contents here"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tc2");
                assert_eq!(name, "read_file");
                assert_eq!(result, "file contents here");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_completed_mcp_first_time() {
        // When an mcp_tool_call appears in ItemCompleted but was never emitted via
        // ItemCreated, both ToolCall and ToolResult should be produced.
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.completed",
            "item": {
                "id": "mcp2",
                "type": "mcp_tool_call",
                "server": "srv",
                "tool": "do_thing",
                "arguments": {"a": 1},
                "result": "ok"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 2);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "mcp2");
                assert_eq!(name, "mcp__srv__do_thing");
                assert_eq!(args["a"], 1);
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
        match &events[1] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "mcp2");
                assert_eq!(name, "mcp__srv__do_thing");
                assert_eq!(result, "ok");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_item_completed_mcp_already_created() {
        // Simulate that ItemCreated already emitted the ToolCall.
        // ItemCompleted should only emit ToolResult.
        let created_event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "mcp3",
                "type": "mcp_tool_call",
                "server": "srv",
                "tool": "do_thing",
                "arguments": {"a": 1}
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let _ = convert_codex_event(created_event, &mut cache);

        let completed_event: CodexEvent = serde_json::from_value(json!({
            "type": "item.completed",
            "item": {
                "id": "mcp3",
                "type": "mcp_tool_call",
                "server": "srv",
                "tool": "do_thing",
                "arguments": {"a": 1},
                "result": "done"
            }
        }))
        .unwrap();
        let events = convert_codex_event(completed_event, &mut cache);
        // Should only have ToolResult, no duplicate ToolCall
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "mcp3");
                assert_eq!(name, "mcp__srv__do_thing");
                assert_eq!(result, "done");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_text_snapshot_updates() {
        // Two ItemUpdated events where the second text extends the first.
        // The second call should produce the full updated snapshot.
        let event1: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "msg_dedup",
                "type": "message",
                "text": "Hello"
            }
        }))
        .unwrap();
        let event2: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "msg_dedup",
                "type": "message",
                "text": "Hello world"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events1 = convert_codex_event(event1, &mut cache);
        assert_eq!(events1.len(), 1);
        match &events1[0] {
            ExecutionEvent::TextDelta { content } => assert_eq!(content, "Hello"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }

        let events2 = convert_codex_event(event2, &mut cache);
        assert_eq!(events2.len(), 1);
        match &events2[0] {
            ExecutionEvent::TextDelta { content } => assert_eq!(content, "Hello world"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_text_snapshot_skips_unchanged() {
        let event1: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "msg_same",
                "type": "message",
                "text": "No change"
            }
        }))
        .unwrap();
        let event2: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "msg_same",
                "type": "message",
                "text": "No change"
            }
        }))
        .unwrap();

        let mut cache = HashMap::new();
        let events1 = convert_codex_event(event1, &mut cache);
        assert_eq!(events1.len(), 1);
        let events2 = convert_codex_event(event2, &mut cache);
        assert!(events2.is_empty());
    }

    #[test]
    fn convert_codex_event_thinking_dedup_skips_unchanged() {
        // Two ItemUpdated with "thinking" and the same text.
        // The second should produce nothing.
        let event1: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "think_dedup",
                "type": "thinking",
                "text": "same thought"
            }
        }))
        .unwrap();
        let event2: CodexEvent = serde_json::from_value(json!({
            "type": "item.updated",
            "item": {
                "id": "think_dedup",
                "type": "thinking",
                "text": "same thought"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events1 = convert_codex_event(event1, &mut cache);
        assert_eq!(events1.len(), 1);
        match &events1[0] {
            ExecutionEvent::Thinking { content } => assert_eq!(content, "same thought"),
            other => panic!("Expected Thinking, got {:?}", other),
        }

        let events2 = convert_codex_event(event2, &mut cache);
        assert!(
            events2.is_empty(),
            "Unchanged thinking text should produce no events"
        );
    }

    #[test]
    fn convert_codex_event_unknown_item_type() {
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "unk1",
                "type": "weird_type",
                "text": "data"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert!(
            events.is_empty(),
            "Unrecognized item type should produce no events"
        );
    }

    #[test]
    fn convert_codex_event_tool_name_fallback_to_tool_object() {
        // When "name" is not a top-level string but is nested under data["tool"]["name"]
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "tc_fallback",
                "type": "tool_call",
                "tool": {
                    "name": "nested_tool",
                    "arguments": "{\"x\": 42}"
                }
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "tc_fallback");
                assert_eq!(name, "nested_tool");
                // Args come from the nested tool object's arguments field (parsed from JSON string)
                assert_eq!(args["x"], 42);
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_tool_args_from_input_field() {
        // When args are provided via data["input"] instead of data["arguments"]
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.created",
            "item": {
                "id": "tc_input",
                "type": "tool_call",
                "name": "write_file",
                "input": {"path": "/tmp/out.txt", "content": "hello"}
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "tc_input");
                assert_eq!(name, "write_file");
                assert_eq!(args["path"], "/tmp/out.txt");
                assert_eq!(args["content"], "hello");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn convert_codex_event_tool_result_with_error() {
        // ItemCompleted tool_call with an error field should produce ToolResult containing the error
        let event: CodexEvent = serde_json::from_value(json!({
            "type": "item.completed",
            "item": {
                "id": "tc_err",
                "type": "tool_call",
                "name": "run_cmd",
                "result": null,
                "error": "command not found"
            }
        }))
        .unwrap();
        let mut cache = HashMap::new();
        let events = convert_codex_event(event, &mut cache);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tc_err");
                assert_eq!(name, "run_cmd");
                // The result should contain the error field
                assert_eq!(result["error"], "command not found");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    // ---------------------------------------------------------------
    // Tests for extract_text_field
    // ---------------------------------------------------------------

    #[test]
    fn extract_text_field_from_text_key() {
        let mut data = HashMap::new();
        data.insert(
            "text".to_string(),
            serde_json::Value::String("hello from text".to_string()),
        );
        let result = extract_text_field(&data);
        assert_eq!(result, Some("hello from text".to_string()));
    }

    #[test]
    fn extract_text_field_from_content_key() {
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            serde_json::Value::String("hello from content".to_string()),
        );
        let result = extract_text_field(&data);
        assert_eq!(result, Some("hello from content".to_string()));
    }

    #[test]
    fn extract_text_field_from_output_text_key() {
        let mut data = HashMap::new();
        data.insert(
            "output_text".to_string(),
            serde_json::Value::String("hello from output_text".to_string()),
        );
        let result = extract_text_field(&data);
        assert_eq!(result, Some("hello from output_text".to_string()));
    }

    #[test]
    fn extract_text_field_from_content_array() {
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([
                {"text": "part one"},
                {"text": " part two"}
            ]),
        );
        let result = extract_text_field(&data);
        assert_eq!(result, Some("part one part two".to_string()));
    }

    #[test]
    fn extract_text_field_from_content_array_skips_reasoning_blocks() {
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([
                {"type": "reasoning", "text": "thinking that should not leak"},
                {"type": "output_text", "text": "actual answer"}
            ]),
        );
        let result = extract_text_field(&data);
        assert_eq!(result, Some("actual answer".to_string()));
    }

    #[test]
    fn extract_text_field_with_reasoning_includes_reasoning_blocks() {
        let mut data = HashMap::new();
        data.insert(
            "content".to_string(),
            json!([
                {"type": "reasoning", "text": "thinking chunk"},
                {"type": "output_text", "text": "final text"}
            ]),
        );
        let result = extract_text_field_with_reasoning(&data);
        assert_eq!(result, Some("thinking chunkfinal text".to_string()));
    }
}
