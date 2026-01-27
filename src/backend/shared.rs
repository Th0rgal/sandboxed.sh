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
use tokio::process::Child;
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
}

impl ProcessHandle {
    pub fn new(child: Arc<Mutex<Option<Child>>>, task_handle: JoinHandle<()>) -> Self {
        Self {
            child,
            _task_handle: task_handle,
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
    /// Amp extension.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ToolUseResultInfo {
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub interrupted: Option<bool>,
    #[serde(default, rename = "isImage")]
    pub is_image: Option<bool>,
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
}

impl ResultEvent {
    /// Extract the best available error/result message.
    /// Checks `result`, `error`, and `message` fields in order.
    /// Parses embedded JSON error format (e.g. `402 {"type":"error",...}`)
    /// to extract a human-readable message.
    pub fn error_message(&self) -> String {
        let raw = self
            .result
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.error.as_deref().filter(|s| !s.is_empty()))
            .or(self.message.as_deref().filter(|s| !s.is_empty()))
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

        CliEvent::StreamEvent(wrapper) => {
            match wrapper.event {
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
            }
        }

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
                            "stdout": extra.stdout,
                            "stderr": extra.stderr,
                            "is_error": is_error,
                            "interrupted": extra.interrupted,
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
            if res.is_error || res.subtype == "error" {
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
