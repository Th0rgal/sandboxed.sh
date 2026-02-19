//! Mission Runner - Isolated execution context for a single mission.
//!
//! This module provides a clean abstraction for running missions in parallel.
//! Each MissionRunner manages its own:
//! - Conversation history
//! - Message queue  
//! - Execution state
//! - Cancellation token
//! - Deliverable tracking
//! - Health monitoring
//! - Working directory (isolated per mission)

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentRef, AgentResult, TerminalReason};
use crate::backend::claudecode::client::{ClaudeEvent, ContentBlock, StreamEvent};
use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::opencode::{extract_reasoning, extract_text};
use crate::secrets::SecretsStore;
use crate::task::{extract_deliverables, DeliverableSet};
use crate::util::{
    auth_entry_has_credentials, build_history_context, env_var_bool, home_dir, strip_jsonc_comments,
};
use crate::workspace::{self, Workspace, WorkspaceType};
use crate::workspace_exec::WorkspaceExec;

use super::control::{
    resolve_claudecode_default_model, safe_truncate_index, AgentEvent, AgentTreeNode,
    ControlRunState, ControlStatus, ExecutionProgress, FrontendToolHub,
};
use super::library::SharedLibrary;

#[derive(Debug, Default)]
struct OpencodeSseState {
    message_roles: HashMap<String, String>,
    part_buffers: HashMap<String, String>,
    emitted_tool_calls: HashMap<String, ()>,
    emitted_tool_results: HashMap<String, ()>,
    response_tool_args: HashMap<String, String>,
    response_tool_names: HashMap<String, String>,
    last_emitted_thinking: Option<String>,
    last_emitted_text: Option<String>,
}

struct OpencodeSseParseResult {
    event: Option<AgentEvent>,
    message_complete: bool,
    session_id: Option<String>,
    /// The SSE stream indicated the session became idle.  This is a weaker
    /// signal than `message_complete` — it means OpenCode is no longer
    /// processing, but not necessarily that a `response.completed` was sent
    /// (common with GLM models that emit `response.incomplete` instead).
    session_idle: bool,
    /// The SSE stream indicated the session entered a retry state, meaning
    /// the model API call failed and OpenCode is retrying automatically.
    session_retry: bool,
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

/// Strip `<think>...</think>` tags from text output.
/// Some models (e.g. Minimax, DeepSeek) emit internal reasoning inside inline
/// `<think>` tags that should not be shown in the text output.
fn strip_think_tags(text: &str) -> String {
    // Case-insensitive search directly on the original text to avoid
    // byte-offset misalignment from to_lowercase() on non-ASCII input.
    fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
        let needle_len = needle.len();
        if haystack.len() < needle_len {
            return None;
        }
        haystack
            .as_bytes()
            .windows(needle_len)
            .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    }

    if find_ci(text, "<think>").is_none() {
        return text.to_string();
    }

    let mut result = String::new();
    let mut pos = 0;

    while pos < text.len() {
        if let Some(rel_start) = find_ci(&text[pos..], "<think>") {
            let abs_start = pos + rel_start;
            result.push_str(&text[pos..abs_start]);

            let after_open = abs_start + 7; // len("<think>")
            if after_open <= text.len() {
                if let Some(rel_close) = find_ci(&text[after_open..], "</think>") {
                    pos = after_open + rel_close + 8; // len("</think>")
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            result.push_str(&text[pos..]);
            break;
        }
    }

    result
}

/// Prefixes that indicate a thought/reasoning line
const THOUGHT_PREFIXES: &[&str] = &["thought:", "thoughts:", "thinking:"];

fn extract_thought_line(text: &str) -> Option<(String, String)> {
    let mut thought: Option<String> = None;
    let mut remaining: Vec<&str> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        let is_thought = THOUGHT_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix));

        if thought.is_none() && is_thought {
            let content = trimmed
                .split_once(':')
                .map(|(_, rest)| rest)
                .unwrap_or("")
                .trim()
                .to_string();
            if !content.is_empty() {
                thought = Some(content);
            }
            continue;
        }
        remaining.push(line);
    }

    thought.map(|t| {
        let cleaned = remaining.join("\n").trim().to_string();
        (t, cleaned)
    })
}

async fn set_control_state_for_mission(
    status: &Arc<RwLock<ControlStatus>>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    state: ControlRunState,
) {
    let (queue_len, mission_id_opt) = {
        let mut guard = status.write().await;
        if let Some(existing) = guard.mission_id {
            if existing != mission_id {
                return;
            }
        } else {
            guard.mission_id = Some(mission_id);
        }
        guard.state = state;
        (guard.queue_len, guard.mission_id)
    };
    let _ = events_tx.send(AgentEvent::Status {
        state,
        queue_len,
        mission_id: mission_id_opt,
    });
}

fn handle_tool_part_update(
    part: &serde_json::Value,
    state: &mut OpencodeSseState,
    mission_id: Uuid,
) -> Option<AgentEvent> {
    let state_obj = part.get("state")?;
    let status = state_obj.get("status").and_then(|v| v.as_str())?;

    let tool_call_id = extract_str(part, &["callID", "id"])
        .unwrap_or("unknown")
        .to_string();

    let tool_name = extract_str(part, &["tool", "name"])
        .unwrap_or("unknown")
        .to_string();

    match status {
        "running" => {
            if state.emitted_tool_calls.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_calls.insert(tool_call_id.clone(), ());
            let args = state_obj
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(AgentEvent::ToolCall {
                tool_call_id,
                name: tool_name,
                args,
                mission_id: Some(mission_id),
            })
        }
        "completed" => {
            if state.emitted_tool_results.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_results.insert(tool_call_id.clone(), ());
            let result = state_obj
                .get("output")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(AgentEvent::ToolResult {
                tool_call_id,
                name: tool_name,
                result,
                mission_id: Some(mission_id),
            })
        }
        "error" => {
            if state.emitted_tool_results.contains_key(&tool_call_id) {
                return None;
            }
            state.emitted_tool_results.insert(tool_call_id.clone(), ());
            let error_msg = state_obj
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            let result = serde_json::json!({ "error": error_msg });
            Some(AgentEvent::ToolResult {
                tool_call_id,
                name: tool_name,
                result,
                mission_id: Some(mission_id),
            })
        }
        _ => None,
    }
}

fn handle_part_update(
    props: &serde_json::Value,
    state: &mut OpencodeSseState,
    mission_id: Uuid,
) -> Option<AgentEvent> {
    let part = props.get("part")?;
    let part_type = part.get("type").and_then(|v| v.as_str())?;

    if part_type == "tool" {
        return handle_tool_part_update(part, state, mission_id);
    }

    let is_thinking = matches!(
        part_type,
        "thinking" | "reasoning" | "step-start" | "step-finish"
    );
    let is_text = matches!(part_type, "text" | "output_text");

    if !is_thinking && !is_text {
        tracing::debug!(
            part_type = %part_type,
            mission_id = %mission_id,
            "Unhandled part type in handle_part_update"
        );
        return None;
    }

    let part_id = extract_str(part, &["id", "partID", "partId"]);
    let message_id = extract_str(part, &["messageID", "messageId", "message_id"])
        .or_else(|| extract_str(props, &["messageID", "messageId", "message_id"]));
    if let Some(message_id) = message_id {
        match state.message_roles.get(message_id) {
            Some(role) if role != "assistant" => return None,
            None => {
                // Role not yet recorded (message.updated hasn't arrived).
                // Skip to avoid emitting user-message text as a TextDelta,
                // which would trigger the text-idle timeout prematurely.
                return None;
            }
            _ => {} // assistant — continue processing
        }
    }

    let delta = props.get("delta").and_then(|v| v.as_str());
    let full_text = extract_part_text(part, part_type);
    let buffer_key = format!(
        "{}:{}",
        part_type,
        part_id.or(message_id).unwrap_or(part_type)
    );
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

    let mut content = content;
    if let Cow::Owned(cleaned) = strip_opencode_banner_lines(&content) {
        if cleaned != content {
            *buffer = cleaned.clone();
        }
        content = cleaned;
    }

    // Strip inline <think>...</think> tags from text parts.
    // Don't modify the buffer so incomplete tags across deltas are handled correctly.
    let content = if !is_thinking {
        strip_think_tags(&content)
    } else {
        content
    };

    if content.trim().is_empty() {
        return None;
    }

    if is_thinking {
        if state.last_emitted_thinking.as_ref() == Some(&content) {
            return None;
        }
        state.last_emitted_thinking = Some(content.clone());
        return Some(AgentEvent::Thinking {
            content,
            done: false,
            mission_id: Some(mission_id),
        });
    }

    if state.last_emitted_text.as_ref() == Some(&content) {
        return None;
    }
    state.last_emitted_text = Some(content.clone());
    Some(AgentEvent::TextDelta {
        content,
        mission_id: Some(mission_id),
    })
}

fn parse_opencode_stderr_text_part(line: &str) -> Option<String> {
    let marker = "message.part (text):";
    let idx = line.find(marker)?;
    let mut text = line[idx + marker.len()..].trim().to_string();
    if let Some(stripped) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        text = stripped.to_string();
    }
    if text.contains('\\') {
        // Use a placeholder to avoid double-processing: \\n in source should stay as literal \n
        text = text
            .replace("\\\\", "\x00BACKSLASH\x00") // Temporarily replace \\
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\x00BACKSLASH\x00", "\\"); // Restore single backslash
    }
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn parse_opencode_sse_event(
    data_str: &str,
    event_name: Option<&str>,
    current_session_id: Option<&str>,
    state: &mut OpencodeSseState,
    mission_id: Uuid,
) -> Option<OpencodeSseParseResult> {
    let json: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(value) => value,
        Err(_) => return None,
    };

    let event_type = json.get("type").and_then(|v| v.as_str()).or(event_name)?;
    let props = json
        .get("properties")
        .cloned()
        .unwrap_or_else(|| json.clone());

    let event_session_id = props
        .get("sessionID")
        .or_else(|| props.get("info").and_then(|v| v.get("sessionID")))
        .or_else(|| props.get("part").and_then(|v| v.get("sessionID")))
        .and_then(|v| v.as_str());

    if let Some(expected) = current_session_id {
        if let Some(actual) = event_session_id {
            if actual != expected {
                return None;
            }
        }
    }

    let mut session_id = None;
    if current_session_id.is_none() {
        if let Some(actual) = event_session_id {
            session_id = Some(actual.to_string());
        }
    }

    let mut message_complete = false;
    let event = match event_type {
        "response.output_text.delta" => {
            let delta = props
                .get("delta")
                .or_else(|| props.get("text"))
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
                let content = buffer.clone();
                if state.last_emitted_text.as_ref() == Some(&content) {
                    None
                } else {
                    state.last_emitted_text = Some(content.clone());
                    Some(AgentEvent::TextDelta {
                        content,
                        mission_id: Some(mission_id),
                    })
                }
            }
        }
        "response.completed" => {
            tracing::info!(
                mission_id = %mission_id,
                "✅ response.completed - mission completing normally"
            );
            message_complete = true;
            None
        }
        "response.incomplete" => {
            tracing::warn!(
                mission_id = %mission_id,
                event_data = ?props,
                "response.incomplete received — treating as completion (common with GLM models)"
            );
            // GLM models from z.ai frequently emit response.incomplete as their
            // terminal event without a subsequent response.completed.  Treat it
            // the same as response.completed so the mission can finish.  If a
            // retry does happen, it will produce a new response.completed.
            message_complete = true;
            None
        }
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
                            serde_json::json!({})
                        } else {
                            serde_json::from_str(&args_str)
                                .unwrap_or_else(|_| serde_json::json!({ "arguments": args_str }))
                        };
                        state.emitted_tool_calls.insert(call_id.clone(), ());
                        Some(AgentEvent::ToolCall {
                            tool_call_id: call_id,
                            name,
                            args,
                            mission_id: Some(mission_id),
                        })
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
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
                handle_part_update(&props, state, mission_id)
            } else {
                None
            }
        }
        "message.part.updated" => handle_part_update(&props, state, mission_id),
        "tool.execute" => {
            let tool_name = props
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let tool_id = format!("opencode-{}", uuid::Uuid::new_v4());
            let args = props
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            state.emitted_tool_calls.insert(tool_id.clone(), ());
            Some(AgentEvent::ToolCall {
                tool_call_id: tool_id,
                name: tool_name,
                args,
                mission_id: Some(mission_id),
            })
        }
        "tool.result" => {
            let tool_name = props
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let output = props
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Use the most recent tool call id if tracking
            let tool_id = format!("opencode-{}", uuid::Uuid::new_v4());
            Some(AgentEvent::ToolResult {
                tool_call_id: tool_id,
                name: tool_name,
                result: serde_json::json!({ "output": output }),
                mission_id: Some(mission_id),
            })
        }
        "message.completed" | "assistant.message.completed" => {
            tracing::info!(
                mission_id = %mission_id,
                event_type = %event_type,
                "Message completed event received"
            );
            message_complete = true;
            None
        }
        "session.error" => {
            let message = props
                .get("error")
                .and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| serde_json::to_string(v).ok())
                })
                .unwrap_or_else(|| "Unknown session error".to_string());
            Some(AgentEvent::Error {
                message,
                mission_id: Some(mission_id),
                resumable: true,
            })
        }
        "error" | "message.error" => {
            let message = props
                .get("message")
                .or(props.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Some(AgentEvent::Error {
                message,
                mission_id: Some(mission_id),
                resumable: true,
            })
        }
        _ => None,
    };

    // Detect session idle signals — oh-my-opencode emits these when the
    // agent finishes all work.  This is critical for GLM models that may
    // not emit response.completed.
    let status_str = if event_type == "session.status" {
        props
            .get("type")
            .or_else(|| props.get("status"))
            .and_then(|v| v.as_str())
    } else {
        None
    };

    let session_idle = matches!(event_type, "session.idle")
        || (event_type == "session.status" && status_str == Some("idle"));

    // Detect retry signals — OpenCode emits session.status with type "retry"
    // when a model API call fails and it's retrying automatically.
    let session_retry = event_type == "session.status" && status_str == Some("retry");

    Some(OpencodeSseParseResult {
        event,
        message_complete,
        session_id,
        session_idle,
        session_retry,
    })
}

/// State of a running mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionRunState {
    /// Waiting in queue
    Queued,
    /// Currently executing
    Running,
    /// Waiting for frontend tool input
    WaitingForTool,
    /// Finished (check result)
    Finished,
}

const STALL_WARN_SECS: u64 = 120;
const STALL_SEVERE_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStallSeverity {
    Warning,
    Severe,
}

/// Health status of a mission.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MissionHealth {
    /// Mission is progressing normally
    Healthy,
    /// Mission may be stalled
    Stalled {
        seconds_since_activity: u64,
        last_state: String,
        severity: MissionStallSeverity,
    },
    /// Mission completed without deliverables
    MissingDeliverables { missing: Vec<String> },
    /// Mission ended unexpectedly
    UnexpectedEnd { reason: String },
}

fn stall_severity(seconds_since_activity: u64) -> Option<MissionStallSeverity> {
    if seconds_since_activity > STALL_SEVERE_SECS {
        Some(MissionStallSeverity::Severe)
    } else if seconds_since_activity > STALL_WARN_SECS {
        Some(MissionStallSeverity::Warning)
    } else {
        None
    }
}

pub fn running_health(state: MissionRunState, seconds_since_activity: u64) -> MissionHealth {
    if matches!(
        state,
        MissionRunState::Running | MissionRunState::WaitingForTool
    ) {
        if let Some(severity) = stall_severity(seconds_since_activity) {
            return MissionHealth::Stalled {
                seconds_since_activity,
                last_state: format!("{:?}", state),
                severity,
            };
        }
    }
    MissionHealth::Healthy
}

/// A message queued for this mission.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    /// Optional agent override for this specific message (e.g., from @agent mention)
    pub agent: Option<String>,
}

/// Isolated runner for a single mission.
/// Info about a tracked subtask (from delegate_task/Task tool calls).
#[derive(Debug, Clone)]
pub struct SubtaskInfo {
    pub tool_call_id: String,
    pub description: String,
    pub completed: bool,
}

pub struct MissionRunner {
    /// Mission ID
    pub mission_id: Uuid,

    /// Workspace ID where this mission should run
    pub workspace_id: Uuid,

    /// Backend ID used for this mission
    pub backend_id: String,

    /// Session ID for conversation persistence (used by Claude Code --session-id)
    pub session_id: Option<String>,

    /// Config profile from the mission (overrides workspace config_profile)
    pub config_profile: Option<String>,

    /// Current state
    pub state: MissionRunState,

    /// Agent override for this mission
    pub agent_override: Option<String>,

    /// Model override for this mission (e.g. "zai/glm-5")
    pub model_override: Option<String>,

    /// Model effort override for this mission (e.g. low/medium/high)
    pub model_effort: Option<String>,

    /// Message queue for this mission
    pub queue: VecDeque<QueuedMessage>,

    /// Conversation history: (role, content)
    pub history: Vec<(String, String)>,

    /// Cancellation token for the current execution
    pub cancel_token: Option<CancellationToken>,

    /// Running task handle
    running_handle: Option<tokio::task::JoinHandle<(Uuid, String, AgentResult)>>,

    /// Tree snapshot for this mission
    pub tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,

    /// Progress snapshot for this mission
    pub progress_snapshot: Arc<RwLock<ExecutionProgress>>,

    /// Expected deliverables extracted from the initial message
    pub deliverables: DeliverableSet,

    /// Last activity timestamp for health monitoring
    pub last_activity: Instant,

    /// Whether complete_mission was explicitly called
    pub explicitly_completed: bool,

    /// Current activity label (derived from latest tool call)
    pub current_activity: Option<String>,

    /// Tracked subtasks (from delegate_task/Task tool calls)
    pub subtasks: Vec<SubtaskInfo>,
}

impl MissionRunner {
    /// Create a new mission runner.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mission_id: Uuid,
        workspace_id: Uuid,
        agent_override: Option<String>,
        backend_id: Option<String>,
        session_id: Option<String>,
        config_profile: Option<String>,
        model_override: Option<String>,
        model_effort: Option<String>,
    ) -> Self {
        Self {
            mission_id,
            workspace_id,
            backend_id: backend_id.unwrap_or_else(|| "opencode".to_string()),
            session_id,
            config_profile,
            state: MissionRunState::Queued,
            agent_override,
            model_override,
            model_effort,
            queue: VecDeque::new(),
            history: Vec::new(),
            cancel_token: None,
            running_handle: None,
            tree_snapshot: Arc::new(RwLock::new(None)),
            progress_snapshot: Arc::new(RwLock::new(ExecutionProgress::default())),
            deliverables: DeliverableSet::default(),
            last_activity: Instant::now(),
            explicitly_completed: false,
            current_activity: None,
            subtasks: Vec::new(),
        }
    }

    /// Check if this runner is currently executing.
    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            MissionRunState::Running | MissionRunState::WaitingForTool
        )
    }

    /// Check if this runner has finished.
    pub fn is_finished(&self) -> bool {
        matches!(self.state, MissionRunState::Finished)
    }

    /// Update the last activity timestamp.
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check the health of this mission.
    pub async fn check_health(&self) -> MissionHealth {
        let seconds_since = self.last_activity.elapsed().as_secs();

        // If running and no activity for a while, consider stalled
        if self.is_running() {
            if let Some(severity) = stall_severity(seconds_since) {
                return MissionHealth::Stalled {
                    seconds_since_activity: seconds_since,
                    last_state: format!("{:?}", self.state),
                    severity,
                };
            }
        }

        // If finished without explicit completion and has deliverables, check them
        if !self.is_running()
            && !self.explicitly_completed
            && !self.deliverables.deliverables.is_empty()
        {
            let missing = self.deliverables.missing_paths().await;
            if !missing.is_empty() {
                return MissionHealth::MissingDeliverables { missing };
            }
        }

        MissionHealth::Healthy
    }

    /// Extract deliverables from initial mission message.
    pub fn set_initial_message(&mut self, message: &str) {
        self.deliverables = extract_deliverables(message);
        if !self.deliverables.deliverables.is_empty() {
            tracing::info!(
                "Mission {} has {} expected deliverables: {:?}",
                self.mission_id,
                self.deliverables.deliverables.len(),
                self.deliverables
                    .deliverables
                    .iter()
                    .filter_map(|d| d.path())
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Queue a message for this mission.
    pub fn queue_message(&mut self, id: Uuid, content: String, agent: Option<String>) {
        self.queue.push_back(QueuedMessage { id, content, agent });
    }

    /// Cancel the current execution.
    pub fn cancel(&mut self) {
        if let Some(token) = &self.cancel_token {
            token.cancel();
        }
    }

    /// Remove a specific message from the queue by ID.
    /// Returns true if the message was found and removed.
    pub fn remove_from_queue(&mut self, message_id: Uuid) -> bool {
        let before_len = self.queue.len();
        self.queue.retain(|qm| qm.id != message_id);
        self.queue.len() < before_len
    }

    /// Clear all queued messages.
    /// Returns the number of messages that were cleared.
    pub fn clear_queue(&mut self) -> usize {
        let cleared = self.queue.len();
        self.queue.clear();
        cleared
    }

    /// Start executing the next queued message (if any and not already running).
    /// Returns true if execution was started.
    #[allow(clippy::too_many_arguments)]
    pub fn start_next(
        &mut self,
        config: Config,
        root_agent: AgentRef,
        mcp: Arc<McpRegistry>,
        workspaces: workspace::SharedWorkspaceStore,
        library: SharedLibrary,
        events_tx: broadcast::Sender<AgentEvent>,
        tool_hub: Arc<FrontendToolHub>,
        status: Arc<RwLock<ControlStatus>>,
        mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
        current_mission: Arc<RwLock<Option<Uuid>>>,
        secrets: Option<Arc<SecretsStore>>,
    ) -> bool {
        // Don't start if already running
        if self.is_running() {
            return false;
        }

        // Get next message from queue
        let msg = match self.queue.pop_front() {
            Some(m) => m,
            None => return false,
        };

        self.state = MissionRunState::Running;

        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());

        let hist_snapshot = self.history.clone();
        let tree_ref = Arc::clone(&self.tree_snapshot);
        let progress_ref = Arc::clone(&self.progress_snapshot);
        let mission_id = self.mission_id;
        let workspace_id = self.workspace_id;
        let agent_override = self.agent_override.clone();
        let model_override = self.model_override.clone();
        let model_effort = self.model_effort.clone();
        let backend_id = self.backend_id.clone();
        let session_id = self.session_id.clone();
        let config_profile = self.config_profile.clone();
        let user_message = msg.content.clone();
        let msg_id = msg.id;
        tracing::info!(
            mission_id = %mission_id,
            workspace_id = %workspace_id,
            agent_override = ?agent_override,
            message_id = %msg_id,
            message_len = user_message.len(),
            "Mission runner starting"
        );

        // Create mission control for complete_mission tool
        let mission_ctrl = crate::tools::mission::MissionControl {
            current_mission_id: current_mission,
            cmd_tx: mission_cmd_tx,
        };

        // Emit user message event with mission context
        let _ = events_tx.send(AgentEvent::UserMessage {
            id: msg_id,
            content: user_message.clone(),
            queued: false,
            mission_id: Some(mission_id),
        });

        let handle = tokio::spawn(async move {
            let result = run_mission_turn(
                config,
                root_agent,
                mcp,
                workspaces,
                library,
                events_tx,
                tool_hub,
                status,
                cancel,
                hist_snapshot,
                user_message.clone(),
                Some(mission_ctrl),
                tree_ref,
                progress_ref,
                mission_id,
                Some(workspace_id),
                backend_id,
                agent_override,
                model_override,
                model_effort,
                secrets,
                session_id,
                config_profile,
            )
            .await;
            (msg_id, user_message, result)
        });

        self.running_handle = Some(handle);
        true
    }

    /// Poll for completion. Returns Some(result) if finished.
    pub async fn poll_completion(&mut self) -> Option<(Uuid, String, AgentResult)> {
        let handle = self.running_handle.take()?;

        // Check if handle is finished
        if handle.is_finished() {
            match handle.await {
                Ok(result) => {
                    self.touch(); // Update last activity
                    self.state = MissionRunState::Queued; // Ready for next message

                    // Check if complete_mission was called
                    if result.2.output.contains("Mission marked as")
                        || result.2.output.contains("complete_mission")
                    {
                        self.explicitly_completed = true;
                    }

                    // Add to history — only include assistant output when it's
                    // a real model response.  Error messages (e.g. "Claude Code
                    // produced no output", "OpenCode CLI exited with status: ...")
                    // would contaminate context for future turns.
                    self.history.push(("user".to_string(), result.1.clone()));
                    if result.2.success && !result.2.output.trim().is_empty() {
                        self.history
                            .push(("assistant".to_string(), result.2.output.clone()));
                    }

                    // Log warning if deliverables are missing and task ended
                    if !self.explicitly_completed && !self.deliverables.deliverables.is_empty() {
                        let missing = self.deliverables.missing_paths().await;
                        if !missing.is_empty() {
                            tracing::warn!(
                                "Mission {} ended but deliverables are missing: {:?}",
                                self.mission_id,
                                missing
                            );
                        }
                    }

                    Some(result)
                }
                Err(e) => {
                    tracing::error!("Mission runner task failed: {}", e);
                    self.state = MissionRunState::Finished;
                    None
                }
            }
        } else {
            // Not finished, put handle back
            self.running_handle = Some(handle);
            None
        }
    }

    /// Check if the running task is finished (non-blocking).
    /// Returns false when no task handle exists (idle/unstarted runners)
    /// to avoid unnecessary poll_completion calls every 100ms.
    pub fn check_finished(&self) -> bool {
        self.running_handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false)
    }
}

/// Try to resolve a library command from a user message starting with `/`.
/// If the message starts with `/command-name` and a matching command exists in the library,
/// returns the command's body content (frontmatter stripped). Otherwise returns the original message.
async fn resolve_library_command(library: &SharedLibrary, message: &str) -> String {
    let trimmed = message.trim();

    // Must start with / and have at least one non-slash character
    if !trimmed.starts_with('/') || trimmed.len() < 2 {
        return message.to_string();
    }

    // Extract command name and optional arguments
    let without_slash = &trimmed[1..];
    let (command_name, args) = match without_slash.find(|c: char| c.is_whitespace()) {
        Some(pos) => (&without_slash[..pos], without_slash[pos..].trim()),
        None => (without_slash, ""),
    };

    // Try to fetch from library
    let lib_guard = library.read().await;
    let Some(lib) = lib_guard.as_ref() else {
        return message.to_string();
    };

    match lib.get_command(command_name).await {
        Ok(command) => {
            // Strip frontmatter from content to get the body
            let (_frontmatter, body) = crate::library::types::parse_frontmatter(&command.content);
            let body = body.trim();

            tracing::info!(
                command_name = command_name,
                has_args = !args.is_empty(),
                "Resolved library command"
            );

            if args.is_empty() {
                body.to_string()
            } else {
                format!("{}\n\nArguments: {}", body, args)
            }
        }
        Err(_) => {
            // Not a library command, pass through as-is (may be a builtin like /plan)
            message.to_string()
        }
    }
}

/// Check whether a failed turn result indicates a corrupt/stale Claude Code session
/// that can be recovered by resetting the session and retrying.
///
/// This covers:
/// - "no stream events after startup timeout" — CLI hangs on resume
/// - API validation errors from corrupted conversation history (e.g. mismatched
///   tool_use_id / tool_result blocks after a session was partially lost)
pub fn is_session_corruption_error(result: &AgentResult) -> bool {
    if result.success || result.terminal_reason != Some(TerminalReason::LlmError) {
        return false;
    }
    let out = &result.output;
    // Stuck session: CLI started but emitted no parseable events
    out.starts_with(
        "Claude Code produced no stream events after startup timeout",
    )
    // API rejected the reconstructed conversation history
    || out.contains("unexpected tool_use_id found in tool_result blocks")
    || out.contains("tool_use block must have a corresponding tool_result")
    || out.contains("tool_result block must have a corresponding tool_use")
    || out.contains("must have a corresponding tool_use block")
    // Session was lost (e.g. after service restart or session expiry)
    || out.contains("No conversation found with session ID")
}

/// Execute a single turn for a mission.
#[allow(clippy::too_many_arguments)]
async fn run_mission_turn(
    config: Config,
    _root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    _mission_control: Option<crate::tools::mission::MissionControl>,
    _tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,
    _progress_snapshot: Arc<RwLock<ExecutionProgress>>,
    mission_id: Uuid,
    workspace_id: Option<Uuid>,
    backend_id: String,
    agent_override: Option<String>,
    model_override: Option<String>,
    model_effort: Option<String>,
    secrets: Option<Arc<SecretsStore>>,
    session_id: Option<String>,
    mission_config_profile: Option<String>,
) -> AgentResult {
    let mut config = config;
    let effective_agent = agent_override.clone();
    if let Some(ref agent) = effective_agent {
        config.opencode_agent = Some(agent.clone());
    }
    if let Some(ref model) = model_override {
        config.default_model = Some(model.clone());
    }
    // Get config profile: mission's config_profile takes priority over workspace's
    let workspace_config_profile = if let Some(ws_id) = workspace_id {
        workspaces.get(ws_id).await.and_then(|ws| ws.config_profile)
    } else {
        None
    };
    tracing::info!(
        mission_id = %mission_id,
        mission_config_profile = ?mission_config_profile,
        workspace_config_profile = ?workspace_config_profile,
        "Resolving config profile"
    );
    let effective_config_profile = mission_config_profile.or(workspace_config_profile);
    if backend_id == "claudecode" && config.default_model.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&library, effective_config_profile.as_deref()).await
        {
            config.default_model = Some(default_model);
        }
    } else if backend_id == "opencode"
        && effective_config_profile.is_some()
        && model_override.is_none()
    {
        // For OpenCode with a config profile but no explicit model override,
        // clear the global default so the profile's oh-my-opencode agent
        // models take precedence instead of being overridden.
        config.default_model = None;
    } else if backend_id == "codex" && model_override.is_none() {
        // The global DEFAULT_MODEL (e.g. claude-opus-4-6) is not valid for
        // Codex.  Clear it so Codex uses its own CLI default.
        config.default_model = None;
    }
    tracing::info!(
        mission_id = %mission_id,
        workspace_id = ?workspace_id,
        opencode_agent = ?config.opencode_agent,
        history_len = history.len(),
        user_message_len = user_message.len(),
        "Mission turn started"
    );

    // Resolve library commands (e.g., /bugbot-review → expanded command content)
    let user_message = resolve_library_command(&library, &user_message).await;

    // Build context with history
    let max_history_chars = config.context.max_history_total_chars;
    let history_context = build_history_context(&history, max_history_chars);

    // Extract deliverables to include in instructions
    let deliverable_set = extract_deliverables(&user_message);
    let deliverable_reminder = if !deliverable_set.deliverables.is_empty() {
        let paths: Vec<String> = deliverable_set
            .deliverables
            .iter()
            .filter_map(|d| d.path())
            .map(|p| p.display().to_string())
            .collect();
        format!(
            "\n\n**REQUIRED DELIVERABLES** (do not stop until these exist):\n{}\n",
            paths
                .iter()
                .map(|p| format!("- {}", p))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        String::new()
    };

    let is_multi_step = deliverable_set.is_research_task
        || deliverable_set.requires_report
        || user_message.contains("1.")
        || user_message.contains("- ")
        || user_message.to_lowercase().contains("then");

    let multi_step_instructions = if is_multi_step {
        r#"

**MULTI-STEP TASK RULES:**
- This task has multiple steps. Complete ALL steps before stopping.
- After each tool call, ask yourself: "Have I completed the FULL goal?"
- DO NOT stop after just one step - keep working until ALL deliverables exist.
- If you made progress but aren't done, continue in the same turn.
- Only call complete_mission when ALL requested outputs have been created."#
    } else {
        ""
    };

    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str(&deliverable_reminder);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- Use available tools to gather information or make changes.\n- For large data processing tasks (>10KB), prefer executing scripts rather than inline processing.\n- USE information already provided in the message - do not ask for URLs, paths, or details that were already given.\n- When you have fully completed the user's goal or determined it cannot be completed, state that clearly in your final response.");
    convo.push_str(multi_step_instructions);
    convo.push('\n');

    // Ensure mission workspace exists and is configured for OpenCode.
    let workspace = workspace::resolve_workspace(&workspaces, &config, workspace_id).await;
    if let Err(e) =
        workspace::sync_workspace_mcp_binaries_for_workspace(&config.working_dir, &workspace).await
    {
        tracing::warn!(
            workspace = %workspace.name,
            error = %e,
            "Failed to sync MCP binaries into workspace"
        );
    }
    let workspace_root = workspace.path.clone();
    let mission_work_dir_result = {
        let lib_guard = library.read().await;
        let lib_ref = lib_guard.as_ref().map(|l| l.as_ref());
        workspace::prepare_mission_workspace_with_skills_backend(
            &workspace,
            &mcp,
            lib_ref,
            mission_id,
            &backend_id,
            None, // custom_providers: TODO integrate with provider store
            effective_config_profile.as_deref(),
        )
        .await
    };
    let mission_work_dir = match mission_work_dir_result {
        Ok(dir) => {
            tracing::info!(
                "Mission {} workspace directory: {}",
                mission_id,
                dir.display()
            );
            dir
        }
        Err(e) => {
            tracing::warn!("Failed to prepare mission workspace, using default: {}", e);
            workspace_root
        }
    };

    // Session rotation: Prevent OOM by resetting sessions every N turns
    // Calculate turn count (each assistant response = 1 turn)
    const SESSION_ROTATION_INTERVAL: usize = 50;
    let turn_count = history
        .iter()
        .filter(|(role, _)| role == "assistant")
        .count();
    let should_rotate = turn_count > 0 && turn_count % SESSION_ROTATION_INTERVAL == 0;

    // Prepare user message and session ID (potentially with rotation)
    let (mut user_message, mut session_id) = (user_message, session_id);

    if should_rotate && backend_id == "claudecode" {
        tracing::info!(
            mission_id = %mission_id,
            turn_count = turn_count,
            interval = SESSION_ROTATION_INTERVAL,
            "Rotating session to prevent OOM from unbounded context accumulation"
        );

        // Generate summary of recent work from history
        let summary = generate_session_summary(&history, SESSION_ROTATION_INTERVAL);

        // Create new session ID
        let new_session_id = Uuid::new_v4().to_string();

        // Inject summary into user message
        user_message = format!(
            "## Session Rotated (Turn {})\n\n\
             **Previous Work Summary:**\n{}\n\n\
             ---\n\n\
             ## Current Task\n\n\
             {}",
            turn_count, summary, user_message
        );

        // Update session ID and notify via events
        let _ = events_tx.send(AgentEvent::SessionIdUpdate {
            mission_id,
            session_id: new_session_id.clone(),
        });

        session_id = Some(new_session_id.clone());

        // Delete the session marker file to force a fresh session
        let session_marker = mission_work_dir.join(".claude-session-initiated");
        if session_marker.exists() {
            if let Err(e) = std::fs::remove_file(&session_marker) {
                tracing::warn!(
                    error = %e,
                    "Failed to remove session marker during rotation"
                );
            }
        }

        tracing::info!(
            mission_id = %mission_id,
            new_session_id = %new_session_id,
            summary_length = summary.len(),
            "Session rotated successfully"
        );
    }

    // Execute based on backend
    // For Claude Code, check if this is a continuation turn (has prior assistant response).
    // Note: history may include the current user message before the turn runs,
    // so we check for assistant messages to determine if this is truly a continuation.
    let is_continuation = history.iter().any(|(role, _)| role == "assistant");
    let result = match backend_id.as_str() {
        "claudecode" => {
            // Track the effective message and session used for the most recent
            // attempt, so account rotation uses the right context (e.g. after
            // session corruption recovery rebuilds the message).
            let mut effective_msg = user_message.clone();
            let mut effective_sid = session_id.clone();

            let mut result = run_claudecode_turn(
                &workspace,
                &mission_work_dir,
                &effective_msg,
                config.default_model.as_deref(),
                effective_agent.as_deref(),
                mission_id,
                events_tx.clone(),
                cancel.clone(),
                secrets.clone(),
                &config.working_dir,
                effective_sid.as_deref(),
                is_continuation,
                Some(Arc::clone(&tool_hub)),
                Some(Arc::clone(&status)),
                None, // override_auth: use default credential resolution
            )
            .await;

            // Claude Code can fail when resuming a session due to stale/corrupt state:
            // - CLI hangs and emits no parseable stream events
            // - API rejects reconstructed history (e.g. mismatched tool_use_id)
            // When that happens, auto-reset the session_id and retry once fresh.
            if is_continuation && is_session_corruption_error(&result) {
                let new_session_id = Uuid::new_v4().to_string();
                tracing::warn!(
                    mission_id = %mission_id,
                    old_session_id = ?session_id,
                    new_session_id = %new_session_id,
                    error = %result.output,
                    "Session corruption detected; resetting session and retrying once"
                );

                // Persist the new session ID via the event pipeline.
                let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                    mission_id,
                    session_id: new_session_id.clone(),
                });

                // Delete the stale session marker so the retry creates a fresh one.
                let session_marker = mission_work_dir.join(".claude-session-initiated");
                if session_marker.exists() {
                    let _ = std::fs::remove_file(&session_marker);
                }

                // Build retry message with history context so the agent retains
                // context from earlier turns (the fresh session has no memory).
                let history_for_retry = match history.last() {
                    Some((role, content)) if role == "user" && content == &user_message => {
                        &history[..history.len() - 1]
                    }
                    _ => history.as_slice(),
                };
                let retry_message = if history_for_retry.is_empty() {
                    user_message.clone()
                } else {
                    let history_ctx = build_history_context(
                        history_for_retry,
                        config.context.max_history_total_chars,
                    );
                    format!(
                        "## Prior conversation (session was reset due to a transient error)\n\n\
                         {history_ctx}\
                         ## Current message\n\n\
                         {user_message}"
                    )
                };

                // Update effective context so account rotation uses the
                // recovery message and new session, not the stale originals.
                effective_msg = retry_message;
                effective_sid = Some(new_session_id);

                result = run_claudecode_turn(
                    &workspace,
                    &mission_work_dir,
                    &effective_msg,
                    config.default_model.as_deref(),
                    effective_agent.as_deref(),
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    secrets.clone(),
                    &config.working_dir,
                    effective_sid.as_deref(),
                    false, // Fresh session — don't pass is_continuation=true
                    Some(Arc::clone(&tool_hub)),
                    Some(Arc::clone(&status)),
                    None, // override_auth
                )
                .await;
            }

            // Account rotation: if rate-limited, try alternate Anthropic credentials.
            // The first entry in the list is the highest-priority credential, which
            // is almost certainly what the initial (override_auth=None) call used.
            // Skip it to avoid a guaranteed duplicate rate-limit failure.
            if result.terminal_reason == Some(TerminalReason::RateLimited) {
                let alt_accounts =
                    super::ai_providers::get_all_anthropic_auth_for_claudecode(&config.working_dir);
                let alt_accounts: Vec<_> = alt_accounts.into_iter().skip(1).collect();
                if !alt_accounts.is_empty() {
                    tracing::info!(
                        mission_id = %mission_id,
                        total_accounts = alt_accounts.len(),
                        "Rate limited on primary account; trying alternate credentials"
                    );
                    for (idx, alt_auth) in alt_accounts.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            break;
                        }
                        tracing::info!(
                            mission_id = %mission_id,
                            attempt = idx + 2,
                            auth_type = match &alt_auth {
                                super::ai_providers::ClaudeCodeAuth::ApiKey(_) => "api_key",
                                super::ai_providers::ClaudeCodeAuth::OAuthToken(_) => "oauth_token",
                            },
                            "Rotating to alternate Anthropic account"
                        );
                        result = run_claudecode_turn(
                            &workspace,
                            &mission_work_dir,
                            &effective_msg,
                            config.default_model.as_deref(),
                            effective_agent.as_deref(),
                            mission_id,
                            events_tx.clone(),
                            cancel.clone(),
                            secrets.clone(),
                            &config.working_dir,
                            effective_sid.as_deref(),
                            is_continuation,
                            Some(Arc::clone(&tool_hub)),
                            Some(Arc::clone(&status)),
                            Some(alt_auth),
                        )
                        .await;
                        // Only continue rotating on rate-limit errors.
                        // Non-rate-limit LLM errors (model errors, context
                        // limit, etc.) would fail on every account, so stop
                        // early to avoid masking the real failure.
                        match result.terminal_reason {
                            Some(TerminalReason::RateLimited) => {
                                tracing::info!(
                                    mission_id = %mission_id,
                                    attempt = idx + 2,
                                    "Rate limited; rotating to next account"
                                );
                                continue;
                            }
                            _ => break,
                        }
                    }
                }
            }

            result
        }
        "opencode" => {
            // Use per-workspace CLI execution for all workspace types to ensure
            // native bash + correct filesystem scope.
            run_opencode_turn(
                &workspace,
                &mission_work_dir,
                &convo,
                config.default_model.as_deref(),
                model_effort.as_deref(),
                effective_agent.as_deref(),
                mission_id,
                events_tx.clone(),
                cancel,
                &config.working_dir,
            )
            .await
        }
        "amp" => {
            let api_key = get_amp_api_key_from_config();
            let mut result = run_amp_turn(
                &workspace,
                &mission_work_dir,
                &user_message,
                effective_agent.as_deref(), // Used as mode (smart/rush)
                mission_id,
                events_tx.clone(),
                cancel.clone(),
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                api_key.as_deref(),
            )
            .await;

            // Account rotation: if rate-limited, try alternate Amp API keys.
            if result.terminal_reason == Some(TerminalReason::RateLimited) {
                let alt_keys = super::ai_providers::get_all_amp_api_keys(&config.working_dir);
                if alt_keys.len() > 1 {
                    tracing::info!(
                        mission_id = %mission_id,
                        total_keys = alt_keys.len(),
                        "Amp rate limited; trying alternate API keys"
                    );
                    // Skip the key we already tried (explicit config key or env var fallback)
                    let already_tried = api_key.map(|s| s.to_string()).or_else(|| {
                        std::env::var("AMP_API_KEY")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    });
                    for (idx, alt_key) in alt_keys.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            break;
                        }
                        if Some(&alt_key) == already_tried.as_ref() {
                            continue;
                        }
                        tracing::info!(
                            mission_id = %mission_id,
                            attempt = idx + 2,
                            "Rotating to alternate Amp API key"
                        );
                        result = run_amp_turn(
                            &workspace,
                            &mission_work_dir,
                            &user_message,
                            effective_agent.as_deref(),
                            mission_id,
                            events_tx.clone(),
                            cancel.clone(),
                            &config.working_dir,
                            session_id.as_deref(),
                            is_continuation,
                            Some(&alt_key),
                        )
                        .await;
                        match result.terminal_reason {
                            Some(TerminalReason::RateLimited) => {
                                tracing::info!(
                                    mission_id = %mission_id,
                                    attempt = idx + 2,
                                    "Amp rate limited; rotating to next key"
                                );
                                continue;
                            }
                            _ => break,
                        }
                    }
                }
            }

            result
        }
        "codex" => {
            let mut result = run_codex_turn(
                &workspace,
                &mission_work_dir,
                &convo,
                config.default_model.as_deref(),
                model_effort.as_deref(),
                effective_agent.as_deref(),
                mission_id,
                events_tx.clone(),
                cancel.clone(),
                &config.working_dir,
                session_id.as_deref(),
                None,
            )
            .await;

            // Account rotation: if rate-limited, try alternate OpenAI API keys.
            // The override key is passed directly to write_codex_credentials_for_workspace
            // to avoid mutating the process-global OPENAI_API_KEY env var (which would
            // race with concurrent missions).
            if result.terminal_reason == Some(TerminalReason::RateLimited) {
                let alt_keys =
                    super::ai_providers::get_all_openai_keys_for_codex(&config.working_dir);
                if alt_keys.len() > 1 {
                    tracing::info!(
                        mission_id = %mission_id,
                        total_keys = alt_keys.len(),
                        "Codex rate limited; trying alternate OpenAI API keys"
                    );
                    // The first key in alt_keys is typically the default (from env var
                    // or auth.json), which we already tried above.
                    let already_tried = super::ai_providers::get_openai_api_key_for_codex_default(
                        &config.working_dir,
                    );
                    for (idx, alt_key) in alt_keys.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            break;
                        }
                        // Skip the key we already tried
                        if Some(&alt_key) == already_tried.as_ref() {
                            continue;
                        }
                        tracing::info!(
                            mission_id = %mission_id,
                            attempt = idx + 2,
                            "Rotating to alternate OpenAI API key for Codex"
                        );
                        result = run_codex_turn(
                            &workspace,
                            &mission_work_dir,
                            &convo,
                            config.default_model.as_deref(),
                            model_effort.as_deref(),
                            effective_agent.as_deref(),
                            mission_id,
                            events_tx.clone(),
                            cancel.clone(),
                            &config.working_dir,
                            session_id.as_deref(),
                            Some(&alt_key),
                        )
                        .await;
                        match result.terminal_reason {
                            Some(TerminalReason::RateLimited) => {
                                tracing::info!(
                                    mission_id = %mission_id,
                                    attempt = idx + 2,
                                    "Codex rate limited; rotating to next key"
                                );
                                continue;
                            }
                            _ => break,
                        }
                    }
                }
            }

            result
        }
        _ => {
            // Don't send Error event - the failure will be emitted as an AssistantMessage
            // with success=false by the caller (control.rs), avoiding duplicate messages.
            AgentResult::failure(format!("Unsupported backend: {}", backend_id), 0)
                .with_terminal_reason(TerminalReason::LlmError)
        }
    };

    tracing::info!(
        mission_id = %mission_id,
        success = result.success,
        cost_cents = result.cost_cents,
        model = ?result.model_used,
        terminal_reason = ?result.terminal_reason,
        "Mission turn finished"
    );

    // Clean up old debug files to prevent unbounded disk/memory growth
    // Keep last 20 debug files (each ~17KB) = ~340KB retained
    if let Err(e) = cleanup_old_debug_files(&mission_work_dir, 20) {
        tracing::warn!(
            mission_id = %mission_id,
            error = %e,
            "Failed to clean up old debug files"
        );
    }

    result
}

fn read_backend_configs() -> Option<Vec<serde_json::Value>> {
    let home = std::env::var("HOME").ok()?;

    // Check WORKING_DIR first (for custom deployment paths), then HOME
    let working_dir = std::env::var("WORKING_DIR").ok();

    let mut candidates = vec![];

    // Add WORKING_DIR paths if set
    if let Some(ref wd) = working_dir {
        candidates.push(
            std::path::PathBuf::from(wd)
                .join(".sandboxed-sh")
                .join("backend_config.json"),
        );
    }

    // Add HOME paths
    candidates.push(
        std::path::PathBuf::from(&home)
            .join(".sandboxed-sh")
            .join("backend_config.json"),
    );
    candidates.push(
        std::path::PathBuf::from(&home)
            .join(".sandboxed-sh")
            .join("data")
            .join("backend_configs.json"),
    );

    // Always check /root/.sandboxed-sh as fallback since the dashboard saves config there
    // and Open Agent service may run with a different HOME (e.g., /var/lib/opencode)
    if home != "/root" {
        candidates.push(
            std::path::PathBuf::from("/root")
                .join(".sandboxed-sh")
                .join("backend_config.json"),
        );
        candidates.push(
            std::path::PathBuf::from("/root")
                .join(".sandboxed-sh")
                .join("data")
                .join("backend_configs.json"),
        );
    }

    for path in candidates {
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };
        if let Ok(configs) = serde_json::from_str::<Vec<serde_json::Value>>(&contents) {
            return Some(configs);
        }
    }
    None
}

/// Read a non-empty string setting from a backend's config entry.
fn get_backend_string_setting(backend_id: &str, key: &str) -> Option<String> {
    let configs = read_backend_configs()?;
    for config in configs {
        if config.get("id")?.as_str()? == backend_id {
            if let Some(val) = config
                .get("settings")
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
            {
                if !val.is_empty() {
                    if key == "api_key" {
                        tracing::debug!("Using {} {} from backend config", backend_id, key);
                    } else {
                        tracing::info!("Using {} {} from backend config: {}", backend_id, key, val);
                    }
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Read a boolean setting from a backend's config entry.
fn get_backend_bool_setting(backend_id: &str, key: &str) -> Option<bool> {
    let configs = read_backend_configs()?;
    for config in configs {
        if config.get("id")?.as_str()? == backend_id {
            if let Some(val) = config
                .get("settings")
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_bool())
            {
                tracing::info!("Using {} {} from backend config: {}", backend_id, key, val);
                return Some(val);
            }
        }
    }
    None
}

/// Read API key from Amp backend config file if available.
pub fn get_amp_api_key_from_config() -> Option<String> {
    let key = get_backend_string_setting("amp", "api_key")?;
    if key.starts_with("[REDACTED") || key == "********" {
        return None;
    }
    Some(key)
}

/// Read amp.url from Amp CLI settings file (~/.config/amp/settings.json)
fn get_amp_url_from_settings() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let settings_path = std::path::PathBuf::from(&home)
        .join(".config")
        .join("amp")
        .join("settings.json");

    let contents = std::fs::read_to_string(&settings_path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&contents).ok()?;

    settings
        .get("amp.url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Execute a turn using Claude Code CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
#[allow(clippy::too_many_arguments)]
pub fn run_claudecode_turn<'a>(
    workspace: &'a Workspace,
    work_dir: &'a std::path::Path,
    message: &'a str,
    model: Option<&'a str>,
    agent: Option<&'a str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    secrets: Option<Arc<SecretsStore>>,
    app_working_dir: &'a std::path::Path,
    session_id: Option<&'a str>,
    is_continuation: bool,
    tool_hub: Option<Arc<FrontendToolHub>>,
    status: Option<Arc<RwLock<ControlStatus>>>,
    override_auth: Option<super::ai_providers::ClaudeCodeAuth>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentResult> + Send + 'a>> {
    Box::pin(async move {
        use super::ai_providers::{
            ensure_anthropic_oauth_token_valid, get_anthropic_auth_for_claudecode,
            get_anthropic_auth_from_host_with_expiry, get_anthropic_auth_from_workspace,
            get_workspace_auth_path, refresh_workspace_anthropic_auth, ClaudeCodeAuth,
        };
        use std::collections::HashMap;
        use tokio::time::{Duration, Instant};

        fn describe_pty_exit_status(
            exit_status: &Result<
                Result<portable_pty::ExitStatus, std::io::Error>,
                tokio::task::JoinError,
            >,
        ) -> String {
            match exit_status {
                Ok(Ok(status)) => format!("{:?}", status),
                Ok(Err(err)) => format!("wait error: {}", err),
                Err(err) => format!("join error: {}", err),
            }
        }

        fn classify_claudecode_secret(value: String) -> ClaudeCodeAuth {
            if value.starts_with("sk-ant-oat") {
                ClaudeCodeAuth::OAuthToken(value)
            } else {
                ClaudeCodeAuth::ApiKey(value)
            }
        }

        fn claude_cli_credentials_info(path: &std::path::Path) -> Option<(i64, bool)> {
            let metadata = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => return None,
            };
            if metadata.len() == 0 {
                return None;
            }
            let contents = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => return None,
            };
            let creds: serde_json::Value = match serde_json::from_str(&contents) {
                Ok(v) => v,
                Err(_) => return None,
            };
            let oauth = match creds.get("claudeAiOauth") {
                Some(o) => o,
                None => return None,
            };
            let has_access_token = oauth
                .get("accessToken")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !has_access_token {
                return None;
            }
            let expires_at = oauth
                .get("expiresAt")
                .and_then(|v| v.as_i64())
                .unwrap_or(i64::MAX);
            let has_refresh = oauth
                .get("refreshToken")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            Some((expires_at, has_refresh))
        }

        fn looks_like_claude_cli_credentials(path: &std::path::Path) -> bool {
            let (expires_at, has_refresh) = match claude_cli_credentials_info(path) {
                Some(info) => info,
                None => return false,
            };
            // Check if the access token is expired.
            // Claude Code in --print mode does not auto-refresh OAuth tokens,
            // so we must ensure the token is valid before launching.
            let now_ms = chrono::Utc::now().timestamp_millis();
            // Add 60s buffer to avoid race conditions with near-expiry tokens
            if expires_at < now_ms + 60_000 {
                tracing::warn!(
                    path = %path.display(),
                    expires_at = expires_at,
                    has_refresh = has_refresh,
                    "Claude CLI credentials expired or near-expiry, will use OAuth refresh flow"
                );
                return false;
            }
            true
        }

        fn find_host_claude_cli_credentials() -> Option<std::path::PathBuf> {
            let mut candidates = vec![
                std::path::PathBuf::from("/var/lib/opencode/.claude/.credentials.json"),
                std::path::PathBuf::from("/root/.claude/.credentials.json"),
            ];
            if let Ok(home) = std::env::var("HOME") {
                candidates.push(std::path::PathBuf::from(home).join(".claude/.credentials.json"));
            }

            candidates
                .into_iter()
                .find(|p| looks_like_claude_cli_credentials(p))
        }

        // Prefer the user's Claude CLI login if present, but avoid mutating the global
        // credentials file. We run each mission with a per-mission HOME, and copy the
        // host credentials into the mission directory if needed.
        let mission_creds_path = work_dir.join(".claude").join(".credentials.json");
        if !looks_like_claude_cli_credentials(&mission_creds_path) {
            if let Some(host_creds) = find_host_claude_cli_credentials() {
                if let Some(parent) = mission_creds_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        tracing::warn!(
                            mission_id = %mission_id,
                            path = %parent.display(),
                            error = %e,
                            "Failed to create parent directory for Claude CLI credentials"
                        );
                    }
                }
                match std::fs::copy(&host_creds, &mission_creds_path) {
                    Ok(_) => {
                        tracing::info!(
                            from = %host_creds.display(),
                            to = %mission_creds_path.display(),
                            "Copied Claude CLI credentials into mission directory"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            from = %host_creds.display(),
                            to = %mission_creds_path.display(),
                            error = %e,
                            "Failed to copy Claude CLI credentials into mission directory"
                        );
                    }
                }
            }
        }
        let has_cli_creds = looks_like_claude_cli_credentials(&mission_creds_path);
        if let Some((expires_at, has_refresh)) = claude_cli_credentials_info(&mission_creds_path) {
            tracing::info!(
                mission_id = %mission_id,
                path = %mission_creds_path.display(),
                expires_at = expires_at,
                has_refresh = has_refresh,
                has_cli_creds = has_cli_creds,
                "Claude CLI credential status for mission"
            );
        } else {
            tracing::info!(
                mission_id = %mission_id,
                path = %mission_creds_path.display(),
                has_cli_creds = has_cli_creds,
                "No Claude CLI credentials found for mission"
            );
        }

        // Only refresh OpenCode/Anthropic OAuth tokens if we plan to inject them.
        let oauth_refresh_result = if has_cli_creds {
            tracing::info!(
                mission_id = %mission_id,
                "Using Claude CLI credentials for mission; skipping OAuth refresh injection"
            );
            Ok(())
        } else {
            tracing::info!(
                mission_id = %mission_id,
                "No valid Claude CLI credentials; using OAuth refresh flow"
            );
            // Ensure OAuth tokens are fresh before resolving credentials.
            ensure_anthropic_oauth_token_valid().await
        };
        if let Err(e) = &oauth_refresh_result {
            tracing::warn!("Failed to refresh Anthropic OAuth token: {}", e);
        }

        // Keep a clone of the override credential so recursive continuation
        // calls (tool-result → next turn) keep using the same rotated account.
        let override_auth_for_continuation = override_auth.clone();

        // If an override credential was provided (account rotation), use it directly.
        let api_auth = if let Some(auth) = override_auth {
            tracing::info!(
                mission_id = %mission_id,
                auth_type = match &auth {
                    ClaudeCodeAuth::ApiKey(_) => "api_key",
                    ClaudeCodeAuth::OAuthToken(_) => "oauth_token",
                },
                "Using override credential for account rotation"
            );
            Some(auth)
        } else
        // Try to get API key/OAuth token from Anthropic provider configured for Claude Code backend.
        // For container workspaces, compare workspace auth vs host auth and use the fresher one.
        // If workspace auth is expired, try to refresh it using the refresh token.
        if has_cli_creds {
            None
        } else {
            // For container workspaces, get both workspace and host auth with expiry info
            let mut workspace_auth = if workspace.workspace_type == WorkspaceType::Container {
                get_anthropic_auth_from_workspace(&workspace.path)
            } else {
                None
            };

            let host_auth = get_anthropic_auth_from_host_with_expiry();
            let now = chrono::Utc::now().timestamp_millis();

            // If workspace auth is expired and we have no fresh host auth, try to refresh the workspace auth
            if let Some(ref ws) = workspace_auth {
                let ws_expiry = ws.expires_at.unwrap_or(i64::MAX);
                let ws_expired = ws_expiry < now;
                let host_has_fresh_auth = host_auth
                    .as_ref()
                    .map(|h| h.expires_at.unwrap_or(i64::MAX) > now)
                    .unwrap_or(false);

                if ws_expired && !host_has_fresh_auth {
                    // Workspace auth is expired and no fresh host auth - try to refresh workspace auth
                    tracing::info!(
                        workspace_path = %workspace.path.display(),
                        ws_expiry = ws_expiry,
                        "Workspace auth is expired, attempting to refresh"
                    );
                    match refresh_workspace_anthropic_auth(&workspace.path).await {
                        Ok(refreshed) => {
                            tracing::info!(
                                workspace_path = %workspace.path.display(),
                                "Successfully refreshed workspace Anthropic auth"
                            );
                            workspace_auth = Some(refreshed);
                        }
                        Err(e) => {
                            tracing::warn!(
                                workspace_path = %workspace.path.display(),
                                error = %e,
                                "Failed to refresh workspace auth, will try other sources"
                            );
                            // Clear the stale workspace auth so we don't keep trying
                            workspace_auth = None;
                        }
                    }
                }
            }

            // Choose the fresher auth based on expiry timestamps
            let chosen_auth: Option<ClaudeCodeAuth> = match (&workspace_auth, &host_auth) {
                (Some(ws), Some(host)) => {
                    // Both available - compare expiry timestamps
                    let ws_expiry = ws.expires_at.unwrap_or(i64::MAX); // API keys never expire
                    let host_expiry = host.expires_at.unwrap_or(i64::MAX);

                    // Check if workspace auth is expired
                    let ws_expired = ws_expiry < now;
                    let host_expired = host_expiry < now;

                    if ws_expired && !host_expired {
                        // Workspace auth is expired but host auth is fresh - use host auth
                        // Also delete the stale workspace auth file
                        let ws_auth_path = get_workspace_auth_path(&workspace.path);
                        if ws_auth_path.exists() {
                            tracing::info!(
                                workspace_path = %workspace.path.display(),
                                ws_expiry = ws_expiry,
                                host_expiry = host_expiry,
                                "Workspace auth is expired, using fresher host auth and removing stale workspace auth"
                            );
                            if let Err(e) = std::fs::remove_file(&ws_auth_path) {
                                tracing::warn!(
                                    path = %ws_auth_path.display(),
                                    error = %e,
                                    "Failed to remove stale workspace auth file"
                                );
                            }
                        }
                        Some(host.auth.clone())
                    } else if host_expiry > ws_expiry {
                        // Host auth has later expiry - use it (it was likely just refreshed)
                        tracing::info!(
                            workspace_path = %workspace.path.display(),
                            ws_expiry = ws_expiry,
                            host_expiry = host_expiry,
                            "Using fresher host auth (expires later than workspace auth)"
                        );
                        Some(host.auth.clone())
                    } else {
                        // Workspace auth is fresher or equal - use it
                        tracing::info!(
                            workspace_path = %workspace.path.display(),
                            ws_expiry = ws_expiry,
                            host_expiry = host_expiry,
                            "Using workspace auth"
                        );
                        Some(ws.auth.clone())
                    }
                }
                (Some(ws), None) => {
                    // Only workspace auth available
                    tracing::info!(
                        workspace_path = %workspace.path.display(),
                        "Using Anthropic credentials from container workspace"
                    );
                    Some(ws.auth.clone())
                }
                (None, Some(host)) => {
                    // Only host auth available
                    tracing::info!("Using Anthropic credentials from host");
                    Some(host.auth.clone())
                }
                (None, None) => None,
            };

            // If we found auth from workspace/host comparison, use it
            if let Some(auth) = chosen_auth {
                Some(auth)
            } else if let Some(auth) = get_anthropic_auth_for_claudecode(app_working_dir) {
                tracing::info!("Using Anthropic credentials from provider for Claude Code");
                Some(auth)
            } else {
                // Fall back to secrets vault (legacy support)
                if let Some(ref store) = secrets {
                    match store.get_secret("claudecode", "api_key").await {
                        Ok(key) => {
                            tracing::info!(
                                "Using Claude Code credentials from secrets vault (legacy)"
                            );
                            Some(classify_claudecode_secret(key))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get Claude API key from secrets: {}", e);
                            // Fall back to environment variable
                            std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
                                .ok()
                                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                                .map(classify_claudecode_secret)
                        }
                    }
                } else {
                    std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
                        .ok()
                        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                        .map(classify_claudecode_secret)
                }
            }
        };

        if matches!(api_auth, Some(ClaudeCodeAuth::OAuthToken(_))) {
            if let Err(err) = oauth_refresh_result {
                let err_msg = format!(
                "Anthropic OAuth token refresh failed: {}. Please re-authenticate in Settings → AI Providers.",
                err
            );
                tracing::warn!(mission_id = %mission_id, "{}", err_msg);
                return AgentResult::failure(err_msg, 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        }

        // Fail fast only if neither:
        // - Claude CLI credentials are available (copied into the mission directory), nor
        // - We have explicit API auth to inject via env vars.
        if api_auth.is_none() && !has_cli_creds {
            let err_msg = "No Claude Code credentials detected. Either run `claude /login` on the host, or authenticate in Settings → AI Providers / set CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY.";
            tracing::warn!(mission_id = %mission_id, "{}", err_msg);
            return AgentResult::failure(err_msg.to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }

        // Determine CLI path: prefer backend config, then env var, then default
        let cli_path = get_backend_string_setting("claudecode", "cli_path")
            .or_else(|| std::env::var("CLAUDE_CLI_PATH").ok())
            .unwrap_or_else(|| "claude".to_string());

        // Use stored session_id for conversation persistence.
        // If session_id is None (legacy mission), generate a new one but warn that continuation
        // won't work correctly since the generated ID isn't persisted back to the mission store.
        let session_id = match session_id {
            Some(id) => id.to_string(),
            None => {
                let generated = Uuid::new_v4().to_string();
                tracing::warn!(
                    mission_id = %mission_id,
                    generated_session_id = %generated,
                    "Mission has no stored session_id (legacy mission). Generated temporary ID, but conversation continuation will not work correctly. Consider recreating the mission."
                );
                generated
            }
        };

        let workspace_exec = WorkspaceExec::new(workspace.clone());
        let cli_path =
            match ensure_claudecode_cli_available(&workspace_exec, work_dir, &cli_path).await {
                Ok(path) => path,
                Err(err_msg) => {
                    tracing::error!("{}", err_msg);
                    return AgentResult::failure(err_msg, 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
            };

        // Proactive network connectivity check - fail fast if API is unreachable
        // This catches DNS/network issues immediately instead of waiting for a timeout
        if let Err(err_msg) = check_claudecode_connectivity(&workspace_exec, work_dir).await {
            tracing::error!(mission_id = %mission_id, "{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }

        tracing::info!(
            mission_id = %mission_id,
            session_id = %session_id,
            work_dir = %work_dir.display(),
            workspace_type = ?workspace.workspace_type,
            model = ?model,
            agent = ?agent,
            "Starting Claude Code execution via WorkspaceExec"
        );

        // Check for Claude Code builtin slash commands that need special handling
        let trimmed_message = message.trim();
        let (effective_message, permission_mode) =
            if trimmed_message == "/plan" || trimmed_message.starts_with("/plan ") {
                // /plan triggers plan mode via --permission-mode plan
                let rest = trimmed_message.strip_prefix("/plan").unwrap_or("").trim();
                let msg = if rest.is_empty() {
                    "Please analyze the codebase and create a plan for the task.".to_string()
                } else {
                    rest.to_string()
                };
                (msg, Some("plan"))
            } else {
                (message.to_string(), None)
            };

        // Build CLI arguments
        let mut args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
        ];

        // Add permission mode if a slash command triggered a special mode
        if let Some(mode) = permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.to_string());
        }

        // Skip all permission checks. IS_SANDBOX=1 is set in env vars below
        // to allow --dangerously-skip-permissions even when running as root.
        args.push("--dangerously-skip-permissions".to_string());

        // Ensure per-workspace Claude settings are loaded (Claude CLI may not auto-load .claude in --print mode).
        //
        // Important: `--mcp-config` expects MCP server definitions, but Claude Code 2.1+ treats raw
        // paths (e.g. "/root/work...") as JSON strings and can hang after a parse error. `--settings`
        // reliably loads our `.claude/settings.local.json` file (which includes mcpServers + permissions).
        //
        // For container workspaces, we must translate the path to be relative to the container filesystem.
        let settings_path = work_dir.join(".claude").join("settings.local.json");
        if settings_path.exists() {
            args.push("--settings".to_string());
            // Translate the path for container execution (host path -> container-relative path)
            let translated_path = workspace_exec.translate_path_for_container(&settings_path);
            args.push(translated_path);
        }
        let mcp_config_path = work_dir.join(".claude").join("mcp.json");
        if mcp_config_path.exists() {
            args.push("--mcp-config".to_string());
            let translated_path = workspace_exec.translate_path_for_container(&mcp_config_path);
            args.push(translated_path);
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        // For continuation turns, use --resume to resume existing session.
        // For first turn, use --session-id to create new session with that ID.
        //
        // Important: We use a marker file to track if the session was ever initiated.
        // This prevents "Session ID already in use" errors when a turn is cancelled
        // after the session is created but before any assistant response is recorded.
        // The marker file contains the session ID to prevent cross-mission interference
        // when workspaces are shared (e.g., fallback to workspace-wide directory).
        let session_marker = work_dir.join(".claude-session-initiated");
        let session_was_initiated = session_marker.exists()
            && std::fs::read_to_string(&session_marker)
                .map(|content| content.trim() == session_id)
                .unwrap_or(false);

        // Determine if we should use --resume:
        // We can only resume if the session was actually initiated at THIS work_dir
        // (confirmed by the marker file containing the matching session ID).
        //
        // Having assistant messages in history (is_continuation) is NOT sufficient on its own,
        // because:
        // - Error messages from failed attempts are recorded as assistant messages
        // - The session may have been created at a different HOME (e.g., container root
        //   before per-mission HOME isolation was added)
        // - The session_id may have been reset (e.g., database update after stuck session)
        //
        // Using --resume with a non-existent session causes Claude Code to exit with
        // "No conversation found with session ID: ..." and code 1.
        let use_resume = session_was_initiated;

        if use_resume {
            args.push("--resume".to_string());
            args.push(session_id.clone());
            tracing::debug!(
                mission_id = %mission_id,
                session_id = %session_id,
                is_continuation = is_continuation,
                session_was_initiated = session_was_initiated,
                "Resuming existing Claude Code session"
            );
        } else {
            // Create the marker file BEFORE starting the CLI to prevent races
            if let Err(e) = std::fs::write(&session_marker, &session_id) {
                tracing::warn!(
                    mission_id = %mission_id,
                    error = %e,
                    "Failed to write session marker file"
                );
            }

            args.push("--session-id".to_string());
            args.push(session_id.clone());
            tracing::debug!(
                mission_id = %mission_id,
                session_id = %session_id,
                "Starting new Claude Code session"
            );
        }

        // Skip `--agent general-purpose` because it's the default behaviour in
        // `--print` mode and causes the CLI to hang during "Loading commands and
        // agents" when spawned from a systemd service (missing interactive
        // environment).  Non-default agents (e.g. Bash, Explore, Plan) are still
        // passed through.
        if let Some(a) = agent {
            if a != "general-purpose" {
                args.push("--agent".to_string());
                args.push(a.to_string());
            }
        }

        // Provide the prompt as a positional argument (instead of stdin).
        //
        // In production we have observed cases where piping stdin from the backend results in
        // Claude Code producing no stdout events (even though it creates the session files),
        // leaving missions stuck "Agent is working..." indefinitely.
        args.push("--".to_string());
        args.push(effective_message.clone());

        // Build environment variables
        let mut env: HashMap<String, String> = HashMap::new();
        // Allow --dangerously-skip-permissions when running as root inside containers.
        env.insert("IS_SANDBOX".to_string(), "1".to_string());

        // Run Claude Code with a per-mission HOME to avoid:
        // - clobbering global `~/.claude/.credentials.json`
        // - cross-mission config lock contention inside the shared home dir
        let mission_home = workspace_exec.translate_path_for_container(work_dir);
        let xdg_config_home = work_dir.join(".config");
        let xdg_data_home = work_dir.join(".local").join("share");
        let xdg_state_home = work_dir.join(".local").join("state");
        let xdg_cache_home = work_dir.join(".cache");

        for dir in [
            &xdg_config_home,
            &xdg_data_home,
            &xdg_state_home,
            &xdg_cache_home,
        ] {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::warn!(
                    mission_id = %mission_id,
                    path = %dir.display(),
                    error = %e,
                    "Failed to create per-mission XDG directory"
                );
            }
        }

        env.insert("HOME".to_string(), mission_home);
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            workspace_exec.translate_path_for_container(&xdg_config_home),
        );
        env.insert(
            "XDG_DATA_HOME".to_string(),
            workspace_exec.translate_path_for_container(&xdg_data_home),
        );
        env.insert(
            "XDG_STATE_HOME".to_string(),
            workspace_exec.translate_path_for_container(&xdg_state_home),
        );
        env.insert(
            "XDG_CACHE_HOME".to_string(),
            workspace_exec.translate_path_for_container(&xdg_cache_home),
        );
        let claude_config_dir =
            workspace_exec.translate_path_for_container(&work_dir.join(".claude"));
        env.insert("CLAUDE_CONFIG_DIR".to_string(), claude_config_dir.clone());
        let claude_config_path = format!("{}/settings.json", claude_config_dir);
        env.insert("CLAUDE_CONFIG".to_string(), claude_config_path);

        if let Some(ref auth) = api_auth {
            match auth {
                ClaudeCodeAuth::OAuthToken(token) => {
                    env.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.clone());
                    tracing::debug!(
                        "Injecting OAuth token for Claude CLI authentication (token_len={})",
                        token.len()
                    );
                }
                ClaudeCodeAuth::ApiKey(key) => {
                    env.insert("ANTHROPIC_API_KEY".to_string(), key.clone());
                    tracing::debug!("Using API key for Claude CLI authentication");
                }
            }
        } else if has_cli_creds {
            tracing::debug!("Using Claude CLI credentials from mission directory");
        } else {
            tracing::warn!("No authentication available for Claude Code!");
        }

        // Handle case where cli_path might be a wrapper command like "bun /path/to/claude"
        let (mut program, mut full_args) = if cli_path.contains(' ') {
            let parts: Vec<&str> = cli_path.splitn(2, ' ').collect();
            let program = parts[0].to_string();
            let mut full_args = if parts.len() > 1 {
                vec![parts[1].to_string()]
            } else {
                vec![]
            };
            full_args.extend(args.clone());
            (program, full_args)
        } else {
            (cli_path.clone(), args.clone())
        };

        // Container workaround:
        //
        // Claude Code CLI 2.1.x in our container templates uses Bun APIs in some
        // code paths (e.g. `Bun.which`). When executed under Node it can crash
        // with `ReferenceError: Bun is not defined`, which breaks automations.
        //
        // If Bun is available in the workspace, prefer running Claude via Bun.
        if workspace.workspace_type == WorkspaceType::Container
            && env_var_bool("SANDBOXED_SH_CLAUDECODE_USE_BUN", true)
            && program != "bun"
            && !program.ends_with("/bun")
        {
            let is_claude_program = program == "claude" || program.ends_with("/claude");
            if is_claude_program && command_available(&workspace_exec, work_dir, "bun").await {
                if let Some(claude_path) =
                    resolve_command_path_in_workspace(&workspace_exec, work_dir, &program).await
                {
                    let force_bun = env_var_bool("SANDBOXED_SH_CLAUDECODE_FORCE_BUN", false);
                    let prefers_bun = force_bun
                        || claude_path.contains("/.bun/")
                        || claude_path.contains("/.cache/.bun/")
                        || claude_cli_shebang_contains(
                            &workspace_exec,
                            work_dir,
                            &claude_path,
                            "bun",
                        )
                        .await
                        .unwrap_or(false);
                    let shebang_is_node = claude_cli_shebang_contains(
                        &workspace_exec,
                        work_dir,
                        &claude_path,
                        "node",
                    )
                    .await
                    .unwrap_or(false);

                    if prefers_bun && !shebang_is_node {
                        program = "bun".to_string();
                        full_args.insert(0, claude_path);
                        tracing::info!(
                            mission_id = %mission_id,
                            "Running Claude CLI via bun wrapper (container workspace)"
                        );
                    } else {
                        tracing::debug!(
                            mission_id = %mission_id,
                            claude_path = %claude_path,
                            prefers_bun = prefers_bun,
                            shebang_is_node = shebang_is_node,
                            "Running Claude CLI directly (bun wrapper not required)"
                        );
                    }
                }
            }
        }

        // Use WorkspaceExec to spawn the CLI in the correct workspace context.
        //
        // Claude Code 2.1.x can hang indefinitely when stdout is a pipe (non-tty),
        // even in `--print --output-format stream-json` mode. Running it under a PTY
        // fixes this and restores streaming.
        let mut pty = match workspace_exec
            .spawn_streaming_pty(work_dir, &program, &full_args, env)
            .await
        {
            Ok(child) => child,
            Err(e) => {
                let err_msg = format!("Failed to start Claude CLI: {}", e);
                tracing::error!("{}", err_msg);
                return AgentResult::failure(err_msg, 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        // Keep stdin open - dropping the writer (closing stdin) can cause some Claude CLI
        // agent modes to hang. We pass the prompt via argv so stdin is not needed, but the
        // CLI may check if stdin is open during initialization.
        let _stdin_writer = pty.take_writer();
        tracing::debug!(mission_id = %mission_id, "PTY writer taken (kept alive)");

        let reader = match pty.try_clone_reader() {
            Ok(r) => {
                tracing::debug!(mission_id = %mission_id, "PTY reader cloned successfully");
                r
            }
            Err(e) => {
                pty.kill();
                let err_msg = format!("Failed to capture Claude PTY output: {}", e);
                tracing::error!("{}", err_msg);
                return AgentResult::failure(err_msg, 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        };

        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let reader_mission_id = mission_id.to_string();
        let reader_handle = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            tracing::debug!(mission_id = %reader_mission_id, "PTY reader task started, waiting for first read");
            let mut buf_reader = std::io::BufReader::new(reader);
            let mut buf: Vec<u8> = Vec::with_capacity(8192);
            let mut line_count = 0u64;
            loop {
                buf.clear();
                match buf_reader.read_until(b'\n', &mut buf) {
                    Ok(0) => {
                        tracing::debug!(
                            mission_id = %reader_mission_id,
                            total_lines = line_count,
                            "PTY reader got EOF"
                        );
                        break;
                    }
                    Ok(n) => {
                        line_count += 1;
                        if line_count <= 3 {
                            tracing::debug!(
                                mission_id = %reader_mission_id,
                                bytes = n,
                                line_num = line_count,
                                "PTY reader got line"
                            );
                        }
                        let s = String::from_utf8_lossy(&buf).to_string();
                        if line_tx.send(s).is_err() {
                            tracing::debug!(
                                mission_id = %reader_mission_id,
                                "PTY reader: channel closed"
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            mission_id = %reader_mission_id,
                            error = %e,
                            total_lines = line_count,
                            "PTY reader error"
                        );
                        break;
                    }
                }
            }
        });

        let mut non_json_output: Vec<String> = Vec::new();

        // Track tool calls for result mapping
        let mut pending_tools: HashMap<String, String> = HashMap::new();
        let mut total_cost_usd = 0.0f64;
        let mut final_result = String::new();
        let mut had_error = false;

        // Track content block types and accumulated content for Claude Code streaming
        // This is needed because Claude sends incremental deltas that need to be accumulated
        let mut block_types: HashMap<u32, String> = HashMap::new();
        let mut thinking_buffer: HashMap<u32, String> = HashMap::new();
        let mut text_buffer: HashMap<u32, String> = HashMap::new();
        let mut active_thinking_index: Option<u32> = None; // Track which thinking block is active
        let mut finalized_thinking_indices: std::collections::HashSet<u32> =
            std::collections::HashSet::new(); // Blocks already sent done:true during streaming
        let mut last_text_len: usize = 0; // Track last emitted text length for streaming text deltas
        let mut thinking_emitted = false;

        let mut saw_non_init_event = false;
        let startup_timeout = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_STARTUP_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(20),
        );
        let idle_timeout = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(600),
        );
        let startup_deadline = Instant::now() + startup_timeout;
        let mut idle_deadline = Instant::now() + idle_timeout;

        // Process events until completion or cancellation
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!(mission_id = %mission_id, "Claude Code execution cancelled, killing process");
                    // Kill the process to stop consuming API resources
                    pty.kill();
                    reader_handle.abort();
                    return AgentResult::failure("Cancelled".to_string(), 0)
                        .with_terminal_reason(TerminalReason::Cancelled);
                }
                _ = tokio::time::sleep_until(startup_deadline), if !saw_non_init_event => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        non_json_lines = non_json_output.len(),
                        non_json_sample = ?non_json_output.first(),
                        "Claude Code startup timeout - no stream events received"
                    );
                    pty.kill();
                    reader_handle.abort();
                    let mut msg = "Claude Code produced no stream events after startup timeout. The Claude CLI started but did not emit any stream-json events.".to_string();
                    msg.push_str("\n\nThis can happen when resuming an old/stuck Claude session or when the CLI hangs during initialization.");
                    if !non_json_output.is_empty() {
                        msg.push_str(&format!(
                            "\n\nNon-JSON output captured ({} lines):\n{}",
                            non_json_output.len(),
                            non_json_output.join("\n")
                        ));
                    }
                    return AgentResult::failure(msg, 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
                _ = tokio::time::sleep_until(idle_deadline), if saw_non_init_event => {
                    pty.kill();
                    reader_handle.abort();
                    return AgentResult::failure(
                        "Claude Code produced no output for an extended period and was terminated (idle timeout).".to_string(),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::LlmError);
                }
                line_opt = line_rx.recv() => {
                    let Some(raw_line) = line_opt else {
                        // EOF - PTY closed
                        break;
                    };

                    idle_deadline = Instant::now() + idle_timeout;

                    let raw_line = raw_line.trim_end_matches(&['\r', '\n'][..]);
                    let cleaned = strip_ansi_codes(raw_line);
                    let line = cleaned.trim();
                    if line.is_empty() {
                        continue;
                    }

                    if !line.starts_with('{') {
                        // Preserve a small excerpt for diagnostics on "no output" failures.
                        if non_json_output.len() < 20 {
                            non_json_output.push(if line.len() > 200 {
                                let end = safe_truncate_index(line, 200);
                                format!("{}...", &line[..end])
                            } else {
                                line.to_string()
                            });
                        }
                        continue;
                    }

                    let claude_event: ClaudeEvent = match serde_json::from_str(line) {
                        Ok(event) => event,
                        Err(e) => {
                            tracing::warn!(
                                mission_id = %mission_id,
                                "Failed to parse Claude event: {} - line: {}",
                                e,
                                if line.len() > 200 {
                                    let end = safe_truncate_index(line, 200);
                                    format!("{}...", &line[..end])
                                } else {
                                    line.to_string()
                                }
                            );
                            continue;
                        }
                    };

                    if !matches!(claude_event, ClaudeEvent::System(_)) {
                        saw_non_init_event = true;
                    }

                            match claude_event {
                                ClaudeEvent::System(sys) => {
                                    tracing::debug!(
                                        "Claude session init: session_id={}, model={:?}",
                                        sys.session_id, sys.model
                                    );
                                }
                                ClaudeEvent::StreamEvent(wrapper) => {
                                    match wrapper.event {
                                        StreamEvent::ContentBlockDelta { index, delta } => {
                                            let block_type = block_types
                                                .get(&index)
                                                .map(|value| value.as_str());
                                            let is_thinking_block =
                                                matches!(block_type, Some("thinking"));
                                            // Check the delta type to determine where to route content
                                            // "thinking_delta" -> thinking panel (uses delta.thinking field)
                                            // "text_delta" -> text output (uses delta.text field)
                                            if delta.delta_type == "thinking_delta"
                                                || (is_thinking_block
                                                    && delta.delta_type == "text_delta")
                                            {
                                                // For thinking deltas, check both `thinking` and `text` fields
                                                // Extended thinking uses `thinking`, but some versions use `text`
                                                let thinking_text = delta.thinking.or(delta.text.clone());
                                                if let Some(thinking_content) = thinking_text {
                                                    if !thinking_content.is_empty() {
                                                        // If a new thinking block started, finalize the previous one
                                                        if let Some(prev_idx) = active_thinking_index {
                                                            if prev_idx != index {
                                                                let _ = events_tx.send(AgentEvent::Thinking {
                                                                    content: String::new(),
                                                                    done: true,
                                                                    mission_id: Some(mission_id),
                                                                });
                                                                finalized_thinking_indices.insert(prev_idx);
                                                            }
                                                        }
                                                        active_thinking_index = Some(index);

                                                        // Accumulate thinking content per block
                                                        let buffer = thinking_buffer.entry(index).or_default();
                                                        buffer.push_str(&thinking_content);

                                                        // Send this block's accumulated content
                                                        let _ = events_tx.send(AgentEvent::Thinking {
                                                            content: buffer.clone(),
                                                            done: false,
                                                            mission_id: Some(mission_id),
                                                        });
                                                        thinking_emitted = true;
                                                    }
                                                }
                                            } else if delta.delta_type == "text_delta" {
                                                // For text deltas, content is in the `text` field
                                                if let Some(text) = delta.text {
                                                    if !text.is_empty() {
                                                        // Accumulate text content (will be used for final response)
                                                        let buffer = text_buffer.entry(index).or_default();
                                                        buffer.push_str(&text);

                                                        // Stream text deltas similar to thinking panel
                                                        // This allows users to see tool use descriptions as they're generated
                                                        let total_len = text_buffer.values().map(|s| s.len()).sum::<usize>();
                                                        if total_len > last_text_len {
                                                            let accumulated: String = text_buffer.values().cloned().collect::<Vec<_>>().join("");
                                                            last_text_len = total_len;

                                                            let _ = events_tx.send(AgentEvent::TextDelta {
                                                                content: accumulated,
                                                                mission_id: Some(mission_id),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                            // Ignore other delta types (e.g., input_json_delta for tool use)
                                        }
                                        StreamEvent::ContentBlockStart { index, content_block } => {
                                            // Track the block type so we know how to handle deltas
                                            block_types.insert(index, content_block.block_type.clone());

                                            if content_block.block_type == "tool_use" {
                                                if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                                                    pending_tools.insert(id, name);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                ClaudeEvent::Assistant(evt) => {
                                    for (content_idx, block) in evt.message.content.into_iter().enumerate() {
                                        let content_idx = content_idx as u32;
                                        match block {
                                            ContentBlock::Text { text } => {
                                                // Text content is the final assistant response
                                                // Don't send as Thinking - it will be in the final AssistantMessage
                                                if !text.is_empty() {
                                                    if !thinking_emitted {
                                                        if let Some((thought, cleaned)) =
                                                            extract_thought_line(&text)
                                                        {
                                                            let _ = events_tx.send(
                                                                AgentEvent::Thinking {
                                                                    content: thought,
                                                                    done: true,
                                                                    mission_id: Some(mission_id),
                                                                },
                                                            );
                                                            thinking_emitted = true;
                                                            final_result = cleaned;
                                                        } else {
                                                            final_result = text;
                                                        }
                                                    } else {
                                                        final_result = text;
                                                    }
                                                }
                                            }
                                            ContentBlock::ToolUse { id, name, input } => {
                                                pending_tools.insert(id.clone(), name.clone());
                                                let _ = events_tx.send(AgentEvent::ToolCall {
                                                    tool_call_id: id.clone(),
                                                    name: name.clone(),
                                                    args: input.clone(),
                                                    mission_id: Some(mission_id),
                                                });

                                                if name == "question" || name == "AskUserQuestion" || name.starts_with("ui_") {
                                                    if let Some(ref hub) = tool_hub {
                                                        tracing::info!(
                                                            mission_id = %mission_id,
                                                            tool_call_id = %id,
                                                            tool_name = %name,
                                                            "Frontend tool detected, pausing for user input"
                                                        );
                                                        let hub = Arc::clone(hub);
                                                        if let Some(ref status_ref) = status {
                                                            set_control_state_for_mission(
                                                                status_ref,
                                                                &events_tx,
                                                                mission_id,
                                                                ControlRunState::WaitingForTool,
                                                            )
                                                            .await;
                                                        }
                                                        let rx = hub.register(id.clone()).await;

                                                        pty.kill();
                                                        reader_handle.abort();

                                                        let answer = tokio::select! {
                                                            _ = cancel.cancelled() => {
                                                                return AgentResult::failure("Cancelled".to_string(), 0)
                                                                    .with_terminal_reason(TerminalReason::Cancelled);
                                                            }
                                                            res = rx => {
                                                                match res {
                                                                    Ok(v) => v,
                                                                    Err(_) => {
                                                                        return AgentResult::failure(
                                                                            "Frontend tool result channel closed".to_string(), 0
                                                                        ).with_terminal_reason(TerminalReason::LlmError);
                                                                    }
                                                                }
                                                            }
                                                        };

                                                        if let Some(ref status_ref) = status {
                                                            set_control_state_for_mission(
                                                                status_ref,
                                                                &events_tx,
                                                                mission_id,
                                                                ControlRunState::Running,
                                                            )
                                                            .await;
                                                        }
                                                        let _ = events_tx.send(AgentEvent::ToolResult {
                                                            tool_call_id: id.clone(),
                                                            name: name.clone(),
                                                            result: answer.clone(),
                                                            mission_id: Some(mission_id),
                                                        });

                                                        let answer_text = if let Some(answers) = answer.get("answers") {
                                                            answers.to_string()
                                                        } else {
                                                            answer.to_string()
                                                        };

                                                        return run_claudecode_turn(
                                                            workspace,
                                                            work_dir,
                                                            &answer_text,
                                                            model,
                                                            agent,
                                                            mission_id,
                                                            events_tx,
                                                            cancel,
                                                            secrets,
                                                            app_working_dir,
                                                            Some(&session_id),
                                                            true,
                                                            tool_hub,
                                                            status,
                                                            override_auth_for_continuation,
                                                        ).await;
                                                    }
                                                }
                                            }
                                            ContentBlock::Thinking { thinking } => {
                                                // Only send done:true for the last active thinking block.
                                                // Earlier blocks were already finalized during streaming
                                                // (via the block-transition mechanism) and re-sending them
                                                // causes duplicate items in the frontend thinking panel.
                                                if !thinking.is_empty() && !finalized_thinking_indices.contains(&content_idx) {
                                                    let _ = events_tx.send(AgentEvent::Thinking {
                                                        content: thinking,
                                                        done: true,
                                                        mission_id: Some(mission_id),
                                                    });
                                                    thinking_emitted = true;
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                    // Reset per-turn accumulation state so the next turn
                                    // starts fresh (block indices restart from 0 each turn)
                                    thinking_buffer.clear();
                                    text_buffer.clear();
                                    active_thinking_index = None;
                                    finalized_thinking_indices.clear();
                                    last_text_len = 0;
                                    block_types.clear();
                                    thinking_emitted = false;
                                }
                                ClaudeEvent::User(evt) => {
                                    for block in evt.message.content {
                                        if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                                            // Get tool name and remove from pending (tool is now complete)
                                            let name = pending_tools
                                                .remove(&tool_use_id)
                                                .unwrap_or_else(|| "unknown".to_string());

                                            // Convert content to string representation (handles both text and image results)
                                            let content_str = content.to_string_lossy();

                                            let result_value = if let Some(ref extra) = evt.tool_use_result {
                                                serde_json::json!({
                                                    "content": content_str,
                                                    "stdout": extra.stdout(),
                                                    "stderr": extra.stderr(),
                                                    "is_error": is_error,
                                                })
                                            } else {
                                                serde_json::Value::String(content_str)
                                            };

                                            let _ = events_tx.send(AgentEvent::ToolResult {
                                                tool_call_id: tool_use_id,
                                                name,
                                                result: result_value,
                                                mission_id: Some(mission_id),
                                            });
                                        }
                                    }
                                }
                                ClaudeEvent::Result(res) => {
                                    if let Some(cost) = res.total_cost_usd {
                                        total_cost_usd = cost;
                                    }
                                    // Check for errors: explicit error flags OR embedded API error payloads.
                                    //
                                    // Note: Claude Code may populate error details in `error` / `message`
                                    // fields (not just `result`). Use `error_message()` for best-effort
                                    // extraction.
                                    let error_msg = res.error_message();
                                    let looks_like_api_error = error_msg.starts_with("API Error:")
                                        || error_msg.contains("\"type\":\"error\"")
                                        || error_msg.contains("\"type\":\"overloaded_error\"")
                                        || error_msg.contains("\"type\":\"api_error\"");

                                    if res.is_error || res.subtype == "error" || looks_like_api_error {
                                        had_error = true;
                                        // Don't send an Error event here - let the failure propagate
                                        // through the AgentResult. control.rs will emit an AssistantMessage
                                        // with success=false which the UI displays as a failure message.
                                        // Sending Error here would cause duplicate messages.
                                        final_result = error_msg;
                                    } else if let Some(result) = res.result {
                                        final_result = result;
                                    }
                                    tracing::info!(
                                        mission_id = %mission_id,
                                        cost_usd = total_cost_usd,
                                        "Claude Code execution completed"
                                    );
                                    break;
                                }
                            }
                }
            }
        }

        // Wait for child process to finish and clean up.
        tracing::debug!(
            mission_id = %mission_id,
            "Event loop completed, waiting for Claude Code process"
        );
        let exit_status = tokio::task::spawn_blocking(move || {
            let mut pty = pty;
            pty.wait()
        })
        .await;
        tracing::debug!(
            mission_id = %mission_id,
            exit_status = ?exit_status,
            "Claude Code process exited"
        );

        // Ensure the PTY reader task stops (it should naturally end after process exit).
        let _ = reader_handle.await;

        // Convert cost from USD to cents
        let cost_cents = (total_cost_usd * 100.0) as u64;

        // If no final result from Assistant or Result events, use accumulated text buffer
        // This handles plan mode and other cases where text is streamed incrementally
        if final_result.trim().is_empty() && !text_buffer.is_empty() {
            // Sort by content block index to ensure correct ordering (HashMap iteration is non-deterministic)
            let mut sorted_entries: Vec<_> = text_buffer.iter().collect();
            sorted_entries.sort_by_key(|(idx, _)| *idx);
            final_result = sorted_entries
                .into_iter()
                .map(|(_, text)| text.clone())
                .collect::<Vec<_>>()
                .join("");
            tracing::debug!(
                mission_id = %mission_id,
                "Using accumulated text buffer as final result ({} chars)",
                final_result.len()
            );
        }

        if final_result.trim().is_empty() && !had_error {
            had_error = true;
            if !non_json_output.is_empty() {
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = ?exit_status,
                    "Claude Code produced no parseable JSON output"
                );
                final_result = format!(
                    "Claude Code produced no parseable output. Last output: {}",
                    non_json_output.join(" | ")
                );
            } else {
                let exit_summary = describe_pty_exit_status(&exit_status);
                let mut message = format!(
                    "Claude Code produced no output. Exit status: {}.",
                    exit_summary
                );
                if exit_summary.contains("signal: Some(\"Killed\")") {
                    message.push_str(
                        " The process was killed by the OS (often OOM or sandbox limits).",
                    );
                }
                message.push_str(" Check CLI installation or authentication.");
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = ?exit_status,
                    "Claude Code produced no output"
                );
                final_result = message;
            }
        }

        // If Claude reported an error but didn't provide a useful message, fall back to raw output.
        if had_error
            && (final_result.trim().is_empty() || final_result.trim() == "Unknown error")
            && !non_json_output.is_empty()
        {
            tracing::warn!(
                mission_id = %mission_id,
                exit_status = ?exit_status,
                "Claude Code failed with empty/generic error; using raw output excerpt"
            );
            final_result = format!("Claude Code error: {}", non_json_output.join(" | "));
        }

        let mut result = if had_error {
            // Detect rate limit / overloaded errors for account rotation.
            //
            // We check for specific Anthropic error types and HTTP status codes.
            // Using "overloaded_error" rather than bare "overloaded" to avoid
            // false positives from tool output or user content.
            let reason = if is_rate_limited_error(&final_result) {
                TerminalReason::RateLimited
            } else {
                TerminalReason::LlmError
            };
            AgentResult::failure(final_result, cost_cents).with_terminal_reason(reason)
        } else {
            AgentResult::success(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::Completed)
        };
        if let Some(model) = model {
            result = result.with_model(model.to_string());
        }
        result
    }) // end Box::pin(async move { ... })
}

/// Read CLI path for opencode from backend config file if available.
fn workspace_path_for_env(
    workspace: &Workspace,
    host_path: &std::path::Path,
) -> std::path::PathBuf {
    if workspace.workspace_type == workspace::WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        if let Ok(rel) = host_path.strip_prefix(&workspace.path) {
            return std::path::PathBuf::from("/").join(rel);
        }
    }
    host_path.to_path_buf()
}

fn strip_ansi_codes(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    if !bytes
        .iter()
        .any(|byte| *byte == 0x1b || *byte == 0x9b || is_disallowed_control(*byte))
    {
        return Cow::Borrowed(input);
    }

    let mut cleaned = String::with_capacity(input.len());
    let mut last_copy = 0;
    let mut idx = 0;

    while idx < bytes.len() {
        match bytes[idx] {
            0x1b => {
                cleaned.push_str(&input[last_copy..idx]);
                idx = consume_escape_sequence(bytes, idx);
                last_copy = idx;
            }
            0x9b => {
                cleaned.push_str(&input[last_copy..idx]);
                idx = consume_csi_sequence(bytes, idx + 1);
                last_copy = idx;
            }
            byte if is_disallowed_control(byte) => {
                cleaned.push_str(&input[last_copy..idx]);
                idx += 1;
                last_copy = idx;
            }
            _ => idx += 1,
        }
    }

    cleaned.push_str(&input[last_copy..]);
    Cow::Owned(cleaned)
}

fn is_disallowed_control(byte: u8) -> bool {
    matches!(byte, 0x00..=0x08 | 0x0b | 0x0c | 0x0d | 0x0e..=0x1f | 0x7f)
}

fn consume_escape_sequence(bytes: &[u8], esc_idx: usize) -> usize {
    let len = bytes.len();
    let idx = esc_idx + 1;
    if idx >= len {
        return len;
    }

    match bytes[idx] {
        b'[' => consume_csi_sequence(bytes, idx + 1),
        b']' => consume_osc_sequence(bytes, idx + 1),
        b'P' | b'^' | b'_' => consume_st_sequence(bytes, idx + 1),
        _ => (esc_idx + 2).min(len),
    }
}

fn consume_csi_sequence(bytes: &[u8], mut idx: usize) -> usize {
    let len = bytes.len();
    while idx < len {
        let byte = bytes[idx];
        if (0x40..=0x7e).contains(&byte) {
            return idx + 1;
        }
        idx += 1;
    }
    len
}

fn consume_osc_sequence(bytes: &[u8], mut idx: usize) -> usize {
    let len = bytes.len();
    while idx < len {
        match bytes[idx] {
            0x07 => return idx + 1,
            0x1b if idx + 1 < len && bytes[idx + 1] == b'\\' => return idx + 2,
            _ => idx += 1,
        }
    }
    len
}

fn consume_st_sequence(bytes: &[u8], mut idx: usize) -> usize {
    let len = bytes.len();
    while idx < len {
        if bytes[idx] == 0x1b && idx + 1 < len && bytes[idx + 1] == b'\\' {
            return idx + 2;
        }
        idx += 1;
    }
    len
}

const OPENCODE_SESSION_KEYS: [&[u8]; 4] =
    [b"session id:", b"session:", b"session_id:", b"session="];

fn parse_opencode_session_token(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut end = 0;
    for (idx, byte) in bytes.iter().enumerate() {
        match byte {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' => {
                end = idx + 1;
            }
            _ => break,
        }
    }

    if end == 0 {
        return None;
    }

    let token = &value[..end];
    if token.starts_with("ses_") || token.len() >= 8 {
        Some(token)
    } else {
        None
    }
}

fn opencode_session_token_from_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let bytes = trimmed.as_bytes();
    for key in OPENCODE_SESSION_KEYS {
        if let Some(idx) = find_ascii_case_insensitive(bytes, key) {
            let rest = trimmed[idx + key.len()..].trim();
            if let Some(token) = parse_opencode_session_token(rest) {
                return Some(token);
            }
        }
    }

    None
}

/// Install a lightweight `opencode` wrapper script that intercepts `opencode serve` commands
/// and overrides the `--port` argument using the `OPENCODE_SERVER_PORT` environment variable.
///
/// oh-my-opencode v3 is a compiled binary that always calls `opencode serve --port=4096`.
/// Patching the JS source files has no effect on the binary. This wrapper sits at a higher
/// PATH priority and intercepts the `serve` call to use the allocated port instead.
fn install_opencode_serve_port_wrapper(
    env: &mut HashMap<String, String>,
    workspace: &Workspace,
    port: &str,
) -> bool {
    // Only needed when a non-default port override is required
    if port == "4096" || port == "0" || port.is_empty() {
        return false;
    }

    // Determine the wrapper directory.
    // For containers: use /root/.sandboxed-sh-bin (NOT /tmp) because nspawn mounts
    // a fresh tmpfs over /tmp, hiding anything we write to the container rootfs.
    let (wrapper_dir_host, wrapper_dir_env) = if workspace.workspace_type
        == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        (
            workspace.path.join("root").join(".sandboxed-sh-bin"),
            "/root/.sandboxed-sh-bin".to_string(),
        )
    } else {
        (
            std::path::PathBuf::from("/tmp/.sandboxed-sh-bin"),
            "/tmp/.sandboxed-sh-bin".to_string(),
        )
    };

    if let Err(e) = std::fs::create_dir_all(&wrapper_dir_host) {
        tracing::warn!("Failed to create opencode wrapper dir: {}", e);
        return false;
    }

    // The wrapper script: intercepts `opencode serve` and overrides --port
    // Note: We exclude our wrapper directory from PATH when searching for the real binary
    // to avoid finding ourselves in an infinite loop.
    let wrapper_script = r#"#!/bin/sh
# opencode serve port override wrapper (installed by Open Agent)
WRAPPER_DIR="$(cd "$(dirname "$0")" && pwd)"
CLEAN_PATH="$(echo "$PATH" | tr ':' '\n' | grep -v "^$WRAPPER_DIR$" | tr '\n' ':' | sed 's/:$//')"
REAL_OPENCODE="$(PATH="$CLEAN_PATH" command -v opencode 2>/dev/null || echo /usr/local/bin/opencode)"
if [ -n "$OPENCODE_SERVER_PORT" ] && [ "$1" = "serve" ]; then
  shift
  new_args=""
  for arg in "$@"; do
    case "$arg" in
      --port=*) ;;
      *) new_args="$new_args $arg" ;;
    esac
  done
  exec "$REAL_OPENCODE" serve --port="$OPENCODE_SERVER_PORT" $new_args
fi
exec "$REAL_OPENCODE" "$@"
"#;

    let wrapper_path = wrapper_dir_host.join("opencode");
    if let Err(e) = std::fs::write(&wrapper_path, wrapper_script) {
        tracing::warn!("Failed to write opencode wrapper: {}", e);
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&wrapper_path, std::fs::Permissions::from_mode(0o755));
    }

    // Prepend the wrapper directory to PATH so it takes priority over the real binary
    let current = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let new_path = if current.is_empty() {
        wrapper_dir_env.clone()
    } else {
        format!("{}:{}", wrapper_dir_env, current)
    };
    env.insert("PATH".to_string(), new_path);

    tracing::debug!(
        "Installed opencode serve port wrapper at {} (port={})",
        wrapper_dir_env,
        port
    );
    true
}

fn prepend_opencode_bin_to_path(env: &mut HashMap<String, String>, workspace: &Workspace) {
    let home = if workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        "/root".to_string()
    } else {
        home_dir()
    };
    let bin_dir = format!("{}/.opencode/bin", home);

    let current = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let already = current.split(':').any(|p| p == bin_dir);
    if !already {
        let next = if current.is_empty() {
            bin_dir.clone()
        } else {
            format!("{}:{}", bin_dir, current)
        };
        env.insert("PATH".to_string(), next);
    }
}

fn extract_opencode_session_id(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(opencode_session_token_from_line)
        .map(ToOwned::to_owned)
}

/// Returns true if the line is an OpenCode runner/status banner (not model output).
///
/// oh-my-opencode writes a fixed set of status lines to stdout. We filter these
/// so they don't pollute `final_result` (which should only contain model text).
///
/// The patterns below are deliberately tight — each matches a known runner status
/// line prefix rather than a bare English word. Using broad substrings like
/// `contains("completed")` would silently drop model responses that happen to
/// contain that word (e.g. "Task completed successfully"), which is a critical
/// correctness bug when the SSE path is unavailable and stdout is the only source.
fn is_opencode_banner_line(line: &str) -> bool {
    const PREFIXES: [&[u8]; 11] = [
        b"starting opencode server",
        b"opencode server started",
        b"auto-selected port",
        b"using port",
        b"server listening",
        b"sending prompt",
        b"waiting for completion",
        b"all tasks completed",
        b"event stream did not close",
        b"continuing shutdown",
        b"[run]",
    ];

    let bytes = line.as_bytes();
    PREFIXES
        .iter()
        .any(|needle| starts_with_ascii_case_insensitive(bytes, needle))
        || opencode_session_token_from_line(line).is_some()
}

fn starts_with_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }

    haystack[..needle.len()]
        .iter()
        .zip(needle.iter())
        .all(|(&left, &right)| ascii_lower(left) == ascii_lower(right))
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() || needle.is_empty() {
        return None;
    }

    for idx in 0..=haystack.len() - needle.len() {
        if starts_with_ascii_case_insensitive(&haystack[idx..], needle) {
            return Some(idx);
        }
    }
    None
}

#[inline]
fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    find_ascii_case_insensitive(haystack.as_bytes(), needle.as_bytes()).is_some()
}

#[inline]
fn ascii_lower(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte + 32,
        _ => byte,
    }
}

fn is_rate_limited_error(message: &str) -> bool {
    const RATE_LIMIT_MARKERS: [&str; 9] = [
        "overloaded_error",
        "rate limit",
        "rate_limit",
        "resource_exhausted",
        "too many requests",
        "error: 429",
        "error: 529",
        "status code: 429",
        "status code: 529",
    ];

    RATE_LIMIT_MARKERS
        .iter()
        .any(|needle| contains_ascii_case_insensitive(message, needle))
}

fn strip_opencode_banner_lines(output: &str) -> Cow<'_, str> {
    let no_ansi = strip_ansi_codes(output);
    let source = no_ansi.as_ref();
    let has_banner = source.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty() && is_opencode_banner_line(trimmed)
    });
    if !has_banner {
        return no_ansi;
    }

    let mut result = String::with_capacity(source.len());
    let mut wrote_line = false;
    for line in source.lines().filter(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || !is_opencode_banner_line(trimmed)
    }) {
        if wrote_line {
            result.push('\n');
        }
        result.push_str(line);
        wrote_line = true;
    }
    Cow::Owned(result)
}

fn sanitized_opencode_stdout(output: &str) -> Cow<'_, str> {
    strip_opencode_banner_lines(output)
}

fn is_opencode_exit_status_placeholder(output: &str) -> bool {
    output
        .lines()
        .next()
        .map(|line| {
            line.trim_start()
                .starts_with("OpenCode CLI exited with status:")
        })
        .unwrap_or(false)
}

fn opencode_output_needs_fallback(output: &str) -> bool {
    let sanitized = sanitized_opencode_stdout(output);
    sanitized.trim().is_empty() || is_opencode_exit_status_placeholder(sanitized.as_ref())
}

fn summarize_recent_opencode_stderr(lines: &std::collections::VecDeque<String>) -> Option<String> {
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_opencode_banner_line(trimmed) {
            continue;
        }

        let lower = trimmed.to_lowercase();
        if lower.contains("server.heartbeat")
            || lower.contains("server.connected")
            || lower.contains("server.listening")
            || lower.contains("message.updated")
            || lower.contains("message.part.updated")
            || lower.contains("session.status: busy")
            || lower.contains("session.status: idle")
            || (lower.contains("using") && lower.contains("skill") && !lower.contains("error"))
        {
            continue;
        }

        const MAX_LEN: usize = 300;
        if trimmed.chars().count() <= MAX_LEN {
            return Some(trimmed.to_string());
        }
        let mut truncated: String = trimmed.chars().take(MAX_LEN).collect();
        truncated.push_str("...");
        return Some(truncated);
    }
    None
}

/// Returns true if the output looks like a raw tool-call JSON fragment rather
/// than a genuine assistant text response. This catches the case (issue #148)
/// where the model emitted a tool call but no final text response, and the
/// tool-call JSON ended up in `final_result` via a TextDelta or stdout path.
///
/// We check each non-empty, non-banner line: if every such line parses as a
/// JSON object containing tool-call markers (`name` + `arguments`/`input`,
/// or `type` == `function_call`/`tool_use`/`tool-call`), the output is
/// considered tool-call-only and should not be returned as assistant text.
fn is_tool_call_only_output(output: &str) -> bool {
    let sanitized = sanitized_opencode_stdout(output);
    let mut saw_candidate = false;

    for raw_line in sanitized.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        saw_candidate = true;

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(obj) = json.as_object() {
                let is_type_tool = obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(|t| {
                        t == "function_call"
                            || t == "tool_use"
                            || t == "tool-call"
                            || t == "tool_call"
                    })
                    .unwrap_or(false);

                let has_name = obj.contains_key("name");
                let has_args = obj.contains_key("arguments") || obj.contains_key("input");
                if is_type_tool || (has_name && has_args) {
                    continue;
                }
            }
        }

        return false; // Non-tool JSON or plain text means we have a real answer
    }

    saw_candidate // true only if at least one non-banner, non-empty line existed
}

fn allocate_opencode_server_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok().map(|addr| addr.port()))
}

fn host_oh_my_opencode_config_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let base = std::path::PathBuf::from(home)
            .join(".config")
            .join("opencode");
        candidates.push(base.join("oh-my-opencode.json"));
        candidates.push(base.join("oh-my-opencode.jsonc"));
    }
    candidates
}

fn omo_config_all_fallback(value: &serde_json::Value) -> bool {
    let agents = match value.get("agents").and_then(|v| v.as_object()) {
        Some(agents) => agents,
        None => return false,
    };
    let mut saw_model = false;
    for agent in agents.values() {
        if let Some(model) = agent.get("model").and_then(|v| v.as_str()) {
            saw_model = true;
            if !model.contains("glm-4.7-free") {
                return false;
            }
        }
    }
    saw_model
}

fn host_oh_my_opencode_config_is_fallback() -> Option<bool> {
    for candidate in host_oh_my_opencode_config_candidates() {
        if !candidate.exists() {
            continue;
        }
        let contents = std::fs::read_to_string(&candidate).ok()?;
        let parsed = serde_json::from_str::<serde_json::Value>(&contents)
            .or_else(|_| {
                let stripped = strip_jsonc_comments(&contents);
                serde_json::from_str::<serde_json::Value>(&stripped)
            })
            .ok();
        if let Some(value) = parsed {
            return Some(omo_config_all_fallback(&value));
        }
        return Some(contents.contains("glm-4.7-free"));
    }
    None
}

struct OpenCodeAuthState {
    has_openai: bool,
    has_anthropic: bool,
    has_google: bool,
    has_zai: bool,
    has_other: bool,
    /// Tracks which specific provider IDs have been detected as configured.
    configured_providers: std::collections::HashSet<String>,
}

fn load_provider_auth_entries(
    auth_dir: &std::path::Path,
) -> serde_json::Map<String, serde_json::Value> {
    let mut entries = serde_json::Map::new();
    let Ok(dir_entries) = std::fs::read_dir(auth_dir) else {
        return entries;
    };

    for entry in dir_entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.is_empty() {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        if auth_entry_has_credentials(&value) {
            entries.insert(stem.to_string(), value);
        }
    }

    entries
}

fn detect_opencode_provider_auth(app_working_dir: Option<&std::path::Path>) -> OpenCodeAuthState {
    let mut has_openai = false;
    let mut has_anthropic = false;
    let mut has_google = false;
    let mut has_zai = false;
    let mut has_other = false;
    let mut configured_providers = std::collections::HashSet::new();

    let mark_provider =
        |key: &str,
         has_openai: &mut bool,
         has_anthropic: &mut bool,
         has_google: &mut bool,
         has_zai: &mut bool,
         has_other: &mut bool,
         configured_providers: &mut std::collections::HashSet<String>| {
            configured_providers.insert(key.to_lowercase());
            match key {
                "openai" | "codex" => *has_openai = true,
                "anthropic" | "claude" => *has_anthropic = true,
                "google" | "gemini" => *has_google = true,
                "zai" | "zhipu" => {
                    *has_zai = true;
                    *has_other = true;
                }
                "minimax" => {
                    *has_other = true;
                }
                _ => *has_other = true,
            }
        };

    if let Some(path) = host_opencode_auth_path() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(map) = parsed.as_object() {
                    for (key, value) in map {
                        if !auth_entry_has_credentials(value) {
                            continue;
                        }
                        mark_provider(
                            key.as_str(),
                            &mut has_openai,
                            &mut has_anthropic,
                            &mut has_google,
                            &mut has_zai,
                            &mut has_other,
                            &mut configured_providers,
                        );
                    }
                }
            }
        }
    }

    if let Some(dir) = host_opencode_provider_auth_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if stem.is_empty() {
                    continue;
                }
                mark_provider(
                    stem,
                    &mut has_openai,
                    &mut has_anthropic,
                    &mut has_google,
                    &mut has_zai,
                    &mut has_other,
                    &mut configured_providers,
                );
            }
        }
    }

    if let Ok(value) = std::env::var("OPENAI_API_KEY") {
        if !value.trim().is_empty() {
            has_openai = true;
            configured_providers.insert("openai".to_string());
        }
    }
    if let Ok(value) = std::env::var("ANTHROPIC_API_KEY") {
        if !value.trim().is_empty() {
            has_anthropic = true;
            configured_providers.insert("anthropic".to_string());
        }
    }
    if let Ok(value) = std::env::var("GOOGLE_GENERATIVE_AI_API_KEY") {
        if !value.trim().is_empty() {
            has_google = true;
            configured_providers.insert("google".to_string());
        }
    }
    if let Ok(value) = std::env::var("GOOGLE_API_KEY") {
        if !value.trim().is_empty() {
            has_google = true;
            configured_providers.insert("google".to_string());
        }
    }
    if let Ok(value) = std::env::var("XAI_API_KEY") {
        if !value.trim().is_empty() {
            has_other = true;
            configured_providers.insert("xai".to_string());
        }
    }
    if let Ok(value) = std::env::var("ZHIPU_API_KEY") {
        if !value.trim().is_empty() {
            has_zai = true;
            has_other = true;
            configured_providers.insert("zai".to_string());
        }
    }
    if let Ok(value) = std::env::var("MINIMAX_API_KEY") {
        if !value.trim().is_empty() {
            has_other = true;
            configured_providers.insert("minimax".to_string());
        }
    }
    if let Ok(value) = std::env::var("CEREBRAS_API_KEY") {
        if !value.trim().is_empty() {
            has_other = true;
            configured_providers.insert("cerebras".to_string());
        }
    }

    if let Some(app_dir) = app_working_dir {
        if let Some(auth) = build_opencode_auth_from_ai_providers(app_dir) {
            if let Some(map) = auth.as_object() {
                for (key, value) in map {
                    if !auth_entry_has_credentials(value) {
                        continue;
                    }
                    mark_provider(
                        key.as_str(),
                        &mut has_openai,
                        &mut has_anthropic,
                        &mut has_google,
                        &mut has_zai,
                        &mut has_other,
                        &mut configured_providers,
                    );
                }
            }
        }
    }

    OpenCodeAuthState {
        has_openai,
        has_anthropic,
        has_google,
        has_zai,
        has_other,
        configured_providers,
    }
}

fn workspace_oh_my_opencode_config_paths(
    opencode_config_dir: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        opencode_config_dir.join("oh-my-opencode.json"),
        opencode_config_dir.join("oh-my-opencode.jsonc"),
    )
}

fn try_copy_host_oh_my_opencode_config(opencode_config_dir: &std::path::Path) -> bool {
    let (omo_path, omo_path_jsonc) = workspace_oh_my_opencode_config_paths(opencode_config_dir);
    for candidate in host_oh_my_opencode_config_candidates() {
        if !candidate.exists() {
            continue;
        }
        if let Some(parent) = opencode_config_dir.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "Failed to create OpenCode config dir {}: {}",
                    parent.display(),
                    e
                );
                return false;
            }
        }
        if let Err(e) = std::fs::create_dir_all(opencode_config_dir) {
            tracing::warn!(
                "Failed to create OpenCode config dir {}: {}",
                opencode_config_dir.display(),
                e
            );
            return false;
        }
        let dest = if candidate.extension().and_then(|s| s.to_str()) == Some("jsonc") {
            &omo_path_jsonc
        } else {
            &omo_path
        };
        if let Err(e) = std::fs::copy(&candidate, dest) {
            tracing::warn!(
                "Failed to copy oh-my-opencode config to workspace {}: {}",
                dest.display(),
                e
            );
            return false;
        }
        tracing::info!(
            "Copied oh-my-opencode config to workspace {}",
            dest.display()
        );
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn ensure_oh_my_opencode_config(
    workspace_exec: &WorkspaceExec,
    work_dir: &std::path::Path,
    opencode_config_dir_host: &std::path::Path,
    opencode_config_dir_env: &std::path::Path,
    cli_runner: &str,
    runner_is_direct: bool,
    has_openai: bool,
    has_anthropic: bool,
    has_google: bool,
) {
    let (omo_path, omo_path_jsonc) =
        workspace_oh_my_opencode_config_paths(opencode_config_dir_host);
    if omo_path.exists() || omo_path_jsonc.exists() {
        return;
    }

    let has_any_provider = has_openai || has_anthropic || has_google;
    let host_fallback = host_oh_my_opencode_config_is_fallback();
    let should_regen = matches!(host_fallback, Some(true)) && has_any_provider;

    if !should_regen && try_copy_host_oh_my_opencode_config(opencode_config_dir_host) {
        return;
    }

    // No config found; run oh-my-opencode install in non-interactive mode to generate defaults.
    let mut args: Vec<String> = Vec::new();
    let claude_flag = if has_anthropic { "yes" } else { "no" };
    let openai_flag = if has_openai { "yes" } else { "no" };
    let gemini_flag = if has_google { "yes" } else { "no" };
    if runner_is_direct {
        args.extend([
            "install".to_string(),
            "--no-tui".to_string(),
            format!("--claude={}", claude_flag),
            format!("--openai={}", openai_flag),
            format!("--gemini={}", gemini_flag),
            "--copilot=no".to_string(),
            "--skip-auth".to_string(),
        ]);
    } else {
        args.extend([
            "oh-my-opencode".to_string(),
            "install".to_string(),
            "--no-tui".to_string(),
            format!("--claude={}", claude_flag),
            format!("--openai={}", openai_flag),
            format!("--gemini={}", gemini_flag),
            "--copilot=no".to_string(),
            "--skip-auth".to_string(),
        ]);
    }

    let mut env = std::collections::HashMap::new();
    env.insert(
        "OPENCODE_CONFIG_DIR".to_string(),
        opencode_config_dir_env.to_string_lossy().to_string(),
    );
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("FORCE_COLOR".to_string(), "0".to_string());

    match workspace_exec
        .output(work_dir, cli_runner, &args, env)
        .await
    {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                tracing::warn!(
                    "oh-my-opencode install failed: {} {}",
                    stderr.trim(),
                    stdout.trim()
                );
            } else {
                tracing::info!("Generated oh-my-opencode config in workspace");
                // Some oh-my-opencode versions ignore OPENCODE_CONFIG_DIR during install,
                // so copy the generated host config into the workspace if still missing.
                if !omo_path.exists() && !omo_path_jsonc.exists() {
                    let _ = try_copy_host_oh_my_opencode_config(opencode_config_dir_host);
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to run oh-my-opencode install: {}", e);
        }
    }
}

fn split_package_spec(spec: &str) -> (&str, Option<&str>) {
    if spec.starts_with('@') {
        if let Some((base, version)) = spec.rsplit_once('@') {
            if base.contains('/') {
                return (base, Some(version));
            }
        }
        return (spec, None);
    }
    spec.rsplit_once('@')
        .map(|(base, version)| (base, Some(version)))
        .unwrap_or((spec, None))
}

fn package_base(spec: &str) -> &str {
    split_package_spec(spec).0
}

fn plugin_module_path(node_modules_dir: &std::path::Path, base: &str) -> std::path::PathBuf {
    if let Some(stripped) = base.strip_prefix('@') {
        if let Some((scope, name)) = stripped.split_once('/') {
            return node_modules_dir.join(format!("@{}", scope)).join(name);
        }
    }
    node_modules_dir.join(base)
}

/// Read `opencode.json` from a config directory, returning `{}` on any failure.
fn load_opencode_json(config_dir: &std::path::Path) -> (std::path::PathBuf, serde_json::Value) {
    let path = config_dir.join("opencode.json");
    let value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    (path, value)
}

/// Write a JSON value to a path, logging a warning on failure.
/// Returns `true` if the write succeeded, `false` otherwise.
fn save_json_warn(path: &std::path::Path, value: &serde_json::Value, context: &str) -> bool {
    match std::fs::write(
        path,
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()),
    ) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("Failed to update {context} at {}: {err}", path.display());
            false
        }
    }
}

fn ensure_opencode_plugin_specs(opencode_config_dir: &std::path::Path, plugin_specs: &[&str]) {
    if plugin_specs.is_empty() {
        return;
    }

    let (opencode_path, mut root) = load_opencode_json(opencode_config_dir);

    let mut updated = false;
    let plugins = root.as_object_mut().and_then(|obj| {
        obj.entry("plugin".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
    });

    let Some(plugins) = plugins else {
        return;
    };

    for spec in plugin_specs {
        let base = package_base(spec);
        let mut found_idx = None;
        for (idx, entry) in plugins.iter().enumerate() {
            if let Some(existing) = entry.as_str() {
                if package_base(existing) == base {
                    found_idx = Some(idx);
                    break;
                }
            }
        }

        match found_idx {
            Some(idx) => {
                if plugins[idx].as_str() != Some(*spec) {
                    plugins[idx] = serde_json::Value::String(spec.to_string());
                    updated = true;
                }
            }
            None => {
                plugins.push(serde_json::Value::String(spec.to_string()));
                updated = true;
            }
        }
    }

    if updated {
        save_json_warn(&opencode_path, &root, "OpenCode plugin config");
    }
}

fn detect_google_project_id() -> Option<String> {
    for key in [
        "SANDBOXED_SH_GOOGLE_PROJECT_ID",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_PROJECT_ID",
        "GCP_PROJECT",
    ] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn ensure_opencode_google_project_id(opencode_config_dir: &std::path::Path, project_id: &str) {
    if project_id.trim().is_empty() {
        return;
    }

    let (opencode_path, mut root) = load_opencode_json(opencode_config_dir);

    let mut updated = false;
    let provider_obj = root.as_object_mut().and_then(|obj| {
        obj.entry("provider".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
    });

    let Some(provider_obj) = provider_obj else {
        return;
    };

    let google_obj = provider_obj
        .entry("google".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let google_obj = google_obj.as_object_mut();

    let Some(google_obj) = google_obj else {
        return;
    };

    let options_obj = google_obj
        .entry("options".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let options_obj = options_obj.as_object_mut();

    let Some(options_obj) = options_obj else {
        return;
    };

    match options_obj.get("projectId").and_then(|v| v.as_str()) {
        Some(existing) if existing == project_id => {}
        _ => {
            options_obj.insert(
                "projectId".to_string(),
                serde_json::Value::String(project_id.to_string()),
            );
            updated = true;
        }
    }

    if updated {
        save_json_warn(&opencode_path, &root, "OpenCode Google projectId");
    }
}

async fn ensure_opencode_plugin_installed(
    workspace_exec: &WorkspaceExec,
    work_dir: &std::path::Path,
    opencode_config_dir_host: &std::path::Path,
    opencode_config_dir_env: &std::path::Path,
    plugin_spec: &str,
) {
    let base = package_base(plugin_spec);
    let node_modules_dir = opencode_config_dir_host.join("node_modules");
    let module_path = plugin_module_path(&node_modules_dir, base);
    if module_path.exists() {
        return;
    }

    let installer = if command_available(workspace_exec, work_dir, "bun").await {
        Some("bun")
    } else if command_available(workspace_exec, work_dir, "npm").await {
        Some("npm")
    } else {
        None
    };

    let Some(installer) = installer else {
        tracing::warn!(
            "No bun/npm available to install OpenCode plugin {}",
            plugin_spec
        );
        return;
    };

    let install_cmd = match installer {
        "bun" => format!(
            "cd {} && bun add {}",
            opencode_config_dir_env.to_string_lossy(),
            plugin_spec
        ),
        _ => format!(
            "cd {} && npm install {}",
            opencode_config_dir_env.to_string_lossy(),
            plugin_spec
        ),
    };

    let mut args = Vec::new();
    args.push("-lc".to_string());
    args.push(install_cmd);

    match workspace_exec
        .output(work_dir, "/bin/sh", &args, std::collections::HashMap::new())
        .await
    {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                tracing::warn!(
                    "Failed to install OpenCode plugin {}: {} {}",
                    plugin_spec,
                    stderr.trim(),
                    stdout.trim()
                );
            } else {
                tracing::info!("Installed OpenCode plugin {}", plugin_spec);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to install OpenCode plugin {}: {}", plugin_spec, e);
        }
    }
}

fn sync_opencode_agent_config(
    opencode_config_dir: &std::path::Path,
    default_model: Option<&str>,
    has_openai: bool,
    has_anthropic: bool,
    has_google: bool,
) {
    let (omo_path, omo_path_jsonc) = workspace_oh_my_opencode_config_paths(opencode_config_dir);
    let omo_path = if omo_path.exists() {
        omo_path
    } else if omo_path_jsonc.exists() {
        omo_path_jsonc
    } else {
        return;
    };

    let omo_contents = match std::fs::read_to_string(&omo_path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(
                "Failed to read oh-my-opencode config at {}: {}",
                omo_path.display(),
                err
            );
            return;
        }
    };

    let omo_json = if omo_path.extension().and_then(|s| s.to_str()) == Some("jsonc") {
        serde_json::from_str::<serde_json::Value>(&strip_jsonc_comments(&omo_contents))
    } else {
        serde_json::from_str::<serde_json::Value>(&omo_contents)
    };

    let omo_json = match omo_json {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "Failed to parse oh-my-opencode config at {}: {}",
                omo_path.display(),
                err
            );
            return;
        }
    };

    let Some(omo_agents) = omo_json.get("agents").and_then(|v| v.as_object()) else {
        return;
    };

    let (opencode_path, mut opencode_json) = load_opencode_json(opencode_config_dir);

    let provider_allowed = |provider: &str| -> bool {
        match provider {
            "anthropic" | "claude" => has_anthropic,
            "openai" | "codex" => has_openai,
            "google" | "gemini" => has_google,
            _ => true,
        }
    };

    let mut effective_default = default_model;
    if let Some(model) = default_model {
        if let Some((provider, _)) = model.split_once('/') {
            if !provider_allowed(provider) {
                tracing::warn!(
                    provider = %provider,
                    "Skipping default OpenCode model override because provider is not configured"
                );
                effective_default = None;
            }
        }
    }

    let model_allowed = |model: &str| -> bool {
        match model.split_once('/') {
            Some((provider, _)) => provider_allowed(provider),
            None => true,
        }
    };

    // When oh-my-opencode is enabled, it injects fully-specified agents
    // (including prompts/permissions) into OpenCode. If we also write
    // per-agent overrides into opencode.json, OpenCode treats those as
    // authoritative and overwrites the plugin-defined agents, which
    // strips the Prometheus/Metis/Momus/etc prompts. Therefore, we avoid
    // writing any per-agent overrides for oh-my-opencode agents and
    // remove any stale overrides that might already exist.
    let oh_my_opencode_enabled = opencode_json
        .get("plugin")
        .and_then(|v| v.as_array())
        .map(|plugins| {
            plugins.iter().any(|entry| {
                entry
                    .as_str()
                    .map(|s| s.contains("oh-my-opencode"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    let mut updated = false;
    if let Some(model) = effective_default {
        if let Some(obj) = opencode_json.as_object_mut() {
            match obj.get("model").and_then(|v| v.as_str()) {
                Some(existing) if existing == model => {}
                _ => {
                    obj.insert(
                        "model".to_string(),
                        serde_json::Value::String(model.to_string()),
                    );
                    updated = true;
                }
            }
        }
    } else if let Some(obj) = opencode_json.as_object_mut() {
        if let Some(existing) = obj.get("model").and_then(|v| v.as_str()) {
            if let Some((provider, _)) = existing.split_once('/') {
                if !provider_allowed(provider) {
                    obj.remove("model");
                    updated = true;
                }
            }
        }
    }

    if let Some(obj) = opencode_json.as_object_mut() {
        let agent_entry = obj
            .entry("agent".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let agent_entry = match agent_entry.as_object_mut() {
            Some(entry) => entry,
            None => return,
        };

        if oh_my_opencode_enabled {
            for name in omo_agents.keys() {
                if agent_entry.remove(name).is_some() {
                    updated = true;
                }
            }
        } else {
            for (name, entry) in omo_agents {
                // Agent-specific model from oh-my-opencode.json takes priority over fallback default
                let desired_model = entry
                    .get("model")
                    .and_then(|v| v.as_str())
                    .filter(|model| model_allowed(model))
                    .map(|s| s.to_string())
                    .or_else(|| {
                        effective_default
                            .filter(|model| model_allowed(model))
                            .map(|s| s.to_string())
                    });

                if let Some(existing) = agent_entry.get_mut(name) {
                    if let (Some(model), Some(existing_obj)) =
                        (desired_model.as_ref(), existing.as_object_mut())
                    {
                        match existing_obj.get("model").and_then(|v| v.as_str()) {
                            Some(current) if current == model => {}
                            _ => {
                                existing_obj.insert(
                                    "model".to_string(),
                                    serde_json::Value::String(model.clone()),
                                );
                                updated = true;
                            }
                        }
                    } else if let Some(existing_obj) = existing.as_object_mut() {
                        if let Some(current) = existing_obj.get("model").and_then(|v| v.as_str()) {
                            if !model_allowed(current) {
                                existing_obj.remove("model");
                                updated = true;
                            }
                        }
                    }
                    continue;
                }

                let mut agent_config = serde_json::Map::new();
                if let Some(model) = desired_model {
                    agent_config.insert("model".to_string(), serde_json::Value::String(model));
                }
                agent_entry.insert(name.clone(), serde_json::Value::Object(agent_config));
                updated = true;
            }
        }
    }

    if updated {
        save_json_warn(&opencode_path, &opencode_json, "opencode.json agent config");
    }
}

fn apply_model_override_to_oh_my_opencode(
    opencode_config_dir: &std::path::Path,
    model_override: &str,
) {
    let model_override = model_override.trim();
    if model_override.is_empty() {
        return;
    }

    let (omo_path, omo_path_jsonc) = workspace_oh_my_opencode_config_paths(opencode_config_dir);
    let target_path = if omo_path.exists() {
        omo_path
    } else if omo_path_jsonc.exists() {
        omo_path_jsonc
    } else {
        return;
    };

    let contents = match std::fs::read_to_string(&target_path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(
                "Failed to read oh-my-opencode config at {}: {}",
                target_path.display(),
                err
            );
            return;
        }
    };

    let json = if target_path.extension().and_then(|s| s.to_str()) == Some("jsonc") {
        serde_json::from_str::<serde_json::Value>(&strip_jsonc_comments(&contents))
    } else {
        serde_json::from_str::<serde_json::Value>(&contents)
    };

    let mut json = match json {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "Failed to parse oh-my-opencode config at {}: {}",
                target_path.display(),
                err
            );
            return;
        }
    };

    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(model_override.to_string()),
        );
    }

    if let Some(agents) = json.get_mut("agents").and_then(|v| v.as_object_mut()) {
        for agent in agents.values_mut() {
            if let Some(agent_obj) = agent.as_object_mut() {
                agent_obj.insert(
                    "model".to_string(),
                    serde_json::Value::String(model_override.to_string()),
                );
                agent_obj.remove("variant");
            }
        }
    }

    if let Ok(updated) = serde_json::to_string_pretty(&json) {
        if let Err(err) = std::fs::write(&target_path, updated) {
            tracing::warn!(
                "Failed to write oh-my-opencode config at {}: {}",
                target_path.display(),
                err
            );
        } else {
            tracing::info!(
                "Applied OpenCode model override {} to {}",
                model_override,
                target_path.display()
            );
        }
    }
}

/// Ensure the `opencode.json` `provider` section contains a definition for the
/// provider used by the model override.  OpenCode's built-in snapshot only knows
/// about a subset of models per provider; if a model (e.g. `zai/glm-5`) is not
/// in the snapshot the session silently fails.  By injecting a custom provider
/// definition we tell the AI-SDK adapter *how* to reach the provider and declare
/// the model as valid.
fn ensure_opencode_provider_for_model(opencode_config_dir: &std::path::Path, model_override: &str) {
    let model_override = model_override.trim();
    if model_override.is_empty() {
        return;
    }

    let (provider_id, model_id) = match model_override.split_once('/') {
        Some(pair) => pair,
        None => return,
    };

    // Build the model definition — include capabilities for reasoning models.
    // GLM-5/6 support "Deep Thinking" mode which sends reasoning tokens via
    // the `reasoning_content` field.  Declaring `capabilities.interleaved`
    // tells the AI-SDK adapter to map that field to `part.type = "reasoning"`.
    let model_entry = if provider_id == "zai"
        && (model_id.starts_with("glm-5") || model_id.starts_with("glm-6"))
    {
        serde_json::json!({
            "name": model_id,
            "capabilities": {
                "interleaved": { "field": "reasoning_content" }
            }
        })
    } else {
        serde_json::json!({ "name": model_id })
    };

    // Only inject definitions for providers that need it.
    // OpenAI, Anthropic, Google are natively supported by OpenCode.
    let provider_def: Option<serde_json::Value> = match provider_id {
        "zai" => {
            let base_url = std::env::var("ZAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.z.ai/api/coding/paas/v4".to_string());
            Some(serde_json::json!({
                "models": {
                    model_id: model_entry.clone()
                },
                "options": {
                    "baseURL": base_url
                }
            }))
        }
        "minimax" => {
            let base_url = std::env::var("MINIMAX_BASE_URL")
                .unwrap_or_else(|_| "https://api.minimax.io/v1".to_string());
            Some(serde_json::json!({
                "npm": "@ai-sdk/openai-compatible",
                "name": "Minimax",
                "models": {
                    model_id: { "name": model_id }
                },
                "options": {
                    "baseURL": base_url
                }
            }))
        }
        "cerebras" => Some(serde_json::json!({
            "npm": "@ai-sdk/cerebras",
            "name": "Cerebras",
            "models": {
                model_id: model_entry.clone()
            }
        })),
        "xai" => Some(serde_json::json!({
            "npm": "@ai-sdk/xai",
            "name": "xAI",
            "models": {
                model_id: model_entry.clone()
            }
        })),
        "builtin" => {
            // Point at the local OpenAI-compatible proxy that handles model
            // chain resolution and failover.  The proxy runs on the same host
            // and is accessible from shared-network workspaces.
            let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
            let proxy_key = std::env::var("SANDBOXED_PROXY_SECRET")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    tracing::error!("SANDBOXED_PROXY_SECRET not set; builtin proxy auth will fail");
                    String::new()
                });
            Some(serde_json::json!({
                "npm": "@ai-sdk/openai-compatible",
                "name": "Builtin",
                "models": {
                    model_id: { "name": model_id }
                },
                "options": {
                    "baseURL": format!("http://127.0.0.1:{}/v1", port),
                    "apiKey": proxy_key
                }
            }))
        }
        _ => None,
    };

    let Some(provider_def) = provider_def else {
        return;
    };

    let (opencode_path, mut root) = load_opencode_json(opencode_config_dir);

    let obj = match root.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };

    let providers = obj
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    let providers_map = match providers.as_object_mut() {
        Some(map) => map,
        None => return,
    };

    if provider_id == "builtin" {
        // Always overwrite the builtin provider definition — the proxy secret
        // (options.apiKey) changes on every server restart.
        providers_map.insert(provider_id.to_string(), provider_def);
    } else if let Some(existing) = providers_map.get_mut(provider_id) {
        // Provider already exists – make sure the model is listed.
        let obj = match existing.as_object_mut() {
            Some(o) => o,
            None => return,
        };
        let models = obj
            .entry("models".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let models_map = match models.as_object_mut() {
            Some(m) => m,
            None => return,
        };
        if models_map.contains_key(model_id) {
            // Model exists — ensure capabilities are up to date for reasoning models.
            if let Some(caps) = model_entry.get("capabilities") {
                if let Some(existing_model) = models_map.get_mut(model_id) {
                    if existing_model.get("capabilities").is_none() {
                        if let Some(obj) = existing_model.as_object_mut() {
                            obj.insert("capabilities".to_string(), caps.clone());
                        }
                    }
                }
            } else {
                return; // already present, nothing to do
            }
        } else {
            models_map.insert(model_id.to_string(), model_entry);
        }
    } else {
        providers_map.insert(provider_id.to_string(), provider_def);
    }

    if save_json_warn(&opencode_path, &root, "OpenCode provider config") {
        tracing::info!(
            "Injected OpenCode provider definition for {}/{} into {}",
            provider_id,
            model_id,
            opencode_path.display()
        );
    }
}

/// Scan the oh-my-opencode config for all model references (top-level, agents,
/// categories) and ensure each provider has a definition in `opencode.json`.
fn ensure_opencode_providers_for_omo_config(opencode_config_dir: &std::path::Path) {
    let (omo_path, omo_path_jsonc) = workspace_oh_my_opencode_config_paths(opencode_config_dir);
    let target_path = if omo_path.exists() {
        omo_path
    } else if omo_path_jsonc.exists() {
        omo_path_jsonc
    } else {
        return;
    };

    let contents = match std::fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let parsed = if target_path.extension().and_then(|s| s.to_str()) == Some("jsonc") {
        serde_json::from_str::<serde_json::Value>(&strip_jsonc_comments(&contents))
    } else {
        serde_json::from_str::<serde_json::Value>(&contents)
    };
    let json = match parsed {
        Ok(v) => v,
        Err(_) => return,
    };

    let mut models = std::collections::HashSet::new();

    // Collect top-level model
    if let Some(m) = json.get("model").and_then(|v| v.as_str()) {
        models.insert(m.to_string());
    }

    // Collect agent models
    if let Some(agents) = json.get("agents").and_then(|v| v.as_object()) {
        for agent in agents.values() {
            if let Some(m) = agent.get("model").and_then(|v| v.as_str()) {
                models.insert(m.to_string());
            }
        }
    }

    // Collect category models
    if let Some(categories) = json.get("categories").and_then(|v| v.as_object()) {
        for cat in categories.values() {
            if let Some(m) = cat.get("model").and_then(|v| v.as_str()) {
                models.insert(m.to_string());
            }
        }
    }

    for model in &models {
        ensure_opencode_provider_for_model(opencode_config_dir, model);
    }
}

fn workspace_abs_path(workspace: &Workspace, path: &std::path::Path) -> std::path::PathBuf {
    if workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        if let Ok(relative) = path.strip_prefix(std::path::Path::new("/")) {
            return workspace.path.join(relative);
        }
        return workspace.path.join(path);
    }
    path.to_path_buf()
}

fn find_oh_my_opencode_cli_js(workspace: &Workspace) -> Option<std::path::PathBuf> {
    // Static paths for global npm installations
    let candidates = [
        "/usr/local/lib/node_modules/oh-my-opencode/dist/cli/index.js",
        "/usr/lib/node_modules/oh-my-opencode/dist/cli/index.js",
        "/opt/homebrew/lib/node_modules/oh-my-opencode/dist/cli/index.js",
        "/usr/local/share/node_modules/oh-my-opencode/dist/cli/index.js",
    ];

    for candidate in candidates {
        let path = workspace_abs_path(workspace, std::path::Path::new(candidate));
        if path.exists() {
            return Some(path);
        }
    }

    // Search bun cache for oh-my-opencode (used when installed via bunx)
    // Pattern: ~/.cache/.bun/install/cache/oh-my-opencode@<version>@@@1/dist/cli/index.js
    // Some bun versions use ~/.bun/install/cache instead of ~/.cache/.bun/install/cache
    let bun_cache_bases = [
        "/root/.cache/.bun/install/cache",
        "/root/.bun/install/cache",
        "/home/*/.cache/.bun/install/cache",
        "/home/*/.bun/install/cache",
    ];
    for base in bun_cache_bases {
        let base_path = workspace_abs_path(workspace, std::path::Path::new(base));
        if let Ok(entries) = std::fs::read_dir(&base_path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("oh-my-opencode@") {
                    let cli_js = entry.path().join("dist/cli/index.js");
                    if cli_js.exists() {
                        return Some(cli_js);
                    }
                }
            }
        }
    }

    None
}

fn patch_oh_my_opencode_port_override(workspace: &Workspace) -> bool {
    let Some(cli_js_path) = find_oh_my_opencode_cli_js(workspace) else {
        return false;
    };

    let contents = match std::fs::read_to_string(&cli_js_path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(
                "Failed to read oh-my-opencode CLI at {}: {}",
                cli_js_path.display(),
                err
            );
            return false;
        }
    };

    if contents.contains("SANDBOXED_SH_OPENCODE_PORT_PATCH") {
        return true;
    }

    let newline = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let needle = format!(
        "const {{ client: client3, server: server2 }} = await createOpencode({{{nl}      signal: abortController.signal{nl}    }});",
        nl = newline
    );
    if !contents.contains(&needle) {
        tracing::warn!(
            "Unable to patch oh-my-opencode CLI (pattern mismatch) at {}",
            cli_js_path.display()
        );
        return false;
    }

    let replacement = format!(
        "const __oaPortRaw = process.env.OPENCODE_SERVER_PORT;{nl}    const __oaPort = __oaPortRaw ? Number(__oaPortRaw) : void 0;{nl}    const __oaHost = process.env.OPENCODE_SERVER_HOSTNAME;{nl}    const {{ client: client3, server: server2 }} = await createOpencode({{{nl}      signal: abortController.signal,{nl}      ...(Number.isFinite(__oaPort) ? {{ port: __oaPort }} : {{}}),{nl}      ...(__oaHost ? {{ hostname: __oaHost }} : {{}}),{nl}      // SANDBOXED_SH_OPENCODE_PORT_PATCH{nl}    }});",
        nl = newline
    );

    let patched = contents.replace(&needle, &replacement);
    if let Err(err) = std::fs::write(&cli_js_path, patched) {
        tracing::warn!(
            "Failed to patch oh-my-opencode CLI at {}: {}",
            cli_js_path.display(),
            err
        );
        return false;
    }

    tracing::info!(
        "Patched oh-my-opencode CLI to honor OPENCODE_SERVER_PORT at {}",
        cli_js_path.display()
    );
    true
}

fn opencode_storage_roots(workspace: &Workspace) -> Vec<std::path::PathBuf> {
    if workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        let mut roots = Vec::new();

        // Prefer container-local /root storage (matches overridden XDG defaults).
        roots.push(
            workspace
                .path
                .join("root")
                .join(".local")
                .join("share")
                .join("opencode")
                .join("storage"),
        );

        if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
            if let Ok(rel) =
                std::path::Path::new(&data_home).strip_prefix(std::path::Path::new("/"))
            {
                roots.push(workspace.path.join(rel).join("opencode").join("storage"));
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            if let Ok(rel) = std::path::Path::new(&home).strip_prefix(std::path::Path::new("/")) {
                roots.push(
                    workspace
                        .path
                        .join(rel)
                        .join(".local")
                        .join("share")
                        .join("opencode")
                        .join("storage"),
                );
            }
        }

        roots.sort();
        roots.dedup();
        return roots;
    }

    let data_home =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home_dir()));
    vec![std::path::PathBuf::from(data_home)
        .join("opencode")
        .join("storage")]
}

fn host_opencode_auth_path() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        candidates.push(
            std::path::PathBuf::from(data_home)
                .join("opencode")
                .join("auth.json"),
        );
    }

    if let Ok(home) = std::env::var("HOME") {
        candidates.push(
            std::path::PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json"),
        );
    }

    candidates.push(
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    candidates.into_iter().next()
}

fn host_opencode_provider_auth_dir() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(
            std::path::PathBuf::from(home)
                .join(".opencode")
                .join("auth"),
        );
    }

    candidates.push(
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".opencode")
            .join("auth"),
    );

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    candidates.into_iter().next()
}

fn workspace_opencode_auth_path(workspace: &Workspace) -> Option<std::path::PathBuf> {
    if workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        return Some(
            workspace
                .path
                .join("root")
                .join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json"),
        );
    }
    host_opencode_auth_path()
}

fn workspace_opencode_provider_auth_dir(workspace: &Workspace) -> Option<std::path::PathBuf> {
    if workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(workspace)
    {
        return Some(workspace.path.join("root").join(".opencode").join("auth"));
    }
    host_opencode_provider_auth_dir()
}

fn build_opencode_auth_from_ai_providers(
    app_working_dir: &std::path::Path,
) -> Option<serde_json::Value> {
    let path = app_working_dir
        .join(".sandboxed-sh")
        .join("ai_providers.json");
    let contents = std::fs::read_to_string(&path).ok()?;
    let providers: Vec<crate::ai_providers::AIProvider> = serde_json::from_str(&contents).ok()?;

    let mut map = serde_json::Map::new();
    for provider in providers {
        if !provider.enabled {
            continue;
        }
        let keys: Vec<&str> = match provider.provider_type {
            crate::ai_providers::ProviderType::OpenAI => vec!["openai", "codex"],
            _ => vec![provider.provider_type.id()],
        };
        if let Some(api_key) = provider.api_key {
            let entry = serde_json::json!({
                "type": "api_key",
                "key": api_key,
            });
            for key in &keys {
                map.insert((*key).to_string(), entry.clone());
            }
        } else if let Some(oauth) = provider.oauth {
            let entry = serde_json::json!({
                "type": "oauth",
                "refresh": oauth.refresh_token,
                "access": oauth.access_token,
                "expires": oauth.expires_at,
            });
            for key in &keys {
                map.insert((*key).to_string(), entry.clone());
            }
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    }
}

fn write_json_file(path: &std::path::Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, contents)
}

fn sync_opencode_auth_to_workspace(
    workspace: &Workspace,
    app_working_dir: &std::path::Path,
) -> Option<serde_json::Value> {
    let mut auth_json: Option<serde_json::Value> = None;

    if let Some(source_path) = host_opencode_auth_path() {
        if let Ok(contents) = std::fs::read_to_string(&source_path) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) {
                auth_json = Some(parsed);
            }
        }

        if let Some(dest_path) = workspace_opencode_auth_path(workspace) {
            if dest_path != source_path && source_path.exists() {
                if let Some(parent) = dest_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        tracing::warn!(
                            "Failed to create OpenCode auth directory {}: {}",
                            parent.display(),
                            e
                        );
                    }
                }
                if let Err(e) = std::fs::copy(&source_path, &dest_path) {
                    tracing::warn!(
                        "Failed to copy OpenCode auth.json to workspace {}: {}",
                        dest_path.display(),
                        e
                    );
                }
            }
        }
    }

    if auth_json.is_none() {
        auth_json = build_opencode_auth_from_ai_providers(app_working_dir);
        if let Some(ref value) = auth_json {
            if let Some(dest_path) = workspace_opencode_auth_path(workspace) {
                if let Err(e) = write_json_file(&dest_path, value) {
                    tracing::warn!(
                        "Failed to write OpenCode auth.json to workspace {}: {}",
                        dest_path.display(),
                        e
                    );
                }
            }
        }
    }

    let providers = [
        "openai",
        "anthropic",
        "google",
        "xai",
        "zai",
        "cerebras",
        "minimax",
    ];
    if let (Some(src_dir), Some(dest_dir)) = (
        host_opencode_provider_auth_dir(),
        workspace_opencode_provider_auth_dir(workspace),
    ) {
        for provider in providers {
            let src = src_dir.join(format!("{}.json", provider));
            if !src.exists() {
                continue;
            }
            let dest = dest_dir.join(format!("{}.json", provider));
            if dest == src {
                continue;
            }
            if let Err(e) = std::fs::create_dir_all(&dest_dir) {
                tracing::warn!(
                    "Failed to create OpenCode provider auth dir {}: {}",
                    dest_dir.display(),
                    e
                );
                continue;
            }
            if let Err(e) = std::fs::copy(&src, &dest) {
                tracing::warn!(
                    "Failed to copy OpenCode provider auth file to workspace {}: {}",
                    dest.display(),
                    e
                );
            }
        }
    }

    // Merge provider auth files into auth.json for env export (e.g., XAI_API_KEY)
    if let Some(provider_dir) = workspace_opencode_provider_auth_dir(workspace) {
        let provider_entries = load_provider_auth_entries(&provider_dir);
        if !provider_entries.is_empty() {
            let mut merged = match auth_json.take() {
                Some(serde_json::Value::Object(map)) => map,
                Some(_) => serde_json::Map::new(),
                None => serde_json::Map::new(),
            };
            for (key, value) in provider_entries {
                merged.entry(key).or_insert(value);
            }
            auth_json = Some(serde_json::Value::Object(merged));

            if let Some(dest_path) = workspace_opencode_auth_path(workspace) {
                if let Some(ref value) = auth_json {
                    if let Err(e) = write_json_file(&dest_path, value) {
                        tracing::warn!(
                            "Failed to write merged OpenCode auth.json to workspace {}: {}",
                            dest_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    if let (Some(value), Some(dest_dir)) = (
        auth_json.as_ref(),
        workspace_opencode_provider_auth_dir(workspace),
    ) {
        let provider_entries = [
            ("openai", "OpenAI"),
            ("anthropic", "Anthropic"),
            ("google", "Google"),
            ("xai", "xAI"),
            ("zai", "Z.AI"),
            ("minimax", "Minimax"),
            ("cerebras", "Cerebras"),
        ];
        for (key, label) in provider_entries {
            let entry = if key == "openai" {
                value.get("openai").or_else(|| value.get("codex"))
            } else {
                value.get(key)
            };
            if let Some(entry) = entry {
                let dest = dest_dir.join(format!("{}.json", key));
                if let Err(e) = write_json_file(&dest, entry) {
                    tracing::warn!(
                        "Failed to write OpenCode {} auth file to workspace {}: {}",
                        label,
                        dest.display(),
                        e
                    );
                }
            }
        }
    }

    auth_json
}

fn extract_opencode_api_key(entry: &serde_json::Value) -> Option<String> {
    let auth_type = entry.get("type").and_then(|v| v.as_str());
    let key = entry
        .get("key")
        .or_else(|| entry.get("api_key"))
        .and_then(|v| v.as_str());

    match auth_type {
        Some("oauth") => None,
        _ => key.map(|s| s.to_string()),
    }
}

fn apply_opencode_auth_env(
    auth: &serde_json::Value,
    env: &mut std::collections::HashMap<String, String>,
) -> Vec<&'static str> {
    let mut providers = Vec::new();
    let mut seen = HashSet::new();

    let Some(map) = auth.as_object() else {
        return providers;
    };

    for (key, entry) in map {
        let Some(provider_type) = crate::ai_providers::ProviderType::from_id(key) else {
            continue;
        };
        let Some(api_key) = extract_opencode_api_key(entry) else {
            continue;
        };

        if let Some(env_var) = provider_type.env_var_name() {
            env.entry(env_var.to_string()).or_insert(api_key.clone());
        }

        if provider_type == crate::ai_providers::ProviderType::Google {
            env.entry("GOOGLE_GENERATIVE_AI_API_KEY".to_string())
                .or_insert(api_key.clone());
            env.entry("GOOGLE_API_KEY".to_string())
                .or_insert(api_key.clone());
        }

        let provider_id = provider_type.id();
        if seen.insert(provider_id) {
            providers.push(provider_id);
        }
    }

    providers
}

#[derive(Debug, Clone)]
struct StoredOpenCodeMessage {
    parts: Vec<serde_json::Value>,
    model: Option<String>,
}

fn extract_model_from_message(value: &serde_json::Value) -> Option<String> {
    fn get_str<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
        for key in keys {
            if let Some(v) = value.get(*key).and_then(|v| v.as_str()) {
                return Some(v);
            }
        }
        None
    }

    let mut candidates = Vec::new();
    candidates.push(value);
    if let Some(info) = value.get("info") {
        candidates.push(info);
        if let Some(info_model) = info.get("model") {
            candidates.push(info_model);
        }
    }
    if let Some(model) = value.get("model") {
        candidates.push(model);
    }

    for candidate in candidates {
        let provider = get_str(
            candidate,
            &["providerID", "providerId", "provider_id", "provider"],
        );
        let model_id = get_str(candidate, &["modelID", "modelId", "model_id", "model"]);
        if let (Some(provider), Some(model_id)) = (provider, model_id) {
            if !provider.is_empty() && !model_id.is_empty() {
                return Some(format!("{}/{}", provider, model_id));
            }
        }

        if let Some(model) = get_str(candidate, &["model", "model_id", "modelID", "modelId"]) {
            if model.contains('/') {
                return Some(model.to_string());
            }
        }
    }

    None
}

fn load_latest_opencode_assistant_message(
    workspace: &Workspace,
    session_id: &str,
) -> Option<StoredOpenCodeMessage> {
    let mut storage_root: Option<std::path::PathBuf> = None;
    for root in opencode_storage_roots(workspace) {
        let message_dir = root.join("message").join(session_id);
        if message_dir.exists() {
            storage_root = Some(root);
            break;
        }
    }

    let storage_root = storage_root?;
    let message_dir = storage_root.join("message").join(session_id);

    let mut latest_time = 0i64;
    let mut latest_message_id: Option<String> = None;
    let mut latest_model: Option<String> = None;

    let entries = std::fs::read_dir(&message_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        let role = value.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }
        let created = value
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if created >= latest_time {
            latest_time = created;
            latest_message_id = value
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            latest_model = extract_model_from_message(&value);
        }
    }

    let message_id = latest_message_id?;
    let parts_dir = storage_root.join("part").join(&message_id);
    if !parts_dir.exists() {
        return None;
    }

    let mut parts: Vec<(i64, String, serde_json::Value)> = Vec::new();
    let part_entries = std::fs::read_dir(&parts_dir).ok()?;
    for entry in part_entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        let start = value
            .get("time")
            .and_then(|t| t.get("start"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        parts.push((start, filename, value));
    }

    if parts.is_empty() {
        return None;
    }

    parts.sort_by(|a, b| {
        let time_cmp = a.0.cmp(&b.0);
        if time_cmp == std::cmp::Ordering::Equal {
            a.1.cmp(&b.1)
        } else {
            time_cmp
        }
    });

    let parts = parts.into_iter().map(|(_, _, value)| value).collect();

    Some(StoredOpenCodeMessage {
        parts,
        model: latest_model,
    })
}

fn resolve_opencode_model_from_config(
    opencode_config_dir: &std::path::Path,
    agent: Option<&str>,
) -> Option<String> {
    let (_opencode_path, value) = load_opencode_json(opencode_config_dir);

    if let Some(agent_name) = agent {
        if let Some(model) = value
            .get("agent")
            .and_then(|v| v.get(agent_name))
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str())
        {
            return Some(model.to_string());
        }
        if let Some(agent_map) = value.get("agent").and_then(|v| v.as_object()) {
            let agent_lower = agent_name.to_lowercase();
            for (name, entry) in agent_map {
                if name.to_lowercase() == agent_lower {
                    if let Some(model) = entry.get("model").and_then(|v| v.as_str()) {
                        return Some(model.to_string());
                    }
                }
            }
        }
    }

    if let Some(model) = value.get("model").and_then(|v| v.as_str()) {
        return Some(model.to_string());
    }

    let omo_path = opencode_config_dir.join("oh-my-opencode.json");
    let omo_jsonc_path = opencode_config_dir.join("oh-my-opencode.jsonc");
    let omo_path = if omo_jsonc_path.exists() {
        omo_jsonc_path
    } else {
        omo_path
    };

    let contents = std::fs::read_to_string(omo_path).ok()?;
    let contents = if contents.contains("//") {
        strip_jsonc_comments(&contents)
    } else {
        contents
    };
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    if let Some(agent_name) = agent {
        if let Some(model) = value
            .get("agents")
            .and_then(|v| v.get(agent_name))
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str())
        {
            return Some(model.to_string());
        }
        if let Some(agent_map) = value.get("agents").and_then(|v| v.as_object()) {
            let agent_lower = agent_name.to_lowercase();
            for (name, entry) in agent_map {
                if name.to_lowercase() == agent_lower {
                    if let Some(model) = entry.get("model").and_then(|v| v.as_str()) {
                        return Some(model.to_string());
                    }
                }
            }
        }
    }

    value
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn command_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    program: &str,
) -> bool {
    if workspace_exec.workspace.workspace_type == WorkspaceType::Host {
        if program.contains('/') {
            return std::path::Path::new(program).is_file();
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                if dir.is_empty() {
                    continue;
                }
                let candidate = std::path::Path::new(dir).join(program);
                if candidate.is_file() {
                    return true;
                }
            }
        }
        return false;
    }

    async fn check_dir(
        workspace_exec: &WorkspaceExec,
        cwd: &std::path::Path,
        program: &str,
    ) -> Option<bool> {
        let mut args = Vec::new();
        args.push("-lc".to_string());
        if program.contains('/') {
            args.push(format!("test -x {}", program));
        } else {
            args.push(format!("command -v {} 2>/dev/null", program));
        }
        let output = workspace_exec
            .output(cwd, "/bin/sh", &args, HashMap::new())
            .await
            .ok()?;
        if !output.status.success() {
            return Some(false);
        }
        if program.contains('/') {
            return Some(true);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Some(!stdout.trim().is_empty())
    }

    if let Some(found) = check_dir(workspace_exec, cwd, program).await {
        if found {
            return true;
        }
    }

    let fallback_dir = &workspace_exec.workspace.path;
    if cwd != fallback_dir {
        if let Some(found) = check_dir(workspace_exec, fallback_dir, program).await {
            return found;
        }
    }

    false
}

async fn resolve_command_path_in_workspace(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    program: &str,
) -> Option<String> {
    if program.contains('/') {
        return Some(program.to_string());
    }

    let mut args = Vec::new();
    args.push("-lc".to_string());
    args.push(format!("command -v {} 2>/dev/null", program));
    let output = workspace_exec
        .output(cwd, "/bin/sh", &args, HashMap::new())
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = stdout.lines().next().unwrap_or("").trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

async fn claude_cli_shebang_contains(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    path: &str,
    needle: &str,
) -> Option<bool> {
    if path.trim().is_empty() || needle.trim().is_empty() {
        return None;
    }
    let quoted = shell_quote(path);
    let cmd = format!("head -n 1 {} 2>/dev/null", quoted);
    let output = workspace_exec
        .output(
            cwd,
            "/bin/sh",
            &["-lc".to_string(), cmd],
            std::collections::HashMap::new(),
        )
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let first_line = line.lines().next().unwrap_or("").trim().to_lowercase();
    if first_line.is_empty() {
        return None;
    }
    Some(first_line.contains(&needle.to_lowercase()))
}

fn format_exit_status(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("code {}", code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal {}", signal);
        }
    }
    "code <unknown>".to_string()
}

/// Check basic internet connectivity using a reliable public endpoint.
/// This verifies the workspace has any network access at all.
async fn check_basic_internet_connectivity(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
) -> Result<(), String> {
    // Use Cloudflare's 1.1.1.1 which is highly reliable and fast.
    //
    // Avoid piping to `head`: under some shells/environments with `pipefail` enabled, the
    // upstream `curl` may be terminated with SIGPIPE which yields an exit code of None (-1)
    // and causes spurious "network check failed" errors.
    let test_cmd = "curl -sS -o /dev/null -w '%{http_code}' --max-time 5 https://1.1.1.1";
    let max_attempts = 3;

    for attempt in 1..=max_attempts {
        let output = match workspace_exec
            .output(
                cwd,
                "/bin/sh",
                &["-c".to_string(), test_cmd.to_string()],
                std::collections::HashMap::new(),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                let err = format!(
                    "Network connectivity check failed: {}. The workspace may have networking issues.",
                    e
                );
                if attempt < max_attempts {
                    tracing::warn!(
                        "Basic internet connectivity check failed on attempt {} of {}: {}",
                        attempt,
                        max_attempts,
                        err
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempt as u64))
                        .await;
                    continue;
                }
                return Err(err);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", stdout, stderr);

        let err = if combined.contains("Network is unreachable") {
            "No internet connectivity: Network is unreachable. \
             The workspace has no network access."
                .to_string()
        } else if combined.contains("Connection timed out")
            || combined.contains("Operation timed out")
        {
            "No internet connectivity: Connection timed out. \
             The workspace cannot reach the internet."
                .to_string()
        } else {
            // Check for successful HTTP response (any non-000 code means we got an HTTP response).
            let code = stdout.trim();
            if !code.is_empty() && code != "000" {
                tracing::debug!("Basic internet connectivity check passed");
                return Ok(());
            }

            // If curl failed completely
            if !output.status.success() {
                format!(
                    "No internet connectivity: Network check failed ({}). Output: {}",
                    format_exit_status(&output.status),
                    combined.trim()
                )
            } else {
                format!(
                    "No internet connectivity: unexpected curl output (http_code={}). Output: {}",
                    if code.is_empty() { "<empty>" } else { code },
                    combined.trim()
                )
            }
        };

        if attempt < max_attempts {
            tracing::warn!(
                "Basic internet connectivity check failed on attempt {} of {}: {}",
                attempt,
                max_attempts,
                err
            );
            tokio::time::sleep(std::time::Duration::from_millis(200 * attempt as u64)).await;
            continue;
        }

        return Err(err);
    }

    Err("No internet connectivity: unexpected error during connectivity check.".to_string())
}

/// Check DNS resolution for a specific hostname.
async fn check_dns_resolution(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    hostname: &str,
) -> Result<(), String> {
    // Use getent or nslookup to test DNS resolution
    let test_cmd = format!(
        "getent hosts {} 2>&1 || nslookup {} 2>&1 | head -3",
        hostname, hostname
    );

    let output = match workspace_exec
        .output(
            cwd,
            "/bin/sh",
            &["-c".to_string(), test_cmd],
            std::collections::HashMap::new(),
        )
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return Err(format!("DNS resolution check failed: {}", e));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    // Check for DNS failure indicators
    if combined.contains("not found")
        || combined.contains("NXDOMAIN")
        || combined.contains("no address")
        || combined.contains("Name or service not known")
    {
        return Err(format!(
            "DNS resolution failed for '{}'. \
             The workspace DNS is not properly configured. \
             For Tailscale workspaces, ensure the VPN connection is established.",
            hostname
        ));
    }

    // If getent succeeded (exit code 0), DNS works
    if output.status.success() {
        tracing::debug!("DNS resolution check passed for {}", hostname);
        return Ok(());
    }

    // Check if we got any IP address in the output (nslookup format)
    let has_ip = combined.lines().any(|line| {
        line.contains("Address:")
            || line
                .split_whitespace()
                .any(|w| w.parse::<std::net::IpAddr>().is_ok())
    });

    if has_ip {
        tracing::debug!("DNS resolution check passed for {} (found IP)", hostname);
        return Ok(());
    }

    Err(format!(
        "DNS resolution failed for '{}'. Check network configuration.",
        hostname
    ))
}

/// Check if a specific API endpoint is reachable.
/// Returns detailed error messages for different failure modes.
async fn check_api_reachability(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    api_name: &str,
    api_url: &str,
) -> Result<(), String> {
    // Use curl to test HTTPS connectivity to the API
    //
    // We intentionally avoid piping to `head` here for the same reason as the basic connectivity
    // check: environments with `pipefail` can turn a harmless SIGPIPE into a non-success status.
    let test_cmd = format!(
        "curl -sS -o /dev/null -w '%{{http_code}}' --max-time 10 {}",
        api_url
    );

    let output = match workspace_exec
        .output(
            cwd,
            "/bin/sh",
            &["-c".to_string(), test_cmd],
            std::collections::HashMap::new(),
        )
        .await
    {
        Ok(out) => out,
        Err(e) => {
            return Err(format!("Cannot connect to {} API: {}", api_name, e));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    // Check for common error patterns
    if combined.contains("Could not resolve host") {
        return Err(format!(
            "Cannot connect to {} API: DNS resolution failed. \
             The workspace network is not properly configured.",
            api_name
        ));
    }
    if combined.contains("Connection refused") {
        return Err(format!(
            "Cannot connect to {} API: Connection refused. \
             Check if network access is blocked or if a proxy is required.",
            api_name
        ));
    }
    if combined.contains("Network is unreachable") {
        return Err(format!(
            "Cannot connect to {} API: Network is unreachable.",
            api_name
        ));
    }
    if combined.contains("Connection timed out") || combined.contains("Operation timed out") {
        return Err(format!(
            "Cannot connect to {} API: Connection timed out. \
             The network may be slow or firewalled.",
            api_name
        ));
    }
    if combined.contains("SSL") || combined.contains("certificate") {
        return Err(format!(
            "Cannot connect to {} API: SSL/TLS error. \
             Check if there's a proxy intercepting HTTPS traffic.",
            api_name
        ));
    }

    // Check for successful HTTP response (any non-000 code means we got an HTTP response).
    let code = stdout.trim();
    if !code.is_empty() && code != "000" {
        tracing::debug!("{} API connectivity check passed", api_name);
        return Ok(());
    }

    // If curl failed with no clear error
    if !output.status.success() {
        return Err(format!(
            "Cannot connect to {} API: Network check failed ({}). \
             Output: {}",
            api_name,
            format_exit_status(&output.status),
            combined.trim()
        ));
    }

    Err(format!(
        "Cannot connect to {} API: unexpected curl output (http_code={}). \
         Output: {}",
        api_name,
        if code.is_empty() { "<empty>" } else { code },
        combined.trim()
    ))
}

/// API endpoint configurations for different providers
struct ApiEndpoint {
    name: &'static str,
    url: &'static str,
    hostname: &'static str,
}

const ANTHROPIC_API: ApiEndpoint = ApiEndpoint {
    name: "Anthropic",
    url: "https://api.anthropic.com/v1/messages",
    hostname: "api.anthropic.com",
};

const OPENAI_API: ApiEndpoint = ApiEndpoint {
    name: "OpenAI",
    url: "https://api.openai.com/v1/models",
    hostname: "api.openai.com",
};

const GOOGLE_AI_API: ApiEndpoint = ApiEndpoint {
    name: "Google AI",
    url: "https://generativelanguage.googleapis.com/",
    hostname: "generativelanguage.googleapis.com",
};

const ZAI_API: ApiEndpoint = ApiEndpoint {
    name: "Z.AI",
    url: "https://api.z.ai/api/coding/paas/v4/chat/completions",
    hostname: "api.z.ai",
};

const MINIMAX_API: ApiEndpoint = ApiEndpoint {
    name: "Minimax",
    url: "https://api.minimax.io/v1/chat/completions",
    hostname: "api.minimax.io",
};

/// Proactive API connectivity check for Claude Code.
/// Tests basic internet, then DNS, then Anthropic API reachability.
async fn check_claudecode_connectivity(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
) -> Result<(), String> {
    // First check basic internet connectivity
    check_basic_internet_connectivity(workspace_exec, cwd).await?;

    // Then check DNS for Anthropic
    check_dns_resolution(workspace_exec, cwd, ANTHROPIC_API.hostname).await?;

    // Finally check Anthropic API reachability
    check_api_reachability(workspace_exec, cwd, ANTHROPIC_API.name, ANTHROPIC_API.url).await
}

/// Proactive API connectivity check for OpenCode.
/// Tests basic internet, then checks the appropriate API based on configured providers.
async fn check_opencode_connectivity(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    has_openai: bool,
    has_anthropic: bool,
    has_google: bool,
    has_zai: bool,
    has_minimax: bool,
) -> Result<(), String> {
    // First check basic internet connectivity
    check_basic_internet_connectivity(workspace_exec, cwd).await?;

    // Determine which API to check based on configured providers
    // Priority: OpenAI > Anthropic > Google > Z.AI > Minimax (most common first)
    // If none are explicitly configured, we already verified internet works
    let api = if has_openai {
        Some(&OPENAI_API)
    } else if has_anthropic {
        Some(&ANTHROPIC_API)
    } else if has_google {
        Some(&GOOGLE_AI_API)
    } else if has_zai {
        Some(&ZAI_API)
    } else if has_minimax {
        Some(&MINIMAX_API)
    } else {
        // No specific provider detected - basic internet check is sufficient
        // The actual API will be determined by OpenCode's config
        None
    };

    if let Some(api) = api {
        // Check DNS for the selected API
        check_dns_resolution(workspace_exec, cwd, api.hostname).await?;

        // Check API reachability
        check_api_reachability(workspace_exec, cwd, api.name, api.url).await
    } else {
        tracing::debug!("No specific provider detected, skipping API-specific connectivity check");
        Ok(())
    }
}

/// Returns the path to the Claude Code CLI that should be used.
/// If the CLI is not available, it will be auto-installed via bun or npm.
async fn ensure_claudecode_cli_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> Result<String, String> {
    // Allow wrapper commands like `bun /path/to/claude` by validating the
    // leading program (and optionally the first argument if it looks like a program).
    let mut parts = cli_path.split_whitespace();
    let program = parts.next().unwrap_or(cli_path);
    let arg0 = parts.next();

    // Check if the wrapper program exists.
    if command_available(workspace_exec, cwd, program).await {
        // If a wrapper is used (e.g. bun <script>), also sanity-check that the
        // wrapped target exists so we don't claim success and then fail at spawn time.
        if let Some(arg0) = arg0 {
            // Skip flags like `--something`; only validate likely program/path tokens.
            if !arg0.starts_with('-') && command_available(workspace_exec, cwd, arg0).await {
                return Ok(cli_path.to_string());
            }
        } else {
            return Ok(cli_path.to_string());
        }
    }

    // Also check bun's global bin directory (bun installs globals to ~/.cache/.bun/bin/)
    const BUN_GLOBAL_CLAUDE_PATH: &str = "/root/.cache/.bun/bin/claude";
    if command_available(workspace_exec, cwd, BUN_GLOBAL_CLAUDE_PATH).await {
        // Claude Code requires Node.js, but if only bun is available, use bun to run it
        if command_available(workspace_exec, cwd, "node").await {
            tracing::debug!(
                "Found Claude Code at {} (using node)",
                BUN_GLOBAL_CLAUDE_PATH
            );
            return Ok(BUN_GLOBAL_CLAUDE_PATH.to_string());
        } else if command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await {
            // Use full path to bun since it's not in PATH
            let bun_cmd = format!("/root/.bun/bin/bun {}", BUN_GLOBAL_CLAUDE_PATH);
            tracing::debug!(
                "Found Claude Code at {} (using bun to run it: {})",
                BUN_GLOBAL_CLAUDE_PATH,
                bun_cmd
            );
            return Ok(bun_cmd);
        } else if command_available(workspace_exec, cwd, "bun").await {
            // Bun is in PATH
            let bun_cmd = format!("bun {}", BUN_GLOBAL_CLAUDE_PATH);
            tracing::debug!(
                "Found Claude Code at {} (using bun to run it: {})",
                BUN_GLOBAL_CLAUDE_PATH,
                bun_cmd
            );
            return Ok(bun_cmd);
        } else {
            tracing::debug!(
                "Found Claude Code at {} but neither node nor bun available to run it",
                BUN_GLOBAL_CLAUDE_PATH
            );
        }
    }

    let auto_install = env_var_bool("SANDBOXED_SH_AUTO_INSTALL_CLAUDECODE", true);
    if !auto_install {
        return Err(format!(
            "Claude Code CLI '{}' not found in workspace. Install it or set CLAUDE_CLI_PATH.",
            cli_path
        ));
    }

    // Check for npm or bun as package manager (bun is preferred for speed)
    let has_npm = command_available(workspace_exec, cwd, "npm").await;
    tracing::debug!("Claude Code auto-install: npm available = {}", has_npm);

    let bun_in_path = command_available(workspace_exec, cwd, "bun").await;
    let bun_direct = command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await;
    let has_bun = bun_in_path || bun_direct;
    tracing::debug!(
        "Claude Code auto-install: bun in PATH = {}, bun at /root/.bun/bin/bun = {}, has_bun = {}",
        bun_in_path,
        bun_direct,
        has_bun
    );

    if !has_npm && !has_bun {
        return Err(format!(
            "Claude Code CLI '{}' not found and neither npm nor bun is available in the workspace. Install Node.js/npm or Bun in the workspace template, or set CLAUDE_CLI_PATH.",
            cli_path
        ));
    }

    // Use bun if available (faster), otherwise npm
    // Bun installs globals to ~/.cache/.bun/bin/
    let install_cmd = if has_bun {
        // Ensure Bun's bin is in PATH and install globally
        r#"export PATH="/root/.bun/bin:/root/.cache/.bun/bin:$PATH" && bun install -g @anthropic-ai/claude-code@latest"#
    } else {
        "npm install -g @anthropic-ai/claude-code@latest"
    };

    let args = vec!["-lc".to_string(), install_cmd.to_string()];
    let output = workspace_exec
        .output(cwd, "/bin/sh", &args, HashMap::new())
        .await
        .map_err(|e| format!("Failed to install Claude Code: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut message = String::new();
        if !stderr.trim().is_empty() {
            message.push_str(stderr.trim());
        }
        if !stdout.trim().is_empty() {
            if !message.is_empty() {
                message.push_str(" | ");
            }
            message.push_str(stdout.trim());
        }
        if message.is_empty() {
            message = "Claude Code install failed with no output".to_string();
        }
        return Err(format!("Claude Code install failed: {}", message));
    }

    // Check if claude is available in PATH or in bun's global bin
    if command_available(workspace_exec, cwd, cli_path).await {
        return Ok(cli_path.to_string());
    }
    if command_available(workspace_exec, cwd, BUN_GLOBAL_CLAUDE_PATH).await {
        // Claude Code requires Node.js, but if only bun is available, use bun to run it
        if command_available(workspace_exec, cwd, "node").await {
            return Ok(BUN_GLOBAL_CLAUDE_PATH.to_string());
        } else if command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await {
            // Use full path to bun since it's not in PATH
            return Ok(format!("/root/.bun/bin/bun {}", BUN_GLOBAL_CLAUDE_PATH));
        } else if command_available(workspace_exec, cwd, "bun").await {
            // Bun is in PATH
            return Ok(format!("bun {}", BUN_GLOBAL_CLAUDE_PATH));
        }
    }

    Err(format!(
        "Claude Code install completed but '{}' is still not available in workspace PATH.",
        cli_path
    ))
}

/// Returns the path to the Codex CLI that should be used.
async fn ensure_codex_cli_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> Result<String, String> {
    let program = cli_path.split(' ').next().unwrap_or(cli_path);

    // For container workspaces, the Codex npm package ships a Node.js ESM wrapper
    // that requires Node 20+. Containers often only have Node 18, which fails with
    // "Cannot use import statement outside a module". The package also ships a
    // native Rust binary in vendor/<triple>/codex/codex that works standalone.
    //
    // IMPORTANT: try the native binary copy BEFORE `command_available` — a previous
    // mission may have left the broken Node.js wrapper at /usr/local/bin/codex,
    // which passes `command_available` but fails at runtime.
    if workspace_exec.workspace.workspace_type == WorkspaceType::Container {
        if let Some(resolved) = resolve_host_executable(program) {
            let native = resolve_openai_codex_native_binary(&resolved);
            tracing::info!(
                host_path = %resolved.display(),
                native_binary = ?native.as_ref().map(|p| p.display().to_string()),
                "Codex CLI host resolution for container"
            );
            let to_copy = native.unwrap_or(resolved);
            if let Ok(dest_in_container) =
                copy_host_executable_into_container(&workspace_exec.workspace, &to_copy)
            {
                let rest = cli_path
                    .split_once(' ')
                    .map(|(_, rest)| rest)
                    .unwrap_or("")
                    .trim();
                let container_cli = if rest.is_empty() {
                    dest_in_container.clone()
                } else {
                    format!("{} {}", dest_in_container, rest)
                };

                let dest_program = container_cli
                    .split(' ')
                    .next()
                    .unwrap_or(&dest_in_container);
                if command_available(workspace_exec, cwd, dest_program).await {
                    tracing::info!(
                        host_source = %to_copy.display(),
                        container_path = %dest_program,
                        "Copied Codex CLI into container workspace"
                    );
                    return Ok(container_cli);
                }
            }
        }
    }

    // Check if already available (host workspace, or container with working binary)
    if command_available(workspace_exec, cwd, program).await {
        return Ok(cli_path.to_string());
    }

    // Check bun's global bin directories (bun installs globals to ~/.cache/.bun/bin/)
    const BUN_GLOBAL_CODEX_PATHS: &[&str] =
        &["/root/.cache/.bun/bin/codex", "/root/.bun/bin/codex"];
    for codex_path in BUN_GLOBAL_CODEX_PATHS {
        if command_available(workspace_exec, cwd, codex_path).await {
            tracing::info!(
                path = %codex_path,
                "Found Codex CLI in bun global bin"
            );
            return Ok(codex_path.to_string());
        }
    }

    // Auto-install Codex CLI if enabled (defaults to true)
    let auto_install = env_var_bool("SANDBOXED_SH_AUTO_INSTALL_CODEX", true);
    if !auto_install {
        return Err(format!(
            "Codex CLI '{}' not found in workspace. Install it or set CODEX_CLI_PATH.",
            cli_path
        ));
    }

    let has_bun = command_available(workspace_exec, cwd, "bun").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await;
    let has_npm = command_available(workspace_exec, cwd, "npm").await;

    if !has_bun && !has_npm {
        return Err(format!(
            "Codex CLI '{}' not found and neither npm nor bun is available in the workspace. Install Node.js/npm or Bun in the workspace template, or set CODEX_CLI_PATH.",
            cli_path
        ));
    }

    let install_cmd = if has_bun {
        r#"export PATH="/root/.bun/bin:/root/.cache/.bun/bin:$PATH" && bun install -g @openai/codex@latest 2>&1"#
    } else {
        "npm install -g @openai/codex@latest 2>&1"
    };

    tracing::info!(
        installer = if has_bun { "bun" } else { "npm" },
        "Auto-installing Codex CLI"
    );

    let output = workspace_exec
        .output(
            cwd,
            "/bin/sh",
            &["-lc".to_string(), install_cmd.to_string()],
            std::collections::HashMap::new(),
        )
        .await
        .map_err(|e| format!("Failed to install Codex CLI: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut message = String::new();
        if !stderr.trim().is_empty() {
            message.push_str(stderr.trim());
        }
        if !stdout.trim().is_empty() {
            if !message.is_empty() {
                message.push_str(" | ");
            }
            message.push_str(stdout.trim());
        }
        if message.is_empty() {
            message = "Codex CLI install failed with no output".to_string();
        }
        return Err(format!("Codex CLI install failed: {}", message));
    }

    // Re-check availability after install
    if command_available(workspace_exec, cwd, cli_path).await {
        return Ok(cli_path.to_string());
    }
    for codex_path in BUN_GLOBAL_CODEX_PATHS {
        if command_available(workspace_exec, cwd, codex_path).await {
            tracing::info!(
                path = %codex_path,
                "Codex CLI available after auto-install"
            );
            return Ok(codex_path.to_string());
        }
    }

    Err(format!(
        "Codex CLI install completed but '{}' is still not available in workspace PATH.",
        cli_path
    ))
}

fn resolve_openai_codex_native_binary(
    wrapper_path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let real = match std::fs::canonicalize(wrapper_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(
                path = %wrapper_path.display(),
                error = %e,
                "Failed to canonicalize Codex wrapper path"
            );
            return None;
        }
    };

    let file_name = real.file_name().and_then(|n| n.to_str());
    tracing::debug!(
        wrapper = %wrapper_path.display(),
        canonical = %real.display(),
        file_name = ?file_name,
        "Resolving Codex native binary"
    );

    let is_codex_wrapper =
        file_name.is_some_and(|n| n == "codex.js") || is_codex_node_wrapper(&real);

    if !is_codex_wrapper {
        return None;
    }

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => {
            tracing::debug!(os, arch, "No Codex native binary triple for this platform");
            return None;
        }
    };

    let binary_name = if cfg!(windows) { "codex.exe" } else { "codex" };

    let search_paths = resolve_codex_native_binary_search_paths(&real, triple, binary_name);

    for native in search_paths {
        if native.is_file() {
            tracing::info!(
                native_path = %native.display(),
                "Found Codex native binary"
            );
            return Some(native);
        }
        tracing::debug!(
            candidate = %native.display(),
            "Codex native binary not found at candidate path"
        );
    }

    tracing::debug!("Codex native binary not found in any search path");
    None
}

fn is_codex_node_wrapper(path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };

    let first_line = content.lines().next().unwrap_or("");
    let has_node_shebang =
        first_line.starts_with("#!/usr/bin/env node") || first_line.starts_with("#!/usr/bin/node");

    if !has_node_shebang {
        return false;
    }

    let lower = content.to_lowercase();
    lower.contains("@openai/codex")
        || lower.contains("codex-linux-x64")
        || lower.contains("codex-linux-arm64")
        || lower.contains("codex-darwin-x64")
        || lower.contains("codex-darwin-arm64")
}

fn codex_npm_package_name(triple: &str) -> &'static str {
    match triple {
        "x86_64-unknown-linux-musl" => "codex-linux-x64",
        "aarch64-unknown-linux-musl" => "codex-linux-arm64",
        "x86_64-apple-darwin" => "codex-darwin-x64",
        "aarch64-apple-darwin" => "codex-darwin-arm64",
        _ => "codex-linux-x64",
    }
}

fn resolve_codex_native_binary_search_paths(
    wrapper_path: &std::path::Path,
    triple: &str,
    binary_name: &str,
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let npm_pkg = codex_npm_package_name(triple);

    let binary_path = |base: &std::path::Path| {
        base.join("vendor")
            .join(triple)
            .join("codex")
            .join(binary_name)
    };

    if let Some(bin_dir) = wrapper_path.parent() {
        if let Some(package_root) = bin_dir.parent() {
            paths.push(binary_path(package_root));

            let nested_optional = package_root
                .join("node_modules")
                .join("@openai")
                .join(npm_pkg);
            paths.push(binary_path(&nested_optional));
        }

        if let Some(node_modules) = bin_dir.parent() {
            let sibling_optional = node_modules.join("@openai").join(npm_pkg);
            paths.push(binary_path(&sibling_optional));
        }
    }

    if let Ok(npm_prefix) = std::env::var("npm_config_prefix") {
        let npm_optional = std::path::PathBuf::from(&npm_prefix)
            .join("lib")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join(npm_pkg);
        paths.push(binary_path(&npm_optional));
    }

    for prefix in ["/usr/local", "/usr"] {
        let npm_optional = std::path::PathBuf::from(prefix)
            .join("lib")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join(npm_pkg);
        paths.push(binary_path(&npm_optional));
    }

    if let Ok(home) = std::env::var("HOME") {
        let bun_optional = std::path::PathBuf::from(&home)
            .join(".bun")
            .join("install")
            .join("global")
            .join("node_modules")
            .join("@openai")
            .join(npm_pkg);
        paths.push(binary_path(&bun_optional));

        let bun_cache_optional = std::path::PathBuf::from(&home)
            .join(".cache")
            .join(".bun")
            .join("install")
            .join("global")
            .join("node_modules")
            .join("@openai")
            .join(npm_pkg);
        paths.push(binary_path(&bun_cache_optional));
    }

    paths
}

fn resolve_host_executable(program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        let p = std::path::PathBuf::from(program);
        if p.is_file() {
            return Some(p);
        }
        return None;
    }

    let path_var = std::env::var("PATH").ok()?;
    for dir in path_var.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = std::path::Path::new(dir).join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn copy_host_executable_into_container(
    workspace: &crate::workspace::Workspace,
    host_executable: &std::path::Path,
) -> Result<String, String> {
    let name = host_executable
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Host executable has invalid filename".to_string())?;

    let dest_dir = workspace.path.join("usr").join("local").join("bin");
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("Failed to create container /usr/local/bin: {}", e))?;

    let dest = dest_dir.join(name);
    let tmp = dest_dir.join(format!("{}.tmp", name));
    std::fs::copy(host_executable, &tmp).map_err(|e| {
        format!(
            "Failed to copy host executable {} into container: {}",
            host_executable.display(),
            e
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }

    std::fs::rename(&tmp, &dest)
        .map_err(|e| format!("Failed to finalize container executable: {}", e))?;

    Ok(format!("/usr/local/bin/{}", name))
}

fn runner_is_oh_my_opencode(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "oh-my-opencode")
        .unwrap_or(false)
}

async fn resolve_opencode_installer_fetcher(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
) -> Option<String> {
    let curl_candidates = ["curl", "/usr/bin/curl", "/bin/curl"];
    for candidate in curl_candidates {
        if command_available(workspace_exec, cwd, candidate).await {
            return Some(format!("{} -fsSL https://opencode.ai/install", candidate));
        }
    }

    let wget_candidates = ["wget", "/usr/bin/wget", "/bin/wget"];
    for candidate in wget_candidates {
        if command_available(workspace_exec, cwd, candidate).await {
            return Some(format!("{} -qO- https://opencode.ai/install", candidate));
        }
    }

    None
}

async fn opencode_binary_available(workspace_exec: &WorkspaceExec, cwd: &std::path::Path) -> bool {
    if command_available(workspace_exec, cwd, "opencode").await {
        return true;
    }
    if command_available(workspace_exec, cwd, "/usr/local/bin/opencode").await {
        return true;
    }
    if workspace_exec.workspace.workspace_type == WorkspaceType::Container
        && workspace::use_nspawn_for_workspace(&workspace_exec.workspace)
    {
        if command_available(workspace_exec, cwd, "/root/.opencode/bin/opencode").await {
            return true;
        }
    } else if let Ok(home) = std::env::var("HOME") {
        let path = format!("{}/.opencode/bin/opencode", home);
        if command_available(workspace_exec, cwd, &path).await {
            return true;
        }
    }
    false
}

async fn cleanup_opencode_listeners(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    port: Option<&str>,
) {
    let port = port
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(4096);
    let mut args = Vec::new();
    args.push("-lc".to_string());
    args.push(format!(
        "if command -v lsof >/dev/null 2>&1; then \
               pids=$(lsof -t -iTCP:{port} -sTCP:LISTEN 2>/dev/null || true); \
               if [ -n \"$pids\" ]; then kill -9 $pids || true; fi; \
             fi",
        port = port
    ));
    let _ = workspace_exec
        .output(cwd, "/bin/sh", &args, HashMap::new())
        .await;
}

async fn ensure_opencode_cli_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
) -> Result<(), String> {
    if opencode_binary_available(workspace_exec, cwd).await {
        return Ok(());
    }

    let auto_install = env_var_bool("SANDBOXED_SH_AUTO_INSTALL_OPENCODE", true);
    if !auto_install {
        return Err(
            "OpenCode CLI 'opencode' not found in workspace. Install it or disable OpenCode."
                .to_string(),
        );
    }

    let fetcher = resolve_opencode_installer_fetcher(workspace_exec, cwd).await.ok_or_else(|| {
        "OpenCode CLI 'opencode' not found and neither curl nor wget is available in the workspace. Install curl/wget in the workspace template or disable OpenCode."
            .to_string()
    })?;

    let mut args = Vec::new();
    args.push("-lc".to_string());
    // Use explicit /root path for container workspaces since $HOME may not be set in nspawn
    // Try both /root and $HOME to cover both container and host workspaces
    args.push(
        format!(
            "{} | bash -s -- --no-modify-path \
        && for bindir in /root/.opencode/bin \"$HOME/.opencode/bin\"; do \
            if [ -x \"$bindir/opencode\" ]; then install -m 0755 \"$bindir/opencode\" /usr/local/bin/opencode && break; fi; \
        done"
            , fetcher
        ),
    );
    let output = workspace_exec
        .output(cwd, "/bin/sh", &args, HashMap::new())
        .await
        .map_err(|e| format!("Failed to run OpenCode installer: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut message = String::new();
        if !stderr.trim().is_empty() {
            message.push_str(stderr.trim());
        }
        if !stdout.trim().is_empty() {
            if !message.is_empty() {
                message.push_str(" | ");
            }
            message.push_str(stdout.trim());
        }
        if message.is_empty() {
            message = "OpenCode install failed with no output".to_string();
        }
        return Err(format!("OpenCode install failed: {}", message));
    }

    if !opencode_binary_available(workspace_exec, cwd).await {
        return Err(
            "OpenCode install completed but 'opencode' is still not available in workspace PATH."
                .to_string(),
        );
    }

    Ok(())
}

/// Result of a backend preflight check
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendPreflightResult {
    pub backend_id: String,
    pub available: bool,
    pub cli_available: bool,
    pub auto_install_possible: bool,
    pub missing_dependencies: Vec<String>,
    pub message: Option<String>,
}

/// Check if a backend can run in the given workspace.
/// This performs a lightweight check without actually installing anything.
pub async fn check_backend_prerequisites(
    workspace: &Workspace,
    backend_id: &str,
    cli_path: Option<&str>,
) -> BackendPreflightResult {
    let workspace_exec = WorkspaceExec::new(workspace.clone());
    let cwd = &workspace.path;

    match backend_id {
        "claudecode" => {
            let cli = cli_path.unwrap_or("claude");
            check_claudecode_prerequisites(&workspace_exec, cwd, cli).await
        }
        "opencode" => check_opencode_prerequisites(&workspace_exec, cwd).await,
        "codex" => {
            let cli = cli_path.unwrap_or("codex");
            check_codex_prerequisites(&workspace_exec, cwd, cli).await
        }
        "amp" => {
            let cli = cli_path.unwrap_or("amp");
            check_amp_prerequisites(&workspace_exec, cwd, cli).await
        }
        _ => BackendPreflightResult {
            backend_id: backend_id.to_string(),
            available: false,
            cli_available: false,
            auto_install_possible: false,
            missing_dependencies: vec![format!("unknown backend: {}", backend_id)],
            message: Some(format!(
                "Unknown backend '{}'. Supported backends: claudecode, opencode, codex, amp",
                backend_id
            )),
        },
    }
}

async fn check_claudecode_prerequisites(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> BackendPreflightResult {
    let mut missing = Vec::new();
    let program = cli_path.split_whitespace().next().unwrap_or(cli_path);

    let cli_available = command_available(workspace_exec, cwd, program).await
        || command_available(workspace_exec, cwd, "/root/.cache/.bun/bin/claude").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/claude").await;

    if cli_available {
        return BackendPreflightResult {
            backend_id: "claudecode".to_string(),
            available: true,
            cli_available: true,
            auto_install_possible: false,
            missing_dependencies: vec![],
            message: None,
        };
    }

    let has_npm = command_available(workspace_exec, cwd, "npm").await;
    let has_bun = command_available(workspace_exec, cwd, "bun").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await;

    if !has_npm && !has_bun {
        missing.push("npm or bun".to_string());
    }

    let auto_install_possible = has_npm || has_bun;

    BackendPreflightResult {
        backend_id: "claudecode".to_string(),
        available: auto_install_possible,
        cli_available: false,
        auto_install_possible,
        missing_dependencies: missing,
        message: if !auto_install_possible {
            Some("Claude Code CLI not found and neither npm nor bun is available. Install Node.js/npm or Bun in the workspace template.".to_string())
        } else {
            Some("Claude Code CLI not found but can be auto-installed via npm/bun.".to_string())
        },
    }
}

async fn check_opencode_prerequisites(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
) -> BackendPreflightResult {
    let mut missing = Vec::new();

    let cli_available = opencode_binary_available(workspace_exec, cwd).await;

    if cli_available {
        return BackendPreflightResult {
            backend_id: "opencode".to_string(),
            available: true,
            cli_available: true,
            auto_install_possible: false,
            missing_dependencies: vec![],
            message: None,
        };
    }

    let has_curl = command_available(workspace_exec, cwd, "curl").await;
    let has_wget = command_available(workspace_exec, cwd, "wget").await;

    if !has_curl && !has_wget {
        missing.push("curl or wget".to_string());
    }

    let auto_install_possible = has_curl || has_wget;

    BackendPreflightResult {
        backend_id: "opencode".to_string(),
        available: auto_install_possible,
        cli_available: false,
        auto_install_possible,
        missing_dependencies: missing,
        message: if !auto_install_possible {
            Some("OpenCode CLI not found and neither curl nor wget is available. Install curl/wget in the workspace template.".to_string())
        } else {
            Some("OpenCode CLI not found but can be auto-installed via curl/wget.".to_string())
        },
    }
}

async fn check_codex_prerequisites(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> BackendPreflightResult {
    let mut missing = Vec::new();
    let program = cli_path.split_whitespace().next().unwrap_or(cli_path);

    let cli_available = command_available(workspace_exec, cwd, program).await
        || command_available(workspace_exec, cwd, "/root/.cache/.bun/bin/codex").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/codex").await;

    if cli_available {
        return BackendPreflightResult {
            backend_id: "codex".to_string(),
            available: true,
            cli_available: true,
            auto_install_possible: false,
            missing_dependencies: vec![],
            message: None,
        };
    }

    let has_npm = command_available(workspace_exec, cwd, "npm").await;
    let has_bun = command_available(workspace_exec, cwd, "bun").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await;

    if !has_npm && !has_bun {
        missing.push("npm or bun".to_string());
    }

    let auto_install_possible = has_npm || has_bun;

    BackendPreflightResult {
        backend_id: "codex".to_string(),
        available: auto_install_possible,
        cli_available: false,
        auto_install_possible,
        missing_dependencies: missing,
        message: if !auto_install_possible {
            Some("Codex CLI not found and neither npm nor bun is available. Install Node.js/npm or Bun in the workspace template.".to_string())
        } else {
            Some("Codex CLI not found but can be auto-installed via npm/bun.".to_string())
        },
    }
}

async fn check_amp_prerequisites(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> BackendPreflightResult {
    let program = cli_path.split_whitespace().next().unwrap_or(cli_path);

    let cli_available = command_available(workspace_exec, cwd, program).await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/amp").await
        || command_available(workspace_exec, cwd, "/root/.cache/.bun/bin/amp").await;

    if cli_available {
        return BackendPreflightResult {
            backend_id: "amp".to_string(),
            available: true,
            cli_available: true,
            auto_install_possible: false,
            missing_dependencies: vec![],
            message: None,
        };
    }

    let has_npm = command_available(workspace_exec, cwd, "npm").await;
    let has_bun = command_available(workspace_exec, cwd, "bun").await
        || command_available(workspace_exec, cwd, "/root/.bun/bin/bun").await;

    let auto_install_possible = has_npm || has_bun;

    BackendPreflightResult {
        backend_id: "amp".to_string(),
        available: auto_install_possible,
        cli_available: false,
        auto_install_possible,
        missing_dependencies: if !auto_install_possible {
            vec!["npm or bun".to_string()]
        } else {
            vec![]
        },
        message: if !auto_install_possible {
            Some("Amp CLI not found and neither npm nor bun is available. Install Node.js/npm or Bun in the workspace template.".to_string())
        } else {
            Some("Amp CLI not found but can be auto-installed via npm/bun.".to_string())
        },
    }
}

/// Execute a turn using OpenCode CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
///
/// This uses the `oh-my-opencode run` CLI which creates an embedded OpenCode server,
/// enabling per-workspace isolation without network issues.
#[allow(clippy::too_many_arguments)]
pub async fn run_opencode_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    _model_effort: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
) -> AgentResult {
    use super::ai_providers::{
        ensure_anthropic_oauth_token_valid, ensure_google_oauth_token_valid,
        ensure_openai_oauth_token_valid,
    };
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Determine CLI runner: prefer backend config, then env var, then try bunx/npx
    // We use 'bunx oh-my-opencode run' or 'npx oh-my-opencode run' for per-workspace execution.
    let workspace_exec = WorkspaceExec::new(workspace.clone());
    if let Err(err) = ensure_opencode_cli_available(&workspace_exec, work_dir).await {
        tracing::error!("{}", err);
        return AgentResult::failure(err, 0).with_terminal_reason(TerminalReason::LlmError);
    }

    let opencode_config_dir_host = work_dir.join(".opencode");

    // Resolve the model: explicit override > agent config > env var defaults.
    // Agent config (oh-my-opencode.json) is checked before env vars so that
    // config profiles with agent-specific models take precedence over global
    // default model env vars.
    let mut resolved_model = model
        .map(|m| m.to_string())
        .or_else(|| resolve_opencode_model_from_config(&opencode_config_dir_host, agent))
        .or_else(|| {
            std::env::var("SANDBOXED_SH_OPENCODE_DEFAULT_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("OPENCODE_DEFAULT_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    let auth_state = detect_opencode_provider_auth(Some(app_working_dir));
    let has_openai = auth_state.has_openai;
    let has_anthropic = auth_state.has_anthropic;
    let has_google = auth_state.has_google;
    let has_any_provider = has_openai || has_anthropic || has_google || auth_state.has_other;

    let mut provider_hint = resolved_model
        .as_deref()
        .and_then(|m| m.split_once('/'))
        .map(|(provider, _)| provider.to_lowercase());

    let configured_providers = &auth_state.configured_providers;
    let provider_available = |provider: &str| -> bool {
        match provider {
            "anthropic" | "claude" => has_anthropic,
            "openai" | "codex" => has_openai,
            "google" | "gemini" => has_google,
            // For known catalog providers (xai, zai, cerebras), check if they are actually configured
            p if super::providers::DEFAULT_CATALOG_PROVIDER_IDS.contains(&p) => {
                configured_providers.contains(p)
            }
            // Unknown providers pass through (custom escape hatch)
            _ => true,
        }
    };

    if let Some(provider) = provider_hint.as_deref() {
        if !provider_available(provider) {
            tracing::warn!(
                mission_id = %mission_id,
                provider = %provider,
                "Requested OpenCode model provider is not configured; falling back to available providers"
            );
            resolved_model = None;
            provider_hint = None;
        }
    }

    // Capture model override AFTER provider availability check so that cleared
    // overrides are not passed to sync_opencode_agent_config.
    let default_model_override = resolved_model.clone();

    let needs_google = matches!(provider_hint.as_deref(), Some("google" | "gemini"));

    let fallback_provider = if has_openai {
        Some("openai")
    } else if has_google {
        Some("google")
    } else if has_anthropic {
        Some("anthropic")
    } else {
        None
    };

    let refresh_provider = provider_hint.as_deref().or(fallback_provider);
    let refresh_result = match refresh_provider {
        Some("anthropic") | Some("claude") => ensure_anthropic_oauth_token_valid().await,
        Some("openai") | Some("codex") => ensure_openai_oauth_token_valid().await,
        Some("google") | Some("gemini") => ensure_google_oauth_token_valid().await,
        None => {
            if has_any_provider {
                Ok(())
            } else {
                Err(
                    "No OpenCode providers configured. Add a provider in Settings → AI Providers."
                        .to_string(),
                )
            }
        }
        _ => Ok(()),
    };

    if let Err(err) = refresh_result {
        let label = refresh_provider
            .map(|v| v.to_string())
            .unwrap_or_else(|| "provider".to_string());
        let err_msg = format!(
            "{} OAuth token refresh failed: {}. Please re-authenticate in Settings → AI Providers.",
            label, err
        );
        tracing::warn!(mission_id = %mission_id, "{}", err_msg);
        return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
    }

    // Note: Provider concurrency semaphores (previously used for ZAI) have been
    // removed. For `builtin/*` models, rate limit handling is done by the proxy's
    // waterfall failover and per-account health tracking in ProviderHealthTracker.
    // For direct provider models (e.g. `zai/*`), OpenCode's own retry logic
    // handles 429s. The old semaphore only serialized requests — it did not do
    // failover — so removing it trades slightly higher 429 rates under heavy
    // concurrency for lower latency in the common case.

    let configured_runner = get_backend_string_setting("opencode", "cli_path")
        .or_else(|| std::env::var("OPENCODE_CLI_PATH").ok());

    let mut runner_is_direct = false;
    let cli_runner = if let Some(path) = configured_runner {
        if command_available(&workspace_exec, work_dir, &path).await {
            runner_is_direct = runner_is_oh_my_opencode(&path);
            path
        } else {
            let err_msg = format!(
                "OpenCode CLI runner '{}' not found in workspace. Install it or update OPENCODE_CLI_PATH.",
                path
            );
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    } else {
        // Prefer bunx for oh-my-opencode (avoids version conflicts from npm global installs)
        if command_available(&workspace_exec, work_dir, "bunx").await {
            "bunx".to_string()
        } else if command_available(&workspace_exec, work_dir, "npx").await {
            "npx".to_string()
        } else {
            let err_msg =
                "No OpenCode CLI runner found in workspace (expected bunx or npx).".to_string();
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Proactive network connectivity check - fail fast if API is unreachable
    // This catches DNS/network issues immediately instead of waiting for a timeout
    if let Err(err_msg) = check_opencode_connectivity(
        &workspace_exec,
        work_dir,
        has_openai,
        has_anthropic,
        has_google,
        auth_state.has_zai,
        auth_state.configured_providers.contains("minimax"),
    )
    .await
    {
        tracing::error!(mission_id = %mission_id, "{}", err_msg);
        return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
    }

    tracing::info!(
        mission_id = %mission_id,
        work_dir = %work_dir.display(),
        workspace_type = ?workspace.workspace_type,
        model = ?resolved_model,
        agent = ?agent,
        cli_runner = %cli_runner,
        "Starting OpenCode execution via WorkspaceExec (per-workspace CLI mode)"
    );

    let work_dir_env = workspace_path_for_env(workspace, work_dir);
    let work_dir_arg = work_dir_env.to_string_lossy().to_string();
    let opencode_config_dir_env = workspace_path_for_env(workspace, &opencode_config_dir_host);
    ensure_oh_my_opencode_config(
        &workspace_exec,
        work_dir,
        &opencode_config_dir_host,
        &opencode_config_dir_env,
        &cli_runner,
        runner_is_direct,
        has_openai,
        has_anthropic,
        needs_google,
    )
    .await;
    if model.is_some() {
        if let Some(model_override) = resolved_model.as_deref() {
            apply_model_override_to_oh_my_opencode(&opencode_config_dir_host, model_override);
        }
    }
    sync_opencode_agent_config(
        &opencode_config_dir_host,
        default_model_override.as_deref(),
        has_openai,
        has_anthropic,
        has_google,
    );
    let mut model_used = resolved_model.clone();
    let agent_model = resolve_opencode_model_from_config(&opencode_config_dir_host, agent);
    if model_used.is_none() {
        model_used = agent_model.clone();
    }
    if resolved_model.is_none() {
        resolved_model = agent_model.clone();
    }
    // Inject provider definitions into opencode.json for models not in
    // OpenCode's built-in snapshot.  We do this *after* sync_opencode_agent_config
    // so all writes to opencode.json's model/agent sections are finished first.
    if let Some(model_override) = resolved_model.as_deref() {
        ensure_opencode_provider_for_model(&opencode_config_dir_host, model_override);
    }
    if let Some(ref am) = agent_model {
        if resolved_model.as_deref() != Some(am) {
            ensure_opencode_provider_for_model(&opencode_config_dir_host, am);
        }
    }
    ensure_opencode_providers_for_omo_config(&opencode_config_dir_host);
    if needs_google {
        if let Some(project_id) = detect_google_project_id() {
            ensure_opencode_google_project_id(&opencode_config_dir_host, &project_id);
        }
        let gemini_plugin = "opencode-gemini-auth@latest";
        ensure_opencode_plugin_specs(&opencode_config_dir_host, &[gemini_plugin]);
        ensure_opencode_plugin_installed(
            &workspace_exec,
            work_dir,
            &opencode_config_dir_host,
            &opencode_config_dir_env,
            gemini_plugin,
        )
        .await;
    }
    if has_openai {
        let openai_plugin = "opencode-openai-codex-auth@latest";
        ensure_opencode_plugin_specs(&opencode_config_dir_host, &[openai_plugin]);
        ensure_opencode_plugin_installed(
            &workspace_exec,
            work_dir,
            &opencode_config_dir_host,
            &opencode_config_dir_env,
            openai_plugin,
        )
        .await;
    }
    // Pre-cache oh-my-opencode via bunx/npx so the port override patch can find the CLI JS.
    // When the runner is bunx/npx, the package isn't cached until the actual run command.
    // Without pre-caching, the patch fails and the port falls back to 4096, which may
    // conflict with a standalone opencode.service on the host (shared network namespace).
    if !runner_is_direct && find_oh_my_opencode_cli_js(workspace).is_none() {
        tracing::debug!(
            mission_id = %mission_id,
            cli_runner = %cli_runner,
            "Pre-caching oh-my-opencode for port override patch"
        );
        let precache_args = vec!["oh-my-opencode".to_string(), "--version".to_string()];
        let _ = workspace_exec
            .output(work_dir, &cli_runner, &precache_args, HashMap::new())
            .await;
    }
    // Patch JS source for older (pre-v3) JS-based oh-my-opencode versions.
    // For v3+ compiled binaries, the wrapper script handles port override instead.
    let _ = patch_oh_my_opencode_port_override(workspace);

    // Build CLI arguments for oh-my-opencode run
    // The 'run' command takes a prompt and executes it with completion detection
    // Arguments: bunx oh-my-opencode run [--agent <agent>] [--directory <path>] <message>
    // Note: --timeout was removed in oh-my-opencode 3.7.2; the runner handles
    // completion detection internally so an explicit timeout is not needed.
    //
    // The message is written to a temp file and passed via $(cat ...) to avoid
    // argument splitting issues when multi-line messages go through
    // systemd-nspawn or nsenter shell wrappers.
    let prompt_file_host = work_dir.join(".sandboxed-sh-prompt.txt");
    if let Err(e) = std::fs::write(&prompt_file_host, message) {
        let err_msg = format!("Failed to write prompt file: {}", e);
        tracing::error!("{}", err_msg);
        return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
    }
    let prompt_file_env = workspace_path_for_env(workspace, &prompt_file_host);
    let prompt_file_arg = prompt_file_env.to_string_lossy().to_string();

    // Build the oh-my-opencode run command as a shell string so that
    // $(cat <file>) correctly expands the message as a single argument.
    let shell_escape = |s: &str| -> String {
        let mut escaped = String::with_capacity(s.len() + 2);
        escaped.push('\'');
        for ch in s.chars() {
            if ch == '\'' {
                escaped.push_str("'\"'\"'");
            } else {
                escaped.push(ch);
            }
        }
        escaped.push('\'');
        escaped
    };

    let mut shell_cmd = String::new();
    if runner_is_direct {
        shell_cmd.push_str(&shell_escape(&cli_runner));
        shell_cmd.push_str(" run");
    } else {
        shell_cmd.push_str(&shell_escape(&cli_runner));
        shell_cmd.push_str(" oh-my-opencode run");
    }

    if let Some(a) = agent {
        shell_cmd.push_str(" --agent ");
        shell_cmd.push_str(&shell_escape(a));
    }

    shell_cmd.push_str(" --directory ");
    shell_cmd.push_str(&shell_escape(&work_dir_arg));

    // Read message from file via command substitution to guarantee a single argument
    shell_cmd.push_str(" \"$(cat ");
    shell_cmd.push_str(&shell_escape(&prompt_file_arg));
    shell_cmd.push_str(")\"");

    let args = vec!["-c".to_string(), shell_cmd.clone()];
    let cli_runner_shell = "/bin/sh".to_string();

    tracing::debug!(
        mission_id = %mission_id,
        runner_is_direct = runner_is_direct,
        shell_cmd = %shell_cmd,
        prompt_file = %prompt_file_arg,
        "OpenCode CLI args prepared (shell wrapper)"
    );

    // Build environment variables
    let mut env: HashMap<String, String> = HashMap::new();
    let opencode_auth = sync_opencode_auth_to_workspace(workspace, app_working_dir);

    // Allow per-mission OpenCode server port; default to an allocated free port.
    let requested_port = std::env::var("SANDBOXED_SH_OPENCODE_SERVER_PORT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let mut opencode_port = requested_port
        .clone()
        .or_else(|| allocate_opencode_server_port().map(|p| p.to_string()))
        .unwrap_or_else(|| "0".to_string());

    if opencode_port == "0" {
        opencode_port = "4096".to_string();
    }

    env.insert("OPENCODE_SERVER_PORT".to_string(), opencode_port.clone());
    if let Ok(host) = std::env::var("SANDBOXED_SH_OPENCODE_SERVER_HOSTNAME") {
        if !host.trim().is_empty() {
            env.insert("OPENCODE_SERVER_HOSTNAME".to_string(), host);
        }
    }
    tracing::info!(
        mission_id = %mission_id,
        opencode_port = %opencode_port,
        "OpenCode server port selected"
    );

    // Pass the model if specified
    if let Some(m) = resolved_model.as_deref() {
        // Parse provider/model format
        if let Some((provider, model_id)) = m.split_once('/') {
            env.insert("OPENCODE_PROVIDER".to_string(), provider.to_string());
            env.insert("OPENCODE_MODEL".to_string(), model_id.to_string());
        } else {
            env.insert("OPENCODE_MODEL".to_string(), m.to_string());
        }
    }

    // Ensure OpenCode uses workspace-local config
    let opencode_config_path =
        workspace_path_for_env(workspace, &opencode_config_dir_host.join("opencode.json"));
    env.insert(
        "OPENCODE_CONFIG_DIR".to_string(),
        opencode_config_dir_env.to_string_lossy().to_string(),
    );
    env.insert(
        "OPENCODE_CONFIG".to_string(),
        opencode_config_path.to_string_lossy().to_string(),
    );

    if let Some(project_id) = detect_google_project_id() {
        env.entry("GOOGLE_CLOUD_PROJECT".to_string())
            .or_insert_with(|| project_id.clone());
        env.entry("GOOGLE_PROJECT_ID".to_string())
            .or_insert(project_id);
    }

    if let Some(permissive) = get_backend_bool_setting("opencode", "permissive") {
        env.insert("OPENCODE_PERMISSIVE".to_string(), permissive.to_string());
    } else if let Ok(value) = std::env::var("OPENCODE_PERMISSIVE") {
        if !value.trim().is_empty() {
            env.insert("OPENCODE_PERMISSIVE".to_string(), value);
        }
    }

    // Disable ANSI color codes for easier parsing
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("FORCE_COLOR".to_string(), "0".to_string());

    // Set non-interactive mode
    env.insert("OPENCODE_NON_INTERACTIVE".to_string(), "true".to_string());
    env.insert("OPENCODE_RUN".to_string(), "true".to_string());
    env.entry("SANDBOXED_SH_WORKSPACE_TYPE".to_string())
        .or_insert_with(|| workspace.workspace_type.as_str().to_string());

    if let Some(auth) = opencode_auth.as_ref() {
        let providers = apply_opencode_auth_env(auth, &mut env);
        if !providers.is_empty() {
            tracing::info!(
                mission_id = %mission_id,
                providers = ?providers,
                "Loaded OpenCode auth credentials for workspace"
            );
        }
    }

    prepend_opencode_bin_to_path(&mut env, workspace);

    // Install the opencode serve wrapper AFTER prepend_opencode_bin_to_path so the
    // wrapper dir (/tmp/.sandboxed-sh-bin) is prepended last and takes priority over
    // the real binary at ~/.opencode/bin/opencode.
    // oh-my-opencode v3+ is a compiled binary that spawns `opencode serve --port=4096`;
    // the wrapper intercepts this and overrides the port.
    if opencode_port != "4096" {
        install_opencode_serve_port_wrapper(&mut env, workspace, &opencode_port);
    }

    cleanup_opencode_listeners(&workspace_exec, work_dir, Some(&opencode_port)).await;

    // Use WorkspaceExec to spawn the CLI in the correct workspace context.
    // We invoke /bin/sh -c '...' so the prompt file is read via $(cat ...)
    // and passed as a single argument regardless of workspace type.
    let mut child = match workspace_exec
        .spawn_streaming(work_dir, &cli_runner_shell, &args, env)
        .await
    {
        Ok(child) => child,
        Err(e) => {
            let err_msg = format!("Failed to start OpenCode CLI: {}", e);
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Get stdout and stderr for reading output
    // oh-my-opencode run writes:
    // - stdout: assistant text output (the actual response)
    // - stderr: event logs (tool calls, results, session status)
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let err_msg = "Failed to capture OpenCode stdout";
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg.to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    let stderr = child.stderr.take();

    let mut final_result = String::new();
    let mut had_error = false;
    let mut final_result_from_nonzero_exit = false;
    let session_id_capture: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stderr_text_buffer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let stderr_recent_lines: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(32)));
    // Accumulates the latest full-text snapshot from SSE TextDelta events.
    // Used as a fallback when stdout JSON and session storage both fail —
    // this buffer contains exactly what was streamed to the dashboard,
    // unlike stderr which truncates long content (fixes #158).
    let sse_text_buffer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let sse_emitted_thinking = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_emitted_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_done_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_error_message: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let rate_limit_detected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_cancel = CancellationToken::new();
    let (sse_complete_tx, mut sse_complete_rx) = tokio::sync::watch::channel(false);
    let (sse_session_idle_tx, mut sse_session_idle_rx) = tokio::sync::watch::channel(false);
    let (sse_retry_tx, mut sse_retry_rx) = tokio::sync::watch::channel(0u32);
    let last_activity = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let (text_output_tx, mut text_output_rx) = tokio::sync::watch::channel(false);
    // Track active tool call depth: incremented on ToolCall, decremented on ToolResult.
    // Used to skip inactivity timeouts during long tool runs (builds, tests, etc.).
    let (sse_tool_depth_tx, sse_tool_depth_rx) = tokio::sync::watch::channel(0u32);

    // oh-my-opencode doesn't support --format json, so use SSE curl for events.
    let use_json_stdout = false;
    let sse_handle = if !use_json_stdout
        && command_available(&workspace_exec, work_dir, "curl").await
    {
        let workspace_exec = workspace_exec.clone();
        let work_dir = work_dir.to_path_buf();
        let work_dir_arg = work_dir_arg.clone();
        let session_id_capture = session_id_capture.clone();
        let sse_emitted_thinking = sse_emitted_thinking.clone();
        let sse_emitted_text = sse_emitted_text.clone();
        let sse_text_buffer = sse_text_buffer.clone();
        let sse_done_sent = sse_done_sent.clone();
        let sse_error_message = sse_error_message.clone();
        let sse_cancel = sse_cancel.clone();
        let sse_complete_tx = sse_complete_tx.clone();
        let sse_session_idle_tx = sse_session_idle_tx.clone();
        let sse_retry_tx = sse_retry_tx.clone();
        let last_activity = last_activity.clone();
        let text_output_tx = text_output_tx.clone();
        let sse_tool_depth_tx = sse_tool_depth_tx.clone();
        let events_tx = events_tx.clone();
        let opencode_port = opencode_port.clone();
        let sse_host = std::env::var("SANDBOXED_SH_OPENCODE_SERVER_HOSTNAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "127.0.0.1".to_string());

        Some(tokio::spawn(async move {
            let event_url = format!(
                "http://{}:{}/event?directory={}",
                sse_host,
                opencode_port,
                urlencoding::encode(&work_dir_arg)
            );

            let mut attempts = 0u32;
            loop {
                if sse_cancel.is_cancelled() {
                    break;
                }
                if attempts > 7 {
                    break;
                }
                attempts += 1;

                let args = vec![
                    "-N".to_string(),
                    "-s".to_string(),
                    "-H".to_string(),
                    "Accept: text/event-stream".to_string(),
                    "-H".to_string(),
                    "Cache-Control: no-cache".to_string(),
                    event_url.clone(),
                ];

                let child = workspace_exec
                    .spawn_streaming(&work_dir, "curl", &args, HashMap::new())
                    .await;

                // Exponential backoff: 50ms, 100ms, 200ms, 400ms, ...
                let backoff_ms = 50u64 * (1u64 << (attempts - 1).min(6));

                let mut child = match child {
                    Ok(child) => child,
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                };

                let stdout = match child.stdout.take() {
                    Some(stdout) => stdout,
                    None => {
                        let _ = child.kill().await;
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                };

                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                let mut current_event: Option<String> = None;
                let mut data_lines: Vec<String> = Vec::new();
                let mut state = OpencodeSseState::default();
                let mut saw_complete = false;
                // Reset SSE state on reconnect so stale values from a lost
                // connection don't cause incorrect behavior:
                // - tool depth: stale counts would permanently disable the
                //   inactivity timeout
                // - session_idle: a stale `true` would trigger the 10s kill
                //   timer after reconnect, prematurely terminating the mission
                // - retry counter: stale counts from a previous connection
                //   should not accumulate across reconnects
                // - last_activity: reset so the 120s global and 30s text idle
                //   timers count from the reconnect, not from the last event
                //   on the dead connection (the depth reset to 0 disables the
                //   tools_active guard, so last_activity is the only remaining
                //   protection against premature timeout during reconnect)
                sse_tool_depth_tx.send_modify(|v| *v = 0);
                let _ = sse_session_idle_tx.send(false);
                sse_retry_tx.send_modify(|v| *v = 0);
                if let Ok(mut guard) = last_activity.lock() {
                    *guard = std::time::Instant::now();
                }

                loop {
                    if sse_cancel.is_cancelled() {
                        let _ = child.kill().await;
                        return;
                    }
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end();
                            if trimmed.is_empty() {
                                if !data_lines.is_empty() {
                                    let data = data_lines.join("\n");
                                    let current_session = session_id_capture
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .clone();
                                    if let Some(parsed) = parse_opencode_sse_event(
                                        &data,
                                        current_event.as_deref(),
                                        current_session.as_deref(),
                                        &mut state,
                                        mission_id,
                                    ) {
                                        if let Some(session_id) = parsed.session_id {
                                            let mut guard = session_id_capture
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner());
                                            if guard.is_none() {
                                                *guard = Some(session_id);
                                            }
                                        }
                                        if let Some(event) = parsed.event {
                                            if let Ok(mut guard) = last_activity.lock() {
                                                *guard = std::time::Instant::now();
                                            }
                                            if let AgentEvent::Error { ref message, .. } = event {
                                                let mut guard = sse_error_message
                                                    .lock()
                                                    .unwrap_or_else(|e| e.into_inner());
                                                if guard.is_none() {
                                                    *guard = Some(message.clone());
                                                }
                                            }
                                            if matches!(event, AgentEvent::Thinking { .. }) {
                                                sse_emitted_thinking.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                            }
                                            if let AgentEvent::TextDelta { ref content, .. } = event
                                            {
                                                let _ = text_output_tx.send(true);
                                                sse_emitted_text.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                                // Capture the latest full-text snapshot so
                                                // it can serve as a fallback for final_result
                                                // when stdout JSON and storage both fail.
                                                if let Ok(mut buf) = sse_text_buffer.lock() {
                                                    *buf = content.clone();
                                                }
                                            }
                                            // Track active tool depth for permit management.
                                            match &event {
                                                AgentEvent::ToolCall { .. } => {
                                                    sse_tool_depth_tx
                                                        .send_modify(|v| *v = v.saturating_add(1));
                                                }
                                                AgentEvent::ToolResult { .. } => {
                                                    sse_tool_depth_tx
                                                        .send_modify(|v| *v = v.saturating_sub(1));
                                                }
                                                _ => {}
                                            }
                                            let _ = events_tx.send(event);
                                        }
                                        if parsed.message_complete {
                                            saw_complete = true;
                                            let _ = sse_complete_tx.send(true);
                                            if sse_emitted_thinking
                                                .load(std::sync::atomic::Ordering::SeqCst)
                                                && !sse_done_sent
                                                    .load(std::sync::atomic::Ordering::SeqCst)
                                            {
                                                let _ = events_tx.send(AgentEvent::Thinking {
                                                    content: String::new(),
                                                    done: true,
                                                    mission_id: Some(mission_id),
                                                });
                                                sse_done_sent.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                            }
                                            let _ = child.kill().await;
                                            break;
                                        }
                                        if parsed.session_idle {
                                            let _ = sse_session_idle_tx.send(true);
                                        }
                                        if parsed.session_retry {
                                            sse_retry_tx.send_modify(|v| *v += 1);
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
                        }
                        Err(_) => break,
                    }
                }

                let _ = child.kill().await;
                if saw_complete {
                    break;
                }
                // Exponential backoff before reconnecting
                let backoff_ms = 50u64 * (1u64 << attempts.min(6));
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
        }))
    } else {
        None
    };
    // Drop the original sender so the channel closes when the SSE handler exits.
    // This prevents a stale `tools_active == true` from permanently disabling
    // the inactivity timeout if the SSE handler dies mid-tool-execution.
    drop(sse_tool_depth_tx);

    // Spawn a task to read stderr (just log in JSON mode, events come on stdout)
    let mission_id_clone = mission_id;
    // Use a separate mutex for stderr errors so that broad stderr pattern
    // matches (e.g. log lines containing "error" with JSON) don't write into
    // sse_error_message.  Only genuine SSE-level errors (session.error,
    // AgentEvent::Error from the SSE stream) should block recovery guards.
    let stderr_error_message: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stderr_error_capture = stderr_error_message.clone();
    let stderr_text_capture = stderr_text_buffer.clone();
    let stderr_recent_capture = stderr_recent_lines.clone();
    let stderr_text_output_tx = text_output_tx.clone();
    let stderr_last_activity = last_activity.clone();
    let stderr_rate_limit = rate_limit_detected.clone();
    let stderr_events_tx = events_tx.clone();
    let stderr_handle = stderr.map(|stderr| {
        tokio::spawn(async move {
            let stderr_reader = BufReader::new(stderr);
            let mut stderr_lines = stderr_reader.lines();
            // Track the last message role seen in stderr so we only capture
            // assistant text parts (not user message echoes) into the buffer.
            let mut last_stderr_role = String::new();
            let mut retry_count: u32 = 0;
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                let clean = line.trim().to_string();
                if !clean.is_empty() {
                    if let Ok(mut recent_lines) = stderr_recent_capture.lock() {
                        if recent_lines.len() >= 32 {
                            let _ = recent_lines.pop_front();
                        }
                        recent_lines.push_back(clean.clone());
                    }
                    // Refresh global inactivity timer for lines that indicate
                    // real work progress.  Heartbeats and server-internal status
                    // lines are excluded — they fire every ~30s and would keep a
                    // hung LLM call alive forever.
                    let is_server_noise = clean.contains("server.heartbeat")
                        || clean.contains("server.connected")
                        || clean.contains("server.listening");
                    if !is_server_noise {
                        if let Ok(mut guard) = stderr_last_activity.lock() {
                            *guard = std::time::Instant::now();
                        }
                    }
                    tracing::debug!(mission_id = %mission_id_clone, line = %clean, "OpenCode CLI stderr");

                    // Track message role from stderr event lines like:
                    //   [MAIN] message.updated (user, build)
                    //   [MAIN] message.updated (assistant, build, glm-4.7)
                    if clean.contains("message.updated") {
                        if clean.contains("(user") {
                            last_stderr_role = "user".to_string();
                        } else if clean.contains("(assistant") {
                            last_stderr_role = "assistant".to_string();
                        }
                    }

                    if let Some(text_part) = parse_opencode_stderr_text_part(&clean) {
                        // Only capture text parts that follow an assistant message,
                        // skip user message echoes
                        if last_stderr_role != "user" {
                            if let Ok(mut buffer) = stderr_text_capture.lock() {
                                // Replace the buffer with the latest text.
                                // Each message.part (text) line contains the full
                                // accumulated text of the part, not just the delta.
                                // Using push_str would concatenate snapshots and
                                // produce stuttered output like "LetLet meLet me get...".
                                *buffer = text_part;
                            }
                            let _ = stderr_text_output_tx.send(true);
                        }
                    }

                    // Detect session/provider errors from stderr and surface
                    // them as AgentEvent::Error so the frontend shows the
                    // reason a mission failed (issue #146).
                    let lower = clean.to_lowercase();
                    let detected_error = if lower.contains("session.error")
                        || lower.contains("session ended with error")
                    {
                        // Standard session error format:
                        //   [MAIN] session.error: Requested entity was not found
                        clean.find(": ").map(|pos| clean[pos + 2..].trim().to_string())
                    } else if lower.contains("response.error") {
                        // Provider response error:
                        //   [MAIN] response.error: 404 Not Found
                        clean.find(": ").map(|pos| clean[pos + 2..].trim().to_string())
                    } else if (lower.contains("error") || lower.contains("failed"))
                        && clean.contains('{')
                    {
                        // JSON error payload on stderr — try to extract a
                        // meaningful message from common fields.
                        if let Some(start) = clean.find('{') {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&clean[start..]) {
                                let msg = // 1. Top-level "message" string
                                    json.get("message")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                    // 2. "error" as a plain string (e.g. {"error": "Rate limited"})
                                    .or_else(|| {
                                        json.get("error")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    // 3. Nested error object: {"error": {"message": "...", "status": "..."}}
                                    .or_else(|| {
                                        json.get("error")
                                            .and_then(|e| e.as_object())
                                            .and_then(|obj| {
                                                let msg = obj.get("message").and_then(|m| m.as_str())?;
                                                let status = obj.get("status").and_then(|s| s.as_str());
                                                Some(if let Some(st) = status {
                                                    format!("{} ({})", msg, st)
                                                } else {
                                                    msg.to_string()
                                                })
                                            })
                                    })
                                    // 4. Last resort: stringify the raw "error" value
                                    .or_else(|| {
                                        json.get("error").map(|v| v.to_string())
                                    });
                                msg
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(err_msg) = detected_error {
                        if !err_msg.is_empty() {
                            tracing::warn!(
                                mission_id = %mission_id_clone,
                                error = %err_msg,
                                "OpenCode provider error detected on stderr"
                            );
                            let mut guard = stderr_error_capture.lock().unwrap_or_else(|e| e.into_inner());
                            if guard.is_none() {
                                *guard = Some(err_msg.clone());
                            }
                            // Emit a real-time error event so the frontend
                            // shows the error immediately, not just at the end.
                            let _ = stderr_events_tx.send(AgentEvent::Error {
                                message: err_msg,
                                mission_id: Some(mission_id_clone),
                                resumable: true,
                            });
                        }
                    }

                    // Detect retry loops: OpenCode emits "session.status: retry"
                    // on stderr when the LLM API call fails and it retries.
                    // After several consecutive retries without progress, surface
                    // this as an error so the mission doesn't silently hang.
                    if lower.contains("session.status: retry")
                        || lower.contains("session.status:retry")
                    {
                        retry_count += 1;
                        if retry_count >= 3 {
                            tracing::warn!(
                                mission_id = %mission_id_clone,
                                retry_count = retry_count,
                                "OpenCode stuck in retry loop — LLM API is likely returning errors (e.g. 429 rate limit)"
                            );
                            // Signal the main loop to kill the process early for faster recovery.
                            stderr_rate_limit.store(true, std::sync::atomic::Ordering::SeqCst);
                            let mut guard = stderr_error_capture.lock().unwrap_or_else(|e| e.into_inner());
                            if guard.is_none() {
                                *guard = Some(format!(
                                    "LLM API request failed after {} retries (possible rate limit or API error). \
                                     Check your API key and provider endpoint configuration.",
                                    retry_count
                                ));
                            }
                        }
                    } else if lower.contains("session.status: busy")
                        || lower.contains("session.status:busy")
                    {
                        // busy between retries is normal, don't reset
                    } else if lower.contains("message.updated")
                        || lower.contains("message.completed")
                    {
                        // Real progress — reset retry counter and clear rate-limit flag
                        retry_count = 0;
                        stderr_rate_limit
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        })
    });

    // Process stdout output from oh-my-opencode
    // Events come via SSE (when curl is available), stdout contains the assistant's text response.
    let stdout_reader = BufReader::new(stdout);
    let mut stdout_lines = stdout_reader.lines();
    let mut state = OpencodeSseState::default();

    let mut sse_complete_seen = false;
    let mut sse_complete_at: Option<std::time::Instant> = None;
    let mut text_output_at: Option<std::time::Instant> = None;
    // Track session idle state — used as a fallback completion signal when
    // response.completed is not emitted (common with GLM models).
    let mut session_idle_seen = false;
    let mut session_idle_at: Option<std::time::Instant> = None;
    let mut had_meaningful_work = false;
    // Track consecutive retries — if the model API keeps failing, abort early
    // instead of waiting for the full idle timeout.  We track the last-seen
    // cumulative value from the SSE channel so that a text-output reset only
    // zeroes the *local* counter and later retries are counted as a fresh run.
    let mut consecutive_retries: u32 = 0;
    let mut last_seen_total_retries: u32 = 0;
    let max_consecutive_retries: u32 = 5;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(mission_id = %mission_id, "OpenCode execution cancelled, killing process");
                let _ = child.kill().await;
                // Await background tasks so in-flight mutex writes complete
                // before we return.  Use the same teardown discipline as the
                // normal exit path to avoid data races on shared state.
                if let Some(mut handle) = stderr_handle {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                            handle.abort();
                        }
                        _ = &mut handle => {}
                    }
                }
                sse_cancel.cancel();
                if let Some(handle) = sse_handle {
                    handle.abort();
                    let _ = handle.await;
                }
                return AgentResult::failure("Cancelled".to_string(), 0)
                    .with_terminal_reason(TerminalReason::Cancelled);
            }
            changed = sse_complete_rx.changed() => {
                if changed.is_ok() && *sse_complete_rx.borrow() && !sse_complete_seen {
                    sse_complete_seen = true;
                    sse_complete_at = Some(std::time::Instant::now());
                }
            }
            changed = sse_session_idle_rx.changed() => {
                if changed.is_ok() {
                    if *sse_session_idle_rx.borrow() && !session_idle_seen {
                        session_idle_seen = true;
                        session_idle_at = Some(std::time::Instant::now());
                        tracing::debug!(
                            mission_id = %mission_id,
                            had_meaningful_work = had_meaningful_work,
                            "Session idle signal received from SSE"
                        );
                    } else if !*sse_session_idle_rx.borrow() && session_idle_seen {
                        // SSE reconnected — the sender reset to false.  Clear
                        // the stale idle state so the 10s kill timer doesn't
                        // fire based on a pre-reconnect timestamp.
                        session_idle_seen = false;
                        session_idle_at = None;
                        tracing::debug!(
                            mission_id = %mission_id,
                            "Session idle state reset (SSE reconnect)"
                        );
                    }
                }
            }
            changed = sse_retry_rx.changed() => {
                if changed.is_ok() {
                    let new_total = *sse_retry_rx.borrow();
                    // On SSE reconnect the sender resets to 0; clear local
                    // tracking so stale counts don't accumulate across
                    // connections.
                    if new_total == 0 && last_seen_total_retries > 0 {
                        last_seen_total_retries = 0;
                        consecutive_retries = 0;
                        continue;
                    }
                    let delta = new_total.saturating_sub(last_seen_total_retries);
                    last_seen_total_retries = new_total;
                    consecutive_retries += delta;
                    tracing::info!(
                        mission_id = %mission_id,
                        consecutive_retries = consecutive_retries,
                        "Model API retry detected"
                    );
                    if consecutive_retries >= max_consecutive_retries {
                        tracing::warn!(
                            mission_id = %mission_id,
                            retries = consecutive_retries,
                            "Model API failed after {} consecutive retries; aborting mission",
                            consecutive_retries
                        );
                        let _ = events_tx.send(AgentEvent::Error {
                            message: format!(
                                "Model API failed after {} consecutive retries. The model provider may be down or misconfigured.",
                                consecutive_retries
                            ),
                            mission_id: Some(mission_id),
                            resumable: true,
                        });
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            changed = text_output_rx.changed() => {
                if changed.is_ok() && *text_output_rx.borrow() {
                    text_output_at = Some(std::time::Instant::now());
                    had_meaningful_work = true;
                    // Reset idle state — new activity means the session is
                    // not truly idle yet.
                    session_idle_seen = false;
                    session_idle_at = None;
                    // Reset retry counter — real output means the model is working.
                    consecutive_retries = 0;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)), if sse_complete_seen => {
                if let Some(started) = sse_complete_at {
                    if started.elapsed() >= std::time::Duration::from_secs(2) {
                        tracing::info!(
                            mission_id = %mission_id,
                            "OpenCode completion observed; terminating lingering CLI process"
                        );
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            // Session idle grace period: if the session has been idle for 10s
            // after meaningful work was produced, treat as completed.  This
            // catches GLM models that emit response.incomplete without a
            // subsequent response.completed.
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)), if session_idle_seen && !sse_complete_seen && (had_meaningful_work
                || sse_emitted_thinking.load(std::sync::atomic::Ordering::SeqCst)
                || sse_emitted_text.load(std::sync::atomic::Ordering::SeqCst)) => {
                if let Some(idle_since) = session_idle_at {
                    if idle_since.elapsed() >= std::time::Duration::from_secs(10) {
                        // Don't kill while tools are actively running — the model
                        // may have sent session.idle prematurely before a long
                        // tool execution (build, test) produces more output.
                        let sse_alive = sse_handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                        let tools_active = sse_alive && *sse_tool_depth_rx.borrow() > 0;
                        if tools_active {
                            tracing::debug!(
                                mission_id = %mission_id,
                                tool_depth = *sse_tool_depth_rx.borrow(),
                                "Session idle but tools still active; deferring kill"
                            );
                        } else {
                            tracing::info!(
                                mission_id = %mission_id,
                                "Session idle for 10s after meaningful work; treating as completion"
                            );
                            let _ = child.kill().await;
                            break;
                        }
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                // Early kill when stderr reader detects a rate-limit retry loop.
                // Only kill if there's also no real SSE activity (tool calls, thinking).
                // If the model is doing tool calls, the retry status may be transient.
                if rate_limit_detected.load(std::sync::atomic::Ordering::SeqCst) {
                    let sse_idle = last_activity
                        .lock()
                        .ok()
                        .map(|g| g.elapsed() >= std::time::Duration::from_secs(15))
                        .unwrap_or(true);
                    if sse_idle {
                        tracing::info!(
                            mission_id = %mission_id,
                            "Rate-limit retry loop detected with no SSE activity; terminating CLI process early"
                        );
                        let _ = child.kill().await;
                        break;
                    }
                }
                if let Some(last_text) = text_output_at {
                    if last_text.elapsed() >= std::time::Duration::from_secs(30) {
                        // Only kill if there's also no recent SSE/stderr activity
                        // AND no tools are actively running.  A long tool execution
                        // (build, test, sleep) may produce no text output for >30s;
                        // killing the process mid-tool would be wrong.
                        // If the SSE handler has exited, the depth value may be
                        // stale (stuck > 0), so treat that as "no tools active".
                        let sse_alive = sse_handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                        let tools_active = sse_alive && *sse_tool_depth_rx.borrow() > 0;
                        let recent_activity = last_activity
                            .lock()
                            .ok()
                            .map(|g| g.elapsed() < std::time::Duration::from_secs(30))
                            .unwrap_or(false);
                        if !recent_activity && !tools_active {
                            tracing::info!(
                                mission_id = %mission_id,
                                "OpenCode output idle timeout reached; terminating CLI process"
                            );
                            let _ = child.kill().await;
                            break;
                        }
                    }
                }
                // Global inactivity timeout: if nothing at all has happened
                // for 120s (no SSE events, no stdout, no stderr), the process
                // is likely stuck.  Kill it and let the fallback recovery
                // logic read the result from OpenCode storage.
                // Skip this check while tools are actively running — long
                // commands (builds, tests) may produce no SSE events for
                // extended periods and heartbeats are intentionally filtered.
                // If the SSE handler has exited, the depth value may be stale,
                // so treat that as "no tools active".
                let sse_alive = sse_handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                let tools_active = sse_alive && *sse_tool_depth_rx.borrow() > 0;
                let inactive_too_long = !tools_active
                    && last_activity
                        .lock()
                        .map(|g| g.elapsed() >= std::time::Duration::from_secs(120))
                        .unwrap_or(false);
                if inactive_too_long {
                    tracing::warn!(
                        mission_id = %mission_id,
                        "Global inactivity timeout (120s); terminating stuck CLI process"
                    );
                    let _ = child.kill().await;
                    break;
                }
            }
            line_result = stdout_lines.next_line() => {
                match line_result {
                    Ok(None) => {
                        // EOF - process finished
                        break;
                    }
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(mut guard) = last_activity.lock() {
                            *guard = std::time::Instant::now();
                        }

                        // Try to parse as JSON event
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                            let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            tracing::debug!(
                                mission_id = %mission_id,
                                event_type = %event_type,
                                "OpenCode JSON event"
                            );

                            // Extract text content from message.part.updated for final result
                            // Only capture assistant messages - skip user message echoes
                            if event_type == "message.part.updated" {
                                if let Some(props) = json.get("properties") {
                                    if let Some(part) = props.get("part") {
                                        let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                        if part_type == "text" {
                                            let msg_id = part.get("messageID")
                                                .or_else(|| part.get("messageId"))
                                                .or_else(|| part.get("message_id"))
                                                .or_else(|| props.get("messageID"))
                                                .or_else(|| props.get("messageId"))
                                                .or_else(|| props.get("message_id"))
                                                .and_then(|v| v.as_str());
                                            // Skip non-assistant and unknown-role messages,
                                            // consistent with the SSE path in handle_part_update
                                            // (lines 325-336). Three cases when msg_id is present:
                                            //   - role is known non-assistant → skip
                                            //   - role is not yet recorded   → skip (avoids
                                            //     emitting user-message echoes as model text,
                                            //     which would set text_output_at and trigger
                                            //     the premature 30s text-idle timeout)
                                            //   - role is "assistant"        → process text
                                            // When msg_id is None (no ID in the event), allow
                                            // text through — same as the SSE path.
                                            let is_confirmed_assistant = match msg_id {
                                                Some(id) => state.message_roles.get(id)
                                                    .map(|role| role == "assistant")
                                                    .unwrap_or(false), // unknown role → skip
                                                None => true, // no msg_id → allow through
                                            };
                                            if is_confirmed_assistant {
                                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                                    final_result = text.to_string();
                                                    let _ = text_output_tx.send(true);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Handle completion and error events from oh-my-opencode
                            if event_type == "completion" {
                                tracing::info!(mission_id = %mission_id, "OpenCode JSON completion event");
                                let _ = sse_complete_tx.send(true);
                            } else if event_type == "error" {
                                had_error = true;
                                if let Some(props) = json.get("properties") {
                                    if let Some(err) = props.get("error").and_then(|e| e.as_str()) {
                                        tracing::warn!(mission_id = %mission_id, error = %err, "OpenCode JSON error event");
                                        if final_result.is_empty() {
                                            final_result = err.to_string();
                                        }
                                    }
                                }
                            }

                            // Route through SSE event parser for thinking/tool events
                            let current_session = session_id_capture.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if let Some(parsed) = parse_opencode_sse_event(
                                trimmed,
                                None,
                                current_session.as_deref(),
                                &mut state,
                                mission_id,
                            ) {
                                if let Some(session_id) = parsed.session_id {
                                    let mut guard = session_id_capture.lock().unwrap_or_else(|e| e.into_inner());
                                    if guard.is_none() {
                                        *guard = Some(session_id);
                                    }
                                }
                                if let Some(event) = parsed.event {
                                    if let Ok(mut guard) = last_activity.lock() {
                                        *guard = std::time::Instant::now();
                                    }
                                    if let AgentEvent::Error { ref message, .. } = event {
                                        let mut guard = sse_error_message.lock().unwrap_or_else(|e| e.into_inner());
                                        if guard.is_none() {
                                            *guard = Some(message.clone());
                                        }
                                    }
                                    if matches!(event, AgentEvent::Thinking { .. }) {
                                        sse_emitted_thinking.store(true, std::sync::atomic::Ordering::SeqCst);
                                        // New thinking content arrived; reset done flag so this
                                        // turn's thinking block will get its own done event.
                                        sse_done_sent.store(false, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    if matches!(event, AgentEvent::TextDelta { .. }) {
                                        let _ = text_output_tx.send(true);
                                        sse_emitted_text.store(true, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    let _ = events_tx.send(event);
                                }
                                if parsed.message_complete {
                                    let _ = sse_complete_tx.send(true);
                                    // Send thinking done signal if needed
                                    if sse_emitted_thinking.load(std::sync::atomic::Ordering::SeqCst)
                                        && !sse_done_sent.load(std::sync::atomic::Ordering::SeqCst)
                                    {
                                        let _ = events_tx.send(AgentEvent::Thinking {
                                            content: String::new(),
                                            done: true,
                                            mission_id: Some(mission_id),
                                        });
                                        sse_done_sent.store(true, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    // Clear per-turn thinking buffers so each model turn
                                    // gets its own thinking block in the UI.
                                    // Note: sse_done_sent stays true here to prevent the
                                    // end-of-session fallback from emitting a duplicate done
                                    // event. It is reset to false when new thinking content
                                    // arrives for the next turn (see AgentEvent::Thinking above).
                                    state.part_buffers.retain(|k, _| {
                                        !k.starts_with("thinking:") && !k.starts_with("reasoning:")
                                    });
                                    state.last_emitted_thinking = None;
                                }
                                if parsed.session_idle {
                                    let _ = sse_session_idle_tx.send(true);
                                }
                                if parsed.session_retry {
                                    sse_retry_tx.send_modify(|v| *v += 1);
                                }
                            }
                        } else {
                            // Non-JSON line - this is the expected output format without --format json
                            tracing::debug!(mission_id = %mission_id, line = %trimmed, "OpenCode stdout");

                            // Detect error lines from CLI stdout
                            let lower = trimmed.to_lowercase();
                            if lower.contains("session ended with error")
                                || lower.contains("session.error")
                            {
                                had_error = true;
                                if let Some(pos) = trimmed.find(": ") {
                                    let err_part = trimmed[pos + 2..].trim();
                                    if !err_part.is_empty() {
                                        let mut guard = sse_error_message.lock().unwrap_or_else(|e| e.into_inner());
                                        if guard.is_none() {
                                            *guard = Some(err_part.to_string());
                                        }
                                    }
                                }
                            }

                            // Skip runner banner/status lines so they don't
                            // pollute the model response (issues #147, #151).
                            if is_opencode_banner_line(trimmed) {
                                tracing::debug!(mission_id = %mission_id, line = %trimmed, "Skipping OpenCode banner line");
                                continue;
                            }

                            final_result.push_str(trimmed);
                            final_result.push('\n');
                            let _ = text_output_tx.send(true);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error reading from OpenCode CLI stdout: {}", e);
                        break;
                    }
                }
            }
        }
    }

    // Wait for stderr task to complete (avoid hangs if the process won't exit)
    if let Some(mut handle) = stderr_handle {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                handle.abort();
            }
            _ = &mut handle => {}
        }
    }

    // Wait for child process to finish and clean up (with timeout to avoid hangs)
    let exit_status =
        match tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    "OpenCode CLI wait timed out; forcing shutdown"
                );
                let _ = child.kill().await;
                had_error = true;
                if final_result.is_empty() {
                    final_result = "OpenCode CLI did not exit after completion".to_string();
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "OpenCode CLI wait timed out",
                ))
            }
        };

    sse_cancel.cancel();
    if let Some(handle) = sse_handle {
        handle.abort();
        // Await the abort so the SSE task finishes any in-flight writes to
        // sse_text_buffer before we read it in the fallback chain below.
        let _ = handle.await;
    }

    let sse_error = sse_error_message
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let has_sse_error = sse_error.is_some();

    // Check exit status
    if let Ok(status) = exit_status {
        if !status.success() {
            had_error = true;
            if opencode_output_needs_fallback(&final_result) {
                if let Some(err_msg) = stderr_error_message.lock().unwrap().clone() {
                    final_result = err_msg;
                } else if let Ok(recent_lines) = stderr_recent_lines.lock() {
                    if let Some(last_stderr) = summarize_recent_opencode_stderr(&recent_lines) {
                        final_result = format!(
                            "OpenCode CLI exited with status: {}. Last stderr: {}",
                            status, last_stderr
                        );
                    } else {
                        final_result = format!("OpenCode CLI exited with status: {}", status);
                    }
                } else {
                    final_result = format!("OpenCode CLI exited with status: {}", status);
                }
                final_result_from_nonzero_exit = true;
            }
        }
    }

    // Surface SSE error messages (e.g. session.error) that were captured during streaming.
    // These are high-confidence errors from the SSE stream and should block recovery.
    if let Some(err_msg) = sse_error.as_ref() {
        had_error = true;
        if opencode_output_needs_fallback(&final_result) {
            final_result = err_msg.clone();
            final_result_from_nonzero_exit = false;
        }
    }

    // Surface stderr-detected errors (e.g. JSON error payloads from provider).
    // These are lower-confidence than SSE errors because the stderr detection
    // uses broad pattern matching and can produce false positives.  They set
    // had_error but do NOT write into sse_error_message, so recovery guards
    // below can still clear had_error when valid content is recovered.
    if !has_sse_error {
        if let Some(err_msg) = stderr_error_message
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            had_error = true;
            if opencode_output_needs_fallback(&final_result) {
                final_result = err_msg;
                final_result_from_nonzero_exit = false;
            }
        }
    }

    let session_id = session_id_capture
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let session_id = session_id.or_else(|| extract_opencode_session_id(&final_result));
    let stored_message = session_id
        .as_deref()
        .and_then(|id| load_latest_opencode_assistant_message(workspace, id));

    let mut recovered_from_stderr = false;
    if opencode_output_needs_fallback(&final_result) {
        if let Some(session_id) = session_id.as_deref() {
            if let Some(message) = stored_message.as_ref() {
                let text = strip_think_tags(&extract_text(&message.parts));
                if !text.trim().is_empty() {
                    tracing::info!(
                        mission_id = %mission_id,
                        session_id = %session_id,
                        text_len = text.len(),
                        "Recovered OpenCode assistant output from storage"
                    );
                    final_result = text;
                    final_result_from_nonzero_exit = false;
                } else {
                    tracing::warn!(
                        mission_id = %mission_id,
                        session_id = %session_id,
                        "OpenCode assistant output not found in storage"
                    );
                }
            } else {
                tracing::warn!(
                    mission_id = %mission_id,
                    session_id = %session_id,
                    "OpenCode assistant output not found in storage"
                );
            }
        } else {
            tracing::warn!(
                mission_id = %mission_id,
                "OpenCode output was empty/banner-only and no session id was detected"
            );
        }
    }

    // SSE text buffer fallback: use the accumulated text from SSE TextDelta
    // events. This is the most reliable source after stdout JSON and session
    // storage because it contains exactly what was streamed to the dashboard,
    // unlike stderr which truncates long content with "..." (fixes #158).
    let mut recovered_from_sse = false;
    if opencode_output_needs_fallback(&final_result) {
        if let Ok(buffer) = sse_text_buffer.lock() {
            if !buffer.trim().is_empty() {
                tracing::info!(
                    mission_id = %mission_id,
                    text_len = buffer.len(),
                    "Recovered OpenCode assistant output from SSE text buffer"
                );
                final_result = buffer.clone();
                recovered_from_sse = true;
                final_result_from_nonzero_exit = false;
            }
        }
    }

    if opencode_output_needs_fallback(&final_result) {
        if let Ok(buffer) = stderr_text_buffer.lock() {
            if !buffer.trim().is_empty() {
                final_result = buffer.clone();
                recovered_from_stderr = true;
                final_result_from_nonzero_exit = false;
            }
        }
    }

    // Only clear had_error from recovery if there is no real SSE error.
    // Without this guard, a session.error followed by partial text in the
    // SSE buffer would clear the error and return a truncated response.
    if (recovered_from_sse || recovered_from_stderr) && !has_sse_error {
        had_error = false;
    }

    // Clear had_error when we have real (non-banner) content and no SSE error.
    // This avoids false failures when the CLI exited non-zero but produced real output.
    if had_error
        && !opencode_output_needs_fallback(&final_result)
        && !has_sse_error
        && !final_result_from_nonzero_exit
    {
        had_error = false;
    }

    // Strip inline <think>...</think> tags from final output (Minimax, DeepSeek, etc.)
    final_result = strip_think_tags(&final_result);

    // Final safeguard: reuse the same ANSI + banner sanitizer we employ for detection
    // (fixes #151 - runner logs appearing in assistant message)
    let cleaned_result = sanitized_opencode_stdout(&final_result);
    if !cleaned_result.trim().is_empty() {
        if let Cow::Owned(clean) = cleaned_result {
            final_result = clean;
        }
    }

    let mut emitted_thinking = false;
    let sse_emitted = sse_emitted_thinking.load(std::sync::atomic::Ordering::SeqCst);
    if let Some(message) = stored_message.as_ref() {
        if let Some(model) = message.model.clone() {
            model_used = Some(model);
        }
        if !sse_emitted {
            if let Some(reasoning) = extract_reasoning(&message.parts) {
                let _ = events_tx.send(AgentEvent::Thinking {
                    content: reasoning,
                    done: false,
                    mission_id: Some(mission_id),
                });
                emitted_thinking = true;
            }
        }
    }

    if !sse_emitted && !emitted_thinking {
        if let Some((thought, cleaned)) = extract_thought_line(&final_result) {
            let _ = events_tx.send(AgentEvent::Thinking {
                content: thought,
                done: false,
                mission_id: Some(mission_id),
            });
            emitted_thinking = true;
            final_result = cleaned;
        }
    }

    if emitted_thinking || (sse_emitted && !sse_done_sent.load(std::sync::atomic::Ordering::SeqCst))
    {
        let _ = events_tx.send(AgentEvent::Thinking {
            content: String::new(),
            done: true,
            mission_id: Some(mission_id),
        });
    }

    // Check for banner-only output BEFORE emitting TextDelta to avoid
    // sending runner logs as model response (fixes #151).
    if !had_error && opencode_output_needs_fallback(&final_result) {
        had_error = true;
        final_result =
            "OpenCode produced no assistant output (only runner status lines or empty). The model may not have responded.".to_string();
    }

    // Detect tool-call-only output: the model emitted tool calls but never
    // produced a final text response. The JSON fragment should not be returned
    // as assistant text — surface a clear error instead (fixes #148).
    if !had_error && is_tool_call_only_output(&final_result) {
        tracing::warn!(
            mission_id = %mission_id,
            result_preview = %final_result.chars().take(200).collect::<String>(),
            "OpenCode output contains only tool-call JSON fragments with no assistant text"
        );
        had_error = true;
        final_result =
            "The model attempted tool calls but produced no final text response. This can happen when the model routing chain doesn't support tool execution.".to_string();
    }

    // Only emit TextDelta if we have actual (non-banner) content and no SSE text was emitted.
    // This avoids sending runner logs as model response.
    if !sse_emitted_text.load(std::sync::atomic::Ordering::SeqCst)
        && !final_result.trim().is_empty()
        && !had_error
    {
        let _ = events_tx.send(AgentEvent::TextDelta {
            content: final_result.clone(),
            mission_id: Some(mission_id),
        });
    }

    tracing::info!(
        mission_id = %mission_id,
        had_error = had_error,
        result_len = final_result.len(),
        "OpenCode CLI execution completed"
    );

    let mut result = if had_error {
        // Use RateLimited terminal reason when rate limit was detected
        let reason = if rate_limit_detected.load(std::sync::atomic::Ordering::SeqCst) {
            TerminalReason::RateLimited
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_result, 0).with_terminal_reason(reason)
    } else {
        AgentResult::success(final_result, 0).with_terminal_reason(TerminalReason::Completed)
    };
    if let Some(model) = model_used {
        result = result.with_model(model);
    }

    // Clean up the temp prompt file (best-effort; the workspace may clean it later)
    let _ = std::fs::remove_file(&prompt_file_host);

    result
}

/// Execute a turn using Amp CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
#[allow(clippy::too_many_arguments)]
pub async fn run_amp_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    mode: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    _app_working_dir: &std::path::Path,
    session_id: Option<&str>,
    is_continuation: bool,
    api_key: Option<&str>,
) -> AgentResult {
    use crate::backend::amp::client::{AmpEvent, ContentBlock, StreamEvent};
    use std::collections::HashMap;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let workspace_exec = WorkspaceExec::new(workspace.clone());

    // Check if amp CLI is available
    if !command_available(&workspace_exec, work_dir, "amp").await {
        let auto_install = env_var_bool("SANDBOXED_SH_AUTO_INSTALL_AMP", true);
        if auto_install {
            // Try to install via bun first (preferred for container templates), then npm
            let has_bun = command_available(&workspace_exec, work_dir, "bun").await;
            let has_npm = command_available(&workspace_exec, work_dir, "npm").await;

            if has_bun {
                tracing::info!(mission_id = %mission_id, "Auto-installing Amp CLI via bun");
                let install_result = workspace_exec
                    .output(
                        work_dir,
                        "/bin/sh",
                        &[
                            "-lc".to_string(),
                            "bun install -g @sourcegraph/amp 2>&1".to_string(),
                        ],
                        HashMap::new(),
                    )
                    .await;
                match &install_result {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if output.status.success() {
                            tracing::info!(mission_id = %mission_id, stdout = %stdout, "Amp CLI installed via bun");
                        } else {
                            tracing::warn!(mission_id = %mission_id, stdout = %stdout, stderr = %stderr, exit_code = ?output.status.code(), "bun install for Amp CLI failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(mission_id = %mission_id, error = %e, "Failed to run bun install for Amp CLI");
                    }
                }
            } else if has_npm {
                tracing::info!(mission_id = %mission_id, "Auto-installing Amp CLI via npm");
                let install_result = workspace_exec
                    .output(
                        work_dir,
                        "/bin/sh",
                        &[
                            "-lc".to_string(),
                            "npm install -g @sourcegraph/amp".to_string(),
                        ],
                        HashMap::new(),
                    )
                    .await;
                if let Err(e) = &install_result {
                    tracing::warn!(mission_id = %mission_id, error = %e, "Failed to auto-install Amp CLI via npm");
                }
            } else {
                tracing::warn!(mission_id = %mission_id, "Neither bun nor npm available for Amp CLI auto-install");
            }
        }
    }

    // Check if node is available (amp CLI is a Node.js script)
    let has_node = command_available(&workspace_exec, work_dir, "node").await;
    let has_bun = command_available(&workspace_exec, work_dir, "bun").await
        || command_available(&workspace_exec, work_dir, "/root/.bun/bin/bun").await;

    // Find the amp binary - check standard PATH first, then bun's global bin paths
    // The amp CLI is a Node.js script, so if node is not available but bun is,
    // we need to run it via "bun run amp" or "bun /path/to/main.js"
    let (amp_binary, amp_args_prefix): (String, Vec<String>) = if has_node
        && command_available(&workspace_exec, work_dir, "amp").await
    {
        // Node available and amp in PATH - run directly
        ("amp".to_string(), vec![])
    } else if has_bun {
        // No node, but bun is available - use "bun run amp" or run the JS directly
        // First check for bun's global install paths
        let bun_path = if command_available(&workspace_exec, work_dir, "bun").await {
            "bun".to_string()
        } else {
            "/root/.bun/bin/bun".to_string()
        };

        // Check for amp's main.js in bun's global install location
        let amp_main_js_paths = [
            "/root/.bun/install/global/node_modules/@sourcegraph/amp/dist/main.js",
            "/root/.cache/.bun/install/global/node_modules/@sourcegraph/amp/dist/main.js",
        ];

        let mut found_js = None;
        for path in &amp_main_js_paths {
            let check_result = workspace_exec
                .output(
                    work_dir,
                    "/bin/sh",
                    &["-c".to_string(), format!("test -f {} && echo exists", path)],
                    HashMap::new(),
                )
                .await;
            if let Ok(output) = check_result {
                if String::from_utf8_lossy(&output.stdout).contains("exists") {
                    found_js = Some(path.to_string());
                    break;
                }
            }
        }

        if let Some(js_path) = found_js {
            tracing::info!(
                mission_id = %mission_id,
                js_path = %js_path,
                "Running Amp CLI via bun (node not available)"
            );
            (bun_path, vec![js_path])
        } else {
            // Try "bun run amp" as fallback
            tracing::info!(
                mission_id = %mission_id,
                "Trying 'bun run amp' (amp main.js not found in expected locations)"
            );
            (bun_path, vec!["run".to_string(), "amp".to_string()])
        }
    } else if command_available(&workspace_exec, work_dir, "/root/.bun/bin/amp").await {
        // Amp exists but may fail without node - try anyway
        ("/root/.bun/bin/amp".to_string(), vec![])
    } else if command_available(&workspace_exec, work_dir, "/root/.cache/.bun/bin/amp").await {
        ("/root/.cache/.bun/bin/amp".to_string(), vec![])
    } else {
        let err_msg = "Amp CLI not found. Install it with: bun install -g @sourcegraph/amp (or npm install -g @sourcegraph/amp)";
        tracing::error!(mission_id = %mission_id, "{}", err_msg);
        return AgentResult::failure(err_msg.to_string(), 0)
            .with_terminal_reason(TerminalReason::LlmError);
    };

    tracing::info!(
        mission_id = %mission_id,
        work_dir = %work_dir.display(),
        workspace_type = ?workspace.workspace_type,
        mode = ?mode,
        is_continuation = is_continuation,
        amp_binary = %amp_binary,
        "Starting Amp execution via WorkspaceExec"
    );

    // Build CLI arguments
    // Amp CLI format: amp [subcommand] --execute "message" [flags]
    // For continuation: amp threads continue <session_id> --execute "message" [flags]
    // When running via bun, amp_args_prefix contains ["/path/to/main.js"] or ["run", "amp"]
    let mut args = amp_args_prefix;

    // For continuation, use threads continue subcommand
    if is_continuation {
        if let Some(sid) = session_id {
            args.push("threads".to_string());
            args.push("continue".to_string());
            args.push(sid.to_string());
        }
    }

    // --execute with message as its argument (must come before other flags)
    args.push("--execute".to_string());
    args.push(message.to_string());

    // Remaining flags
    args.push("--stream-json".to_string());
    args.push("--dangerously-allow-all".to_string());

    // Mode (smart/rush)
    if let Some(m) = mode {
        args.push("--mode".to_string());
        args.push(m.to_string());
    }

    // Build environment
    let mut env = HashMap::new();

    // Use API key from config, or fall back to environment variable
    if let Some(key) = api_key {
        env.insert("AMP_API_KEY".to_string(), key.to_string());
    } else if let Ok(key) = std::env::var("AMP_API_KEY") {
        env.insert("AMP_API_KEY".to_string(), key);
    }

    // Pass through AMP_URL for CLIProxyAPI integration
    // This allows routing Amp requests through a local proxy (e.g., CLIProxyAPI)
    // AMP_URL sets the Amp service URL (default: https://ampcode.com/)
    if let Ok(amp_url) = std::env::var("AMP_URL") {
        env.insert("AMP_URL".to_string(), amp_url);
    }

    // Also support legacy AMP_PROVIDER_URL as an alias
    if !env.contains_key("AMP_URL") {
        if let Ok(provider_url) = std::env::var("AMP_PROVIDER_URL") {
            env.insert("AMP_URL".to_string(), provider_url);
        }
    }

    // Fall back to reading amp.url from Amp CLI settings file if no env var set
    if !env.contains_key("AMP_URL") {
        if let Some(amp_url) = get_amp_url_from_settings() {
            tracing::debug!(mission_id = %mission_id, amp_url = %amp_url, "Using amp.url from Amp CLI settings");
            env.insert("AMP_URL".to_string(), amp_url);
        }
    }

    // Log the environment for debugging
    tracing::debug!(
        mission_id = %mission_id,
        env_vars = ?env.keys().collect::<Vec<_>>(),
        amp_url = ?env.get("AMP_URL"),
        amp_api_key_present = env.contains_key("AMP_API_KEY"),
        "Spawning Amp CLI with environment"
    );

    // Use WorkspaceExec to spawn the CLI
    let mut child = match workspace_exec
        .spawn_streaming(work_dir, &amp_binary, &args, env)
        .await
    {
        Ok(child) => child,
        Err(e) => {
            let err_msg = format!("Failed to start Amp CLI: {}", e);
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Close stdin immediately - Amp uses --execute with args, not stdin
    // Leaving the pipe open can cause issues with Node.js process lifecycle
    drop(child.stdin.take());

    // Get stdout for reading events
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let err_msg = "Failed to capture Amp stdout";
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg.to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Capture stderr for debugging
    let stderr = child.stderr.take();
    let stderr_capture = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
    let stderr_capture_clone = stderr_capture.clone();
    let mission_id_for_stderr = mission_id;
    let stderr_handle = stderr.map(|stderr| {
        tokio::spawn(async move {
            let stderr_reader = BufReader::new(stderr);
            let mut stderr_lines = stderr_reader.lines();
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    tracing::debug!(mission_id = %mission_id_for_stderr, stderr = %trimmed, "Amp CLI stderr");
                    let mut captured = stderr_capture_clone.lock().await;
                    if !captured.is_empty() {
                        captured.push('\n');
                    }
                    captured.push_str(trimmed);
                }
            }
        })
    });

    // Track tool calls for result mapping
    let mut pending_tools: HashMap<String, String> = HashMap::new();
    let mut final_result = String::new();
    let mut had_error = false;
    let mut model_used: Option<String> = None;

    // Track token usage for cost calculation
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_cache_creation_tokens: u64 = 0;
    let mut total_cache_read_tokens: u64 = 0;

    // Track content blocks for streaming
    let mut block_types: HashMap<u32, String> = HashMap::new();
    let mut thinking_buffer: HashMap<u32, String> = HashMap::new();
    let mut text_buffer: HashMap<u32, String> = HashMap::new();
    let mut active_thinking_index: Option<u32> = None;
    let mut finalized_thinking_indices: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    let mut last_text_len: usize = 0;
    let mut thinking_streamed = false; // Track if thinking was already streamed

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    // Process events until completion or cancellation
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(mission_id = %mission_id, "Amp execution cancelled, killing process");
                let _ = child.kill().await;
                if let Some(handle) = stderr_handle {
                    handle.abort();
                }
                return AgentResult::failure("Cancelled".to_string(), 0)
                    .with_terminal_reason(TerminalReason::Cancelled);
            }
            line_result = lines.next_line() => {
                match line_result {
                    Ok(Some(line)) => {
                        if line.is_empty() {
                            continue;
                        }

                        let amp_event: AmpEvent = match serde_json::from_str(&line) {
                            Ok(event) => event,
                            Err(e) => {
                                tracing::warn!(
                                    mission_id = %mission_id,
                                    error = %e,
                                    line = %if line.len() > 200 { let end = safe_truncate_index(&line, 200); format!("{}...", &line[..end]) } else { line.clone() },
                                    "Failed to parse Amp event"
                                );
                                continue;
                            }
                        };

                        match amp_event {
                            AmpEvent::System(sys) => {
                                tracing::debug!(
                                    mission_id = %mission_id,
                                    session_id = %sys.session_id,
                                    model = ?sys.model,
                                    "Amp session init"
                                );
                                if sys.model.is_some() {
                                    model_used = sys.model;
                                }
                                // Amp generates its own session/thread ID; emit an update so the
                                // mission's session_id gets updated for continuation.
                                let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                                    session_id: sys.session_id.clone(),
                                    mission_id,
                                });
                            }
                            AmpEvent::StreamEvent(wrapper) => {
                                match wrapper.event {
                                    StreamEvent::ContentBlockDelta { index, delta } => {
                                        let block_type = block_types
                                            .get(&index)
                                            .map(|value| value.as_str());
                                        let is_thinking_block =
                                            matches!(block_type, Some("thinking"));
                                        if delta.delta_type == "thinking_delta"
                                            || (is_thinking_block
                                                && delta.delta_type == "text_delta")
                                        {
                                            let thinking_text = delta.thinking.or(delta.text.clone());
                                            if let Some(thinking_text) = thinking_text {
                                                if !thinking_text.is_empty() {
                                                    // If a new thinking block started, finalize the previous one
                                                    if let Some(prev_idx) = active_thinking_index {
                                                        if prev_idx != index {
                                                            let _ = events_tx.send(AgentEvent::Thinking {
                                                                content: String::new(),
                                                                done: true,
                                                                mission_id: Some(mission_id),
                                                            });
                                                            finalized_thinking_indices.insert(prev_idx);
                                                        }
                                                    }
                                                    active_thinking_index = Some(index);

                                                    let buffer = thinking_buffer.entry(index).or_default();
                                                    buffer.push_str(&thinking_text);
                                                    thinking_streamed = true;

                                                    let _ = events_tx.send(AgentEvent::Thinking {
                                                        content: buffer.clone(),
                                                        done: false,
                                                        mission_id: Some(mission_id),
                                                    });
                                                }
                                            }
                                        } else if delta.delta_type == "text_delta" {
                                            if let Some(text) = delta.text {
                                                if !text.is_empty() {
                                                    let buffer = text_buffer.entry(index).or_default();
                                                    buffer.push_str(&text);

                                                    // Stream text deltas similar to thinking
                                                    let total_len = text_buffer.values().map(|s| s.len()).sum::<usize>();
                                                    if total_len > last_text_len {
                                                        let accumulated: String = text_buffer.values().cloned().collect::<Vec<_>>().join("");
                                                        last_text_len = total_len;

                                                        let _ = events_tx.send(AgentEvent::TextDelta {
                                                            content: accumulated,
                                                            mission_id: Some(mission_id),
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    StreamEvent::ContentBlockStart { index, content_block } => {
                                        block_types.insert(index, content_block.block_type.clone());

                                        if content_block.block_type == "tool_use" {
                                            if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                                                pending_tools.insert(id, name);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            AmpEvent::Assistant(evt) => {
                                // Track model from assistant message
                                if evt.message.model.is_some() {
                                    model_used = evt.message.model.clone();
                                }

                                // Accumulate token usage for cost calculation
                                if let Some(usage) = &evt.message.usage {
                                    total_input_tokens += usage.input_tokens.unwrap_or(0);
                                    total_output_tokens += usage.output_tokens.unwrap_or(0);
                                    total_cache_creation_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
                                    total_cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
                                }

                                for (content_idx, block) in evt.message.content.into_iter().enumerate() {
                                    let content_idx = content_idx as u32;
                                    match block {
                                        ContentBlock::Text { text } => {
                                            if !text.is_empty() {
                                                if !thinking_streamed {
                                                    if let Some((thought, cleaned)) =
                                                        extract_thought_line(&text)
                                                    {
                                                        let _ = events_tx.send(
                                                            AgentEvent::Thinking {
                                                                content: thought,
                                                                done: true,
                                                                mission_id: Some(mission_id),
                                                            },
                                                        );
                                                        thinking_streamed = true;
                                                        final_result = cleaned;
                                                    } else {
                                                        final_result = text;
                                                    }
                                                } else {
                                                    final_result = text;
                                                }
                                            }
                                        }
                                        ContentBlock::ToolUse { id, name, input } => {
                                            pending_tools.insert(id.clone(), name.clone());
                                            let _ = events_tx.send(AgentEvent::ToolCall {
                                                tool_call_id: id.clone(),
                                                name: name.clone(),
                                                args: input,
                                                mission_id: Some(mission_id),
                                            });
                                        }
                                        ContentBlock::Thinking { thinking } => {
                                            // Skip blocks already finalized during streaming
                                            if finalized_thinking_indices.contains(&content_idx) {
                                                continue;
                                            }
                                            if !thinking.is_empty() && !thinking_streamed {
                                                let _ = events_tx.send(AgentEvent::Thinking {
                                                    content: thinking,
                                                    done: true,
                                                    mission_id: Some(mission_id),
                                                });
                                            } else if thinking_streamed {
                                                // Send done=true signal without content to indicate thinking is complete
                                                let _ = events_tx.send(AgentEvent::Thinking {
                                                    content: String::new(),
                                                    done: true,
                                                    mission_id: Some(mission_id),
                                                });
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                // Reset per-turn accumulation state so the next turn
                                // starts fresh (block indices restart from 0 each turn)
                                thinking_buffer.clear();
                                text_buffer.clear();
                                active_thinking_index = None;
                                finalized_thinking_indices.clear();
                                last_text_len = 0;
                                block_types.clear();
                                thinking_streamed = false;
                            }
                            AmpEvent::User(evt) => {
                                for block in evt.message.content {
                                    if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                                        // Get tool name and remove from pending (tool is now complete)
                                        let name = pending_tools
                                            .remove(&tool_use_id)
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
                                            serde_json::json!(content_str)
                                        };

                                        let _ = events_tx.send(AgentEvent::ToolResult {
                                            tool_call_id: tool_use_id,
                                            name,
                                            result: result_value,
                                            mission_id: Some(mission_id),
                                        });
                                    }
                                }
                            }
                            AmpEvent::Result(res) => {
                                if res.is_error || res.subtype == "error" {
                                    had_error = true;
                                    let err_msg = res.error_message();
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        subtype = %res.subtype,
                                        result = ?res.result,
                                        error = ?res.error,
                                        message = ?res.message,
                                        raw_line = %if line.len() > 500 { let end = safe_truncate_index(&line, 500); format!("{}...", &line[..end]) } else { line.clone() },
                                        "Amp error result event"
                                    );
                                    // Don't send an Error event here - let the failure propagate
                                    // through the AgentResult. control.rs will emit an AssistantMessage
                                    // with success=false which the UI displays as a failure message.
                                    // Sending Error here would cause duplicate messages.
                                    final_result = err_msg;
                                } else if let Some(result) = res.result {
                                    final_result = result;
                                }

                                tracing::debug!(
                                    mission_id = %mission_id,
                                    subtype = %res.subtype,
                                    duration_ms = ?res.duration_ms,
                                    num_turns = ?res.num_turns,
                                    "Amp result received"
                                );

                                // Result event means we're done
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF
                        break;
                    }
                    Err(e) => {
                        tracing::error!(mission_id = %mission_id, error = %e, "Error reading Amp stdout");
                        break;
                    }
                }
            }
        }
    }

    // Wait for process to finish
    let exit_status = child.wait().await;

    // Wait for stderr capture to complete (don't abort - we need the content)
    if let Some(handle) = stderr_handle {
        let _ = handle.await;
    }

    // Compute cost from accumulated token usage
    let usage = crate::cost::TokenUsage {
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        cache_creation_input_tokens: if total_cache_creation_tokens > 0 {
            Some(total_cache_creation_tokens)
        } else {
            None
        },
        cache_read_input_tokens: if total_cache_read_tokens > 0 {
            Some(total_cache_read_tokens)
        } else {
            None
        },
    };
    let cost_cents = model_used
        .as_deref()
        .map(|m| crate::cost::cost_cents_from_usage(m, &usage))
        .unwrap_or(0);

    tracing::debug!(
        mission_id = %mission_id,
        model = ?model_used,
        input_tokens = total_input_tokens,
        output_tokens = total_output_tokens,
        cache_creation_tokens = total_cache_creation_tokens,
        cache_read_tokens = total_cache_read_tokens,
        cost_cents = cost_cents,
        "Amp cost computed from token usage"
    );

    // If no final result from Assistant or Result events, use accumulated text buffer
    if final_result.trim().is_empty() && !text_buffer.is_empty() {
        let mut sorted_entries: Vec<_> = text_buffer.iter().collect();
        sorted_entries.sort_by_key(|(idx, _)| *idx);
        final_result = sorted_entries
            .into_iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>()
            .join("");
        tracing::debug!(
            mission_id = %mission_id,
            "Using accumulated text buffer as final result ({} chars)",
            final_result.len()
        );
    }

    // If result is still empty/generic, include stderr for a useful error message
    if (final_result.trim().is_empty() || final_result == "Unknown error") && !had_error {
        had_error = true;
        let stderr_content = stderr_capture.lock().await;
        if !stderr_content.is_empty() {
            tracing::warn!(
                mission_id = %mission_id,
                stderr = %stderr_content,
                exit_status = ?exit_status,
                "Amp CLI produced no useful output but had stderr"
            );
            final_result = format!(
                "Amp error: {}",
                stderr_content
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        } else {
            tracing::warn!(
                mission_id = %mission_id,
                exit_status = ?exit_status,
                "Amp CLI produced no output and no stderr"
            );
            final_result =
                "Amp CLI produced no output. Check CLI installation or API key.".to_string();
        }
    } else if had_error && (final_result.trim().is_empty() || final_result == "Unknown error") {
        // Error was flagged by Result event but message is empty/generic - enrich with stderr
        let stderr_content = stderr_capture.lock().await;
        if !stderr_content.is_empty() {
            tracing::warn!(
                mission_id = %mission_id,
                stderr = %stderr_content,
                "Amp error with no result text, using stderr"
            );
            final_result = format!(
                "Amp error: {}",
                stderr_content
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        } else {
            final_result = "Amp CLI returned an error with no details. Check API key and network connectivity.".to_string();
        }
    }

    // Check exit status
    let success = match exit_status {
        Ok(status) => status.success() && !had_error,
        Err(e) => {
            tracing::error!(mission_id = %mission_id, error = %e, "Failed to wait for Amp process");
            false
        }
    };

    // Note: Do NOT emit AssistantMessage here - control.rs emits it based on AgentResult.
    // Emitting here would cause duplicate messages in the UI.

    let mut result = if success {
        AgentResult::success(final_result, cost_cents)
            .with_terminal_reason(TerminalReason::Completed)
    } else {
        // Detect rate limit / overloaded errors for account rotation.
        let reason = if is_rate_limited_error(&final_result) {
            TerminalReason::RateLimited
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_result, cost_cents).with_terminal_reason(reason)
    };

    if let Some(model) = model_used {
        result = result.with_model(model);
    }

    result
}

/// Compact info about a running mission (for API responses).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningMissionInfo {
    pub mission_id: Uuid,
    pub state: String,
    pub queue_len: usize,
    pub history_len: usize,
    pub seconds_since_activity: u64,
    pub health: MissionHealth,
    pub expected_deliverables: usize,
    /// Current activity label (e.g., "Reading: main.rs")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_activity: Option<String>,
    /// Total tracked subtasks
    pub subtask_total: usize,
    /// Completed subtasks
    pub subtask_completed: usize,
}

impl From<&MissionRunner> for RunningMissionInfo {
    fn from(runner: &MissionRunner) -> Self {
        let seconds_since_activity = runner.last_activity.elapsed().as_secs();
        Self {
            mission_id: runner.mission_id,
            state: match runner.state {
                MissionRunState::Queued => "queued".to_string(),
                MissionRunState::Running => "running".to_string(),
                MissionRunState::WaitingForTool => "waiting_for_tool".to_string(),
                MissionRunState::Finished => "finished".to_string(),
            },
            queue_len: runner.queue.len(),
            history_len: runner.history.len(),
            seconds_since_activity,
            health: running_health(runner.state, seconds_since_activity),
            expected_deliverables: runner.deliverables.deliverables.len(),
            current_activity: runner.current_activity.clone(),
            subtask_total: runner.subtasks.len(),
            subtask_completed: runner.subtasks.iter().filter(|s| s.completed).count(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_codex_turn(
    workspace: &Workspace,
    mission_work_dir: &std::path::Path,
    user_message: &str,
    model: Option<&str>,
    model_effort: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    _session_id: Option<&str>,
    override_api_key: Option<&str>,
) -> AgentResult {
    use crate::backend::codex::CodexBackend;
    use crate::backend::events::ExecutionEvent;
    use crate::backend::{Backend, SessionConfig};

    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let model_effort = model_effort.map(str::trim).filter(|m| !m.is_empty());
    let resolved_model: Option<String> = model.map(|m| m.to_string());

    tracing::info!(
        mission_id = %mission_id,
        requested_model = ?model,
        resolved_model = ?resolved_model,
        model_effort = ?model_effort,
        agent = ?agent,
        "Starting Codex turn"
    );

    // Best-effort: try to mint an OpenAI API key from the OAuth refresh token.
    // If this fails (e.g. no API platform org), write_codex_credentials_for_workspace
    // will fall back to auth_mode: "chatgpt" using the access_token directly.
    if let Err(e) = crate::api::ai_providers::ensure_openai_api_key_for_codex(app_working_dir).await
    {
        tracing::warn!(
            "Could not ensure OpenAI API key for Codex (will try chatgpt auth mode): {}",
            e
        );
    }

    // Ensure Codex auth.json is present in the workspace context (host or container).
    if let Err(e) = crate::api::ai_providers::write_codex_credentials_for_workspace(
        workspace,
        app_working_dir,
        override_api_key,
    ) {
        tracing::error!("Failed to write Codex credentials: {}", e);
        return AgentResult::failure(
            format!("Failed to configure Codex authentication: {}", e),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);
    }

    let workspace_exec = WorkspaceExec::new(workspace.clone());
    let cli_path = get_backend_string_setting("codex", "cli_path")
        .or_else(|| std::env::var("CODEX_CLI_PATH").ok())
        .unwrap_or_else(|| "codex".to_string());
    let cli_path = match ensure_codex_cli_available(&workspace_exec, mission_work_dir, &cli_path)
        .await
    {
        Ok(path) => path,
        Err(err_msg) => {
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    tracing::info!(
        mission_id = %mission_id,
        workspace_type = ?workspace.workspace_type,
        cli_path = %cli_path,
        model = ?model,
        "Starting Codex execution via WorkspaceExec"
    );

    let codex_config = crate::backend::codex::client::CodexConfig {
        cli_path,
        model_effort: model_effort.map(|s| s.to_string()),
        ..Default::default()
    };

    // Create Codex backend
    let backend = CodexBackend::with_config_and_workspace(codex_config, workspace_exec);

    // Create session
    let session = match backend
        .create_session(SessionConfig {
            directory: mission_work_dir.to_string_lossy().to_string(),
            title: Some(format!("Mission {}", mission_id)),
            model: resolved_model.clone(),
            agent: agent.map(|s| s.to_string()),
        })
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to create Codex session: {}", e);
            return AgentResult::failure(format!("Failed to start Codex: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Send message streaming
    let (mut event_rx, _handle) = match backend.send_message_streaming(&session, user_message).await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("Failed to send message to Codex: {}", e);
            return AgentResult::failure(format!("Codex execution failed: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Process events until completion or cancellation
    let mut assistant_message = String::new();
    let mut success = false;
    let mut error_message: Option<String> = None;
    let mut pending_tools: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut thinking_emitted = false;
    let mut thinking_done_emitted = false;
    let mut last_summary: Option<String> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("Codex turn cancelled for mission {}", mission_id);
                // Note: Codex process will be cleaned up automatically when the event stream task ends
                return AgentResult::failure("Mission cancelled".to_string(), 0)
                    .with_terminal_reason(TerminalReason::Cancelled);
            }
            Some(event) = event_rx.recv() => {
                match event {
                    ExecutionEvent::TextDelta { content } => {
                        assistant_message.push_str(&content);
                        let _ = events_tx.send(AgentEvent::TextDelta {
                            content: assistant_message.clone(),
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::Thinking { content } => {
                        let _ = events_tx.send(AgentEvent::Thinking {
                            content,
                            done: false,
                            mission_id: Some(mission_id),
                        });
                        thinking_emitted = true;
                    }
                    ExecutionEvent::ToolCall { id, name, args } => {
                        pending_tools.insert(id.clone(), name.clone());
                        let _ = events_tx.send(AgentEvent::ToolCall {
                            tool_call_id: id,
                            name,
                            args,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::ToolResult { id, name, result } => {
                        pending_tools.remove(&id);
                        let _ = events_tx.send(AgentEvent::ToolResult {
                            tool_call_id: id,
                            name,
                            result,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::TurnSummary { content } => {
                        if !content.trim().is_empty() {
                            last_summary = Some(content);
                        }
                    }
                    ExecutionEvent::Error { message } => {
                        error_message = Some(message.clone());
                        tracing::error!("Codex error: {}", message);
                    }
                    ExecutionEvent::MessageComplete { session_id: _ } => {
                        success = error_message.is_none();
                        break;
                    }
                }
            }
            else => {
                // Channel closed
                break;
            }
        }
    }

    if !thinking_emitted {
        if let Some((thought, cleaned)) = extract_thought_line(&assistant_message) {
            let _ = events_tx.send(AgentEvent::Thinking {
                content: thought,
                done: true,
                mission_id: Some(mission_id),
            });
            thinking_emitted = true;
            thinking_done_emitted = true;
            assistant_message = cleaned;
        }
    }

    if thinking_emitted && !thinking_done_emitted {
        let _ = events_tx.send(AgentEvent::Thinking {
            content: String::new(),
            done: true,
            mission_id: Some(mission_id),
        });
    }

    let no_output = assistant_message.trim().is_empty() && last_summary.is_none();
    if no_output && error_message.is_none() {
        success = false;
        error_message = Some(
            "Codex produced no output. This usually means the Codex CLI failed before emitting JSON (often authentication). Check that the host has a valid `~/.codex/auth.json` and that the backend can access it."
                .to_string(),
        );
    }

    let mut final_message = if let Some(err) = error_message {
        err
    } else if !assistant_message.is_empty() {
        assistant_message
    } else if let Some(summary) = last_summary {
        summary
    } else {
        "No response from Codex".to_string()
    };

    let lower_final = final_message.to_lowercase();
    if lower_final.contains("does not exist or you do not have access")
        || lower_final.contains("model_not_found")
    {
        final_message
            .push_str("\n\nTry model `gpt-5-codex` or `gpt-5.1-codex` for Codex missions.");
        if matches!(model, Some("gpt-5.3-codex") | Some("gpt-5.3-codex-spark")) {
            final_message.push_str(
                "\n\nIf you expected GPT-5.3 Codex to work, your Codex CLI may be outdated. \
Update it to the latest version (`npm install -g @openai/codex@latest`) and retry.",
            );
        }
    }

    let mut result = if success {
        AgentResult::success(final_message, 0) // TODO: Calculate cost from Codex usage
            .with_terminal_reason(TerminalReason::Completed)
    } else {
        // Detect rate limit / overloaded errors for account rotation.
        let reason = if is_rate_limited_error(&final_message) {
            TerminalReason::RateLimited
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_message, 0).with_terminal_reason(reason)
    };

    if let Some(m) = resolved_model.as_deref() {
        result = result.with_model(m.to_string());
    }

    result
}

/// Generate a concise summary of recent conversation turns for session rotation.
/// Summarizes the last N turns to preserve context when starting a new session.
fn generate_session_summary(history: &[(String, String)], last_n_turns: usize) -> String {
    // Get the last N turns (user + assistant pairs)
    let recent_entries: Vec<_> = history
        .iter()
        .rev()
        .take(last_n_turns * 2) // Each turn = user + assistant message
        .rev()
        .collect();

    if recent_entries.is_empty() {
        return "No previous work to summarize.".to_string();
    }

    // Build a concise summary focusing on key accomplishments
    let mut summary_lines = Vec::new();
    let mut last_user_request = None;
    let mut accomplishments = Vec::new();

    // Save length before consuming iterator
    let entry_count = recent_entries.len();
    // Use a HashSet to track already-added lines to avoid duplicates across all messages
    let mut seen_lines = std::collections::HashSet::new();

    for (role, content) in &recent_entries {
        match role.as_str() {
            "user" => {
                last_user_request = Some(content.lines().next().unwrap_or(content).to_string());
            }
            "assistant" => {
                // Extract key accomplishments from assistant responses
                // Look for files created, commands run, decisions made

                let keywords = [
                    ("created", "Created"),
                    ("implemented", "Implemented"),
                    ("fixed", "Fixed"),
                ];

                for (lower_kw, upper_kw) in &keywords {
                    if content.contains(lower_kw) || content.contains(upper_kw) {
                        if let Some(line) = content.lines().find(|l| {
                            (l.contains(lower_kw) || l.contains(upper_kw))
                                && !seen_lines.contains(l.trim())
                        }) {
                            let trimmed = line.trim().to_string();
                            seen_lines.insert(trimmed.clone());
                            accomplishments.push(trimmed);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Build summary
    if let Some(request) = last_user_request {
        summary_lines.push(format!(
            "**Last Request:** {}",
            request.chars().take(200).collect::<String>()
        ));
    }

    if !accomplishments.is_empty() {
        summary_lines.push("**Recent Work:**".to_string());
        for (i, accomplishment) in accomplishments.iter().take(10).enumerate() {
            summary_lines.push(format!(
                "{}. {}",
                i + 1,
                accomplishment.chars().take(150).collect::<String>()
            ));
        }
    } else {
        summary_lines.push(format!("**Conversation Context:** Discussed {} topics over the last {} turns. Continue from previous context.", entry_count / 2, last_n_turns));
    }

    summary_lines.join("\n")
}

/// Clean up old debug files to prevent disk bloat and reduce memory pressure.
/// Keeps only the most recent N debug files, deleting older ones.
fn cleanup_old_debug_files(
    workspace_dir: &std::path::Path,
    keep_last_n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let debug_dir = workspace_dir.join(".claude").join("debug");

    // Skip if debug directory doesn't exist
    if !debug_dir.exists() {
        return Ok(());
    }

    // Collect all debug files with their modification times
    let mut files: Vec<_> = std::fs::read_dir(&debug_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            // Only process .txt files (debug logs)
            if path.extension().and_then(|s| s.to_str()) != Some("txt") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified = metadata.modified().ok()?;
            Some((path, modified))
        })
        .collect();

    // Sort by modification time (oldest first)
    files.sort_by_key(|(_, modified)| *modified);

    // Keep only the last N files
    let to_delete = files.len().saturating_sub(keep_last_n);
    for (path, _) in files.iter().take(to_delete) {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to delete old debug file"
            );
        } else {
            tracing::debug!(
                path = %path.display(),
                "Deleted old debug file"
            );
        }
    }

    if to_delete > 0 {
        tracing::info!(
            deleted_count = to_delete,
            kept_count = keep_last_n,
            debug_dir = %debug_dir.display(),
            "Cleaned up old debug files"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        extract_opencode_session_id, extract_part_text, extract_str, extract_thought_line,
        is_codex_node_wrapper, is_rate_limited_error, is_session_corruption_error,
        is_tool_call_only_output, opencode_output_needs_fallback, opencode_session_token_from_line,
        parse_opencode_session_token, parse_opencode_stderr_text_part, running_health,
        sanitized_opencode_stdout, stall_severity, strip_ansi_codes, strip_opencode_banner_lines,
        strip_think_tags, summarize_recent_opencode_stderr, sync_opencode_agent_config,
        MissionHealth, MissionRunState, MissionStallSeverity, STALL_SEVERE_SECS, STALL_WARN_SECS,
    };
    use crate::agents::{AgentResult, TerminalReason};
    use serde_json::json;
    use std::borrow::Cow;
    use std::fs;

    #[test]
    fn sync_opencode_agent_config_removes_overrides_when_plugin_enabled() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_dir = temp_dir.path();

        fs::write(
            config_dir.join("oh-my-opencode.json"),
            r#"{
  "agents": {
    "prometheus": { "model": "openai/gpt-4o" },
    "sisyphus": {}
  }
}"#,
        )
        .expect("write oh-my-opencode.json");

        fs::write(
            config_dir.join("opencode.json"),
            r#"{
  "plugin": ["oh-my-opencode@0.0.1"],
  "agent": {
    "prometheus": { "model": "openai/gpt-4o-mini", "foo": "bar" },
    "sisyphus": {},
    "custom": { "model": "openai/gpt-4o" }
  }
}"#,
        )
        .expect("write opencode.json");

        sync_opencode_agent_config(config_dir, Some("openai/gpt-4o-mini"), true, false, false);

        let opencode_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(config_dir.join("opencode.json")).expect("read opencode.json"),
        )
        .expect("parse opencode.json");
        let agents = opencode_json
            .get("agent")
            .and_then(|v| v.as_object())
            .expect("agent object");

        assert!(agents.get("prometheus").is_none());
        assert!(agents.get("sisyphus").is_none());
        assert!(agents.get("custom").is_some());
    }

    #[test]
    fn sync_opencode_agent_config_writes_overrides_when_plugin_disabled() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_dir = temp_dir.path();

        fs::write(
            config_dir.join("oh-my-opencode.json"),
            r#"{
  "agents": {
    "prometheus": { "model": "openai/gpt-4o" },
    "sisyphus": {}
  }
}"#,
        )
        .expect("write oh-my-opencode.json");

        sync_opencode_agent_config(config_dir, Some("openai/gpt-4o-mini"), true, false, false);

        let opencode_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(config_dir.join("opencode.json")).expect("read opencode.json"),
        )
        .expect("parse opencode.json");
        let agents = opencode_json
            .get("agent")
            .and_then(|v| v.as_object())
            .expect("agent object");

        let prometheus_model = agents
            .get("prometheus")
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str())
            .expect("prometheus model");
        let sisyphus_model = agents
            .get("sisyphus")
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str())
            .expect("sisyphus model");

        assert_eq!(prometheus_model, "openai/gpt-4o");
        assert_eq!(sisyphus_model, "openai/gpt-4o-mini");
    }

    #[test]
    fn is_opencode_banner_line_detects_runner_status() {
        use super::is_opencode_banner_line;

        // Runner lifecycle banners
        assert!(is_opencode_banner_line("Starting opencode server"));
        assert!(is_opencode_banner_line(
            "Starting OpenCode server (auto port selection enabled)..."
        ));
        assert!(is_opencode_banner_line("opencode server started"));
        assert!(is_opencode_banner_line(
            "OpenCode server started on port 4096"
        ));

        // Port selection
        assert!(is_opencode_banner_line("auto-selected port 44563"));
        assert!(is_opencode_banner_line("Using port 44563"));
        assert!(is_opencode_banner_line("using port 4096"));

        // Server status
        assert!(is_opencode_banner_line(
            "server listening on 127.0.0.1:4096"
        ));
        assert!(is_opencode_banner_line("Server listening..."));

        // Prompt/completion status
        assert!(is_opencode_banner_line("Sending prompt..."));
        assert!(is_opencode_banner_line("Waiting for completion..."));
        assert!(is_opencode_banner_line("All tasks completed."));

        // Session identification
        assert!(is_opencode_banner_line("Session ID: ses_abc123"));
        assert!(is_opencode_banner_line("Session: ses_abc123"));

        // [run]-prefixed lines
        assert!(is_opencode_banner_line("[run] Starting execution"));
        assert!(is_opencode_banner_line("[RUN] task started"));
    }

    #[test]
    fn is_opencode_banner_line_rejects_model_text() {
        use super::is_opencode_banner_line;

        // Model responses should NOT be detected as banner lines
        assert!(!is_opencode_banner_line("Hello, I am the assistant."));
        assert!(!is_opencode_banner_line("Let me help you with that."));
        assert!(!is_opencode_banner_line("Here's the code you requested:"));
        assert!(!is_opencode_banner_line(
            "The file has been modified successfully."
        ));
        assert!(!is_opencode_banner_line("I found 3 issues in your code."));
        assert!(!is_opencode_banner_line(
            "If you see 'All tasks completed', the build finished."
        ));
    }

    #[test]
    fn is_rate_limited_error_detects_markers_case_insensitively() {
        assert!(is_rate_limited_error("Error: 429 Too Many Requests"));
        assert!(is_rate_limited_error("resource_exhausted: slow down"));
        assert!(is_rate_limited_error("Overloaded_Error occurred"));
        assert!(!is_rate_limited_error("Model finished successfully"));
        assert!(!is_rate_limited_error("error: 123"));
    }

    #[test]
    fn extract_opencode_session_id_matches_case_insensitively() {
        let source = "noise\nSESSION ID: ses_abc123\nmore noise";
        assert_eq!(
            extract_opencode_session_id(source),
            Some("ses_abc123".to_string())
        );

        let equals_variant = "Session=SES_DEF456";
        assert_eq!(
            extract_opencode_session_id(equals_variant),
            Some("SES_DEF456".to_string())
        );

        assert!(extract_opencode_session_id("no session here").is_none());
    }

    #[test]
    fn opencode_session_token_from_line_parses_supported_variants() {
        assert_eq!(
            opencode_session_token_from_line("Session ID: ses_abc123"),
            Some("ses_abc123")
        );
        assert_eq!(
            opencode_session_token_from_line("session: SES_DEF456"),
            Some("SES_DEF456")
        );
        assert_eq!(
            opencode_session_token_from_line("session_id: foo-bar-123"),
            Some("foo-bar-123")
        );
        assert_eq!(
            opencode_session_token_from_line("session=foo_bar_789"),
            Some("foo_bar_789")
        );
        assert_eq!(opencode_session_token_from_line("session=foo_bar"), None);
        assert_eq!(opencode_session_token_from_line("session id: short"), None);
        assert_eq!(opencode_session_token_from_line("no session here"), None);
    }

    #[test]
    fn strip_opencode_banner_lines_removes_runner_status() {
        // Pure banner output should become empty
        let input = "Starting opencode server (auto port selection enabled)...\nUsing port 44563\nSession: ses_abc\nSending prompt...\nWaiting for completion...\nAll tasks completed.";
        let result = strip_opencode_banner_lines(input);
        assert!(result.trim().is_empty());

        // Mixed output should keep only non-banner lines
        let mixed = "Starting opencode server...\nHello, I am the model.\nAll tasks completed.";
        let result = strip_opencode_banner_lines(mixed);
        assert_eq!(result.trim(), "Hello, I am the model.");

        // Non-banner output should be preserved
        let model_output = "Here's the solution:\n\n```python\nprint('hello')\n```";
        let result = strip_opencode_banner_lines(model_output);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, model_output);
    }

    #[test]
    fn strip_opencode_banner_lines_preserves_inner_whitespace() {
        let input = "Starting opencode server...\n\n  indented line\n[run] helper\ntrailing  \n";
        let result = strip_opencode_banner_lines(input);
        assert_eq!(result.as_ref(), "\n  indented line\ntrailing  ");
    }

    #[test]
    fn strip_ansi_codes_removes_csi_and_osc_sequences() {
        let input = "\u{1b}[31mred\u{1b}[0m normal \u{1b}]0;title\u{7}text";
        let cleaned = strip_ansi_codes(input);
        assert_eq!(cleaned, "red normal text");
    }

    #[test]
    fn strip_ansi_codes_handles_st_terminated_sequences() {
        let input = "\u{1b}]52;c;payload\u{1b}\\body\u{1b}[?25l";
        let cleaned = strip_ansi_codes(input);
        assert_eq!(cleaned, "body");
    }

    #[test]
    fn strip_ansi_codes_removes_disallowed_control_bytes() {
        let input = "\0leading\u{1f}middle\u{7f}end";
        let cleaned = strip_ansi_codes(input);
        assert_eq!(cleaned, "leadingmiddleend");
    }

    #[test]
    fn sanitized_opencode_stdout_strips_ansi_and_banners() {
        let noisy = "\u{1b}[31mStarting opencode server...\u{1b}[0m\n[run] helper\nreal output";
        let sanitized = sanitized_opencode_stdout(noisy);
        assert_eq!(sanitized, "real output");
        assert!(matches!(sanitized, Cow::Owned(_)));

        let clean = "Here is the answer";
        let passthrough = sanitized_opencode_stdout(clean);
        assert_eq!(passthrough, clean);
        assert!(matches!(passthrough, Cow::Borrowed(_)));
    }

    #[test]
    fn opencode_output_needs_fallback_detects_banner_only() {
        // Empty output needs fallback
        assert!(opencode_output_needs_fallback(""));
        assert!(opencode_output_needs_fallback("   "));
        assert!(opencode_output_needs_fallback("\n\n"));

        // Banner-only output needs fallback
        let banner_only = "Starting opencode server...\nAll tasks completed.";
        assert!(opencode_output_needs_fallback(banner_only));

        // Output with real content does NOT need fallback
        let with_content =
            "Starting opencode server...\nHello, I am the model.\nAll tasks completed.";
        assert!(!opencode_output_needs_fallback(with_content));

        // Pure model output does NOT need fallback
        let model_only = "Here is your answer: 42";
        assert!(!opencode_output_needs_fallback(model_only));
    }

    #[test]
    fn opencode_output_needs_fallback_detects_exit_status_placeholder() {
        let status_only = "OpenCode CLI exited with status: exit status: 1";
        assert!(opencode_output_needs_fallback(status_only));

        let status_with_stderr = "OpenCode CLI exited with status: exit status: 1. Last stderr: session.error: Requested entity was not found";
        assert!(opencode_output_needs_fallback(status_with_stderr));

        let normal_text = "The OpenCode CLI exited with status: 1 in a prior run, now fixed.";
        assert!(!opencode_output_needs_fallback(normal_text));
    }

    #[test]
    fn summarize_recent_opencode_stderr_prefers_last_meaningful_line() {
        use std::collections::VecDeque;

        let mut lines = VecDeque::new();
        lines.push_back("server.connected".to_string());
        lines.push_back("message.updated (assistant, build)".to_string());
        lines.push_back("response.error: 404 Not Found".to_string());

        assert_eq!(
            summarize_recent_opencode_stderr(&lines).as_deref(),
            Some("response.error: 404 Not Found")
        );
    }

    #[test]
    fn summarize_recent_opencode_stderr_filters_skill_activation_messages() {
        use std::collections::VecDeque;

        let mut lines = VecDeque::new();
        lines.push_back("server.connected".to_string());
        lines.push_back("Start now using github-cli skill".to_string());

        assert_eq!(summarize_recent_opencode_stderr(&lines), None);
    }
    #[test]
    fn strip_opencode_banner_lines_handles_ansi_codes() {
        use super::strip_opencode_banner_lines;

        // ANSI-prefixed banner lines should be stripped too
        let input_with_ansi = "\x1b[32mStarting opencode server\x1b[0m\n\x1b[33mUsing port 44563\x1b[0m\nHello, I am the model.";
        let result = strip_opencode_banner_lines(input_with_ansi);
        assert_eq!(result.trim(), "Hello, I am the model.");

        // Pure ANSI-wrapped banners should become empty
        let ansi_only =
            "\x1b[32mStarting opencode server\x1b[0m\n\x1b[33mAll tasks completed.\x1b[0m";
        let result = strip_opencode_banner_lines(ansi_only);
        assert!(result.trim().is_empty());
    }

    // ── extract_str tests ─────────────────────────────────────────────

    #[test]
    fn extract_str_returns_first_matching_key() {
        let val = json!({"text": "hello", "content": "world"});
        assert_eq!(extract_str(&val, &["text", "content"]), Some("hello"));
    }

    #[test]
    fn extract_str_returns_none_when_no_keys_match() {
        let val = json!({"foo": "bar"});
        assert_eq!(extract_str(&val, &["text", "content"]), None);
    }

    #[test]
    fn extract_str_skips_non_string_values() {
        let val = json!({"text": 42, "content": "hello"});
        assert_eq!(extract_str(&val, &["text", "content"]), Some("hello"));
    }

    // ── extract_part_text tests ───────────────────────────────────────

    #[test]
    fn extract_part_text_thinking_type_checks_thinking_key_first() {
        let val = json!({"thinking": "deep thought", "text": "surface"});
        assert_eq!(extract_part_text(&val, "thinking"), Some("deep thought"));
    }

    #[test]
    fn extract_part_text_thinking_type_falls_back_to_text() {
        let val = json!({"text": "some text"});
        assert_eq!(extract_part_text(&val, "reasoning"), Some("some text"));
    }

    #[test]
    fn extract_part_text_normal_type_checks_text_first() {
        let val = json!({"text": "hello", "content": "world"});
        assert_eq!(extract_part_text(&val, "text"), Some("hello"));
    }

    #[test]
    fn extract_part_text_normal_type_falls_back_to_output_text() {
        let val = json!({"output_text": "result"});
        assert_eq!(extract_part_text(&val, "message"), Some("result"));
    }

    #[test]
    fn extract_part_text_step_types_use_thinking_key_priority() {
        let val = json!({"reasoning": "step reason"});
        assert_eq!(extract_part_text(&val, "step-start"), Some("step reason"));
        assert_eq!(extract_part_text(&val, "step-finish"), Some("step reason"));
    }

    // ── strip_think_tags tests ────────────────────────────────────────

    #[test]
    fn strip_think_tags_no_tags_returns_original() {
        let input = "Hello world, no tags here.";
        assert_eq!(strip_think_tags(input), input);
    }

    #[test]
    fn strip_think_tags_removes_single_block() {
        let input = "before<think>secret</think>after";
        assert_eq!(strip_think_tags(input), "beforeafter");
    }

    #[test]
    fn strip_think_tags_removes_multiple_blocks() {
        let input = "a<think>1</think>b<think>2</think>c";
        assert_eq!(strip_think_tags(input), "abc");
    }

    #[test]
    fn strip_think_tags_case_insensitive() {
        let input = "x<THINK>hidden</THINK>y<Think>also</Think>z";
        assert_eq!(strip_think_tags(input), "xyz");
    }

    #[test]
    fn strip_think_tags_unclosed_tag_drops_rest() {
        let input = "visible<think>invisible with no close";
        assert_eq!(strip_think_tags(input), "visible");
    }

    #[test]
    fn strip_think_tags_empty_content() {
        let input = "<think></think>";
        assert_eq!(strip_think_tags(input), "");
    }

    // ── extract_thought_line tests ────────────────────────────────────

    #[test]
    fn extract_thought_line_extracts_thought_prefix() {
        let input = "thought: I need to check the file\nLet me look at it.";
        let (thought, remaining) = extract_thought_line(input).unwrap();
        assert_eq!(thought, "I need to check the file");
        assert_eq!(remaining, "Let me look at it.");
    }

    #[test]
    fn extract_thought_line_extracts_thinking_prefix() {
        let input = "Thinking: Analyzing the problem\nHere is my answer.";
        let (thought, remaining) = extract_thought_line(input).unwrap();
        assert_eq!(thought, "Analyzing the problem");
        assert_eq!(remaining, "Here is my answer.");
    }

    #[test]
    fn extract_thought_line_extracts_thoughts_prefix() {
        let input = "thoughts: multiple ideas\nLine 2";
        let (thought, _) = extract_thought_line(input).unwrap();
        assert_eq!(thought, "multiple ideas");
    }

    #[test]
    fn extract_thought_line_returns_none_without_thought() {
        let input = "Just regular text\nMore text";
        assert!(extract_thought_line(input).is_none());
    }

    #[test]
    fn extract_thought_line_returns_none_for_empty_thought() {
        let input = "thought: \nsome text after";
        assert!(extract_thought_line(input).is_none());
    }

    #[test]
    fn extract_thought_line_only_first_thought_line_extracted() {
        let input = "thought: first\nthought: second\nregular text";
        let (thought, remaining) = extract_thought_line(input).unwrap();
        assert_eq!(thought, "first");
        assert!(remaining.contains("thought: second"));
        assert!(remaining.contains("regular text"));
    }

    // ── strip_ansi_codes tests ────────────────────────────────────────

    #[test]
    fn strip_ansi_codes_removes_color_codes() {
        assert_eq!(strip_ansi_codes("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(
            strip_ansi_codes("\x1b[1;32mbold green\x1b[0m"),
            "bold green"
        );
    }

    #[test]
    fn strip_ansi_codes_no_codes_unchanged() {
        let input = "plain text with no ANSI";
        assert_eq!(strip_ansi_codes(input), input);
    }

    #[test]
    fn strip_ansi_codes_empty_string() {
        assert_eq!(strip_ansi_codes(""), "");
    }

    #[test]
    fn strip_ansi_codes_multiple_codes_in_sequence() {
        let input = "\x1b[1m\x1b[31mhello\x1b[0m \x1b[32mworld\x1b[0m";
        assert_eq!(strip_ansi_codes(input), "hello world");
    }

    // ── is_tool_call_only_output tests ────────────────────────────────

    #[test]
    fn is_tool_call_only_output_detects_tool_use_type() {
        let output = r#"{"type":"tool_use","id":"abc","name":"read","input":{}}"#;
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_detects_function_call_type() {
        let output = r#"{"type":"function_call","id":"abc","name":"write","input":{}}"#;
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_detects_name_plus_arguments_shape() {
        let output = r#"{"name":"read_file","arguments":{"path":"/tmp/test"}}"#;
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_detects_name_plus_input_shape() {
        let output = r#"{"name":"read_file","input":{"path":"/tmp/test"}}"#;
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_false_for_empty() {
        assert!(!is_tool_call_only_output(""));
        assert!(!is_tool_call_only_output("   "));
    }

    #[test]
    fn is_tool_call_only_output_false_for_real_text() {
        assert!(!is_tool_call_only_output("Here is the code you asked for."));
    }

    #[test]
    fn is_tool_call_only_output_false_for_mixed_content() {
        let output = r#"{"name":"read","input":{}}\nActual model text here"#;
        assert!(!is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_ignores_banner_lines() {
        let output =
            "Starting opencode server\n{\"type\":\"tool_use\",\"name\":\"read\",\"input\":{}}";
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_multiple_tool_calls() {
        let output = "{\"name\":\"a\",\"arguments\":{}}\n{\"name\":\"b\",\"input\":{}}";
        assert!(is_tool_call_only_output(output));
    }

    #[test]
    fn is_tool_call_only_output_json_without_tool_markers() {
        let output = r#"{"result": "success", "count": 42}"#;
        assert!(!is_tool_call_only_output(output));
    }

    // ── stall_severity tests ──────────────────────────────────────────

    #[test]
    fn stall_severity_none_below_warning_threshold() {
        assert!(stall_severity(0).is_none());
        assert!(stall_severity(60).is_none());
        assert!(stall_severity(STALL_WARN_SECS).is_none());
    }

    #[test]
    fn stall_severity_warning_above_warn_threshold() {
        let result = stall_severity(STALL_WARN_SECS + 1).unwrap();
        assert!(matches!(result, MissionStallSeverity::Warning));
    }

    #[test]
    fn stall_severity_severe_above_severe_threshold() {
        let result = stall_severity(STALL_SEVERE_SECS + 1).unwrap();
        assert!(matches!(result, MissionStallSeverity::Severe));
    }

    #[test]
    fn stall_severity_at_exact_severe_threshold_is_still_warning() {
        let result = stall_severity(STALL_SEVERE_SECS).unwrap();
        assert!(matches!(result, MissionStallSeverity::Warning));
    }

    // ── running_health tests ──────────────────────────────────────────

    #[test]
    fn running_health_healthy_when_running_below_threshold() {
        let health = running_health(MissionRunState::Running, 10);
        assert!(matches!(health, MissionHealth::Healthy));
    }

    #[test]
    fn running_health_stalled_when_running_above_threshold() {
        let health = running_health(MissionRunState::Running, STALL_WARN_SECS + 1);
        match health {
            MissionHealth::Stalled {
                seconds_since_activity,
                last_state,
                severity,
            } => {
                assert_eq!(seconds_since_activity, STALL_WARN_SECS + 1);
                assert_eq!(last_state, "Running");
                assert!(matches!(severity, MissionStallSeverity::Warning));
            }
            other => panic!("Expected Stalled, got {:?}", other),
        }
    }

    #[test]
    fn running_health_stalled_when_waiting_for_tool_above_threshold() {
        let health = running_health(MissionRunState::WaitingForTool, STALL_SEVERE_SECS + 1);
        match health {
            MissionHealth::Stalled {
                last_state,
                severity,
                ..
            } => {
                assert_eq!(last_state, "WaitingForTool");
                assert!(matches!(severity, MissionStallSeverity::Severe));
            }
            other => panic!("Expected Stalled, got {:?}", other),
        }
    }

    #[test]
    fn running_health_healthy_for_queued_state_even_if_stale() {
        let health = running_health(MissionRunState::Queued, STALL_SEVERE_SECS + 100);
        assert!(matches!(health, MissionHealth::Healthy));
    }

    #[test]
    fn running_health_healthy_for_finished_state() {
        let health = running_health(MissionRunState::Finished, STALL_SEVERE_SECS + 100);
        assert!(matches!(health, MissionHealth::Healthy));
    }

    // ── is_session_corruption_error tests ─────────────────────────────

    #[test]
    fn is_session_corruption_error_false_for_success() {
        let result = AgentResult::success("all good", 0);
        assert!(!is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_false_for_non_llm_error() {
        let result = AgentResult::failure("something failed", 0)
            .with_terminal_reason(TerminalReason::Stalled);
        assert!(!is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_no_stream_events() {
        let result = AgentResult::failure(
            "Claude Code produced no stream events after startup timeout",
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_tool_use_id_mismatch() {
        let result = AgentResult::failure("unexpected tool_use_id found in tool_result blocks", 0)
            .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_missing_tool_result() {
        let result =
            AgentResult::failure("tool_use block must have a corresponding tool_result", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_missing_tool_use() {
        let result =
            AgentResult::failure("tool_result block must have a corresponding tool_use", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_must_have_corresponding() {
        let result = AgentResult::failure("must have a corresponding tool_use block", 0)
            .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_detects_lost_session() {
        let result = AgentResult::failure("No conversation found with session ID ses_abc", 0)
            .with_terminal_reason(TerminalReason::LlmError);
        assert!(is_session_corruption_error(&result));
    }

    #[test]
    fn is_session_corruption_error_false_for_other_llm_error() {
        let result = AgentResult::failure("rate limit exceeded", 0)
            .with_terminal_reason(TerminalReason::LlmError);
        assert!(!is_session_corruption_error(&result));
    }

    // ── parse_opencode_session_token tests ────────────────────────────

    #[test]
    fn parse_opencode_session_token_ses_prefix() {
        assert_eq!(
            parse_opencode_session_token("ses_abc123"),
            Some("ses_abc123")
        );
    }

    #[test]
    fn parse_opencode_session_token_ses_prefix_short() {
        // ses_ prefix is accepted regardless of length
        assert_eq!(parse_opencode_session_token("ses_a"), Some("ses_a"));
    }

    #[test]
    fn parse_opencode_session_token_long_token_without_prefix() {
        assert_eq!(parse_opencode_session_token("abcdefgh"), Some("abcdefgh"));
    }

    #[test]
    fn parse_opencode_session_token_short_token_without_prefix_rejected() {
        assert_eq!(parse_opencode_session_token("abc"), None);
    }

    #[test]
    fn parse_opencode_session_token_stops_at_non_alnum_char() {
        assert_eq!(
            parse_opencode_session_token("ses_abc!rest"),
            Some("ses_abc")
        );
    }

    #[test]
    fn parse_opencode_session_token_allows_hyphens_and_underscores() {
        assert_eq!(
            parse_opencode_session_token("ses_abc-def_ghi"),
            Some("ses_abc-def_ghi")
        );
    }

    #[test]
    fn parse_opencode_session_token_empty_string() {
        assert_eq!(parse_opencode_session_token(""), None);
    }

    // ── parse_opencode_stderr_text_part tests ─────────────────────────

    #[test]
    fn parse_opencode_stderr_text_part_extracts_text() {
        let line = r#"some prefix message.part (text): "Hello world""#;
        assert_eq!(
            parse_opencode_stderr_text_part(line),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn parse_opencode_stderr_text_part_handles_escape_sequences() {
        let line = r#"message.part (text): "line1\nline2""#;
        assert_eq!(
            parse_opencode_stderr_text_part(line),
            Some("line1\nline2".to_string())
        );
    }

    #[test]
    fn parse_opencode_stderr_text_part_handles_escaped_backslash() {
        let line = r#"message.part (text): "path\\file""#;
        assert_eq!(
            parse_opencode_stderr_text_part(line),
            Some("path\\file".to_string())
        );
    }

    #[test]
    fn parse_opencode_stderr_text_part_handles_escaped_quotes() {
        let line = r#"message.part (text): "say \"hello\"""#;
        assert_eq!(
            parse_opencode_stderr_text_part(line),
            Some("say \"hello\"".to_string())
        );
    }

    #[test]
    fn parse_opencode_stderr_text_part_no_marker_returns_none() {
        let line = "just a regular log line";
        assert_eq!(parse_opencode_stderr_text_part(line), None);
    }

    #[test]
    fn parse_opencode_stderr_text_part_empty_content_returns_none() {
        let line = r#"message.part (text): """#;
        assert_eq!(parse_opencode_stderr_text_part(line), None);
    }

    #[test]
    fn parse_opencode_stderr_text_part_without_quotes() {
        let line = "message.part (text): Hello world";
        assert_eq!(
            parse_opencode_stderr_text_part(line),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn opencode_output_needs_fallback_ignores_ansi_banners() {
        let banner_with_ansi = "\u{1b}[32mStarting opencode server...\u{1b}[0m";
        assert!(opencode_output_needs_fallback(banner_with_ansi));

        let ansi_with_content = "\u{1b}[33mStarting opencode server...\u{1b}[0m\nreal output";
        assert!(!opencode_output_needs_fallback(ansi_with_content));
    }

    #[test]
    fn is_tool_call_only_output_detects_tool_json_after_sanitizing() {
        let ansi_tool = "\u{1b}[32mStarting opencode server...\u{1b}[0m\n{\"name\":\"do\",\"arguments\":\"{}\"}";
        assert!(is_tool_call_only_output(ansi_tool));
    }

    #[test]
    fn is_tool_call_only_output_rejects_real_text() {
        let mixed = "{\"name\":\"tool\",\"arguments\":\"{}\"}\nreal answer";
        assert!(!is_tool_call_only_output(mixed));
    }

    // ── is_codex_node_wrapper tests ─────────────────────────────────────

    #[test]
    fn is_codex_node_wrapper_detects_npm_installed_wrapper() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let wrapper_path = temp_dir.path().join("codex");
        let mut file = std::fs::File::create(&wrapper_path).unwrap();
        writeln!(
            file,
            "#!/usr/bin/env node\nconst {{ spawn }} = require('child_process');\n// @openai/codex wrapper"
        )
        .unwrap();

        assert!(is_codex_node_wrapper(&wrapper_path));
    }

    #[test]
    fn is_codex_node_wrapper_detects_bun_installed_wrapper() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let wrapper_path = temp_dir.path().join("codex");
        let mut file = std::fs::File::create(&wrapper_path).unwrap();
        writeln!(
            file,
            "#!/usr/bin/env node\n// references codex-linux-x64 optional dep"
        )
        .unwrap();

        assert!(is_codex_node_wrapper(&wrapper_path));
    }

    #[test]
    fn is_codex_node_wrapper_rejects_native_binary() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let wrapper_path = temp_dir.path().join("codex");
        let mut file = std::fs::File::create(&wrapper_path).unwrap();
        write!(file, "\x7fELF\x02\x01\x01\x00").unwrap();

        assert!(!is_codex_node_wrapper(&wrapper_path));
    }

    #[test]
    fn is_codex_node_wrapper_rejects_shell_script() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let wrapper_path = temp_dir.path().join("codex");
        let mut file = std::fs::File::create(&wrapper_path).unwrap();
        writeln!(file, "#!/bin/bash\necho 'hello'").unwrap();

        assert!(!is_codex_node_wrapper(&wrapper_path));
    }

    #[test]
    fn is_codex_node_wrapper_rejects_nonexistent_file() {
        let wrapper_path = std::path::Path::new("/nonexistent/path/codex");
        assert!(!is_codex_node_wrapper(wrapper_path));
    }
}
