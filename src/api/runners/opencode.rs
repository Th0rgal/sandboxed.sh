//! OpenCode CLI turn runner.
//!
//! Moved verbatim from `mission_runner.rs` (Phase 2 of the decomposition).
//! The OpenCode SSE/JSON parsing helpers it shares with the goal/control
//! arm stay in `mission_runner` behind `pub(crate)`.

use std::borrow::Cow;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, CompletionConfidence, CompletionSignal, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::*;
use crate::cost::resolve_cost_cents_and_source;
use crate::opencode::{extract_reasoning, extract_text};
use crate::workspace::Workspace;
use crate::workspace_exec::WorkspaceExec;

/// Hard inactivity threshold when neither heartbeats nor proxy streaming are
/// observed. Thinking models routinely spend 2–5 minutes inside a reasoning
/// segment with zero harness output, so 120s produced false stall kills
/// (mission f9ba703a: MiniMax-M3 killed mid-reasoning after exactly 120s).
const GLOBAL_INACTIVITY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Inactivity threshold while OpenCode server heartbeats are still arriving.
const HEARTBEAT_INACTIVITY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(420);
/// A builtin-proxy chunk within this window counts as "the LLM call is alive".
const PROXY_STREAM_RECENT: std::time::Duration = std::time::Duration::from_secs(30);
/// Absolute cap on the proxy-stream grace: even with live upstream chunks,
/// kill after this much harness silence (protects against a wedged CLI that
/// stopped consuming its own LLM stream).
const PROXY_STREAM_INACTIVITY_CAP: std::time::Duration = std::time::Duration::from_secs(1800);

/// Execute a turn using OpenCode CLI backend.
///
/// For Host workspaces: spawns the CLI directly on the host.
/// For Container workspaces: spawns the CLI inside the container using systemd-nspawn.
///
/// This uses `opencode run` directly for per-workspace isolation.
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
    session_id: Option<&str>,
    is_continuation: bool,
) -> AgentResult {
    use crate::api::ai_providers::{
        ensure_anthropic_oauth_token_valid, ensure_google_oauth_token_valid,
        ensure_openai_oauth_token_valid,
    };
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};

    // When no agent is requested, default to vanilla opencode's primary "build" agent.
    let default_agent = if agent.is_none() { Some("build") } else { None };
    let agent = agent.or(default_agent);
    let opencode_goal_objective = parse_opencode_goal_objective(message);
    if let Some(objective) = opencode_goal_objective.as_deref() {
        let _ = events_tx.send(AgentEvent::GoalStatus {
            status: "active".to_string(),
            objective: objective.to_string(),
            mission_id: Some(mission_id),
        });
        let _ = events_tx.send(AgentEvent::GoalIteration {
            iteration: 1,
            objective: objective.to_string(),
            mission_id: Some(mission_id),
        });
    }

    // Use the OpenCode CLI directly for per-workspace execution.
    let workspace_exec = WorkspaceExec::new(workspace.clone());
    if let Err(err) = ensure_opencode_cli_available(&workspace_exec, work_dir).await {
        tracing::error!("{}", err);
        let _ = events_tx.send(AgentEvent::Error {
            message: err.clone(),
            mission_id: Some(mission_id),
            resumable: true,
        });
        return AgentResult::failure(err, 0).with_terminal_reason(TerminalReason::LlmError);
    }

    let opencode_config_dir_host = work_dir.join(".opencode");

    // Resolve the model: explicit override > agent config > env var defaults.
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
            p if crate::api::providers::DEFAULT_CATALOG_PROVIDER_IDS.contains(&p) => {
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

    let cli_runner = if let Some(path) = configured_runner {
        if command_available(&workspace_exec, work_dir, &path).await {
            path
        } else {
            let err_msg = format!(
                "OpenCode CLI runner '{}' not found in workspace. Install it or update OPENCODE_CLI_PATH.",
                path
            );
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    } else if command_available(&workspace_exec, work_dir, "opencode").await {
        "opencode".to_string()
    } else {
        let err_msg =
            "OpenCode CLI not found in workspace. Install opencode or update OPENCODE_CLI_PATH."
                .to_string();
        tracing::error!("{}", err_msg);
        return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
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
    let mut model_used: Option<String> = None;
    // Accumulate token usage from SSE response.completed events for cost estimation
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_cache_creation_input_tokens: u64 = 0;
    let mut total_cache_read_input_tokens: u64 = 0;
    let agent_model = resolve_opencode_model_from_config(&opencode_config_dir_host, agent);
    if resolved_model.is_none() {
        resolved_model = agent_model.clone();
    }
    // Inject provider definitions into opencode.json for models not in
    // OpenCode's built-in snapshot.
    let workspace_host_ip = workspace.host_ip_from_workspace();
    if let Some(model_override) = resolved_model.as_deref() {
        ensure_opencode_provider_for_model(
            &opencode_config_dir_host,
            app_working_dir,
            model_override,
            &workspace_host_ip,
            Some(&mission_id.to_string()),
        );
    }
    if let Some(ref am) = agent_model {
        if resolved_model.as_deref() != Some(am) {
            ensure_opencode_provider_for_model(
                &opencode_config_dir_host,
                app_working_dir,
                am,
                &workspace_host_ip,
                Some(&mission_id.to_string()),
            );
        }
    }
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
    if has_anthropic {
        // OpenCode 1.17 does not load its Anthropic OAuth transport merely
        // because auth.json contains an OAuth credential. Without the plugin,
        // the provider is absent from the model catalog (or the plain AI SDK
        // adapter asks for an API key instead of using the OAuth token).
        let anthropic_plugin = "opencode-anthropic-auth@latest";
        ensure_opencode_plugin_specs(&opencode_config_dir_host, &[anthropic_plugin]);
        ensure_opencode_plugin_installed(
            &workspace_exec,
            work_dir,
            &opencode_config_dir_host,
            &opencode_config_dir_env,
            anthropic_plugin,
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

    // Build the opencode run command as a shell string so that $(cat <file>)
    // correctly expands the message as a single argument.
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

    let opencode_model = resolved_model.as_deref().unwrap_or("builtin/fast");
    if opencode_model.starts_with("builtin/") {
        ensure_opencode_provider_for_model(
            &opencode_config_dir_host,
            app_working_dir,
            opencode_model,
            &workspace_host_ip,
            Some(&mission_id.to_string()),
        );
    }

    let mut inner_cmd = String::new();
    inner_cmd.push_str("#!/bin/sh\n");
    inner_cmd.push_str(&shell_escape(&cli_runner));
    inner_cmd.push_str(" run --format json --model ");
    inner_cmd.push_str(&shell_escape(opencode_model));
    if let Some(a) = agent {
        inner_cmd.push_str(" --agent ");
        inner_cmd.push_str(&shell_escape(a));
    }
    // Resume the per-mission OpenCode session on continuation turns so the
    // CLI loads prior message history from `<XDG_DATA_HOME>/opencode/storage`
    // (which is now scoped to the workspace — see the XDG overrides above).
    // Without this, every turn starts a brand-new session and the model
    // loses all prior context. `--continue` is the simpler "resume last
    // session in this dir" form for freshly-created missions that don't
    // have a stored session id yet.
    //
    // Mission rows in the DB also carry a *Claude Code*-style session id
    // (a plain UUID generated at mission creation) that is NOT a valid
    // OpenCode session id. Passing `--session <UUID>` to the opencode CLI
    // makes it error out with "Session not found". Only treat a session
    // id as an OpenCode id when it starts with the "ses_" prefix the CLI
    // uses (`ses_<base62>`); fall back to `--continue` otherwise.
    if is_continuation {
        // A stored `ses_*` id is only usable if the session actually lives in
        // the store the CLI will read. Missions created before the per-mission
        // XDG isolation persisted their sessions in the shared host store —
        // passing `--session` for those makes the CLI fail with "Session not
        // found". Fall back to `--continue` (resume last session in this dir)
        // when the session is not present in the effective store.
        let shared_xdg = std::env::var("SANDBOXED_SH_OPENCODE_SHARED_XDG")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
        let mission_data_home = work_dir.join(".local").join("share");
        let opencode_sid = session_id
            .filter(|s| is_opencode_session_id(s))
            .filter(|s| shared_xdg || opencode_session_exists_in_data_home(&mission_data_home, s));
        match opencode_sid {
            Some(sid) => {
                inner_cmd.push_str(" --session ");
                inner_cmd.push_str(&shell_escape(sid));
            }
            None => {
                if session_id.is_some() {
                    tracing::info!(
                        mission_id = %mission_id,
                        session_id = ?session_id,
                        "Stored OpenCode session not found in per-mission storage \
                         (likely created before XDG isolation); using --continue"
                    );
                }
                inner_cmd.push_str(" --continue");
            }
        }
    }
    inner_cmd.push_str(" --dir ");
    inner_cmd.push_str(&shell_escape(&work_dir_arg));
    inner_cmd.push_str(" \"$(cat ");
    inner_cmd.push_str(&shell_escape(&prompt_file_arg));
    inner_cmd.push_str(")\"");

    let script_host_path = format!("{}/.sandboxed-sh-opencode-cmd.sh", work_dir.display());
    let script_env_path = format!(
        "{}/.sandboxed-sh-opencode-cmd.sh",
        prompt_file_arg
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(".")
    );
    if let Err(e) = std::fs::write(&script_host_path, &inner_cmd) {
        let err_msg = format!("Failed to write OpenCode command script: {}", e);
        tracing::error!(mission_id = %mission_id, "{}", err_msg);
        return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_host_path, std::fs::Permissions::from_mode(0o755));
    }

    let mut shell_cmd = String::from("script -qe /dev/null -c ");
    shell_cmd.push_str(&shell_escape(&script_env_path));
    shell_cmd.push_str(" 2>/dev/null");

    let args = vec!["-c".to_string(), shell_cmd.clone()];
    let cli_runner_shell = "/bin/sh".to_string();

    tracing::debug!(
        mission_id = %mission_id,
        shell_cmd = %shell_cmd,
        prompt_file = %prompt_file_arg,
        "OpenCode CLI args prepared (shell wrapper)"
    );

    let telegram_action_helpers_enabled =
        message.contains("[Telegram from ") || message.contains("[Telegram workflow reply ");
    if telegram_action_helpers_enabled {
        write_telegram_action_cli_helpers(work_dir);
    }

    // Build environment variables
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("MISSION_ID".to_string(), mission_id.to_string());
    // DGX Spark build offload (opt-in per workspace) — see Workspace::spark_offload_env.
    if let Some(spark_vars) = workspace.spark_offload_env(mission_id) {
        env.extend(spark_vars);
    }
    // Remote lean-build dispatch — see Workspace::remote_build_env. Exported
    // so the in-workspace `remote-lean-build` wrapper can reach the host
    // /api/remote-build endpoint.
    if let Some(remote_build_vars) = workspace.remote_build_env(mission_id) {
        env.extend(remote_build_vars);
    }
    if let Some(public_url) = public_api_base_url_from_env() {
        env.insert("API_URL".to_string(), public_url);
    } else if let Some(local_url) = workspace_api_base_url(workspace) {
        env.insert("API_URL".to_string(), local_url);
    }

    // Per-mission XDG isolation for OpenCode. Without this, every mission on
    // the same host shares the operator's `~/.local/share/opencode` storage,
    // which (a) lets sessions from concurrent missions collide on the same
    // SQLite DB and (b) means resuming mission A pulls in any unrelated
    // session that the operator's opencode created locally. Mirror the
    // per-mission HOME/XDG pattern used by Claude Code (see the `claudecode`
    // arm above) so storage and config are scoped to the workspace.
    let opencode_xdg_config = work_dir.join(".config");
    let opencode_xdg_data = work_dir.join(".local").join("share");
    let opencode_xdg_state = work_dir.join(".local").join("state");
    let opencode_xdg_cache = work_dir.join(".cache");
    for dir in [
        &opencode_xdg_config,
        &opencode_xdg_data,
        &opencode_xdg_state,
        &opencode_xdg_cache,
    ] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                mission_id = %mission_id,
                path = %dir.display(),
                error = %e,
                "Failed to create per-mission OpenCode XDG directory"
            );
        }
    }
    let opencode_xdg_config_env = workspace_path_for_env(workspace, &opencode_xdg_config);
    let opencode_xdg_data_env = workspace_path_for_env(workspace, &opencode_xdg_data);
    let opencode_xdg_state_env = workspace_path_for_env(workspace, &opencode_xdg_state);
    let opencode_xdg_cache_env = workspace_path_for_env(workspace, &opencode_xdg_cache);
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        opencode_xdg_config_env.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_DATA_HOME".to_string(),
        opencode_xdg_data_env.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_STATE_HOME".to_string(),
        opencode_xdg_state_env.to_string_lossy().to_string(),
    );
    env.insert(
        "XDG_CACHE_HOME".to_string(),
        opencode_xdg_cache_env.to_string_lossy().to_string(),
    );
    // HOME is the fallback OpenCode uses when XDG_* aren't set. Setting it to
    // the workspace also keeps credential lookups (e.g. `~/.local/share/opencode/auth.json`)
    // inside the per-mission XDG_DATA_HOME we just set above.
    env.insert(
        "HOME".to_string(),
        workspace_path_for_env(workspace, work_dir)
            .to_string_lossy()
            .to_string(),
    );
    // Allow opting out of the per-mission XDG override when an operator
    // explicitly wants the opencode CLI to share storage with the host (e.g.
    // for debugging with `opencode session list` on the host shell).
    let shared_opencode_xdg = std::env::var("SANDBOXED_SH_OPENCODE_SHARED_XDG")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_some();
    if shared_opencode_xdg {
        env.remove("XDG_CONFIG_HOME");
        env.remove("XDG_DATA_HOME");
        env.remove("XDG_STATE_HOME");
        env.remove("XDG_CACHE_HOME");
        env.remove("HOME");
    }
    if telegram_action_helpers_enabled {
        if let Some(token) = crate::api::telegram::build_internal_telegram_action_token(mission_id)
        {
            env.insert("TELEGRAM_ACTION_TOKEN".to_string(), token);
        }
        let internal_api_url = workspace_api_base_url(workspace);
        if let Some(api_url) = internal_api_url {
            env.insert(
                "TELEGRAM_ACTION_URL".to_string(),
                format!("{}/api/control/telegram/actions/internal", api_url),
            );
            env.insert(
                "TELEGRAM_WORKFLOW_URL".to_string(),
                format!(
                    "{}/api/control/telegram/workflows/request/internal",
                    api_url
                ),
            );
        }
        env.insert(
            "TELEGRAM_ACTION_CLI".to_string(),
            format!("{}/.sandboxed-sh-telegram-action.py", work_dir_arg),
        );
        env.insert(
            "TELEGRAM_ACTION_COMMAND".to_string(),
            format!("{}/telegram-action", work_dir_arg),
        );
    }

    // Ensure OpenCode's install directory is available in PATH.
    {
        let path = opencode_path(
            env.get("PATH").map(String::as_str),
            &work_dir_arg,
            telegram_action_helpers_enabled,
        );
        env.insert("PATH".to_string(), path);
    }

    let opencode_auth =
        sync_opencode_auth_to_workspace(workspace, app_working_dir, work_dir, shared_opencode_xdg);

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
    let mut tool_call_step_count: u32 = 0;
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
    // Latest cumulative thinking content streamed this session. The block-final
    // Thinking event (the only persisted one) is built from this, including in
    // the post-loop fallback where the reader task's parser state is gone.
    let sse_last_thinking: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let sse_emitted_text = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_done_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_error_message: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let latest_tool_result_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let rate_limit_detected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sse_cancel = CancellationToken::new();
    let (sse_complete_tx, mut sse_complete_rx) = tokio::sync::watch::channel(false);
    let (sse_session_idle_tx, mut sse_session_idle_rx) = tokio::sync::watch::channel(false);
    let (sse_retry_tx, mut sse_retry_rx) = tokio::sync::watch::channel(0u32);
    let last_activity = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    // Track recent OpenCode heartbeats separately from "meaningful" activity.
    // Some provider chains can spend >120s between message/status updates while
    // still emitting heartbeats, so treating heartbeat-only periods as hard
    // inactivity can kill valid runs prematurely.
    let last_heartbeat = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    let (text_output_tx, mut text_output_rx) = tokio::sync::watch::channel(false);
    // Track active tool call depth: incremented on ToolCall, decremented on ToolResult.
    // Used to skip inactivity timeouts during long tool runs (builds, tests, etc.).
    let (sse_tool_depth_tx, sse_tool_depth_rx) = tokio::sync::watch::channel(0u32);

    // OpenCode's supported integration path is `run --format json`; all events
    // are consumed from stdout, with no parallel curl/SSE side channel.
    let sse_handle: Option<tokio::task::JoinHandle<()>> = None;
    let json_tool_depth_tx = Some(sse_tool_depth_tx);

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
    let stderr_last_heartbeat = last_heartbeat.clone();
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
                    let is_heartbeat = clean.contains("server.heartbeat");
                    let is_server_noise = is_heartbeat
                        || clean.contains("server.connected")
                        || clean.contains("server.listening");
                    if is_heartbeat {
                        if let Ok(mut guard) = stderr_last_heartbeat.lock() {
                            *guard = Some(std::time::Instant::now());
                        }
                    }
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

    // Process stdout output from OpenCode.
    // Events come via SSE (when curl is available), stdout contains the assistant's text response.
    let stdout_reader = BufReader::new(stdout);
    let mut stdout_lines = stdout_reader.lines();
    let mut state = OpencodeSseState::default();

    let mut sse_complete_seen = false;
    let mut sse_complete_at: Option<std::time::Instant> = None;
    let mut text_output_at: Option<std::time::Instant> = None;
    // Set when the process is killed by an idle timeout (text-output or global).
    // Used after the event loop to flag the result as incomplete so the caller
    // can surface the truncation to the user.
    let mut killed_by_idle_timeout = false;
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
    // OpenCode can legitimately spend more than 30s in the next provider call
    // after emitting an initial acknowledgement and finishing a tool-call step.
    // A short timeout turns that acknowledgement into a false successful answer
    // for Telegram. Let the global inactivity timeout handle truly stuck turns.
    let opencode_text_idle_timeout_secs: u64 =
        std::env::var("SANDBOXED_SH_OPENCODE_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120);

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
                        let tools_active = if json_tool_depth_tx.is_some() {
                            *sse_tool_depth_rx.borrow() > 0
                        } else {
                            sse_alive && *sse_tool_depth_rx.borrow() > 0
                        };
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
                    if last_text.elapsed() >= std::time::Duration::from_secs(opencode_text_idle_timeout_secs) {
                        // Only kill if there's also no recent SSE/stderr activity
                        // AND no tools are actively running.  A long tool execution
                        // (build, test, sleep) may produce no text output for >30s;
                        // killing the process mid-tool would be wrong.
                        // If the SSE handler has exited, the depth value may be
                        // stale (stuck > 0), so treat that as "no tools active".
                        let sse_alive = sse_handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                        // In JSON stdout mode, tool depth is tracked directly via
                        // json_tool_depth_tx (no SSE handler).  Check the receiver
                        // regardless of sse_alive — the sender is kept alive in JSON
                        // mode specifically for this purpose.
                        let tools_active = if json_tool_depth_tx.is_some() {
                            *sse_tool_depth_rx.borrow() > 0
                        } else {
                            sse_alive && *sse_tool_depth_rx.borrow() > 0
                        };
                        let recent_activity = last_activity
                            .lock()
                            .ok()
                            .map(|g| g.elapsed() < std::time::Duration::from_secs(opencode_text_idle_timeout_secs))
                            .unwrap_or(false);
                        // Proxy-stream grace: thinking models can spend minutes
                        // inside a reasoning segment with no OpenCode events at
                        // all, while the builtin proxy sees upstream chunks
                        // streaming the whole time. Don't kill while the
                        // mission's own LLM call is demonstrably alive.
                        let proxy_streaming =
                            crate::api::proxy_liveness::time_since_activity(mission_id)
                                .map(|d| d <= PROXY_STREAM_RECENT)
                                .unwrap_or(false);
                        if !recent_activity && !tools_active && !proxy_streaming {
                            tracing::info!(
                                mission_id = %mission_id,
                                "OpenCode output idle timeout reached; terminating CLI process"
                            );
                            killed_by_idle_timeout = true;
                            let _ = child.kill().await;
                            break;
                        }
                    }
                }
                // Global inactivity timeout: if nothing at all has happened
                // (no SSE events, no stdout, no stderr), the process is
                // likely stuck.  Kill it and let the fallback recovery
                // logic read the result from OpenCode storage.
                // Skip this check while tools are actively running — long
                // commands (builds, tests) may produce no SSE events for
                // extended periods and heartbeats are intentionally filtered.
                // If the SSE handler has exited, the depth value may be stale,
                // so treat that as "no tools active".
                let sse_alive = sse_handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
                let tools_active = if json_tool_depth_tx.is_some() {
                    *sse_tool_depth_rx.borrow() > 0
                } else {
                    sse_alive && *sse_tool_depth_rx.borrow() > 0
                };
                let inactivity_elapsed = last_activity
                    .lock()
                    .ok()
                    .map(|g| g.elapsed())
                    .unwrap_or_default();
                let recent_heartbeat = last_heartbeat
                    .lock()
                    .ok()
                    .and_then(|g| *g)
                    .map(|ts| ts.elapsed() <= std::time::Duration::from_secs(45))
                    .unwrap_or(false);
                if !tools_active && inactivity_elapsed >= GLOBAL_INACTIVITY_TIMEOUT {
                    // Proxy-stream grace: the mission's own LLM call is still
                    // streaming chunks through the builtin proxy (long
                    // reasoning segments emit no OpenCode events). Defer the
                    // kill up to a hard cap in case OpenCode itself wedged
                    // while the proxy keeps receiving.
                    let proxy_streaming =
                        crate::api::proxy_liveness::time_since_activity(mission_id)
                            .map(|d| d <= PROXY_STREAM_RECENT)
                            .unwrap_or(false);
                    if proxy_streaming && inactivity_elapsed < PROXY_STREAM_INACTIVITY_CAP {
                        tracing::debug!(
                            mission_id = %mission_id,
                            inactivity_secs = inactivity_elapsed.as_secs(),
                            "No harness output but builtin proxy stream is active; deferring inactivity kill"
                        );
                    } else if recent_heartbeat
                        && inactivity_elapsed < HEARTBEAT_INACTIVITY_TIMEOUT
                    {
                        // Heartbeat-only grace: avoid killing while the OpenCode server is
                        // still alive and sending heartbeats. This especially affects smart
                        // routing chains (e.g. GLM/Minimax fallbacks) that can take longer
                        // to produce non-heartbeat events.
                    } else {
                        tracing::warn!(
                            mission_id = %mission_id,
                            inactivity_secs = inactivity_elapsed.as_secs(),
                            recent_heartbeat = recent_heartbeat,
                            proxy_streaming = proxy_streaming,
                            "Global inactivity timeout; terminating stuck CLI process"
                        );
                        killed_by_idle_timeout = true;
                        let _ = child.kill().await;
                        break;
                    }
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

                            // Reset tool depth at step boundaries for plain opencode
                            // JSON mode.  Per-tool increments/decrements happen below
                            // on the ToolCall/ToolResult events emitted by the shared
                            // parser (which also covers message.part.updated tool
                            // parts), so the resets here are a safety net against a
                            // missed ToolResult pinning depth above zero forever.
                            if let Some(ref tx) = json_tool_depth_tx {
                                if event_type == "step_finish" || event_type == "step_start" {
                                    tx.send_modify(|v| *v = 0);
                                }
                            }

                            // Handle plain opencode --format json events.
                            // Plain opencode emits: step_start, text, step_finish
                            // (different from message.part.updated/completion)
                            if event_type == "text" {
                                if let Some(part) = json.get("part") {
                                    if let Some(text) =
                                        part.get("text").and_then(|t| t.as_str())
                                    {
                                        // Extract <think>...</think> content as
                                        // thinking events before stripping them.
                                        if let Some(thinking) = extract_think_content(text) {
                                            if !thinking.trim().is_empty()
                                                && state.last_emitted_thinking.as_deref()
                                                    != Some(thinking.as_str())
                                            {
                                                state.last_emitted_thinking =
                                                    Some(thinking.clone());
                                                *sse_last_thinking
                                                    .lock()
                                                    .unwrap_or_else(|e| e.into_inner()) =
                                                    thinking.clone();
                                                sse_emitted_thinking.store(
                                                    true,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                                sse_done_sent.store(
                                                    false,
                                                    std::sync::atomic::Ordering::SeqCst,
                                                );
                                                let _ =
                                                    events_tx.send(AgentEvent::Thinking {
                                                        content: thinking,
                                                        done: false,
                                                        mission_id: Some(mission_id),
                                                    });
                                            }
                                        }

                                        // Strip <think>...</think> tags for
                                        // visible text / final result
                                        let clean_text = strip_think_tags(text);
                                        let clean_text = clean_text.trim();
                                        if !clean_text.is_empty() {
                                            final_result = clean_text.to_string();
                                            let _ = text_output_tx.send(true);
                                            sse_emitted_text.store(
                                                true,
                                                std::sync::atomic::Ordering::SeqCst,
                                            );
                                            let _ =
                                                events_tx.send(AgentEvent::TextDelta {
                                                    content: clean_text.to_string(),
                                                    mission_id: Some(mission_id),
                                                });
                                        }
                                    }
                                }
                            } else if event_type == "step_finish" {
                                let reason = json
                                    .get("part")
                                    .and_then(|p| p.get("reason"))
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("");
                                tracing::info!(
                                    mission_id = %mission_id,
                                    reason = %reason,
                                    tool_call_steps = tool_call_step_count,
                                    "OpenCode JSON step_finish event"
                                );
                                // Match the shared SSE parser: an empty reason
                                // also marks the final step (some providers omit
                                // it); otherwise completion waits on idle
                                // timeouts or process exit.
                                if reason == "stop" || reason.is_empty() {
                                    let _ = sse_complete_tx.send(true);
                                } else {
                                    // Track consecutive tool-call steps to detect runaway loops
                                    tool_call_step_count += 1;
                                    const MAX_TOOL_CALL_STEPS: u32 = 40;
                                    if tool_call_step_count >= MAX_TOOL_CALL_STEPS {
                                        tracing::warn!(
                                            mission_id = %mission_id,
                                            steps = tool_call_step_count,
                                            "OpenCode tool-call step limit reached, forcing completion"
                                        );
                                        let _ = sse_complete_tx.send(true);
                                    }
                                }
                            } else if event_type == "step_start" {
                                // Extract session ID from step_start
                                if let Some(sid) =
                                    json.get("sessionID").and_then(|s| s.as_str())
                                {
                                    let mut guard = session_id_capture
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    if guard.is_none() {
                                        *guard = Some(sid.to_string());
                                    }
                                }
                            }

                            // Handle completion and error events from OpenCode.
                            if event_type == "completion" {
                                tracing::info!(mission_id = %mission_id, "OpenCode JSON completion event");
                                let _ = sse_complete_tx.send(true);
                            } else if event_type == "error" {
                                had_error = true;
                                let mut extracted_err: Option<String> = None;
                                // Try legacy path: properties.error (string)
                                if let Some(props) = json.get("properties") {
                                    if let Some(err) = props.get("error").and_then(|e| e.as_str()) {
                                        extracted_err = Some(err.to_string());
                                    }
                                }
                                // Try current opencode path: error.data.message (string)
                                if extracted_err.is_none() {
                                    if let Some(err_obj) = json.get("error") {
                                        if let Some(data) = err_obj.get("data") {
                                            if let Some(msg) = data.get("message").and_then(|m| m.as_str()) {
                                                extracted_err = Some(msg.to_string());
                                            }
                                        }
                                        // Fallback: error.message (string)
                                        if extracted_err.is_none() {
                                            if let Some(msg) = err_obj.get("message").and_then(|m| m.as_str()) {
                                                extracted_err = Some(msg.to_string());
                                            }
                                        }
                                        // Last resort: include the error name
                                        if extracted_err.is_none() {
                                            if let Some(name) = err_obj.get("name").and_then(|n| n.as_str()) {
                                                extracted_err = Some(format!("OpenCode error: {}", name));
                                            }
                                        }
                                    }
                                }
                                if let Some(err) = extracted_err {
                                    tracing::warn!(mission_id = %mission_id, error = %err, "OpenCode JSON error event");
                                    if final_result.is_empty() {
                                        final_result = err;
                                    }
                                } else if final_result.is_empty() {
                                    // Absolute fallback - include the raw JSON type info
                                    final_result = "OpenCode returned an error event with no parsable message".to_string();
                                }
                            }

                            // Route through SSE event parser for thinking/tool events.
                            // Skip events already handled inline to avoid double processing
                            // (e.g. step_finish would set message_complete in the SSE parser
                            // even for tool-call steps, conflicting with the inline handler).
                            let skip_sse = matches!(event_type, "step_finish" | "step_start" | "text");
                            let current_session = session_id_capture.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            if !skip_sse {
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
                                if let Some(model) = parsed.model {
                                    model_used = Some(model);
                                }
                                // Only accumulate usage from stdout when the dedicated SSE
                                // curl task is not running.  When both paths are active they
                                // can see the same `response.completed` event, which would
                                // double-count tokens (and inflate cost estimates to ~2x).
                                if sse_handle.is_none() {
                                    if let Some(usage) = parsed.usage {
                                        total_input_tokens = total_input_tokens
                                            .saturating_add(usage.input_tokens);
                                        total_output_tokens = total_output_tokens
                                            .saturating_add(usage.output_tokens);
                                        total_cache_creation_input_tokens =
                                            total_cache_creation_input_tokens.saturating_add(
                                                usage.cache_creation_input_tokens.unwrap_or(0),
                                            );
                                        total_cache_read_input_tokens = total_cache_read_input_tokens
                                            .saturating_add(
                                                usage.cache_read_input_tokens.unwrap_or(0),
                                            );
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
                                    if let AgentEvent::Thinking { ref content, .. } = event {
                                        if !content.trim().is_empty() {
                                            *sse_last_thinking
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner()) = content.clone();
                                        }
                                        sse_emitted_thinking.store(true, std::sync::atomic::Ordering::SeqCst);
                                        // New thinking content arrived; reset done flag so this
                                        // turn's thinking block will get its own done event.
                                        sse_done_sent.store(false, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    if matches!(event, AgentEvent::TextDelta { .. }) {
                                        let _ = text_output_tx.send(true);
                                        sse_emitted_text.store(true, std::sync::atomic::Ordering::SeqCst);
                                    }
                                    // Track active tool depth so inactivity timeouts
                                    // don't kill the process mid-tool-run (builds,
                                    // web fetches, etc.).
                                    if let Some(ref tx) = json_tool_depth_tx {
                                        match event {
                                            AgentEvent::ToolCall { .. } => {
                                                tx.send_modify(|v| *v = v.saturating_add(1));
                                            }
                                            AgentEvent::ToolResult { .. } => {
                                                tx.send_modify(|v| *v = v.saturating_sub(1));
                                            }
                                            _ => {}
                                        }
                                    }
                                    remember_tool_result_text(&event, &latest_tool_result_text);
                                    let _ = events_tx.send(event);
                                }
                                for event in parsed.extra_events {
                                    if let Some(ref tx) = json_tool_depth_tx {
                                        match event {
                                            AgentEvent::ToolCall { .. } => {
                                                tx.send_modify(|v| *v = v.saturating_add(1));
                                            }
                                            AgentEvent::ToolResult { .. } => {
                                                tx.send_modify(|v| *v = v.saturating_sub(1));
                                            }
                                            _ => {}
                                        }
                                    }
                                    remember_tool_result_text(&event, &latest_tool_result_text);
                                    let _ = events_tx.send(event);
                                }
                                if parsed.message_complete {
                                    let _ = sse_complete_tx.send(true);
                                    // Send thinking done signal if needed
                                    if sse_emitted_thinking.load(std::sync::atomic::Ordering::SeqCst)
                                        && !sse_done_sent.load(std::sync::atomic::Ordering::SeqCst)
                                    {
                                        // Block-final event: carry the cumulative thinking so
                                        // persisted history keeps the full block (incremental
                                        // done:false deltas are not persisted).
                                        let last_thinking = sse_last_thinking
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .clone();
                                        let _ = events_tx
                                            .send(thinking_final_event(last_thinking, mission_id));
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
                            } // !skip_sse
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

    // Check exit status.
    // When we intentionally killed the process after seeing step_finish/completion
    // (sse_complete_seen), don't treat the SIGKILL as an error — we have the response.
    if let Ok(status) = exit_status {
        if !status.success() && !sse_complete_seen {
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
    // Persist the opencode session id so the next turn can resume the
    // conversation with `--session <id>`. Mirrors the path used by Grok
    // (see `AgentEvent::SessionIdUpdate` emission in `run_grok_turn`).
    if let Some(sid) = session_id.as_deref() {
        let _ = events_tx.send(AgentEvent::SessionIdUpdate {
            mission_id,
            session_id: sid.to_string(),
        });
    }
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

    // Detect and truncate garbled/repetitive output where the model echoes
    // tool results (SSH warnings, nvidia-smi tables, etc.) verbatim in its
    // text response instead of summarizing them. This produces extremely
    // long assistant messages with >80% line repetition and repeated tool
    // output blocks. Truncate to the first unique-content region.
    if let Some(truncated) = truncate_garbled_output(&final_result) {
        tracing::warn!(
            mission_id = %mission_id,
            original_len = final_result.len(),
            truncated_len = truncated.len(),
            "Truncated garbled/repetitive assistant output"
        );
        final_result = truncated;
    }

    if let Ok(guard) = latest_tool_result_text.lock() {
        if let Some(tool_output) = guard.as_deref() {
            if let Some(repaired) =
                replace_filepath_artifact_with_tool_output(&final_result, tool_output)
            {
                tracing::info!(
                    mission_id = %mission_id,
                    "Replaced filepath-style OpenCode final output with latest tool result text"
                );
                final_result = repaired;
            }
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
                    content: reasoning.clone(),
                    done: false,
                    mission_id: Some(mission_id),
                });
                *sse_last_thinking.lock().unwrap_or_else(|e| e.into_inner()) = reasoning;
                emitted_thinking = true;
            }
        }
    }

    if emitted_thinking || (sse_emitted && !sse_done_sent.load(std::sync::atomic::Ordering::SeqCst))
    {
        // Block-final event with the full content — the only persisted form.
        let last_thinking = sse_last_thinking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let _ = events_tx.send(thinking_final_event(last_thinking, mission_id));
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

    // A timeout-killed OpenCode process is not a successful turn, even when it
    // emitted partial text first. Returning partial text as TurnComplete caused
    // Telegram to send "Je m'en occupe" followed by a warning while the actual
    // tool-backed work never finished.
    if killed_by_idle_timeout {
        tracing::warn!(
            mission_id = %mission_id,
            result_len = final_result.len(),
            "OpenCode idle timeout killed process; marking turn as stalled"
        );
        had_error = true;
        final_result = opencode_idle_timeout_result_message(&final_result);
    }

    tracing::info!(
        mission_id = %mission_id,
        had_error = had_error,
        result_len = final_result.len(),
        "OpenCode CLI execution completed"
    );

    if let Some(objective) = opencode_goal_objective.as_deref() {
        if let Some(status) = opencode_goal_terminal_status(&final_result) {
            let _ = events_tx.send(AgentEvent::GoalStatus {
                status: status.to_string(),
                objective: objective.to_string(),
                mission_id: Some(mission_id),
            });
        }
    }

    let mut result = if had_error {
        // Use RateLimited terminal reason when rate limit was detected
        let reason = if rate_limit_detected.load(std::sync::atomic::Ordering::SeqCst) {
            TerminalReason::RateLimited
        } else if killed_by_idle_timeout {
            TerminalReason::Stalled
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_result, 0).with_terminal_reason(reason)
    } else {
        AgentResult::success(final_result, 0).with_terminal_reason(TerminalReason::TurnComplete)
    };
    let success_signal = if sse_complete_seen {
        CompletionSignal::NativeTerminal
    } else if session_idle_seen {
        CompletionSignal::SessionIdle
    } else {
        CompletionSignal::ProcessExit
    };
    let success_confidence = if sse_complete_seen {
        CompletionConfidence::High
    } else if session_idle_seen {
        CompletionConfidence::Medium
    } else {
        CompletionConfidence::Low
    };
    let outcome = turn_outcome_for_result(&result, success_signal, success_confidence);
    result = result.with_turn_outcome(outcome);
    if model_used.is_none() {
        if let Some(model) = resolved_model.as_deref() {
            if !model.starts_with("builtin/") {
                model_used = Some(model.to_string());
            }
        }
    }

    // Compute cost from accumulated token usage and model (if available)
    if total_input_tokens > 0
        || total_output_tokens > 0
        || total_cache_creation_input_tokens > 0
        || total_cache_read_input_tokens > 0
    {
        let usage = crate::cost::TokenUsage {
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
            cache_creation_input_tokens: (total_cache_creation_input_tokens > 0)
                .then_some(total_cache_creation_input_tokens),
            cache_read_input_tokens: (total_cache_read_input_tokens > 0)
                .then_some(total_cache_read_input_tokens),
        };
        let (cost_cents, cost_source) =
            resolve_cost_cents_and_source(None, model_used.as_deref(), &usage);
        result.cost_cents = cost_cents;
        result.cost_source = cost_source;
        result = result.with_usage(usage);
        tracing::info!(
            mission_id = %mission_id,
            input_tokens = total_input_tokens,
            output_tokens = total_output_tokens,
            cost_cents = cost_cents,
            cost_source = ?cost_source,
            model = ?model_used,
            "OpenCode turn cost resolved from SSE usage"
        );
    }

    if let Some(model) = model_used {
        result = result.with_model(model);
    }

    // Clean up the temp prompt file (best-effort; the workspace may clean it later)
    let _ = std::fs::remove_file(&prompt_file_host);

    result
}

fn opencode_path(
    _mission_path: Option<&str>,
    work_dir_arg: &str,
    telegram_action_helpers_enabled: bool,
) -> String {
    let process_path = std::env::var("PATH").unwrap_or_default();
    let current_path = process_path.as_str();
    let bun_bins = "/root/.bun/bin:/root/.cache/.bun/bin";
    let mut path_parts = Vec::new();
    if !current_path.contains("/root/.bun/bin") {
        path_parts.push(bun_bins.to_string());
    }
    path_parts.push(current_path.to_string());
    if telegram_action_helpers_enabled {
        path_parts.push(format!("{work_dir_arg}/.sandboxed-sh-bin"));
    }
    path_parts.join(":")
}

#[cfg(test)]
mod path_tests {
    use super::opencode_path;

    #[test]
    fn opencode_path_preserves_mission_path_prepends() {
        let path = opencode_path(
            Some("/mission/.sandboxed-sh/bin:/usr/bin"),
            "/mission",
            true,
        );

        assert!(path.contains("/mission/.sandboxed-sh/bin:/usr/bin"));
        assert!(path.ends_with("/mission/.sandboxed-sh-bin"));
    }
}
