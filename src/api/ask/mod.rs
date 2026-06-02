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

use serde_json::{json, Value};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::api::mission_store::MissionStore;
use crate::workspace_exec::WorkspaceExec;

pub use client::AskClient;
pub use store::{AskMessage, AskStore, AskThread, OperatorNote};

/// Hard cap on tool-calling iterations per turn — keeps cost/latency bounded.
const MAX_ITERATIONS: usize = 6;
/// Max bytes of any single tool result fed back to the model.
const MAX_TOOL_RESULT_BYTES: usize = 8_000;
/// Number of recent mission events seeded into the system prompt.
const SEED_EVENT_COUNT: usize = 40;

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
}

/// Run one Ask turn: persist the operator message, drive the tool loop, persist
/// the assistant answer, and return the final text.
pub async fn run_ask_turn(turn: &AskTurn, user_content: &str) -> Result<String, String> {
    // Persist the operator's message first.
    turn.ask_store
        .append_message(turn.thread_id, "user", user_content, None, None, None)
        .await?;

    // Assemble the OpenAI message array: system + prior turns + this message.
    let system = build_system_prompt(turn).await;
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
            let result = truncate(&result, MAX_TOOL_RESULT_BYTES);

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
        final_answer =
            "(The assistant reached the tool-call limit without a final answer.)".to_string();
    }

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

async fn build_system_prompt(turn: &AskTurn) -> String {
    let mut seed = String::new();
    if let Ok(Some(mission)) = turn.mission_store.get_mission(turn.mission_id).await {
        if let Some(title) = &mission.title {
            seed.push_str(&format!("Mission title: {title}\n"));
        }
        if let Some(desc) = &mission.short_description {
            seed.push_str(&format!("Mission status: {desc}\n"));
        }
        seed.push_str(&format!("Mission backend: {}\n", mission.backend));
    }

    if let Ok(events) = turn
        .mission_store
        .get_latest_events(turn.mission_id, SEED_EVENT_COUNT)
        .await
    {
        seed.push_str("\nRecent mission events (newest last):\n");
        for ev in &events {
            let tool = ev
                .tool_name
                .as_deref()
                .map(|t| format!(" {t}"))
                .unwrap_or_default();
            seed.push_str(&format!(
                "[{}] {}{}: {}\n",
                ev.sequence,
                ev.event_type,
                tool,
                truncate(&ev.content, 240)
            ));
        }
    }

    format!(
        "You are the Ask co-pilot for an autonomous coding mission. A separate \
         \"working agent\" is doing the real work in this same workspace; you are a \
         read-mostly assistant helping the operator understand and occasionally nudge \
         what's happening.\n\n\
         Prefer reading (history, files, logs) over writing. You MAY write to the \
         workspace with the bash tool, but anything you change is reported to the \
         working agent, so keep writes minimal and intentional. The full mission \
         history may be large — retrieve what you need with read_history / bash \
         (grep, rg, cat) rather than assuming it is all provided.\n\n\
         Be concise and concrete. Cite event sequence numbers or file paths when \
         relevant.\n\n\
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
                "description": "Fetch the most recent mission events (the working agent's transcript: messages, tool calls/results, errors).",
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
                "description": "Read a file from the mission workspace by path.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (absolute or relative to the workspace root)." }
                    },
                    "required": ["path"]
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
            run_bash(turn, &format!("cat -- {}", single_quote(path))).await
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
                            "[{}] {}{}: {}\n",
                            ev.sequence,
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
        other => format!("Error: unknown tool '{other}'"),
    }
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
}
