//! Native Grok Build CLI as a typed remote launch.
//!
//! A `backend: "grok"` launch runs the real `grok` CLI on the node (not an
//! OpenCode stand-in): `grok --output-format streaming-json --always-approve
//! [--model <id>] (--session-id <new> | --resume <existing>) -p <prompt>` under the node's raw job
//! executor, which clears the environment and sets `HOME` to the per-mission
//! job directory. Three things distinguish it from the Claude/OpenCode plans:
//!
//! - **Auth is node-managed.** Grok >= 1.0 authenticates through its own
//!   cached login (`$GROK_HOME/auth.json`), not a proxy key, so the job asks
//!   the node for the `grok` managed-auth profile by *name*
//!   (`JobPayload::RawCommand::managed_auth`). The node exports
//!   `GROK_HOME=<its configured trusted dir>`; no credential and no path is
//!   ever in the payload, the job database, the command line or the logs.
//!   The command still fails closed (exit 78) on a node that ignores the
//!   field, instead of hanging on the CLI's interactive sign-in.
//! - **Events stream.** The observer reads the job log incrementally
//!   (`GET /jobs/:id/log?offset=`), parses the CLI's streaming-json lines and
//!   broadcasts `Thinking`/`TextDelta`/`SessionIdUpdate` like the local
//!   runner. New session UUIDs are persisted under the run generation before
//!   dispatch, so interruption before the `end` event remains resumable.
//! - **Goals are native.** Grok 1.0.34 executes literal `/goal <objective>`
//!   through planning, implementation and verification. A paused native goal
//!   continues with `--resume <session> -p '/goal resume'`. The host never
//!   wraps prompts in sentinels or launches its own iteration loop.
//!
//! Resume/continuation of an existing remote Grok mission submits a new job
//! on the same node with `--resume`; other remote harnesses (and a node that
//! is no longer configured) are reported as needing a replacement mission.

use std::collections::VecDeque;
use std::sync::Arc;

use axum::http::StatusCode;
use uuid::Uuid;

use super::{
    positional_prompt, resolve_grok_default_model, shell_single_quote, AgentEvent, ControlState,
    MissionStatus, RemoteExecution, RemoteHarnessPlan, RemoteMissionOwner,
};
use crate::api::mission_store::{Mission, MissionStore, SessionUpdateRun};
use crate::api::routes::AppState;
use crate::api::runners::grok::{
    grok_event_is_error, grok_event_model, grok_event_reasoning, grok_event_session_id,
    grok_event_text, grok_line_requests_interactive_login,
};
use crate::remote_node::{
    self, JobLogChunk, NodeJobStatus, RemoteNodeClient, RemoteNodeConfig, RemoteNodeError,
};

/// Backend id of the Grok Build harness.
pub(crate) const GROK_BACKEND: &str = "grok";
/// Managed-auth profile the node must provide (see `crate::node::managed_auth`).
pub(crate) const MANAGED_AUTH_PROFILE: &str = crate::node::managed_auth::PROFILE_GROK;
/// Exit status of the job wrapper when the node did not inject `GROK_HOME`
/// (a node predating managed auth, or one whose profile went away between
/// the heartbeat and the job).
pub(crate) const MISSING_MANAGED_AUTH_EXIT: i32 = 78;
/// Stable `400` prefix: the selected node advertises no `grok` managed auth.
pub(crate) const REMOTE_AUTH_REQUIRED: &str = "REMOTE_AUTH_REQUIRED";
/// Stable `409` prefix: the mission cannot be continued on its node and a
/// replacement mission must be created instead.
pub(crate) const REMOTE_RESUME_REQUIRES_REPLACEMENT: &str = "REMOTE_RESUME_REQUIRES_REPLACEMENT";
/// Stable `409` prefix: the node job of this mission is still live.
pub(crate) const REMOTE_JOB_STILL_RUNNING: &str = "REMOTE_JOB_STILL_RUNNING";

/// Arguments verified with native Grok 1.0.34 on DGX.
pub(crate) const GROK_HEADLESS_ARGS: &[&str] = &[
    "--output-format",
    "streaming-json",
    "--always-approve",
    "--no-plan",
];

/// Source tag on user messages a remote resume injects.
const RESUME_SOURCE: &str = "remote_grok:resume";
/// Bound on the diagnostic (non-JSON) log lines kept for a failure report.
const DIAGNOSTIC_LINES: usize = 40;
/// Log chunks fetched per poll tick before yielding to the state poll.
const MAX_CHUNKS_PER_TICK: usize = 8;

// ─── Plan / execution ────────────────────────────────────────────────────────

/// Preserve native slash commands verbatim, including `/goal` and its budget.
pub(crate) fn plan(model: Option<String>, prompt: String) -> RemoteHarnessPlan {
    RemoteHarnessPlan::Grok {
        model: Some(model.unwrap_or_else(resolve_grok_default_model)),
        prompt,
        resume_session_id: None,
        new_session_id: Some(Uuid::new_v4().to_string()),
    }
}

/// Node job for a Grok plan. The command fails closed (exit 127 / 78) when
/// the CLI or the managed auth environment is missing, so an unsupported
/// node produces a fast, explicit failure instead of an interactive login
/// prompt that blocks until the job timeout.
pub(crate) fn execution(
    model: Option<&str>,
    prompt: &str,
    resume_session_id: Option<&str>,
    new_session_id: Option<&str>,
    label: String,
) -> RemoteExecution {
    let mut command = String::from(
        "command -v grok >/dev/null 2>&1 || { echo 'grok is not installed on this node' >&2; exit 127; }; \
         [ -n \"${GROK_HOME:-}\" ] && [ -r \"${GROK_HOME}/auth.json\" ] || { echo 'grok managed auth is not available in this job: the node must set SANDBOXED_NODE_GROK_HOME to a directory logged in with `grok login --device-auth` (see docs/REMOTE_NODES.md)' >&2; exit ",
    );
    command.push_str(&MISSING_MANAGED_AUTH_EXIT.to_string());
    command.push_str("; }; exec grok");
    for arg in GROK_HEADLESS_ARGS {
        command.push(' ');
        command.push_str(arg);
    }
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        command.push_str(" --model ");
        command.push_str(&shell_single_quote(model));
    }
    if let Some(session) = resume_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        command.push_str(" --resume ");
        command.push_str(&shell_single_quote(session));
    }
    if resume_session_id.is_none() {
        if let Some(session) = new_session_id {
            command.push_str(" --session-id ");
            command.push_str(&shell_single_quote(session));
        }
    }
    command.push_str(" --cwd \"$PWD\" -p ");
    command.push_str(&shell_single_quote(&positional_prompt(prompt)));
    RemoteExecution {
        command,
        env: Some(std::collections::HashMap::from([(
            "NO_COLOR".to_string(),
            "1".to_string(),
        )])),
        managed_auth: vec![MANAGED_AUTH_PROFILE.to_string()],
        label,
    }
}

/// Planning-time check that the selected node advertises the `grok`
/// managed-auth profile. A node whose last heartbeat lacks it is refused with
/// [`REMOTE_AUTH_REQUIRED`] before any mission exists; a node without any
/// cached heartbeat is probed once. Missing capability evidence fails closed.
pub(crate) async fn require_node_managed_auth(
    state: &AppState,
    node_id: &str,
    plan: &RemoteHarnessPlan,
) -> Result<(), String> {
    if !matches!(plan, RemoteHarnessPlan::Grok { .. }) {
        return Ok(());
    }
    let Some(node) = state.config.remote_nodes.node(node_id) else {
        return Ok(());
    };
    if state
        .fleet
        .get(node_id)
        .and_then(|cached| cached.last_heartbeat)
        .is_none()
    {
        let client = RemoteNodeClient::default();
        remote_node::probe_node(&state.fleet, &client, node).await;
    }
    let Some(heartbeat) = state
        .fleet
        .get(node_id)
        .and_then(|cached| cached.last_heartbeat)
    else {
        return Err(format!("{REMOTE_AUTH_REQUIRED}: remote node '{node_id}' has no verified managed-auth heartbeat; check node connectivity and upgrade/configure sandboxed-node"));
    };
    heartbeat_supports_grok(&heartbeat.managed_auth, node_id)
}

pub(crate) fn heartbeat_supports_grok(
    managed_auth: &[String],
    node_id: &str,
) -> Result<(), String> {
    if managed_auth
        .iter()
        .any(|profile| profile == MANAGED_AUTH_PROFILE)
    {
        Ok(())
    } else {
        Err(format!(
            "{REMOTE_AUTH_REQUIRED}: remote node '{node_id}' advertises no managed grok auth; \
             install the grok CLI there, set SANDBOXED_NODE_GROK_HOME on the node service and run \
             `GROK_HOME=<that dir> grok login --device-auth` as the node service account (advertised profiles: [{}])",
            managed_auth.join(", ")
        ))
    }
}

// ─── streaming-json parser ───────────────────────────────────────────────────

/// What one fed chunk changed, for the observer to broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamUpdate {
    Text,
    Thinking,
    TextSnapshot(String),
    ThinkingSnapshot(String),
    SessionId(String),
    End,
    AuthRequired,
    Error(String),
    Tool {
        update: serde_json::Value,
        completed: bool,
    },
}

/// Incremental parser over the CLI's `--output-format streaming-json` lines
/// (`{"type":"text","data":…}`, `{"type":"thought","data":…}`,
/// `{"type":"end","stopReason":…,"sessionId":…}`), tolerant of the older
/// shapes the local runner accepts, plus the interactive-login sniffer for
/// non-JSON (stderr) lines.
#[derive(Debug, Default)]
pub(crate) struct GrokStream {
    partial: String,
    dropping_line: bool,
    text_segment: String,
    thinking_active: bool,
    pub(crate) text: String,
    pub(crate) thinking: String,
    pub(crate) session_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) stop_reason: Option<String>,
    pub(crate) ended: bool,
    pub(crate) auth_required: bool,
    pub(crate) error: Option<String>,
    pub(crate) json_events: u64,
    progress: bool,
    pub(crate) diagnostics: VecDeque<String>,
}

impl GrokStream {
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<StreamUpdate> {
        let mut updates = Vec::new();
        for part in chunk.split_inclusive('\n') {
            let complete = part.ends_with('\n');
            if !self.dropping_line && self.partial.len() + part.len() <= 1024 * 1024 {
                self.partial.push_str(part);
            } else {
                self.partial.clear();
                self.dropping_line = true;
            }
            if complete {
                if !self.dropping_line {
                    let line = std::mem::take(&mut self.partial);
                    self.feed_line(line.trim_end_matches('\n'), &mut updates);
                }
                self.dropping_line = false;
            }
        }
        self.snapshot_pending(&mut updates);
        updates
    }

    /// Flush a trailing partial line at end of log.
    pub(crate) fn finish(&mut self) -> Vec<StreamUpdate> {
        let mut updates = Vec::new();
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.feed_line(&line, &mut updates);
        }
        self.snapshot_pending(&mut updates);
        updates
    }

    // Materialize once per contiguous output segment, never once per delta.
    // Boundaries preserve the exact position of tool events within each chunk.
    fn snapshot_pending(&self, updates: &mut [StreamUpdate]) {
        if let Some(last) = updates.last_mut() {
            match last {
                StreamUpdate::Text => *last = StreamUpdate::TextSnapshot(self.text.clone()),
                StreamUpdate::Thinking => {
                    *last = StreamUpdate::ThinkingSnapshot(self.thinking.clone())
                }
                _ => {}
            }
        }
    }

    fn feed_line(&mut self, raw: &str, updates: &mut Vec<StreamUpdate>) {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            if grok_line_requests_interactive_login(line) && !self.auth_required {
                self.snapshot_pending(updates);
                self.auth_required = true;
                updates.push(StreamUpdate::AuthRequired);
            }
            if self.diagnostics.len() >= DIAGNOSTIC_LINES {
                self.diagnostics.pop_front();
            }
            let mut diagnostic = line.to_string();
            if diagnostic.len() > 4096 {
                let mut cut = 4096;
                while !diagnostic.is_char_boundary(cut) {
                    cut -= 1;
                }
                diagnostic.truncate(cut);
            }
            self.diagnostics.push_back(diagnostic);
            return;
        };
        // Codex exec emits native thread/turn/item events. Keep the thread id
        // for continuation on the same node and preserve tool/text ordering.
        let kind = value["type"].as_str().unwrap_or_default();
        if matches!(
            kind,
            "thread.started"
                | "turn.started"
                | "turn.completed"
                | "turn.failed"
                | "item.started"
                | "item.updated"
                | "item.completed"
        ) {
            self.snapshot_pending(updates);
            self.json_events += 1;
            if kind == "thread.started" {
                if let Some(session) = value["thread_id"].as_str() {
                    self.session_id = Some(session.to_string());
                    updates.push(StreamUpdate::SessionId(session.to_string()));
                }
            } else if kind == "turn.completed" {
                // Codex can emit retry diagnostics before a successful turn.
                self.error = None;
                self.ended = true;
                self.stop_reason = Some("end_turn".to_string());
                updates.push(StreamUpdate::End);
            } else if kind == "turn.failed" {
                let error = value["error"]["message"]
                    .as_str()
                    .unwrap_or("Codex turn failed")
                    .to_string();
                self.error = Some(error.clone());
                updates.push(StreamUpdate::Error(error));
            } else if kind.starts_with("item.") {
                self.progress = true;
                let item = &value["item"];
                match item["type"].as_str() {
                    Some("agent_message") if kind == "item.completed" => {
                        if let Some(text) = item["text"].as_str() {
                            if !self.text.is_empty() {
                                self.text.push_str("\n\n");
                            }
                            self.text.push_str(text);
                            updates.push(StreamUpdate::TextSnapshot(self.text.clone()));
                        }
                    }
                    Some("reasoning") if kind == "item.completed" => {
                        if let Some(text) = item["text"].as_str() {
                            self.thinking = text.to_string();
                            updates.push(StreamUpdate::ThinkingSnapshot(self.thinking.clone()));
                        }
                    }
                    Some("command_execution" | "file_change" | "mcp_tool_call" | "web_search")
                        if kind != "item.updated" =>
                    {
                        updates.push(StreamUpdate::Tool {
                                update: serde_json::json!({
                                    "toolCallId": item["id"], "name": item["type"],
                                    "rawInput": item, "output": item["aggregated_output"],
                                    "status": if item["status"] == "failed" { "failed" } else { "completed" },
                                }),
                                completed: kind == "item.completed",
                            });
                    }
                    _ => {}
                }
            }
            return;
        }
        // OpenCode emits full text/tool parts and a native sessionID on every event.
        if let Some(session) = value.get("sessionID").and_then(|v| v.as_str()) {
            self.snapshot_pending(updates);
            self.json_events += 1;
            self.progress = true;
            if self.session_id.as_deref() != Some(session) {
                self.session_id = Some(session.to_string());
                updates.push(StreamUpdate::SessionId(session.to_string()));
            }
            let part = &value["part"];
            match value["type"].as_str() {
                Some("text") => {
                    if let Some(text) = part["text"].as_str() {
                        if !self.text.is_empty() {
                            self.text.push_str("\n\n");
                        }
                        self.text.push_str(text);
                        updates.push(StreamUpdate::TextSnapshot(self.text.clone()));
                    }
                }
                Some("tool_use") => {
                    let update = serde_json::json!({
                        "toolCallId": part["callID"], "name": part["tool"],
                        "rawInput": part["state"]["input"], "status": part["state"]["status"],
                        "output": part["state"]["output"],
                    });
                    updates.push(StreamUpdate::Tool {
                        update: update.clone(),
                        completed: false,
                    });
                    updates.push(StreamUpdate::Tool {
                        update,
                        completed: true,
                    });
                }
                Some("error") => {
                    self.error = Some(value["error"].to_string());
                }
                _ => {}
            }
            return;
        }
        let is_text = grok_event_text(&value).is_some() && grok_event_reasoning(&value).is_none();
        let is_thinking = grok_event_reasoning(&value).is_some();
        if !matches!(updates.last(), Some(StreamUpdate::Text) if is_text)
            && !matches!(updates.last(), Some(StreamUpdate::Thinking) if is_thinking)
        {
            self.snapshot_pending(updates);
        }
        self.json_events += 1;
        if let Some(model) = grok_event_model(&value) {
            self.model = Some(model);
        }
        if let Some(session) = grok_event_session_id(&value) {
            if self.session_id.as_deref() != Some(session.as_str()) {
                self.snapshot_pending(updates);
                self.session_id = Some(session.clone());
                updates.push(StreamUpdate::SessionId(session));
            }
        }
        let kind = value
            .get("type")
            .and_then(|t| t.as_str())
            .map(|t| t.to_ascii_lowercase());
        if matches!(kind.as_deref(), Some("tool_call" | "tool_call_update")) {
            self.snapshot_pending(updates);
            // Text after a tool belongs to a new assistant segment. Native
            // goals emit several such segments before their final snapshot.
            // Keep accumulated output, but compare snapshots only to the
            // current segment (available_commands does not end a segment).
            self.text_segment.clear();
            self.thinking_active = false;
            self.progress = true;
            let update = value
                .get("data")
                .filter(|data| data.is_object())
                .unwrap_or(&value)
                .clone();
            let completed = kind.as_deref() == Some("tool_call_update");
            updates.push(StreamUpdate::Tool { update, completed });
            return;
        }
        if grok_event_is_error(&value) {
            self.snapshot_pending(updates);
            let message = value
                .get("error")
                .map(|e| match e.as_str() {
                    Some(s) => s.to_string(),
                    None => e.to_string(),
                })
                .or_else(|| grok_event_text(&value))
                .unwrap_or_else(|| line.to_string());
            self.error = Some(message.clone());
            updates.push(StreamUpdate::Error(message));
            return;
        }
        if kind.as_deref() == Some("end") {
            self.snapshot_pending(updates);
            self.ended = true;
            self.progress = true;
            self.stop_reason = value
                .get("stopReason")
                .or_else(|| value.get("stop_reason"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            updates.push(StreamUpdate::End);
            return;
        }
        if let Some(reasoning) = grok_event_reasoning(&value) {
            if !reasoning.is_empty() {
                self.progress = true;
                if !self.thinking_active {
                    self.thinking.clear();
                    self.thinking_active = true;
                }
                append_bounded(&mut self.thinking, &reasoning);
                if !matches!(updates.last(), Some(StreamUpdate::Thinking)) {
                    updates.push(StreamUpdate::Thinking);
                }
            }
            return;
        }
        if let Some(text) = grok_event_text(&value) {
            if !text.is_empty() {
                self.thinking_active = false;
                self.progress = true;
                // Native CLI emits deltas followed by the same full text snapshot.
                if text == self.text_segment && !self.text_segment.is_empty() {
                    self.text_segment.clear();
                } else if !self.text_segment.is_empty() && text.starts_with(&self.text_segment) {
                    let suffix = &text[self.text_segment.len()..];
                    append_bounded(&mut self.text, suffix);
                    self.text_segment.clear();
                    if !matches!(updates.last(), Some(StreamUpdate::Text)) {
                        updates.push(StreamUpdate::Text);
                    }
                } else {
                    append_bounded(&mut self.text, &text);
                    append_bounded(&mut self.text_segment, &text);
                    if !matches!(updates.last(), Some(StreamUpdate::Text)) {
                        updates.push(StreamUpdate::Text);
                    }
                }
            }
        }
    }

    fn diagnostics_text(&self) -> String {
        self.diagnostics
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn append_bounded(target: &mut String, text: &str) {
    target.push_str(text);
    const LIMIT: usize = 1024 * 1024;
    if target.len() > LIMIT {
        let mut cut = target.len() - LIMIT;
        while !target.is_char_boundary(cut) {
            cut += 1;
        }
        target.drain(..cut);
    }
}

fn goal_objective(mission: &Mission) -> Option<String> {
    if !mission.goal_mode {
        return None;
    }
    mission
        .goal_objective
        .as_deref()
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .map(str::to_string)
}

// ─── Observer (attached to the poll loop) ────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogStreaming {
    Unknown,
    Supported,
    /// The node answered 404: it predates `GET /jobs/:id/log`. Only the
    /// terminal `log_tail` is available.
    Unsupported,
}

/// Terminal decision the poll loop applies for a native Grok job.
#[derive(Debug)]
pub(crate) struct TerminalVerdict {
    pub(crate) success: bool,
    pub(crate) content: String,
    /// Reason recorded on the mission/lease when the mission is finalized.
    pub(crate) status_reason: &'static str,
}

pub(crate) struct NativeGrokObserver {
    mission_id: Uuid,
    job_id: Uuid,
    owner: RemoteMissionOwner,
    mission: Mission,
    log_offset: u64,
    log_len: u64,
    streaming: LogStreaming,
    stream: GrokStream,
    thinking_open: bool,
    thinking_snapshot: String,
    session_persisted: Option<String>,
    auth_cancel_requested: bool,
    running_since: Option<std::time::Instant>,
}

impl NativeGrokObserver {
    /// Attach to a native Grok job without a host goal driver.
    pub(crate) async fn attach(
        owner: &RemoteMissionOwner,
        _node_id: &str,
        mission_id: Uuid,
        job_id: Uuid,
    ) -> Option<Self> {
        let mission = match owner.mission_store.get_mission(mission_id).await {
            Ok(Some(mission))
                if matches!(
                    mission.backend.as_str(),
                    GROK_BACKEND | "opencode" | "codex"
                ) =>
            {
                mission
            }
            _ => return None,
        };
        Some(Self {
            mission_id,
            job_id,
            owner: owner.clone(),
            session_persisted: mission.session_id.clone(),
            mission,
            log_offset: 0,
            log_len: 0,
            streaming: LogStreaming::Unknown,
            stream: GrokStream::default(),
            thinking_open: false,
            thinking_snapshot: String::new(),
            auth_cancel_requested: false,
            running_since: None,
        })
    }

    /// Fetch and apply new log output. Called on every successful status
    /// observation (terminal ones included, so the tail is never lost).
    pub(crate) async fn pump(
        &mut self,
        client: &RemoteNodeClient,
        node: &RemoteNodeConfig,
        shared_token: &str,
    ) {
        if self.streaming == LogStreaming::Unsupported {
            return;
        }
        for _ in 0..MAX_CHUNKS_PER_TICK {
            let chunk = match client
                .get_job_log(node, shared_token, self.job_id, self.log_offset)
                .await
            {
                Ok(chunk) => chunk,
                Err(error) if error.is_not_found() && self.streaming == LogStreaming::Unknown => {
                    tracing::warn!(
                        mission_id = %self.mission_id,
                        job_id = %self.job_id,
                        node = %node.id,
                        "node has no job log route; grok output is only available from the terminal log tail"
                    );
                    self.streaming = LogStreaming::Unsupported;
                    return;
                }
                Err(error) => {
                    tracing::debug!(mission_id = %self.mission_id, job_id = %self.job_id, ?error, "remote grok log chunk fetch failed; retrying next tick");
                    return;
                }
            };
            self.streaming = LogStreaming::Supported;
            let caught_up = chunk.next_offset >= chunk.log_len;
            self.apply_chunk(&chunk, client, node, shared_token).await;
            if caught_up || chunk.data.is_empty() {
                return;
            }
        }
    }

    /// Bound silent login/startup hangs without limiting a running native goal.
    pub(crate) async fn check_startup(
        &mut self,
        status: &NodeJobStatus,
        client: &RemoteNodeClient,
        node: &RemoteNodeConfig,
        token: &str,
    ) {
        if status.state != "running" || self.stream.progress {
            return;
        }
        let since = self
            .running_since
            .get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= std::time::Duration::from_secs(120) {
            self.stream.error = Some("Remote harness produced no model/tool progress within 120 seconds; check the node's managed login and CLI connectivity before resuming.".to_string());
            if !self.auth_cancel_requested {
                self.auth_cancel_requested =
                    client.cancel_job(node, token, self.job_id).await.is_ok();
            }
        }
    }

    pub(crate) fn caught_up(&self) -> bool {
        self.streaming == LogStreaming::Unsupported || self.log_offset >= self.log_len
    }

    async fn apply_chunk(
        &mut self,
        chunk: &JobLogChunk,
        client: &RemoteNodeClient,
        node: &RemoteNodeConfig,
        shared_token: &str,
    ) {
        if chunk.next_offset < self.log_offset {
            // The node restarted its log (or the job was re-run); start over
            // rather than parse from a stale offset.
            self.close_thinking();
            self.log_offset = 0;
            self.log_len = chunk.log_len;
            self.stream = GrokStream::default();
            return;
        }
        self.log_len = chunk.log_len;
        self.log_offset = chunk.next_offset;
        let updates = self.stream.feed(&chunk.data);
        self.broadcast(updates).await;
        if self.stream.auth_required && !self.auth_cancel_requested {
            // The CLI is blocked on a browser callback that never comes;
            // cancel instead of burning the job timeout.
            if let Err(error) = client.cancel_job(node, shared_token, self.job_id).await {
                tracing::warn!(mission_id = %self.mission_id, job_id = %self.job_id, ?error, "remote grok job cancellation after interactive login prompt failed; poll loop will retry");
            } else {
                self.auth_cancel_requested = true;
            }
        }
    }

    async fn broadcast(&mut self, updates: Vec<StreamUpdate>) {
        for update in updates {
            match update {
                StreamUpdate::Tool { update, completed } => {
                    self.close_thinking();
                    let id = update
                        .get("toolCallId")
                        .or_else(|| update.get("id"))
                        .and_then(|v| v.as_str());
                    if let Some(id) = id {
                        let name = update
                            .get("title")
                            .or_else(|| update.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let event = if completed {
                            if !matches!(
                                update.get("status").and_then(|v| v.as_str()),
                                Some("completed" | "failed" | "cancelled")
                            ) {
                                continue;
                            }
                            AgentEvent::ToolResult {
                                tool_call_id: id.to_string(),
                                name,
                                result: update,
                                mission_id: Some(self.mission_id),
                            }
                        } else {
                            AgentEvent::ToolCall {
                                tool_call_id: id.to_string(),
                                name,
                                args: update
                                    .get("rawInput")
                                    .or_else(|| update.get("arguments"))
                                    .cloned()
                                    .unwrap_or_default(),
                                mission_id: Some(self.mission_id),
                            }
                        };
                        self.owner.publish_native(event).await;
                    }
                }
                StreamUpdate::ThinkingSnapshot(content) => {
                    self.thinking_open = true;
                    self.thinking_snapshot = content.clone();
                    self.owner.send(AgentEvent::Thinking {
                        content,
                        done: false,
                        mission_id: Some(self.mission_id),
                    });
                }
                StreamUpdate::TextSnapshot(content) => {
                    self.close_thinking();
                    self.owner.send(AgentEvent::TextDelta {
                        content,
                        mission_id: Some(self.mission_id),
                    });
                }
                StreamUpdate::Text | StreamUpdate::Thinking => {
                    unreachable!("unmaterialized stream snapshot")
                }
                StreamUpdate::SessionId(session_id) => {
                    self.persist_session(&session_id).await;
                }
                StreamUpdate::End => {
                    self.close_thinking();
                }
                StreamUpdate::AuthRequired => {
                    tracing::warn!(mission_id = %self.mission_id, job_id = %self.job_id, "remote grok job printed an interactive sign-in prompt; managed auth on the node is not usable");
                }
                StreamUpdate::Error(message) => {
                    tracing::warn!(mission_id = %self.mission_id, job_id = %self.job_id, %message, "remote grok job reported an error event");
                }
            }
        }
    }

    fn close_thinking(&mut self) {
        if self.thinking_open {
            self.thinking_open = false;
            let content = std::mem::take(&mut self.thinking_snapshot);
            self.owner
                .send(super::super::mission_runner::thinking_final_event(
                    content,
                    self.mission_id,
                ));
        }
    }

    /// Persist the CLI session id as the mission's native identity, fenced
    /// by this job's run lease, and broadcast it.
    async fn persist_session(&mut self, session_id: &str) {
        if self.session_persisted.as_deref() == Some(session_id) {
            return;
        }
        let run = match self
            .owner
            .mission_store
            .get_latest_mission_run(self.mission_id)
            .await
        {
            Ok(Some(run)) if run.owner_actor_id == super::remote_job_lease_owner(self.job_id) => {
                Some(SessionUpdateRun::from(&run))
            }
            _ => return,
        };
        match self
            .owner
            .mission_store
            .update_mission_session_id(
                self.mission_id,
                session_id,
                &self.mission.backend,
                run.as_ref(),
            )
            .await
        {
            Ok(true) => {
                self.session_persisted = Some(session_id.to_string());
                self.mission.session_id = Some(session_id.to_string());
                self.owner.send(AgentEvent::SessionIdUpdate {
                    run,
                    backend: self.mission.backend.clone(),
                    session_id: session_id.to_string(),
                    mission_id: self.mission_id,
                });
            }
            Ok(false) => {
                tracing::warn!(mission_id = %self.mission_id, job_id = %self.job_id, "remote grok session id rejected as stale; continuation will not resume this session");
            }
            Err(error) => {
                tracing::warn!(mission_id = %self.mission_id, job_id = %self.job_id, %error, "remote grok session id could not be persisted");
            }
        }
    }

    /// Terminal decision for the job. Flushes the parser, closes an open
    /// thinking block, and reports the native CLI outcome.
    pub(crate) async fn verdict(
        &mut self,
        status: &NodeJobStatus,
        node_id: &str,
    ) -> TerminalVerdict {
        let flushed = self.stream.finish();
        self.broadcast(flushed).await;
        self.close_thinking();
        if self.stream.session_id.is_none() {
            // Some CLI versions only print the session in the `end` event;
            // if the log route was unavailable, try the terminal tail.
            if self.streaming != LogStreaming::Supported {
                if let Some(tail) = status.log_tail.as_deref() {
                    let updates = self.stream.feed(tail);
                    let flushed = self.stream.finish();
                    self.broadcast(updates).await;
                    self.broadcast(flushed).await;
                    self.close_thinking();
                }
            }
        }

        let exit = status.exit_code;
        let succeeded = status.state == "succeeded" && exit.unwrap_or(0) == 0;
        let auth_required = self.stream.auth_required || exit == Some(MISSING_MANAGED_AUTH_EXIT);
        let native_end = self.stream.ended
            && matches!(
                self.stream.stop_reason.as_deref(),
                Some("end_turn" | "EndTurn")
            );
        let success = succeeded
            && !auth_required
            && self.stream.error.is_none()
            && (!self.mission.goal_mode || native_end);
        let mut content = self.stream.text.trim().to_string();
        let status_reason: &'static str = if auth_required {
            "remote_grok_auth_required"
        } else {
            "remote_node_job"
        };
        if !success {
            let mut report = String::new();
            if auth_required {
                report.push_str(
                    "The grok CLI on the node could not authenticate non-interactively: managed auth is not usable there. \
                     On the node, set SANDBOXED_NODE_GROK_HOME for the sandboxed-node service and run \
                     `GROK_HOME=<that dir> grok login --device-auth` as the service account, then resume this mission.",
                );
            } else {
                report.push_str(&format!(
                    "Remote {} job {} on node '{}' finished with state '{}' (exit {:?}){}.",
                    self.mission.backend,
                    self.job_id,
                    node_id,
                    status.state,
                    exit,
                    status
                        .error
                        .as_deref()
                        .map(|e| format!("; error: {e}"))
                        .unwrap_or_default()
                ));
                if let Some(error) = &self.stream.error {
                    report.push_str(&format!("\nCLI error: {error}"));
                }
            }
            let diagnostics = self.stream.diagnostics_text();
            if !diagnostics.is_empty() {
                report.push_str("\n\ndiagnostics:\n");
                report.push_str(&diagnostics);
            } else if content.is_empty() && self.streaming != LogStreaming::Supported {
                if let Some(tail) = status.log_tail.as_deref() {
                    report.push_str("\n\nlog tail:\n");
                    report.push_str(tail);
                }
            }
            if content.is_empty() {
                content = report;
            } else {
                content.push_str("\n\n");
                content.push_str(&report);
            }
        } else if content.is_empty() {
            content = format!(
                "Remote {} job {} on node '{}' finished without assistant text (stop reason: {}).",
                self.mission.backend,
                self.job_id,
                node_id,
                self.stream.stop_reason.as_deref().unwrap_or("unknown")
            );
        }

        if let Some(objective) = goal_objective(&self.mission) {
            let event = AgentEvent::GoalStatus {
                status: if success { "complete" } else { "paused" }.to_string(),
                objective,
                mission_id: Some(self.mission_id),
            };
            self.owner.publish_native(event).await;
        }

        TerminalVerdict {
            success,
            content,
            status_reason,
        }
    }
}

fn mission_model(mission: &Mission) -> Option<String> {
    Some(
        mission
            .model_override
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(resolve_grok_default_model),
    )
}

/// Persist a user-visible message for the prompt of a continuation turn.
async fn persist_turn_prompt(
    owner: &RemoteMissionOwner,
    mission_id: Uuid,
    prompt: &str,
    source: &str,
    message_id: Option<Uuid>,
) {
    let event = AgentEvent::UserMessage {
        id: message_id.unwrap_or_else(Uuid::new_v4),
        content: prompt.to_string(),
        queued: false,
        mission_id: Some(mission_id),
        source: Some(source.to_string()),
    };
    if let Err(error) = owner.mission_store.log_event(mission_id, &event).await {
        tracing::warn!(%mission_id, %error, "remote grok turn prompt could not be persisted");
    }
    owner.send(event);
}

// ─── Placement + remote continuation of existing missions ────────────────────

/// Where a mission's remote job lives (or lived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemotePlacement {
    pub(crate) node_id: String,
    pub(crate) job_id: Uuid,
    /// A ledger handle is still open: the job has not been observed terminal.
    pub(crate) live: bool,
}

/// Remote placement of a mission from the durable ledger, falling back to
/// the (possibly settled) `remote-job:*` run lease. `None` means the mission
/// never dispatched a raw/typed remote job.
pub(crate) async fn placement(
    ledger_dir: &std::path::Path,
    store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
) -> Result<Option<RemotePlacement>, String> {
    if let Some(t) = super::machine_transfer::committed(store, mission_id).await? {
        match t.destination {
            crate::api::mission_store::transfer::Machine::Node { id } => {
                let run = store.get_latest_mission_run(mission_id).await?;
                let job = run
                    .as_ref()
                    .filter(|r| r.generation > t.generation)
                    .and_then(|r| r.owner_actor_id.strip_prefix("remote-job:"))
                    .and_then(|s| Uuid::parse_str(s).ok());
                return Ok(Some(RemotePlacement {
                    node_id: id,
                    job_id: job.unwrap_or(Uuid::nil()),
                    live: run.is_some_and(|r| {
                        r.generation > t.generation && !r.execution_state.is_terminal()
                    }),
                }));
            }
            _ => return Ok(None),
        }
    }
    use remote_node::job_ledger::JobHandleKind;
    let handles = remote_node::job_ledger::load(ledger_dir)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(handle) = handles
        .iter()
        .filter(|handle| {
            handle.mission_id == mission_id
                && (handle.kind == JobHandleKind::Mission
                    || (handle.kind == JobHandleKind::Tentative && handle.identity.is_none()))
        })
        .max_by_key(|handle| (handle.submission_sequence, handle.started_at))
    {
        return Ok(Some(RemotePlacement {
            node_id: handle.node_id.clone(),
            job_id: handle.job_id,
            live: true,
        }));
    }
    let unknown = match store.get_mission(mission_id).await? {
        Some(mission) if !mission.requires_local_disk => Some(RemotePlacement {
            node_id: "unknown".into(),
            job_id: Uuid::nil(),
            live: false,
        }),
        _ => None,
    };
    let Some(run) = store.get_latest_mission_run(mission_id).await? else {
        return Ok(unknown);
    };
    let Some(job_id) = run
        .owner_actor_id
        .strip_prefix("remote-job:")
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return Ok(unknown);
    };
    let node_id = run
        .scope_unit
        .as_deref()
        .and_then(|scope| scope.strip_prefix("remote-node:"))
        .unwrap_or("unknown")
        .to_string();
    Ok(Some(RemotePlacement {
        node_id,
        job_id,
        live: !run.execution_state.is_terminal(),
    }))
}

/// Every message entry point must fail closed before starting a Core runner.
pub(crate) async fn reject_local_followup(
    ledger_dir: &std::path::Path,
    store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
) -> Result<(), String> {
    if let Some(placement) = placement(ledger_dir, store, mission_id).await? {
        let mission = store
            .get_mission(mission_id)
            .await?
            .ok_or("mission not found")?;
        return Err(local_resume_refusal(&mission, &placement));
    }
    Ok(())
}

/// Why a local resume of a remotely placed mission is refused. Used by
/// `resume_mission_impl` so internal callers (watchdog, MCP) never start a
/// local harness beside — or instead of — the node job.
pub(crate) fn local_resume_refusal(mission: &Mission, placement: &RemotePlacement) -> String {
    if matches!(
        mission.backend.as_str(),
        GROK_BACKEND | "opencode" | "codex"
    ) {
        format!(
            "{REMOTE_RESUME_REQUIRES_REPLACEMENT}: mission {} runs natively on remote node '{}'; \
             resume it through POST /api/control/missions/{}/resume (which continues it on the node) or create a replacement mission with remote_node_id",
            mission.id, placement.node_id, mission.id
        )
    } else {
        format!(
            "{REMOTE_RESUME_REQUIRES_REPLACEMENT}: mission {} ran '{}' on remote node '{}'; that harness keeps no resumable node session — \
             create a replacement mission (supersedes_mission_id={}) with remote_node_id instead of resuming locally",
            mission.id, mission.backend, placement.node_id, mission.id
        )
    }
}

/// Continue a remotely placed mission on its node with a new turn.
///
/// - Grok and OpenCode continue using their recorded native CLI session.
///   Other harnesses get a
///   [`REMOTE_RESUME_REQUIRES_REPLACEMENT`] conflict.
/// - A live node job ([`REMOTE_JOB_STILL_RUNNING`]) and an unconfigured
///   node are conflicts too; nothing is started locally in any case.
///
/// Explicit content is passed verbatim; a goal resumes with `/goal resume`.
pub(crate) async fn continue_on_node(
    state: &Arc<AppState>,
    control: &ControlState,
    user_id: &str,
    mission_id: Uuid,
    placement: RemotePlacement,
    content: Option<String>,
    message_id: Option<Uuid>,
) -> Result<Mission, (StatusCode, String)> {
    let _admission = super::DISPATCH_ADMISSION.lock().await;
    let _file_guard = super::dispatch_admission::durable_lock(&state.config)
        .await
        .map_err(internal)?;
    let store = control.mission_store.clone();
    super::machine_transfer::guard(&store, mission_id)
        .await
        .map_err(internal)?;
    let mission = store
        .get_mission(mission_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Mission {mission_id} not found"),
            )
        })?;
    // Create absorbs project missions under a generated track, including Orb
    // requests with no writer flag. Re-admit that same identity and capability;
    // PR bindings and explicit tracks still require full create admission.
    let replacement = || {
        (StatusCode::CONFLICT, format!("{REMOTE_RESUME_REQUIRES_REPLACEMENT}: PR or explicit-track missions need create admission; create a remote replacement with supersedes_mission_id={mission_id}"))
    };
    if mission.project.github_pr.is_some()
        || mission.project.tags.iter().any(|tag| tag == "pr-writer")
    {
        return Err(replacement());
    }
    let track_claim = if let Some(track) = mission.project.track.as_deref() {
        let slug = mission
            .project
            .project
            .as_deref()
            .ok_or_else(&replacement)?;
        if track != crate::api::track_leases::generated_track_key(&mission_id.to_string()) {
            return Err(replacement());
        }
        let canonical = state
            .projects
            .track(slug, track)
            .map_err(internal)?
            .ok_or_else(&replacement)?;
        if canonical.track != track {
            return Err(replacement());
        }
        // Preserve the generated-track restriction against another owner, but
        // allow our own still-live writer lease to be renewed on continuation.
        if state
            .projects
            .live_leases(Some(slug))
            .map_err(internal)?
            .iter()
            .any(|lease| {
                lease.track_id == canonical.id
                    && lease.mode == "writer"
                    && lease.attempt_id != mission_id.to_string()
            })
        {
            return Err(replacement());
        }
        let writer = super::mission_is_pr_writer_in_store(&store, &mission)
            .await
            .map_err(internal)?;
        let mode = crate::api::track_leases::lease_mode(
            writer.then_some(true),
            &mission.project.tags,
            mission.project.intent.as_deref(),
        );
        Some(crate::api::track_leases::lease_request(
            slug,
            track,
            &mission_id.to_string(),
            mode,
            None,
        ))
    } else {
        None
    };
    if !matches!(
        mission.backend.as_str(),
        GROK_BACKEND | "opencode" | "codex"
    ) {
        return Err((
            StatusCode::CONFLICT,
            local_resume_refusal(&mission, &placement),
        ));
    }
    if placement.live
        || matches!(
            mission.status,
            MissionStatus::Active | MissionStatus::Pending
        )
    {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "{REMOTE_JOB_STILL_RUNNING}: mission {} still owns job {} on remote node '{}'; wait for it to finish or cancel the mission first",
                mission.id, placement.job_id, placement.node_id
            ),
        ));
    }
    let _node = state
        .config
        .remote_nodes
        .node(&placement.node_id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                format!(
                    "{REMOTE_RESUME_REQUIRES_REPLACEMENT}: remote node '{}' is no longer configured; create a replacement mission on another node",
                    placement.node_id
                ),
            )
        })?;
    if !state.config.remote_nodes.enabled {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "{REMOTE_RESUME_REQUIRES_REPLACEMENT}: {}",
                RemoteNodeError::Disabled
            ),
        ));
    }
    let owner = RemoteMissionOwner::live(control);

    let content = content
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    // Jobs launched before OpenCode streaming was enabled have a placeholder
    // session id. Recover the native identity from that exact job's log.
    let mut native_session = mission.session_id.clone();
    let transferred = super::machine_transfer::committed(&store, mission_id)
        .await
        .map_err(internal)?
        .is_some();
    if !transferred
        && mission.backend == "opencode"
        && !native_session
            .as_deref()
            .is_some_and(|s| s.starts_with("ses_"))
    {
        let token = std::env::var(&_node.token_env).map_err(internal)?;
        let client = RemoteNodeClient::default();
        let mut stream = GrokStream::default();
        let mut offset = 0;
        for _ in 0..MAX_CHUNKS_PER_TICK {
            let chunk = client
                .get_job_log(&_node, &token, placement.job_id, offset)
                .await
                .map_err(internal)?;
            stream.feed(&chunk.data);
            if stream.session_id.is_some() {
                break;
            }
            if chunk.next_offset <= offset || chunk.next_offset >= chunk.log_len {
                break;
            }
            offset = chunk.next_offset;
        }
        native_session = stream.session_id;
    }
    let session_id = native_session.filter(|s| !s.trim().is_empty());
    if session_id.is_none() && !transferred {
        return Err((
            StatusCode::CONFLICT,
            "Mission has no recorded native session".into(),
        ));
    }
    let prompt = content
        .clone()
        .unwrap_or_else(|| super::INTERRUPTED_RESUME_PROMPT.to_string());
    let history_prompt = prompt.clone();
    let prompt =
        super::machine_transfer::context(&store, mission_id, prompt, session_id.as_deref())
            .await
            .map_err(internal)?;
    if let Some(objective) = super::parse_goal_objective(&prompt) {
        if objective == "clear" {
            store
                .update_mission_goal(mission.id, false, None)
                .await
                .map_err(internal)?;
        } else if !matches!(objective.as_str(), "resume" | "pause" | "status") {
            store
                .update_mission_goal(mission.id, true, Some(&objective))
                .await
                .map_err(internal)?;
        }
    }
    let source = if content.is_some() {
        format!("api:{user_id}")
    } else {
        RESUME_SOURCE.to_string()
    };
    let plan = if mission.backend == "codex" {
        RemoteHarnessPlan::Codex {
            effort: mission.model_effort.clone(),
            fast_mode: mission.fast_mode,
            model: mission.model_override.clone().ok_or_else(|| {
                (
                    StatusCode::CONFLICT,
                    "Codex remote session has no recorded model".to_string(),
                )
            })?,
            prompt: prompt.clone(),
            resume_session_id: session_id.clone(),
        }
    } else if mission.backend == "opencode" {
        RemoteHarnessPlan::OpenCode {
            model: mission
                .model_override
                .as_deref()
                .map(|m| m.strip_prefix("builtin/").unwrap_or(m).to_string()),
            prompt: prompt.clone(),
            resume_session_id: session_id.clone(),
        }
    } else {
        RemoteHarnessPlan::Grok {
            model: mission_model(&mission),
            prompt: prompt.clone(),
            resume_session_id: session_id.clone(),
            new_session_id: if session_id.is_none() {
                Some(Uuid::new_v4().to_string())
            } else {
                None
            },
        }
    };
    require_node_managed_auth(state, &placement.node_id, &plan)
        .await
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    // Terminal cleanup may have released the original claim. Reacquire and
    // revalidate it under admission locks before making the mission runnable.
    if let Some(request) = track_claim.as_ref() {
        state
            .projects
            .acquire_track_lease(request)
            .map_err(|_| replacement())?;
        state
            .projects
            .revalidate_track_lease(request)
            .map_err(|_| replacement())?;
    }
    // Acquiring a run requires Pending/Active. Under admission locks, mark
    // the mission remote before moving it to Pending so no local scheduler
    // can claim the activation window. Dispatch then fences before submit.
    let previous = crate::api::mission_store::MissionStatusSnapshot::capture(&mission);
    store
        .set_mission_requires_local_disk(mission.id, false)
        .await
        .map_err(internal)?;
    store
        .update_mission_status(mission.id, MissionStatus::Pending)
        .await
        .map_err(internal)?;
    let resumed =
        match super::dispatch_remote_job(state, control, &mission, &placement.node_id, &plan).await
        {
            Ok(resumed) => resumed,
            Err(message) => {
                store
                    .restore_mission_status(mission.id, &previous)
                    .await
                    .map_err(internal)?;
                return Err((StatusCode::CONFLICT, message));
            }
        };
    persist_turn_prompt(&owner, mission.id, &history_prompt, &source, message_id).await;
    Ok(resumed)
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPARK_STREAM: &str =
        include_str!("../../../tests/fixtures/native_grok_goal_resume.jsonl");
    const SPARK_TEXT: &str = include_str!("../../../tests/fixtures/native_grok_goal_resume.txt");

    #[test]
    fn opencode_stream_preserves_session_text_and_tools_across_chunks() {
        let mut stream = GrokStream::default();
        let lines = concat!(
            "{\"type\":\"text\",\"sessionID\":\"ses_test\",\"part\":{\"text\":\"Checking files\"}}\n",
            "{\"type\":\"tool_use\",\"sessionID\":\"ses_test\",\"part\":{\"callID\":\"call_1\",\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"input\":{\"command\":\"pwd\"},\"output\":\"/workspace\"}}}\n",
            "{\"type\":\"text\",\"sessionID\":\"ses_test\",\"part\":{\"text\":\"Done\"}}\n"
        );
        let mut updates = stream.feed(&lines[..17]);
        updates.extend(stream.feed(&lines[17..]));
        assert_eq!(stream.session_id.as_deref(), Some("ses_test"));
        assert_eq!(stream.text, "Checking files\n\nDone");
        assert!(stream.progress);
        assert!(updates.iter().any(|u| matches!(
            u,
            StreamUpdate::Tool {
                completed: true,
                ..
            }
        )));
        let plan = RemoteHarnessPlan::OpenCode {
            model: Some("smart".into()),
            prompt: "continue".into(),
            resume_session_id: Some("ses_test".into()),
        };
        let execution =
            super::super::remote_execution_for_plan(&plan, "https://core.example", "test-key");
        assert!(execution.command.contains("--session 'ses_test'"));
    }

    #[tokio::test]
    async fn native_finish_preserves_event_order_with_delayed_logger() {
        use crate::api::mission_store::{MissionStore, SqliteMissionStore};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn MissionStore> = Arc::new(
            SqliteMissionStore::new(dir.path().join("missions"), "native-events")
                .await
                .unwrap(),
        );
        let mission = store
            .create_mission(Some("hostname"), None, None, None, None, Some("grok"), None)
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(1024);
        let owner = RemoteMissionOwner {
            mission_store: store.clone(),
            events_tx: Some(tx),
        };
        let job_id = Uuid::new_v4();
        let mut observer = NativeGrokObserver::attach(&owner, "spark", mission.id, job_id)
            .await
            .unwrap();
        observer.streaming = LogStreaming::Supported;
        // The real hostname canary has one tool invocation, text deltas and
        // a repeated full text snapshot. Hold the logger until after finish
        // to deterministically reproduce the former direct-write race.
        let fixture = include_str!("../../../tests/fixtures/native_grok_fixed_session.jsonl");
        for line in fixture.lines() {
            let updates = observer.stream.feed(&format!("{line}\n"));
            observer.broadcast(updates).await;
        }
        let status: NodeJobStatus = serde_json::from_value(serde_json::json!({
            "job_id":job_id, "mission_id":mission.id, "state":"succeeded",
            "exit_code":0, "created_at":"2026-09-20T00:00:00Z"
        }))
        .unwrap();
        let verdict = observer.verdict(&status, "spark").await;
        assert!(verdict.success);
        let final_text = verdict.content.clone();
        super::super::finalize_remote_mission(
            &owner,
            mission.id,
            None,
            "spark",
            verdict.success,
            verdict.content,
            verdict.status_reason,
            true,
        )
        .await
        .unwrap();
        assert!(
            store
                .get_events(mission.id, None, None, None)
                .await
                .unwrap()
                .is_empty(),
            "native events must not race the queued logger with direct writes"
        );
        let mut canonical_seen = false;
        let mut canonical_count = 0;
        let mut tool_calls = 0;
        let mut tool_results = 0;
        while let Ok(event) = rx.try_recv() {
            match &event {
                AgentEvent::AssistantMessage { content, .. } => {
                    assert_eq!(content, &final_text);
                    canonical_seen = true;
                    canonical_count += 1;
                }
                AgentEvent::TextDelta { .. } => assert!(
                    !canonical_seen,
                    "no text delta after the canonical assistant message"
                ),
                AgentEvent::ToolCall { .. } => tool_calls += 1,
                AgentEvent::ToolResult { .. } => tool_results += 1,
                _ => {}
            }
            if super::super::should_persist_event(&event) {
                store.log_event(mission.id, &event).await.unwrap();
            }
        }
        assert_eq!(canonical_count, 1);
        assert_eq!(
            tool_calls, 1,
            "observation must not repeat the node invocation"
        );
        assert_eq!(tool_results, 1);
        let saved = store
            .get_events(mission.id, None, None, None)
            .await
            .unwrap();
        let canonical = saved
            .iter()
            .find(|e| e.event_type == "assistant_message")
            .unwrap();
        assert!(saved
            .iter()
            .filter(|e| e.event_type == "text_delta")
            .all(|e| e.sequence < canonical.sequence));
        assert_eq!(
            saved.iter().filter(|e| e.event_type == "tool_call").count(),
            1
        );
        assert_eq!(
            saved
                .iter()
                .filter(|e| e.event_type == "tool_result")
                .count(),
            1
        );
    }

    fn assert_spark_stream(stream: &GrokStream, updates: &[StreamUpdate]) {
        assert_eq!(stream.json_events, 420);
        assert_eq!(stream.text, SPARK_TEXT);
        assert!(stream.ended);
        assert_eq!(stream.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(
            stream.session_id.as_deref(),
            Some("01a0beb8-9ccf-7a43-bf61-ef87af487088")
        );
        assert!(!stream.auth_required);
        assert!(stream.error.is_none());
        assert!(stream.diagnostics.is_empty());
        assert!(stream.partial.is_empty());
        assert_eq!(
            updates
                .iter()
                .filter(|u| matches!(
                    u,
                    StreamUpdate::Tool {
                        completed: false,
                        ..
                    }
                ))
                .count(),
            35
        );
        assert_eq!(
            updates
                .iter()
                .filter(|u| matches!(
                    u,
                    StreamUpdate::Tool {
                        completed: true,
                        ..
                    }
                ))
                .count(),
            76
        );
        assert_eq!(
            updates
                .iter()
                .filter(|u| matches!(u, StreamUpdate::SessionId(_)))
                .count(),
            1
        );
        assert_eq!(
            updates
                .iter()
                .filter(|u| matches!(u, StreamUpdate::End))
                .count(),
            1
        );
    }

    #[test]
    fn real_spark_goal_resume_stream_preserves_progress_and_deduplicates_snapshot() {
        let mut stream = GrokStream::default();
        let mut updates = Vec::new();
        for line in SPARK_STREAM.lines() {
            updates.extend(stream.feed(line));
            updates.extend(stream.feed("\n"));
            if !stream.ended && !stream.text.is_empty() {
                assert!(updates
                    .iter()
                    .any(|u| matches!(u, StreamUpdate::TextSnapshot(_))));
            }
        }
        updates.extend(stream.finish());
        assert_spark_stream(&stream, &updates);
    }

    #[tokio::test]
    async fn real_spark_stream_survives_bounded_log_cursors_and_unterminated_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.jsonl");
        // Exercise finish() as well as lines split across node log chunks.
        std::fs::write(&path, SPARK_STREAM.trim_end_matches('\n')).unwrap();
        for chunk_size in [257, 4096, 65536] {
            let mut stream = GrokStream::default();
            let mut updates = Vec::new();
            let mut offset = 0;
            loop {
                let (data, next, len) =
                    crate::node::read_log_chunk(&path, offset, chunk_size, true)
                        .await
                        .unwrap();
                assert!(next > offset);
                assert!(data.len() <= chunk_size as usize);
                updates.extend(stream.feed(&data));
                assert!(stream.partial.len() <= 1024 * 1024);
                offset = next;
                if offset == len {
                    break;
                }
            }
            assert!(!stream.ended);
            updates.extend(stream.finish());
            assert_spark_stream(&stream, &updates);
        }
    }

    #[test]
    fn stream_coalesces_delta_bursts_without_moving_tools_or_copying_per_delta() {
        let mut stream = GrokStream::default();
        let deltas: String = (0..500)
            .map(|i| format!("{{\"type\":\"text\",\"data\":\"{i},\"}}\n"))
            .collect();
        let segment: String = (0..500).map(|i| format!("{i},")).collect();
        let thought = "{\"type\":\"thought\",\"data\":\"p\"}\n";
        let chunk = format!(
            "{}{}{{\"type\":\"tool_call\",\"toolCallId\":\"t\"}}\n{}",
            thought.repeat(500),
            deltas,
            deltas
        );
        let updates = stream.feed(&chunk);
        assert_eq!(updates.len(), 4, "one snapshot per segment, not per event");
        assert_eq!(updates[0], StreamUpdate::ThinkingSnapshot("p".repeat(500)));
        assert_eq!(updates[1], StreamUpdate::TextSnapshot(segment.clone()));
        assert!(matches!(updates[2], StreamUpdate::Tool { .. }));
        assert_eq!(updates[3], StreamUpdate::TextSnapshot(segment.repeat(2)));
        let copied_bytes: usize = updates
            .iter()
            .map(|u| match u {
                StreamUpdate::TextSnapshot(s) | StreamUpdate::ThinkingSnapshot(s) => s.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(copied_bytes, 500 + segment.len() * 3);
    }

    #[test]
    fn real_fixed_session_fixture_matches_preallocated_cli_identity() {
        let fixture = include_str!("../../../tests/fixtures/native_grok_fixed_session.jsonl");
        let id = "a1778740-4e51-4de4-b8c3-0f16d68b07de";
        let mut stream = GrokStream::default();
        for line in fixture.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["type"] != "end" {
                assert!(grok_event_session_id(&value).is_none());
            }
            stream.feed(&format!("{line}\n"));
        }
        assert_eq!(stream.session_id.as_deref(), Some(id));
        let fresh = execution(None, "/goal objective", None, Some(id), "grok".into());
        assert!(fresh.command.contains(&format!("--session-id '{id}'")));
        assert!(!fresh.command.contains("--resume"));
        let resumed = execution(None, "/goal resume", Some(id), None, "grok".into());
        assert!(resumed.command.contains(&format!("--resume '{id}'")));
        assert!(!resumed.command.contains("--session-id"));
    }

    #[test]
    fn plan_preserves_native_goal_and_pins_a_model() {
        let plan = super::plan(None, "/goal ship the vanity generator".into());
        let RemoteHarnessPlan::Grok {
            model,
            prompt,
            resume_session_id,
            new_session_id,
        } = plan
        else {
            panic!("grok plan");
        };
        assert_eq!(
            model.as_deref(),
            Some(resolve_grok_default_model().as_str())
        );
        assert_eq!(prompt, "/goal ship the vanity generator");
        assert_eq!(resume_session_id, None);

        assert!(Uuid::parse_str(new_session_id.as_deref().unwrap()).is_ok());
        let plain = super::plan(Some("grok-4.6".into()), "say hi".into());
        assert_eq!(
            plain,
            RemoteHarnessPlan::Grok {
                model: Some("grok-4.6".into()),
                prompt: "say hi".into(),
                resume_session_id: None,
                new_session_id: match &plain {
                    RemoteHarnessPlan::Grok { new_session_id, .. } => new_session_id.clone(),
                    _ => unreachable!(),
                }
            }
        );
    }

    #[test]
    fn execution_requests_managed_auth_by_name_and_fails_closed() {
        let exec = execution(
            Some("grok-4.6"),
            "-list files it's here",
            Some("sess-1"),
            None,
            "grok/grok-4.6".into(),
        );
        assert_eq!(exec.managed_auth, vec!["grok".to_string()]);
        let env = exec.env.unwrap();
        assert_eq!(env.get("NO_COLOR").map(String::as_str), Some("1"));
        // No credential-shaped or home-shaped env: the node owns both.
        assert!(env
            .keys()
            .all(|key| !key.starts_with("GROK_") && !key.starts_with("XAI_")));
        let command = &exec.command;
        assert!(
            command.starts_with("command -v grok >/dev/null 2>&1 ||"),
            "{command}"
        );
        assert!(
            command.contains("[ -r \"${GROK_HOME}/auth.json\" ] ||"),
            "{command}"
        );
        assert!(command.contains("exit 78; }; exec grok --output-format streaming-json --always-approve --no-plan --model 'grok-4.6' --resume 'sess-1' --cwd \"$PWD\" -p ' -list files it'\\''s here'"), "{command}");
        assert!(
            !command.contains("auth.json'"),
            "no path literal beyond the guard: {command}"
        );

        let fresh = execution(None, "hi", None, None, "grok".into());
        assert!(fresh.command.ends_with("exec grok --output-format streaming-json --always-approve --no-plan --cwd \"$PWD\" -p 'hi'"), "{}", fresh.command);
        assert!(!fresh.command.contains("--resume"));
        assert!(!fresh.command.contains("--session-id"));
        assert!(!fresh.command.contains("--continue"));
    }

    #[test]
    fn native_goal_resume_and_snapshot_deduplication() {
        let command = execution(
            Some("grok-4.6"),
            "/goal resume",
            Some("native-session"),
            None,
            "grok".into(),
        )
        .command;
        assert!(command.contains("--resume 'native-session'"));
        assert!(command.ends_with("-p '/goal resume'"));
        let mut stream = GrokStream::default();
        for text in ["Hel", "lo 🦀", "Hello 🦀", "Next", " phase", "Next phase"] {
            stream.feed(&format!(
                "{}\n",
                serde_json::json!({"type":"text", "data":text})
            ));
        }
        assert_eq!(stream.text, "Hello 🦀Next phase");
        assert!(!stream.text.contains("goal_complete"));
    }

    #[test]
    fn oversized_partial_line_is_dropped_then_parser_recovers() {
        let mut stream = GrokStream::default();
        stream.feed(&"x".repeat(1024 * 1024 + 1));
        assert!(stream.partial.is_empty());
        stream.feed("\n{\"type\":\"text\",\"data\":\"ok\"}\n");
        assert_eq!(stream.text, "ok");
    }

    #[test]
    fn heartbeat_gate_names_the_fix() {
        assert!(heartbeat_supports_grok(&["grok".into()], "dgx").is_ok());
        let err = heartbeat_supports_grok(&[], "dgx").unwrap_err();
        assert!(err.starts_with("REMOTE_AUTH_REQUIRED: "), "{err}");
        assert!(err.contains("SANDBOXED_NODE_GROK_HOME"), "{err}");
        assert!(err.contains("grok login --device-auth"), "{err}");
    }

    #[test]
    fn stream_parses_text_thought_end_and_partial_lines() {
        let mut stream = GrokStream::default();
        let mut updates = stream.feed("A new version of Grok Build is available: 0.1.210 -> 1.0.34\n{\"type\":\"thought\",\"data\":\"plan\"}\n{\"type\":\"text\",\"data\":\"Hel\"}\n{\"type\":\"text\",\"da");
        assert_eq!(
            updates,
            vec![
                StreamUpdate::ThinkingSnapshot("plan".into()),
                StreamUpdate::TextSnapshot("Hel".into())
            ]
        );
        assert_eq!(stream.text, "Hel");
        assert_eq!(stream.thinking, "plan");
        updates = stream.feed("ta\":\"lo <goal_continue/>\"}\n{\"type\":\"end\",\"stopReason\":\"EndTurn\",\"sessionId\":\"abc123\",\"requestId\":\"r1\"}\n");
        assert_eq!(
            updates,
            vec![
                StreamUpdate::TextSnapshot("Hello <goal_continue/>".into()),
                StreamUpdate::SessionId("abc123".into()),
                StreamUpdate::End
            ]
        );
        assert_eq!(stream.text, "Hello <goal_continue/>");
        assert!(stream.ended);
        assert_eq!(stream.stop_reason.as_deref(), Some("EndTurn"));
        assert_eq!(stream.session_id.as_deref(), Some("abc123"));
        assert!(!stream.auth_required);
        assert_eq!(stream.diagnostics.len(), 1);
        assert_eq!(stream.json_events, 4);
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn codex_stream_preserves_thread_tools_text_and_failure() {
        let mut stream = GrokStream::default();
        let events = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"thread-123\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\",\"id\":\"cmd-1\",\"command\":\"pwd\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"id\":\"cmd-1\",\"status\":\"completed\",\"aggregated_output\":\"/work\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Done\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n",
        );
        let mut updates = stream.feed(&events[..23]);
        updates.extend(stream.feed(&events[23..]));
        assert_eq!(stream.session_id.as_deref(), Some("thread-123"));
        assert_eq!(stream.text, "Done");
        assert!(stream.ended && stream.progress);
        assert!(matches!(
            &updates[1],
            StreamUpdate::Tool {
                completed: false,
                ..
            }
        ));
        assert!(matches!(
            &updates[2],
            StreamUpdate::Tool {
                completed: true,
                ..
            }
        ));
        assert_eq!(updates.last(), Some(&StreamUpdate::End));
        stream.feed("{\"type\":\"turn.failed\",\"error\":{\"message\":\"rate limited\"}}\n");
        assert_eq!(stream.error.as_deref(), Some("rate limited"));
    }

    #[test]
    fn stream_flags_interactive_login_and_errors() {
        let mut stream = GrokStream::default();
        let updates = stream.feed("\nSigning in with Grok...\nOpen this URL to sign in:\n  https://auth.x.ai/oauth2/authorize?x=y\n");
        assert_eq!(updates, vec![StreamUpdate::AuthRequired]);
        assert!(stream.auth_required);
        let mut stream = GrokStream::default();
        let updates = stream.feed("{\"type\":\"error\",\"error\":\"model not found\"}\n");
        assert_eq!(updates, vec![StreamUpdate::Error("model not found".into())]);
        assert_eq!(stream.error.as_deref(), Some("model not found"));
        // A JSON line containing the sign-in words is assistant text, not a prompt.
        let mut stream = GrokStream::default();
        stream.feed("{\"type\":\"text\",\"data\":\"do not open this url to sign in\"}\n");
        assert!(!stream.auth_required);
        // A trailing partial line is flushed at finish.
        let mut stream = GrokStream::default();
        assert!(stream
            .feed("{\"type\":\"text\",\"data\":\"tail\"}")
            .is_empty());
        assert_eq!(
            stream.finish(),
            vec![StreamUpdate::TextSnapshot("tail".into())]
        );
        assert_eq!(stream.text, "tail");
    }

    #[test]
    fn local_resume_refusal_distinguishes_grok_from_other_harnesses() {
        let placement = RemotePlacement {
            node_id: "dgx".into(),
            job_id: Uuid::nil(),
            live: false,
        };
        let mut mission: Mission = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "status": "failed",
            "workspace_id": Uuid::nil(),
            "backend": "grok",
            "history": [],
            "created_at": "",
            "updated_at": "",
        }))
        .unwrap();
        let grok = local_resume_refusal(&mission, &placement);
        assert!(
            grok.starts_with("REMOTE_RESUME_REQUIRES_REPLACEMENT: "),
            "{grok}"
        );
        assert!(grok.contains("/resume"), "{grok}");
        mission.backend = "codex".into();
        assert!(local_resume_refusal(&mission, &placement).contains("/resume"));
        mission.backend = "claudecode".into();
        let other = local_resume_refusal(&mission, &placement);
        assert!(other.contains("supersedes_mission_id"), "{other}");
        assert!(other.contains("'claudecode'"), "{other}");
    }
}
