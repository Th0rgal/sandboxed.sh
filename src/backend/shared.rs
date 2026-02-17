//! Types and conversion logic shared between Claude Code and Amp backends.
//!
//! Both CLIs use the same NDJSON streaming protocol. Amp extends it with a few
//! extra fields (`mcp_servers`, `usage`, `RedactedThinking`, error helpers).
//! This module defines the superset type that deserializes events from either
//! backend.

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

/// Handle to a running CLI process (Claude Code or Amp).
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

/// Events emitted by Claude Code / Amp CLIs in stream-json mode.
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
    /// Amp extension.
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
    /// Amp extension.
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
    /// Amp extension: separate error field.
    #[serde(default)]
    pub error: Option<String>,
    /// Amp extension: additional error context.
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

/// Convert a CLI event (Claude Code or Amp) to backend-agnostic ExecutionEvents.
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
            StreamEvent::ContentBlockStart { content_block, .. } => {
                if content_block.block_type == "tool_use" {
                    if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                        pending_tools.insert(id, name);
                    }
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
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── ToolResultContent::to_string_lossy tests ──────────────────────

    #[test]
    fn tool_result_content_text_returns_string() {
        let content = ToolResultContent::Text("hello world".to_string());
        assert_eq!(content.to_string_lossy(), "hello world");
    }

    #[test]
    fn tool_result_content_structured_image_placeholder() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "image", "source": {"data": "base64..."}}),
        ]);
        assert_eq!(content.to_string_lossy(), "[image]");
    }

    #[test]
    fn tool_result_content_structured_text_extracted() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "text", "text": "file contents here"}),
        ]);
        assert_eq!(content.to_string_lossy(), "file contents here");
    }

    #[test]
    fn tool_result_content_structured_mixed() {
        let content = ToolResultContent::Structured(vec![
            json!({"type": "text", "text": "before"}),
            json!({"type": "image", "source": {}}),
            json!({"type": "text", "text": "after"}),
        ]);
        assert_eq!(content.to_string_lossy(), "before\n[image]\nafter");
    }

    #[test]
    fn tool_result_content_structured_empty_falls_back_to_json() {
        let content = ToolResultContent::Structured(vec![json!(42)]);
        // No recognized objects, falls back to JSON serialization
        let result = content.to_string_lossy();
        assert!(result.contains("42"));
    }

    // ── ToolUseResultInfo method tests ────────────────────────────────

    #[test]
    fn tool_use_result_info_structured_stdout() {
        let info: ToolUseResultInfo = serde_json::from_value(json!({
            "stdout": "output here",
            "stderr": "err here",
            "interrupted": false
        }))
        .unwrap();
        assert_eq!(info.stdout(), Some("output here"));
        assert_eq!(info.stderr(), Some("err here"));
        assert_eq!(info.interrupted(), Some(false));
    }

    #[test]
    fn tool_use_result_info_text_stderr() {
        let info: ToolUseResultInfo = serde_json::from_value(json!("error message")).unwrap();
        assert_eq!(info.stdout(), None);
        assert_eq!(info.stderr(), Some("error message"));
        assert_eq!(info.interrupted(), None);
    }

    #[test]
    fn tool_use_result_info_raw_returns_none() {
        // Use a JSON array — objects match Structured (all fields are #[serde(default)]),
        // strings match Text, so only non-object non-string values reach Raw.
        let info: ToolUseResultInfo =
            serde_json::from_value(json!([{"type": "text", "text": "hello"}])).unwrap();
        assert_eq!(info.stdout(), None);
        assert_eq!(info.stderr(), None);
        assert_eq!(info.interrupted(), None);
    }

    #[test]
    fn tool_use_result_info_object_matches_structured_not_raw() {
        // Verify that unknown-field objects still match Structured (not Raw)
        // because all Structured fields are #[serde(default)].
        let info: ToolUseResultInfo =
            serde_json::from_value(json!({"unknown_field": true})).unwrap();
        assert_eq!(info.stdout(), None);
        assert_eq!(info.stderr(), None);
        assert_eq!(info.interrupted(), None);
    }

    // ── ResultEvent::error_message tests ──────────────────────────────

    #[test]
    fn error_message_prefers_result() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: Some("result msg".to_string()),
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: Some("error field".to_string()),
            message: Some("message field".to_string()),
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "result msg");
    }

    #[test]
    fn error_message_falls_back_to_error_field() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: None,
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: Some("error field".to_string()),
            message: None,
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "error field");
    }

    #[test]
    fn error_message_falls_back_to_message_field() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: None,
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: Some("message field".to_string()),
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "message field");
    }

    #[test]
    fn error_message_falls_back_to_errors_array() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: None,
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec!["first error".to_string(), "second error".to_string()],
        };
        assert_eq!(evt.error_message(), "first error");
    }

    #[test]
    fn error_message_unknown_when_all_empty() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: None,
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "Unknown error");
    }

    #[test]
    fn error_message_parses_embedded_json() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: Some(r#"402 {"error":{"message":"Insufficient credits"}}"#.to_string()),
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "Insufficient credits");
    }

    #[test]
    fn error_message_parses_top_level_message_json() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: Some(r#"{"message":"rate limited"}"#.to_string()),
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "rate limited");
    }

    #[test]
    fn error_message_skips_empty_strings() {
        let evt = ResultEvent {
            subtype: "error".to_string(),
            session_id: "s".to_string(),
            result: Some("".to_string()),
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: Some("".to_string()),
            message: Some("actual message".to_string()),
            errors: vec![],
        };
        assert_eq!(evt.error_message(), "actual message");
    }

    // ── convert_cli_event tests ───────────────────────────────────────

    #[test]
    fn convert_cli_event_system_produces_no_events() {
        let event = CliEvent::System(SystemEvent {
            subtype: "init".to_string(),
            session_id: "sess_1".to_string(),
            tools: vec![],
            model: Some("claude-3".to_string()),
            agents: vec![],
            cwd: None,
            mcp_servers: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_cli_event_text_delta() {
        let event = CliEvent::StreamEvent(StreamEventWrapper {
            event: StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta {
                    delta_type: "text_delta".to_string(),
                    text: Some("hello".to_string()),
                    partial_json: None,
                    thinking: None,
                },
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::TextDelta { content } if content == "hello"));
    }

    #[test]
    fn convert_cli_event_thinking_delta() {
        let event = CliEvent::StreamEvent(StreamEventWrapper {
            event: StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta {
                    delta_type: "thinking_delta".to_string(),
                    text: None,
                    partial_json: None,
                    thinking: Some("reasoning...".to_string()),
                },
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Thinking { content } if content == "reasoning...")
        );
    }

    #[test]
    fn convert_cli_event_empty_text_delta_produces_nothing() {
        let event = CliEvent::StreamEvent(StreamEventWrapper {
            event: StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta {
                    delta_type: "text_delta".to_string(),
                    text: Some("".to_string()),
                    partial_json: None,
                    thinking: None,
                },
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_cli_event_content_block_start_tool_use_tracks_pending() {
        let event = CliEvent::StreamEvent(StreamEventWrapper {
            event: StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlockInfo {
                    block_type: "tool_use".to_string(),
                    text: None,
                    id: Some("tool_1".to_string()),
                    name: Some("bash".to_string()),
                },
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        convert_cli_event(event, &mut pending);
        assert_eq!(pending.get("tool_1").map(|s| s.as_str()), Some("bash"));
    }

    #[test]
    fn convert_cli_event_assistant_tool_use() {
        let event = CliEvent::Assistant(AssistantEvent {
            message: AssistantMessage {
                content: vec![ContentBlock::ToolUse {
                    id: "t1".to_string(),
                    name: "read".to_string(),
                    input: json!({"path": "/tmp/test"}),
                }],
                stop_reason: None,
                model: None,
                id: None,
                usage: None,
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::ToolCall { id, name, .. }
            if id == "t1" && name == "read")
        );
        assert_eq!(pending.get("t1").map(|s| s.as_str()), Some("read"));
    }

    #[test]
    fn convert_cli_event_assistant_text_becomes_thinking() {
        let event = CliEvent::Assistant(AssistantEvent {
            message: AssistantMessage {
                content: vec![ContentBlock::Text {
                    text: "Let me analyze...".to_string(),
                }],
                stop_reason: None,
                model: None,
                id: None,
                usage: None,
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Thinking { content } if content == "Let me analyze...")
        );
    }

    #[test]
    fn convert_cli_event_user_tool_result() {
        // First register a pending tool
        let mut pending = HashMap::new();
        pending.insert("t1".to_string(), "bash".to_string());

        let event = CliEvent::User(UserEvent {
            message: UserMessage {
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: ToolResultContent::Text("output here".to_string()),
                    is_error: false,
                }],
                role: None,
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
            tool_use_result: None,
        });
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::ToolResult { id, name, .. }
            if id == "t1" && name == "bash")
        );
    }

    #[test]
    fn convert_cli_event_user_unknown_tool() {
        let mut pending = HashMap::new();
        // Don't register any tool — should fall back to "unknown"

        let event = CliEvent::User(UserEvent {
            message: UserMessage {
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t99".to_string(),
                    content: ToolResultContent::Text("result".to_string()),
                    is_error: false,
                }],
                role: None,
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
            tool_use_result: None,
        });
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::ToolResult { name, .. }
            if name == "unknown")
        );
    }

    #[test]
    fn convert_cli_event_user_tool_result_with_structured_extra() {
        let mut pending = HashMap::new();
        pending.insert("t1".to_string(), "bash".to_string());

        let extra: ToolUseResultInfo = serde_json::from_value(json!({
            "stdout": "hello",
            "stderr": "",
            "interrupted": false
        }))
        .unwrap();

        let event = CliEvent::User(UserEvent {
            message: UserMessage {
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: ToolResultContent::Text("output".to_string()),
                    is_error: false,
                }],
                role: None,
            },
            session_id: "s1".to_string(),
            parent_tool_use_id: None,
            tool_use_result: Some(extra),
        });
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        if let ExecutionEvent::ToolResult { result, .. } = &results[0] {
            // When extra info is present, result is a JSON object with stdout/stderr
            assert!(result.get("stdout").is_some());
            assert!(result.get("content").is_some());
        } else {
            panic!("Expected ToolResult event");
        }
    }

    #[test]
    fn convert_cli_event_result_error_with_is_error() {
        let event = CliEvent::Result(ResultEvent {
            subtype: "result".to_string(),
            session_id: "s1".to_string(),
            result: Some("something went wrong".to_string()),
            is_error: true,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Error { message } if message == "something went wrong")
        );
    }

    #[test]
    fn convert_cli_event_result_error_subtype() {
        let event = CliEvent::Result(ResultEvent {
            subtype: "error".to_string(),
            session_id: "s1".to_string(),
            result: None,
            is_error: false,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: Some("timeout".to_string()),
            message: None,
            errors: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::Error { message } if message == "timeout"));
    }

    #[test]
    fn convert_cli_event_result_api_error_in_text() {
        let event = CliEvent::Result(ResultEvent {
            subtype: "result".to_string(),
            session_id: "s1".to_string(),
            result: Some(r#"API Error: 429 {"error":{"message":"Rate limited"}}"#.to_string()),
            is_error: false,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], ExecutionEvent::Error { message } if message == "Rate limited")
        );
    }

    #[test]
    fn convert_cli_event_result_success_produces_nothing() {
        let event = CliEvent::Result(ResultEvent {
            subtype: "result".to_string(),
            session_id: "s1".to_string(),
            result: Some("Task completed".to_string()),
            is_error: false,
            total_cost_usd: Some(0.05),
            duration_ms: Some(3000),
            num_turns: Some(5),
            error: None,
            message: None,
            errors: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert!(results.is_empty());
    }

    #[test]
    fn convert_cli_event_result_overloaded_error() {
        let event = CliEvent::Result(ResultEvent {
            subtype: "result".to_string(),
            session_id: "s1".to_string(),
            result: Some(r#"{"type":"overloaded_error","message":"server busy"}"#.to_string()),
            is_error: false,
            total_cost_usd: None,
            duration_ms: None,
            num_turns: None,
            error: None,
            message: None,
            errors: vec![],
        });
        let mut pending = HashMap::new();
        let results = convert_cli_event(event, &mut pending);
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], ExecutionEvent::Error { .. }));
    }

    // ── McpServerInfo tests ──────────────────────────────────────────

    #[test]
    fn mcp_server_info_name_object() {
        let info = McpServerInfo::Object {
            name: "my-server".to_string(),
            status: "ready".to_string(),
        };
        assert_eq!(info.name(), "my-server");
    }

    #[test]
    fn mcp_server_info_name_string() {
        let info = McpServerInfo::String("legacy-server".to_string());
        assert_eq!(info.name(), "legacy-server");
    }
}
