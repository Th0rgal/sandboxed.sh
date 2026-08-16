//! Grok Build CLI turn runner.
//!
//! Moved verbatim from `mission_runner.rs` (Phase 2 of the decomposition).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::agents::{AgentResult, CompletionConfidence, CompletionSignal, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::*;
use crate::cost::resolve_cost_cents_and_source;
use crate::util::env_var_bool;
use crate::workspace::Workspace;
use crate::workspace_exec::WorkspaceExec;

/// Overlay / guest path for a Grok CLI, never a host path like `/opt/grok-cli`.
///
/// Absolute configured programs are not overlay-looked-up: an overlay file at
/// `opt/grok-cli` would otherwise become the exec path, and nsenter without
/// `--root` would resolve it on the host.
fn grok_overlay_guest_path(workspace: &Workspace, program: &str) -> Option<String> {
    let relative = (!program.starts_with('/')).then_some(program);
    let overlay = container_overlay_command_path(workspace, "/usr/local/bin/grok")
        .or_else(|| container_overlay_command_path(workspace, "grok"))
        .or_else(|| relative.and_then(|name| container_overlay_command_path(workspace, name)))?;
    Some(container_overlay_guest_path(workspace, &overlay, "grok"))
}

/// Whether nsenter `Present` may be used as the exec path. Absolute container
/// paths that missed the guest overlay are host paths (`/opt/grok-cli`).
fn grok_cli_path_from_presence(
    is_container: bool,
    program: &str,
    cli_path: &str,
    presence: CommandPresence,
) -> Option<String> {
    match presence {
        CommandPresence::Present if !(is_container && program.starts_with('/')) => {
            Some(cli_path.to_string())
        }
        _ => None,
    }
}

async fn install_grok_cli_in_workspace(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> Result<String, String> {
    match command_presence(workspace_exec, cwd, "curl").await {
        CommandPresence::Present => {}
        CommandPresence::Inconclusive => {
            return Err(format!(
                "Grok Build CLI '{}' probe timed out and the binary is not visible on the container overlay. Not treating grok/curl as absent (host nsenter overloaded).",
                cli_path
            ));
        }
        CommandPresence::Absent => {
            if container_overlay_command_path(&workspace_exec.workspace, "curl").is_none()
                && container_overlay_command_path(&workspace_exec.workspace, "/usr/bin/curl")
                    .is_none()
            {
                return Err(format!(
                    "Grok Build CLI '{}' not found and curl is not available in the workspace. Install curl or install Grok manually.",
                    cli_path
                ));
            }
        }
    }

    tracing::info!("Auto-installing Grok Build CLI");
    let output = workspace_exec
        .output(
            cwd,
            "/bin/sh",
            &[
                "-lc".to_string(),
                "curl -fsSL https://x.ai/cli/install.sh | GROK_BIN_DIR=/usr/local/bin bash 2>&1"
                    .to_string(),
            ],
            HashMap::new(),
        )
        .await
        .map_err(|e| format!("Failed to run Grok Build installer: {}", e))?;

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
            message = "Grok Build install failed with no output".to_string();
        }
        return Err(format!("Grok Build install failed: {}", message));
    }

    if let Some(guest) = grok_overlay_guest_path(&workspace_exec.workspace, "grok") {
        return Ok(guest);
    }
    if command_available(workspace_exec, cwd, "/usr/local/bin/grok").await {
        Ok("/usr/local/bin/grok".to_string())
    } else if !cli_path.starts_with('/') && command_available(workspace_exec, cwd, cli_path).await {
        Ok(cli_path.to_string())
    } else {
        Err(
            "Grok Build install completed but 'grok' is still not available in workspace PATH."
                .to_string(),
        )
    }
}

async fn copy_host_grok_cli_into_container(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    program: &str,
) -> Result<Option<String>, String> {
    let Some(host) = resolve_host_executable(program)
        .or_else(|| resolve_host_executable("/usr/local/bin/grok"))
        .or_else(|| resolve_host_executable("grok"))
    else {
        return Ok(None);
    };
    let dest = copy_host_executable_into_container(&workspace_exec.workspace, &host)?;
    if container_overlay_command_path(&workspace_exec.workspace, &dest).is_some()
        || command_available(workspace_exec, cwd, &dest).await
    {
        tracing::info!(
            host = %host.display(),
            dest,
            "Copied host Grok CLI into container workspace"
        );
        return Ok(Some(dest));
    }
    Ok(None)
}

async fn ensure_grok_cli_available(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    cli_path: &str,
) -> Result<String, String> {
    let program = cli_path.split(' ').next().unwrap_or(cli_path);
    let is_container =
        workspace_exec.workspace.workspace_type == crate::workspace::WorkspaceType::Container;

    if let Some(guest) = grok_overlay_guest_path(&workspace_exec.workspace, program) {
        return Ok(guest);
    }
    let presence = command_presence(workspace_exec, cwd, program).await;
    if let Some(path) = grok_cli_path_from_presence(is_container, program, cli_path, presence) {
        return Ok(path);
    }
    match presence {
        CommandPresence::Present => {
            tracing::warn!(
                program,
                "Ignoring host-absolute Grok CLI path for container workspace"
            );
        }
        CommandPresence::Inconclusive => {
            tracing::warn!(
                program,
                "Grok CLI nsenter probe timed out; not treating as absent"
            );
        }
        CommandPresence::Absent => {}
    }

    let auto_install = env_var_bool("SANDBOXED_SH_AUTO_INSTALL_GROK", true);
    let mut install_error = None;
    if auto_install {
        match install_grok_cli_in_workspace(workspace_exec, cwd, cli_path).await {
            Ok(path) => return Ok(path),
            Err(err) => install_error = Some(err),
        }
    }

    // Host copy is last resort and only after the ELF arch check inside
    // copy_host_executable_into_container.
    if is_container {
        match copy_host_grok_cli_into_container(workspace_exec, cwd, program).await {
            Ok(Some(dest)) => return Ok(dest),
            Ok(None) => {}
            Err(err) => {
                return Err(format!(
                    "Grok Build CLI not found in workspace and the host CLI cannot be copied: {err}"
                ));
            }
        }
    }

    if let Some(err) = install_error {
        return Err(err);
    }

    Err(format!(
        "Grok Build CLI '{}' not found in workspace. Install it with: curl -fsSL https://x.ai/cli/install.sh | bash",
        cli_path
    ))
}

fn grok_event_is_reasoning_type(value: &serde_json::Value) -> bool {
    value.get("type").and_then(|v| v.as_str()).is_some_and(|t| {
        let lower = t.to_ascii_lowercase();
        // grok-cli 0.2.x `--output-format streaming-json` emits incremental
        // thinking as `{"type":"thought","data":"..."}` (verified against
        // grok 0.2.16 with grok-build-0.1 and grok-4.20-reasoning).
        lower == "reasoning"
            || lower == "thinking"
            || lower == "reasoning_delta"
            || lower == "thought"
    })
}

pub(crate) fn grok_event_text(value: &serde_json::Value) -> Option<String> {
    if grok_event_is_reasoning_type(value) {
        return None;
    }

    if let Some(text) = value
        .get("delta")
        .and_then(|delta| delta.get("text").or_else(|| delta.get("content")))
        .and_then(|v| v.as_str())
    {
        return Some(text.to_string());
    }

    if value
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.eq_ignore_ascii_case("text"))
    {
        if let Some(text) = value.get("data").and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }
    }

    if let Some(content) = value.get("content") {
        if let Some(text) = content.as_str() {
            return Some(text.to_string());
        }
        if let Some(text) = content.get("text").and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }
    }

    if let Some(text) = value.get("message").and_then(|message| {
        message.as_str().map(str::to_string).or_else(|| {
            message.get("content").and_then(|content| {
                content.as_str().map(str::to_string).or_else(|| {
                    content.as_array().map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                })
            })
        })
    }) {
        if !text.is_empty() {
            return Some(text);
        }
    }

    for key in ["text", "answer", "result", "output"] {
        if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }
    }

    None
}

/// Extract Grok / xAI reasoning text from a streamed JSONL event.
///
/// The Grok Build CLI mostly mirrors the xAI Chat Completions stream, which
/// puts chain-of-thought in `delta.reasoning_content` (some builds) or
/// `delta.reasoning` (others), and sometimes wraps it as a typed event
/// (`type: "reasoning" | "thinking"` with `data` or `text`). Field name
/// discovery is conservative — return None if no known key is present so a
/// CLI version bump doesn't accidentally show user-visible noise as
/// reasoning.
pub(crate) fn grok_event_reasoning(value: &serde_json::Value) -> Option<String> {
    let is_reasoning_type = grok_event_is_reasoning_type(value);

    if let Some(delta) = value.get("delta") {
        for key in ["reasoning_content", "reasoning", "thinking"] {
            if let Some(text) = delta.get(key).and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
        if is_reasoning_type {
            for key in ["text", "content"] {
                if let Some(text) = delta.get(key).and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }

    if is_reasoning_type {
        for key in ["data", "text", "content", "reasoning"] {
            if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }

    if let Some(text) = value
        .get("message")
        .and_then(|m| m.get("reasoning_content").or_else(|| m.get("reasoning")))
        .and_then(|v| v.as_str())
    {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    None
}

fn grok_event_session_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("session_id")
        .or_else(|| value.get("sessionId"))
        .or_else(|| value.get("session").and_then(|session| session.get("id")))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

fn grok_event_model(value: &serde_json::Value) -> Option<String> {
    value
        .get("model")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("model"))
        })
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}
pub(crate) fn grok_event_usage(value: &serde_json::Value) -> Option<crate::cost::TokenUsage> {
    let usage = value
        .get("usage")
        .or_else(|| value.get("tokenUsage"))
        .or_else(|| value.get("token_usage"))
        .or_else(|| value.get("response").and_then(|r| r.get("usage")))
        .or_else(|| value.get("message").and_then(|m| m.get("usage")))?;

    let raw_input_tokens = usage_value_tokens(
        usage,
        &[
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
        ],
    );
    let output_tokens = usage_value_tokens(
        usage,
        &[
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
        ],
    );
    let cache_creation_tokens = usage_value_tokens(
        usage,
        &[
            "cache_creation_input_tokens",
            "cacheCreationInputTokens",
            "cache_write_input_tokens",
            "cacheWriteInputTokens",
        ],
    );
    let explicit_cache_read_tokens = usage_value_tokens(
        usage,
        &[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cached_tokens",
            "cachedTokens",
        ],
    );
    let nested_cached_tokens =
        nested_usage_value_tokens(usage, &["input_tokens_details", "cached_tokens"])
            .saturating_add(nested_usage_value_tokens(
                usage,
                &["prompt_tokens_details", "cached_tokens"],
            ));
    let cache_read_tokens = explicit_cache_read_tokens.saturating_add(nested_cached_tokens);
    // xAI/OpenAI-compatible usage reports usually include cached prompt
    // tokens inside the prompt/input total. Internally we store billable
    // non-cached input separately from discounted cache-read input, so the
    // two buckets can be summed for display without double counting and
    // priced at their respective rates.
    let input_tokens = raw_input_tokens.saturating_sub(cache_read_tokens);
    let token_usage = crate::cost::TokenUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: Some(cache_creation_tokens),
        cache_read_input_tokens: Some(cache_read_tokens),
    };
    token_usage.has_usage().then_some(token_usage)
}

fn grok_event_is_error(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.eq_ignore_ascii_case("error"))
        || value.get("error").is_some()
}

/// Detect the Grok CLI's interactive sign-in prompt. The CLI prints these to
/// stderr when it can't authenticate non-interactively, then blocks on a local
/// OAuth callback that never arrives in a headless mission. Matching any of
/// these lets the runner fail fast instead of hanging.
fn grok_line_requests_interactive_login(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("signing in with grok")
        || lower.contains("open this url to sign in")
        || lower.contains("oauth2/authorize")
}

pub(crate) fn grok_stdout_line_requests_interactive_login(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).is_err()
        && grok_line_requests_interactive_login(line)
}

/// Resolve the env the Grok CLI needs for non-interactive auth: refresh the
/// xAI OAuth token if stale, materialize/sync the CLI auth file, and export
/// `XAI_API_KEY` (key > OAuth access token > ambient env). Shared by the
/// streaming-json and ACP turn paths.
async fn prepare_grok_auth_env(
    app_working_dir: &std::path::Path,
    mission_id: Uuid,
) -> Result<HashMap<String, String>, AgentResult> {
    let mut oauth_access_token: Option<String> = None;
    if let Some(entry) =
        crate::api::ai_providers::read_oauth_token_entry(crate::ai_providers::ProviderType::Xai)
    {
        if crate::api::ai_providers::oauth_token_expired(entry.expires_at) {
            match crate::api::ai_providers::refresh_oauth_token_with_lock(
                crate::ai_providers::ProviderType::Xai,
                entry.expires_at,
            )
            .await
            {
                Ok((access, _refresh, expires_at)) => {
                    oauth_access_token = Some(access);
                    tracing::info!(
                        mission_id = %mission_id,
                        expires_at,
                        "Refreshed xAI OAuth token before starting Grok Build"
                    );
                }
                Err(crate::api::ai_providers::OAuthRefreshError::InvalidGrant(err)) => {
                    return Err(AgentResult::failure(
                        format!(
                            "Grok Build xAI OAuth refresh token is expired or revoked. Reconnect the xAI provider, then retry the mission. {}",
                            err
                        ),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::LlmError));
                }
                Err(err) => {
                    return Err(AgentResult::failure(
                        format!(
                            "Failed to refresh xAI OAuth before starting Grok Build: {}",
                            err
                        ),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::LlmError));
                }
            }
        } else {
            oauth_access_token = Some(entry.access_token.clone());
        }
    }

    // Grok CLI 0.2.93 rejects the legacy `auth_mode: "oauth"` shape older
    // Sandboxed versions generated. The runner authenticates with an explicit
    // environment key below, so never copy or overwrite the CLI's native auth
    // file; just remove our obsolete entry when present.
    if let Err(err) = crate::api::ai_providers::remove_legacy_grok_oauth_auth_entries() {
        tracing::warn!(
            mission_id = %mission_id,
            error = %err,
            "Failed to remove obsolete Grok OAuth auth entry"
        );
    }

    // Authenticate the Grok CLI non-interactively via XAI_API_KEY. Priority:
    // an explicit xAI API key, then the captured OAuth access token, then any
    // ambient env key. Setting this is what prevents the interactive-sign-in
    // hang; the CLI prints "You are using XAI_API_KEY" and goes straight to
    // api.x.ai.
    let mut env = HashMap::new();
    let xai_api_key = crate::api::ai_providers::get_xai_api_key_for_grok(app_working_dir)
        .or_else(|| oauth_access_token.clone())
        .or_else(|| {
            std::env::var("XAI_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("GROK_CODE_XAI_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty())
        });
    if let Some(key) = xai_api_key {
        // Newer Grok CLIs read XAI_API_KEY; keep GROK_CODE_XAI_API_KEY for
        // backward compatibility with older builds.
        env.insert("XAI_API_KEY".to_string(), key.clone());
        env.insert("GROK_CODE_XAI_API_KEY".to_string(), key);
    }

    Ok(env)
}

/// Execute a turn using the Grok Build CLI backend.
///
/// Dispatches to the ACP path (`grok agent stdio`) by default — it is the
/// only mode that surfaces tool calls and works for thinking on every model.
/// Set `SANDBOXED_SH_GROK_ACP=0` to force the legacy `--output-format
/// streaming-json` path; the dispatcher also falls back to it automatically
/// when the ACP handshake fails before the prompt is sent.
#[allow(clippy::too_many_arguments)]
pub async fn run_grok_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    session_id: Option<&str>,
    is_continuation: bool,
) -> AgentResult {
    if workspace.id == crate::workspace::DEFAULT_WORKSPACE_ID && !work_dir.join(".git").exists() {
        let file_count = std::fs::read_dir(work_dir)
            .map(|mut d| {
                d.by_ref()
                    .filter(|e| {
                        e.as_ref()
                            .map(|e| {
                                let n = e.file_name();
                                let n = n.to_string_lossy();
                                !n.starts_with('.') && n != "output"
                            })
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if file_count == 0 && !is_continuation {
            let dir_display = work_dir.display();
            tracing::warn!(
                mission_id = %mission_id,
                work_dir = %dir_display,
                "Grok mission running in empty host workspace with no git repo — goal loop will hallucinate edits"
            );
            let msg = format!(
                "The mission workspace ({dir_display}) is empty and has no git repository. \
                 Grok cannot edit files or push changes without a project checkout. \
                 Create this mission on a workspace that contains the target repository, \
                 or clone the repo into the workspace first.",
            );
            // Return a failure result so the control loop emits a single
            // `AssistantMessage { success: false }` and marks the mission
            // `Failed` (Bugbot f4a7a2d8). Emitting a manual AssistantMessage
            // and then returning success:true caused the control loop to
            // emit a SECOND assistant message with success:true and record
            // automations as successful, despite the workspace being
            // unusable. LlmError is the right terminal reason: this is a
            // "can't run" error, not a clean turn boundary.
            return AgentResult::failure(msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    }

    if env_var_bool("SANDBOXED_SH_GROK_ACP", true) {
        match run_grok_acp_turn(
            workspace,
            work_dir,
            message,
            model,
            mission_id,
            events_tx.clone(),
            cancel.clone(),
            app_working_dir,
            session_id,
            is_continuation,
        )
        .await
        {
            Ok(result) => return result,
            Err(fallback) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    reason = %fallback.reason,
                    drop_session_id = fallback.drop_session_id,
                    "Grok ACP handshake failed before the prompt was sent; \
                     falling back to streaming-json mode"
                );
                // An unloadable session must not be passed as --session-id:
                // its upsert semantics would create a fresh EMPTY session
                // under that id. Omitting it makes the legacy path use
                // --continue, which resumes the last session in this
                // mission directory and preserves context.
                let fallback_session_id = if fallback.drop_session_id {
                    None
                } else {
                    session_id
                };
                return run_grok_streaming_json_turn(
                    workspace,
                    work_dir,
                    message,
                    model,
                    mission_id,
                    events_tx,
                    cancel,
                    app_working_dir,
                    fallback_session_id,
                    is_continuation,
                )
                .await;
            }
        }
    }
    run_grok_streaming_json_turn(
        workspace,
        work_dir,
        message,
        model,
        mission_id,
        events_tx,
        cancel,
        app_working_dir,
        session_id,
        is_continuation,
    )
    .await
}

/// Legacy turn path: `grok -p <msg> --output-format streaming-json`.
///
/// Emits `thought`/`text` events only — the CLI executes tools silently in
/// this mode (verified on grok 0.2.16), so tool calls never reach the UI.
/// Kept as the fallback while the ACP path soaks.
#[allow(clippy::too_many_arguments)]
async fn run_grok_streaming_json_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    session_id: Option<&str>,
    is_continuation: bool,
) -> AgentResult {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let workspace_exec = WorkspaceExec::new(workspace.clone());

    let cli_path =
        get_backend_string_setting("grok", "cli_path").unwrap_or_else(|| "grok".to_string());
    let cli_path = match ensure_grok_cli_available(&workspace_exec, work_dir, &cli_path).await {
        Ok(cli_path) => cli_path,
        Err(err_msg) => {
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    let mut args = Vec::new();
    // Use `-s/--session-id` for both first-turn and continuation when we
    // already have a session id from the mission store. Per grok headless
    // docs, `--session-id` has upsert semantics — loads the session if it
    // exists, creates one with that id otherwise — so it self-heals the
    // "orphan session" case where the first turn failed before grok could
    // persist the session and `--resume <sid>` would error with "Session
    // does not exist". `--resume` is strict-existence-only; we only fall
    // through to `--continue` when we have no session id at all.
    if let Some(sid) = session_id {
        args.push("--session-id".to_string());
        args.push(sid.to_string());
    } else if is_continuation {
        args.push("--continue".to_string());
    }
    args.push("-p".to_string());
    args.push(message.to_string());
    args.push("--output-format".to_string());
    args.push("streaming-json".to_string());
    args.push("--always-approve".to_string());
    args.push("--cwd".to_string());
    args.push(workspace_exec.translate_path_for_container(work_dir));
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }

    // The Grok CLI authenticates non-interactively via the XAI_API_KEY env var.
    // The xAI OAuth access token works as a bearer key against api.x.ai, so we
    // capture the freshest one here and inject it below. Without it the CLI
    // falls back to an interactive browser sign-in that never completes in a
    // headless mission — the run then hangs forever ("Agent is working").
    let env = match prepare_grok_auth_env(app_working_dir, mission_id).await {
        Ok(env) => env,
        Err(result) => return result,
    };

    let mut child = match workspace_exec
        .spawn_streaming(work_dir, &cli_path, &args, env)
        .await
    {
        Ok(child) => child,
        Err(e) => {
            return AgentResult::failure(format!("Failed to start Grok Build CLI: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    drop(child.stdin.take());

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            return AgentResult::failure("Failed to capture Grok stdout".to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    let stderr = child.stderr.take();
    let stderr_capture = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
    let stderr_capture_clone = stderr_capture.clone();
    // The Grok CLI prints its interactive sign-in prompt to STDERR, then blocks
    // on a local OAuth callback. Watch for it here and signal the main loop to
    // abort so the mission fails fast instead of hanging forever.
    let auth_fail = CancellationToken::new();
    let auth_fail_signal = auth_fail.clone();
    let mut stderr_handle = stderr.map(|stderr| {
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if grok_line_requests_interactive_login(trimmed) {
                    auth_fail_signal.cancel();
                }
                let mut captured = stderr_capture_clone.lock().await;
                if !captured.is_empty() {
                    captured.push('\n');
                }
                captured.push_str(trimmed);
            }
        })
    });

    let mut final_result = String::new();
    let mut had_error = false;
    let mut model_used = model.map(str::to_string);
    let mut last_streamed_len = 0usize;
    let mut text_delta_coalescer = TextDeltaCoalescer::new();
    let mut token_usage = crate::cost::TokenUsage::default();
    // Accumulate Grok's reasoning deltas into a cumulative buffer and
    // throttle Thinking emissions the same way text deltas are throttled.
    // Grok's CLI delivers reasoning as incremental tokens, mirroring the
    // text path.
    let mut reasoning_buffer = String::new();
    let mut last_reasoning_len = 0usize;
    let mut reasoning_delta_coalescer = TextDeltaCoalescer::new();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut cancelled = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                if let Some(handle) = stderr_handle.take() {
                    handle.abort();
                }
                cancelled = true;
                break;
            }
            _ = auth_fail.cancelled() => {
                // Grok CLI emitted an interactive sign-in prompt (it can't
                // authenticate non-interactively). Kill it and fail fast.
                let _ = child.kill().await;
                if let Some(handle) = stderr_handle.take() {
                    handle.abort();
                }
                return AgentResult::failure(
                    "Grok Build could not authenticate non-interactively (the CLI requested a browser sign-in). Reconnect the xAI / Grok Build provider in Settings → Providers, then retry the mission.".to_string(),
                    0,
                )
                .with_terminal_reason(TerminalReason::LlmError);
            }
            line_result = lines.next_line() => {
                match line_result {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let value: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(value) => value,
                            Err(_) => {
                                // Fail fast on raw interactive sign-in prompts.
                                // Valid streaming-json events may contain these
                                // substrings as assistant/tool text, so only
                                // inspect stdout after JSON parsing fails.
                                if grok_stdout_line_requests_interactive_login(&line) {
                                    let _ = child.kill().await;
                                    if let Some(handle) = stderr_handle.take() {
                                        handle.abort();
                                    }
                                    return AgentResult::failure(
                                        "Grok Build could not authenticate non-interactively (the CLI requested a browser sign-in). Reconnect the xAI / Grok Build provider in Settings → Providers, then retry the mission.".to_string(),
                                        0,
                                    )
                                    .with_terminal_reason(TerminalReason::LlmError);
                                }
                                if final_result.is_empty() {
                                    final_result.push_str(&line);
                                } else {
                                    final_result.push('\n');
                                    final_result.push_str(&line);
                                }
                                continue;
                            }
                        };
                        if let Some(sid) = grok_event_session_id(&value) {
                            let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                                session_id: sid,
                                mission_id,
                            });
                        }
                        if model_used.is_none() {
                            model_used = grok_event_model(&value);
                        }
                        if let Some(usage) = grok_event_usage(&value) {
                            token_usage.input_tokens =
                                token_usage.input_tokens.max(usage.input_tokens);
                            token_usage.output_tokens =
                                token_usage.output_tokens.max(usage.output_tokens);
                            token_usage.cache_creation_input_tokens = Some(
                                token_usage
                                    .cache_creation_input_tokens
                                    .unwrap_or(0)
                                    .max(usage.cache_creation_input_tokens.unwrap_or(0)),
                            );
                            token_usage.cache_read_input_tokens = Some(
                                token_usage
                                    .cache_read_input_tokens
                                    .unwrap_or(0)
                                    .max(usage.cache_read_input_tokens.unwrap_or(0)),
                            );
                        }
                        if grok_event_is_error(&value) {
                            had_error = true;
                            if let Some(text) = grok_event_text(&value) {
                                final_result = text;
                            } else {
                                final_result = value.to_string();
                            }
                            continue;
                        }
                        if let Some(reasoning) = grok_event_reasoning(&value) {
                            if !reasoning.is_empty() {
                                merge_stream_fragment(&mut reasoning_buffer, &reasoning);
                                // Mirror the TextDelta coalescing strategy:
                                // emit cumulative snapshots throttled to ~50ms.
                                if reasoning_buffer.len() > last_reasoning_len
                                    && reasoning_delta_coalescer.should_emit()
                                {
                                    last_reasoning_len = reasoning_buffer.len();
                                    let _ = events_tx.send(AgentEvent::Thinking {
                                        content: reasoning_buffer.clone(),
                                        done: false,
                                        mission_id: Some(mission_id),
                                    });
                                }
                            }
                        }
                        if let Some(text) = grok_event_text(&value) {
                            if !text.is_empty() {
                                // The first non-reasoning content marks the
                                // boundary between thinking and answer; flush
                                // a final Thinking { done: true } so the
                                // dashboard collapses the reasoning panel
                                // before streaming text deltas.
                                if !reasoning_buffer.is_empty() {
                                    let _ = events_tx.send(thinking_final_event(
                                        std::mem::take(&mut reasoning_buffer),
                                        mission_id,
                                    ));
                                    last_reasoning_len = 0;
                                }
                                if value
                                    .get("delta")
                                    .is_some()
                                    || value.get("type").and_then(|v| v.as_str()).is_some_and(|t| {
                                    t.contains("delta") || t.contains("chunk") || t == "text"
                                    })
                                {
                                    merge_stream_fragment(&mut final_result, &text);
                                } else {
                                    final_result = text;
                                }
                                // P3-#21: rate-limit TextDelta emissions
                                // to at most one per ~50ms per turn. Grok
                                // bursts can hit ~100 tokens/sec; without
                                // this every token becomes its own SSE
                                // frame even though the dashboard rAF
                                // coalesces them into a single render.
                                // The cumulative-buffer semantics mean
                                // skipping intermediate frames loses no
                                // content — each emit replaces the prior.
                                if final_result.len() > last_streamed_len
                                    && text_delta_coalescer.should_emit()
                                {
                                    last_streamed_len = final_result.len();
                                    let _ = events_tx.send(AgentEvent::TextDelta {
                                        content: final_result.clone(),
                                        mission_id: Some(mission_id),
                                    });
                                }
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        had_error = true;
                        final_result = format!("Error reading Grok stdout: {}", e);
                        break;
                    }
                }
            }
        }
    }

    let exit_status = child.wait().await;
    if let Some(handle) = stderr_handle {
        let _ = handle.await;
    }

    // P3-#21 final flush: the coalescer may have dropped the very last
    // delta within the trailing 50ms window. Always emit one more
    // TextDelta carrying the full buffer so the dashboard sees the
    // closing tokens; the AssistantMessage that follows will replace it.
    if final_result.len() > last_streamed_len {
        let _ = events_tx.send(AgentEvent::TextDelta {
            content: final_result.clone(),
            mission_id: Some(mission_id),
        });
        last_streamed_len = final_result.len();
    }
    let _ = last_streamed_len; // silence "unused after final assignment"

    let reasoning_for_fallback = if reasoning_buffer.trim().is_empty() {
        None
    } else {
        Some(reasoning_buffer.clone())
    };

    // Flush any remaining reasoning that never got followed by a text
    // delta (e.g., reasoning-only turns or the trailing coalescer window).
    // Emit done: true so the dashboard finalizes the thinking block in the
    // event store.
    if !reasoning_buffer.is_empty() {
        let _ = events_tx.send(thinking_final_event(
            std::mem::take(&mut reasoning_buffer),
            mission_id,
        ));
    }
    let _ = last_reasoning_len;

    let cancel_marker = if cancelled {
        Some(cancel_or_shutdown_failure())
    } else {
        None
    };

    if final_result.trim().is_empty() {
        let stderr_content = stderr_capture.lock().await;
        if let Some(reasoning) = reasoning_for_fallback {
            final_result = reasoning;
        } else if let Some(marker) = cancel_marker.as_ref() {
            final_result = marker.output.clone();
        } else if !stderr_content.trim().is_empty() {
            final_result = format!(
                "Grok Build error: {}",
                stderr_content
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            had_error = true;
        } else {
            final_result = "Grok Build produced no output. Run `grok login` or configure an xAI provider for Grok Build.".to_string();
            had_error = true;
        }
    }

    let success = exit_status.map(|status| status.success()).unwrap_or(false) && !had_error;
    let model_for_cost = model_used.as_deref().or(Some("grok-build"));
    let (cost_cents, cost_source) =
        resolve_cost_cents_and_source(None, model_for_cost, &token_usage);
    let mut result = if success {
        AgentResult::success(final_result, cost_cents)
            .with_cost_source(cost_source)
            .with_terminal_reason(TerminalReason::TurnComplete)
    } else if let Some(marker) = cancel_marker {
        AgentResult::failure(final_result, cost_cents)
            .with_cost_source(cost_source)
            .with_terminal_reason(marker.terminal_reason.unwrap_or(TerminalReason::Cancelled))
    } else {
        AgentResult::failure(final_result, cost_cents)
            .with_cost_source(cost_source)
            .with_terminal_reason(TerminalReason::LlmError)
    };
    let success_signal = CompletionSignal::ProcessExit;
    let success_confidence = CompletionConfidence::Low;
    let outcome = turn_outcome_for_result(&result, success_signal, success_confidence);
    result = result.with_turn_outcome(outcome);
    if token_usage.has_usage() {
        result = result.with_usage(token_usage);
    }
    result = result.with_model(model_used.unwrap_or_else(|| "grok-build".to_string()));
    result
}

// ── ACP (`grok agent stdio`) turn path ─────────────────────────────────
//
// The streaming-json mode hides tool execution entirely and its event
// vocabulary depends on the model. The ACP mode (JSON-RPC over stdio, see
// https://docs.x.ai/build/cli/headless-scripting) emits the full session
// stream — verified against grok 0.2.16:
//   session/update: tool_call {toolCallId, title, rawInput}
//                   tool_call_update {kind, title, content, locations, status?}
//                   agent_thought_chunk {content.text}   (incremental thinking)
//                   agent_message_chunk {content.text}   (assistant text)
//   result of session/prompt: {stopReason, _meta: {totalTokens, modelId, ...}}
// Sessions persist server-side (`loadSession: true`), addressed by the same
// session ids the streaming path stored, so continuity carries over.

const GROK_ACP_INIT_ID: u64 = 1;
const GROK_ACP_SESSION_ID: u64 = 2;
const GROK_ACP_SESSION_NEW_ID: u64 = 3;
const GROK_ACP_SET_MODEL_ID: u64 = 4;
const GROK_ACP_PROMPT_ID: u64 = 5;

/// Marker embedded in handshake errors that mean "the stored session can't
/// be resumed over ACP — the legacy path must use --continue, not
/// --session-id". Matched by the dispatcher to set `drop_session_id`.
const GROK_ACP_DEFER_TO_CONTINUE: &str = "deferring to --continue";

/// Why the ACP path handed the turn back to the streaming-json fallback.
pub(crate) struct GrokAcpFallback {
    reason: String,
    /// True when the stored session id is not loadable over ACP: the
    /// fallback must NOT pass it as `--session-id` (upsert semantics would
    /// create a fresh empty session under that id) — omitting it makes the
    /// legacy path use `--continue`, which resumes the CLI's last session
    /// in the mission directory and preserves context.
    drop_session_id: bool,
}

impl From<String> for GrokAcpFallback {
    fn from(reason: String) -> Self {
        Self {
            reason,
            drop_session_id: false,
        }
    }
}

/// Per-call state for an in-flight grok ACP tool call.
#[derive(Default, Clone)]
struct GrokAcpToolCall {
    name: String,
    latest_update: serde_json::Value,
    result_emitted: bool,
}

fn grok_acp_update_is_terminal(update: &serde_json::Value) -> bool {
    matches!(
        update.get("status").and_then(|v| v.as_str()),
        Some("completed") | Some("failed")
    )
}

/// Execute a turn over `grok agent stdio` (ACP JSON-RPC).
///
/// Returns `Err(reason)` only for failures BEFORE the prompt is sent
/// (spawn, initialize, session setup) so the dispatcher can safely fall
/// back to the streaming-json path without double-executing the turn.
#[allow(clippy::too_many_arguments)]
async fn run_grok_acp_turn(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    session_id: Option<&str>,
    is_continuation: bool,
) -> Result<AgentResult, GrokAcpFallback> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let workspace_exec = WorkspaceExec::new(workspace.clone());

    let cli_path =
        get_backend_string_setting("grok", "cli_path").unwrap_or_else(|| "grok".to_string());
    let cli_path = ensure_grok_cli_available(&workspace_exec, work_dir, &cli_path)
        .await
        .map_err(|e| format!("grok CLI unavailable: {e}"))?;

    let env = match prepare_grok_auth_env(app_working_dir, mission_id).await {
        Ok(env) => env,
        // Auth failures are terminal for BOTH paths — surface them directly
        // instead of falling back into the same failure.
        Err(result) => return Ok(result),
    };

    // `grok agent stdio` accepts no further flags (verified: it rejects
    // --no-auto-update). cwd comes from spawn_streaming's working dir and
    // the session/new params.
    let args = vec!["agent".to_string(), "stdio".to_string()];
    let mut child = workspace_exec
        .spawn_streaming(work_dir, &cli_path, &args, env)
        .await
        .map_err(|e| format!("failed to spawn grok agent stdio: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to capture grok stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture grok stdout".to_string())?;
    let mut lines = BufReader::new(stdout).lines();

    // Capture stderr for diagnostics, and watch for the interactive
    // sign-in prompt: without a usable XAI_API_KEY the CLI prints it to
    // stderr and blocks on a browser OAuth callback that never arrives in
    // headless mode. Fail fast instead of waiting out the idle guard.
    let stderr_tail = Arc::new(tokio::sync::Mutex::new(String::new()));
    let auth_fail = CancellationToken::new();
    if let Some(stderr) = child.stderr.take() {
        let stderr_tail = Arc::clone(&stderr_tail);
        let auth_fail = auth_fail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if grok_line_requests_interactive_login(line.trim()) {
                    auth_fail.cancel();
                }
                let mut tail = stderr_tail.lock().await;
                tail.push_str(&line);
                tail.push('\n');
                if tail.len() > 8_192 {
                    let cut = tail.len() - 8_192;
                    tail.drain(..cut);
                }
            }
        });
    }

    async fn send(
        stdin: &mut (impl tokio::io::AsyncWrite + Unpin),
        value: serde_json::Value,
    ) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let mut payload = value.to_string();
        payload.push('\n');
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("grok ACP stdin write failed: {e}"))
    }

    /// Read lines until the response for `id` arrives, with a deadline.
    /// Notifications received meanwhile are returned to the caller.
    async fn await_response(
        lines: &mut tokio::io::Lines<impl tokio::io::AsyncBufRead + Unpin>,
        id: u64,
        deadline_secs: u64,
    ) -> Result<(serde_json::Value, Vec<serde_json::Value>), String> {
        let mut notifications = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| format!("grok ACP timed out waiting for response id {id}"))?;
            let line = tokio::time::timeout(remaining, lines.next_line())
                .await
                .map_err(|_| format!("grok ACP timed out waiting for response id {id}"))?
                .map_err(|e| format!("grok ACP stdout read failed: {e}"))?
                .ok_or_else(|| "grok ACP stream closed during handshake".to_string())?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = value.get("error") {
                    return Err(format!("grok ACP request {id} failed: {err}"));
                }
                return Ok((value, notifications));
            }
            notifications.push(value);
        }
    }

    // ── Handshake (failures here fall back to streaming-json) ──────────
    // Wrapped so every early error kills the spawned CLI first — a dropped
    // tokio Child keeps running (no kill_on_drop), and the fallback path
    // would spawn a second CLI for the same turn.
    let handshake: Result<(String, bool), String> = async {
        let mut session_load_failed = false;
        send(
            &mut stdin,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": GROK_ACP_INIT_ID,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } }
                }
            }),
        )
        .await?;
        let _ = await_response(&mut lines, GROK_ACP_INIT_ID, 30).await?;

        let acp_cwd = workspace_exec.translate_path_for_container(work_dir);
        let mut acp_session_id: Option<String> = None;
        if let Some(sid) = session_id.filter(|s| !s.trim().is_empty()) {
            // Sessions persist server-side; `session/load` resumes prior context.
            send(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": GROK_ACP_SESSION_ID,
                    "method": "session/load",
                    "params": { "sessionId": sid, "cwd": acp_cwd, "mcpServers": [] }
                }),
            )
            .await?;
            match await_response(&mut lines, GROK_ACP_SESSION_ID, 60).await {
                Ok(_) => acp_session_id = Some(sid.to_string()),
                Err(err) if is_continuation => {
                    // The stored session holds real prior context we cannot
                    // reach over ACP. The streaming path's `--continue`
                    // resumes the CLI's last session in this directory —
                    // defer to it instead of silently dropping context.
                    return Err(format!(
                        "stored session {sid} not loadable over ACP on a continuation \
                         turn ({err}); {GROK_ACP_DEFER_TO_CONTINUE}"
                    ));
                }
                Err(err) => {
                    session_load_failed = true;
                    tracing::info!(
                        mission_id = %mission_id,
                        session_id = %sid,
                        error = %err,
                        "Grok ACP session/load failed; starting a fresh session"
                    );
                }
            }
        } else if is_continuation {
            // Continuation with no stored session id at all: only the
            // streaming path's `--continue` can recover the prior session.
            return Err(format!(
                "continuation turn without a stored session id; {GROK_ACP_DEFER_TO_CONTINUE}"
            ));
        }
        if acp_session_id.is_none() {
            send(
                &mut stdin,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": GROK_ACP_SESSION_NEW_ID,
                    "method": "session/new",
                    "params": { "cwd": acp_cwd, "mcpServers": [] }
                }),
            )
            .await?;
            let (resp, _) = await_response(&mut lines, GROK_ACP_SESSION_NEW_ID, 60).await?;
            let sid = resp
                .get("result")
                .and_then(|r| r.get("sessionId"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "grok ACP session/new returned no sessionId".to_string())?
                .to_string();
            let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                mission_id,
                session_id: sid.clone(),
            });
            acp_session_id = Some(sid);
        }
        Ok((
            acp_session_id.expect("session id established above"),
            session_load_failed,
        ))
    }
    .await;
    let (acp_session_id, session_load_failed) = match handshake {
        Ok(pair) => pair,
        Err(err) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let drop_session_id = err.contains(GROK_ACP_DEFER_TO_CONTINUE);
            return Err(GrokAcpFallback {
                reason: err,
                drop_session_id,
            });
        }
    };

    // The ACP session default is the non-reasoning chat model
    // (grok-4.20-*-non-reasoning), which emits no thought chunks. The legacy
    // `grok -p` path defaulted to grok-build (a reasoning model), so missions
    // without an explicit model keep that behavior — and a populated thoughts
    // panel — here too. Override via backend setting `default_model`.
    let requested_model = model.filter(|m| !m.trim().is_empty()).map(str::to_string);
    let model_was_explicit = requested_model.is_some();
    let effective_model = requested_model
        .clone()
        .or_else(|| get_backend_string_setting("grok", "default_model"))
        .or_else(|| Some("grok-build-0.1".to_string()));
    let mut selected_model: Option<String> = None;
    if let Some(model) = effective_model.as_deref() {
        send(
            &mut stdin,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": GROK_ACP_SET_MODEL_ID,
                "method": "session/set_model",
                "params": { "sessionId": acp_session_id, "modelId": model }
            }),
        )
        .await?;
        match await_response(&mut lines, GROK_ACP_SET_MODEL_ID, 30).await {
            Ok(_) => {
                selected_model = Some(model.to_string());
            }
            Err(err) if model_was_explicit => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(AgentResult::failure(
                    format!(
                        "Grok Build rejected model override '{}': {}. Run `grok models` on the server to see models available for this account.",
                        model, err
                    ),
                    0,
                )
                .with_terminal_reason(TerminalReason::LlmError)
                .with_model(model.to_string()));
            }
            Err(err) => {
                tracing::warn!(mission_id = %mission_id, model, error = %err, "Grok ACP set_model failed; using session default");
            }
        }
    }

    // ── Prompt (from here on, failures are real turn failures) ─────────
    if let Err(e) = send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": GROK_ACP_PROMPT_ID,
            "method": "session/prompt",
            "params": {
                "sessionId": acp_session_id,
                "prompt": [{ "type": "text", "text": message }]
            }
        }),
    )
    .await
    {
        // Prompt never reached the agent — kill the CLI before handing the
        // turn back to the fallback path (same orphan hazard as handshake).
        let _ = child.kill().await;
        let _ = child.wait().await;
        // If the stored session already failed to load over ACP, the legacy
        // path must not upsert an empty session under that id either.
        return Err(GrokAcpFallback {
            reason: e,
            drop_session_id: session_load_failed,
        });
    }

    let mut thinking_buffer = String::new();
    let mut thinking_done_emitted = false;
    let mut text_buffer = String::new();
    let mut tool_calls: HashMap<String, GrokAcpToolCall> = HashMap::new();
    let mut model_used: Option<String> = selected_model;
    let mut usage = crate::cost::TokenUsage::default();
    let mut stop_reason: Option<String> = None;
    let mut transport_error: Option<String> = None;

    // Idle guard: tool executions stream tool_call_update events, so a long
    // silent gap means the CLI is stuck (or waiting on something that will
    // never arrive in headless mode).
    let idle_limit = std::time::Duration::from_secs(180);

    loop {
        let line = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Ok(AgentResult::failure("Mission cancelled".to_string(), 0)
                    .with_terminal_reason(TerminalReason::Cancelled));
            }
            _ = auth_fail.cancelled() => {
                let _ = child.kill().await;
                return Ok(AgentResult::failure(
                    "Grok Build requires interactive sign-in (no usable XAI_API_KEY). \
                     Reconnect the xAI provider or set an API key, then retry."
                        .to_string(),
                    0,
                )
                .with_terminal_reason(TerminalReason::AuthError));
            }
            line = tokio::time::timeout(idle_limit, lines.next_line()) => match line {
                Err(_) => {
                    transport_error = Some(format!(
                        "Grok ACP produced no events for {}s; killing the CLI",
                        idle_limit.as_secs()
                    ));
                    let _ = child.kill().await;
                    break;
                }
                Ok(Err(e)) => {
                    transport_error = Some(format!("Grok ACP stdout read failed: {e}"));
                    break;
                }
                Ok(Ok(None)) => {
                    if stop_reason.is_none() {
                        transport_error =
                            Some("Grok ACP stream closed before the prompt completed".to_string());
                    }
                    break;
                }
                Ok(Ok(Some(line))) => line,
            }
        };

        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };

        // Incoming request FROM the agent (has both id and method): the only
        // one we expect is a permission prompt — auto-approve it, mirroring
        // the streaming path's --always-approve.
        if let (Some(req_id), Some(method)) = (value.get("id"), value.get("method")) {
            if method == "session/request_permission" {
                let option_id = value
                    .pointer("/params/options")
                    .and_then(|v| v.as_array())
                    .and_then(|opts| {
                        opts.iter()
                            .find(|o| {
                                o.get("kind")
                                    .and_then(|k| k.as_str())
                                    .is_some_and(|k| k.starts_with("allow"))
                            })
                            .or_else(|| opts.first())
                    })
                    .and_then(|o| o.get("optionId"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let _ = send(
                    &mut stdin,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "params": null,
                        "result": { "outcome": { "outcome": "selected", "optionId": option_id } }
                    }),
                )
                .await;
            }
            continue;
        }

        // Prompt completion.
        if value.get("id").and_then(|v| v.as_u64()) == Some(GROK_ACP_PROMPT_ID) {
            if let Some(err) = value.get("error") {
                transport_error = Some(format!("Grok ACP prompt failed: {err}"));
                break;
            }
            let result = value.get("result").cloned().unwrap_or_default();
            stop_reason = result
                .get("stopReason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if let Some(meta) = result.get("_meta") {
                if let Some(m) = meta.get("modelId").and_then(|v| v.as_str()) {
                    model_used = Some(m.to_string());
                }
                usage.input_tokens = usage_value_tokens(meta, &["inputTokens", "input_tokens"]);
                usage.output_tokens = usage_value_tokens(meta, &["outputTokens", "output_tokens"]);
                if !usage.has_usage() {
                    // Only a total is exposed: attribute it to input so cost
                    // estimation has something to work with.
                    usage.input_tokens = usage_value_tokens(meta, &["totalTokens", "total_tokens"]);
                }
            }
            break;
        }

        // Session updates arrive both as standard `session/update` and the
        // vendor-prefixed `_x.ai/session_notification` envelope.
        let update = match value.get("method").and_then(|v| v.as_str()) {
            Some("session/update") | Some("_x.ai/session_notification") => {
                value.pointer("/params/update").cloned()
            }
            _ => None,
        };
        let Some(update) = update else { continue };
        match update.get("sessionUpdate").and_then(|v| v.as_str()) {
            Some("agent_thought_chunk") => {
                if let Some(text) = update.pointer("/content/text").and_then(|v| v.as_str()) {
                    thinking_buffer.push_str(text);
                    thinking_done_emitted = false;
                    let _ = events_tx.send(AgentEvent::Thinking {
                        content: thinking_buffer.clone(),
                        done: false,
                        mission_id: Some(mission_id),
                    });
                }
            }
            Some("agent_message_chunk") => {
                if let Some(text) = update.pointer("/content/text").and_then(|v| v.as_str()) {
                    text_buffer.push_str(text);
                    let _ = events_tx.send(AgentEvent::TextDelta {
                        content: text_buffer.clone(),
                        mission_id: Some(mission_id),
                    });
                }
            }
            Some("tool_call") => {
                // Close the open thinking block: tool execution marks a
                // boundary, and the finalizer is the only persisted form.
                if !thinking_buffer.is_empty() && !thinking_done_emitted {
                    let _ = events_tx.send(thinking_final_event(
                        std::mem::take(&mut thinking_buffer),
                        mission_id,
                    ));
                    thinking_done_emitted = true;
                }
                let id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                let name = update
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let args = update.get("rawInput").cloned().unwrap_or_default();
                let _ = events_tx.send(AgentEvent::ToolCall {
                    tool_call_id: id.clone(),
                    name: name.clone(),
                    args,
                    mission_id: Some(mission_id),
                });
                tool_calls.insert(
                    id,
                    GrokAcpToolCall {
                        name,
                        latest_update: update,
                        result_emitted: false,
                    },
                );
            }
            Some("tool_call_update") => {
                let Some(id) = update.get("toolCallId").and_then(|v| v.as_str()) else {
                    continue;
                };
                let entry = tool_calls.entry(id.to_string()).or_default();
                if entry.name.is_empty() {
                    entry.name = update
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                }
                entry.latest_update = update.clone();
                if grok_acp_update_is_terminal(&update) && !entry.result_emitted {
                    entry.result_emitted = true;
                    let _ = events_tx.send(AgentEvent::ToolResult {
                        tool_call_id: id.to_string(),
                        name: entry.name.clone(),
                        result: update,
                        mission_id: Some(mission_id),
                    });
                }
            }
            _ => {}
        }
    }
    drop(stdin);
    let _ = child.wait().await;

    // The CLI doesn't always stamp a terminal status on the last
    // tool_call_update — flush whatever we have so every ToolCall gets a
    // matching ToolResult in the UI/history.
    for (id, call) in tool_calls.iter() {
        if !call.result_emitted {
            let _ = events_tx.send(AgentEvent::ToolResult {
                tool_call_id: id.clone(),
                name: call.name.clone(),
                result: call.latest_update.clone(),
                mission_id: Some(mission_id),
            });
        }
    }
    if !thinking_buffer.is_empty() && !thinking_done_emitted {
        let _ = events_tx.send(thinking_final_event(thinking_buffer.clone(), mission_id));
    }

    if let Some(err) = transport_error {
        let stderr_tail = stderr_tail.lock().await.trim().to_string();
        let detail = if stderr_tail.is_empty() {
            err
        } else {
            format!("{err}\nstderr tail:\n{stderr_tail}")
        };
        return Ok(AgentResult::failure(detail, 0)
            .with_terminal_reason(TerminalReason::LlmError)
            .with_model(model_used.unwrap_or_else(|| "grok-build".to_string())));
    }

    let final_text = text_buffer.trim().to_string();
    let (cost_cents, cost_source) =
        resolve_cost_cents_and_source(None, model_used.as_deref().or(Some("grok-build")), &usage);
    let mut result = if final_text.is_empty() && stop_reason.is_some() && !tool_calls.is_empty() {
        // Tool-only turn: the model acted but never emitted a final
        // message chunk. The work happened — surface it as success with a
        // synthetic summary instead of a phantom LLM error.
        let summary = format!(
            "Completed {} tool action(s) without a final text reply (stopReason: {}).",
            tool_calls.len(),
            stop_reason.as_deref().unwrap_or("unknown")
        );
        AgentResult::success(summary, cost_cents).with_terminal_reason(TerminalReason::TurnComplete)
    } else if final_text.is_empty() {
        AgentResult::failure(
            format!(
                "Grok completed the turn (stopReason: {}) without producing assistant text.",
                stop_reason.as_deref().unwrap_or("unknown")
            ),
            cost_cents,
        )
        .with_terminal_reason(TerminalReason::LlmError)
    } else {
        AgentResult::success(final_text, cost_cents)
            .with_terminal_reason(TerminalReason::TurnComplete)
    };
    result = result.with_cost_source(cost_source);
    let outcome = turn_outcome_for_result(
        &result,
        CompletionSignal::ProcessExit,
        CompletionConfidence::High,
    );
    result = result.with_turn_outcome(outcome);
    if usage.has_usage() {
        result = result.with_usage(usage);
    }
    result = result.with_model(model_used.unwrap_or_else(|| "grok-build".to_string()));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceType;
    use std::fs;

    fn container_workspace_at(path: &std::path::Path) -> Workspace {
        Workspace {
            id: Uuid::new_v4(),
            name: "verity".into(),
            workspace_type: WorkspaceType::Container,
            path: path.to_path_buf(),
            status: crate::workspace::WorkspaceStatus::Ready,
            error_message: None,
            config: serde_json::json!({}),
            template: None,
            distro: None,
            env_vars: Default::default(),
            init_scripts: Vec::new(),
            init_script: None,
            created_at: chrono::Utc::now(),
            skills: Vec::new(),
            plugins: Vec::new(),
            shared_network: None,
            tailscale_mode: None,
            mcps: Vec::new(),
            mcps_replace_defaults: true,
            config_profile: None,
            resolved_git_credentials: None,
            read_only_command_guard_dir: None,
            harness_versions: None,
        }
    }

    fn write_executable(path: &std::path::Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn grok_overlay_prefers_guest_path_over_host_opt_cli() {
        let root = tempfile::tempdir().unwrap();
        write_executable(
            &root.path().join("usr/local/bin/grok"),
            b"#!/bin/sh\necho grok\n",
        );
        let workspace = container_workspace_at(root.path());
        assert_eq!(
            grok_overlay_guest_path(&workspace, "/opt/grok-cli").as_deref(),
            Some("/usr/local/bin/grok")
        );
    }

    #[test]
    fn grok_overlay_ignores_absolute_opt_cli_even_when_present() {
        let root = tempfile::tempdir().unwrap();
        write_executable(
            &root.path().join("opt/grok-cli"),
            b"#!/bin/sh\necho host-grok\n",
        );
        let workspace = container_workspace_at(root.path());
        assert_eq!(
            grok_overlay_guest_path(&workspace, "/opt/grok-cli"),
            None,
            "overlay /opt/grok-cli must not become the guest exec path"
        );
    }

    #[test]
    fn grok_presence_never_returns_absolute_container_host_path() {
        assert_eq!(
            grok_cli_path_from_presence(
                true,
                "/opt/grok-cli",
                "/opt/grok-cli",
                CommandPresence::Present,
            ),
            None
        );
        assert_eq!(
            grok_cli_path_from_presence(true, "grok", "grok", CommandPresence::Present).as_deref(),
            Some("grok")
        );
        assert_eq!(
            grok_cli_path_from_presence(
                false,
                "/opt/grok-cli",
                "/opt/grok-cli",
                CommandPresence::Present,
            )
            .as_deref(),
            Some("/opt/grok-cli")
        );
    }

    #[tokio::test]
    async fn ensure_grok_cli_prefers_guest_overlay_over_host_opt_path() {
        let root = tempfile::tempdir().unwrap();
        write_executable(
            &root.path().join("usr/local/bin/grok"),
            b"#!/bin/sh\necho grok\n",
        );
        let exec = WorkspaceExec::new(container_workspace_at(root.path()));
        let path = ensure_grok_cli_available(&exec, root.path(), "/opt/grok-cli")
            .await
            .expect("overlay grok must be used");
        assert_eq!(path, "/usr/local/bin/grok");
    }

    #[tokio::test]
    async fn ensure_grok_cli_never_returns_opt_path_when_overlay_has_no_grok() {
        let root = tempfile::tempdir().unwrap();
        write_executable(
            &root.path().join("opt/grok-cli"),
            b"#!/bin/sh\necho host-grok\n",
        );
        let prev = std::env::var("SANDBOXED_SH_AUTO_INSTALL_GROK").ok();
        std::env::set_var("SANDBOXED_SH_AUTO_INSTALL_GROK", "0");
        let exec = WorkspaceExec::new(container_workspace_at(root.path()));
        let result = ensure_grok_cli_available(&exec, root.path(), "/opt/grok-cli").await;
        match prev {
            Some(value) => std::env::set_var("SANDBOXED_SH_AUTO_INSTALL_GROK", value),
            None => std::env::remove_var("SANDBOXED_SH_AUTO_INSTALL_GROK"),
        }
        match result {
            Ok(path) => assert_ne!(
                path, "/opt/grok-cli",
                "overlay-miss must not exec the host path"
            ),
            Err(_) => {}
        }
    }

    #[test]
    fn grok_event_reasoning_handles_streaming_json_thought_events() {
        // Real event captured from grok 0.2.16 `--output-format streaming-json`
        // with grok-build-0.1: thinking arrives as type "thought" with the
        // chunk in `data`. This was silently dropped before.
        let event = serde_json::json!({ "type": "thought", "data": "The user wants" });
        assert_eq!(
            grok_event_reasoning(&event).as_deref(),
            Some("The user wants")
        );
        assert_eq!(grok_event_text(&event), None);
    }

    #[test]
    fn grok_acp_terminal_update_detection() {
        assert!(grok_acp_update_is_terminal(
            &serde_json::json!({ "sessionUpdate": "tool_call_update", "status": "completed" })
        ));
        assert!(grok_acp_update_is_terminal(
            &serde_json::json!({ "status": "failed" })
        ));
        // Real captured update without a status stamp — not terminal; the
        // end-of-turn flush covers it.
        assert!(!grok_acp_update_is_terminal(&serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "kind": "edit",
            "title": "Write `/tmp/x`",
            "content": [{ "type": "diff", "path": "/tmp/x", "oldText": "", "newText": "delta" }]
        })));
    }
}
