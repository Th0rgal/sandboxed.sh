//! Types and conversion logic for the Claude Code CLI NDJSON streaming protocol.
//!
//! This module defines the event shape consumed by Claude Code-compatible
//! streaming integrations.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::events::ExecutionEvent;

// ── Process handle ────────────────────────────────────────────────

/// Handle to a running Claude Code CLI process.
/// Call `kill()` to terminate the process when cancelling a mission.
pub struct ProcessHandle {
    child: Arc<Mutex<Option<Child>>>,
    _task_handle: JoinHandle<()>,
    /// Keep stdin alive to prevent process from exiting prematurely
    _stdin: Option<ChildStdin>,
}

impl ProcessHandle {
    pub fn new(child: Arc<Mutex<Option<Child>>>, task_handle: JoinHandle<()>) -> Self {
        Self {
            child,
            _task_handle: task_handle,
            _stdin: None,
        }
    }

    pub fn new_with_stdin(
        child: Arc<Mutex<Option<Child>>>,
        task_handle: JoinHandle<()>,
        stdin: ChildStdin,
    ) -> Self {
        Self {
            child,
            _task_handle: task_handle,
            _stdin: Some(stdin),
        }
    }

    /// Get a clone of the child process Arc for external kill handling.
    pub fn child_arc(&self) -> Arc<Mutex<Option<Child>>> {
        Arc::clone(&self.child)
    }

    /// Kill the underlying CLI process.
    pub async fn kill(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            if let Err(e) = child.kill().await {
                warn!("Failed to kill CLI process: {}", e);
            } else {
                info!("CLI process killed");
            }
        }
    }
}

// ── NDJSON event types ────────────────────────────────────────────

/// Events emitted by the Claude Code CLI in stream-json mode.
///
/// The `Unknown` variant acts as a forward-compatibility catch-all: if a future
/// CLI version introduces a new event type, it will be deserialized as `Unknown`
/// instead of causing a parse error.  This prevents startup timeouts caused by
/// unrecognized event types being silently discarded (logged as warnings) before
/// any known event arrives.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum CliEvent {
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "stream_event")]
    StreamEvent(StreamEventWrapper),
    #[serde(rename = "assistant")]
    Assistant(AssistantEvent),
    #[serde(rename = "user")]
    User(UserEvent),
    #[serde(rename = "result")]
    Result(ResultEvent),
    /// Catch-all for unrecognized event types from newer CLI versions.
    #[serde(other)]
    Unknown,
}

/// MCP server status in the init event.
/// Claude Code 2.1+ returns objects with name/status, older versions return strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum McpServerInfo {
    /// New format: object with name and status
    Object { name: String, status: String },
    /// Legacy format: just the server name as a string
    String(String),
}

impl McpServerInfo {
    pub fn name(&self) -> &str {
        match self {
            McpServerInfo::Object { name, .. } => name,
            McpServerInfo::String(s) => s,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// MCP servers configured for this session.
    /// Claude Code 2.1+ returns objects with {name, status}, older versions return strings.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamEventWrapper {
    pub event: StreamEvent,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: Value },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockInfo,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: Delta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: Value, usage: Option<Value> },
    #[serde(rename = "message_stop")]
    MessageStop,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockInfo {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
    /// Thinking content for thinking_delta events (extended thinking).
    #[serde(default)]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        /// Content can be a string (text result) or an array (e.g., image results).
        content: ToolResultContent,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
}

/// Tool result content — either a simple string or structured content (array with images/text).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Simple text content
    Text(String),
    /// Structured content (e.g., array of image/text blocks)
    Structured(Vec<Value>),
}

impl ToolResultContent {
    /// Convert to a string representation for storage/display.
    /// For structured content (images), returns a JSON string or placeholder.
    pub fn to_string_lossy(&self) -> String {
        match self {
            ToolResultContent::Text(s) => s.clone(),
            ToolResultContent::Structured(items) => {
                let mut parts = Vec::new();
                for item in items {
                    if let Some(obj) = item.as_object() {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("image") {
                            parts.push("[image]".to_string());
                        } else if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            parts.push(text.to_string());
                        }
                    }
                }
                if parts.is_empty() {
                    serde_json::to_string(items)
                        .unwrap_or_else(|_| "[structured content]".to_string())
                } else {
                    parts.join("\n")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEvent {
    pub message: UserMessage,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResultInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub role: Option<String>,
}

/// Tool use result info — can be a structured object or a simple string (error message).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResultInfo {
    /// Structured result with stdout/stderr/etc
    Structured {
        #[serde(default)]
        stdout: Option<String>,
        #[serde(default)]
        stderr: Option<String>,
        #[serde(default)]
        interrupted: Option<bool>,
        #[serde(default, rename = "isImage")]
        is_image: Option<bool>,
    },
    /// Simple string result (often an error message)
    Text(String),
    /// Fallback for newer/unknown shapes (e.g. tool_result content blocks)
    Raw(serde_json::Value),
}

impl ToolUseResultInfo {
    pub fn stdout(&self) -> Option<&str> {
        match self {
            ToolUseResultInfo::Structured { stdout, .. } => stdout.as_deref(),
            ToolUseResultInfo::Text(_) => None,
            ToolUseResultInfo::Raw(_) => None,
        }
    }

    pub fn stderr(&self) -> Option<&str> {
        match self {
            ToolUseResultInfo::Structured { stderr, .. } => stderr.as_deref(),
            ToolUseResultInfo::Text(s) => Some(s.as_str()),
            ToolUseResultInfo::Raw(_) => None,
        }
    }

    pub fn interrupted(&self) -> Option<bool> {
        match self {
            ToolUseResultInfo::Structured { interrupted, .. } => *interrupted,
            ToolUseResultInfo::Text(_) => None,
            ToolUseResultInfo::Raw(_) => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub session_id: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u32>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Claude Code puts errors in an array field.
    #[serde(default)]
    pub errors: Vec<String>,
}

impl ResultEvent {
    /// Extract the best available error/result message.
    /// Checks `result`, `error`, and `message` fields in order.
    /// Parses embedded JSON error format (e.g. `402 {"type":"error",...}`)
    /// to extract a human-readable message.
    pub fn error_message(&self) -> String {
        // Extract from `errors` array (Claude Code puts session errors here).
        // Used as a last-resort fallback after `result`, `error`, and `message`.
        let from_errors = self
            .errors
            .first()
            .filter(|s| !s.is_empty())
            .map(|s| s.as_str());

        let raw = self
            .result
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.error.as_deref().filter(|s| !s.is_empty()))
            .or(self.message.as_deref().filter(|s| !s.is_empty()))
            .or(from_errors)
            .unwrap_or("Unknown error");

        Self::parse_error_json(raw).unwrap_or_else(|| raw.to_string())
    }

    /// Parse CLI error strings that may contain embedded JSON.
    fn parse_error_json(raw: &str) -> Option<String> {
        let json_str = raw.find('{').map(|idx| &raw[idx..]).unwrap_or(raw);
        let parsed: Value = serde_json::from_str(json_str).ok()?;
        parsed
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| parsed.get("message").and_then(|m| m.as_str()))
            .map(|s| s.to_string())
    }
}

// ── Event conversion ──────────────────────────────────────────────

/// Convert a CLI event to backend-agnostic ExecutionEvents.
pub fn convert_cli_event(
    event: CliEvent,
    pending_tools: &mut HashMap<String, String>,
) -> Vec<ExecutionEvent> {
    let mut results = vec![];

    match event {
        CliEvent::System(sys) => {
            debug!(
                "CLI session initialized: session_id={}, model={:?}",
                sys.session_id, sys.model
            );
        }

        CliEvent::StreamEvent(wrapper) => match wrapper.event {
            StreamEvent::ContentBlockDelta { delta, .. } => {
                if let Some(text) = delta.text {
                    if !text.is_empty() {
                        results.push(ExecutionEvent::TextDelta { content: text });
                    }
                }
                if let Some(thinking) = delta.thinking {
                    if !thinking.is_empty() {
                        results.push(ExecutionEvent::Thinking { content: thinking });
                    }
                }
                if let Some(partial) = delta.partial_json {
                    debug!("Tool input delta: {}", partial);
                }
            }
            StreamEvent::ContentBlockStart { content_block, .. }
                if content_block.block_type == "tool_use" =>
            {
                if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                    pending_tools.insert(id, name);
                }
            }
            _ => {}
        },

        CliEvent::Assistant(evt) => {
            for block in evt.message.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text.is_empty() {
                            results.push(ExecutionEvent::Thinking { content: text });
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        pending_tools.insert(id.clone(), name.clone());
                        results.push(ExecutionEvent::ToolCall {
                            id,
                            name,
                            args: input,
                        });
                    }
                    ContentBlock::Thinking { thinking } => {
                        if !thinking.is_empty() {
                            results.push(ExecutionEvent::Thinking { content: thinking });
                        }
                    }
                    ContentBlock::ToolResult { .. } | ContentBlock::RedactedThinking { .. } => {}
                }
            }
        }

        CliEvent::User(evt) => {
            for block in evt.message.content {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = block
                {
                    let name = pending_tools
                        .get(&tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());

                    let content_str = content.to_string_lossy();

                    let result_value = if let Some(ref extra) = evt.tool_use_result {
                        serde_json::json!({
                            "content": content_str,
                            "stdout": extra.stdout(),
                            "stderr": extra.stderr(),
                            "is_error": is_error,
                            "interrupted": extra.interrupted(),
                        })
                    } else {
                        Value::String(content_str)
                    };

                    results.push(ExecutionEvent::ToolResult {
                        id: tool_use_id,
                        name,
                        result: result_value,
                    });
                }
            }
        }

        CliEvent::Result(res) => {
            // Check for errors: explicit error flags OR result text that looks like an API error
            let result_text = res.result.as_deref().unwrap_or("");
            let looks_like_api_error = result_text.starts_with("API Error:")
                || result_text.contains("\"type\":\"error\"")
                || result_text.contains("\"type\":\"overloaded_error\"")
                || result_text.contains("\"type\":\"api_error\"");

            if res.is_error || res.subtype == "error" || looks_like_api_error {
                results.push(ExecutionEvent::Error {
                    message: res.error_message(),
                });
            } else {
                debug!(
                    "CLI result: subtype={}, cost={:?}, duration={:?}ms, turns={:?}",
                    res.subtype, res.total_cost_usd, res.duration_ms, res.num_turns
                );
            }
        }

        CliEvent::Unknown => {
            // Forward-compatibility: silently ignore unrecognized event types.
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    // ── ToolResultContent::to_string_lossy ─────────────────────────

    #[test]
    fn tool_result_content_text_returns_string() {
        let content = ToolResultContent::Text("hello world".to_string());
        assert_eq!(content.to_string_lossy(), "hello world");
    }

    #[test]
    fn tool_result_content_structured_text_items_joined() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "text", "text": "line one"}),
            json!({"type": "text", "text": "line two"}),
        ]);
        assert_eq!(content.to_string_lossy(), "line one\nline two");
    }

    #[test]
    fn tool_result_content_structured_image_returns_placeholder() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "image", "source": {"data": "base64..."}}),
        ]);
        assert_eq!(content.to_string_lossy(), "[image]");
    }

    #[test]
    fn tool_result_content_structured_mixed() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "text", "text": "before image"}),
            json!({"type": "image", "source": {"data": "abc"}}),
            json!({"type": "text", "text": "after image"}),
        ]);
        assert_eq!(
            content.to_string_lossy(),
            "before image\n[image]\nafter image"
        );
    }

    #[test]
    fn tool_result_content_structured_empty_vec_falls_back_to_json() {
        let content = ToolResultContent::Structured(vec![]);
        // Empty vec has no text/image items, so parts is empty -> JSON serialization
        assert_eq!(content.to_string_lossy(), "[]");
    }

    // ── ToolUseResultInfo accessors ────────────────────────────────

    #[test]
    fn tool_use_result_info_structured_accessors() {
        let info = ToolUseResultInfo::Structured {
            stdout: Some("out".to_string()),
            stderr: Some("err".to_string()),
            interrupted: Some(true),
            is_image: None,
        };
        assert_eq!(info.stdout(), Some("out"));
        assert_eq!(info.stderr(), Some("err"));
        assert_eq!(info.interrupted(), Some(true));
    }

    #[test]
    fn tool_use_result_info_text_variant() {
        let info = ToolUseResultInfo::Text("error msg".to_string());
        assert_eq!(info.stdout(), None);
        assert_eq!(info.stderr(), Some("error msg"));
        assert_eq!(info.interrupted(), None);
    }

    #[test]
    fn tool_use_result_info_raw_variant() {
        let info = ToolUseResultInfo::Raw(json!({"something": "else"}));
        assert_eq!(info.stdout(), None);
        assert_eq!(info.stderr(), None);
        assert_eq!(info.interrupted(), None);
    }

    // ── McpServerInfo::name ────────────────────────────────────────

    #[test]
    fn mcp_server_info_object_returns_name() {
        let info = McpServerInfo::Object {
            name: "my-server".to_string(),
            status: "running".to_string(),
        };
        assert_eq!(info.name(), "my-server");
    }

    #[test]
    fn mcp_server_info_string_returns_string() {
        let info = McpServerInfo::String("legacy-server".to_string());
        assert_eq!(info.name(), "legacy-server");
    }

    // ── ResultEvent::error_message ─────────────────────────────────

    fn make_result_event(
        result: Option<&str>,
        error: Option<&str>,
        message: Option<&str>,
        errors: Vec<&str>,
    ) -> ResultEvent {
        ResultEvent {
            subtype: "success".to_string(),
            session_id: "s1".to_string(),
            result: result.map(|s| s.to_string()),
            is_error: false,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: error.map(|s| s.to_string()),
            message: message.map(|s| s.to_string()),
            errors: errors.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn error_message_prefers_result_field() {
        let evt = make_result_event(Some("result text"), Some("error text"), Some("msg"), vec![]);
        assert_eq!(evt.error_message(), "result text");
    }

    #[test]
    fn error_message_falls_back_to_error_field() {
        let evt = make_result_event(None, Some("error text"), Some("msg"), vec![]);
        assert_eq!(evt.error_message(), "error text");
    }

    #[test]
    fn error_message_falls_back_to_message_field() {
        let evt = make_result_event(None, None, Some("msg text"), vec![]);
        assert_eq!(evt.error_message(), "msg text");
    }

    #[test]
    fn error_message_falls_back_to_errors_array() {
        let evt = make_result_event(None, None, None, vec!["first error", "second error"]);
        assert_eq!(evt.error_message(), "first error");
    }

    #[test]
    fn error_message_returns_unknown_when_all_empty() {
        let evt = make_result_event(None, None, None, vec![]);
        assert_eq!(evt.error_message(), "Unknown error");
    }

    #[test]
    fn error_message_parses_embedded_json() {
        let evt = make_result_event(
            Some(r#"402 {"error":{"message":"Payment required"}}"#),
            None,
            None,
            vec![],
        );
        assert_eq!(evt.error_message(), "Payment required");
    }

    #[test]
    fn error_message_skips_empty_strings_in_priority() {
        // result is empty string -> skip; error is empty -> skip; message has content
        let evt = make_result_event(Some(""), Some(""), Some("fallback msg"), vec![]);
        assert_eq!(evt.error_message(), "fallback msg");
    }

    // ── convert_cli_event ──────────────────────────────────────────

    #[test]
    fn convert_system_event_produces_nothing() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "system",
            "subtype": "init",
            "session_id": "s1",
            "tools": [],
            "mcp_servers": []
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_stream_content_block_delta_text_produces_text_delta() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": "hello"
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::TextDelta { content } => assert_eq!(content, "hello"),
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn convert_stream_content_block_delta_thinking_produces_thinking() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": "I need to think"
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Thinking { content } => assert_eq!(content, "I need to think"),
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn convert_stream_content_block_delta_empty_text_produces_nothing() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": ""
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_stream_content_block_start_tool_use_tracks_pending() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "tu_123",
                    "name": "Bash"
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
        assert_eq!(pending.get("tu_123").unwrap(), "Bash");
    }

    #[test]
    fn convert_assistant_text_produces_thinking() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "assistant",
            "session_id": "s1",
            "message": {
                "content": [
                    {"type": "text", "text": "I will run a command"}
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Thinking { content } => {
                assert_eq!(content, "I will run a command")
            }
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn convert_assistant_tool_use_produces_tool_call_and_tracks_pending() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "assistant",
            "session_id": "s1",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tu_456",
                        "name": "Read",
                        "input": {"path": "/tmp/foo"}
                    }
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "tu_456");
                assert_eq!(name, "Read");
                assert_eq!(args, &json!({"path": "/tmp/foo"}));
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
        assert_eq!(pending.get("tu_456").unwrap(), "Read");
    }

    #[test]
    fn convert_assistant_thinking_produces_thinking() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "assistant",
            "session_id": "s1",
            "message": {
                "content": [
                    {"type": "thinking", "thinking": "deep thought"}
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Thinking { content } => assert_eq!(content, "deep thought"),
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn convert_user_tool_result_looks_up_pending_tool_name() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "user",
            "session_id": "s1",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tu_789",
                        "content": "file contents here",
                        "is_error": false
                    }
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        pending.insert("tu_789".to_string(), "Read".to_string());
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tu_789");
                assert_eq!(name, "Read");
                assert_eq!(result, &json!("file contents here"));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_user_tool_result_unknown_tool_use_id() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "user",
            "session_id": "s1",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tu_unknown",
                        "content": "some output",
                        "is_error": false
                    }
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::ToolResult { name, .. } => assert_eq!(name, "unknown"),
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_user_tool_result_with_tool_use_result_builds_json() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "user",
            "session_id": "s1",
            "tool_use_result": {
                "stdout": "standard out",
                "stderr": "standard err",
                "interrupted": false
            },
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "tu_900",
                        "content": "command output",
                        "is_error": false
                    }
                ]
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        pending.insert("tu_900".to_string(), "Bash".to_string());
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tu_900");
                assert_eq!(name, "Bash");
                assert_eq!(result["content"], "command output");
                assert_eq!(result["stdout"], "standard out");
                assert_eq!(result["stderr"], "standard err");
                assert_eq!(result["is_error"], false);
                assert_eq!(result["interrupted"], false);
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_result_with_is_error_produces_error() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "success",
            "session_id": "s1",
            "is_error": true,
            "result": "Something went wrong"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Error { message } => assert_eq!(message, "Something went wrong"),
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_result_with_error_subtype_produces_error() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "error",
            "session_id": "s1",
            "is_error": false,
            "error": "bad things"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Error { message } => assert_eq!(message, "bad things"),
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_result_with_api_error_in_result_text() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "success",
            "session_id": "s1",
            "is_error": false,
            "result": "API Error: rate limited"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Error { message } => assert_eq!(message, "API Error: rate limited"),
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_result_with_overloaded_error() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "success",
            "session_id": "s1",
            "is_error": false,
            "result": "{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Error { message } => assert_eq!(message, "overloaded"),
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_successful_result_produces_no_events() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "success",
            "session_id": "s1",
            "is_error": false,
            "result": "All done"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn text_delta_does_not_contain_thinking_content() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "content_block_delta",
                    "text": "Hello world"
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::TextDelta { content } => {
                assert_eq!(content, "Hello world");
            }
            ExecutionEvent::Thinking { .. } => {
                panic!("Thinking content should not appear in TextDelta event");
            }
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn thinking_produces_thinking_event() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": "Let me think about this..."
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Thinking { content } => {
                assert_eq!(content, "Let me think about this...");
            }
            ExecutionEvent::TextDelta { .. } => {
                panic!("Thinking content should produce Thinking event, not TextDelta");
            }
            other => panic!("Expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn tool_call_stored_for_result_correlation() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "s1",
            "event": {
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "tool_123",
                    "name": "Bash"
                }
            }
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
        assert_eq!(pending.get("tool_123"), Some(&"Bash".to_string()));
    }

    #[test]
    fn error_result_preserves_message() {
        let event: CliEvent = serde_json::from_value(json!({
            "type": "result",
            "subtype": "error",
            "session_id": "s1",
            "is_error": true,
            "error": "Rate limit exceeded"
        }))
        .unwrap();
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ExecutionEvent::Error { message } => {
                assert_eq!(message, "Rate limit exceeded");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }
}
