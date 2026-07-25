//! Conservative ChatGPT web-UI turn driver.
//!
//! The browser helper speaks NDJSON on stdout. Browser profiles remain outside
//! mission workspaces and are referenced by an operator-supplied absolute path.

pub mod chromium_cleanup;
pub mod profile_pool;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, FailureClass, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::{
    get_backend_string_list_setting, get_backend_string_setting, get_backend_u64_setting,
};
use chromium_cleanup::SingletonCleanup;
use profile_pool::SlotFailureKind;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DriverEvent {
    Diagnostic {
        message: String,
    },
    TextDelta {
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        id: String,
        name: String,
        result: serde_json::Value,
    },
    Complete {
        content: String,
        model: Option<String>,
    },
    Artifact {
        path: String,
        name: String,
        #[serde(rename = "content_type")]
        _content_type: String,
        #[serde(rename = "size_bytes")]
        _size_bytes: u64,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct Settings {
    driver_path: PathBuf,
    python_path: String,
    profile_dirs: Vec<PathBuf>,
    browser: String,
    proxy_server: Option<String>,
    display: Option<String>,
    timeout: Duration,
    headless: bool,
}

async fn prepare_download_dir(work_dir: &Path) -> Result<PathBuf, String> {
    let canonical_work_dir = work_dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve ChatGPT UI working directory: {error}"))?;
    let download_dir = work_dir.join("chatgpt-ui-downloads");
    tokio::fs::create_dir_all(&download_dir)
        .await
        .map_err(|error| format!("cannot create ChatGPT UI download directory: {error}"))?;
    let canonical_download_dir = download_dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve ChatGPT UI download directory: {error}"))?;
    if !canonical_download_dir.starts_with(&canonical_work_dir) {
        return Err("chatgpt_ui download directory escapes the mission workspace".to_string());
    }
    Ok(canonical_download_dir)
}

fn escape_rich_tag_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn validate_artifact_receipt(
    work_dir: &Path,
    path: &str,
    name: &str,
) -> Result<(String, u64), String> {
    const MAX_ARTIFACT_BYTES: u64 = 50 * 1024 * 1024;
    let canonical_work_dir = work_dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve ChatGPT UI working directory: {error}"))?;
    let canonical_path = Path::new(path)
        .canonicalize()
        .map_err(|error| format!("cannot resolve ChatGPT UI artifact: {error}"))?;
    if !canonical_path.starts_with(&canonical_work_dir) || !canonical_path.is_file() {
        return Err("chatgpt_ui artifact path is outside the mission workspace".to_string());
    }
    let metadata = canonical_path
        .metadata()
        .map_err(|error| format!("cannot inspect ChatGPT UI artifact: {error}"))?;
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err("chatgpt_ui artifact exceeds the 50 MiB limit".to_string());
    }
    let relative = canonical_path
        .strip_prefix(&canonical_work_dir)
        .map_err(|_| "chatgpt_ui artifact path is outside the mission workspace".to_string())?;
    let display_name = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("chatgpt-artifact");
    Ok((
        format!(
            r#"<file path="{}" name="{}" />"#,
            escape_rich_tag_attribute(&relative.to_string_lossy()),
            escape_rich_tag_attribute(display_name),
        ),
        metadata.len(),
    ))
}

fn timeout_result(timeout: Duration) -> AgentResult {
    AgentResult::failure(
        format!("chatgpt_ui timed out after {} seconds", timeout.as_secs()),
        0,
    )
    .with_terminal_reason(TerminalReason::LlmError)
    .with_data(serde_json::json!({
        "provider_error_source": "chatgpt_ui_driver",
        "failure_class": crate::agents::FailureClass::ProviderError,
        "classification_source": "structured",
    }))
}

/// Map a structured driver error code onto a terminal reason, a failure
/// class, and — when the failure is slot-local — a profile-pool health record.
fn classify_driver_error(
    code: Option<&str>,
) -> (TerminalReason, FailureClass, Option<SlotFailureKind>) {
    match code {
        Some("auth_required") => (
            TerminalReason::AuthError,
            FailureClass::AuthError,
            Some(SlotFailureKind::Auth),
        ),
        Some("rate_limited") => (TerminalReason::RateLimited, FailureClass::RateLimited, None),
        Some("browser_launch") => (
            TerminalReason::LlmError,
            FailureClass::TransportError,
            Some(SlotFailureKind::Launch),
        ),
        Some("compatibility") => (
            TerminalReason::LlmError,
            FailureClass::ProviderError,
            Some(SlotFailureKind::Compatibility),
        ),
        _ => (TerminalReason::LlmError, FailureClass::ProviderError, None),
    }
}

fn parse_bool_setting(key: &str, default: bool) -> bool {
    crate::api::mission_runner::get_backend_bool_setting("chatgpt_ui", key).unwrap_or(default)
}

fn safe_driver_diagnostic(message: &str) -> Option<&str> {
    match message {
        "compatibility=chatgpt-ui-v2; browser=chromium"
        | "compatibility=chatgpt-ui-v2; browser=firefox"
        | "compatibility=chatgpt-ui-v2; browser=webkit"
        | "stage=page_loaded"
        | "stage=account_confirmed"
        | "stage=blank_route"
        | "stage=composer_ready"
        | "stage=send_button_fallback"
        | "stage=stop_button_fallback"
        | "stage=composer_model_picker_not_ready"
        | "stage=model_already_selected"
        | "stage=composer_model_option_unavailable"
        | "stage=artifact_size_limit"
        | "stage=artifact_download_skipped" => Some(message),
        _ => None,
    }
}

fn validate_proxy_server(value: &str) -> Result<(), String> {
    let parsed =
        url::Url::parse(value).map_err(|_| "chatgpt_ui proxy_server must be a URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err("chatgpt_ui proxy_server must use http, https, socks5, or socks5h".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("chatgpt_ui proxy_server must not contain credentials".to_string());
    }
    Ok(())
}

fn validate_display(value: &str) -> Result<(), String> {
    let Some(rest) = value.strip_prefix(':') else {
        return Err("chatgpt_ui display must use X11 syntax such as :93".to_string());
    };
    let mut parts = rest.split('.');
    let display = parts.next().unwrap_or_default();
    let screen = parts.next();
    if display.is_empty()
        || !display.chars().all(|character| character.is_ascii_digit())
        || screen.is_some_and(|value| {
            value.is_empty() || !value.chars().all(|character| character.is_ascii_digit())
        })
        || parts.next().is_some()
    {
        return Err("chatgpt_ui display must use X11 syntax such as :93".to_string());
    }
    Ok(())
}

pub(crate) fn configured_profile_dirs(app_working_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut configured_profiles = get_backend_string_setting("chatgpt_ui", "profile_dir")
        .into_iter()
        .chain(get_backend_string_list_setting(
            "chatgpt_ui",
            "profile_dirs",
        ))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    configured_profiles.dedup();
    if configured_profiles.is_empty() {
        return Err("chatgpt_ui profile_dir or profile_dirs is required".to_string());
    }
    let canonical_working_dir = app_working_dir.canonicalize().ok();
    let mut profile_dirs = Vec::with_capacity(configured_profiles.len());
    for profile_dir in configured_profiles {
        if !profile_dir.is_absolute() {
            return Err("chatgpt_ui profile directories must be absolute paths".to_string());
        }
        if !profile_dir.is_dir() {
            return Err(format!(
                "chatgpt_ui profile directory does not exist or is not a directory: {}",
                profile_dir.display()
            ));
        }
        let canonical_profile = profile_dir
            .canonicalize()
            .map_err(|error| format!("failed to resolve chatgpt_ui profile directory: {error}"))?;
        if canonical_working_dir
            .as_ref()
            .is_some_and(|working_dir| canonical_profile.starts_with(working_dir))
        {
            return Err(
                "chatgpt_ui profile directories must be outside the sandboxed.sh working directory"
                    .to_string(),
            );
        }
        if !profile_dirs.contains(&canonical_profile) {
            profile_dirs.push(canonical_profile);
        }
    }
    Ok(profile_dirs)
}

fn validated_settings(app_working_dir: &Path) -> Result<Settings, String> {
    let profile_dirs = configured_profile_dirs(app_working_dir)?;
    let driver_path = get_backend_string_setting("chatgpt_ui", "driver_path")
        .map(PathBuf::from)
        .ok_or_else(|| "chatgpt_ui driver_path is required".to_string())?;
    if !driver_path.is_absolute() {
        return Err("chatgpt_ui driver_path must be an absolute path".to_string());
    }
    if !driver_path.is_file() {
        return Err(format!(
            "chatgpt_ui browser driver not found: {}",
            driver_path.display()
        ));
    }
    let timeout_secs = get_backend_u64_setting("chatgpt_ui", "timeout_secs").unwrap_or(14_400);
    if !(30..=86_400).contains(&timeout_secs) {
        return Err("chatgpt_ui timeout_secs must be between 30 and 86400".to_string());
    }
    let browser = get_backend_string_setting("chatgpt_ui", "browser")
        .unwrap_or_else(|| "chromium".to_string());
    if !matches!(browser.as_str(), "chromium" | "firefox" | "webkit") {
        return Err("chatgpt_ui browser must be chromium, firefox, or webkit".to_string());
    }
    let proxy_server = get_backend_string_setting("chatgpt_ui", "proxy_server")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(proxy_server) = proxy_server.as_deref() {
        validate_proxy_server(proxy_server)?;
    }
    let headless = parse_bool_setting("headless", true);
    let display = get_backend_string_setting("chatgpt_ui", "display")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if !headless {
        let display = display
            .as_deref()
            .ok_or_else(|| "chatgpt_ui display is required when headless is false".to_string())?;
        validate_display(display)?;
    }
    Ok(Settings {
        driver_path,
        python_path: get_backend_string_setting("chatgpt_ui", "python_path")
            .unwrap_or_else(|| "python3".to_string()),
        profile_dirs,
        browser,
        proxy_server,
        display,
        timeout: Duration::from_secs(timeout_secs),
        headless,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn run_chatgpt_ui_turn(
    work_dir: &Path,
    message: &str,
    model: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &Path,
) -> AgentResult {
    let settings = match validated_settings(app_working_dir) {
        Ok(settings) => settings,
        Err(error) => {
            return AgentResult::failure(error, 0).with_terminal_reason(TerminalReason::AuthError)
        }
    };
    let (profile_slot, profile_dir, _profile_lock) = match profile_pool::acquire_profile(
        &settings.profile_dirs,
        mission_id,
        &events_tx,
        &cancel,
    )
    .await
    {
        Ok(lease) => lease,
        Err(result) => return result,
    };
    if settings.browser == "chromium" {
        let owned = chromium_cleanup::pool_owns_singletons(&profile_dir);
        let outcome = chromium_cleanup::cleanup_profile_singletons(&profile_dir, owned);
        tracing::debug!(
            mission_id = %mission_id,
            outcome = outcome.as_str(),
            "chatgpt_ui pre-launch profile singleton cleanup"
        );
        if !outcome.profile_is_launchable() {
            profile_pool::record_slot_failure(&profile_dir, SlotFailureKind::Launch);
            let message = match outcome {
                SingletonCleanup::ActiveProcess => {
                    "chatgpt_ui profile is held by a live browser process outside the profile pool; close it before retrying"
                }
                SingletonCleanup::ForeignHost => {
                    "chatgpt_ui profile holds a SingletonLock from another host; remove it manually if that browser is gone"
                }
                SingletonCleanup::Unrecognized => {
                    "chatgpt_ui profile has an unrecognized Chromium SingletonLock; inspect it manually before retrying"
                }
                SingletonCleanup::Clean | SingletonCleanup::Removed(_) => unreachable!(),
            };
            return AgentResult::failure(message, 0)
                .with_terminal_reason(TerminalReason::LlmError)
                .with_data(serde_json::json!({
                    "provider_error_source": "chatgpt_ui_profile_pool",
                    "failure_class": FailureClass::TransportError,
                    "classification_source": "structured",
                }));
        }
    }
    let download_dir = match prepare_download_dir(work_dir).await {
        Ok(path) => path,
        Err(error) => {
            return AgentResult::failure(error, 0).with_terminal_reason(TerminalReason::LlmError)
        }
    };

    let mut command = Command::new(&settings.python_path);
    command
        .arg(&settings.driver_path)
        .arg("--profile-dir")
        .arg(&profile_dir)
        .arg("--browser")
        .arg(&settings.browser)
        .arg("--headless")
        .arg(if settings.headless { "true" } else { "false" });
    if let Some(proxy_server) = settings.proxy_server.as_deref() {
        command.arg("--proxy-server").arg(proxy_server);
    }
    command
        .current_dir(work_dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Driver stderr can contain browser/OS diagnostics with account or
        // filesystem details. Do not ingest it into mission logs.
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(display) = settings.display.as_deref() {
        command.env("DISPLAY", display);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            profile_pool::record_slot_failure(&profile_dir, SlotFailureKind::Launch);
            return AgentResult::failure(format!("failed to start chatgpt_ui driver: {error}"), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    if settings.browser == "chromium" {
        // Claim ownership only after the driver was successfully spawned. A
        // pre-spawn marker could authorize a later run to remove singleton
        // state that this pool never created.
        chromium_cleanup::claim_singleton_ownership(&profile_dir);
    }
    #[cfg(unix)]
    let process_group = child.id().map(|pid| -(pid as i32));

    let request = serde_json::json!({
        "type": "run",
        "message": message,
        "model": model,
        "timeout_ms": settings.timeout.as_millis() as u64,
        "download_dir": download_dir,
    });
    if let Some(mut stdin) = child.stdin.take() {
        if stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .is_err()
        {
            terminate_child_tree(&mut child).await;
            return AgentResult::failure("failed to send request to chatgpt_ui driver", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    }

    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut model_used = model.map(str::to_string);
    let mut pending_tools: HashMap<String, String> = HashMap::new();
    let mut artifact_receipts: Vec<(String, String)> = Vec::new();
    let mut completed = false;
    let deadline_at = Instant::now() + settings.timeout;
    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                terminate_child_tree(&mut child).await;
                return AgentResult::failure(
                    if crate::api::routes::is_shutdown_initiated() {
                        "Server restart — paused. Click Resume to continue."
                    } else {
                        "Mission cancelled"
                    }, 0
                ).with_terminal_reason(if crate::api::routes::is_shutdown_initiated() {
                    TerminalReason::ServerShutdown
                } else {
                    TerminalReason::Cancelled
                });
            }
            _ = &mut deadline => {
                kill_child_tree(&mut child).await;
                return timeout_result(settings.timeout);
            }
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        terminate_child_tree(&mut child).await;
                        return AgentResult::failure(format!("chatgpt_ui stream read failed: {error}"), 0)
                            .with_terminal_reason(TerminalReason::LlmError);
                    }
                };
                let event: DriverEvent = match serde_json::from_str(&line) {
                    Ok(event) => event,
                    Err(error) => {
                        terminate_child_tree(&mut child).await;
                        return AgentResult::failure(
                            format!("chatgpt_ui emitted an invalid protocol event: {error}"), 0
                        ).with_terminal_reason(TerminalReason::LlmError);
                    }
                };
                match event {
                    DriverEvent::Diagnostic { message } => {
                        if let Some(diagnostic) = safe_driver_diagnostic(&message) {
                            tracing::debug!(
                                mission_id = %mission_id,
                                diagnostic,
                                "chatgpt_ui driver diagnostic received"
                            );
                        } else {
                            // Driver paths are operator-configurable. Never
                            // copy an unrecognized payload into server logs.
                            tracing::debug!(
                                mission_id = %mission_id,
                                "chatgpt_ui driver emitted an unrecognized diagnostic"
                            );
                        }
                    }
                    DriverEvent::TextDelta { content } => {
                        output = content;
                        let _ = events_tx.send(AgentEvent::TextDelta { content: output.clone(), mission_id: Some(mission_id) });
                    }
                    DriverEvent::ToolCall { id, name, args } => {
                        if pending_tools.insert(id.clone(), name.clone()).is_some() {
                            terminate_child_tree(&mut child).await;
                            return AgentResult::failure(
                                "chatgpt_ui emitted a duplicate unresolved tool call id", 0
                            ).with_terminal_reason(TerminalReason::LlmError);
                        }
                        let _ = events_tx.send(AgentEvent::ToolCall { tool_call_id: id, name, args, mission_id: Some(mission_id) });
                    }
                    DriverEvent::ToolResult { id, name, result } => {
                        if pending_tools.remove(&id).as_deref() != Some(name.as_str()) {
                            terminate_child_tree(&mut child).await;
                            return AgentResult::failure(
                                "chatgpt_ui emitted an unmatched tool result", 0
                            ).with_terminal_reason(TerminalReason::LlmError);
                        }
                        let _ = events_tx.send(AgentEvent::ToolResult { tool_call_id: id, name, result, mission_id: Some(mission_id) });
                    }
                    DriverEvent::Complete { content, model } => {
                        output = content;
                        model_used = model.or(model_used);
                        completed = true;
                        break;
                    }
                    DriverEvent::Artifact { path, name, .. } => {
                        artifact_receipts.push((path, name));
                    }
                    DriverEvent::Error { code, message } => {
                        let (reason, failure_class, slot_failure) =
                            classify_driver_error(code.as_deref());
                        if let Some(kind) = slot_failure {
                            profile_pool::record_slot_failure(&profile_dir, kind);
                        }
                        terminate_child_tree(&mut child).await;
                        return AgentResult::failure(format!("chatgpt_ui: {message}"), 0)
                            .with_terminal_reason(reason)
                            .with_data(serde_json::json!({
                                "provider_error_source": "chatgpt_ui_driver",
                                "failure_class": failure_class,
                                "classification_source": "structured",
                            }));
                    }
                }
            }
        }
    }
    let remaining = deadline_at.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        kill_child_tree(&mut child).await;
        return timeout_result(settings.timeout);
    }
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            terminate_child_tree(&mut child).await;
            let shutdown = crate::api::routes::is_shutdown_initiated();
            return AgentResult::failure(
                if shutdown {
                    "Server restart — paused. Click Resume to continue."
                } else {
                    "Mission cancelled"
                }, 0
            ).with_terminal_reason(if shutdown {
                TerminalReason::ServerShutdown
            } else {
                TerminalReason::Cancelled
            });
        }
        _ = &mut deadline => {
            kill_child_tree(&mut child).await;
            return timeout_result(settings.timeout);
        }
        status = tokio::time::timeout(remaining.min(Duration::from_secs(5)), child.wait()) => status,
    };
    let status = match status {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            return AgentResult::failure(format!("chatgpt_ui driver wait failed: {error}"), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
        Err(_) => {
            if Instant::now() >= deadline_at {
                kill_child_tree(&mut child).await;
                return timeout_result(settings.timeout);
            }
            terminate_child_tree(&mut child).await;
            return AgentResult::failure("chatgpt_ui driver did not exit after completion", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    // A well-behaved driver closes its browser before exiting, but enforce
    // that boundary even if it leaves descendants behind after a clean exit.
    #[cfg(unix)]
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(process_group, libc::SIGKILL);
        }
    }
    if settings.browser == "chromium" {
        // Best effort: freshly killed descendants may briefly linger as
        // zombies, in which case the ownership marker defers this sweep to
        // the next lease's pre-launch cleanup.
        let outcome = chromium_cleanup::cleanup_profile_singletons(&profile_dir, true);
        if outcome.profile_is_launchable() {
            chromium_cleanup::release_singleton_ownership(&profile_dir);
        }
        tracing::debug!(
            mission_id = %mission_id,
            outcome = outcome.as_str(),
            "chatgpt_ui post-run profile singleton cleanup"
        );
    }
    if let Err(message) = validate_completion(
        completed,
        &output,
        pending_tools.len(),
        status.is_some_and(|status| status.success()),
    ) {
        return AgentResult::failure(message, 0).with_terminal_reason(TerminalReason::LlmError);
    }
    const MAX_ARTIFACT_FILES: usize = 8;
    const MAX_ARTIFACT_BYTES: u64 = 50 * 1024 * 1024;
    let mut artifact_count = 0usize;
    let mut artifact_bytes = 0u64;
    for (path, name) in artifact_receipts {
        if artifact_count >= MAX_ARTIFACT_FILES {
            tracing::warn!(mission_id = %mission_id, "Rejected excess ChatGPT UI artifact receipt");
            break;
        }
        match validate_artifact_receipt(work_dir, &path, &name) {
            Ok((tag, size)) if artifact_bytes.saturating_add(size) <= MAX_ARTIFACT_BYTES => {
                output.push_str("\n\n");
                output.push_str(&tag);
                artifact_count += 1;
                artifact_bytes += size;
            }
            Ok((_tag, _size)) => {
                tracing::warn!(mission_id = %mission_id, "Rejected ChatGPT UI artifact receipts exceeding the 50 MiB turn limit");
            }
            Err(error) => {
                tracing::warn!(mission_id = %mission_id, error = %error, "Rejected ChatGPT UI artifact receipt");
            }
        }
    }
    profile_pool::record_slot_success(&profile_dir);
    let mut result = AgentResult::success(output, 0)
        .with_terminal_reason(TerminalReason::TurnComplete)
        .with_data(serde_json::json!({
            "backend": "chatgpt_ui",
            "usage_source": "chatgpt_web_subscription",
            "profile_slot": profile_slot + 1,
            "artifact_count": artifact_count,
            "artifact_bytes": artifact_bytes,
        }));
    if let Some(model) = model_used {
        result = result.with_model(model);
    }
    result
}

fn validate_completion(
    completed: bool,
    output: &str,
    pending_tool_count: usize,
    exit_success: bool,
) -> Result<(), &'static str> {
    if pending_tool_count != 0 {
        return Err("chatgpt_ui ended with unresolved tool calls");
    }
    if !completed || output.trim().is_empty() || !exit_success {
        return Err("chatgpt_ui driver exited without a completed response");
    }
    Ok(())
}

async fn terminate_child_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let process_group = -(pid as i32);
        unsafe {
            libc::kill(process_group, libc::SIGTERM);
        }
        let exited = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_ok();
        // The group leader can exit before Chromium descendants. Always send
        // SIGKILL to the original process group after the grace period; ESRCH
        // simply means the group is already empty.
        unsafe {
            libc::kill(process_group, libc::SIGKILL);
        }
        if !exited {
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
        return;
    }
    let _ = child.kill().await;
}

/// Enforce a hard deadline without adding a graceful-shutdown tail to it.
async fn kill_child_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
        return;
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_driver_events() {
        let event: DriverEvent = serde_json::from_str(
            r#"{"type":"tool_call","id":"1","name":"read_file","args":{"path":"a"}}"#,
        )
        .unwrap();
        assert!(matches!(event, DriverEvent::ToolCall { .. }));
        let artifact: DriverEvent = serde_json::from_str(
            r#"{"type":"artifact","path":"/tmp/work/report.txt","name":"report.txt","content_type":"text/plain","size_bytes":12}"#,
        )
        .unwrap();
        assert!(matches!(artifact, DriverEvent::Artifact { .. }));
    }

    #[test]
    fn driver_diagnostics_are_allowlisted_before_logging() {
        assert_eq!(
            safe_driver_diagnostic("stage=model_already_selected"),
            Some("stage=model_already_selected")
        );
        assert_eq!(
            safe_driver_diagnostic("compatibility=chatgpt-ui-v2; browser=chromium"),
            Some("compatibility=chatgpt-ui-v2; browser=chromium")
        );
        assert_eq!(
            safe_driver_diagnostic("stage=send_button_fallback"),
            Some("stage=send_button_fallback")
        );
        assert_eq!(
            safe_driver_diagnostic("stage=stop_button_fallback"),
            Some("stage=stop_button_fallback")
        );
        assert_eq!(
            safe_driver_diagnostic("stage=page_loaded account=user@example.com"),
            None
        );
        assert_eq!(safe_driver_diagnostic("prompt=private text"), None);
    }

    #[test]
    fn timeout_is_a_structured_provider_failure() {
        let result = timeout_result(Duration::from_secs(900));
        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
        assert_eq!(result.output, "chatgpt_ui timed out after 900 seconds");
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("failure_class"))
                .and_then(serde_json::Value::as_str),
            Some("provider_error")
        );
    }

    #[test]
    fn driver_error_codes_map_to_failure_classes_and_slot_health() {
        assert_eq!(
            classify_driver_error(Some("auth_required")),
            (
                TerminalReason::AuthError,
                FailureClass::AuthError,
                Some(SlotFailureKind::Auth)
            )
        );
        assert_eq!(
            classify_driver_error(Some("rate_limited")),
            (TerminalReason::RateLimited, FailureClass::RateLimited, None)
        );
        assert_eq!(
            classify_driver_error(Some("browser_launch")),
            (
                TerminalReason::LlmError,
                FailureClass::TransportError,
                Some(SlotFailureKind::Launch)
            )
        );
        assert_eq!(
            classify_driver_error(Some("compatibility")),
            (
                TerminalReason::LlmError,
                FailureClass::ProviderError,
                Some(SlotFailureKind::Compatibility)
            )
        );
        assert_eq!(
            classify_driver_error(Some("timeout")),
            (TerminalReason::LlmError, FailureClass::ProviderError, None)
        );
        assert_eq!(
            classify_driver_error(None),
            (TerminalReason::LlmError, FailureClass::ProviderError, None)
        );
    }

    #[test]
    fn completion_requires_signal_content_clean_exit_and_balanced_tools() {
        assert!(validate_completion(true, "done", 0, true).is_ok());
        assert_eq!(
            validate_completion(false, "partial", 0, true),
            Err("chatgpt_ui driver exited without a completed response")
        );
        assert_eq!(
            validate_completion(true, "done", 1, true),
            Err("chatgpt_ui ended with unresolved tool calls")
        );
        assert!(validate_completion(true, " ", 0, true).is_err());
        assert!(validate_completion(true, "done", 0, false).is_err());
    }

    #[test]
    fn validates_proxy_server_without_embedded_credentials() {
        assert!(validate_proxy_server("socks5://127.0.0.1:10880").is_ok());
        assert!(validate_proxy_server("https://proxy.example.com:8443").is_ok());
        assert!(validate_proxy_server("ftp://proxy.example.com").is_err());
        assert!(validate_proxy_server("socks5://user:secret@127.0.0.1:10880").is_err());
    }

    #[test]
    fn validates_x11_display_without_accepting_shell_syntax() {
        assert!(validate_display(":93").is_ok());
        assert!(validate_display(":93.0").is_ok());
        assert!(validate_display("localhost:93").is_err());
        assert!(validate_display(":93;touch /tmp/nope").is_err());
    }

    #[test]
    fn artifact_receipt_must_resolve_inside_the_mission_workspace() {
        let root = tempfile::tempdir().unwrap();
        let work_dir = root.path().join("mission");
        let outside = root.path().join("outside.txt");
        std::fs::create_dir(&work_dir).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        let artifact = work_dir.join("report.txt");
        std::fs::write(&artifact, "report").unwrap();

        let validated =
            validate_artifact_receipt(&work_dir, artifact.to_str().unwrap(), "report.txt").unwrap();
        assert_eq!(
            validated.0,
            r#"<file path="report.txt" name="report.txt" />"#
        );
        assert_eq!(validated.1, 6);
        assert!(
            validate_artifact_receipt(&work_dir, outside.to_str().unwrap(), "outside.txt").is_err()
        );
    }
}
