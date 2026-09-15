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
    // Relative cli_path overrides (e.g. `custom-grok`) must win over the
    // ordinary overlay binary. Absolute configured programs are host paths
    // and are never overlay-looked-up.
    let overlay = if program.starts_with('/') {
        container_overlay_command_path(workspace, "/usr/local/bin/grok")
            .or_else(|| container_overlay_command_path(workspace, "grok"))?
    } else {
        container_overlay_command_path(workspace, program)
            .or_else(|| container_overlay_command_path(workspace, "/usr/local/bin/grok"))
            .or_else(|| container_overlay_command_path(workspace, "grok"))?
    };
    Some(container_overlay_guest_path(workspace, &overlay, program))
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

/// Resolve API-key authentication separately from native Grok auth files.
/// OAuth access tokens are not API keys; native authentication and refresh
/// belong to the CLI that owns the session and its auth-file format.
#[allow(clippy::result_large_err)]
async fn prepare_grok_env(
    workspace: &Workspace,
    app_working_dir: &std::path::Path,
    mission_id: Uuid,
) -> Result<HashMap<String, String>, AgentResult> {
    let mut env = HashMap::new();
    let xai_api_key = crate::api::ai_providers::get_xai_api_key_for_grok(app_working_dir)
        .or_else(|| {
            workspace
                .env_vars
                .get("XAI_API_KEY")
                .filter(|key| !key.trim().is_empty())
                .cloned()
        })
        .or_else(|| {
            workspace
                .env_vars
                .get("GROK_CODE_XAI_API_KEY")
                .filter(|key| !key.trim().is_empty())
                .cloned()
        })
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
    } else if let Err(error) =
        crate::api::ai_providers::sync_host_grok_auth_into_workspace(workspace)
    {
        tracing::warn!(mission_id = %mission_id, "Grok native authentication preparation failed");
        return Err(AgentResult::failure(
            format!("Grok native authentication could not be prepared: {error}"),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError));
    }

    // Both native transports must inherit the same server-owned policy,
    // wrapper path and mission-scoped capability as durable workspace jobs.
    if let Some(remote_env) = workspace.remote_build_env(mission_id) {
        env.extend(remote_env);
    }

    Ok(env)
}

async fn persist_grok_session(
    store: Option<&std::sync::Arc<dyn crate::api::mission_store::MissionStore>>,
    mission_id: Uuid,
    session_id: &str,
) -> Result<(), String> {
    if let Some(store) = store {
        return store
            .update_mission_session_id(
                mission_id,
                session_id,
                "grok",
                super::session_update_run().as_ref(),
            )
            .await
            .and_then(|accepted| {
                if accepted {
                    Ok(())
                } else {
                    Err("stale or unattributed execution generation".into())
                }
            })
            .map_err(|error| format!("Grok native session could not be persisted: {error}"));
    }
    // Protocol-only subprocess fixtures do not own a mission store. Real
    // launch paths must provide the actor's authoritative store.
    #[cfg(test)]
    {
        Ok(())
    }
    #[cfg(not(test))]
    {
        Err("Grok session persistence store is unavailable".into())
    }
}

async fn grok_session_for_turn(
    store: &Arc<dyn crate::api::mission_store::MissionStore>,
    mission_id: Uuid,
) -> Result<Option<String>, String> {
    let mission = store
        .get_mission(mission_id)
        .await?
        .ok_or("Grok mission is missing")?;
    if mission.backend != "grok" {
        return Err("Grok no longer owns this mission backend".into());
    }
    if mission
        .session_id
        .as_deref()
        .is_some_and(|id| id.trim().is_empty())
    {
        return Err("Grok native identity is empty; reconciliation required".into());
    }
    if mission.session_id.is_none() && store.native_prompt_attempted(mission_id, "grok").await? {
        return Err("Prior Grok native attempt has no durable identity; session creation or prompt outcome requires reconciliation".into());
    }
    Ok(mission.session_id)
}

async fn claim_grok_prompt(
    store: &Arc<dyn crate::api::mission_store::MissionStore>,
    mission_id: Uuid,
    session_id: Option<&str>,
) -> Result<(), String> {
    if store
        .claim_native_prompt(
            mission_id,
            "grok",
            session_id,
            super::session_update_run().as_ref(),
        )
        .await?
    {
        Ok(())
    } else {
        Err("Grok prompt refused: stale generation, changed binding or prior unbound prompt outcome".into())
    }
}

/// Execute a turn using the Grok Build CLI backend.
///
/// Dispatches to the ACP path (`grok agent stdio`) by default — it is the
/// only mode that surfaces tool calls and works for thinking on every model.
/// Set `SANDBOXED_SH_GROK_ACP=0` to force the legacy `--output-format
/// streaming-json` path; the dispatcher also falls back to it automatically
/// when ACP fails before prompt delivery and native continuity permits fallback.
#[allow(clippy::too_many_arguments)]
pub async fn run_grok_turn(
    mission_store: std::sync::Arc<dyn crate::api::mission_store::MissionStore>,
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    requested_session_id: Option<&str>,
    _is_continuation: bool,
) -> AgentResult {
    // Canonical native identity and durable prompt provenance, never history
    // from another harness, decide whether a fresh session is safe.
    let saved_session = match grok_session_for_turn(&mission_store, mission_id).await {
        Ok(id) => id,
        Err(error) => {
            return AgentResult::failure(error, 0)
                .with_terminal_reason(TerminalReason::NativeContinuityRequired)
        }
    };
    if requested_session_id.is_some() && requested_session_id != saved_session.as_deref() {
        return AgentResult::failure(
            "Grok supplied native identity disagrees with its durable binding",
            0,
        )
        .with_terminal_reason(TerminalReason::NativeContinuityRequired);
    }
    let session_id = saved_session.as_deref();
    let is_continuation = session_id.is_some();

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
            &mission_store,
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
                    continuity_required = fallback.continuity_required,
                    "Grok ACP setup failed; evaluating native continuity before transport fallback"
                );
                if fallback.continuity_required {
                    return AgentResult::failure(fallback.reason, 0)
                        .with_terminal_reason(TerminalReason::NativeContinuityRequired);
                }
                // ACP may have committed session/new before a pre-prompt
                // failure. Reload that exact binding before transport fallback.
                let fallback_session = match grok_session_for_turn(&mission_store, mission_id).await
                {
                    Ok(id) => id,
                    Err(error) => {
                        return AgentResult::failure(error, 0)
                            .with_terminal_reason(TerminalReason::NativeContinuityRequired)
                    }
                };
                return run_grok_streaming_json_turn(
                    &mission_store,
                    workspace,
                    work_dir,
                    message,
                    model,
                    mission_id,
                    events_tx,
                    cancel,
                    app_working_dir,
                    fallback_session.as_deref(),
                    fallback_session.is_some(),
                )
                .await;
            }
        }
    }
    run_grok_streaming_json_turn(
        &mission_store,
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
    mission_store: &std::sync::Arc<dyn crate::api::mission_store::MissionStore>,
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
    // Continuations must load this exact native ID. --session-id is an
    // new-session option and --continue picks an arbitrary latest session; neither proves
    // that the original native history is being continued.
    if let Some(sid) = session_id {
        args.push("--resume".to_string());
        args.push(sid.to_string());
    } else if is_continuation {
        return AgentResult::failure("Grok continuation has no native session identity", 0)
            .with_terminal_reason(TerminalReason::NativeContinuityRequired);
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

    // Prepare provider authentication and the same mission-scoped remote
    // build context used by ACP and the other native runners.
    let env = match prepare_grok_env(workspace, app_working_dir, mission_id).await {
        Ok(env) => env,
        Err(result) => return result,
    };

    // streaming-json passes the prompt on argv: record uncertainty before
    // spawn, because the first observable output may follow arbitrary tools.
    if let Err(error) = claim_grok_prompt(mission_store, mission_id, session_id).await {
        return AgentResult::failure(error, 0)
            .with_terminal_reason(TerminalReason::NativeContinuityRequired);
    }

    let child = match workspace_exec
        .spawn_streaming(work_dir, &cli_path, &args, env)
        .await
    {
        Ok(child) => child,
        Err(e) => {
            return AgentResult::failure(format!("Failed to start Grok Build CLI: {}", e), 0)
                .with_terminal_reason(TerminalReason::NativeContinuityRequired);
        }
    };
    run_grok_streaming_process(
        Some(mission_store),
        child,
        model,
        mission_id,
        events_tx,
        cancel,
        session_id.is_some(),
    )
    .await
}

async fn run_grok_streaming_process(
    mission_store: Option<&std::sync::Arc<dyn crate::api::mission_store::MissionStore>>,
    mut child: tokio::process::Child,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    is_resume: bool,
) -> AgentResult {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let unbound_start = mission_store.is_some() && !is_resume;
    drop(child.stdin.take());

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return AgentResult::failure("Failed to capture Grok stdout".to_string(), 0)
                .with_terminal_reason(if unbound_start {
                    TerminalReason::NativeContinuityRequired
                } else {
                    TerminalReason::LlmError
                });
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
                .with_terminal_reason(if unbound_start { TerminalReason::NativeContinuityRequired } else { TerminalReason::LlmError });
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
                                    .with_terminal_reason(if unbound_start { TerminalReason::NativeContinuityRequired } else { TerminalReason::LlmError });
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
                            if let Err(error) = persist_grok_session(mission_store, mission_id, &sid).await {
                                let _ = child.kill().await;
                                if let Some(handle) = stderr_handle.take() { handle.abort(); }
                                return AgentResult::failure(error, 0)
                                    .with_terminal_reason(TerminalReason::NativeContinuityRequired);
                            }
                            let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                                run: crate::api::runners::session_update_run(),
                                backend: "grok".to_string(),
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

    let missing_resume = is_resume && {
        let stderr = stderr_capture.lock().await;
        [&*stderr, &final_result].iter().any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("no session found")
                || lower.contains("session not found")
                || lower.contains("session does not exist")
        })
    };
    let unbound_outcome = if let Some(store) = mission_store {
        !matches!(store.get_mission(mission_id).await, Ok(Some(m)) if m.session_id.is_some())
    } else {
        false
    };
    let success = exit_status.map(|status| status.success()).unwrap_or(false) && !had_error;
    let model_for_cost = model_used.as_deref().or(Some("grok-build"));
    let (cost_cents, cost_source) =
        resolve_cost_cents_and_source(None, model_for_cost, &token_usage);
    let mut result = if unbound_outcome {
        AgentResult::failure(format!("Grok prompt outcome has no durable native identity; reconciliation required. {final_result}"), cost_cents)
            .with_cost_source(cost_source)
            .with_terminal_reason(TerminalReason::NativeContinuityRequired)
    } else if success {
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
            .with_terminal_reason(if missing_resume {
                TerminalReason::NativeContinuityRequired
            } else {
                TerminalReason::LlmError
            })
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

/// Handshake failures for an exact native session require reconciliation;
/// neither latest-session continuation nor session-ID upsert is permitted.
const GROK_ACP_CONTINUITY_REQUIRED: &str = "native session requires reconciliation";

/// Why the ACP path handed the turn back to the streaming-json fallback.
pub(crate) struct GrokAcpFallback {
    reason: String,
    /// True when falling back could replace the original native history.
    continuity_required: bool,
}

impl From<String> for GrokAcpFallback {
    fn from(reason: String) -> Self {
        Self {
            reason,
            continuity_required: false,
        }
    }
}

/// Per-call state for an in-flight grok ACP tool call.
#[derive(Default, Clone)]
struct GrokAcpToolCall {
    name: String,
    latest_update: serde_json::Value,
    result_emitted: bool,
    started_at: Option<tokio::time::Instant>,
    deadline: Option<tokio::time::Instant>,
    in_progress: bool,
}

#[derive(Clone, Copy)]
struct GrokAcpIdlePolicy {
    diagnostic: std::time::Duration,
    transport: std::time::Duration,
    tool_grace: std::time::Duration,
    shutdown: std::time::Duration,
}

impl Default for GrokAcpIdlePolicy {
    fn default() -> Self {
        Self {
            diagnostic: std::time::Duration::from_secs(180),
            transport: std::time::Duration::from_secs(600),
            tool_grace: std::time::Duration::from_secs(30),
            shutdown: std::time::Duration::from_secs(10),
        }
    }
}

impl GrokAcpToolCall {
    fn observe(
        &mut self,
        update: &serde_json::Value,
        now: tokio::time::Instant,
        policy: GrokAcpIdlePolicy,
    ) {
        // ACP updates are partial. In particular, the in_progress update can
        // omit the rawInput supplied with the original pending tool call.
        if let (Some(current), Some(delta)) =
            (self.latest_update.as_object_mut(), update.as_object())
        {
            current.extend(delta.clone());
        } else {
            self.latest_update = update.clone();
        }
        match update.get("status").and_then(|v| v.as_str()) {
            Some("in_progress") if !self.result_emitted => {
                self.in_progress = true;
                let started = *self.started_at.get_or_insert(now);
                // Grok Bash `timeout` and task-output `timeout_ms` are in
                // milliseconds. Zero/missing/invalid means no observed finite
                // deadline, not an unlimited exemption from transport checks.
                let input = &self.latest_update["rawInput"];
                let timeout = input.get("timeout_ms").or_else(|| input.get("timeout"));
                let millis = timeout.and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                });
                self.deadline = millis.filter(|ms| *ms > 0).and_then(|ms| {
                    started
                        .checked_add(std::time::Duration::from_millis(ms))?
                        .checked_add(policy.tool_grace)
                });
            }
            Some("completed" | "failed" | "pending") => {
                self.in_progress = false;
                self.deadline = None;
            }
            _ => {}
        }
    }
}

fn grok_acp_idle_deadline(
    last_event: tokio::time::Instant,
    calls: &HashMap<String, GrokAcpToolCall>,
    policy: GrokAcpIdlePolicy,
) -> tokio::time::Instant {
    calls
        .values()
        .filter(|call| call.in_progress && !call.result_emitted)
        .filter_map(|call| call.deadline)
        .fold(last_event + policy.transport, std::cmp::max)
}

fn grok_acp_note_activity(
    last_event: &mut tokio::time::Instant,
    diagnostic_emitted: &mut bool,
    events: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
) {
    *last_event = tokio::time::Instant::now();
    if std::mem::take(diagnostic_emitted) {
        let _ = events.send(AgentEvent::AgentPhase {
            phase: "executing".to_string(),
            detail: Some("Grok activity resumed".to_string()),
            agent: Some("grok".to_string()),
            mission_id: Some(mission_id),
        });
    }
}

fn grok_acp_update_is_terminal(update: &serde_json::Value) -> bool {
    matches!(
        update.get("status").and_then(|v| v.as_str()),
        Some("completed") | Some("failed")
    )
}

/// Execute a turn over `grok agent stdio` (ACP JSON-RPC).
///
/// Pre-prompt failures return a fallback decision. Uncertain native creation
/// or identity requires reconciliation; only safe failures may fall back to
/// streaming after reloading the canonical binding and claiming durable intent.
#[allow(clippy::too_many_arguments)]
async fn run_grok_acp_turn(
    mission_store: &std::sync::Arc<dyn crate::api::mission_store::MissionStore>,
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
    let workspace_exec = WorkspaceExec::new(workspace.clone());

    let cli_path =
        get_backend_string_setting("grok", "cli_path").unwrap_or_else(|| "grok".to_string());
    let cli_path = ensure_grok_cli_available(&workspace_exec, work_dir, &cli_path)
        .await
        .map_err(|e| format!("grok CLI unavailable: {e}"))?;

    let env = match prepare_grok_env(workspace, app_working_dir, mission_id).await {
        Ok(env) => env,
        // Auth failures are terminal for BOTH paths — surface them directly
        // instead of falling back into the same failure.
        Err(result) => return Ok(result),
    };

    // `grok agent stdio` accepts no further flags (verified: it rejects
    // --no-auto-update). cwd comes from spawn_streaming's working dir and
    // the session/new params.
    let args = vec!["agent".to_string(), "stdio".to_string()];
    let child = workspace_exec
        .spawn_streaming(work_dir, &cli_path, &args, env)
        .await
        .map_err(|e| format!("failed to spawn grok agent stdio: {e}"))?;

    run_grok_acp_process(
        Some(mission_store),
        child,
        &workspace_exec.translate_path_for_container(work_dir),
        message,
        model,
        mission_id,
        events_tx,
        cancel,
        session_id,
        is_continuation,
        GrokAcpIdlePolicy::default(),
    )
    .await
}

// Kept below discovery/auth so the real protocol and process lifecycle can be
// exercised with a local ACP fixture, without credentials or inference calls.
#[allow(clippy::too_many_arguments)]
async fn run_grok_acp_process(
    mission_store: Option<&std::sync::Arc<dyn crate::api::mission_store::MissionStore>>,
    mut child: tokio::process::Child,
    acp_cwd: &str,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    session_id: Option<&str>,
    is_continuation: bool,
    idle_policy: GrokAcpIdlePolicy,
) -> Result<AgentResult, GrokAcpFallback> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err("failed to capture Grok ACP pipes".to_string().into());
    };
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
    let mut session_creation_attempted = false;
    let handshake: Result<(String, bool), String> = async {
        let session_load_failed = false;
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
                Err(err) => {
                    // A missing exact session is not permission to continue
                    // an arbitrary last session or create an empty replacement.
                    return Err(format!(
                        "stored session {sid} not loadable over ACP on a continuation \
                         turn ({err}); {GROK_ACP_CONTINUITY_REQUIRED}"
                    ));
                }

            }
        } else if is_continuation {
            // An asserted continuation without an exact ID is ambiguous.
            return Err(format!(
                "continuation turn without a stored session id; {GROK_ACP_CONTINUITY_REQUIRED}"
            ));
        }
        if acp_session_id.is_none() {
            // session/new itself creates native state. Record intent before
            // the RPC so a lost response cannot authorize another fresh entry.
            if let Some(store) = mission_store {
                claim_grok_prompt(store, mission_id, None).await
                    .map_err(|error| format!("{error}; {GROK_ACP_CONTINUITY_REQUIRED}"))?;
            }
            session_creation_attempted = true;
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
            persist_grok_session(mission_store, mission_id, &sid)
                .await.map_err(|error| format!("{error}; {GROK_ACP_CONTINUITY_REQUIRED}"))?;
            let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                run: crate::api::runners::session_update_run(),
                backend: "grok".to_string(),
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
    let (acp_session_id, _session_load_failed) = match handshake {
        Ok(pair) => pair,
        Err(err) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let continuity_required =
                session_creation_attempted || err.contains(GROK_ACP_CONTINUITY_REQUIRED);
            return Err(GrokAcpFallback {
                reason: err,
                continuity_required,
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
        if let Err(error) = send(
            &mut stdin,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": GROK_ACP_SET_MODEL_ID,
                "method": "session/set_model",
                "params": { "sessionId": acp_session_id, "modelId": model }
            }),
        )
        .await
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error.into());
        }
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

    if let Some(store) = mission_store {
        if let Err(error) = claim_grok_prompt(store, mission_id, Some(&acp_session_id)).await {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Ok(AgentResult::failure(error, 0)
                .with_terminal_reason(TerminalReason::NativeContinuityRequired));
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
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Ok(AgentResult::failure(
            format!("Grok prompt delivery outcome is unknown: {e}"),
            0,
        )
        .with_terminal_reason(TerminalReason::NativeContinuityRequired));
    }

    let mut thinking_buffer = String::new();
    let mut thinking_done_emitted = false;
    let mut text_buffer = String::new();
    let mut tool_calls: HashMap<String, GrokAcpToolCall> = HashMap::new();
    let mut model_used: Option<String> = selected_model;
    let mut usage = crate::cost::TokenUsage::default();
    let mut stop_reason: Option<String> = None;
    let mut transport_error: Option<String> = None;
    let mut transport_failure_stage = "stream_closed";
    let mut last_event = tokio::time::Instant::now();
    let mut idle_diagnostic_emitted = false;
    let mut awaiting_input = false;

    loop {
        let deadline = grok_acp_idle_deadline(last_event, &tool_calls, idle_policy);
        let next_check = if idle_diagnostic_emitted {
            deadline
        } else {
            last_event + idle_policy.diagnostic
        };
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
            _ = tokio::time::sleep_until(next_check) => {
                let idle_secs = last_event.elapsed().as_secs();
                let wait_state = if awaiting_input {
                    "awaiting_client_input"
                } else if tool_calls.values().any(|call| call.in_progress && !call.result_emitted) {
                    "awaiting_tool_results"
                } else {
                    "awaiting_inference"
                };
                if tokio::time::Instant::now() >= deadline {
                    transport_failure_stage = "grok_acp_transport_idle";
                    transport_error = Some(format!(
                        "Grok ACP transport-idle timeout after {idle_secs}s without a protocol event \
                         ({wait_state}); no active tool has an unexpired observed deadline. \
                         Checkout and session are preserved; reconcile existing work before recovery."
                    ));
                    let _ = child.kill().await;
                    break;
                }
                // Process existence is diagnostic, not proof of useful work.
                // A healthy inference or foreground tool can be silent for
                // longer than 180s. Only its hard deadline can terminate it.
                let process_running = matches!(child.try_wait(), Ok(None));
                tracing::warn!(
                    %mission_id,
                    idle_secs,
                    wait_state,
                    process_running,
                    remaining_secs = deadline.saturating_duration_since(tokio::time::Instant::now()).as_secs(),
                    "Grok ACP is silent; continuing to observe until the transport/tool deadline"
                );
                let _ = events_tx.send(AgentEvent::AgentPhase {
                    phase: "waiting".to_string(),
                    detail: Some(format!("Grok has been silent for {idle_secs}s ({wait_state}); observing until its deadline.")),
                    agent: Some("grok".to_string()),
                    mission_id: Some(mission_id),
                });
                idle_diagnostic_emitted = true;
                continue;
            }
            line = lines.next_line() => match line {
                Err(e) => {
                    transport_failure_stage = "stdout_read";
                    transport_error = Some(format!("Grok ACP stdout read failed: {e}"));
                    break;
                }
                Ok(None) => {
                    if stop_reason.is_none() {
                        transport_error =
                            Some("Grok ACP stream closed before the prompt completed".to_string());
                    }
                    break;
                }
                Ok(Some(line)) => line,
            }
        };

        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };

        // Incoming request FROM the agent (has both id and method): the only
        // one we expect is a permission prompt — auto-approve it, mirroring
        // the streaming path's --always-approve.
        if let (Some(req_id), Some(method)) = (value.get("id"), value.get("method")) {
            grok_acp_note_activity(
                &mut last_event,
                &mut idle_diagnostic_emitted,
                &events_tx,
                mission_id,
            );
            awaiting_input = true;
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
                let response = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        let _ = child.kill().await;
                        return Ok(AgentResult::failure("Mission cancelled", 0)
                            .with_terminal_reason(TerminalReason::Cancelled));
                    }
                    response = tokio::time::timeout(idle_policy.transport, send(&mut stdin, serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "params": null,
                        "result": { "outcome": { "outcome": "selected", "optionId": option_id } }
                    }))) => response.unwrap_or_else(|_| Err("permission response write timed out".to_string())),
                };
                if let Err(error) = response {
                    transport_failure_stage = "permission_response";
                    transport_error = Some(format!("Grok ACP permission response failed: {error}"));
                    break;
                }
                awaiting_input = false;
            }
            continue;
        }

        // Prompt completion.
        if value.get("id").and_then(|v| v.as_u64()) == Some(GROK_ACP_PROMPT_ID) {
            if let Some(err) = value.get("error") {
                // A JSON-RPC error is a provider/agent response, not proof of
                // a broken transport. Do not auto-replay it as an idle retry.
                transport_failure_stage = "prompt_error";
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
        grok_acp_note_activity(
            &mut last_event,
            &mut idle_diagnostic_emitted,
            &events_tx,
            mission_id,
        );
        awaiting_input = false;
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
                let entry = tool_calls.entry(id).or_default();
                entry.name = name;
                entry.observe(&update, last_event, idle_policy);
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
                entry.observe(&update, last_event, idle_policy);
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
    // EOF and a terminal response must not hang forever on a CLI/descendant
    // that keeps the process alive. This tears down only our harness process;
    // it never resets or removes the mission checkout/session.
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Ok(AgentResult::failure("Mission cancelled", 0)
                .with_terminal_reason(TerminalReason::Cancelled));
        }
        exit = tokio::time::timeout(idle_policy.shutdown, child.wait()) => {
            if exit.is_err() {
                let _ = child.kill().await;
            }
        }
    }

    // Missing terminal tool evidence is unknown, not a synthetic ToolResult.
    // Retain the unresolved call in the durable event stream and block replay.
    let pending_tools = tool_calls
        .values()
        .filter(|call| !call.result_emitted)
        .count();
    if pending_tools > 0 && transport_error.is_none() {
        transport_error = Some("Grok ended with unknown native tool outcomes".into());
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
        return Ok(AgentResult::failure(detail.clone(), 0)
            .with_terminal_reason(if pending_tools > 0 {
                TerminalReason::NativeContinuityRequired
            } else {
                TerminalReason::LlmError
            })
            .with_terminal_evidence(detail)
            .with_data(serde_json::json!({
                "failure_class": if transport_failure_stage == "prompt_error" {
                    "provider_error"
                } else {
                    "transport_error"
                },
                "transport_failure_stage": transport_failure_stage,
                "grok_acp_transport_failure": true,
                "pending_tools": pending_tools,
                "idle_seconds": last_event.elapsed().as_secs(),
                "awaiting_input": awaiting_input,
            }))
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

    #[tokio::test]
    async fn grok_native_remote_env_reaches_workspace_children() {
        const CASE: &str = "GROK_REMOTE_ENV_FIXTURE";
        let Ok(case) = std::env::var(CASE) else {
            // Isolate signing configuration from the test runner and never
            // read shared provider credentials or mutate process-global env.
            for case in ["configured", "unconfigured"] {
                let result = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "api::runners::grok::tests::grok_native_remote_env_reaches_workspace_children", "--nocapture"])
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env(CASE, case)
                    .envs(if case == "configured" {
                        vec![("PORT", "19876"), ("SANDBOXED_INTERNAL_ACTION_SECRET", "synthetic-remote-signing-secret")]
                    } else {
                        vec![]
                    })
                    .output().unwrap();
                assert!(
                    result.status.success(),
                    "{case}: {}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            return;
        };
        for mode in ["host", "fallback", "container"] {
            let root = tempfile::tempdir().unwrap();
            let mission_id = Uuid::new_v4();
            let mut workspace = if mode == "host" {
                Workspace::default_host(root.path().to_path_buf())
            } else {
                Workspace::new_container("remote-env-fixture".into(), root.path().to_path_buf())
            };
            workspace.config = serde_json::json!({
                "compute_policy": "remote_required",
                "container_fallback": mode == "fallback",
                "remote_build": {"node_id": "auto", "requirements": ["lean"], "timeout_secs": 321}
            });
            workspace
                .env_vars
                .insert("HOME".into(), root.path().display().to_string());
            workspace
                .env_vars
                .insert("XAI_API_KEY".into(), "synthetic-provider-key".into());
            // Caller/workspace settings cannot downgrade the authoritative policy
            // or replace the mission identity passed by the runner.
            workspace
                .env_vars
                .insert("SANDBOXED_COMPUTE_POLICY".into(), "local_allowed".into());
            workspace
                .env_vars
                .insert("REMOTE_BUILD_MISSION_ID".into(), Uuid::new_v4().to_string());
            let env = prepare_grok_env(&workspace, root.path(), mission_id)
                .await
                .unwrap_or_else(|_| panic!("fixture preparation failed"));
            assert_eq!(
                env.get("SANDBOXED_COMPUTE_POLICY").map(String::as_str),
                Some("remote_required")
            );
            assert_eq!(
                env.get("REMOTE_BUILD_MISSION_ID"),
                Some(&mission_id.to_string())
            );
            assert_eq!(
                env.get("REMOTE_BUILD_TIMEOUT_SECS").map(String::as_str),
                Some("321")
            );
            assert_eq!(env.contains_key("REMOTE_BUILD_URL"), case == "configured");
            assert_eq!(env.contains_key("REMOTE_BUILD_TOKEN"), case == "configured");
            let wrapper = env
                .get("REMOTE_BUILD_COMMAND")
                .expect("mission wrapper path");
            assert!(env["PATH"].starts_with(&format!(
                "{}:",
                std::path::Path::new(wrapper).parent().unwrap().display()
            )));
            if case == "configured" {
                assert_eq!(
                    env["REMOTE_BUILD_URL"],
                    format!(
                        "http://{}:19876/api/remote-build",
                        workspace.host_ip_from_workspace()
                    )
                );
                use base64::Engine;
                let payload = env["REMOTE_BUILD_TOKEN"].split('.').next().unwrap();
                let claims: serde_json::Value = serde_json::from_slice(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(payload)
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(claims["mission_id"], mission_id.to_string());
                assert!(claims["expires_at"].as_i64().unwrap() > chrono::Utc::now().timestamp());
            }
            if mode != "container" {
                let child = WorkspaceExec::new(workspace).spawn_streaming(
                    root.path(), "/bin/sh", &["-c".into(),
                        format!("test \"$SANDBOXED_COMPUTE_POLICY\" = remote_required && test \"$REMOTE_BUILD_MISSION_ID\" = {mission_id} && test -n \"$REMOTE_BUILD_COMMAND\" && test \"${{REMOTE_BUILD_URL:+yes}}\" = {} && test \"${{REMOTE_BUILD_TOKEN:+yes}}\" = {}", if case == "configured" { "yes" } else { "\"\"" }, if case == "configured" { "yes" } else { "\"\"" })], env
                ).await.unwrap();
                assert!(
                    child.wait_with_output().await.unwrap().status.success(),
                    "{mode}/{case}: child environment"
                );
            }
        }
    }

    #[tokio::test]
    async fn grok_unbound_prompt_survives_death_reopen_and_fences_retry() {
        use crate::api::mission_store::{
            FileMissionStore, InMemoryMissionStore, MissionStore, SessionUpdateRun,
            SqliteMissionStore,
        };
        use std::process::Stdio;
        for kind in ["memory", "file", "sqlite"] {
            for outcome in ["death", "success_without_id", "persistence_failure"] {
                let root = tempfile::tempdir().unwrap();
                let mut store: Arc<dyn MissionStore> = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(root.path().into(), "unknown")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(root.path().into(), "unknown")
                            .await
                            .unwrap(),
                    ),
                    _ => Arc::new(InMemoryMissionStore::new()),
                };
                // History/identity from Codex is not evidence of prior Grok effects.
                let mission = store
                    .create_mission(Some("handoff"), None, None, None, None, Some("codex"), None)
                    .await
                    .unwrap();
                store
                    .update_mission_run_settings(
                        mission.id,
                        Some("grok"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        "generic-not-native",
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    grok_session_for_turn(&store, mission.id).await.unwrap(),
                    None
                );
                let run = store
                    .begin_mission_run(mission.id, "first", None)
                    .await
                    .unwrap();
                let stamp = SessionUpdateRun::from(&run);
                let stale = SessionUpdateRun {
                    run_id: Uuid::new_v4(),
                    generation: run.generation,
                };
                assert!(crate::api::runners::SESSION_UPDATE_RUN
                    .scope(Some(stale), claim_grok_prompt(&store, mission.id, None))
                    .await
                    .is_err());
                assert!(!store
                    .native_prompt_attempted(mission.id, "grok")
                    .await
                    .unwrap());

                crate::api::runners::SESSION_UPDATE_RUN
                    .scope(
                        Some(stamp.clone()),
                        claim_grok_prompt(&store, mission.id, None),
                    )
                    .await
                    .unwrap();
                assert!(store
                    .native_prompt_attempted(mission.id, "grok")
                    .await
                    .unwrap());
                assert!(
                    crate::api::runners::SESSION_UPDATE_RUN
                        .scope(
                            Some(stamp.clone()),
                            claim_grok_prompt(&store, mission.id, None)
                        )
                        .await
                        .is_err(),
                    "a second unbound launch in the same run must be fenced"
                );
                // Model a newer lease making the old process's later ID write
                // fail; its earlier prompt/tool acceptance must still be fenced.
                if outcome == "persistence_failure" {
                    store
                        .finish_mission_run(run.run_id, run.generation, Some("fixture"))
                        .await
                        .unwrap();
                    store
                        .begin_mission_run(mission.id, "successor", None)
                        .await
                        .unwrap();
                }
                let script = r#"import pathlib,sys,json
p=pathlib.Path('.')
with (p/'accepted-prompts').open('a') as f: f.write('accepted\n')
with (p/'tool-effects').open('a') as f: f.write('effect\n')
if sys.argv[1]=='persistence_failure': print(json.dumps({'session_id':'late-native','type':'text','text':'done'}), flush=True)
else: print(json.dumps({'type':'text','text':'accepted tool'}), flush=True)
sys.exit(0 if sys.argv[1]=='success_without_id' else 1)
"#;
                let child = tokio::process::Command::new("python3")
                    .args(["-u", "-c", script, outcome])
                    .current_dir(root.path())
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let (events, _) = broadcast::channel(32);
                let result = crate::api::runners::SESSION_UPDATE_RUN
                    .scope(
                        Some(stamp),
                        run_grok_streaming_process(
                            Some(&store),
                            child,
                            None,
                            mission.id,
                            events,
                            CancellationToken::new(),
                            false,
                        ),
                    )
                    .await;
                assert_eq!(
                    result.terminal_reason,
                    Some(TerminalReason::NativeContinuityRequired),
                    "{kind}/{outcome}"
                );
                assert!(store
                    .get_mission(mission.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .session_id
                    .is_none());
                store = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(root.path().into(), "unknown")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(root.path().into(), "unknown")
                            .await
                            .unwrap(),
                    ),
                    _ => store,
                };
                // Model the retry's real entry gate before any prompt process
                // can be launched. A failed guard must leave exactly one effect.
                if grok_session_for_turn(&store, mission.id).await.is_ok() {
                    let _ = tokio::process::Command::new("python3")
                        .args(["-c", script, outcome])
                        .current_dir(root.path())
                        .output()
                        .await
                        .unwrap();
                    panic!("retry allowed a second prompt: {kind}/{outcome}");
                }
                assert_eq!(
                    std::fs::read_to_string(root.path().join("accepted-prompts")).unwrap(),
                    "accepted\n"
                );
                assert_eq!(
                    std::fs::read_to_string(root.path().join("tool-effects")).unwrap(),
                    "effect\n"
                );
                store
                    .update_mission_run_settings(
                        mission.id,
                        Some("codex"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        "other",
                    )
                    .await
                    .unwrap();
                store
                    .update_mission_run_settings(
                        mission.id,
                        Some("grok"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        "not-native",
                    )
                    .await
                    .unwrap();
                assert!(
                    grok_session_for_turn(&store, mission.id).await.is_err(),
                    "handoff must not erase uncertainty"
                );
            }
        }
    }

    #[tokio::test]
    async fn grok_acp_unknown_session_creation_is_durable_without_claiming_prompt_execution() {
        use crate::api::mission_store::{
            FileMissionStore, InMemoryMissionStore, MissionStore, SessionUpdateRun,
            SqliteMissionStore,
        };
        use std::process::Stdio;
        for kind in ["memory", "file", "sqlite"] {
            for scenario in ["initialize_eof", "new_eof", "new_missing_id"] {
                let root = tempfile::tempdir().unwrap();
                let mut store: Arc<dyn MissionStore> = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(root.path().into(), "creation")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(root.path().into(), "creation")
                            .await
                            .unwrap(),
                    ),
                    _ => Arc::new(InMemoryMissionStore::new()),
                };
                let mission = store
                    .create_mission(
                        Some("unknown creation"),
                        None,
                        None,
                        None,
                        None,
                        Some("grok"),
                        None,
                    )
                    .await
                    .unwrap();
                let run = store
                    .begin_mission_run(mission.id, "first", None)
                    .await
                    .unwrap();
                let child = tokio::process::Command::new("python3")
                    .args(["-u", "-c", include_str!("fixtures/grok_acp.py"), scenario])
                    .current_dir(root.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let (events, _) = broadcast::channel(16);
                let error = crate::api::runners::SESSION_UPDATE_RUN
                    .scope(
                        Some(SessionUpdateRun::from(&run)),
                        run_grok_acp_process(
                            Some(&store),
                            child,
                            root.path().to_str().unwrap(),
                            "must not reach prompt",
                            None,
                            mission.id,
                            events,
                            CancellationToken::new(),
                            None,
                            false,
                            GrokAcpIdlePolicy::default(),
                        ),
                    )
                    .await
                    .unwrap_err();
                if scenario == "initialize_eof" {
                    assert!(!error.continuity_required, "{kind}/{scenario}");
                    assert!(!root.path().join("session-methods").exists());
                    assert!(!root.path().join("accepted-prompts").exists());
                    assert!(!store
                        .native_prompt_attempted(mission.id, "grok")
                        .await
                        .unwrap());
                    assert_eq!(
                        grok_session_for_turn(&store, mission.id).await.unwrap(),
                        None
                    );
                    assert!(crate::api::runners::SESSION_UPDATE_RUN
                        .scope(
                            Some(SessionUpdateRun::from(&run)),
                            claim_grok_prompt(&store, mission.id, None)
                        )
                        .await
                        .is_ok());
                    continue;
                }
                assert!(error.continuity_required, "{kind}/{scenario}");
                assert!(!root.path().join("accepted-prompts").exists());
                assert_eq!(
                    std::fs::read_to_string(root.path().join("session-methods")).unwrap(),
                    "session/new\n"
                );
                store = match kind {
                    "file" => Arc::new(
                        FileMissionStore::new(root.path().into(), "creation")
                            .await
                            .unwrap(),
                    ),
                    "sqlite" => Arc::new(
                        SqliteMissionStore::new(root.path().into(), "creation")
                            .await
                            .unwrap(),
                    ),
                    _ => store,
                };
                assert!(store
                    .native_prompt_attempted(mission.id, "grok")
                    .await
                    .unwrap());
                assert!(store
                    .get_mission(mission.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .session_id
                    .is_none());
                assert!(grok_session_for_turn(&store, mission.id).await.is_err());
                assert!(store
                    .finish_mission_run(run.run_id, run.generation, Some("unknown_creation"))
                    .await
                    .unwrap());
                let retry = store
                    .begin_mission_run(mission.id, "retry", None)
                    .await
                    .unwrap();
                assert!(crate::api::runners::SESSION_UPDATE_RUN
                    .scope(
                        Some(SessionUpdateRun::from(&retry)),
                        claim_grok_prompt(&store, mission.id, None)
                    )
                    .await
                    .is_err());
                assert!(!root.path().join("accepted-prompts").exists());
                assert_eq!(
                    std::fs::read_to_string(root.path().join("session-methods")).unwrap(),
                    "session/new\n"
                );
            }
        }
    }

    #[tokio::test]
    async fn grok_session_is_persisted_without_actor_event_delivery() {
        use crate::api::mission_store::{FileMissionStore, MissionStore, SqliteMissionStore};
        use std::process::Stdio;
        use std::sync::Arc;
        for kind in ["file", "sqlite"] {
            let root = tempfile::tempdir().unwrap();
            let store: Arc<dyn MissionStore> = if kind == "file" {
                Arc::new(
                    FileMissionStore::new(root.path().to_path_buf(), "ack")
                        .await
                        .unwrap(),
                )
            } else {
                Arc::new(
                    SqliteMissionStore::new(root.path().to_path_buf(), "ack")
                        .await
                        .unwrap(),
                )
            };
            let mission = store
                .create_mission(
                    Some("session ack"),
                    None,
                    None,
                    None,
                    None,
                    Some("grok"),
                    None,
                )
                .await
                .unwrap();
            let (events, _unread) = broadcast::channel(64);
            for _ in 0..2 {
                let saved = store
                    .get_mission(mission.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .session_id;
                let child = tokio::process::Command::new("python3")
                    .args([
                        "-u",
                        "-c",
                        include_str!("fixtures/grok_acp.py"),
                        "silent_success",
                    ])
                    .current_dir(root.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let result = run_grok_acp_process(
                    Some(&store),
                    child,
                    root.path().to_str().unwrap(),
                    "fixture prompt",
                    Some("fixture-model"),
                    mission.id,
                    events.clone(),
                    CancellationToken::new(),
                    saved.as_deref(),
                    saved.is_some(),
                    GrokAcpIdlePolicy::default(),
                )
                .await
                .unwrap_or_else(|error| panic!("unexpected ACP fallback: {}", error.reason));
                assert!(result.success);
                assert_eq!(
                    store
                        .get_mission(mission.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .session_id
                        .as_deref(),
                    Some("fixture-session")
                );
            }
            assert_eq!(
                grok_session_for_turn(&store, mission.id)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("fixture-session")
            );
            assert_eq!(
                fs::read_to_string(root.path().join("session-methods")).unwrap(),
                "session/new\nsession/load\n"
            );
            assert_eq!(
                fs::read_to_string(root.path().join("accepted-prompts")).unwrap(),
                "accepted\naccepted\n"
            );
            // A failed store write must stop before accepting any ACP prompt,
            // and must not authorize fallback into another native session.
            fs::remove_file(root.path().join("accepted-prompts")).unwrap();
            let child = tokio::process::Command::new("python3")
                .args([
                    "-u",
                    "-c",
                    include_str!("fixtures/grok_acp.py"),
                    "silent_success",
                ])
                .current_dir(root.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let error = run_grok_acp_process(
                Some(&store),
                child,
                root.path().to_str().unwrap(),
                "must not run",
                Some("fixture-model"),
                Uuid::new_v4(),
                events.clone(),
                CancellationToken::new(),
                None,
                false,
                GrokAcpIdlePolicy::default(),
            )
            .await
            .unwrap_err();
            assert!(error.continuity_required);
            assert!(!root.path().join("accepted-prompts").exists());

            let child = tokio::process::Command::new("python3")
                .args(["-c", "print('{\"session_id\":\"stream-native\",\"type\":\"text\",\"text\":\"done\"}')"])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            let result = run_grok_streaming_process(
                Some(&store),
                child,
                None,
                mission.id,
                events.clone(),
                CancellationToken::new(),
                false,
            )
            .await;
            assert!(result.success);
            assert_eq!(
                store
                    .get_mission(mission.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .session_id
                    .as_deref(),
                Some("stream-native")
            );

            // A late same-backend response cannot replace the successor's ID
            // or authorize another ACP prompt.
            let old = store
                .begin_mission_run(mission.id, "old", None)
                .await
                .unwrap();
            store
                .finish_mission_run(old.run_id, old.generation, Some("turn_complete"))
                .await
                .unwrap();
            let newer = store
                .begin_mission_run(mission.id, "new", None)
                .await
                .unwrap();
            let newer_stamp = crate::api::mission_store::SessionUpdateRun::from(&newer);
            assert!(store
                .update_mission_session_id(
                    mission.id,
                    "successor-native",
                    "grok",
                    Some(&newer_stamp)
                )
                .await
                .unwrap());
            let child = tokio::process::Command::new("python3")
                .args([
                    "-u",
                    "-c",
                    include_str!("fixtures/grok_acp.py"),
                    "silent_success",
                ])
                .current_dir(root.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let error = crate::api::runners::SESSION_UPDATE_RUN
                .scope(
                    Some(crate::api::mission_store::SessionUpdateRun::from(&old)),
                    run_grok_acp_process(
                        Some(&store),
                        child,
                        root.path().to_str().unwrap(),
                        "stale turn must not run",
                        Some("fixture-model"),
                        mission.id,
                        events.clone(),
                        CancellationToken::new(),
                        None,
                        false,
                        GrokAcpIdlePolicy::default(),
                    ),
                )
                .await
                .unwrap_err();
            assert!(error.continuity_required);
            assert!(!root.path().join("accepted-prompts").exists());
            assert_eq!(
                store
                    .get_mission(mission.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .session_id
                    .as_deref(),
                Some("successor-native")
            );
        }
    }

    #[tokio::test]
    async fn grok_streaming_missing_resume_preserves_continuity() {
        use std::process::Stdio;
        for (is_resume, error, expected) in [
            (
                true,
                "No session found for saved-id",
                TerminalReason::NativeContinuityRequired,
            ),
            (
                true,
                "Session not found: saved-id",
                TerminalReason::NativeContinuityRequired,
            ),
            (
                true,
                "Session does not exist",
                TerminalReason::NativeContinuityRequired,
            ),
            (false, "Session does not exist", TerminalReason::LlmError),
            (false, "No session found", TerminalReason::LlmError),
            (true, "401 invalid credentials", TerminalReason::LlmError),
        ] {
            let child = tokio::process::Command::new("sh")
                .args(["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "fixture", error])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let (events, _) = broadcast::channel(16);
            let result = run_grok_streaming_process(
                None,
                child,
                None,
                Uuid::new_v4(),
                events,
                CancellationToken::new(),
                is_resume,
            )
            .await;
            assert_eq!(result.terminal_reason, Some(expected));
        }
    }

    #[test]
    fn grok_acp_idle_deadlines_distinguish_pending_running_and_finished_tools() {
        use std::time::Duration;
        let policy = GrokAcpIdlePolicy::default();
        let now = tokio::time::Instant::now();
        let mut calls = HashMap::new();
        assert_eq!(policy.diagnostic, Duration::from_secs(180));
        assert_eq!(
            grok_acp_idle_deadline(now, &calls, policy),
            now + Duration::from_secs(600)
        );

        let call = calls
            .entry("build".to_string())
            .or_insert_with(GrokAcpToolCall::default);
        call.observe(
            &serde_json::json!({"status": "pending", "rawInput": {"timeout": "3600000"}}),
            now,
            policy,
        );
        assert_eq!(
            grok_acp_idle_deadline(now, &calls, policy),
            now + Duration::from_secs(600)
        );
        calls.get_mut("build").unwrap().observe(
            &serde_json::json!({"status": "in_progress"}),
            now,
            policy,
        );
        let tool_deadline = now + Duration::from_secs(3630);
        assert_eq!(grok_acp_idle_deadline(now, &calls, policy), tool_deadline);
        // Repeated status updates cannot restart the tool's own timeout.
        calls.get_mut("build").unwrap().observe(
            &serde_json::json!({"status": "in_progress"}),
            now + Duration::from_secs(200),
            policy,
        );
        assert_eq!(grok_acp_idle_deadline(now, &calls, policy), tool_deadline);
        calls.get_mut("build").unwrap().observe(
            &serde_json::json!({"status": "completed"}),
            now,
            policy,
        );
        assert_eq!(
            grok_acp_idle_deadline(now, &calls, policy),
            now + Duration::from_secs(600)
        );
    }

    #[test]
    fn grok_acp_unknown_or_unbounded_tools_do_not_disable_idle_guard() {
        let policy = GrokAcpIdlePolicy::default();
        let now = tokio::time::Instant::now();
        for input in [
            serde_json::json!({}),
            serde_json::json!({"timeout": 0}),
            serde_json::json!({"timeout": -1}),
            serde_json::json!({"timeout": "forever"}),
        ] {
            let mut calls = HashMap::new();
            let mut call = GrokAcpToolCall::default();
            call.observe(
                &serde_json::json!({"status": "in_progress", "rawInput": input}),
                now,
                policy,
            );
            calls.insert("tool".to_string(), call);
            assert_eq!(
                grok_acp_idle_deadline(now, &calls, policy),
                now + policy.transport
            );
        }
    }

    async fn grok_acp_fixture(
        scenario: &str,
        cancel_after_tool: bool,
    ) -> (AgentResult, Vec<AgentEvent>, std::time::Duration) {
        use std::process::Stdio;
        use std::time::Duration;
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("proof-artifact"), "preserved checkout").unwrap();
        let child = tokio::process::Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(include_str!("fixtures/grok_acp.py"))
            .arg(scenario)
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (events_tx, mut events_rx) = broadcast::channel(64);
        let cancel = CancellationToken::new();
        if cancel_after_tool {
            let mut cancel_rx = events_tx.subscribe();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                while let Ok(event) = cancel_rx.recv().await {
                    if matches!(event, AgentEvent::ToolCall { .. }) {
                        cancel.cancel();
                        break;
                    }
                    if matches!(event, AgentEvent::TextDelta { ref content, .. } if content == "closing stdout")
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        cancel.cancel();
                        break;
                    }
                }
            });
        }
        let start = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_grok_acp_process(
                None,
                child,
                root.path().to_str().unwrap(),
                "fixture prompt",
                Some("fixture-model"),
                Uuid::new_v4(),
                events_tx,
                cancel,
                None,
                false,
                GrokAcpIdlePolicy {
                    diagnostic: Duration::from_millis(180),
                    transport: Duration::from_millis(600),
                    tool_grace: Duration::from_millis(30),
                    shutdown: Duration::from_millis(120),
                },
            ),
        )
        .await
        .expect("ACP lifecycle must remain bounded")
        .unwrap_or_else(|e| panic!("unexpected pre-prompt fallback: {}", e.reason));
        let elapsed = start.elapsed();
        // Post-prompt recovery belongs to the control actor; the protocol
        // runner must never replay a prompt itself, even after repeated EOF.
        assert_eq!(
            fs::read_to_string(root.path().join("accepted-prompts")).unwrap(),
            "accepted\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("proof-artifact")).unwrap(),
            "preserved checkout"
        );
        let mut events = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            events.push(event);
        }
        (result, events, elapsed)
    }

    #[tokio::test]
    async fn grok_acp_missing_exact_session_requires_reconciliation_without_prompt() {
        use std::process::Stdio;
        let root = tempfile::tempdir().unwrap();
        let child = tokio::process::Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(include_str!("fixtures/grok_acp.py"))
            .arg("missing_session")
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (events_tx, mut events_rx) = broadcast::channel(64);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_grok_acp_process(
                None,
                child,
                root.path().to_str().unwrap(),
                "must not replay",
                Some("fixture-model"),
                Uuid::new_v4(),
                events_tx,
                CancellationToken::new(),
                Some("original-grok-session"),
                true,
                GrokAcpIdlePolicy::default(),
            ),
        )
        .await
        .unwrap();
        let failure = match result {
            Err(failure) => failure,
            Ok(_) => panic!("missing native history must not run"),
        };
        assert!(failure.continuity_required);
        assert!(failure.reason.contains("No session found"));
        assert!(!root.path().join("accepted-prompts").exists());
        while let Ok(event) = events_rx.try_recv() {
            assert!(!matches!(
                event,
                AgentEvent::ToolCall { .. } | AgentEvent::SessionIdUpdate { .. }
            ));
        }
    }

    #[tokio::test]
    async fn grok_acp_silent_inference_survives_diagnostic_deadline() {
        let (result, events, _) = grok_acp_fixture("silent_success", false).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(result.output, "fixture completed");
        assert!(events.iter().any(|event| matches!(event, AgentEvent::AgentPhase { detail: Some(detail), .. } if detail.contains("awaiting_inference"))));
    }

    #[tokio::test]
    async fn grok_acp_dead_transport_times_out_with_evidence_and_no_replay() {
        for scenario in ["dead", "junk", "missing_status", "completed_tool_dead"] {
            let (result, events, elapsed) = grok_acp_fixture(scenario, false).await;
            if scenario == "missing_status" {
                assert_eq!(
                    result.terminal_reason,
                    Some(TerminalReason::NativeContinuityRequired)
                );
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::ToolResult { .. })));
            }
            assert!(!result.success, "scenario: {scenario}");
            assert!(elapsed >= std::time::Duration::from_millis(600));
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "{scenario}: {elapsed:?}"
            );
            assert_eq!(
                result.data.as_ref().unwrap()["transport_failure_stage"],
                "grok_acp_transport_idle"
            );
            assert_eq!(
                result.data.as_ref().unwrap()["failure_class"],
                "transport_error"
            );
            assert!(result
                .terminal_evidence
                .unwrap()
                .contains("fixture stderr diagnostic"));
        }
    }

    #[tokio::test]
    async fn grok_acp_long_tool_survives_transport_idle_under_its_own_deadline() {
        let (result, events, elapsed) = grok_acp_fixture("long_tool", false).await;
        assert!(result.success, "{}", result.output);
        assert!(elapsed > std::time::Duration::from_millis(600));
        assert!(events.iter().any(|event| matches!(event, AgentEvent::AgentPhase { detail: Some(detail), .. } if detail.contains("awaiting_tool_results"))));
        assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolResult { result, .. } if result["status"] == "completed")));
    }

    #[tokio::test]
    async fn grok_acp_stale_running_tool_times_out_after_its_deadline() {
        let (result, events, elapsed) = grok_acp_fixture("expired_tool", false).await;
        assert_eq!(
            result.terminal_reason,
            Some(TerminalReason::NativeContinuityRequired)
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolResult { .. })));
        assert!(!result.success);
        assert_eq!(
            result.data.unwrap()["transport_failure_stage"],
            "grok_acp_transport_idle"
        );
        assert!(elapsed >= std::time::Duration::from_millis(1200));
        assert!(elapsed < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn grok_acp_cancellation_interrupts_a_long_tool_without_recovery() {
        let (result, _, elapsed) = grok_acp_fixture("cancel", true).await;
        assert_eq!(result.terminal_reason, Some(TerminalReason::Cancelled));
        assert!(elapsed < std::time::Duration::from_millis(600));
        assert!(result.data.is_none());
    }

    #[tokio::test]
    async fn grok_acp_input_wait_is_distinct_from_inference_idle() {
        let (result, events, _) = grok_acp_fixture("input", false).await;
        assert!(result.output.contains("awaiting_client_input"));
        assert_eq!(result.data.unwrap()["awaiting_input"], true);
        assert!(events.iter().any(|event| matches!(event, AgentEvent::AgentPhase { detail: Some(detail), .. } if detail.contains("awaiting_client_input"))));
    }

    #[tokio::test]
    async fn grok_acp_closed_stdout_does_not_hang_waiting_for_process_exit() {
        let (result, _, elapsed) = grok_acp_fixture("eof", false).await;
        assert!(!result.success);
        assert_eq!(
            result.data.unwrap()["transport_failure_stage"],
            "stream_closed"
        );
        assert!(elapsed < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn grok_acp_cancellation_during_eof_teardown_is_not_a_transport_retry() {
        let (result, _, elapsed) = grok_acp_fixture("eof_cancel", true).await;
        assert_eq!(result.terminal_reason, Some(TerminalReason::Cancelled));
        assert!(result.data.is_none());
        assert!(elapsed < std::time::Duration::from_millis(600));
    }

    #[tokio::test]
    async fn grok_acp_prompt_error_is_not_retried_as_a_transport_failure() {
        let (result, _, _) = grok_acp_fixture("prompt_error", false).await;
        assert!(!result.success);
        assert_eq!(result.data.unwrap()["failure_class"], "provider_error");
    }

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
    fn grok_overlay_prefers_configured_relative_program() {
        let root = tempfile::tempdir().unwrap();
        write_executable(
            &root.path().join("usr/local/bin/grok"),
            b"#!/bin/sh\necho ordinary\n",
        );
        write_executable(
            &root.path().join("usr/local/bin/custom-grok"),
            b"#!/bin/sh\necho custom\n",
        );
        let workspace = container_workspace_at(root.path());
        assert_eq!(
            grok_overlay_guest_path(&workspace, "custom-grok").as_deref(),
            Some("/usr/local/bin/custom-grok"),
            "relative cli_path must win over ordinary /usr/local/bin/grok"
        );
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
