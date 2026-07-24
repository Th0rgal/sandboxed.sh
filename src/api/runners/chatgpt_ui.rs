//! Conservative ChatGPT web-UI turn driver.
//!
//! The browser helper speaks NDJSON on stdout. Browser profiles remain outside
//! mission workspaces and are referenced by an operator-supplied absolute path.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use fs2::FileExt;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::{get_backend_string_setting, get_backend_u64_setting};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DriverEvent {
    Diagnostic {
        #[serde(rename = "message")]
        _message: String,
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
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct Settings {
    driver_path: PathBuf,
    python_path: String,
    profile_dir: PathBuf,
    browser: String,
    timeout: Duration,
    headless: bool,
}

struct ProfileLock(File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock_profile(profile_dir: &Path) -> Result<ProfileLock, String> {
    let name = profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("profile");
    let lock_path = profile_dir
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(format!(".{name}.sandboxed-chatgpt-ui.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("cannot create ChatGPT UI profile lock: {error}"))?;
    file.try_lock_exclusive().map_err(|_| {
        "ChatGPT UI profile is already in use; each concurrent mission needs a separate profile directory".to_string()
    })?;
    Ok(ProfileLock(file))
}

fn parse_bool_setting(key: &str, default: bool) -> bool {
    crate::api::mission_runner::get_backend_bool_setting("chatgpt_ui", key).unwrap_or(default)
}

fn validated_settings(app_working_dir: &Path) -> Result<Settings, String> {
    let profile = get_backend_string_setting("chatgpt_ui", "profile_dir")
        .ok_or_else(|| "chatgpt_ui profile_dir is required".to_string())?;
    let profile_dir = PathBuf::from(profile);
    if !profile_dir.is_absolute() {
        return Err("chatgpt_ui profile_dir must be an absolute path".to_string());
    }
    if !profile_dir.is_dir() {
        return Err(format!(
            "chatgpt_ui profile_dir does not exist or is not a directory: {}",
            profile_dir.display()
        ));
    }
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
    let canonical_profile = profile_dir
        .canonicalize()
        .map_err(|error| format!("failed to resolve chatgpt_ui profile_dir: {error}"))?;
    if let Ok(canonical_working_dir) = app_working_dir.canonicalize() {
        if canonical_profile.starts_with(canonical_working_dir) {
            return Err(
                "chatgpt_ui profile_dir must be outside the sandboxed.sh working directory"
                    .to_string(),
            );
        }
    }
    let timeout_secs = get_backend_u64_setting("chatgpt_ui", "timeout_secs").unwrap_or(900);
    if !(30..=7200).contains(&timeout_secs) {
        return Err("chatgpt_ui timeout_secs must be between 30 and 7200".to_string());
    }
    let browser = get_backend_string_setting("chatgpt_ui", "browser")
        .unwrap_or_else(|| "chromium".to_string());
    if !matches!(browser.as_str(), "chromium" | "firefox" | "webkit") {
        return Err("chatgpt_ui browser must be chromium, firefox, or webkit".to_string());
    }
    Ok(Settings {
        driver_path,
        python_path: get_backend_string_setting("chatgpt_ui", "python_path")
            .unwrap_or_else(|| "python3".to_string()),
        profile_dir: canonical_profile,
        browser,
        timeout: Duration::from_secs(timeout_secs),
        headless: parse_bool_setting("headless", true),
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
    let _profile_lock = match lock_profile(&settings.profile_dir) {
        Ok(lock) => lock,
        Err(error) => {
            return AgentResult::failure(error, 0).with_terminal_reason(TerminalReason::LlmError)
        }
    };

    let mut command = Command::new(&settings.python_path);
    command
        .arg(&settings.driver_path)
        .arg("--profile-dir")
        .arg(&settings.profile_dir)
        .arg("--browser")
        .arg(&settings.browser)
        .arg("--headless")
        .arg(if settings.headless { "true" } else { "false" })
        .current_dir(work_dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Driver stderr can contain browser/OS diagnostics with account or
        // filesystem details. Do not ingest it into mission logs.
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return AgentResult::failure(format!("failed to start chatgpt_ui driver: {error}"), 0)
                .with_terminal_reason(TerminalReason::LlmError)
        }
    };

    let request = serde_json::json!({
        "type": "run",
        "message": message,
        "model": model,
        "timeout_ms": settings.timeout.as_millis() as u64,
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
                return AgentResult::failure(
                    format!("chatgpt_ui timed out after {} seconds", settings.timeout.as_secs()), 0
                ).with_terminal_reason(TerminalReason::LlmError);
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
                    // Diagnostics are deliberately not logged: a future
                    // third-party UI selector must not accidentally turn
                    // account or page text into server logs.
                    DriverEvent::Diagnostic { .. } => tracing::debug!(mission_id = %mission_id, "chatgpt_ui driver diagnostic received"),
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
                    DriverEvent::Error { code, message } => {
                        let reason = if code.as_deref() == Some("auth_required") {
                            TerminalReason::AuthError
                        } else if code.as_deref() == Some("rate_limited") {
                            TerminalReason::RateLimited
                        } else {
                            TerminalReason::LlmError
                        };
                        terminate_child_tree(&mut child).await;
                        return AgentResult::failure(format!("chatgpt_ui: {message}"), 0).with_terminal_reason(reason);
                    }
                }
            }
        }
    }
    let remaining = deadline_at.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        kill_child_tree(&mut child).await;
        return AgentResult::failure(
            format!(
                "chatgpt_ui timed out after {} seconds",
                settings.timeout.as_secs()
            ),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);
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
            return AgentResult::failure(
                format!("chatgpt_ui timed out after {} seconds", settings.timeout.as_secs()), 0
            ).with_terminal_reason(TerminalReason::LlmError);
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
                return AgentResult::failure(
                    format!(
                        "chatgpt_ui timed out after {} seconds",
                        settings.timeout.as_secs()
                    ),
                    0,
                )
                .with_terminal_reason(TerminalReason::LlmError);
            }
            terminate_child_tree(&mut child).await;
            return AgentResult::failure("chatgpt_ui driver did not exit after completion", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    if let Err(message) = validate_completion(
        completed,
        &output,
        pending_tools.len(),
        status.is_some_and(|status| status.success()),
    ) {
        return AgentResult::failure(message, 0).with_terminal_reason(TerminalReason::LlmError);
    }
    let mut result = AgentResult::success(output, 0)
        .with_terminal_reason(TerminalReason::TurnComplete)
        .with_data(serde_json::json!({"backend": "chatgpt_ui", "usage_source": "chatgpt_web_subscription"}));
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
    }

    #[test]
    fn profile_lock_is_exclusive_and_reusable() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let first = lock_profile(&profile).unwrap();
        assert!(lock_profile(&profile).is_err());
        drop(first);
        assert!(lock_profile(&profile).is_ok());
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
}
