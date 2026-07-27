//! Gemini CLI turn runner.
//!
//! Moved verbatim from `mission_runner.rs` (Phase 2 of the decomposition).

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{
    AgentResult, CompletionConfidence, CompletionSignal, FailureClass, TerminalReason, TurnOutcome,
};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::*;
use crate::cost::resolve_cost_cents_and_source;
use crate::workspace::Workspace;
use crate::workspace_exec::WorkspaceExec;

fn gemini_bootstrap_failure_class(error: &str) -> FailureClass {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("connectionrefused")
        || normalized.contains("connection refused")
        || normalized.contains("timed out")
        || normalized.contains("timeout")
        || normalized.contains("network")
    {
        FailureClass::TransportError
    } else {
        FailureClass::AgentError
    }
}

/// Run a single Gemini CLI turn for a mission.
#[allow(clippy::too_many_arguments)]
pub async fn run_gemini_turn(
    workspace: &Workspace,
    mission_work_dir: &std::path::Path,
    user_message: &str,
    model: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    _session_id: Option<&str>,
) -> AgentResult {
    use crate::backend::events::ExecutionEvent;
    use crate::backend::gemini::GeminiBackend;
    use crate::backend::{Backend, SessionConfig};

    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let resolved_model: Option<String> = model.map(|m| m.to_string());

    tracing::info!(
        mission_id = %mission_id,
        requested_model = ?model,
        resolved_model = ?resolved_model,
        agent = ?agent,
        "Starting Gemini CLI turn"
    );

    // Get Google credentials for Gemini CLI
    let gemini_creds = get_google_credentials_for_gemini(app_working_dir);
    match &gemini_creds {
        GeminiCredentials::ApiKey(k) => {
            tracing::info!(
                "Using Gemini API key (prefix: {}...)",
                k.chars().take(8).collect::<String>()
            );
        }
        GeminiCredentials::OAuth { .. } => {
            tracing::info!("Using Google OAuth credentials for Gemini CLI");
        }
        GeminiCredentials::None => {
            tracing::warn!(
                "No Google credentials found for Gemini CLI; will rely on CLI's own auth"
            );
        }
    }

    let workspace_exec = WorkspaceExec::new(workspace.clone());
    let cli_path = get_backend_string_setting("gemini", "cli_path")
        .or_else(|| std::env::var("GEMINI_CLI_PATH").ok())
        .unwrap_or_else(|| "gemini".to_string());

    // Ensure Gemini CLI is available, auto-install if needed
    let cli_path =
        match ensure_gemini_cli_available(&workspace_exec, mission_work_dir, &cli_path).await {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Gemini CLI not available: {}", e);
                let message = format!("Gemini CLI not available: {}", e);
                return AgentResult::failure(message.clone(), 0).with_turn_outcome(
                    TurnOutcome::Failed {
                        reason: TerminalReason::LlmError,
                        source: Some(gemini_bootstrap_failure_class(&e)),
                        message: Some(message),
                    },
                );
            }
        };

    // Ensure ~/.gemini directory exists (gemini CLI needs it for projects.json and settings).
    // Use $HOME so this works for non-root users in host workspaces.
    let gemini_dir_result = workspace_exec
        .output(
            mission_work_dir,
            "/bin/sh",
            &[
                "-c".to_string(),
                r#"mkdir -p "${HOME:-/root}/.gemini""#.to_string(),
            ],
            std::collections::HashMap::new(),
        )
        .await;
    if let Err(e) = &gemini_dir_result {
        tracing::warn!("Failed to create ~/.gemini directory: {}", e);
    }

    // Configure auth in the container based on credential type
    let api_key = match &gemini_creds {
        GeminiCredentials::ApiKey(key) => {
            // Write settings.json for API key auth
            if let Err(e) = workspace_exec
                .output(
                    mission_work_dir,
                    "/bin/sh",
                    &[
                        "-c".to_string(),
                        r#"echo '{"security":{"auth":{"selectedType":"gemini-api-key"}}}' > "${HOME:-/root}/.gemini/settings.json""#.to_string(),
                    ],
                    std::collections::HashMap::new(),
                )
                .await
            {
                tracing::warn!("Failed to write Gemini settings.json: {}", e);
            }
            Some(key.clone())
        }
        GeminiCredentials::OAuth {
            access_token,
            refresh_token,
            expires_at,
        } => {
            // Write settings.json for OAuth auth
            if let Err(e) = workspace_exec
                .output(
                    mission_work_dir,
                    "/bin/sh",
                    &[
                        "-c".to_string(),
                        r#"echo '{"security":{"auth":{"selectedType":"oauth-personal"}}}' > "${HOME:-/root}/.gemini/settings.json""#.to_string(),
                    ],
                    std::collections::HashMap::new(),
                )
                .await
            {
                tracing::warn!("Failed to write Gemini settings.json for OAuth: {}", e);
            }
            // Write OAuth credentials file for the CLI to pick up
            let oauth_creds = serde_json::json!({
                "access_token": access_token,
                "refresh_token": refresh_token,
                "token_type": "Bearer",
                "expiry_date": expires_at
            });
            let creds_json = serde_json::to_string(&oauth_creds).unwrap_or_default();
            // Escape single quotes in the JSON for shell
            let escaped = creds_json.replace('\'', "'\\''");
            if let Err(e) = workspace_exec
                .output(
                    mission_work_dir,
                    "/bin/sh",
                    &[
                        "-c".to_string(),
                        format!(
                            r#"echo '{}' > "${{HOME:-/root}}/.gemini/oauth_creds.json""#,
                            escaped
                        ),
                    ],
                    std::collections::HashMap::new(),
                )
                .await
            {
                tracing::warn!("Failed to write Gemini OAuth credentials: {}", e);
            }
            // Don't set GEMINI_API_KEY for OAuth - the CLI uses its own credential store
            None
        }
        GeminiCredentials::None => None,
    };

    tracing::info!(
        mission_id = %mission_id,
        workspace_type = ?workspace.workspace_type,
        cli_path = %cli_path,
        model = ?model,
        has_api_key = api_key.is_some(),
        auth_type = ?gemini_creds.auth_type_str(),
        "Starting Gemini CLI execution via WorkspaceExec"
    );

    let gemini_config = crate::backend::gemini::client::GeminiConfig {
        cli_path,
        api_key,
        default_model: resolved_model.clone(),
        force_file_storage: matches!(gemini_creds, GeminiCredentials::OAuth { .. }),
    };

    let backend = GeminiBackend::with_config_and_workspace(gemini_config, workspace_exec);

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
            tracing::error!("Failed to create Gemini session: {}", e);
            return AgentResult::failure(format!("Failed to start Gemini CLI: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Send message streaming
    let (mut event_rx, handle) = match backend.send_message_streaming(&session, user_message).await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("Failed to send message to Gemini CLI: {}", e);
            return AgentResult::failure(format!("Gemini CLI execution failed: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Process events until completion or cancellation
    // Gemini usually emits incremental token deltas. Keep canonical
    // cumulative buffers anyway so a future CLI snapshot event cannot
    // duplicate streamed words in the UI.
    let mut assistant_message = String::new();
    let mut success = false;
    let mut error_message: Option<String> = None;
    let mut pending_tools: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut thinking_emitted = false;
    let mut thinking_done_emitted = false;
    let mut thinking_accumulated = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    // See run_codex_turn: on cancellation we break instead of returning so
    // the post-loop fallback can surface accumulated text / reasoning as the
    // final assistant message.
    let mut cancelled = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("Gemini turn cancelled for mission {}", mission_id);
                // Kill the Gemini CLI child process to stop consuming API resources
                backend.kill().await;
                // Abort the event-conversion task
                handle.abort();
                cancelled = true;
                break;
            }
            Some(event) = event_rx.recv() => {
                match event {
                    ExecutionEvent::TextDelta { content } => {
                        merge_stream_fragment(&mut assistant_message, &content);
                        let _ = events_tx.send(AgentEvent::TextDelta {
                            content: assistant_message.clone(),
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::Thinking { content, item_id: _ } => {
                        if thinking_overlaps_visible_answer(&content, &assistant_message) {
                            tracing::debug!(
                                thinking_len = content.len(),
                                assistant_len = assistant_message.len(),
                                "Dropping Gemini thinking event that duplicates visible assistant text"
                            );
                            continue;
                        }
                        merge_stream_fragment(&mut thinking_accumulated, &content);
                        // Stream the canonical cumulative buffer for real-time UI.
                        let _ = events_tx.send(AgentEvent::Thinking {
                            content: thinking_accumulated.clone(),
                            done: false,
                            mission_id: Some(mission_id),
                        });
                        if !thinking_accumulated.is_empty() {
                            thinking_done_emitted = false;
                        }
                        thinking_emitted = true;
                    }
                    ExecutionEvent::ToolCall { id, name, args } => {
                        // Flush accumulated thinking before tool call
                        if !thinking_accumulated.is_empty() {
                            let _ = events_tx.send(thinking_final_event(
                                std::mem::take(&mut thinking_accumulated),
                                mission_id,
                            ));
                            thinking_done_emitted = true;
                        }
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
                            tracing::debug!("Gemini turn summary: {}", content);
                        }
                    }
                    ExecutionEvent::Usage { input_tokens, output_tokens } => {
                        total_input_tokens = total_input_tokens.saturating_add(input_tokens);
                        total_output_tokens = total_output_tokens.saturating_add(output_tokens);
                    }
                    // Goal events don't apply to Gemini today (no /goal
                    // continuation loop for that backend), but we still
                    // forward them so a future Gemini integration that
                    // adds goal mode just works.
                    ExecutionEvent::GoalIteration { iteration, objective } => {
                        let _ = events_tx.send(AgentEvent::GoalIteration {
                            iteration,
                            objective,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::GoalStatus { status, objective } => {
                        let _ = events_tx.send(AgentEvent::GoalStatus {
                            status,
                            objective,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::Cancelled => {
                        cancelled = true;
                        break;
                    }
                    ExecutionEvent::Error { message } => {
                        error_message = Some(message.clone());
                        tracing::error!("Gemini CLI error: {}", message);
                    }
                    ExecutionEvent::MessageComplete { session_id: _ } => {
                        success = error_message.is_none();
                        break;
                    }
                }
            }
            else => {
                break;
            }
        }
    }

    // See run_codex_turn: capture thinking before the flush below moves it,
    // so the final-message picker can surface it when no text was produced.
    let thinking_for_fallback = if thinking_accumulated.trim().is_empty() {
        None
    } else {
        Some(thinking_accumulated.clone())
    };

    // Flush any remaining accumulated thinking with full content
    if thinking_emitted && !thinking_done_emitted {
        let _ = events_tx.send(thinking_final_event(thinking_accumulated, mission_id));
    }

    let no_output = assistant_message.trim().is_empty() && thinking_for_fallback.is_none();
    if no_output && error_message.is_none() && !cancelled {
        success = false;
        error_message = Some(
            "Gemini CLI produced no output. Check that the Gemini CLI is installed and configured with valid credentials (GEMINI_API_KEY or Google OAuth)."
                .to_string(),
        );
    }

    // See run_codex_turn: snapshot the cancel marker once to keep the
    // output/terminal_reason pair consistent if shutdown fires mid-finalize.
    let cancel_marker = if cancelled {
        Some(cancel_or_shutdown_failure())
    } else {
        None
    };

    let final_message = if let Some(err) = error_message {
        err
    } else if !assistant_message.is_empty() {
        assistant_message
    } else if let Some(thinking_text) = thinking_for_fallback {
        thinking_text
    } else if let Some(marker) = cancel_marker.as_ref() {
        marker.output.clone()
    } else {
        "No response from Gemini CLI".to_string()
    };

    let usage = crate::cost::TokenUsage {
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };

    let model_for_cost = resolved_model.as_deref();
    let (cost_cents, cost_source) = resolve_cost_cents_and_source(None, model_for_cost, &usage);

    let mut result = if let Some(marker) = cancel_marker {
        let cancel_reason = marker.terminal_reason.unwrap_or(TerminalReason::Cancelled);
        AgentResult::failure(final_message, cost_cents).with_terminal_reason(cancel_reason)
    } else if success {
        AgentResult::success(final_message, cost_cents)
            .with_terminal_reason(TerminalReason::TurnComplete)
    } else {
        let reason = if is_rate_limited_error(&final_message) {
            TerminalReason::RateLimited
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_message, cost_cents).with_terminal_reason(reason)
    };

    let outcome = turn_outcome_for_result(
        &result,
        CompletionSignal::ProcessExit,
        CompletionConfidence::Low,
    );
    result = result.with_turn_outcome(outcome);
    result = result.with_cost_source(cost_source);
    if usage.has_usage() {
        result = result.with_usage(usage);
    }
    if let Some(m) = resolved_model.as_deref() {
        result = result.with_model(m.to_string());
    }

    result
}

/// Credentials for the Gemini CLI backend.
#[derive(Debug)]
enum GeminiCredentials {
    /// A Gemini API key (from ai_providers.json or GEMINI_API_KEY env var)
    ApiKey(String),
    /// Google OAuth credentials (access token + refresh token from credentials store)
    OAuth {
        access_token: String,
        refresh_token: String,
        expires_at: i64,
    },
    /// No credentials found
    None,
}

impl GeminiCredentials {
    fn auth_type_str(&self) -> &'static str {
        match self {
            GeminiCredentials::ApiKey(_) => "api-key",
            GeminiCredentials::OAuth { .. } => "oauth",
            GeminiCredentials::None => "none",
        }
    }
}

/// Get Google credentials for the Gemini CLI backend.
///
/// Checks (in order):
/// 1. Environment variables (GEMINI_API_KEY, GOOGLE_API_KEY, etc.)
/// 2. AI provider store for a Google provider with an API key
/// 3. Sandboxed-sh credentials store for Google OAuth credentials
/// 4. OpenCode's auth.json for Google API key or OAuth credentials
fn get_google_credentials_for_gemini(working_dir: &std::path::Path) -> GeminiCredentials {
    // 1. Check environment variables first (most explicit)
    if let Some(key) = env_google_api_key() {
        return GeminiCredentials::ApiKey(key);
    }

    let google_targets_gemini = crate::api::ai_providers::provider_targets_backend(
        working_dir,
        crate::ai_providers::ProviderType::Google,
        "gemini",
    );

    if !google_targets_gemini {
        tracing::info!(
            "Google provider does not target 'gemini' backend; skipping provider credentials"
        );
        return GeminiCredentials::None;
    }
    // 2. Try to get API key from the AI provider store
    let store_path = working_dir.join(crate::util::AI_PROVIDERS_PATH);
    if let Ok(store) = std::fs::read_to_string(&store_path) {
        if let Ok(providers) = serde_json::from_str::<serde_json::Value>(&store) {
            if let Some(providers_arr) = providers.as_array() {
                for provider in providers_arr {
                    let pt = match provider.get("provider_type").and_then(|v| v.as_str()) {
                        Some(t) => t,
                        None => continue,
                    };
                    if pt != "google" {
                        continue;
                    }
                    let enabled = provider
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if !enabled {
                        continue;
                    }
                    if let Some(key) = provider.get("api_key").and_then(|v| v.as_str()) {
                        if !key.is_empty() {
                            tracing::info!("Using Google API key from ai_providers.json");
                            return GeminiCredentials::ApiKey(key.to_string());
                        }
                    }
                }
            }
        }
    }

    // 3. Try sandboxed-sh credentials store for OAuth
    if let Some(creds) = read_google_oauth_from_credentials() {
        return creds;
    }

    // 4. Try OpenCode's auth.json
    if let Some(creds) = read_google_credentials_from_opencode_auth() {
        return creds;
    }

    GeminiCredentials::None
}

/// Read Google OAuth credentials from the sandboxed-sh credentials store.
fn read_google_oauth_from_credentials() -> Option<GeminiCredentials> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let candidates = [
        std::path::PathBuf::from(&home)
            .join(".sandboxed-sh")
            .join("credentials.json"),
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".sandboxed-sh")
            .join("credentials.json"),
    ];
    let creds_path = candidates.iter().find(|p| p.exists())?;
    let contents = std::fs::read_to_string(creds_path).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&contents).ok()?;

    for key_name in ["google", "gemini"] {
        let entry = match auth.get(key_name) {
            Some(e) => e,
            None => continue,
        };
        let access_token = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
        let refresh_token = entry.get("refresh").and_then(|v| v.as_str()).unwrap_or("");
        let expires_at = entry.get("expires").and_then(|v| v.as_i64()).unwrap_or(0);

        if access_token.is_empty() || refresh_token.is_empty() {
            continue;
        }

        tracing::info!("Using Google OAuth credentials from credentials.json for Gemini CLI");
        return Some(GeminiCredentials::OAuth {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at,
        });
    }
    None
}

/// Read Google API key or OAuth credentials from OpenCode's auth.json.
fn read_google_credentials_from_opencode_auth() -> Option<GeminiCredentials> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let mut candidates = Vec::new();
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        candidates.push(
            std::path::PathBuf::from(data_home)
                .join("opencode")
                .join("auth.json"),
        );
    }
    candidates.push(
        std::path::PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    candidates.push(
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    let auth_path = candidates.iter().find(|p| p.exists())?;
    let contents = std::fs::read_to_string(auth_path).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&contents).ok()?;

    for key_name in ["google", "gemini"] {
        if let Some(entry) = auth.get(key_name) {
            // Check for API key first
            for field in ["key", "api_key"] {
                if let Some(key) = entry.get(field).and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        let entry_type = entry.get("type").and_then(|v| v.as_str());
                        if entry_type != Some("oauth") {
                            tracing::info!("Using Google API key from OpenCode auth.json");
                            return Some(GeminiCredentials::ApiKey(key.to_string()));
                        }
                    }
                }
            }
            // Check for OAuth credentials
            let access = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
            let refresh = entry.get("refresh").and_then(|v| v.as_str()).unwrap_or("");
            let expires = entry.get("expires").and_then(|v| v.as_i64()).unwrap_or(0);
            if !access.is_empty() && !refresh.is_empty() {
                tracing::info!(
                    "Using Google OAuth credentials from OpenCode auth.json for Gemini CLI"
                );
                return Some(GeminiCredentials::OAuth {
                    access_token: access.to_string(),
                    refresh_token: refresh.to_string(),
                    expires_at: expires,
                });
            }
        }
    }
    None
}

/// Get Google API key from environment variables.
fn env_google_api_key() -> Option<String> {
    for var in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_GENERATIVE_AI_API_KEY",
    ] {
        if let Ok(key) = std::env::var(var) {
            let key = key.trim().to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_bootstrap_connection_refusal_is_transport() {
        assert_eq!(
            gemini_bootstrap_failure_class(
                "Gemini CLI install failed: ConnectionRefused downloading package manifest"
            ),
            FailureClass::TransportError
        );
    }

    #[test]
    fn gemini_missing_cli_without_network_signal_is_agent_error() {
        assert_eq!(
            gemini_bootstrap_failure_class("no package manager is available"),
            FailureClass::AgentError
        );
    }
}
