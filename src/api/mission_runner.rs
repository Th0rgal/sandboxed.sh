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
use crate::workspace::{self, Workspace, WorkspaceType};
use crate::workspace_exec::WorkspaceExec;

use super::control::{
    resolve_claudecode_default_model, safe_truncate_index, AgentEvent, AgentTreeNode,
    ControlStatus, ExecutionProgress, FrontendToolHub,
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
}

struct OpencodeSseParseResult {
    event: Option<AgentEvent>,
    message_complete: bool,
    session_id: Option<String>,
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
    if part_type == "thinking" || part_type == "reasoning" {
        extract_str(part, &["thinking", "text", "content"])
    } else {
        extract_str(part, &["text", "content", "output_text"])
    }
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

fn handle_tool_part_update(
    part: &serde_json::Value,
    state: &mut OpencodeSseState,
    mission_id: Uuid,
) -> Option<AgentEvent> {
    let state_obj = part.get("state")?;
    let status = state_obj.get("status").and_then(|v| v.as_str())?;

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

    if !matches!(part_type, "thinking" | "reasoning") {
        return None;
    }

    let part_id = extract_str(part, &["id", "partID", "partId"]);
    let message_id = extract_str(part, &["messageID", "messageId", "message_id"])
        .or_else(|| extract_str(props, &["messageID", "messageId", "message_id"]));
    if let Some(message_id) = message_id {
        if let Some(role) = state.message_roles.get(message_id) {
            if role != "assistant" {
                return None;
            }
        }
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

    let filtered = strip_opencode_status_lines(&content);
    if filtered != content {
        *buffer = filtered.clone();
    }
    let content = filtered;

    if content.trim().is_empty() {
        return None;
    }

    if state.last_emitted_thinking.as_ref() == Some(&content) {
        return None;
    }
    state.last_emitted_thinking = Some(content.clone());

    Some(AgentEvent::Thinking {
        content,
        done: false,
        mission_id: Some(mission_id),
    })
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

    let event_type = match json.get("type").and_then(|v| v.as_str()).or(event_name) {
        Some(event_type) => event_type,
        None => return None,
    };
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
        "response.output_text.delta" => None,
        "response.completed" | "response.incomplete" => {
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

    Some(OpencodeSseParseResult {
        event,
        message_complete,
        session_id,
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

/// Health status of a mission.
#[derive(Debug, Clone, serde::Serialize)]
pub enum MissionHealth {
    /// Mission is progressing normally
    Healthy,
    /// Mission may be stalled
    Stalled {
        seconds_since_activity: u64,
        last_state: String,
    },
    /// Mission completed without deliverables
    MissingDeliverables { missing: Vec<String> },
    /// Mission ended unexpectedly
    UnexpectedEnd { reason: String },
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
    pub fn new(
        mission_id: Uuid,
        workspace_id: Uuid,
        agent_override: Option<String>,
        backend_id: Option<String>,
        session_id: Option<String>,
        config_profile: Option<String>,
    ) -> Self {
        Self {
            mission_id,
            workspace_id,
            backend_id: backend_id.unwrap_or_else(|| "opencode".to_string()),
            session_id,
            config_profile,
            state: MissionRunState::Queued,
            agent_override,
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

        // If running and no activity for 60+ seconds, consider stalled
        if self.is_running() && seconds_since > 60 {
            return MissionHealth::Stalled {
                seconds_since_activity: seconds_since,
                last_state: format!("{:?}", self.state),
            };
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

    /// Start executing the next queued message (if any and not already running).
    /// Returns true if execution was started.
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

                    // Add to history
                    self.history.push(("user".to_string(), result.1.clone()));
                    self.history
                        .push(("assistant".to_string(), result.2.output.clone()));

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
    pub fn check_finished(&self) -> bool {
        self.running_handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true)
    }
}

/// Build a history context string from conversation history.
fn build_history_context(history: &[(String, String)], max_chars: usize) -> String {
    let mut result = String::new();
    let mut total_chars = 0;
    for (role, content) in history.iter().rev() {
        let entry = format!("{}: {}\n\n", role.to_uppercase(), content);
        if total_chars + entry.len() > max_chars && !result.is_empty() {
            break;
        }
        result = format!("{}{}", entry, result);
        total_chars += entry.len();
    }
    result
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

/// Execute a single turn for a mission.
async fn run_mission_turn(
    config: Config,
    _root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    _status: Arc<RwLock<ControlStatus>>,
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
    secrets: Option<Arc<SecretsStore>>,
    session_id: Option<String>,
    mission_config_profile: Option<String>,
) -> AgentResult {
    let mut config = config;
    let effective_agent = agent_override.clone();
    if let Some(ref agent) = effective_agent {
        config.opencode_agent = Some(agent.clone());
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
    convo.push_str("\n");

    // Ensure mission workspace exists and is configured for OpenCode.
    let workspace = workspace::resolve_workspace(&workspaces, &config, workspace_id).await;
    let workspace_root = workspace.path.clone();
    let mission_work_dir = match {
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
    } {
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

    // Execute based on backend
    // For Claude Code, check if this is a continuation turn (has prior assistant response).
    // Note: history may include the current user message before the turn runs,
    // so we check for assistant messages to determine if this is truly a continuation.
    let is_continuation = history.iter().any(|(role, _)| role == "assistant");
    let result = match backend_id.as_str() {
        "claudecode" => {
            run_claudecode_turn(
                &workspace,
                &mission_work_dir,
                &user_message,
                config.default_model.as_deref(),
                effective_agent.as_deref(),
                mission_id,
                events_tx.clone(),
                cancel,
                secrets,
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                Some(Arc::clone(&tool_hub)),
            )
            .await
        }
        "opencode" => {
            // Use per-workspace CLI execution for all workspace types to ensure
            // native bash + correct filesystem scope.
            run_opencode_turn(
                &workspace,
                &mission_work_dir,
                &convo,
                config.default_model.as_deref(),
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
            run_amp_turn(
                &workspace,
                &mission_work_dir,
                &user_message,
                effective_agent.as_deref(), // Used as mode (smart/rush)
                mission_id,
                events_tx.clone(),
                cancel,
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                api_key.as_deref(),
            )
            .await
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

/// Read CLI path from backend config file if available.
fn get_claudecode_cli_path_from_config(_app_working_dir: &std::path::Path) -> Option<String> {
    let configs = read_backend_configs()?;

    for config in configs {
        if config.get("id")?.as_str()? == "claudecode" {
            if let Some(settings) = config.get("settings") {
                if let Some(cli_path) = settings.get("cli_path").and_then(|v| v.as_str()) {
                    if !cli_path.is_empty() {
                        tracing::info!(
                            "Using Claude Code CLI path from backend config: {}",
                            cli_path
                        );
                        return Some(cli_path.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Read API key from Amp backend config file if available.
pub fn get_amp_api_key_from_config() -> Option<String> {
    let configs = read_backend_configs()?;

    for config in configs {
        if config.get("id")?.as_str()? == "amp" {
            if let Some(settings) = config.get("settings") {
                if let Some(api_key) = settings.get("api_key").and_then(|v| v.as_str()) {
                    if !api_key.is_empty()
                        && !api_key.starts_with("[REDACTED")
                        && api_key != "********"
                    {
                        tracing::debug!("Using Amp API key from backend config");
                        return Some(api_key.to_string());
                    }
                }
            }
        }
    }
    None
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
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentResult> + Send + 'a>> {
    Box::pin(async move {
        use super::ai_providers::{
            ensure_anthropic_oauth_token_valid, get_anthropic_auth_for_claudecode,
            get_anthropic_auth_from_host_with_expiry, get_anthropic_auth_from_workspace,
            get_workspace_auth_path, refresh_workspace_anthropic_auth,
            write_claudecode_credentials_for_workspace, ClaudeCodeAuth,
        };
        use std::collections::HashMap;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        fn classify_claudecode_secret(value: String) -> ClaudeCodeAuth {
            if value.starts_with("sk-ant-oat") {
                ClaudeCodeAuth::OAuthToken(value)
            } else {
                ClaudeCodeAuth::ApiKey(value)
            }
        }

        // Ensure OAuth tokens are fresh before resolving credentials.
        let oauth_refresh_result = ensure_anthropic_oauth_token_valid().await;
        if let Err(e) = &oauth_refresh_result {
            tracing::warn!("Failed to refresh Anthropic OAuth token: {}", e);
        }

        // Try to get API key/OAuth token from Anthropic provider configured for Claude Code backend.
        // For container workspaces, compare workspace auth vs host auth and use the fresher one.
        // If workspace auth is expired, try to refresh it using the refresh token.
        let api_auth = {
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

        // Fail fast if no auth is available
        if api_auth.is_none() {
            let err_msg = "No Anthropic credentials detected; please authenticate in Settings → AI Providers or set CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY.";
            tracing::warn!(mission_id = %mission_id, "{}", err_msg);
            return AgentResult::failure(err_msg.to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }

        // Write Claude Code credentials file with refresh token for long-running missions.
        // This allows Claude Code to refresh tokens automatically during execution.
        let is_oauth = matches!(api_auth, Some(ClaudeCodeAuth::OAuthToken(_)));
        let mut wrote_claude_credentials = false;
        tracing::debug!(
            mission_id = %mission_id,
            is_oauth = is_oauth,
            workspace_path = %workspace.path.display(),
            workspace_type = ?workspace.workspace_type,
            "Checking if should write Claude Code credentials"
        );
        if is_oauth {
            match write_claudecode_credentials_for_workspace(&workspace) {
                Ok(()) => {
                    wrote_claude_credentials = true;
                    tracing::info!(
                        mission_id = %mission_id,
                        workspace_type = ?workspace.workspace_type,
                        "Wrote Claude Code credentials with refresh token for automatic token refresh"
                    );
                }
                Err(e) => {
                    // Non-fatal: we still have the access token in env var as fallback
                    tracing::warn!(
                        mission_id = %mission_id,
                        error = %e,
                        "Failed to write Claude Code credentials file (token refresh during mission may fail)"
                    );
                }
            }
        }

        // Determine CLI path: prefer backend config, then env var, then default
        let cli_path = get_claudecode_cli_path_from_config(app_working_dir)
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

        // Ensure per-workspace MCP config is loaded (Claude CLI may not auto-load .claude in --print mode).
        // For container workspaces, we must translate the path to be relative to the container filesystem.
        let mcp_config_path = work_dir.join(".claude").join("settings.local.json");
        if mcp_config_path.exists() {
            args.push("--mcp-config".to_string());
            // Translate the path for container execution (host path -> container-relative path)
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
        // - If we have assistant messages in history (normal continuation)
        // - OR if the session was previously initiated for THIS mission (handles crash/cancel case)
        let use_resume = is_continuation || session_was_initiated;

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

        if let Some(a) = agent {
            args.push("--agent".to_string());
            args.push(a.to_string());
        }

        // Build environment variables
        let mut env: HashMap<String, String> = HashMap::new();
        // Allow --dangerously-skip-permissions when running as root inside containers.
        env.insert("IS_SANDBOX".to_string(), "1".to_string());
        if let Some(ref auth) = api_auth {
            match auth {
                ClaudeCodeAuth::OAuthToken(token) => {
                    env.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.clone());
                    if wrote_claude_credentials {
                        tracing::debug!(
                            "Claude credentials file also written for token refresh (token_len={})",
                            token.len()
                        );
                    } else {
                        tracing::debug!(
                            "Using OAuth token for Claude CLI authentication (token_len={})",
                            token.len()
                        );
                    }
                }
                ClaudeCodeAuth::ApiKey(key) => {
                    env.insert("ANTHROPIC_API_KEY".to_string(), key.clone());
                    tracing::debug!("Using API key for Claude CLI authentication");
                }
            }
        } else {
            tracing::warn!("No authentication available for Claude Code!");
        }

        // Handle case where cli_path might be a wrapper command like "bun /path/to/claude"
        let (program, mut full_args) = if cli_path.contains(' ') {
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

        // Use WorkspaceExec to spawn the CLI in the correct workspace context
        let mut child = match workspace_exec
            .spawn_streaming(work_dir, &program, &full_args, env)
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

        // Write message to stdin (use effective_message which may have been transformed from slash commands)
        if let Some(mut stdin) = child.stdin.take() {
            let msg = effective_message.clone();
            tokio::spawn(async move {
                if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                    tracing::error!("Failed to write to Claude stdin: {}", e);
                }
                // Close stdin to signal end of input
                drop(stdin);
            });
        }

        // Get stdout for reading events
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let err_msg = "Failed to capture Claude stdout";
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
        let mut stderr_handle = if let Some(stderr) = stderr {
            Some(tokio::spawn(async move {
                let stderr_reader = BufReader::new(stderr);
                let mut stderr_lines = stderr_reader.lines();
                while let Ok(Some(line)) = stderr_lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        tracing::debug!(mission_id = %mission_id_for_stderr, stderr = %trimmed, "Claude Code stderr");
                        let mut captured = stderr_capture_clone.lock().await;
                        if !captured.is_empty() {
                            captured.push('\n');
                        }
                        captured.push_str(trimmed);
                    }
                }
            }))
        } else {
            None
        };

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
        let mut last_thinking_len: usize = 0; // Track last emitted length to avoid re-sending same content

        let auth_missing = api_auth.is_none();
        let auth_timeout = std::time::Duration::from_secs(45);

        // Create a buffered reader for stdout
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        // Process events until completion or cancellation
        loop {
            let mut timeout = tokio::time::sleep(auth_timeout);
            tokio::pin!(timeout);
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!(mission_id = %mission_id, "Claude Code execution cancelled, killing process");
                    // Kill the process to stop consuming API resources
                    let _ = child.kill().await;
                    if let Some(handle) = stderr_handle.take() {
                        handle.abort();
                    }
                    return AgentResult::failure("Cancelled".to_string(), 0)
                        .with_terminal_reason(TerminalReason::Cancelled);
                }
                _ = &mut timeout, if auth_missing => {
                    let err_msg = "Claude Code produced no output. No Anthropic credentials detected; please authenticate in Settings → AI Providers or set CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY.";
                    tracing::warn!(mission_id = %mission_id, "{}", err_msg);
                    let _ = child.kill().await;
                    if let Some(handle) = stderr_handle.take() {
                        handle.abort();
                    }
                    return AgentResult::failure(err_msg.to_string(), 0)
                        .with_terminal_reason(TerminalReason::LlmError);
                }
                line_result = lines.next_line() => {
                    match line_result {
                        Ok(Some(line)) => {
                            if line.is_empty() {
                                continue;
                            }

                            let claude_event: ClaudeEvent = match serde_json::from_str(&line) {
                                Ok(event) => event,
                                Err(e) => {
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        "Failed to parse Claude event: {} - line: {}",
                                        e,
                                        if line.len() > 200 {
                                            let end = safe_truncate_index(&line, 200);
                                            format!("{}...", &line[..end])
                                        } else {
                                            line.clone()
                                        }
                                    );
                                    continue;
                                }
                            };

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
                                            // Check the delta type to determine where to route content
                                            // "thinking_delta" -> thinking panel (uses delta.thinking field)
                                            // "text_delta" -> text output (uses delta.text field)
                                            if delta.delta_type == "thinking_delta" {
                                                // For thinking deltas, content is in the `thinking` field, not `text`
                                                if let Some(thinking_text) = delta.thinking {
                                                    if !thinking_text.is_empty() {
                                                        // Accumulate thinking content
                                                        let buffer = thinking_buffer.entry(index).or_default();
                                                        buffer.push_str(&thinking_text);

                                                        // Send accumulated thinking content (cumulative, like OpenCode)
                                                        // Only send if we have new content since last emit
                                                        let total_len = thinking_buffer.values().map(|s| s.len()).sum::<usize>();
                                                        if total_len > last_thinking_len {
                                                            // Combine all thinking buffers for the cumulative content
                                                            let accumulated: String = thinking_buffer.values().cloned().collect::<Vec<_>>().join("");
                                                            last_thinking_len = total_len;

                                                            let _ = events_tx.send(AgentEvent::Thinking {
                                                                content: accumulated,
                                                                done: false,
                                                                mission_id: Some(mission_id),
                                                            });
                                                        }
                                                    }
                                                }
                                            } else if delta.delta_type == "text_delta" {
                                                // For text deltas, content is in the `text` field
                                                if let Some(text) = delta.text {
                                                    if !text.is_empty() {
                                                        // Accumulate text content (will be used for final response)
                                                        let buffer = text_buffer.entry(index).or_default();
                                                        buffer.push_str(&text);
                                                        // Don't send text deltas as thinking events
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
                                    for block in evt.message.content {
                                        match block {
                                            ContentBlock::Text { text } => {
                                                // Text content is the final assistant response
                                                // Don't send as Thinking - it will be in the final AssistantMessage
                                                if !text.is_empty() {
                                                    final_result = text;
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

                                                if name == "question" || name.starts_with("ui_") {
                                                    if let Some(ref hub) = tool_hub {
                                                        tracing::info!(
                                                            mission_id = %mission_id,
                                                            tool_call_id = %id,
                                                            tool_name = %name,
                                                            "Frontend tool detected, pausing for user input"
                                                        );
                                                        let hub = Arc::clone(hub);
                                                        let rx = hub.register(id.clone()).await;

                                                        let _ = child.kill().await;
                                                        if let Some(handle) = stderr_handle.take() {
                                                            handle.abort();
                                                        }

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
                                                        ).await;
                                                    }
                                                }
                                            }
                                            ContentBlock::Thinking { thinking } => {
                                                // Only send if this is new content not already streamed
                                                // The streaming deltas already accumulated this, so this is
                                                // typically the final complete thinking block
                                                if !thinking.is_empty() {
                                                    let _ = events_tx.send(AgentEvent::Thinking {
                                                        content: thinking,
                                                        done: true, // Mark as done since this is the final block
                                                        mission_id: Some(mission_id),
                                                    });
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                ClaudeEvent::User(evt) => {
                                    for block in evt.message.content {
                                        if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                                            let name = pending_tools
                                                .get(&tool_use_id)
                                                .cloned()
                                                .unwrap_or_else(|| "unknown".to_string());

                                            // Convert content to string representation (handles both text and image results)
                                            let content_str = content.to_string_lossy();

                                            let result_value = if let Some(ref extra) = evt.tool_use_result {
                                                serde_json::json!({
                                                    "content": content_str,
                                                    "stdout": extra.stdout,
                                                    "stderr": extra.stderr,
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
                                    // Check for errors: explicit error flags OR result text that looks like an API error
                                    let result_text = res.result.clone().unwrap_or_default();
                                    let looks_like_api_error = result_text.starts_with("API Error:")
                                        || result_text.contains("\"type\":\"error\"")
                                        || result_text.contains("\"type\":\"overloaded_error\"")
                                        || result_text.contains("\"type\":\"api_error\"");

                                    if res.is_error || res.subtype == "error" || looks_like_api_error {
                                        had_error = true;
                                        let err_msg = if result_text.is_empty() { "Unknown error".to_string() } else { result_text };
                                        // Don't send an Error event here - let the failure propagate
                                        // through the AgentResult. control.rs will emit an AssistantMessage
                                        // with success=false which the UI displays as a failure message.
                                        // Sending Error here would cause duplicate messages.
                                        final_result = err_msg;
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
                        Ok(None) => {
                            // EOF - process finished
                            break;
                        }
                        Err(e) => {
                            tracing::error!("Error reading from Claude CLI: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        // Wait for child process to finish and clean up
        let exit_status = child.wait().await;

        // Wait for stderr capture to complete
        if let Some(handle) = stderr_handle {
            let _ = handle.await;
        }

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
            // Include stderr in error message if available
            let stderr_content = stderr_capture.lock().await;
            if !stderr_content.is_empty() {
                tracing::warn!(
                    mission_id = %mission_id,
                    stderr = %stderr_content,
                    exit_status = ?exit_status,
                    "Claude Code produced no output but had stderr"
                );
                final_result = format!(
                    "Claude Code error: {}",
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
                    "Claude Code produced no output and no stderr"
                );
                final_result =
                    "Claude Code produced no output. Check CLI installation or authentication."
                        .to_string();
            }
        }

        if had_error {
            AgentResult::failure(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::LlmError)
        } else {
            AgentResult::success(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::Completed)
        }
    }) // end Box::pin(async move { ... })
}

/// Read CLI path for opencode from backend config file if available.
fn get_opencode_cli_path_from_config(_app_working_dir: &std::path::Path) -> Option<String> {
    let configs = read_backend_configs()?;

    for config in configs {
        if config.get("id")?.as_str()? == "opencode" {
            if let Some(settings) = config.get("settings") {
                if let Some(cli_path) = settings.get("cli_path").and_then(|v| v.as_str()) {
                    if !cli_path.is_empty() {
                        tracing::info!("Using OpenCode CLI path from backend config: {}", cli_path);
                        return Some(cli_path.to_string());
                    }
                }
            }
        }
    }
    None
}

fn get_opencode_permissive_from_config(_app_working_dir: &std::path::Path) -> Option<bool> {
    let configs = read_backend_configs()?;

    for config in configs {
        if config.get("id")?.as_str()? == "opencode" {
            if let Some(settings) = config.get("settings") {
                if let Some(permissive) = settings.get("permissive").and_then(|v| v.as_bool()) {
                    tracing::info!(
                        "Using OpenCode permissive setting from backend config: {}",
                        permissive
                    );
                    return Some(permissive);
                }
            }
        }
    }
    None
}

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

fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ANSI escape sequences like "\x1b[31m"
            if let Some('[') = chars.peek() {
                let _ = chars.next();
                while let Some(c) = chars.next() {
                    if c == 'm' {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn parse_opencode_session_token(value: &str) -> Option<String> {
    let mut token = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            token.push(ch);
        } else {
            break;
        }
    }
    if token.starts_with("ses_") {
        return Some(token);
    }
    if token.len() < 8 {
        None
    } else {
        Some(token)
    }
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
        std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
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
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        for key in ["session id:", "session:", "session_id:", "session="] {
            if let Some(idx) = lower.find(key) {
                let rest = trimmed[idx + key.len()..].trim();
                if let Some(token) = parse_opencode_session_token(rest) {
                    return Some(token);
                }
            }
        }
    }
    None
}

fn opencode_output_needs_fallback(output: &str) -> bool {
    let cleaned = strip_ansi_codes(output);
    let mut lines: Vec<String> = cleaned
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    if lines.is_empty() {
        return true;
    }

    for line in lines.drain(..) {
        let lower = line.to_lowercase();
        let is_banner = lower.contains("starting opencode server")
            || lower.contains("opencode server started")
            || lower.contains("sending prompt")
            || lower.contains("waiting for completion")
            || lower.contains("all tasks completed")
            || lower.contains("completed")
            || lower.contains("session id:")
            || lower.contains("session:");
        if !is_banner {
            return false;
        }
    }

    true
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

fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    while let Some(n) = chars.next() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                    continue;
                }
                _ => {}
            }
        }

        out.push(c);
    }

    out
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
    has_other: bool,
}

fn auth_entry_has_credentials(value: &serde_json::Value) -> bool {
    value.get("key").is_some()
        || value.get("api_key").is_some()
        || value.get("apiKey").is_some()
        || value.get("refresh").is_some()
        || value.get("refresh_token").is_some()
        || value.get("access").is_some()
        || value.get("access_token").is_some()
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
    let mut has_other = false;

    if let Some(path) = host_opencode_auth_path() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(map) = parsed.as_object() {
                    for (key, value) in map {
                        if !auth_entry_has_credentials(value) {
                            continue;
                        }
                        match key.as_str() {
                            "openai" | "codex" => has_openai = true,
                            "anthropic" | "claude" => has_anthropic = true,
                            "google" | "gemini" => has_google = true,
                            _ => has_other = true,
                        }
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
                match stem {
                    "openai" | "codex" => has_openai = true,
                    "anthropic" | "claude" => has_anthropic = true,
                    "google" | "gemini" => has_google = true,
                    "" => {}
                    _ => has_other = true,
                }
            }
        }
    }

    if let Ok(value) = std::env::var("OPENAI_API_KEY") {
        if !value.trim().is_empty() {
            has_openai = true;
        }
    }
    if let Ok(value) = std::env::var("ANTHROPIC_API_KEY") {
        if !value.trim().is_empty() {
            has_anthropic = true;
        }
    }
    if let Ok(value) = std::env::var("GOOGLE_GENERATIVE_AI_API_KEY") {
        if !value.trim().is_empty() {
            has_google = true;
        }
    }
    if let Ok(value) = std::env::var("GOOGLE_API_KEY") {
        if !value.trim().is_empty() {
            has_google = true;
        }
    }
    if let Ok(value) = std::env::var("XAI_API_KEY") {
        if !value.trim().is_empty() {
            has_other = true;
        }
    }

    if let Some(app_dir) = app_working_dir {
        if let Some(auth) = build_opencode_auth_from_ai_providers(app_dir) {
            if let Some(map) = auth.as_object() {
                for (key, value) in map {
                    if !auth_entry_has_credentials(value) {
                        continue;
                    }
                    match key.as_str() {
                        "openai" | "codex" => has_openai = true,
                        "anthropic" | "claude" => has_anthropic = true,
                        "google" | "gemini" => has_google = true,
                        _ => has_other = true,
                    }
                }
            }
        }
    }

    OpenCodeAuthState {
        has_openai,
        has_anthropic,
        has_google,
        has_other,
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

    if !should_regen {
        if try_copy_host_oh_my_opencode_config(opencode_config_dir_host) {
            return;
        }
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

fn ensure_opencode_plugin_specs(opencode_config_dir: &std::path::Path, plugin_specs: &[&str]) {
    if plugin_specs.is_empty() {
        return;
    }

    let opencode_path = opencode_config_dir.join("opencode.json");
    let mut root = if opencode_path.exists() {
        match std::fs::read_to_string(&opencode_path)
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        {
            Some(value) => value,
            None => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

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
        if let Err(err) = std::fs::write(
            &opencode_path,
            serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string()),
        ) {
            tracing::warn!(
                "Failed to update OpenCode plugin config at {}: {}",
                opencode_path.display(),
                err
            );
        }
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

    let opencode_path = opencode_config_dir.join("opencode.json");
    let mut root = if opencode_path.exists() {
        match std::fs::read_to_string(&opencode_path)
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        {
            Some(value) => value,
            None => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

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
        if let Err(err) = std::fs::write(
            &opencode_path,
            serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string()),
        ) {
            tracing::warn!(
                "Failed to update OpenCode Google projectId at {}: {}",
                opencode_path.display(),
                err
            );
        }
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

    let opencode_path = opencode_config_dir.join("opencode.json");
    let mut opencode_json = if opencode_path.exists() {
        match std::fs::read_to_string(&opencode_path)
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
        {
            Some(value) => value,
            None => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

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
        if let Err(err) = std::fs::write(
            &opencode_path,
            serde_json::to_string_pretty(&opencode_json).unwrap_or_else(|_| "{}".to_string()),
        ) {
            tracing::warn!(
                "Failed to update opencode.json agent config at {}: {}",
                opencode_path.display(),
                err
            );
        }
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

    let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        format!("{}/.local/share", home)
    });
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

    let providers = ["openai", "anthropic", "google", "xai"];
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
    let opencode_path = opencode_config_dir.join("opencode.json");
    let opencode_value = std::fs::read_to_string(opencode_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok());

    if let Some(value) = opencode_value.as_ref() {
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

fn env_var_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        ),
        Err(_) => default,
    }
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

/// Returns the path to the Claude Code CLI that should be used.
/// If the CLI is not available, it will be auto-installed via bun or npm.
async fn ensure_claudecode_cli_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> Result<String, String> {
    // Check if claude is available at the specified path
    if command_available(workspace_exec, cwd, cli_path).await {
        return Ok(cli_path.to_string());
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

    let mut args = Vec::new();
    args.push("-lc".to_string());
    args.push(install_cmd.to_string());
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

/// Execute a turn using OpenCode CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
///
/// This uses the `oh-my-opencode run` CLI which creates an embedded OpenCode server,
/// enabling per-workspace isolation without network issues.
pub async fn run_opencode_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    // Determine CLI runner: prefer backend config, then env var, then try bunx/npx
    // We use 'bunx oh-my-opencode run' or 'npx oh-my-opencode run' for per-workspace execution.
    let workspace_exec = WorkspaceExec::new(workspace.clone());
    if let Err(err) = ensure_opencode_cli_available(&workspace_exec, work_dir).await {
        tracing::error!("{}", err);
        return AgentResult::failure(err, 0).with_terminal_reason(TerminalReason::LlmError);
    }

    let opencode_config_dir_host = work_dir.join(".opencode");

    let mut resolved_model = model
        .map(|m| m.to_string())
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
    let default_model_override = resolved_model.clone();

    let auth_state = detect_opencode_provider_auth(Some(app_working_dir));
    let has_openai = auth_state.has_openai;
    let has_anthropic = auth_state.has_anthropic;
    let has_google = auth_state.has_google;
    let has_any_provider = has_openai || has_anthropic || has_google || auth_state.has_other;

    if resolved_model.is_none() {
        resolved_model = resolve_opencode_model_from_config(&opencode_config_dir_host, agent);
    }

    let mut provider_hint = resolved_model
        .as_deref()
        .and_then(|m| m.split_once('/'))
        .map(|(provider, _)| provider.to_lowercase());

    let provider_available = |provider: &str| -> bool {
        match provider {
            "anthropic" | "claude" => has_anthropic,
            "openai" | "codex" => has_openai,
            "google" | "gemini" => has_google,
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

    let configured_runner = get_opencode_cli_path_from_config(app_working_dir)
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
        has_google,
    )
    .await;
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
    if has_google {
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
    // Arguments: bunx oh-my-opencode run [--agent <agent>] [--directory <path>] [--timeout <ms>] <message>
    let mut args = if runner_is_direct {
        vec!["run".to_string()]
    } else {
        vec!["oh-my-opencode".to_string(), "run".to_string()]
    };

    if let Some(a) = agent {
        args.push("--agent".to_string());
        args.push(a.to_string());
    }

    args.push("--directory".to_string());
    args.push(work_dir_arg.clone());

    // Add timeout (0 = no timeout, let the agent complete)
    args.push("--timeout".to_string());
    args.push("0".to_string());

    // The message is passed as the final argument
    args.push(message.to_string());

    tracing::debug!(
        mission_id = %mission_id,
        runner_is_direct = runner_is_direct,
        cli_args = ?args,
        "OpenCode CLI args prepared"
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

    if let Some(permissive) = get_opencode_permissive_from_config(app_working_dir) {
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

    // Use WorkspaceExec to spawn the CLI in the correct workspace context
    let mut child = match workspace_exec
        .spawn_streaming(work_dir, &cli_runner, &args, env)
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
    let session_id_capture: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sse_emitted_thinking = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_done_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_cancel = CancellationToken::new();

    // oh-my-opencode doesn't support --format json, so use SSE curl for events.
    let use_json_stdout = false;
    let sse_handle =
        if !use_json_stdout && command_available(&workspace_exec, work_dir, "curl").await {
            let workspace_exec = workspace_exec.clone();
            let work_dir = work_dir.to_path_buf();
            let work_dir_arg = work_dir_arg.clone();
            let session_id_capture = session_id_capture.clone();
            let sse_emitted_thinking = sse_emitted_thinking.clone();
            let sse_done_sent = sse_done_sent.clone();
            let sse_cancel = sse_cancel.clone();
            let events_tx = events_tx.clone();
            let opencode_port = opencode_port.clone();
            let mission_id = mission_id;
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
                    if attempts > 5 {
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

                    let mut child = match child {
                        Ok(child) => child,
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            continue;
                        }
                    };

                    let stdout = match child.stdout.take() {
                        Some(stdout) => stdout,
                        None => {
                            let _ = child.kill().await;
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            continue;
                        }
                    };

                    let mut reader = BufReader::new(stdout);
                    let mut line = String::new();
                    let mut current_event: Option<String> = None;
                    let mut data_lines: Vec<String> = Vec::new();
                    let mut state = OpencodeSseState::default();
                    let mut saw_complete = false;

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
                                        let current_session =
                                            session_id_capture.lock().unwrap().clone();
                                        if let Some(parsed) = parse_opencode_sse_event(
                                            &data,
                                            current_event.as_deref(),
                                            current_session.as_deref(),
                                            &mut state,
                                            mission_id,
                                        ) {
                                            if let Some(session_id) = parsed.session_id {
                                                let mut guard = session_id_capture.lock().unwrap();
                                                if guard.is_none() {
                                                    *guard = Some(session_id);
                                                }
                                            }
                                            if let Some(event) = parsed.event {
                                                if matches!(event, AgentEvent::Thinking { .. }) {
                                                    sse_emitted_thinking.store(
                                                        true,
                                                        std::sync::atomic::Ordering::SeqCst,
                                                    );
                                                }
                                                let _ = events_tx.send(event);
                                            }
                                            if parsed.message_complete {
                                                saw_complete = true;
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
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
            }))
        } else {
            None
        };

    // Spawn a task to read stderr (just log in JSON mode, events come on stdout)
    let mission_id_clone = mission_id;
    let stderr_handle = if let Some(stderr) = stderr {
        Some(tokio::spawn(async move {
            let stderr_reader = BufReader::new(stderr);
            let mut stderr_lines = stderr_reader.lines();
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                let clean = line.trim().to_string();
                if !clean.is_empty() {
                    tracing::debug!(mission_id = %mission_id_clone, line = %clean, "OpenCode CLI stderr");
                }
            }
        }))
    } else {
        None
    };

    // Process stdout output from oh-my-opencode
    // Events come via SSE (when curl is available), stdout contains the assistant's text response.
    let stdout_reader = BufReader::new(stdout);
    let mut stdout_lines = stdout_reader.lines();
    let mut state = OpencodeSseState::default();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(mission_id = %mission_id, "OpenCode execution cancelled, killing process");
                let _ = child.kill().await;
                if let Some(handle) = stderr_handle {
                    handle.abort();
                }
                return AgentResult::failure("Cancelled".to_string(), 0)
                    .with_terminal_reason(TerminalReason::Cancelled);
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

                        // Try to parse as JSON event
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
                            let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            tracing::debug!(
                                mission_id = %mission_id,
                                event_type = %event_type,
                                "OpenCode JSON event"
                            );

                            // Extract text content from message.part.updated for final result
                            if event_type == "message.part.updated" {
                                if let Some(props) = json.get("properties") {
                                    if let Some(part) = props.get("part") {
                                        let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                        if part_type == "text" {
                                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                                final_result = text.to_string();
                                            }
                                        }
                                    }
                                }
                            }

                            // Handle completion and error events from oh-my-opencode
                            if event_type == "completion" {
                                tracing::info!(mission_id = %mission_id, "OpenCode JSON completion event");
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
                            let current_session = session_id_capture.lock().unwrap().clone();
                            if let Some(parsed) = parse_opencode_sse_event(
                                trimmed,
                                None,
                                current_session.as_deref(),
                                &mut state,
                                mission_id,
                            ) {
                                if let Some(session_id) = parsed.session_id {
                                    let mut guard = session_id_capture.lock().unwrap();
                                    if guard.is_none() {
                                        *guard = Some(session_id);
                                    }
                                }
                                if let Some(event) = parsed.event {
                                    if matches!(event, AgentEvent::Thinking { .. }) {
                                        sse_emitted_thinking.store(true, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    let _ = events_tx.send(event);
                                }
                                if parsed.message_complete {
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
                                }
                            }
                        } else {
                            // Non-JSON line - this is the expected output format without --format json
                            tracing::debug!(mission_id = %mission_id, line = %trimmed, "OpenCode stdout");
                            final_result.push_str(trimmed);
                            final_result.push('\n');
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

    // Wait for stderr task to complete
    if let Some(handle) = stderr_handle {
        let _ = handle.await;
    }

    // Wait for child process to finish and clean up
    let exit_status = child.wait().await;

    sse_cancel.cancel();
    if let Some(handle) = sse_handle {
        handle.abort();
    }

    // Check exit status
    if let Ok(status) = exit_status {
        if !status.success() {
            had_error = true;
            if final_result.is_empty() {
                final_result = format!("OpenCode CLI exited with status: {}", status);
            }
        }
    }

    let session_id = session_id_capture.lock().unwrap().clone();
    let session_id = session_id.or_else(|| extract_opencode_session_id(&final_result));
    let stored_message = session_id
        .as_deref()
        .and_then(|id| load_latest_opencode_assistant_message(workspace, id));

    if opencode_output_needs_fallback(&final_result) {
        if let Some(session_id) = session_id.as_deref() {
            if let Some(message) = stored_message.as_ref() {
                let text = extract_text(&message.parts);
                if !text.trim().is_empty() {
                    tracing::info!(
                        mission_id = %mission_id,
                        session_id = %session_id,
                        text_len = text.len(),
                        "Recovered OpenCode assistant output from storage"
                    );
                    final_result = text;
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

    if emitted_thinking {
        let _ = events_tx.send(AgentEvent::Thinking {
            content: String::new(),
            done: true,
            mission_id: Some(mission_id),
        });
    } else if sse_emitted && !sse_done_sent.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = events_tx.send(AgentEvent::Thinking {
            content: String::new(),
            done: true,
            mission_id: Some(mission_id),
        });
    }

    if final_result.trim().is_empty() && !had_error {
        had_error = true;
        final_result =
            "OpenCode produced no output. Check CLI installation or authentication.".to_string();
    }

    tracing::info!(
        mission_id = %mission_id,
        had_error = had_error,
        result_len = final_result.len(),
        "OpenCode CLI execution completed"
    );

    let mut result = if had_error {
        AgentResult::failure(final_result, 0).with_terminal_reason(TerminalReason::LlmError)
    } else {
        AgentResult::success(final_result, 0).with_terminal_reason(TerminalReason::Completed)
    };
    if let Some(model) = model_used {
        result = result.with_model(model);
    }
    result
}

/// Execute a turn using Amp CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
pub async fn run_amp_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    mode: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
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
    let stderr_handle = if let Some(stderr) = stderr {
        Some(tokio::spawn(async move {
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
        }))
    } else {
        None
    };

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
    let mut last_thinking_len: usize = 0;
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
                                        if delta.delta_type == "thinking_delta" {
                                            if let Some(thinking_text) = delta.thinking {
                                                if !thinking_text.is_empty() {
                                                    let buffer = thinking_buffer.entry(index).or_default();
                                                    buffer.push_str(&thinking_text);

                                                    let total_len = thinking_buffer.values().map(|s| s.len()).sum::<usize>();
                                                    if total_len > last_thinking_len {
                                                        let accumulated: String = thinking_buffer.values().cloned().collect::<Vec<_>>().join("");
                                                        last_thinking_len = total_len;
                                                        thinking_streamed = true;

                                                        let _ = events_tx.send(AgentEvent::Thinking {
                                                            content: accumulated,
                                                            done: false,
                                                            mission_id: Some(mission_id),
                                                        });
                                                    }
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

                                for block in evt.message.content {
                                    match block {
                                        ContentBlock::Text { text } => {
                                            if !text.is_empty() {
                                                final_result = text;
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
                                            // Only emit thinking from Assistant event if it wasn't already streamed
                                            // via ContentBlockDelta events. This prevents duplicate thinking content.
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
                            }
                            AmpEvent::User(evt) => {
                                for block in evt.message.content {
                                    if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
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
                                } else {
                                    if let Some(result) = res.result {
                                        final_result = result;
                                    }
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
        AgentResult::failure(final_result, cost_cents)
            .with_terminal_reason(TerminalReason::LlmError)
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
            seconds_since_activity: runner.last_activity.elapsed().as_secs(),
            expected_deliverables: runner.deliverables.deliverables.len(),
            current_activity: runner.current_activity.clone(),
            subtask_total: runner.subtasks.len(),
            subtask_completed: runner.subtasks.iter().filter(|s| s.completed).count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sync_opencode_agent_config;
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
}
