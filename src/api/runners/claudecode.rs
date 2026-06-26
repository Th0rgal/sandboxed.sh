//! Claude Code CLI turn runner.
//!
//! Moved verbatim from `mission_runner.rs` (Phase 2 of the decomposition).
//! Kept as a sync fn returning `Pin<Box<dyn Future>>`: the boxed future is
//! the fix for the debug-build async stack overflow (see CLAUDE.md notes) —
//! do not convert to `async fn`.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, CompletionConfidence, CompletionSignal, TerminalReason};
use crate::api::control::AgentEvent;
use tokio::sync::RwLock;

use crate::api::control::{safe_truncate_index, ControlRunState, ControlStatus, FrontendToolHub};
use crate::api::mission_runner::*;
use crate::backend::claudecode::client::{ClaudeEvent, ContentBlock, StreamEvent};
use crate::cost::resolve_cost_cents_and_source;
use crate::secrets::SecretsStore;
use crate::util::{build_history_context, env_var_bool};
use crate::workspace::{Workspace, WorkspaceType};
use crate::workspace_exec::WorkspaceExec;

/// Map a mission `model_effort` to a Claude Code extended-thinking budget
/// (`MAX_THINKING_TOKENS`).
///
/// `CLAUDE_CODE_EFFORT_LEVEL` alone only nudges *adaptive* reasoning: on
/// tool-heavy turns the model frequently chooses not to think at all, so no
/// `thinking_delta` blocks stream and the Thoughts panel stays empty (see
/// mission 5aede562, which ran at effort=max yet recorded 0 thinking events
/// across ~1600 tool calls). Pinning a non-zero budget forces an extended
/// thinking block every turn, so thoughts are captured deterministically.
///
/// Returns 0 for unknown efforts, leaving thinking fully adaptive.
fn claude_thinking_budget(effort: &str) -> u32 {
    match effort.trim().to_ascii_lowercase().as_str() {
        "max" => 32_000,
        "xhigh" => 24_000,
        "high" => 16_000,
        "medium" => 8_000,
        "low" => 4_000,
        _ => 0,
    }
}

/// Marker emitted by Claude Code's `Bash` tool when invoked with
/// `run_in_background: true`. The CLI handles the launch locally and returns a
/// confirmation `tool_result` of the exact form:
///
/// `Command running in background with ID: <id>. Output is being written to: <path>`
///
/// In `--print` (non-interactive) mode no further re-invocation happens when the
/// job finishes, so nothing wakes the agent — the background-task auto-resume
/// watcher (see `supervision::background_task_autoresume_loop`) closes that gap.
const BACKGROUND_TASK_ID_MARKER: &str = "Command running in background with ID:";
const BACKGROUND_TASK_OUTPUT_MARKER: &str = "Output is being written to:";

/// Pure parser for the Claude Code background-start marker.
///
/// Given a `Bash` tool_result's textual content, returns `Some((id, output_path))`
/// when the content is a background-launch confirmation, or `None` otherwise.
///
/// The marker format is:
/// `Command running in background with ID: <id>. Output is being written to: <path>`
///
/// We locate both labels by substring (rather than a strict full-string match)
/// so leading/trailing whitespace or a trailing newline in the CLI payload does
/// not break detection.
///
/// Both the id and the output path are single whitespace-free tokens. The real
/// marker continues with explanatory prose after the path
/// (`... tasks/bokwqyjak.output. You will be notified when it completes. ...`),
/// so for each label we take only the **first whitespace-delimited token** and
/// strip a single trailing `.` (the sentence separator). This prevents the
/// trailing marker sentence from being captured as part of the path.
pub fn parse_background_task_start(content: &str) -> Option<(String, String)> {
    let id_label = content.find(BACKGROUND_TASK_ID_MARKER)?;
    let after_id = &content[id_label + BACKGROUND_TASK_ID_MARKER.len()..];

    // The output label must come *after* the id label.
    let out_rel = after_id.find(BACKGROUND_TASK_OUTPUT_MARKER)?;
    let id_segment = &after_id[..out_rel];
    let after_out = &after_id[out_rel + BACKGROUND_TASK_OUTPUT_MARKER.len()..];

    // The id is the first whitespace-delimited token between the two labels,
    // terminated by the literal "." that precedes the output label. Strip a
    // single trailing period.
    let id = id_segment
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();

    // The path is the first whitespace-delimited token after the output label
    // (paths contain no spaces). Strip a single trailing "." (sentence
    // separator) so the trailing marker prose
    // ("... .output. You will be notified ...") is not captured.
    let path = after_out
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();

    if id.is_empty() || path.is_empty() {
        return None;
    }
    Some((id, path))
}

/// Classify a proxy health-probe result into pass/fast-fail.
///
/// The probe (`check_claudecode_proxy_health`) GETs `<base>/v1/models` through
/// the same CLI proxy the Claude CLI will use. The verdict is deliberately
/// conservative so it can only catch a genuinely-down/erroring proxy and never
/// false-blocks a slow-but-working one:
/// - any HTTP status in 2xx/3xx/4xx (incl. 401/404) proves the proxy is up and
///   routing → pass (the `/v1/models` route does not even need to exist);
/// - 5xx means the proxy's upstream is erroring (e.g. provider cooldown / 502),
///   which is exactly the stall we want to fail fast on → fail;
/// - a connection-level failure or no response (refused / DNS / unreachable /
///   timeout / `000`) → fail.
fn claudecode_proxy_health_verdict(combined: &str, http_code: &str) -> Result<(), String> {
    if combined.contains("Could not resolve host") {
        return Err("Claude CLI proxy unreachable: DNS resolution failed".to_string());
    }
    if combined.contains("Connection refused") {
        return Err(
            "Claude CLI proxy unreachable: connection refused (proxy not listening?)".to_string(),
        );
    }
    if combined.contains("Network is unreachable") {
        return Err("Claude CLI proxy unreachable: network is unreachable".to_string());
    }
    if combined.contains("Connection timed out") || combined.contains("Operation timed out") {
        return Err("Claude CLI proxy unreachable: connection timed out".to_string());
    }

    let code = http_code.trim();
    if let Ok(n) = code.parse::<u16>() {
        if n == 0 {
            // curl's `000` sentinel: TCP/TLS connected attempt produced no HTTP
            // response (proxy down or hung).
            return Err(
                "Claude CLI proxy did not return an HTTP response (proxy down or hung)".to_string(),
            );
        }
        if (500..=599).contains(&n) {
            return Err(format!(
                "Claude CLI proxy upstream is erroring (HTTP {n}); failing fast instead of \
                 waiting out the startup timeout"
            ));
        }
        // Any other HTTP response (2xx/3xx/4xx, including 401/404) proves the
        // proxy is up and routing requests.
        return Ok(());
    }

    // Unparseable/empty code: treat as no response.
    Err("Claude CLI proxy did not return an HTTP response (proxy down or hung)".to_string())
}

/// Fast health check for the CLI proxy auth path. Mirrors `check_api_reachability`'s
/// in-sandbox curl, but probes the proxy (which proxy environments allow) rather
/// than api.anthropic.com (which they block). The API key is passed via env so it
/// never lands in the command string or logs.
async fn check_claudecode_proxy_health(
    workspace_exec: &WorkspaceExec,
    cwd: &std::path::Path,
    base_url: &str,
    api_key: &str,
) -> Result<(), String> {
    let timeout_secs = std::env::var("SANDBOXED_SH_CLAUDECODE_PROXY_HEALTH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(8);
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let test_cmd = format!(
        "curl -sS -o /dev/null -w '%{{http_code}}' --max-time {timeout_secs} \
         -H \"Authorization: Bearer ${{CLAUDE_PROXY_HEALTH_KEY}}\" {url}"
    );
    let mut env = std::collections::HashMap::new();
    env.insert("CLAUDE_PROXY_HEALTH_KEY".to_string(), api_key.to_string());

    let output = match workspace_exec
        .output(cwd, "/bin/sh", &["-c".to_string(), test_cmd], env)
        .await
    {
        Ok(out) => out,
        // An exec error here is about the sandbox, not the proxy; don't block.
        Err(e) => {
            tracing::debug!("Claude proxy health probe could not run ({e}); skipping fast-fail");
            return Ok(());
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    claudecode_proxy_health_verdict(&combined, stdout.trim())
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
    model_effort: Option<&'a str>,
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
    override_auth: Option<crate::api::ai_providers::ClaudeCodeAuth>,
    force_argv_prompt: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentResult> + Send + 'a>> {
    Box::pin(async move {
        use crate::api::ai_providers::{
            anthropic_cli_proxy_account_available, ensure_anthropic_oauth_token_valid,
            get_anthropic_auth_for_claudecode, get_anthropic_auth_from_host_with_expiry,
            get_anthropic_auth_from_workspace, get_workspace_auth_path,
            refresh_workspace_anthropic_auth, ClaudeCodeAuth,
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

        #[derive(Debug, Clone)]
        struct ClaudeCodeProxyConfig {
            base_url: String,
            api_key: String,
        }

        fn claudecode_cli_proxy_config() -> Option<ClaudeCodeProxyConfig> {
            // Only fall back to the CLI proxy when it is actually configured —
            // either via explicit env vars or a fresh CLI-proxy-api account.
            // Without this gate we would hijack any ANTHROPIC_* setup on hosts
            // that never opted into the proxy and inject the synthetic key.
            if !anthropic_cli_proxy_account_available() {
                return None;
            }

            // Note: ANTHROPIC_BASE_URL is intentionally *not* consulted here;
            // it is a standard Anthropic SDK variable and users set it for
            // unrelated API proxies. The aliases used here are the same ones
            // listed in `util::CLI_PROXY_BASE_URL_ENV_VARS` so every CLI-proxy
            // code path agrees.
            let base_url = crate::util::cli_proxy_base_url_from_env()
                .unwrap_or_else(|| "http://127.0.0.1:8317".to_string());
            let base_url = base_url.trim_end_matches('/').to_string();
            if base_url.is_empty() {
                return None;
            }

            // The CLI Proxy API commonly runs unauthenticated on localhost, but
            // Claude Code still requires a non-empty ANTHROPIC_API_KEY when an
            // Anthropic base URL is configured. If the proxy needs auth, pass
            // through the configured proxy key; otherwise use an inert value.
            let api_key = crate::util::cli_proxy_api_key_from_env()
                .unwrap_or_else(|| "sandboxed-sh-cli-proxy".to_string());

            Some(ClaudeCodeProxyConfig { base_url, api_key })
        }

        fn claude_cli_credentials_info(path: &std::path::Path) -> Option<(i64, bool)> {
            let (_, expires_at, _, has_refresh) = read_claude_cli_credentials(path)?;
            Some((expires_at, has_refresh))
        }

        /// Read the full claudeAiOauth payload from a credentials file.
        /// Returns `(access_token, expires_at, refresh_token, has_refresh)`.
        fn read_claude_cli_credentials(
            path: &std::path::Path,
        ) -> Option<(String, i64, String, bool)> {
            let metadata = std::fs::metadata(path).ok()?;
            if metadata.len() == 0 {
                return None;
            }
            let contents = std::fs::read_to_string(path).ok()?;
            let creds: serde_json::Value = serde_json::from_str(&contents).ok()?;
            let oauth = creds.get("claudeAiOauth")?;
            let access_token = oauth
                .get("accessToken")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())?;
            let expires_at = oauth
                .get("expiresAt")
                .and_then(|v| v.as_i64())
                .unwrap_or(i64::MAX);
            let refresh_token = oauth
                .get("refreshToken")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let has_refresh = !refresh_token.trim().is_empty();
            Some((access_token, expires_at, refresh_token, has_refresh))
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
        let using_override_auth = override_auth.is_some();
        if using_override_auth && mission_creds_path.exists() {
            match std::fs::remove_file(&mission_creds_path) {
                Ok(_) => {
                    tracing::info!(
                        mission_id = %mission_id,
                        path = %mission_creds_path.display(),
                        "Removed mission Claude CLI credentials so override auth can take precedence"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        path = %mission_creds_path.display(),
                        error = %e,
                        "Failed to remove mission Claude CLI credentials before override auth"
                    );
                }
            }
        }
        // Propagate mission → host BEFORE deciding whether to copy host → mission.
        // Anthropic's OAuth uses rotating refresh tokens (each refresh returns a
        // new refresh_token and invalidates the old one). If a previous turn's
        // Claude CLI rotated tokens inside the mission directory, the host file
        // still holds the old (now-invalid) refresh_token. Without this back-sync
        // the next backend refresh — or any sibling mission that copies host
        // creds — would hit "refresh_token already used" / invalid_grant.
        if !using_override_auth {
            if let (Some(host_path), Some((m_access, m_expires, m_refresh, m_has_refresh))) = (
                find_host_claude_cli_credentials(),
                read_claude_cli_credentials(&mission_creds_path),
            ) {
                if m_has_refresh {
                    let host_expires = claude_cli_credentials_info(&host_path)
                        .map(|(e, _)| e)
                        .unwrap_or(i64::MIN);
                    if m_expires > host_expires {
                        // Capture the host's pre-rotation refresh token so the
                        // store-reconcile pass can find the owning account record.
                        let host_old_refresh = read_claude_cli_credentials(&host_path)
                            .map(|(_, _, r, _)| r)
                            .unwrap_or_default();
                        tracing::info!(
                            mission_id = %mission_id,
                            mission_expires_at = m_expires,
                            host_expires_at = host_expires,
                            "Mission credentials are fresher than host; syncing back to all storage tiers"
                        );
                        match crate::api::ai_providers::sync_oauth_to_all_tiers(
                            crate::ai_providers::ProviderType::Anthropic,
                            &m_refresh,
                            &m_access,
                            m_expires,
                        ) {
                            Ok(()) => {
                                // The runner has no safe handle on the in-memory
                                // AIProviderStore (a second instance would
                                // split-brain ai_providers.json), so queue the
                                // rotation for the proactive loop — which owns the
                                // store — to apply against the matching record.
                                if let Err(e) = crate::api::oauth_reconcile::record_pending_rotation(
                                    &crate::api::oauth_reconcile::sidecar_path(app_working_dir),
                                    crate::api::oauth_reconcile::PendingRotation {
                                        provider: "anthropic".to_string(),
                                        old_refresh_token: host_old_refresh,
                                        new_refresh_token: m_refresh.clone(),
                                        new_access_token: m_access.clone(),
                                        expires_at: m_expires,
                                    },
                                ) {
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        error = %e,
                                        "Failed to queue Anthropic rotation for store reconcile"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    mission_id = %mission_id,
                                    error = %e,
                                    "Failed to write mission-rotated Anthropic credentials back to host"
                                );
                            }
                        }
                    }
                }
            }
        }

        // Copy host credentials if missing OR if the existing ones are expired/near-expiry.
        let needs_copy = if using_override_auth {
            false
        } else if !looks_like_claude_cli_credentials(&mission_creds_path) {
            true
        } else if let Some((expires_at, _)) = claude_cli_credentials_info(&mission_creds_path) {
            let now_ms = chrono::Utc::now().timestamp_millis();
            if expires_at < now_ms + 120_000 {
                true // expired or about to expire
            } else {
                // Re-copy only when host credentials are STRICTLY newer than the
                // mission's local copy. The previous `!=` check overwrote a
                // mission's freshly-rotated tokens with the host's stale ones
                // whenever the two diverged, which destroyed the only valid
                // refresh_token and triggered the invalid_grant we're guarding
                // against.
                if let Some(host_path) = find_host_claude_cli_credentials() {
                    if let Some((host_expires, _)) = claude_cli_credentials_info(&host_path) {
                        host_expires > expires_at
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        } else {
            false
        };
        // Proactive refresh: if host CLI credentials are expired or near-expiry,
        // refresh them before copying into the mission directory.  This prevents
        // the mission from starting with stale credentials that will fail mid-turn.
        if needs_copy {
            if let Some(host_creds_path) = find_host_claude_cli_credentials() {
                if let Some((host_expires, _)) = claude_cli_credentials_info(&host_creds_path) {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    if host_expires < now_ms + 300_000 {
                        // 5 minute buffer
                        tracing::info!(
                            mission_id = %mission_id,
                            host_expires_at = host_expires,
                            now_ms = now_ms,
                            "Host CLI credentials expired or near-expiry; triggering proactive OAuth refresh"
                        );
                        if let Err(e) =
                            crate::api::ai_providers::force_refresh_anthropic_oauth_token().await
                        {
                            tracing::warn!(
                                mission_id = %mission_id,
                                "Proactive OAuth refresh failed: {}",
                                e
                            );
                        }
                    }
                }
            }
        }
        if needs_copy {
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
        let mut has_cli_creds =
            !using_override_auth && looks_like_claude_cli_credentials(&mission_creds_path);
        if let Some((expires_at, has_refresh)) = claude_cli_credentials_info(&mission_creds_path) {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let is_expired = expires_at < now_ms;
            tracing::info!(
                mission_id = %mission_id,
                path = %mission_creds_path.display(),
                expires_at = expires_at,
                has_refresh = has_refresh,
                has_cli_creds = has_cli_creds,
                is_expired = is_expired,
                "Claude CLI credential status for mission"
            );
            // If credentials are expired even after the copy/refresh attempt,
            // don't trust them — fall through to OAuth injection instead.
            if is_expired {
                tracing::warn!(
                    mission_id = %mission_id,
                    expires_at = expires_at,
                    now_ms = now_ms,
                    "Mission CLI credentials are expired; removing stale file and falling through to OAuth refresh"
                );
                has_cli_creds = false;
                // Remove the stale file so Claude Code doesn't pick it up
                // and fail with "Invalid authentication credentials".
                if let Err(e) = std::fs::remove_file(&mission_creds_path) {
                    tracing::debug!(
                        mission_id = %mission_id,
                        error = %e,
                        "Failed to remove expired credentials file (may not exist)"
                    );
                }
            }
        } else {
            tracing::info!(
                mission_id = %mission_id,
                path = %mission_creds_path.display(),
                has_cli_creds = has_cli_creds,
                "No Claude CLI credentials found for mission"
            );
        }

        let proxy_auth = if !using_override_auth && !has_cli_creds {
            let config = claudecode_cli_proxy_config();
            if let Some(ref proxy) = config {
                tracing::info!(
                    mission_id = %mission_id,
                    base_url = %proxy.base_url,
                    "Using Claude Code via CLI Proxy API fallback"
                );
            }
            config
        } else {
            None
        };

        // Only refresh OpenCode/Anthropic OAuth tokens if we plan to inject them.
        let oauth_refresh_result = if has_cli_creds || proxy_auth.is_some() {
            tracing::info!(
                mission_id = %mission_id,
                has_cli_creds = has_cli_creds,
                using_cli_proxy = proxy_auth.is_some(),
                "Using non-OAuth-refresh Claude Code auth path; skipping OAuth refresh injection"
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
        } else if proxy_auth.is_some() || has_cli_creds {
            // CLI-proxy runs get credentials injected via `proxy_auth` env vars,
            // and CLI credentials come from the mirrored `.credentials.json`.
            // Either way, there's nothing to select here.
            None
        } else {
            // Try to get API key/OAuth token from Anthropic provider configured for Claude Code backend.
            // For container workspaces, compare workspace auth vs host auth and use the fresher one.
            // If workspace auth is expired, try to refresh it using the refresh token.
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
        if api_auth.is_none() && !has_cli_creds && proxy_auth.is_none() {
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
        // This catches DNS/network issues immediately instead of waiting for a timeout.
        // When the CLI proxy is the auth source, skip this probe: it hits
        // `api.anthropic.com` directly, and environments that rely on the CLI
        // proxy may intentionally block direct Anthropic egress.
        if proxy_auth.is_none() {
            if let Err(err_msg) = check_claudecode_connectivity(&workspace_exec, work_dir).await {
                tracing::error!(mission_id = %mission_id, "{}", err_msg);
                return AgentResult::failure(err_msg, 0)
                    .with_terminal_reason(TerminalReason::LlmError);
            }
        } else if let Some(ref proxy) = proxy_auth {
            // The CLI proxy is the auth path; the direct-Anthropic probe above is
            // skipped because proxy environments block direct egress. Probe the
            // proxy instead so a down/hung/erroring proxy fails fast (≈8s) and
            // routes through ResetSessionFresh recovery, instead of silently
            // burning the full startup timeout (90s, ×2 with recovery).
            if let Err(err_msg) = check_claudecode_proxy_health(
                &workspace_exec,
                work_dir,
                &proxy.base_url,
                &proxy.api_key,
            )
            .await
            {
                tracing::warn!(mission_id = %mission_id, "{}", err_msg);
                return AgentResult::failure(err_msg, 0)
                    .with_terminal_reason(TerminalReason::LlmError)
                    .with_data(claudecode_transport_failure_data(
                        ClaudeTransportFailureStage::Startup,
                        false,
                        false,
                        &[],
                    ));
            }
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

        // Claude Code settings and MCP config are loaded via CLAUDE_CONFIG_DIR
        // which points to the per-mission .claude directory. Claude Code auto-discovers
        // settings.local.json and mcp.json from that directory.
        //
        // Note: --settings and --mcp-config flags are NOT used because Claude Code 2.1.77+
        // changed these to expect inline JSON content rather than file paths, causing
        // SyntaxError ("Unexpected token '/'") at startup when a path is passed.
        let settings_path = work_dir.join(".claude").join("settings.local.json");
        if settings_path.exists() {
            match std::fs::read_to_string(&settings_path) {
                Ok(json_content) => {
                    args.push("--settings".to_string());
                    args.push(json_content);
                }
                Err(e) => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        path = %settings_path.display(),
                        error = %e,
                        "Failed to read settings file for --settings flag"
                    );
                }
            }
        }
        let mcp_config_path = work_dir.join(".claude").join("mcp.json");
        if mcp_config_path.exists() {
            match std::fs::read_to_string(&mcp_config_path) {
                Ok(json_content) => {
                    args.push("--mcp-config".to_string());
                    args.push(json_content);
                }
                Err(e) => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        path = %mcp_config_path.display(),
                        error = %e,
                        "Failed to read MCP config file for --mcp-config flag"
                    );
                }
            }
        }

        if let Some(m) = model {
            // Claude Code expects bare model IDs (e.g. "claude-opus-4-7"),
            // not provider-prefixed ones (e.g. "anthropic/claude-opus-4-7").
            let bare = m.strip_prefix("anthropic/").unwrap_or(m);
            args.push("--model".to_string());
            args.push(bare.to_string());
        }

        // Note: model_effort is set via CLAUDE_CODE_EFFORT_LEVEL env var below,
        // not as a CLI flag (Claude Code CLI does not have an --effort flag).

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
        //
        // Additional safety: even when the marker file says the session was initiated,
        // verify that Claude's session data directory actually exists on disk.
        // A stale marker file (e.g., after container restart, HOME wipe, or service
        // restart) combined with --resume causes the CLI to hang silently, triggering
        // the startup timeout. This pre-validation avoids that entirely.
        let session_data_exists = if session_was_initiated {
            // Claude Code stores session data under $CLAUDE_CONFIG_DIR/projects/<hash>/
            // or ~/.claude/projects/<hash>/.  We check the broader `.claude/projects`
            // dir for *any* session data rather than guessing the exact hash, since the
            // hash depends on the absolute cwd path inside the container.
            let claude_projects_dir = work_dir.join(".claude").join("projects");
            let has_projects = claude_projects_dir.exists()
                && std::fs::read_dir(&claude_projects_dir)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
            if !has_projects {
                tracing::warn!(
                    mission_id = %mission_id,
                    session_id = %session_id,
                    projects_dir = %claude_projects_dir.display(),
                    "Session marker exists but no Claude session data found on disk; \
                     skipping --resume to avoid CLI hang"
                );
            }
            has_projects
        } else {
            false
        };

        let use_resume = session_was_initiated && session_data_exists;

        if use_resume {
            args.push("--resume".to_string());
            args.push(session_id.clone());
            tracing::debug!(
                mission_id = %mission_id,
                session_id = %session_id,
                is_continuation = is_continuation,
                session_was_initiated = session_was_initiated,
                session_data_exists = session_data_exists,
                "Resuming existing Claude Code session"
            );
        } else {
            // If the marker was stale (session data missing), remove it so it
            // gets recreated with the current session ID.
            if session_was_initiated && !session_data_exists {
                let _ = std::fs::remove_file(&session_marker);
            }

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

        // Stream-input mode (opt-in): deliver the prompt over stdin as
        // stream-json and keep stdin open so messages can be injected
        // MID-TURN (picked up after the current tool call completes, like
        // typing in the interactive CLI). The positional prompt is ignored
        // by the CLI in this mode, so it is not added.
        let stream_input = crate::util::env_var_bool("SANDBOXED_SH_CLAUDE_STREAM_INPUT", false)
            && !force_argv_prompt;
        if stream_input {
            args.push("--input-format".to_string());
            args.push("stream-json".to_string());
        } else {
            // Provide the prompt as a positional argument (instead of stdin).
            //
            // In production we have observed cases where piping stdin from the backend results in
            // Claude Code producing no stdout events (even though it creates the session files),
            // leaving missions stuck "Agent is working..." indefinitely.
            args.push("--".to_string());
            args.push(effective_message.clone());
        }

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
        // Note: CLAUDE_CONFIG is NOT set. Recent Claude Code versions interpret it
        // as inline JSON (not a file path), causing a SyntaxError at startup.
        // CLAUDE_CONFIG_DIR + --settings flag are sufficient.

        // Set effort level via environment variable.
        // Claude Code reads CLAUDE_CODE_EFFORT_LEVEL to control adaptive reasoning depth.
        if let Some(effort) = model_effort {
            env.insert("CLAUDE_CODE_EFFORT_LEVEL".to_string(), effort.to_string());

            // CLAUDE_CODE_EFFORT_LEVEL only nudges adaptive reasoning, which
            // leaves the Thoughts panel empty on tool-heavy turns. Pin an
            // explicit extended-thinking budget so every turn emits a thinking
            // block we can capture and stream. (The capture pipeline already
            // handles thinking_delta — see backend/shared.rs — the CLI just
            // wasn't emitting any.)
            let thinking_tokens = claude_thinking_budget(effort);
            if thinking_tokens > 0 {
                env.insert(
                    "MAX_THINKING_TOKENS".to_string(),
                    thinking_tokens.to_string(),
                );
            }
            tracing::info!(
                mission_id = %mission_id,
                effort = %effort,
                max_thinking_tokens = thinking_tokens,
                "Setting Claude Code effort level + extended-thinking budget"
            );
        }

        // Trigger auto-compaction at 80% context capacity to prevent "Prompt is too long"
        // errors on long-running missions. Claude Code's default (95%) is too aggressive
        // and can fail to compact in time, permanently locking the session.
        env.insert(
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE".to_string(),
            "80".to_string(),
        );

        // Prevent CLI tools from hanging in our PTY environment.
        //
        // The `gh` CLI's terminal renderer (lipgloss/glamour) sends escape sequences
        // like `\033]11;?` (background color query) and `\033[6n` (cursor position)
        // when it detects a TTY. Our PTY has no terminal emulator to respond, so
        // these queries block forever. This specifically affects tabular commands
        // like `gh issue list` and `gh pr list`.
        //
        // GH_NO_PAGER=1  — disables paging (prevents `less` from activating)
        // NO_COLOR=1     — disables color and terminal capability queries
        // GH_PROMPT_DISABLED=1 — disables interactive prompts
        env.insert("GH_NO_PAGER".to_string(), "1".to_string());
        env.insert("NO_COLOR".to_string(), "1".to_string());
        env.insert("GH_PROMPT_DISABLED".to_string(), "1".to_string());

        if let Some(ref proxy) = proxy_auth {
            env.insert("ANTHROPIC_BASE_URL".to_string(), proxy.base_url.clone());
            env.insert("ANTHROPIC_API_KEY".to_string(), proxy.api_key.clone());
            tracing::info!(
                mission_id = %mission_id,
                base_url = %proxy.base_url,
                "Injecting Claude Code CLI Proxy API environment"
            );
        } else if let Some(ref auth) = api_auth {
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

        // DGX Spark build offload (opt-in per workspace): expose the local host
        // endpoint + token + workspace paths so the in-workspace `spark-build`
        // wrapper can ship Lean builds to the Spark. Credentials stay on the host.
        if let Some(spark_vars) = workspace.spark_offload_env(mission_id) {
            env.extend(spark_vars);
        }

        // Inject Telegram action environment variables when processing a Telegram message.
        // These are needed by the telegram-action CLI helper inside the container to schedule
        // reminders, send replies, etc.
        let telegram_action_helpers_enabled =
            message.contains("[Telegram from ") || message.contains("[Telegram workflow reply ");
        if telegram_action_helpers_enabled {
            write_telegram_action_cli_helpers(work_dir);

            env.insert("MISSION_ID".to_string(), mission_id.to_string());

            if let Some(token) =
                crate::api::telegram::build_internal_telegram_action_token(mission_id)
            {
                env.insert("TELEGRAM_ACTION_TOKEN".to_string(), token);
            }

            // Use the internal host address only — never fall back to a public
            // URL for internal action endpoints (they use HMAC tokens, not
            // bearer auth). Workspace-aware: private-network containers reach
            // the host via the veth gateway, not 127.0.0.1.
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

            let container_work_dir = workspace_exec.translate_path_for_container(work_dir);
            env.insert(
                "TELEGRAM_ACTION_CLI".to_string(),
                format!("{}/.sandboxed-sh-telegram-action.py", container_work_dir),
            );
            env.insert(
                "TELEGRAM_ACTION_COMMAND".to_string(),
                format!("{}/telegram-action", container_work_dir),
            );

            // Append a dedicated bin subdirectory (not the workspace root) to
            // PATH so that `telegram-action` is findable as a bare command
            // without letting arbitrary repo files shadow system binaries.
            {
                let current_path = env
                    .get("PATH")
                    .cloned()
                    .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
                env.insert(
                    "PATH".to_string(),
                    format!("{}:{}/.sandboxed-sh-bin", current_path, container_work_dir),
                );
            }

            tracing::info!(
                mission_id = %mission_id,
                "Telegram action env vars injected for Claude Code backend"
            );
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
        // agent modes to hang. In argv mode stdin is unused but must stay open;
        // in stream-input mode it carries the prompt and mid-turn injections.
        let mut stdin_writer = pty.take_writer().ok();
        if stream_input {
            #[cfg(unix)]
            if let Err(e) = pty.set_raw_input_mode() {
                // Without raw mode, canonical-mode line buffering truncates
                // stdin writes >4096 bytes and echo reflects injected JSON
                // back into the parsed stdout stream — stream-input cannot
                // work. Fail the turn loudly; the operator can unset
                // SANDBOXED_SH_CLAUDE_STREAM_INPUT to fall back to argv mode.
                tracing::error!(mission_id = %mission_id, "Failed to set PTY raw input mode: {e}");
                pty.kill();
                return AgentResult::failure(
                    format!(
                        "Stream-input mode requires PTY raw input mode, which failed: {e}.                          Unset SANDBOXED_SH_CLAUDE_STREAM_INPUT to use argv prompt delivery."
                    ),
                    0,
                )
                .with_terminal_reason(TerminalReason::LlmError);
            }
            let mut initial_prompt_delivered = false;
            if let Some(w) = stdin_writer.as_mut() {
                let init = serde_json::json!({
                    "type": "user",
                    "message": { "role": "user", "content": [{ "type": "text", "text": effective_message }] }
                });
                use std::io::Write as _;
                match writeln!(w, "{}", init).and_then(|_| w.flush()) {
                    Ok(()) => initial_prompt_delivered = true,
                    Err(e) => tracing::error!(
                        mission_id = %mission_id,
                        "Failed to write initial stream-json prompt: {e}"
                    ),
                }
            }
            if !initial_prompt_delivered {
                // Without the prompt the CLI would idle on an empty session —
                // a silent no-op turn. Fail loudly instead.
                pty.kill();
                return AgentResult::failure(
                    "Stream-input mode could not deliver the initial prompt over stdin".to_string(),
                    0,
                )
                .with_terminal_reason(TerminalReason::LlmError);
            }
        }
        // Poll cadence for mid-turn operator-note injection (stream-input mode).
        let mut last_note_poll = Instant::now();
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
        let mut malformed_json_output: Vec<String> = Vec::new();

        // Track tool calls for result mapping
        let mut pending_tools: HashMap<String, String> = HashMap::new();
        // Track Claude Code's built-in ScheduleWakeup calls so we can convert
        // a successful tool result into an open_agent wakeup automation.
        // Maps tool_use_id -> (delay_seconds, prompt, reason).
        let mut pending_wakeups: HashMap<String, (u64, String, String)> = HashMap::new();
        let mut total_cost_usd: Option<f64> = None;
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_cache_creation_tokens: u64 = 0;
        let mut total_cache_read_tokens: u64 = 0;
        let mut observed_model: Option<String> = None;
        let mut final_result = String::new();
        let mut had_error = false;
        let mut saw_terminal_result_event = false;
        let mut process_exited_without_result = false;
        let mut idle_timeout_triggered = false;
        let mut transport_failure_stage: Option<ClaudeTransportFailureStage> = None;
        // Cancellation breaks out of the loop instead of returning immediately,
        // so the post-loop fallback (final_result ← text_buffer ← thinking_buffer)
        // can surface whatever the agent already produced. See run_codex_turn.
        let mut cancelled = false;

        // Track content block types and accumulated content for Claude Code streaming
        // This is needed because Claude sends incremental deltas that need to be accumulated
        let mut block_types: HashMap<u32, String> = HashMap::new();
        let mut thinking_buffer: HashMap<u32, String> = HashMap::new();
        // Per-turn audit: catches thinking blocks whose deltas we can't
        // decode (OAuth-encrypted / signature-only) so the turn end can
        // surface a marker instead of a silently empty thoughts panel.
        let mut thinking_audit = crate::backend::shared::ThinkingDeltaAudit::default();
        let mut encrypted_marker_emitted = false;
        let mut text_buffer: HashMap<u32, String> = HashMap::new();
        let mut active_thinking_index: Option<u32> = None; // Track which thinking block is active
        let mut finalized_thinking_indices: std::collections::HashSet<u32> =
            std::collections::HashSet::new(); // Blocks already sent done:true during streaming
        let mut last_text_len: usize = 0; // Track last emitted text length for streaming text deltas
                                          // Degenerate-stream detector state. When the model loops on the same
                                          // substring for a long time (e.g. emitting "Yielding pending your
                                          // choice." or "..." indefinitely) we want to cut the turn off
                                          // ourselves rather than wait for the model to hit max_tokens or
                                          // surface an unhelpful "Yielding." final answer. Tunables are env
                                          // overrides so production can tighten/loosen without a code change.
        let degenerate_min_duration = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_DEGENERATE_MIN_DURATION_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(90),
        );
        let degenerate_min_repeats: usize =
            std::env::var("SANDBOXED_SH_CLAUDECODE_DEGENERATE_MIN_REPEATS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(3);
        let degenerate_min_substring_len: usize =
            std::env::var("SANDBOXED_SH_CLAUDECODE_DEGENERATE_MIN_SUBSTRING_LEN")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(40);
        let degenerate_window_chars: usize =
            std::env::var("SANDBOXED_SH_CLAUDECODE_DEGENERATE_WINDOW_CHARS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(4096);
        let mut first_text_delta_at: Option<Instant> = None;
        let mut degenerate_stage_triggered: bool = false;

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
        let tool_idle_timeout = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_TOOL_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1800),
        );
        let post_tool_result_idle_timeout = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_POST_TOOL_RESULT_IDLE_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30),
        );
        // Heartbeat interval used to signal liveness to the actor-level
        // stuck-mission watchdog. During extended thinking (notably with
        // model_effort=max), Claude CLI can emit only scaffolding stream
        // events (message_start, content_block_start, signature_delta…)
        // for many minutes without any thinking_delta. Those reset the
        // per-turn PTY idle timer but never become broadcast events, so
        // the actor's main_runner_last_activity never updates and the
        // 900s stuck-mission watchdog cancels the mission mid-turn.
        let heartbeat_interval = Duration::from_secs(
            std::env::var("SANDBOXED_SH_CLAUDECODE_HEARTBEAT_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(300),
        );
        let mut last_heartbeat_at = Instant::now();
        let startup_deadline = Instant::now() + startup_timeout;
        let mut turn_wait_state = ClaudeTurnWaitState::Startup;
        let mut tool_timeout_override: Option<tokio::time::Instant> = None;
        let mut idle_deadline = claudecode_idle_deadline(
            turn_wait_state,
            Instant::now(),
            idle_timeout,
            tool_idle_timeout,
            post_tool_result_idle_timeout,
            tool_timeout_override,
        );

        // Monitor child process exit. When Claude Code exits mid-tool-execution
        // (e.g. while `gh` is still running), child processes can keep the PTY
        // slave fd open, preventing the PTY reader from getting EOF. We detect
        // the main process exit and break the loop with a grace period.
        let process_exit_notify = {
            let notify = Arc::new(tokio::sync::Notify::new());
            if let Some(pid) = pty.process_id() {
                let notify_clone = Arc::clone(&notify);
                let exit_mission_id = mission_id.to_string();
                tokio::task::spawn_blocking(move || {
                    let pid = pid as i32;
                    loop {
                        // kill(pid, 0) checks if the process exists without
                        // actually sending a signal.
                        let alive = unsafe { libc::kill(pid, 0) } == 0;
                        if !alive {
                            tracing::debug!(
                                mission_id = %exit_mission_id,
                                pid = pid,
                                "PTY child process has exited"
                            );
                            notify_clone.notify_one();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });
            }
            notify
        };
        let mut process_exited = false;
        // Grace period: after process exits, wait briefly for remaining events
        // before breaking the loop. This lets us capture any final `result` event
        // that may already be buffered in the PTY/channel.
        let mut process_exit_grace_deadline: Option<Instant> = None;
        // Process events until completion or cancellation
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!(mission_id = %mission_id, "Claude Code execution cancelled, killing process");
                    // Kill the process to stop consuming API resources
                    pty.kill();
                    reader_handle.abort();
                    cancelled = true;
                    break;
                }
                _ = tokio::time::sleep_until(startup_deadline), if !saw_non_init_event => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        use_resume = use_resume,
                        non_json_lines = non_json_output.len(),
                        malformed_json_lines = malformed_json_output.len(),
                        non_json_sample = ?non_json_output.first(),
                        malformed_json_sample = ?malformed_json_output.first(),
                        cli_program = %program,
                        cli_args_count = full_args.len(),
                        "Claude Code startup timeout - no stream events received"
                    );
                    pty.kill();
                    reader_handle.abort();
                    let mut msg = if !malformed_json_output.is_empty() {
                        claudecode_malformed_startup_message(
                            &malformed_json_output,
                            use_resume,
                            &session_id,
                        )
                    } else {
                        let mut msg = "Claude Code produced no stream events after startup timeout. The Claude CLI started but did not emit any stream-json events.".to_string();
                        msg.push_str("\n\nThis can happen when resuming an old/stuck Claude session or when the CLI hangs during initialization.");
                        msg.push_str(&format!("\n\nDiagnostics: use_resume={}, session_id={}", use_resume, session_id));
                        msg
                    };
                    if !non_json_output.is_empty() {
                        msg.push_str(&format!(
                            "\n\nNon-JSON output captured ({} lines):\n{}",
                            non_json_output.len(),
                            non_json_output.join("\n")
                        ));
                    }
                    return AgentResult::failure(msg, 0)
                        .with_terminal_reason(TerminalReason::LlmError)
                        .with_data(claudecode_transport_failure_data(
                            ClaudeTransportFailureStage::Startup,
                            false,
                            false,
                            &[],
                        ));
                }
                _ = tokio::time::sleep_until(idle_deadline), if saw_non_init_event => {
                    tracing::warn!(
                        mission_id = %mission_id,
                        wait_state = ?turn_wait_state,
                        pending_tool_count = pending_tools.len(),
                        had_partial_output = !final_result.trim().is_empty() || !text_buffer.is_empty(),
                        "Claude Code idle timeout after activity; treating turn as incomplete"
                    );
                    pty.kill();
                    reader_handle.abort();
                    idle_timeout_triggered = true;
                    break;
                }
                _ = process_exit_notify.notified(), if !process_exited => {
                    // The main PTY child (nsenter/claude) has exited.
                    // Give a short grace period to drain any buffered events
                    // (the `result` event may already be in the channel).
                    process_exited = true;
                    process_exit_grace_deadline = Some(Instant::now() + Duration::from_secs(3));
                    tracing::info!(
                        mission_id = %mission_id,
                        "PTY child process exited, draining remaining events (3s grace)"
                    );
                }
                _ = tokio::time::sleep_until(process_exit_grace_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(86400))), if process_exited => {
                    // Grace period expired after process exit — no `result` event arrived.
                    tracing::warn!(
                        mission_id = %mission_id,
                        "Claude Code process exited without emitting a result event, breaking event loop"
                    );
                    process_exited_without_result = true;
                    // Kill any orphaned child processes still holding the PTY open
                    pty.kill();
                    reader_handle.abort();
                    break;
                }
                // Timer-based liveness heartbeat, gated to AwaitingToolResults.
                //
                // While a foreground tool runs (notably a long build), the CLI
                // emits no stream events for minutes, so the event-gated
                // heartbeat below never fires. The actor-level stuck-mission
                // watchdog (control.rs, 900s) keys off broadcast events and
                // would cancel the mission mid-tool — even though the turn-level
                // `tool_idle_timeout` (much larger) is the correct arbiter for a
                // running tool. Emitting a heartbeat on a timer here makes the
                // coarse watchdog defer to `tool_idle_timeout`.
                //
                // Strictly gated to AwaitingToolResults so it does NOT mask a
                // genuine hang: a fire-and-forget background job that returns
                // immediately leaves the turn in AwaitingTerminalResult/
                // AwaitingClaude (not this state), so those stalls remain subject
                // to the watchdog as before.
                _ = tokio::time::sleep_until(last_note_poll + crate::api::runners::midturn::MID_TURN_POLL), if stream_input && stdin_writer.is_some() => {
                    last_note_poll = Instant::now();
                    crate::api::runners::midturn::drain_and_inject(
                        mission_id,
                        &events_tx,
                        |block| {
                                // Claude delivery is synchronous: a successful
                                // blocking write to the live stream-json stdin
                                // means the frame is handed to the CLI, so the
                                // returned `bool` is the true outcome.
                                let msg = serde_json::json!({
                                    "type": "user",
                                    "message": { "role": "user", "content": [{ "type": "text", "text": block }] }
                                });
                                let delivered = if let Some(w) = stdin_writer.as_mut() {
                                    use std::io::Write as _;
                                    let ok = writeln!(w, "{}", msg).and_then(|_| w.flush()).is_ok();
                                    if ok {
                                        tracing::info!(
                                            mission_id = %mission_id,
                                            "Injected operator notes mid-turn via stream-json stdin"
                                        );
                                    }
                                    ok
                                } else {
                                    false
                                };
                                std::future::ready(delivered)
                        }
                    ).await;
                }
                _ = tokio::time::sleep_until(last_heartbeat_at + heartbeat_interval),
                    if saw_non_init_event
                        && matches!(turn_wait_state, ClaudeTurnWaitState::AwaitingToolResults) => {
                    let _ = events_tx.send(AgentEvent::MissionActivity {
                        label: "Tool running…".to_string(),
                        tool_name: "claudecode_heartbeat".to_string(),
                        mission_id: Some(mission_id),
                    });
                    last_heartbeat_at = Instant::now();
                }
                line_opt = line_rx.recv() => {
                    let Some(raw_line) = line_opt else {
                        // EOF - PTY closed
                        break;
                    };

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
                            if malformed_json_output.len() < 20 {
                                let excerpt = if line.len() > 200 {
                                    let end = safe_truncate_index(line, 200);
                                    format!("{}...", &line[..end])
                                } else {
                                    line.to_string()
                                };
                                malformed_json_output
                                    .push(format!("Parse error: {} | line: {}", e, excerpt));
                            }
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
                        if matches!(turn_wait_state, ClaudeTurnWaitState::Startup) {
                            turn_wait_state = ClaudeTurnWaitState::AwaitingClaude;
                        }
                    }

                            match claude_event {
                                ClaudeEvent::System(sys) => {
                                    if let Some(m) = sys.model {
                                        observed_model = Some(m);
                                    }
                                    tracing::debug!(
                                        "Claude session init: session_id={}, model={:?}",
                                        sys.session_id, observed_model
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
                                                                // The finalizer must carry the full block content:
                                                                // it is the only thinking event that survives into
                                                                // persisted history.
                                                                let _ = events_tx.send(thinking_final_event(
                                                                    thinking_buffer
                                                                        .get(&prev_idx)
                                                                        .cloned()
                                                                        .unwrap_or_default(),
                                                                    mission_id,
                                                                ));
                                                                finalized_thinking_indices.insert(prev_idx);
                                                            }
                                                        }
                                                        active_thinking_index = Some(index);

                                                        // Accumulate thinking content per block. Most Claude events are
                                                        // incremental deltas, but using the merge helper also handles
                                                        // CLI versions that resend a cumulative snapshot.
                                                        let buffer = thinking_buffer.entry(index).or_default();
                                                        merge_stream_fragment(buffer, &thinking_content);

                                                        // Send this block's accumulated content
                                                        thinking_audit.note_emitted_thinking();
                                                        let _ = events_tx.send(AgentEvent::Thinking {
                                                            content: buffer.clone(),
                                                            done: false,
                                                            mission_id: Some(mission_id),
                                                        });
                                                    }
                                                }
                                            } else if delta.delta_type == "text_delta" {
                                                // For text deltas, content is in the `text` field
                                                if let Some(text) = delta.text {
                                                    if !text.is_empty() {
                                                        // Accumulate text content (will be used for final response).
                                                        // This accepts both incremental chunks and snapshot-style
                                                        // replacements so streamed text never doubles words if a CLI
                                                        // changes semantics.
                                                        let buffer = text_buffer.entry(index).or_default();
                                                        merge_stream_fragment(buffer, &text);

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

                                                        // Degenerate-stream detector. Some models enter a
                                                        // tight loop emitting the same short string over
                                                        // and over (e.g. "Yielding pending your choice.")
                                                        // and never emit a terminal result. The per-turn
                                                        // idle timer never fires because events keep
                                                        // arriving, so the user is stuck watching a
                                                        // streaming view that never finalises and is
                                                        // billed for the full token burn. Once we see the
                                                        // same meaningful substring repeated several
                                                        // times in a sliding window past a minimum
                                                        // streaming duration we kill the CLI, surface a
                                                        // clear "model entered a degenerate loop"
                                                        // failure, and let the user send a new turn.
                                                        if !degenerate_stage_triggered {
                                                            if first_text_delta_at.is_none() {
                                                                first_text_delta_at = Some(Instant::now());
                                                            }
                                                            let streaming_for = first_text_delta_at
                                                                .map(|t| t.elapsed())
                                                                .unwrap_or(Duration::ZERO);
                                                            let total_acc: String = text_buffer
                                                                .values()
                                                                .cloned()
                                                                .collect::<Vec<_>>()
                                                                .join("");
                                                            if streaming_for >= degenerate_min_duration
                                                                && text_buffer_stream_looks_degenerate(
                                                                    &total_acc,
                                                                    degenerate_window_chars,
                                                                    degenerate_min_substring_len,
                                                                    degenerate_min_repeats,
                                                                )
                                                            {
                                                                tracing::warn!(
                                                                    mission_id = %mission_id,
                                                                    streaming_for_secs = streaming_for.as_secs(),
                                                                    total_text_chars = total_len,
                                                                    window_chars = degenerate_window_chars,
                                                                    min_substring_len = degenerate_min_substring_len,
                                                                    min_repeats = degenerate_min_repeats,
                                                                    "Claude Code stream looks degenerate (same substring repeated); killing CLI"
                                                                );
                                                                degenerate_stage_triggered = true;
                                                                pty.kill();
                                                                reader_handle.abort();
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            else if delta.delta_type != "input_json_delta" {
                                                // Unknown delta type (e.g. signature_delta for
                                                // encrypted thinking) — record it so the turn end
                                                // can warn and surface a marker.
                                                thinking_audit
                                                    .note_undecoded_delta(&delta.delta_type);
                                            }
                                        }
                                        StreamEvent::ContentBlockStart { index, content_block }
                                            if content_block.block_type == "tool_use" =>
                                        {
                                            // Track the block type so we know how to handle deltas
                                            block_types.insert(index, content_block.block_type.clone());

                                            if let (Some(id), Some(name)) =
                                                (content_block.id, content_block.name)
                                            {
                                                pending_tools.insert(id, name);
                                                turn_wait_state =
                                                    ClaudeTurnWaitState::AwaitingToolResults;
                                            }
                                        }
                                        StreamEvent::ContentBlockStart { index, content_block } => {
                                            thinking_audit
                                                .note_block_start(&content_block.block_type);
                                            block_types.insert(index, content_block.block_type);
                                        }
                                        _ => {}
                                    }
                                }
                                ClaudeEvent::Assistant(evt) => {
                                    if let Some(m) = evt.message.model.as_ref() {
                                        observed_model = Some(m.clone());
                                    }
                                    if let Some(usage) = &evt.message.usage {
                                        total_input_tokens += usage.input_tokens.unwrap_or(0);
                                        total_output_tokens += usage.output_tokens.unwrap_or(0);
                                        total_cache_creation_tokens +=
                                            usage.cache_creation_input_tokens.unwrap_or(0);
                                        total_cache_read_tokens +=
                                            usage.cache_read_input_tokens.unwrap_or(0);
                                    }
                                    let mut assistant_thinking_fallback = String::new();
                                    let mut assistant_emitted_thinking_final = false;
                                    for (content_idx, block) in evt.message.content.into_iter().enumerate() {
                                        let content_idx = content_idx as u32;
                                        match block {
                                            ContentBlock::Text { text } if !text.is_empty() => {
                                                // Text content is the final assistant response.
                                                // Thinking must come from explicit provider
                                                // reasoning/thinking blocks, not answer text.
                                                final_result = text;
                                            }
                                            ContentBlock::ToolUse { id, name, input } => {
                                                pending_tools.insert(id.clone(), name.clone());
                                                turn_wait_state = ClaudeTurnWaitState::AwaitingToolResults;
                                                let _ = events_tx.send(AgentEvent::ToolCall {
                                                    tool_call_id: id.clone(),
                                                    name: name.clone(),
                                                    args: input.clone(),
                                                    mission_id: Some(mission_id),
                                                });

                                                // Capture args from Claude Code's built-in
                                                // ScheduleWakeup so the matching ToolResult
                                                // can turn it into a real wakeup automation.
                                                if name == "ScheduleWakeup" {
                                                    let delay = input
                                                        .get("delaySeconds")
                                                        .or_else(|| input.get("delay_seconds"))
                                                        .and_then(|v| v.as_u64());
                                                    let prompt = input
                                                        .get("prompt")
                                                        .and_then(|v| v.as_str())
                                                        .map(|s| s.to_string());
                                                    let reason = input
                                                        .get("reason")
                                                        .and_then(|v| v.as_str())
                                                        .map(|s| s.to_string())
                                                        .unwrap_or_default();
                                                    match (delay, prompt) {
                                                        (Some(d), Some(p)) => {
                                                            pending_wakeups
                                                                .insert(id.clone(), (d, p, reason));
                                                        }
                                                        _ => {
                                                            tracing::warn!(
                                                                mission_id = %mission_id,
                                                                tool_use_id = %id,
                                                                "Claude built-in ScheduleWakeup tool call missing delaySeconds or prompt; skipping wakeup automation"
                                                            );
                                                        }
                                                    }
                                                }

                                                // Extend idle timeout when tool has its own timeout.
                                                // Long-running commands (e.g. `lake build` with timeout: 600000ms)
                                                // produce no PTY output while waiting, so our default idle
                                                // timeout would kill the process prematurely.
                                                if let Some(tool_timeout_ms) = input.get("timeout").and_then(|v| v.as_u64()) {
                                                    let tool_timeout = Duration::from_millis(tool_timeout_ms);
                                                    // Add a buffer beyond the tool's own timeout
                                                    let extended = tool_timeout + Duration::from_secs(30);
                                                    let new_deadline = Instant::now() + extended;
                                                    let should_extend = tool_timeout_override
                                                        .map(|current| new_deadline > current)
                                                        .unwrap_or(true);
                                                    if should_extend {
                                                        tracing::info!(
                                                            mission_id = %mission_id,
                                                            tool_name = %name,
                                                            tool_timeout_secs = tool_timeout_ms / 1000,
                                                            "Extending idle timeout for long-running tool call"
                                                        );
                                                        tool_timeout_override = Some(new_deadline);
                                                    }
                                                }

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
                                                        // Mark the mission as waiting on the user so the
                                                        // stuck-mission watchdog does not interrupt it for
                                                        // "inactivity" while it is parked here. The guard
                                                        // clears the mark on every exit path (answered or
                                                        // cancelled).
                                                        let wait_guard =
                                                            FrontendToolHub::begin_waiting(&hub, mission_id);
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
                                                        // Answer received — the mission is active again and
                                                        // resumes emitting events, so release the watchdog
                                                        // exemption before running the continuation turn.
                                                        drop(wait_guard);

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
                                                            model_effort,
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
                                                            force_argv_prompt,
                                                        ).await;
                                                    }
                                                }
                                            }
                                            ContentBlock::Thinking { thinking }
                                                if !thinking.is_empty()
                                                    && !finalized_thinking_indices
                                                        .contains(&content_idx) =>
                                            {
                                                if !assistant_thinking_fallback.is_empty() {
                                                    assistant_thinking_fallback.push('\n');
                                                }
                                                assistant_thinking_fallback.push_str(&thinking);
                                                // Only send done:true for the last active thinking block.
                                                // Earlier blocks were already finalized during streaming
                                                // (via the block-transition mechanism) and re-sending them
                                                // causes duplicate items in the frontend thinking panel.
                                                assistant_emitted_thinking_final = true;
                                                thinking_audit.note_emitted_thinking();
                                                let _ = events_tx
                                                    .send(thinking_final_event(thinking, mission_id));
                                            }
                                            _ => {}
                                        }
                                    }
                                    // If the Assistant event's ContentBlock::Text didn't
                                    // populate final_result, fall back to the accumulated
                                    // text_buffer from streaming deltas (text_delta events).
                                    if final_result.trim().is_empty() && !text_buffer.is_empty() && pending_tools.is_empty() {
                                        let mut sorted: Vec<_> = text_buffer.iter().collect();
                                        sorted.sort_by_key(|(idx, _)| *idx);
                                        final_result = sorted.into_iter().map(|(_, t)| t.clone()).collect::<Vec<_>>().join("");
                                        tracing::info!(
                                            mission_id = %mission_id,
                                            "Using text delta buffer as final result ({} chars, ContentBlock::Text was empty)",
                                            final_result.len()
                                        );
                                    }
                                    // If still empty, try thinking buffer
                                    if final_result.trim().is_empty() && !thinking_buffer.is_empty() && pending_tools.is_empty() {
                                        let mut sorted: Vec<_> = thinking_buffer.iter().collect();
                                        sorted.sort_by_key(|(idx, _)| *idx);
                                        final_result = sorted.into_iter().map(|(_, t)| t.clone()).collect::<Vec<_>>().join("");
                                        tracing::info!(
                                            mission_id = %mission_id,
                                            "Using thinking buffer as final result ({} chars, no text content in this turn)",
                                            final_result.len()
                                        );
                                    }
                                    if use_thinking_only_fallback(
                                        &mut final_result,
                                        &assistant_thinking_fallback,
                                        pending_tools.is_empty(),
                                    ) {
                                        tracing::info!(
                                            mission_id = %mission_id,
                                            "Using assistant thinking-only block as final result ({} chars, no text content in this turn)",
                                            final_result.len()
                                        );
                                    }
                                    // The last streaming thinking block has no block-transition
                                    // finalizer, and OAuth/encrypted turns may carry no
                                    // ContentBlock::Thinking in the Assistant event. Finalize it
                                    // here from the streamed buffer so the block-final event
                                    // (the only persisted one) is never lost.
                                    if !assistant_emitted_thinking_final {
                                        if let Some(idx) = active_thinking_index {
                                            if !finalized_thinking_indices.contains(&idx) {
                                                if let Some(buffer) = thinking_buffer.get(&idx) {
                                                    if !buffer.trim().is_empty() {
                                                        let _ = events_tx.send(thinking_final_event(
                                                            buffer.clone(),
                                                            mission_id,
                                                        ));
                                                        finalized_thinking_indices.insert(idx);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    // Encrypted/undecoded thinking: if a thinking block was
                                    // opened but produced no decodable content this turn,
                                    // surface a marker so the panel isn't silently empty
                                    // (also warns once about unknown delta types).
                                    if let Some(marker) = thinking_audit.finish_turn() {
                                        // Coalesce: Fable/OAuth missions emit a signature-only
                                        // thinking block on EVERY assistant round, which used to
                                        // render dozens of identical "[encrypted thinking]"
                                        // bubbles per turn (47 in one observed mission). One
                                        // marker per CLI run says everything the panel needs.
                                        if !encrypted_marker_emitted {
                                            encrypted_marker_emitted = true;
                                            let _ = events_tx
                                                .send(thinking_final_event(marker, mission_id));
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
                                }
                                ClaudeEvent::User(evt) => {
                                    for block in evt.message.content {
                                        if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                                            // Get tool name and remove from pending (tool is now complete)
                                            let name = pending_tools
                                                .remove(&tool_use_id)
                                                .unwrap_or_else(|| "unknown".to_string());
                                            if pending_tools.is_empty() {
                                                turn_wait_state =
                                                    ClaudeTurnWaitState::AwaitingTerminalResult;
                                                tool_timeout_override = None;
                                                tracing::debug!(
                                                    mission_id = %mission_id,
                                                    "All observed Claude tool results completed; waiting for terminal result"
                                                );
                                            }

                                            // Convert a successful Claude built-in
                                            // ScheduleWakeup into an open_agent wakeup
                                            // automation. Claude Code's CLI handles the
                                            // tool locally and emits a confirmation result
                                            // but no further re-invocation happens in
                                            // --print mode — we have to schedule it.
                                            if let Some((delay, prompt, reason)) =
                                                pending_wakeups.remove(&tool_use_id)
                                            {
                                                if !is_error {
                                                    spawn_claude_builtin_wakeup_automation(
                                                        mission_id, delay, prompt, reason,
                                                    );
                                                } else {
                                                    tracing::warn!(
                                                        mission_id = %mission_id,
                                                        tool_use_id = %tool_use_id,
                                                        "Claude built-in ScheduleWakeup result was an error; skipping wakeup automation"
                                                    );
                                                }
                                            }

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
                                    saw_terminal_result_event = true;
                                    if let Some(cost) = res.total_cost_usd {
                                        total_cost_usd = Some(cost);
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
                                    } else {
                                        apply_terminal_result_text(&mut final_result, res.result);
                                    }
                                    tracing::info!(
                                        mission_id = %mission_id,
                                        cost_usd = total_cost_usd.unwrap_or(0.0),
                                        "Claude Code execution completed"
                                    );
                                    break;
                                }
                                ClaudeEvent::Unknown => {
                                    // Forward-compatibility: unknown event types from
                                    // newer CLI versions are silently ignored.
                                    tracing::trace!(
                                        mission_id = %mission_id,
                                        "Ignoring unknown Claude event type"
                                    );
                                }
                            }
                    idle_deadline = claudecode_idle_deadline(
                        turn_wait_state,
                        Instant::now(),
                        idle_timeout,
                        tool_idle_timeout,
                        post_tool_result_idle_timeout,
                        tool_timeout_override,
                    );
                    // Emit a throttled liveness heartbeat so the stuck-mission
                    // watchdog (control.rs:stuck_mission_watchdog_loop) does not
                    // cancel us while Claude is producing CLI scaffolding events
                    // that don't translate to broadcast events (e.g. extended
                    // thinking without thinking_delta).
                    if last_heartbeat_at.elapsed() >= heartbeat_interval {
                        let label = match turn_wait_state {
                            ClaudeTurnWaitState::Startup => "Claude Code starting…",
                            ClaudeTurnWaitState::AwaitingClaude => "Claude is responding…",
                            ClaudeTurnWaitState::AwaitingToolResults => "Awaiting tool results…",
                            ClaudeTurnWaitState::AwaitingTerminalResult => "Claude is thinking…",
                        };
                        let _ = events_tx.send(AgentEvent::MissionActivity {
                            label: label.to_string(),
                            tool_name: "claudecode_heartbeat".to_string(),
                            mission_id: Some(mission_id),
                        });
                        last_heartbeat_at = Instant::now();
                    }
                }
            }
        }

        // Wait for child process to finish and clean up.
        tracing::debug!(
            mission_id = %mission_id,
            "Event loop completed, waiting for Claude Code process"
        );
        // The final result has already been parsed at this point — the only
        // thing left is process teardown. The CLI can fail to exit when a
        // spawned MCP server (or any child) keeps running and holds the PTY
        // open; an unbounded wait here loses the completed turn and stalls
        // the mission until the 900s supervision watchdog force-aborts it
        // (observed on orchestrator missions 832725c5/5daaa900). Bound the
        // wait and kill the leftover process tree on expiry.
        // In stream-input mode the CLI waits for more stdin after the result;
        // close it so the process exits instead of hitting the kill grace.
        // In argv mode the writer must stay open through the wait — closing
        // stdin early can change CLI exit behavior (see spawn-site comment).
        if stream_input {
            drop(stdin_writer.take());
        }
        const CLI_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
        let child_pid = pty.process_id();
        let mut wait_handle = tokio::task::spawn_blocking(move || {
            let mut pty = pty;
            pty.wait()
        });
        let exit_status = match tokio::time::timeout(CLI_EXIT_GRACE, &mut wait_handle).await {
            Ok(joined) => joined,
            Err(_) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    grace_secs = CLI_EXIT_GRACE.as_secs(),
                    "Claude CLI did not exit after final result; killing leftover process tree"
                );
                #[cfg(unix)]
                if let Some(pid) = child_pid {
                    // The CLI is the session leader on its PTY, so its pgid
                    // matches its pid — killpg takes down lingering MCP
                    // children too. Plain kill as a fallback.
                    unsafe {
                        libc::killpg(pid as i32, libc::SIGKILL);
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
                wait_handle.await
            }
        };
        tracing::debug!(
            mission_id = %mission_id,
            exit_status = ?exit_status,
            "Claude Code process exited"
        );

        // Ensure the PTY reader task stops (it should naturally end after
        // process exit). Bounded: an orphaned child holding the PTY slave
        // open keeps the blocking read alive indefinitely.
        let mut reader_handle = reader_handle;
        if tokio::time::timeout(std::time::Duration::from_secs(10), &mut reader_handle)
            .await
            .is_err()
        {
            tracing::warn!(
                mission_id = %mission_id,
                "PTY reader did not stop after process exit; abandoning it"
            );
            reader_handle.abort();
        }

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
        let actual_cost_cents = actual_cost_cents_from_total_cost_usd(total_cost_usd);
        let model_for_cost = preferred_model_for_cost(model, observed_model.as_deref());
        let (cost_cents, cost_source) =
            resolve_cost_cents_and_source(actual_cost_cents, model_for_cost, &usage);

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

        // If still no final result, fall back to thinking buffer.
        // This handles cases where the model's entire response is in extended thinking
        // (no text content block), e.g. when the answer is generated as thinking content.
        if final_result.trim().is_empty() && !thinking_buffer.is_empty() {
            let mut sorted_entries: Vec<_> = thinking_buffer.iter().collect();
            sorted_entries.sort_by_key(|(idx, _)| *idx);
            final_result = sorted_entries
                .into_iter()
                .map(|(_, text)| text.clone())
                .collect::<Vec<_>>()
                .join("");
            tracing::info!(
                mission_id = %mission_id,
                "Using accumulated thinking buffer as final result ({} chars, no text content was produced)",
                final_result.len()
            );
        }

        // Cancellation suppresses the "no terminal result" / "no output"
        // failure-message construction below: those messages describe a
        // broken Claude Code transport, but a user/system cancel is not a
        // transport failure. We want the accumulated text/thinking buffers
        // (or, as a last resort, the synthetic cancel string) to surface.
        if !cancelled && !had_error && !saw_terminal_result_event {
            had_error = true;
            let exit_summary = describe_pty_exit_status(&exit_status);
            if degenerate_stage_triggered {
                transport_failure_stage = Some(ClaudeTransportFailureStage::DegenerateStream);
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = %exit_summary,
                    "Claude Code stream looked degenerate; killed CLI and treating as degenerate-stream failure"
                );
                let partial_chars = final_result.chars().count();
                final_result = format!(
                    "Claude Code entered a degenerate output loop (the same short string was repeated many times in the streamed response) and the turn was cut short to avoid a runaway 50-minute bill — see mission ab260b2e for the canonical example.\n\nThe model never produced a terminal result event. Partial output ({} chars) was preserved; resend your last message to try again.",
                    partial_chars
                );
            } else if !saw_non_init_event {
                transport_failure_stage = Some(ClaudeTransportFailureStage::Startup);
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = %exit_summary,
                    process_exited_without_result,
                    idle_timeout_triggered,
                    non_json_lines = non_json_output.len(),
                    malformed_json_lines = malformed_json_output.len(),
                    "Claude Code ended before any usable turn events; treating as startup transport failure"
                );
                final_result = claudecode_pre_turn_transport_message(
                    &exit_summary,
                    &non_json_output,
                    &malformed_json_output,
                    use_resume,
                    &session_id,
                );
            } else {
                let stage = claudecode_transport_failure_stage_for_incomplete_turn(
                    saw_non_init_event,
                    turn_wait_state,
                );
                transport_failure_stage = Some(stage);
                let partial_output =
                    (!final_result.trim().is_empty()).then_some(final_result.as_str());
                let pending_tool_names: Vec<String> = pending_tools
                    .values()
                    .map(|name| format!("- {}", name))
                    .collect();
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = %exit_summary,
                    process_exited_without_result,
                    idle_timeout_triggered,
                    had_partial_output = partial_output.is_some(),
                    "Claude Code turn ended without a terminal result event; treating as incomplete"
                );
                final_result = claudecode_incomplete_turn_message(
                    &exit_summary,
                    ClaudeIncompleteTurnContext {
                        partial_output,
                        non_json_output: &non_json_output,
                        malformed_json_output: &malformed_json_output,
                        process_exited_without_result,
                        idle_timeout_triggered,
                        wait_state: turn_wait_state,
                        pending_tools: &pending_tool_names,
                    },
                );
            }
        }

        if !cancelled && final_result.trim().is_empty() && !had_error {
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
            } else if !malformed_json_output.is_empty() {
                tracing::warn!(
                    mission_id = %mission_id,
                    exit_status = ?exit_status,
                    "Claude Code produced malformed JSON output"
                );
                final_result = format!(
                    "Claude Code produced malformed stream-json output. Last malformed lines: {}",
                    malformed_json_output.join(" | ")
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

        let mut result = if cancelled {
            // The cancel arm fell through here instead of returning a synthetic
            // "Cancelled" failure, so final_result still holds whatever the
            // text/thinking-buffer fallbacks managed to recover. Surface that
            // partial work but mark the mission Interrupted/ServerShutdown
            // so the dashboard renders the resume affordance.
            //
            // Snapshot the cancel marker once — calling
            // `cancel_or_shutdown_failure()` twice could pair "Mission
            // cancelled" text with ServerShutdown (or vice versa) if a
            // shutdown signal arrives between reads.
            let cancel_marker = cancel_or_shutdown_failure();
            if final_result.trim().is_empty() {
                final_result = cancel_marker.output.clone();
            }
            let cancel_reason = cancel_marker
                .terminal_reason
                .unwrap_or(TerminalReason::Cancelled);
            AgentResult::failure(final_result, cost_cents).with_terminal_reason(cancel_reason)
        } else if had_error {
            // Detect rate limit / overloaded errors for account rotation.
            //
            // We check for specific Anthropic error types and HTTP status codes.
            // Using "overloaded_error" rather than bare "overloaded" to avoid
            // false positives from tool output or user content.
            //
            // Check both the final result text and non-JSON output (stderr) for
            // auth/rate-limit markers. When Claude Code is SIGKILL'd mid-turn, the
            // final_result is a generic "did not emit terminal result" message, but
            // stderr may contain the actual auth error from the Anthropic API.
            let combined_for_detection = if non_json_output.is_empty() {
                final_result.clone()
            } else {
                format!("{}\n{}", final_result, non_json_output.join("\n"))
            };
            let reason = if degenerate_stage_triggered {
                // Degenerate-stream is its own failure mode; never let the
                // account-rotation / auth-error inference override it.
                TerminalReason::InfiniteLoop
            } else if is_rate_limited_error(&combined_for_detection) {
                TerminalReason::RateLimited
            } else if is_auth_error(&combined_for_detection) {
                TerminalReason::AuthError
            } else {
                TerminalReason::LlmError
            };
            AgentResult::failure(final_result, cost_cents).with_terminal_reason(reason)
        } else if is_success_path_rate_limited_error(&final_result) {
            // Claude Code sometimes surfaces subscription quota exhaustion as a
            // normal assistant message (e.g. "You've hit your limit · resets
            // 9pm") and exits with code 0. Without this check the turn would be
            // treated as TurnComplete and account rotation would never trigger.
            tracing::warn!(
                mission_id = %mission_id,
                "Claude Code returned a rate-limit message as a successful turn; marking as RateLimited for account rotation"
            );
            AgentResult::failure(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::RateLimited)
        } else if is_success_path_auth_error(&final_result) {
            // Claude Code can surface revoked/expired credential failures as a
            // normal assistant message while exiting successfully. Treat that
            // as AuthError so the caller invalidates stale credentials, refreshes
            // OAuth, and retries instead of completing the mission with the error
            // text as if it were the agent's answer.
            tracing::warn!(
                mission_id = %mission_id,
                "Claude Code returned an auth error as a successful turn; marking as AuthError for credential refresh"
            );
            AgentResult::failure(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::AuthError)
        } else if is_success_path_provider_payload_error(&final_result) {
            // Claude Code can surface provider request validation errors as
            // ordinary assistant text while exiting successfully. Treat them as
            // LLM failures so the mission does not falsely complete.
            tracing::warn!(
                mission_id = %mission_id,
                "Claude Code returned a provider payload error as a successful turn; marking as LlmError"
            );
            AgentResult::failure(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::LlmError)
        } else {
            AgentResult::success(final_result, cost_cents)
                .with_terminal_reason(TerminalReason::TurnComplete)
        };
        if let Some(stage) = transport_failure_stage {
            let pending_tool_names: Vec<String> = pending_tools.values().cloned().collect();
            result = result.with_data(claudecode_transport_failure_data(
                stage,
                idle_timeout_triggered,
                process_exited_without_result,
                &pending_tool_names,
            ));
        }
        let outcome = turn_outcome_for_result(
            &result,
            CompletionSignal::NativeTerminal,
            CompletionConfidence::High,
        );
        result = result.with_turn_outcome(outcome);
        if let Some(model) = model_for_cost {
            result = result.with_model(model.to_string());
        }
        if usage.has_usage() {
            result = result.with_usage(usage);
        }
        result = result.with_cost_source(cost_source);
        result
    }) // end Box::pin(async move { ... })
}

fn claudecode_result_is_startup_transport_failure(result: &AgentResult) -> bool {
    result
        .data
        .as_ref()
        .and_then(|data| data.get("claudecode_transport_failure"))
        .and_then(|value| value.get("stage"))
        .and_then(|value| value.as_str())
        == Some("startup")
}

/// Claude Code turn with the full recovery orchestration:
/// transport-failure retries (resume current session, then fresh-session
/// reset with condensed history), SIGKILL-driven proactive OAuth refresh,
/// stale-credential retry on auth errors, and Anthropic account rotation
/// on rate-limit/auth failures.
///
/// Shared by both the mission arm and the control arm (Phase 3) — they
/// previously carried near-duplicate copies of this loop, with the control
/// copy missing the SIGKILL refresh and initial auth retry.
#[allow(clippy::too_many_arguments)]
/// Returns the byte size of the on-disk Claude session transcript for
/// `session_id` when it exceeds the configured resume cap, otherwise `None`.
///
/// Resuming a very large transcript can make the CLI spend the entire startup
/// window replaying it before emitting any stream-json event, tripping the
/// startup timeout (see `run_claudecode_turn_with_recovery`). The cap is
/// configurable via `SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES`
/// (default 25 MB); set it to `0` to disable the check entirely.
fn claudecode_oversized_resume_transcript(
    work_dir: &std::path::Path,
    session_id: &str,
) -> Option<u64> {
    let cap = std::env::var("SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(25_000_000);
    if cap == 0 {
        return None;
    }
    // Claude stores transcripts at
    // <work_dir>/.claude/projects/<hash>/<session_id>.jsonl. The <hash> dir
    // depends on the absolute cwd inside the container, so scan every project
    // dir rather than guessing the hash.
    let projects_dir = work_dir.join(".claude").join("projects");
    let entries = std::fs::read_dir(&projects_dir).ok()?;
    let file_name = format!("{session_id}.jsonl");
    for entry in entries.flatten() {
        let candidate = entry.path().join(&file_name);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && meta.len() > cap {
                return Some(meta.len());
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_claudecode_turn_with_recovery(
    workspace: &Workspace,
    work_dir: &std::path::Path,
    message: &str,
    model: Option<&str>,
    model_effort: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    secrets: Option<Arc<SecretsStore>>,
    app_working_dir: &std::path::Path,
    session_id: Option<&str>,
    is_continuation: bool,
    tool_hub: Option<Arc<FrontendToolHub>>,
    status: Option<Arc<RwLock<ControlStatus>>>,
    history: &[(String, String)],
    max_history_total_chars: usize,
) -> AgentResult {
    // Track the effective message and session used for the most recent
    // attempt, so account rotation uses the right context (e.g. after
    // session corruption recovery rebuilds the message).
    let mut effective_msg = message.to_string();
    let mut effective_sid: Option<String> = session_id.map(str::to_string);
    let mut attempted_same_session_resume = false;
    let mut attempted_session_reset = false;

    // Proactive guard: resuming a very large session transcript can make the
    // Claude CLI spend the entire startup window replaying it before emitting
    // any stream-json event, tripping the startup timeout. When the would-be
    // resumed transcript exceeds the configured cap, rotate to a fresh session
    // up front (same mechanism as the ResetSessionFresh recovery arm below) so
    // the first attempt starts clean with rebuilt history instead of hanging.
    let mut first_turn_is_continuation = is_continuation;
    if is_continuation && !cancel.is_cancelled() && !crate::api::routes::is_shutdown_initiated() {
        if let Some(sid) = effective_sid.clone() {
            if let Some(size) = claudecode_oversized_resume_transcript(work_dir, &sid) {
                let new_session_id = Uuid::new_v4().to_string();
                tracing::warn!(
                    mission_id = %mission_id,
                    old_session_id = %sid,
                    new_session_id = %new_session_id,
                    transcript_bytes = size,
                    "Resume transcript exceeds cap; rotating to a fresh session before first attempt"
                );
                let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                    mission_id,
                    session_id: new_session_id.clone(),
                });
                let session_marker = work_dir.join(".claude-session-initiated");
                if session_marker.exists() {
                    let _ = std::fs::remove_file(&session_marker);
                }
                let history_for_retry = match history.last() {
                    Some((role, content)) if role == "user" && content == message => {
                        &history[..history.len() - 1]
                    }
                    _ => history,
                };
                effective_msg = if history_for_retry.is_empty() {
                    message.to_string()
                } else {
                    let history_ctx =
                        build_history_context(history_for_retry, max_history_total_chars);
                    format!(
                        "## Prior conversation (session was reset: prior transcript too large to resume)\n\n\
                         {history_ctx}\
                         ## Current message\n\n\
                         {message}"
                    )
                };
                effective_sid = Some(new_session_id);
                first_turn_is_continuation = false;
                // A fresh session has no transcript to replay; if it still
                // times out at startup the cause is environmental, so don't let
                // recovery burn another reset on it.
                attempted_session_reset = true;
            }
        }
    }

    let mut result = run_claudecode_turn(
        workspace,
        work_dir,
        &effective_msg,
        model,
        model_effort,
        agent,
        mission_id,
        events_tx.clone(),
        cancel.clone(),
        secrets.clone(),
        app_working_dir,
        effective_sid.as_deref(),
        first_turn_is_continuation,
        tool_hub.clone(),
        status.clone(),
        None, // override_auth: use default credential resolution
        false,
    )
    .await;

    let mut force_argv_prompt = false;
    if crate::util::env_var_bool("SANDBOXED_SH_CLAUDE_STREAM_INPUT", false)
        && claudecode_result_is_startup_transport_failure(&result)
        && !cancel.is_cancelled()
        && !crate::api::routes::is_shutdown_initiated()
    {
        force_argv_prompt = true;
        tracing::warn!(
            mission_id = %mission_id,
            "Claude stream-input turn emitted zero usable events; retrying once with argv prompt delivery"
        );
        result = run_claudecode_turn(
            workspace,
            work_dir,
            &effective_msg,
            model,
            model_effort,
            agent,
            mission_id,
            events_tx.clone(),
            cancel.clone(),
            secrets.clone(),
            app_working_dir,
            effective_sid.as_deref(),
            is_continuation,
            tool_hub.clone(),
            status.clone(),
            None,
            true,
        )
        .await;
    }

    loop {
        if cancel.is_cancelled() || crate::api::routes::is_shutdown_initiated() {
            tracing::debug!(
                mission_id = %mission_id,
                "Skipping Claude transport recovery because execution is cancelling or shutting down"
            );
            break;
        }

        match claudecode_transport_recovery_strategy(
            &result,
            effective_sid.is_some(),
            attempted_same_session_resume,
            attempted_session_reset,
        ) {
            ClaudeTransportRecoveryStrategy::None => break,
            ClaudeTransportRecoveryStrategy::ResumeCurrentSession => {
                attempted_same_session_resume = true;
                tracing::warn!(
                    mission_id = %mission_id,
                    session_id = ?effective_sid,
                    error = %result.output,
                    "Incomplete Claude turn detected; retrying once by continuing the current session"
                );
                effective_msg = claudecode_resume_current_session_message().to_string();
                result = run_claudecode_turn(
                    workspace,
                    work_dir,
                    &effective_msg,
                    model,
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    secrets.clone(),
                    app_working_dir,
                    effective_sid.as_deref(),
                    true,
                    tool_hub.clone(),
                    status.clone(),
                    None,
                    force_argv_prompt,
                )
                .await;
            }
            ClaudeTransportRecoveryStrategy::ResetSessionFresh => {
                attempted_session_reset = true;
                let new_session_id = Uuid::new_v4().to_string();
                tracing::warn!(
                    mission_id = %mission_id,
                    old_session_id = ?effective_sid,
                    new_session_id = %new_session_id,
                    attempted_same_session_resume,
                    is_continuation = is_continuation,
                    error = %result.output,
                    "Claude transport recovery is rotating to a fresh session"
                );

                let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                    mission_id,
                    session_id: new_session_id.clone(),
                });

                let session_marker = work_dir.join(".claude-session-initiated");
                if session_marker.exists() {
                    let _ = std::fs::remove_file(&session_marker);
                }

                let history_for_retry = match history.last() {
                    Some((role, content)) if role == "user" && content == message => {
                        &history[..history.len() - 1]
                    }
                    _ => history,
                };
                let retry_message = if history_for_retry.is_empty() {
                    message.to_string()
                } else {
                    let history_ctx =
                        build_history_context(history_for_retry, max_history_total_chars);
                    format!(
                        "## Prior conversation (session was reset due to a transient error)\n\n\
                         {history_ctx}\
                         ## Current message\n\n\
                         {message}"
                    )
                };

                effective_msg = retry_message;
                effective_sid = Some(new_session_id);

                result = run_claudecode_turn(
                    workspace,
                    work_dir,
                    &effective_msg,
                    model,
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    secrets.clone(),
                    app_working_dir,
                    effective_sid.as_deref(),
                    false,
                    tool_hub.clone(),
                    status.clone(),
                    None,
                    force_argv_prompt,
                )
                .await;
            }
        }
    }

    // Proactive auth refresh for SIGKILL'd processes: when Claude Code is
    // killed mid-turn (signal: Killed, no terminal result), the cause is often
    // an expired OAuth token that caused Node.js to crash. Even if we can't
    // detect "auth error" in the output, preemptively refresh credentials so
    // the transport recovery retry (above) uses fresh tokens. This is cheap
    // (just a token validity check) and prevents cascading auth failures.
    if !cancel.is_cancelled()
        && result.terminal_reason == Some(TerminalReason::LlmError)
        && result.output.contains("signal: Some(\"Killed\")")
    {
        tracing::info!(
            mission_id = %mission_id,
            "SIGKILL detected — preemptively refreshing OAuth credentials"
        );
        let mission_creds = work_dir.join(".claude").join(".credentials.json");
        if mission_creds.exists() {
            let _ = std::fs::remove_file(&mission_creds);
        }
        if let Err(e) = crate::api::ai_providers::force_refresh_anthropic_oauth_token().await {
            tracing::debug!(
                "Preemptive OAuth refresh after SIGKILL failed (non-fatal): {}",
                e
            );
        }
    }

    // Auth error recovery: if the token was revoked server-side but the
    // local expiry hadn't passed yet, invalidate stale credentials, force
    // an OAuth refresh, and retry once.
    if result.terminal_reason == Some(TerminalReason::AuthError) && !cancel.is_cancelled() {
        tracing::warn!(
            mission_id = %mission_id,
            "Auth error detected — invalidating stale credentials and retrying"
        );

        refresh_claude_credentials_after_auth_error(work_dir, "mission_runner_initial_auth_error")
            .await;

        // Retry with fresh credentials (override_auth=None forces re-resolution)
        result = run_claudecode_turn(
            workspace,
            work_dir,
            &effective_msg,
            model,
            model_effort,
            agent,
            mission_id,
            events_tx.clone(),
            cancel.clone(),
            secrets.clone(),
            app_working_dir,
            effective_sid.as_deref(),
            false,
            tool_hub.clone(),
            status.clone(),
            None,
            force_argv_prompt,
        )
        .await;
    }

    // Account rotation: if rate-limited, or if auth still fails after
    // one refresh attempt, try alternate Anthropic credentials.
    // The first entry in the list is the highest-priority credential, which
    // is almost certainly what the initial (override_auth=None) call used.
    // Skip it to avoid a guaranteed duplicate failure.
    let mut rotated_anthropic_account = false;
    if matches!(
        result.terminal_reason,
        Some(TerminalReason::RateLimited | TerminalReason::AuthError)
    ) {
        let rotation_reason = result.terminal_reason;
        let rotation_accounts = anthropic_rotation_accounts(workspace, work_dir, app_working_dir);
        if !rotation_accounts.accounts.is_empty() {
            tracing::info!(
                mission_id = %mission_id,
                total_accounts = rotation_accounts.total_accounts,
                alternate_accounts = rotation_accounts.accounts.len(),
                skipped_current = rotation_accounts.skipped_current,
                ?rotation_reason,
                "Primary Anthropic credential failed; trying alternate credentials"
            );
            for (idx, alt_auth) in rotation_accounts.accounts.into_iter().enumerate() {
                if cancel.is_cancelled() {
                    break;
                }
                rotated_anthropic_account = true;
                tracing::info!(
                    mission_id = %mission_id,
                    rotation_attempt = idx + 1,
                    auth_type = match &alt_auth {
                        crate::api::ai_providers::ClaudeCodeAuth::ApiKey(_) => "api_key",
                        crate::api::ai_providers::ClaudeCodeAuth::OAuthToken(_) => "oauth_token",
                    },
                    "Rotating to alternate Anthropic account"
                );
                result = run_claudecode_turn(
                    workspace,
                    work_dir,
                    &effective_msg,
                    model,
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    secrets.clone(),
                    app_working_dir,
                    effective_sid.as_deref(),
                    is_continuation,
                    tool_hub.clone(),
                    status.clone(),
                    Some(alt_auth),
                    force_argv_prompt,
                )
                .await;
                // Continue rotating on account-specific failures.
                // Other LLM errors (model errors, context limit, etc.)
                // would fail on every account, so stop early to avoid
                // masking the real failure.
                match result.terminal_reason {
                    Some(TerminalReason::RateLimited | TerminalReason::AuthError) => {
                        tracing::info!(
                            mission_id = %mission_id,
                            rotation_attempt = idx + 1,
                            ?result.terminal_reason,
                            "Anthropic credential failed; rotating to next account"
                        );
                        continue;
                    }
                    _ => break,
                }
            }
        }
    }

    // If an alternate OAuth credential is revoked, rotation returns
    // AuthError. Refresh stale Claude credentials and retry once with
    // freshly resolved auth instead of surfacing a raw 401.
    if rotated_anthropic_account
        && result.terminal_reason == Some(TerminalReason::AuthError)
        && !cancel.is_cancelled()
    {
        tracing::warn!(
            mission_id = %mission_id,
            "Auth error detected after credential rotation - invalidating stale credentials and retrying"
        );

        refresh_claude_credentials_after_auth_error(work_dir, "mission_runner_rotated_auth_error")
            .await;

        result = run_claudecode_turn(
            workspace,
            work_dir,
            &effective_msg,
            model,
            model_effort,
            agent,
            mission_id,
            events_tx.clone(),
            cancel.clone(),
            secrets.clone(),
            app_working_dir,
            effective_sid.as_deref(),
            is_continuation,
            tool_hub.clone(),
            status.clone(),
            None,
            force_argv_prompt,
        )
        .await;
    }

    result
}

#[cfg(test)]
mod background_task_tests {
    use super::parse_background_task_start;

    #[test]
    fn parses_real_marker() {
        let content = "Command running in background with ID: bash_1. \
             Output is being written to: /tmp/claude-bg/bash_1.log";
        let (id, path) = parse_background_task_start(content).expect("should parse marker");
        assert_eq!(id, "bash_1");
        assert_eq!(path, "/tmp/claude-bg/bash_1.log");
    }

    #[test]
    fn parses_real_marker_with_trailing_prose() {
        // Exact marker emitted by the Claude CLI: the path is followed by
        // explanatory prose on the same line. Only the path token must be
        // captured (no trailing sentence).
        let content = "Command running in background with ID: bokwqyjak. \
             Output is being written to: \
             /tmp/claude-0/-workspaces-mission-20b6ee43/3cd99fa9-c3f8-4127-954a-d3be352221bf/tasks/bokwqyjak.output. \
             You will be notified when it completes. To check interim output, use Read on that file path.";
        let (id, path) = parse_background_task_start(content).expect("should parse real marker");
        assert_eq!(id, "bokwqyjak");
        assert_eq!(
            path,
            "/tmp/claude-0/-workspaces-mission-20b6ee43/3cd99fa9-c3f8-4127-954a-d3be352221bf/tasks/bokwqyjak.output"
        );
    }

    #[test]
    fn parses_marker_with_surrounding_whitespace_and_newline() {
        let content = "  Command running in background with ID: abc-123. \
             Output is being written to: /var/tmp/out.log\n";
        let (id, path) = parse_background_task_start(content).expect("should tolerate whitespace");
        assert_eq!(id, "abc-123");
        assert_eq!(path, "/var/tmp/out.log");
    }

    #[test]
    fn returns_none_for_unrelated_content() {
        assert!(parse_background_task_start("total 0\n-rw-r--r-- 1 root root 0 file").is_none());
        assert!(parse_background_task_start("").is_none());
    }

    #[test]
    fn returns_none_when_only_id_label_present() {
        // Malformed: id label but no output label.
        assert!(parse_background_task_start("Command running in background with ID: x").is_none());
    }

    #[test]
    fn returns_none_when_id_or_path_empty() {
        // Output label present but path missing.
        let content = "Command running in background with ID: bash_2. Output is being written to:";
        assert!(parse_background_task_start(content).is_none());
        // Id missing.
        let content = "Command running in background with ID: . \
             Output is being written to: /tmp/x.log";
        assert!(parse_background_task_start(content).is_none());
    }
}

#[cfg(test)]
mod resilience_tests {
    use super::{claudecode_oversized_resume_transcript, claudecode_proxy_health_verdict};

    // --- Proxy health verdict (fail-fast classification) ---

    #[test]
    fn proxy_verdict_passes_on_any_http_response() {
        // 2xx/3xx/4xx all prove the proxy is up and routing — including 404
        // (route may not exist) and 401 (auth) — so none should block.
        for code in ["200", "204", "301", "400", "401", "403", "404", "429"] {
            assert!(
                claudecode_proxy_health_verdict("", code).is_ok(),
                "HTTP {code} should pass"
            );
        }
    }

    #[test]
    fn proxy_verdict_fails_on_upstream_5xx() {
        // The observed production stall: provider cooldown / 502 surfaced by the proxy.
        for code in ["500", "502", "503", "529"] {
            assert!(
                claudecode_proxy_health_verdict("", code).is_err(),
                "HTTP {code} should fast-fail"
            );
        }
    }

    #[test]
    fn proxy_verdict_fails_on_connection_errors_and_no_response() {
        assert!(claudecode_proxy_health_verdict("curl: (7) Connection refused", "000").is_err());
        assert!(claudecode_proxy_health_verdict("Could not resolve host: proxy", "000").is_err());
        assert!(claudecode_proxy_health_verdict("Network is unreachable", "000").is_err());
        assert!(claudecode_proxy_health_verdict("Operation timed out", "000").is_err());
        // Empty / unparseable code with no recognizable error → treated as no response.
        assert!(claudecode_proxy_health_verdict("", "000").is_err());
        assert!(claudecode_proxy_health_verdict("", "").is_err());
    }

    // --- Oversized resume transcript detection ---

    #[test]
    fn oversized_transcript_detected_above_cap_only() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let work_dir = tmp.path();
        let session_id = "11111111-2222-3333-4444-555555555555";
        let proj = work_dir.join(".claude").join("projects").join("somehash");
        std::fs::create_dir_all(&proj).unwrap();
        let transcript = proj.join(format!("{session_id}.jsonl"));

        // Small transcript: under the (default 25MB) cap → not flagged.
        {
            let mut f = std::fs::File::create(&transcript).unwrap();
            f.write_all(b"{\"x\":1}\n").unwrap();
        }
        assert_eq!(
            claudecode_oversized_resume_transcript(work_dir, session_id),
            None
        );

        // Force a tiny cap via env so the test stays fast and deterministic.
        std::env::set_var("SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES", "4");
        let got = claudecode_oversized_resume_transcript(work_dir, session_id);
        assert!(got.is_some(), "transcript above tiny cap should be flagged");

        // Cap of 0 disables the check entirely.
        std::env::set_var("SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES", "0");
        assert_eq!(
            claudecode_oversized_resume_transcript(work_dir, session_id),
            None
        );

        // Unknown session id → nothing found.
        std::env::set_var("SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES", "4");
        assert_eq!(
            claudecode_oversized_resume_transcript(work_dir, "no-such-session"),
            None
        );
        std::env::remove_var("SANDBOXED_SH_CLAUDECODE_MAX_RESUME_TRANSCRIPT_BYTES");
    }
}
