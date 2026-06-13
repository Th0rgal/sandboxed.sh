//! Ask assistant — a fast, read-mostly sidecar co-pilot that rides alongside a
//! running mission. It can read the mission history and run bash in the live
//! workspace, answering operator questions **without** touching the working
//! agent's turn loop, queue, or harness lock.
//!
//! This module hosts:
//! - the persistent [`store`] (separate `ask.db`, never merged into mission
//!   history),
//! - the OpenAI-compatible tool-calling [`client`],
//! - the in-process agentic [`run_ask_turn`] loop, and
//! - the global [`ask_store`] accessor.
//!
//! See `ASK_ASSISTANT_DESIGN.md` for the full design.

pub mod client;
pub mod http;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::api::mission_store::MissionStore;
use crate::api::proxy_keys::SharedProxyApiKeyStore;
use crate::workspace::SharedWorkspaceStore;
use crate::workspace_exec::WorkspaceExec;

pub use client::AskClient;
pub use store::{AskMessage, AskStore, AskThread, OperatorNote};

/// Hard cap on tool-calling iterations per turn — keeps cost/latency bounded.
/// When the cap is hit mid-investigation, a final tools-disabled pass still
/// forces a synthesized answer (see the loops below), so this bounds *tool*
/// rounds rather than guaranteeing a dead end.
const MAX_ITERATIONS: usize = 10;
/// Max bytes of any single tool result fed back to the model.
const MAX_TOOL_RESULT_BYTES: usize = 8_000;
/// Number of recent mission events seeded into the system prompt.
const SEED_EVENT_COUNT: usize = 40;
/// How far back to scan when looking for the latest goal_status event. One
/// fetch serves both this scan and the [`SEED_EVENT_COUNT`] seed.
const GOAL_SCAN_EVENT_COUNT: usize = 200;

static ASK_STORE: OnceCell<Arc<AskStore>> = OnceCell::const_new();

/// Get (lazily initializing) the global Ask store, placed next to the mission
/// databases under the working directory.
pub async fn ask_store(config: &crate::config::Config) -> Result<Arc<AskStore>, String> {
    ASK_STORE
        .get_or_try_init(|| async {
            let base = config.working_dir.join(".sandboxed-sh").join("missions");
            tokio::fs::create_dir_all(&base)
                .await
                .map_err(|e| e.to_string())?;
            AskStore::open(base.join("ask.db")).await.map(Arc::new)
        })
        .await
        .map(Arc::clone)
}

/// Non-initializing accessor for runners: returns the store only if some Ask
/// interaction already opened it this process lifetime. Runners use this for
/// best-effort mid-turn note injection without needing a `Config`.
pub fn ask_store_if_initialized() -> Option<Arc<AskStore>> {
    ASK_STORE.get().cloned()
}

/// Everything the Ask loop needs for one turn.
pub struct AskTurn {
    pub ask_store: Arc<AskStore>,
    pub mission_store: Arc<dyn MissionStore>,
    pub workspace_exec: WorkspaceExec,
    pub work_dir: PathBuf,
    pub llm: AskClient,
    pub mission_id: Uuid,
    pub thread_id: Uuid,
    /// When true, `work_dir` is an isolated sandbox copy of the workspace, so
    /// writes are throwaway and we skip the operator-note bridge entirely.
    pub sandbox: bool,
    /// Workspace store + the mission's workspace id, for the workspace-env
    /// tools (list/set env vars that get injected into the harness).
    pub workspaces: SharedWorkspaceStore,
    pub workspace_id: Uuid,
    /// Proxy API key store, for answering "is the key the operator issued
    /// actually being used?" with `last_used_at` facts.
    pub proxy_keys: SharedProxyApiKeyStore,
    /// Command channel into the user's control session, for the steering tools
    /// (`stop_agent` / `send_to_agent`). Same channel the dashboard's Stop
    /// button and composer use.
    pub control_cmd_tx: tokio::sync::mpsc::Sender<crate::api::control::ControlCommand>,
    /// Event broadcast for the control session. Events sent here reach live
    /// viewers AND the persistent event logger — used to leave a durable audit
    /// record when the Copilot stops the working agent.
    pub events_tx: tokio::sync::broadcast::Sender<crate::api::control::AgentEvent>,
}

/// Run one Ask turn: persist the operator message, drive the tool loop, persist
/// the assistant answer, and return the final text.
pub async fn run_ask_turn(turn: &AskTurn, user_content: &str) -> Result<String, String> {
    // Persist the operator's message first.
    turn.ask_store
        .append_message(turn.thread_id, "user", user_content, None, None, None)
        .await?;

    // Assemble the OpenAI message array: system + prior turns + this message.
    let system = build_system_prompt(turn, user_content).await;
    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system })];

    // Replay prior user/assistant turns for continuity (tool details are kept in
    // the store for the UI but not replayed to the model — the final answers
    // carry what matters). `prior` already includes the message just appended.
    let prior = turn.ask_store.list_messages(turn.thread_id).await?;
    for m in &prior {
        match m.role.as_str() {
            "user" => messages.push(json!({ "role": "user", "content": m.content })),
            "assistant" => messages.push(json!({ "role": "assistant", "content": m.content })),
            _ => {}
        }
    }

    let tools = tool_definitions();
    let mut final_answer = String::new();
    let mut total_tokens: u64 = 0;

    for _ in 0..MAX_ITERATIONS {
        let completion = turn.llm.complete(&messages, &tools).await?;
        total_tokens += completion.total_tokens.unwrap_or(0);

        if completion.tool_calls.is_empty() {
            final_answer = completion.content.unwrap_or_default();
            break;
        }

        // Reflect the assistant's tool-calling turn into the live context.
        let assistant_tool_calls: Vec<Value> = completion
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments },
                })
            })
            .collect();
        messages.push(json!({
            "role": "assistant",
            "content": completion.content.clone().unwrap_or_default(),
            "tool_calls": assistant_tool_calls,
        }));

        for tc in &completion.tool_calls {
            turn.ask_store
                .append_message(
                    turn.thread_id,
                    "tool_call",
                    &tc.arguments,
                    Some(tc.name.clone()),
                    Some(tc.id.clone()),
                    None,
                )
                .await?;

            let result = execute_tool(turn, &tc.name, &tc.arguments).await;
            let result = truncate_tool_result(&result);

            turn.ask_store
                .append_message(
                    turn.thread_id,
                    "tool_result",
                    &result,
                    Some(tc.name.clone()),
                    Some(tc.id.clone()),
                    None,
                )
                .await?;

            messages.push(json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result,
            }));
        }
    }

    if final_answer.is_empty() {
        // The loop ended on a tool-calling turn — the model never volunteered a
        // final answer. Give it one more pass with tools disabled so it must
        // synthesize from the results it already gathered, rather than bailing
        // with a canned "tool-call limit" message.
        match turn.llm.complete(&messages, &[]).await {
            Ok(c) => {
                total_tokens += c.total_tokens.unwrap_or(0);
                final_answer = c.content.unwrap_or_default();
            }
            Err(e) => tracing::warn!("[Ask] forced final-answer pass failed: {e}"),
        }
        if final_answer.is_empty() {
            final_answer =
                "(The assistant reached the tool-call limit without a final answer.)".to_string();
        }
    }

    // Reasoning models (MiniMax, GLM) can leak <think>/<mm:think> blocks into
    // the visible answer; strip them like the mission path does.
    final_answer = crate::api::mission_runner::strip_think_tags(&final_answer)
        .trim()
        .to_string();

    turn.ask_store
        .append_message(
            turn.thread_id,
            "assistant",
            &final_answer,
            None,
            None,
            Some(json!({ "model": turn.llm.model(), "total_tokens": total_tokens })),
        )
        .await?;

    Ok(final_answer)
}

/// An incremental event from the streaming Ask loop, serialized into SSE.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AskStreamEvent {
    /// A fragment of the assistant's visible answer.
    Delta { content: String },
    /// The assistant invoked a tool.
    ToolCall {
        tool_call_id: String,
        name: String,
        args: String,
    },
    /// A tool returned a result.
    ToolResult {
        tool_call_id: String,
        name: String,
        result: String,
    },
    /// Terminal: the turn finished. Carries the thread id (new threads) and the
    /// final answer so the client can reconcile.
    Done { thread_id: Uuid, answer: String },
    /// Terminal error.
    Error { message: String },
}

/// Streaming variant of [`run_ask_turn`]: drives the same agentic loop but emits
/// [`AskStreamEvent`]s on `tx` as tokens, tool calls, and results arrive.
/// Returns `true` if the turn completed successfully (a `done` event was sent),
/// `false` if it failed (an `error` event was sent instead).
pub async fn run_ask_turn_streaming(
    turn: &AskTurn,
    user_content: &str,
    tx: UnboundedSender<AskStreamEvent>,
) -> bool {
    match run_ask_turn_streaming_inner(turn, user_content, &tx).await {
        Ok(()) => true,
        Err(e) => {
            let _ = tx.send(AskStreamEvent::Error { message: e });
            false
        }
    }
}

async fn run_ask_turn_streaming_inner(
    turn: &AskTurn,
    user_content: &str,
    tx: &UnboundedSender<AskStreamEvent>,
) -> Result<(), String> {
    turn.ask_store
        .append_message(turn.thread_id, "user", user_content, None, None, None)
        .await?;

    let system = build_system_prompt(turn, user_content).await;
    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system })];
    let prior = turn.ask_store.list_messages(turn.thread_id).await?;
    for m in &prior {
        match m.role.as_str() {
            "user" => messages.push(json!({ "role": "user", "content": m.content })),
            "assistant" => messages.push(json!({ "role": "assistant", "content": m.content })),
            _ => {}
        }
    }

    let tools = tool_definitions();
    let mut final_answer = String::new();
    let mut total_tokens: u64 = 0;

    for _ in 0..MAX_ITERATIONS {
        let txc = tx.clone();
        let completion = turn
            .llm
            .complete_stream(&messages, &tools, |frag| {
                let _ = txc.send(AskStreamEvent::Delta {
                    content: frag.to_string(),
                });
            })
            .await?;
        total_tokens += completion.total_tokens.unwrap_or(0);

        if completion.tool_calls.is_empty() {
            final_answer = completion.content.unwrap_or_default();
            break;
        }

        let assistant_tool_calls: Vec<Value> = completion
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments },
                })
            })
            .collect();
        messages.push(json!({
            "role": "assistant",
            "content": completion.content.clone().unwrap_or_default(),
            "tool_calls": assistant_tool_calls,
        }));

        for tc in &completion.tool_calls {
            let _ = tx.send(AskStreamEvent::ToolCall {
                tool_call_id: tc.id.clone(),
                name: tc.name.clone(),
                args: tc.arguments.clone(),
            });
            turn.ask_store
                .append_message(
                    turn.thread_id,
                    "tool_call",
                    &tc.arguments,
                    Some(tc.name.clone()),
                    Some(tc.id.clone()),
                    None,
                )
                .await?;

            let result = execute_tool(turn, &tc.name, &tc.arguments).await;
            let result = truncate_tool_result(&result);
            let _ = tx.send(AskStreamEvent::ToolResult {
                tool_call_id: tc.id.clone(),
                name: tc.name.clone(),
                result: result.clone(),
            });
            turn.ask_store
                .append_message(
                    turn.thread_id,
                    "tool_result",
                    &result,
                    Some(tc.name.clone()),
                    Some(tc.id.clone()),
                    None,
                )
                .await?;
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result,
            }));
        }
    }

    if final_answer.is_empty() {
        // Same forced synthesis as the non-streaming path: one more pass with
        // tools disabled, streamed so the operator sees the answer arrive.
        let txc = tx.clone();
        match turn
            .llm
            .complete_stream(&messages, &[], |frag| {
                let _ = txc.send(AskStreamEvent::Delta {
                    content: frag.to_string(),
                });
            })
            .await
        {
            Ok(c) => {
                total_tokens += c.total_tokens.unwrap_or(0);
                final_answer = c.content.unwrap_or_default();
            }
            Err(e) => tracing::warn!("[Ask] forced final-answer pass (stream) failed: {e}"),
        }
        if final_answer.is_empty() {
            final_answer =
                "(The assistant reached the tool-call limit without a final answer.)".to_string();
        }
    }

    // Reasoning models (MiniMax, GLM) can leak <think>/<mm:think> blocks into
    // the visible answer; strip them like the mission path does.
    final_answer = crate::api::mission_runner::strip_think_tags(&final_answer)
        .trim()
        .to_string();

    turn.ask_store
        .append_message(
            turn.thread_id,
            "assistant",
            &final_answer,
            None,
            None,
            Some(json!({ "model": turn.llm.model(), "total_tokens": total_tokens })),
        )
        .await?;

    let _ = tx.send(AskStreamEvent::Done {
        thread_id: turn.thread_id,
        answer: final_answer,
    });
    Ok(())
}

async fn build_system_prompt(turn: &AskTurn, user_content: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut seed = String::new();
    if let Ok(Some(mission)) = turn.mission_store.get_mission(turn.mission_id).await {
        if let Some(title) = &mission.title {
            seed.push_str(&format!("Mission title: {title}\n"));
        }
        seed.push_str(&format!(
            "Mission status: {:?} (last updated {})\n",
            mission.status, mission.updated_at
        ));
        if let Some(desc) = &mission.short_description {
            seed.push_str(&format!("Mission summary: {desc}\n"));
        }
        seed.push_str(&format!("Mission backend: {}\n", mission.backend));
    }

    // One slightly deeper fetch serves both the goal-status scan and the seed.
    if let Ok(events) = turn
        .mission_store
        .get_latest_events(turn.mission_id, GOAL_SCAN_EVENT_COUNT)
        .await
    {
        if let Some(goal) = events
            .iter()
            .rev()
            .find(|ev| ev.event_type == "goal_status")
        {
            seed.push_str(&format!(
                "\nLatest goal status [{} {}]: {}\n",
                goal.sequence,
                goal.timestamp,
                truncate(&goal.content, 600)
            ));
        }
        // Latest plan snapshot (Claude Code TodoWrite / Codex update_plan /
        // OpenCode todowrite): the working agent's own task board is the
        // single best "where are we" signal — surface the newest one.
        if let Some(plan) = events.iter().rev().find(|ev| {
            ev.event_type == "tool_call"
                && matches!(
                    ev.tool_name.as_deref(),
                    Some("TodoWrite") | Some("update_plan") | Some("todowrite")
                )
        }) {
            seed.push_str(&format!(
                "\nWorking agent's latest task board [{} {}]:\n{}\n",
                plan.sequence,
                plan.timestamp,
                truncate(&plan.content, 1500)
            ));
        }
        if let Some(iter_ev) = events
            .iter()
            .rev()
            .find(|ev| ev.event_type == "goal_iteration")
        {
            seed.push_str(&format!(
                "\nLatest loop/wakeup marker [{} {}]: {}\n",
                iter_ev.sequence,
                iter_ev.timestamp,
                truncate(&iter_ev.content, 400)
            ));
        }
        let start = events.len().saturating_sub(SEED_EVENT_COUNT);
        seed.push_str("\nRecent mission events (newest last):\n");
        for ev in &events[start..] {
            let tool = ev
                .tool_name
                .as_deref()
                .map(|t| format!(" {t}"))
                .unwrap_or_default();
            seed.push_str(&format!(
                "[{} {}] {}{}: {}\n",
                ev.sequence,
                ev.timestamp,
                ev.event_type,
                tool,
                truncate(&ev.content, 300)
            ));
        }
    }

    // Worker fleet: for orchestrator missions, list child missions so the
    // co-pilot can answer "what is running right now" without grepping.
    if let Ok(children) = turn.mission_store.get_child_missions(turn.mission_id).await {
        if !children.is_empty() {
            seed.push_str("\nWorker missions (children):\n");
            for c in children.iter().rev().take(12) {
                seed.push_str(&format!(
                    "- {} [{} {}] {:?} {}\n",
                    c.id,
                    c.backend,
                    c.model_override.as_deref().unwrap_or("-"),
                    c.status,
                    truncate(c.title.as_deref().unwrap_or("untitled"), 80)
                ));
            }
        }
    }
    // Pending wakeups: when the next scheduled turn fires and why.
    if let Ok(autos) = turn
        .mission_store
        .get_mission_automations(turn.mission_id)
        .await
    {
        for a in autos.iter().filter(|a| a.active) {
            seed.push_str(&format!(
                "\nPending automation: trigger={:?} last_triggered={:?}\n",
                a.trigger, a.last_triggered_at
            ));
        }
    }

    let secret_note = if message_mentions_secret(user_content) {
        "\n\n⚠ The operator's latest message contains what looks like a credential \
         (API key / token). Persist it NOW with set_workspace_env under the variable \
         name the mission's tooling expects (check its scripts/docs, or ask), then \
         refer to it only by variable name. Never echo the full value back: chat \
         context does not survive compaction, workspace env vars do."
    } else {
        ""
    };

    let cwd = turn.work_dir.display();
    format!(
        "You are the Ask co-pilot for an autonomous coding mission. A separate \
         \"working agent\" is doing the real work in this same workspace; you are a \
         read-mostly assistant helping the operator understand and occasionally nudge \
         what's happening.\n\n\
         Current UTC time: {now}. Mission events and files carry timestamps — compare \
         them against the current time and date your claims (\"as of 14:32Z\"); never \
         present a snapshot from hours ago as the present state.\n\n\
         Recency first: when asked where the mission stands, check the newest data \
         before summarizing — `ls -lt` on results/output dirs, `tail` on logs and \
         journals, `git log --oneline -10`, and read_history. Research logs and \
         journals are append-only: the END of the file is the current state, the \
         beginning is history.\n\n\
         Tool results are capped at 8KB; a truncation notice reports the full size. \
         For big files use read_file with offset/limit or tail/sed line ranges — \
         never assume you saw the whole file.\n\n\
         Your bash tool runs in `{cwd}` — this is the mission's workspace and the \
         working agent's project root. Commands start there and relative paths \
         resolve against it, so you can `ls`, `cat`, or `git log` directly without \
         hunting for the project first.\n\n\
         Prefer reading (history, files, logs) over writing. You MAY write to the \
         workspace with the bash tool, but anything you change is reported to the \
         working agent, so keep writes minimal and intentional. The full mission \
         history may be large — retrieve what you need with read_history / bash \
         (grep, rg, cat) rather than assuming it is all provided.\n\n\
         Secrets: if the operator shares a credential, persist it immediately with \
         set_workspace_env — workspace env vars are injected into the working \
         agent's process environment from its next turn onward and survive restarts \
         and context compaction, unlike anything pasted in chat. Confirm by variable \
         name and never repeat the value. Use list_workspace_env and \
         proxy_key_status to answer \"is the key set / is it being used?\" with \
         facts instead of guesses.\n\n\
         Steering authority: you can stop the working agent (stop_agent) and send \
         it steering messages (send_to_agent) — the same controls the operator has. \
         These are interventions, not observations: use them when the operator asks \
         you to stop/steer/redirect the agent, or when it is burning resources in a \
         clearly harmful loop. Otherwise, propose the steering message and let the \
         operator decide. When you do steer, make the message self-contained and \
         bounded (what to stop, what to do instead, when to stop doing it) — the \
         working agent has no access to this conversation.\n\n\
         You are strictly reactive: you run only when the operator sends a \
         message, and you cannot watch, poll, or follow up on your own. Never \
         promise to \"keep an eye on\" or \"let you know when\" something \
         happens — instead tell the operator what to ask about next time, or \
         queue a steering note for the working agent.\n\n\
         Be concise and concrete. Cite event sequence numbers or file paths when \
         relevant. When you identify a blocker the operator could fix through \
         configuration (missing env var, unused key), propose the concrete fix — \
         and apply it with your tools when the operator asks.{secret_note}\n\n\
         === Mission context ===\n{seed}"
    )
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a bash command in the mission's live workspace. Full read+write. Use for grep/rg/cat/ls/git status/git log to inspect the current state.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The bash command to run." }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_history",
                "description": "Fetch the most recent mission events (the working agent's transcript: messages, tool calls/results, errors). Each event carries its timestamp.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "How many recent events (default 60, max 300)." }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file from the mission workspace by path. Always reports total lines/bytes first. For big files pass offset/limit to read a line range (e.g. the tail of a long log).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (absolute or relative to the workspace root)." },
                        "offset": { "type": "integer", "description": "1-based line number to start reading from (omit to start at the top)." },
                        "limit": { "type": "integer", "description": "Number of lines to read (omit to read to the end)." }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_workspace_env",
                "description": "List the workspace environment variables injected into the working agent's process environment (names + masked values). Use to check whether a credential or config var is already set.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "set_workspace_env",
                "description": "Persist a workspace environment variable. It is injected into the working agent's environment from its next turn onward and survives restarts — the durable channel for credentials the operator shares in chat. The working agent is notified by name (never by value).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "POSIX env var name (e.g. DEFAULT_HARNESS_API_KEY)." },
                        "value": { "type": "string", "description": "The value to store." }
                    },
                    "required": ["name", "value"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "proxy_key_status",
                "description": "List the gateway's proxy API keys (name, prefix, created_at, last_used_at). Use to answer whether a key the operator issued is valid/actually being used.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "stop_agent",
                "description": "Interrupt the working agent's current turn (same as the operator's Stop button). The mission becomes interrupted/awaiting until resumed or steered. Use only when the operator asked you to stop it, or when it is clearly stuck in a harmful loop.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "reason": { "type": "string", "description": "One-line reason, recorded for the operator." }
                    },
                    "required": ["reason"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "send_to_agent",
                "description": "Send a steering message to the working agent, exactly as if the operator typed it in the mission composer. If the agent is mid-turn the message is queued and picked up at the next turn boundary — set interrupt=true to cancel the current turn first so it takes effect immediately. If the agent is idle this STARTS a new turn. Use only when the operator asked you to steer/redirect the agent.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string", "description": "The steering instructions for the working agent. Be specific: what to stop doing, what to do instead, and any bounds." },
                        "interrupt": { "type": "boolean", "description": "Cancel the agent's current turn before delivering, so the steer applies now instead of after the turn ends. Default false." }
                    },
                    "required": ["message"]
                }
            }
        }),
    ]
}

async fn execute_tool(turn: &AskTurn, name: &str, arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    match name {
        "bash" => {
            let cmd = args["command"].as_str().unwrap_or("").to_string();
            if cmd.trim().is_empty() {
                return "Error: empty command".to_string();
            }
            // In sandbox mode the writes land in a throwaway copy, so there's
            // nothing to report to the working agent. Otherwise snapshot the
            // working tree before/after for the operator-note bridge.
            if turn.sandbox {
                return run_bash(turn, &cmd).await;
            }
            let baseline = capture_write_baseline(turn).await;
            let result = run_bash(turn, &cmd).await;
            record_writes(turn, &cmd, baseline).await;
            result
        }
        "read_file" => {
            let path = args["path"].as_str().unwrap_or("");
            if path.trim().is_empty() {
                return "Error: empty path".to_string();
            }
            let offset = args["offset"].as_u64().map(|v| v.max(1));
            let limit = args["limit"].as_u64().filter(|v| *v > 0);
            let read_cmd = match (offset, limit) {
                (Some(o), Some(l)) => {
                    format!("sed -n '{o},{}p' -- \"$f\"", o.saturating_add(l - 1))
                }
                (Some(o), None) => format!("tail -n +{o} -- \"$f\""),
                (None, Some(l)) => format!("head -n {l} -- \"$f\""),
                (None, None) => "cat -- \"$f\"".to_string(),
            };
            // Lead with the file's true size so the model knows what fraction
            // it is seeing (the 8KB tool-result cap truncates silently otherwise).
            let cmd = format!(
                "f={path}; if [ ! -f \"$f\" ]; then echo \"Error: no such file: $f\" >&2; exit 1; fi; \
                 printf '[%s: %s lines, %s bytes total]\\n' \"$f\" \"$(wc -l < \"$f\")\" \"$(wc -c < \"$f\")\"; {read_cmd}",
                path = single_quote(path),
            );
            run_bash(turn, &cmd).await
        }
        "read_history" => {
            let limit = args["limit"].as_u64().unwrap_or(60).clamp(1, 300) as usize;
            match turn
                .mission_store
                .get_latest_events(turn.mission_id, limit)
                .await
            {
                Ok(events) => {
                    let mut out = String::new();
                    for ev in &events {
                        let tool = ev
                            .tool_name
                            .as_deref()
                            .map(|t| format!(" {t}"))
                            .unwrap_or_default();
                        out.push_str(&format!(
                            "[{} {}] {}{}: {}\n",
                            ev.sequence,
                            ev.timestamp,
                            ev.event_type,
                            tool,
                            truncate(&ev.content, 400)
                        ));
                    }
                    if out.is_empty() {
                        "(no events)".to_string()
                    } else {
                        out
                    }
                }
                Err(e) => format!("Error reading history: {e}"),
            }
        }
        "list_workspace_env" => match turn.workspaces.get(turn.workspace_id).await {
            Some(ws) => {
                if ws.env_vars.is_empty() {
                    "(no workspace env vars configured)".to_string()
                } else {
                    let mut entries: Vec<_> = ws.env_vars.iter().collect();
                    entries.sort_by(|a, b| a.0.cmp(b.0));
                    entries
                        .into_iter()
                        .map(|(k, v)| format!("{k} = {}", mask_value(v)))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            None => "Error: workspace not found".to_string(),
        },
        "set_workspace_env" => {
            if turn.sandbox {
                return "Error: set_workspace_env is disabled in sandbox mode (it would \
                        change the live workspace configuration)."
                    .to_string();
            }
            let env_name = args["name"].as_str().unwrap_or("").trim().to_string();
            let value = args["value"].as_str().unwrap_or("").to_string();
            if env_name.is_empty() || value.is_empty() {
                return "Error: name and value are required".to_string();
            }
            if !crate::api::workspaces::is_valid_env_name(&env_name) {
                return format!(
                    "Error: '{env_name}' is not a valid POSIX env var name \
                     (letters, digits, underscores; cannot start with a digit)"
                );
            }
            if value.contains('\0') {
                return "Error: value contains a NUL byte".to_string();
            }
            match turn.workspaces.get(turn.workspace_id).await {
                Some(mut ws) => {
                    let replaced = ws
                        .env_vars
                        .insert(env_name.clone(), value.clone())
                        .is_some();
                    if !turn.workspaces.update(ws).await {
                        return "Error: failed to persist the workspace env var".to_string();
                    }
                    // Tell the working agent the variable exists — by name only,
                    // so the value never lands in mission history.
                    let note = format!(
                        "The operator configured the workspace environment variable \
                         `{env_name}` via the Ask assistant (value withheld). It is \
                         injected into your process environment from your next turn \
                         onward — read it from the environment by name instead of \
                         asking for the value or inlining it in commands."
                    );
                    if let Err(e) = turn
                        .ask_store
                        .enqueue_operator_note(turn.mission_id, &note, Some(turn.thread_id))
                        .await
                    {
                        tracing::warn!("[Ask] failed to enqueue env-var operator note: {e}");
                    }
                    format!(
                        "{} workspace env var `{env_name}` ({} chars). It is injected into \
                         the working agent's environment from its next turn onward; an \
                         operator note was queued so the agent knows it is available.",
                        if replaced { "Updated" } else { "Set" },
                        value.chars().count()
                    )
                }
                None => "Error: workspace not found".to_string(),
            }
        }
        "stop_agent" => {
            let reason = args["reason"].as_str().unwrap_or("").trim().to_string();
            if reason.is_empty() {
                return "Error: a reason is required".to_string();
            }
            match cancel_working_agent(turn).await {
                Ok(()) => {
                    record_copilot_stop(turn, &reason).await;
                    format!(
                        "Interrupted the working agent's current turn (reason: {reason}). \
                         The reason was recorded in the mission transcript. The mission \
                         will settle as interrupted/awaiting; use send_to_agent to \
                         redirect it, or the operator can resume it from the dashboard."
                    )
                }
                Err(e) => format!("Error stopping the agent: {e}"),
            }
        }
        "send_to_agent" => {
            let message = args["message"].as_str().unwrap_or("").trim().to_string();
            if message.is_empty() {
                return "Error: message is empty".to_string();
            }
            let interrupt = args["interrupt"].as_bool().unwrap_or(false);
            // Track whether the requested interrupt actually landed — a failed
            // cancel (timeout, control error) must not be reported as "delivered
            // after interrupting" when the agent may still be mid-turn.
            let mut interrupt_error: Option<String> = None;
            if interrupt {
                if let Err(e) = cancel_working_agent(turn).await {
                    tracing::info!(
                        mission_id = %turn.mission_id,
                        "[Ask] interrupt before steer did not cancel anything: {e}"
                    );
                    interrupt_error = Some(e);
                } else {
                    record_copilot_stop(turn, &format!("steering: {message}")).await;
                }
            }
            let content = format_steer_message(&message);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let send = turn
                .control_cmd_tx
                .send(crate::api::control::ControlCommand::UserMessage {
                    id: Uuid::new_v4(),
                    content,
                    agent: None,
                    target_mission_id: Some(turn.mission_id),
                    strict: false,
                    respond: tx,
                })
                .await;
            if send.is_err() {
                return "Error: the control session is unavailable".to_string();
            }
            use crate::api::control::UserMessageAck;
            match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
                Ok(Ok(UserMessageAck::Queued)) => match (interrupt, &interrupt_error) {
                    (true, None) => {
                        "Steering message delivered after interrupting the current turn — \
                         the agent will act on it as soon as the cancellation settles."
                            .to_string()
                    }
                    (true, Some(err)) => format!(
                        "Steering message queued, but the requested interrupt FAILED \
                         ({err}) — the agent may still be mid-turn and will only act on \
                         the message at the next turn boundary. Verify with read_history; \
                         retry stop_agent if it must stop now."
                    ),
                    (false, _) => {
                        "Steering message queued — the working agent is mid-turn and will \
                         act on it at the next turn boundary. Pass interrupt=true if it \
                         must take effect immediately."
                            .to_string()
                    }
                },
                Ok(Ok(UserMessageAck::Delivered)) => {
                    "Steering message delivered — a turn is starting on it now.".to_string()
                }
                Ok(Ok(UserMessageAck::Dropped)) => {
                    "Error: the steering message was DROPPED — it never reached the \
                     working agent (parallel mission cap, mission load failure, or a \
                     rejected goal kickoff). Check read_history for the error event, \
                     resolve the cause, then retry."
                        .to_string()
                }
                Ok(Err(_)) | Err(_) => {
                    // The control loop accepted the command but never confirmed —
                    // the message is most likely in flight; don't claim failure.
                    "Steering message submitted (no delivery confirmation received). \
                     Check read_history shortly to confirm the agent saw it."
                        .to_string()
                }
            }
        }
        "proxy_key_status" => {
            let keys = turn.proxy_keys.list().await;
            if keys.is_empty() {
                "(no proxy API keys)".to_string()
            } else {
                keys.iter()
                    .map(|k| {
                        format!(
                            "{} ({}…) created {} — last used: {}",
                            k.name,
                            k.key_prefix,
                            k.created_at
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            k.last_used_at
                                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                                .unwrap_or_else(|| "never".to_string())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        other => format!("Error: unknown tool '{other}'"),
    }
}

/// Cancel the working agent's current turn through the control session — the
/// same `CancelMission` path the dashboard's Stop button uses.
async fn cancel_working_agent(turn: &AskTurn) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    turn.control_cmd_tx
        .send(crate::api::control::ControlCommand::CancelMission {
            mission_id: turn.mission_id,
            min_idle: None,
            respond: tx,
        })
        .await
        .map_err(|_| "the control session is unavailable".to_string())?;
    match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("the control session dropped the request".to_string()),
        Err(_) => Err("timed out waiting for the cancellation to be acknowledged".to_string()),
    }
}

/// Leave a durable audit record of a Copilot-initiated stop in the mission
/// transcript. Broadcast on `events_tx` so live viewers see it AND the
/// persistent event logger writes it to mission events.
async fn record_copilot_stop(turn: &AskTurn, reason: &str) {
    let _ = turn.events_tx.send(crate::api::control::AgentEvent::Error {
        message: format!(
            "⏹ The Copilot stopped the working agent's turn (operator-authorized). \
                 Reason: {}",
            truncate(reason, 400)
        ),
        mission_id: Some(turn.mission_id),
        resumable: true,
    });
}

/// Wrap a Copilot steering message so the working agent (and the mission
/// transcript) can tell it came through the co-pilot acting on the operator's
/// behalf, not from the operator typing directly.
fn format_steer_message(message: &str) -> String {
    format!("[Steering from the operator via the Copilot]\n{message}")
}

/// Mask an env var value for display: short prefix + length, never the value.
fn mask_value(v: &str) -> String {
    let prefix: String = v.chars().take(4).collect();
    format!("{prefix}… ({} chars)", v.chars().count())
}

async fn run_bash(turn: &AskTurn, command: &str) -> String {
    let args = vec!["-lc".to_string(), command.to_string()];
    match turn
        .workspace_exec
        .output(&turn.work_dir, "/bin/bash", &args, HashMap::new())
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut out = String::new();
            if !stdout.trim().is_empty() {
                out.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[stderr] ");
                out.push_str(stderr.trim_end());
            }
            let code = output.status.code().unwrap_or(-1);
            if code != 0 {
                out.push_str(&format!("\n[exit code {code}]"));
            }
            if out.is_empty() {
                "(no output)".to_string()
            } else {
                out
            }
        }
        Err(e) => format!("Error running command: {e}"),
    }
}

/// Snapshot `git status --porcelain` (tracked + untracked) as a set of lines.
/// Returns `None` when the workspace is not a git repo (no detection possible).
async fn git_status_set(turn: &AskTurn) -> Option<HashSet<String>> {
    let args = vec![
        "-lc".to_string(),
        "git status --porcelain=v1 --untracked-files=all 2>/dev/null".to_string(),
    ];
    let output = turn
        .workspace_exec
        .output(&turn.work_dir, "/bin/bash", &args, HashMap::new())
        .await
        .ok()?;
    if !output.status.success() {
        return None; // not a git repo (or git unavailable)
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().map(|l| l.to_string()).collect())
}

/// Baseline of the working tree captured before an Ask bash command, used to
/// detect writes afterwards. Git workspaces use a porcelain snapshot; non-git
/// workspaces fall back to an mtime cutoff.
enum WriteBaseline {
    Git(HashSet<String>),
    /// Epoch seconds, for `find -newermt @epoch` detection.
    Mtime(String),
    None,
}

async fn capture_write_baseline(turn: &AskTurn) -> WriteBaseline {
    if let Some(set) = git_status_set(turn).await {
        return WriteBaseline::Git(set);
    }
    // Non-git fallback: capture an epoch marker for mtime-based detection.
    let args = vec!["-lc".to_string(), "date +%s".to_string()];
    if let Ok(out) = turn
        .workspace_exec
        .output(&turn.work_dir, "/bin/bash", &args, HashMap::new())
        .await
    {
        let epoch = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !epoch.is_empty() && epoch.chars().all(|c| c.is_ascii_digit()) {
            return WriteBaseline::Mtime(epoch);
        }
    }
    WriteBaseline::None
}

/// After an Ask bash command, diff the working tree against the `baseline` and
/// enqueue an operator-note describing any new/changed paths so the working
/// agent learns about edits it didn't make.
async fn record_writes(turn: &AskTurn, command: &str, baseline: WriteBaseline) {
    let mut changed: Vec<String> = match baseline {
        WriteBaseline::Git(before) => {
            let Some(after) = git_status_set(turn).await else {
                return;
            };
            // New porcelain lines that weren't present before — predominantly
            // this command's writes (the window is a single command).
            after
                .difference(&before)
                .map(|l| l.trim().to_string())
                .collect()
        }
        WriteBaseline::Mtime(epoch) => {
            let cmd = format!(
                "find . -type f -newermt @{epoch} \
                 -not -path './.git/*' -not -path './node_modules/*' \
                 -not -path './target/*' 2>/dev/null | head -50"
            );
            let args = vec!["-lc".to_string(), cmd];
            match turn
                .workspace_exec
                .output(&turn.work_dir, "/bin/bash", &args, HashMap::new())
                .await
            {
                Ok(out) => String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect(),
                Err(_) => return,
            }
        }
        WriteBaseline::None => return,
    };

    if changed.is_empty() {
        return;
    }
    changed.sort();
    changed.dedup();

    let body = format!(
        "While you were working, the operator made out-of-band changes to this \
         workspace via the Ask assistant (not by you), running `{}`:\n{}\n\
         If relevant, re-read these files before continuing; do not assume your \
         previous view of them is current.",
        truncate(command, 200),
        changed
            .iter()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    if let Err(e) = turn
        .ask_store
        .enqueue_operator_note(turn.mission_id, &body, Some(turn.thread_id))
        .await
    {
        tracing::warn!("[Ask] failed to enqueue operator note: {e}");
    }
}

/// Flush any pending operator-notes for a mission into the working agent's next
/// turn by prepending a tagged block to its `user_message`. Returns the message
/// unchanged when there are no pending notes.
///
/// This is the **delivery** half of the operator-note bridge, called from each
/// backend's turn-prep (see `mission_runner`). Notes are passive: this only ever
/// runs when a turn is already about to execute — it never starts one.
pub async fn prepend_pending_operator_notes(
    store: &AskStore,
    mission_id: Uuid,
    user_message: String,
) -> (String, usize) {
    let notes = match store.take_pending_operator_notes(mission_id).await {
        Ok(n) if !n.is_empty() => n,
        _ => return (user_message, 0),
    };
    let count = notes.len();
    let mut block = String::from("<operator-note>\n");
    for note in &notes {
        block.push_str(&note.body);
        block.push('\n');
    }
    block.push_str("</operator-note>\n\n");
    block.push_str(&user_message);
    (block, count)
}

/// Prepare an isolated sandbox copy of the workspace for "Ask in isolated copy"
/// mode, using a detached git worktree (cheap, shares history, no full copy).
/// Returns the sandbox path, or `None` if the workspace isn't a git repo (we
/// deliberately do NOT `cp -a` a possibly-huge non-git tree). The caller treats
/// `None` for an explicit sandbox request as an error rather than silently
/// running against the live tree.
pub async fn prepare_sandbox(exec: &WorkspaceExec, base_work_dir: &Path) -> Option<PathBuf> {
    let sandbox = PathBuf::from(format!("/tmp/ask-sandbox-{}", Uuid::new_v4()));
    let base_str = base_work_dir.to_string_lossy().to_string();
    let sandbox_str = sandbox.to_string_lossy().to_string();
    let cmd = format!(
        "git -C {b} rev-parse --git-dir >/dev/null 2>&1 && \
         git -C {b} worktree add --detach {s} HEAD >/dev/null 2>&1 && \
         echo SANDBOX_OK",
        b = single_quote(&base_str),
        s = single_quote(&sandbox_str)
    );
    let args = vec!["-lc".to_string(), cmd];
    match exec
        .output(base_work_dir, "/bin/bash", &args, HashMap::new())
        .await
    {
        Ok(out)
            if out.status.success()
                && String::from_utf8_lossy(&out.stdout).contains("SANDBOX_OK") =>
        {
            Some(sandbox)
        }
        _ => None,
    }
}

/// Tear down a sandbox created by [`prepare_sandbox`]. Best-effort: removes the
/// git worktree if it is one, otherwise `rm -rf`s the copy.
pub async fn cleanup_sandbox(exec: &WorkspaceExec, base_work_dir: &Path, sandbox: &Path) {
    let base_str = base_work_dir.to_string_lossy().to_string();
    let sandbox_str = sandbox.to_string_lossy().to_string();
    let cmd = format!(
        "git -C {b} worktree remove --force {s} 2>/dev/null || rm -rf {s}",
        b = single_quote(&base_str),
        s = single_quote(&sandbox_str)
    );
    let args = vec!["-lc".to_string(), cmd];
    let _ = exec
        .output(base_work_dir, "/bin/bash", &args, HashMap::new())
        .await;
}

fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &s[..end])
}

/// Truncate a tool result at [`MAX_TOOL_RESULT_BYTES`], appending an explicit
/// notice with the full size so the model knows it saw a partial view and how
/// to fetch the rest (instead of silently mistaking the prefix for the whole).
fn truncate_tool_result(s: &str) -> String {
    if s.len() <= MAX_TOOL_RESULT_BYTES {
        return s.to_string();
    }
    let mut end = MAX_TOOL_RESULT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… [truncated: showing first {} of {} bytes — use read_file with \
         offset/limit, or tail/sed line ranges, to view the rest]",
        &s[..end],
        end,
        s.len()
    )
}

/// Heuristic detection of credential-shaped tokens in an operator message, used
/// to nudge the model into persisting them via `set_workspace_env` instead of
/// letting them live (and die) in chat context.
fn message_mentions_secret(text: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?x)
            \b(
                sk-[A-Za-z0-9_-]{12,}                       # OpenAI / proxy-style keys
              | (ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}      # GitHub tokens
              | github_pat_[A-Za-z0-9_]{20,}
              | AKIA[0-9A-Z]{16}                            # AWS access key id
              | xox[abprs]-[A-Za-z0-9-]{10,}                # Slack tokens
              | AIza[0-9A-Za-z_-]{30,}                      # Google API keys
              | eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,} # JWT
            )",
        )
        .expect("static secret-detection regex must compile")
    });
    re.is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_store() -> Arc<AskStore> {
        let path = std::env::temp_dir().join(format!("ask-bridge-{}.db", Uuid::new_v4()));
        Arc::new(AskStore::open(path).await.unwrap())
    }

    #[tokio::test]
    async fn operator_notes_prepend_then_drain() {
        let store = temp_store().await;
        let mission = Uuid::new_v4();

        // No pending notes → message is returned unchanged.
        let (msg, n) = prepend_pending_operator_notes(&store, mission, "Continue.".into()).await;
        assert_eq!(n, 0);
        assert_eq!(msg, "Continue.");

        store
            .enqueue_operator_note(mission, "Added scripts/probe.sh", None)
            .await
            .unwrap();

        let (msg, n) = prepend_pending_operator_notes(&store, mission, "Continue.".into()).await;
        assert_eq!(n, 1);
        assert!(msg.starts_with("<operator-note>"));
        assert!(msg.contains("Added scripts/probe.sh"));
        assert!(msg.trim_end().ends_with("Continue."));

        // Flushed exactly once — the next turn sees no note.
        let (msg, n) = prepend_pending_operator_notes(&store, mission, "Continue.".into()).await;
        assert_eq!(n, 0);
        assert_eq!(msg, "Continue.");
    }

    #[test]
    fn tool_result_truncation_reports_full_size() {
        let small = "ok";
        assert_eq!(truncate_tool_result(small), "ok");

        let big = "x".repeat(MAX_TOOL_RESULT_BYTES + 500);
        let out = truncate_tool_result(&big);
        assert!(out.contains(&format!(
            "showing first {} of {} bytes",
            MAX_TOOL_RESULT_BYTES,
            big.len()
        )));
        assert!(out.contains("read_file with"));
    }

    #[test]
    fn secret_detection_matches_common_token_shapes() {
        // Synthetic values only — shaped like real tokens, never actual ones.
        assert!(message_mentions_secret(
            "try sk-proxy-0123456789abcdef0123456789abcdef and tell me"
        ));
        assert!(message_mentions_secret(
            "token: ghp_0123456789abcdefghijABCDEFGHIJ0123"
        ));
        assert!(message_mentions_secret("AKIAIOSFODNN7EXAMPLE"));
        // Plain prose, short ids, and tactic punctuation must not trip it.
        assert!(!message_mentions_secret("what is the mission status?"));
        assert!(!message_mentions_secret("the sk-learn library"));
        assert!(!message_mentions_secret("commit 381a865f is on master"));
    }

    #[test]
    fn steer_messages_carry_a_copilot_attribution() {
        let msg = format_steer_message("Stop rewriting orchestrator-state.json.");
        assert!(msg.starts_with("[Steering from the operator via the Copilot]"));
        assert!(msg.ends_with("Stop rewriting orchestrator-state.json."));
    }

    #[test]
    fn env_value_masking_never_leaks() {
        let masked = mask_value("sk-proxy-0123456789abcdef0123456789abcdef");
        assert_eq!(masked, "sk-p… (41 chars)");
        assert!(!masked.contains("0123456789"));
    }
}
