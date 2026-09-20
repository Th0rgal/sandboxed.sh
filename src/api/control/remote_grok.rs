//! Native Grok Build CLI as a typed remote launch.
//!
//! A `backend: "grok"` launch runs the real `grok` CLI on the node (not an
//! OpenCode stand-in): `grok --output-format streaming-json --always-approve
//! [--model <id>] [--resume <session>] -p <prompt>` under the node's raw job
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
//!   runner. The `end` event's `sessionId` is persisted as the mission's
//!   native identity so later turns resume the same CLI session.
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
        updates
    }

    /// Flush a trailing partial line at end of log.
    pub(crate) fn finish(&mut self) -> Vec<StreamUpdate> {
        let mut updates = Vec::new();
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.feed_line(&line, &mut updates);
        }
        updates
    }

    fn feed_line(&mut self, raw: &str, updates: &mut Vec<StreamUpdate>) {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            if grok_line_requests_interactive_login(line) && !self.auth_required {
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
        self.json_events += 1;
        if let Some(model) = grok_event_model(&value) {
            self.model = Some(model);
        }
        if let Some(session) = grok_event_session_id(&value) {
            if self.session_id.as_deref() != Some(session.as_str()) {
                self.session_id = Some(session.clone());
                updates.push(StreamUpdate::SessionId(session));
            }
        }
        let kind = value
            .get("type")
            .and_then(|t| t.as_str())
            .map(|t| t.to_ascii_lowercase());
        if matches!(kind.as_deref(), Some("tool_call" | "tool_call_update")) {
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
                append_bounded(&mut self.thinking, &reasoning);
                updates.push(StreamUpdate::Thinking);
            }
            return;
        }
        if let Some(text) = grok_event_text(&value) {
            if !text.is_empty() {
                self.progress = true;
                // Native CLI emits deltas followed by the same full text snapshot.
                if text == self.text_segment && !self.text_segment.is_empty() {
                    self.text_segment.clear();
                } else if !self.text_segment.is_empty() && text.starts_with(&self.text_segment) {
                    let suffix = &text[self.text_segment.len()..];
                    append_bounded(&mut self.text, suffix);
                    self.text_segment.clear();
                    updates.push(StreamUpdate::Text);
                } else {
                    append_bounded(&mut self.text, &text);
                    append_bounded(&mut self.text_segment, &text);
                    updates.push(StreamUpdate::Text);
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
            Ok(Some(mission)) if mission.backend == GROK_BACKEND => mission,
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
            self.stream.error = Some("Native Grok produced no model/tool progress within 120 seconds; check the node's managed login and CLI connectivity before resuming.".to_string());
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
                        let _ = self
                            .owner
                            .mission_store
                            .log_event(self.mission_id, &event)
                            .await;
                        self.owner.send(event);
                    }
                }
                StreamUpdate::Thinking => {
                    self.thinking_open = true;
                    self.owner.send(AgentEvent::Thinking {
                        content: self.stream.thinking.clone(),
                        done: false,
                        mission_id: Some(self.mission_id),
                    });
                }
                StreamUpdate::Text => {
                    self.close_thinking();
                    self.owner.send(AgentEvent::TextDelta {
                        content: self.stream.text.clone(),
                        mission_id: Some(self.mission_id),
                    });
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
            let content = std::mem::take(&mut self.stream.thinking);
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
            .update_mission_session_id(self.mission_id, session_id, GROK_BACKEND, run.as_ref())
            .await
        {
            Ok(true) => {
                self.session_persisted = Some(session_id.to_string());
                self.mission.session_id = Some(session_id.to_string());
                self.owner.send(AgentEvent::SessionIdUpdate {
                    run,
                    backend: GROK_BACKEND.to_string(),
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
                    "Remote grok job {} on node '{}' finished with state '{}' (exit {:?}){}.",
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
                "Remote grok job {} on node '{}' finished without assistant text (stop reason: {}).",
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
            let _ = self
                .owner
                .mission_store
                .log_event(self.mission_id, &event)
                .await;
            self.owner.send(event);
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
) {
    let event = AgentEvent::UserMessage {
        id: Uuid::new_v4(),
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

/// Why a local resume of a remotely placed mission is refused. Used by
/// `resume_mission_impl` so internal callers (watchdog, MCP) never start a
/// local harness beside — or instead of — the node job.
pub(crate) fn local_resume_refusal(mission: &Mission, placement: &RemotePlacement) -> String {
    if mission.backend == GROK_BACKEND {
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
/// - Only native Grok missions can continue: their CLI session id is the
///   durable identity `--resume` needs. Other harnesses get a
///   [`REMOTE_RESUME_REQUIRES_REPLACEMENT`] conflict.
/// - A live node job ([`REMOTE_JOB_STILL_RUNNING`]) and an unconfigured
///   node are conflicts too; nothing is started locally in any case.
/// Explicit content is passed verbatim; a goal resumes with `/goal resume`.
pub(crate) async fn continue_on_node(
    state: &Arc<AppState>,
    control: &ControlState,
    user_id: &str,
    mission_id: Uuid,
    placement: RemotePlacement,
    content: Option<String>,
) -> Result<Mission, (StatusCode, String)> {
    let _admission = super::DISPATCH_ADMISSION.lock().await;
    let _file_guard = super::dispatch_admission::durable_lock(&state.config)
        .await
        .map_err(internal)?;
    let store = control.mission_store.clone();
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
    // PR/track writer re-admission is intentionally outside this bounded
    // native continuation path. Preserve its existing identity checks by
    // requiring a linked replacement through normal create admission.
    if mission.project.github_pr.is_some() || mission.project.track.is_some() {
        return Err((StatusCode::CONFLICT, format!("{REMOTE_RESUME_REQUIRES_REPLACEMENT}: tracked/PR writer missions need create admission; create a remote replacement with supersedes_mission_id={mission_id}")));
    }
    if mission.backend != GROK_BACKEND {
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
    let session_id = mission.session_id.as_deref().map(str::trim)
        .filter(|s| !s.is_empty()).map(str::to_string)
        .ok_or_else(|| (StatusCode::CONFLICT, format!(
            "{REMOTE_RESUME_REQUIRES_REPLACEMENT}: mission {mission_id} has no recorded native Grok session; create a remote replacement with supersedes_mission_id={mission_id}"
        )))?;
    let prompt = content.clone().unwrap_or_else(|| {
        if mission.goal_mode {
            "/goal resume".to_string()
        } else {
            super::INTERRUPTED_RESUME_PROMPT.to_string()
        }
    });
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
    let plan = RemoteHarnessPlan::Grok {
        model: mission_model(&mission),
        prompt: prompt.clone(),
        resume_session_id: Some(session_id),
    };
    require_node_managed_auth(state, &placement.node_id, &plan)
        .await
        .map_err(|message| (StatusCode::BAD_REQUEST, message))?;
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
    persist_turn_prompt(&owner, mission.id, &prompt, &source).await;
    Ok(resumed)
}

fn internal(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_preserves_native_goal_and_pins_a_model() {
        let plan = super::plan(None, "/goal ship the vanity generator".into());
        let RemoteHarnessPlan::Grok {
            model,
            prompt,
            resume_session_id,
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

        let plain = super::plan(Some("grok-4.6".into()), "say hi".into());
        assert_eq!(
            plain,
            RemoteHarnessPlan::Grok {
                model: Some("grok-4.6".into()),
                prompt: "say hi".into(),
                resume_session_id: None
            }
        );
    }

    #[test]
    fn execution_requests_managed_auth_by_name_and_fails_closed() {
        let exec = execution(
            Some("grok-4.6"),
            "-list files it's here",
            Some("sess-1"),
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

        let fresh = execution(None, "hi", None, "grok".into());
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
        assert_eq!(updates, vec![StreamUpdate::Thinking, StreamUpdate::Text]);
        assert_eq!(stream.text, "Hel");
        assert_eq!(stream.thinking, "plan");
        updates = stream.feed("ta\":\"lo <goal_continue/>\"}\n{\"type\":\"end\",\"stopReason\":\"EndTurn\",\"sessionId\":\"abc123\",\"requestId\":\"r1\"}\n");
        assert_eq!(
            updates,
            vec![
                StreamUpdate::Text,
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
        assert_eq!(stream.finish(), vec![StreamUpdate::Text]);
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
        mission.backend = "opencode".into();
        let other = local_resume_refusal(&mission, &placement);
        assert!(other.contains("supersedes_mission_id"), "{other}");
        assert!(other.contains("'opencode'"), "{other}");
    }
}
