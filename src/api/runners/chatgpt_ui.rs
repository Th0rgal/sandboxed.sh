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
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::{get_backend_string_setting, get_backend_u64_setting};

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
    let timeout_secs = get_backend_u64_setting("chatgpt_ui", "timeout_secs")
        .unwrap_or(900)
        .clamp(30, 7200);
    Ok(Settings {
        driver_path,
        python_path: get_backend_string_setting("chatgpt_ui", "python_path")
            .unwrap_or_else(|| "python3".to_string()),
        profile_dir,
        browser: get_backend_string_setting("chatgpt_ui", "browser")
            .unwrap_or_else(|| "chromium".to_string()),
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
            let _ = child.kill().await;
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
    let deadline = tokio::time::sleep(settings.timeout);
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
                terminate_child_tree(&mut child).await;
                return AgentResult::failure(
                    format!("chatgpt_ui timed out after {} seconds", settings.timeout.as_secs()), 0
                ).with_terminal_reason(TerminalReason::LlmError);
            }
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => return AgentResult::failure(format!("chatgpt_ui stream read failed: {error}"), 0)
                        .with_terminal_reason(TerminalReason::LlmError),
                };
                let event: DriverEvent = match serde_json::from_str(&line) {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(error = %error, "Ignoring invalid chatgpt_ui driver event");
                        continue;
                    }
                };
                match event {
                    DriverEvent::Diagnostic { message } => tracing::info!(mission_id = %mission_id, "{message}"),
                    DriverEvent::TextDelta { content } => {
                        output = content;
                        let _ = events_tx.send(AgentEvent::TextDelta { content: output.clone(), mission_id: Some(mission_id) });
                    }
                    DriverEvent::ToolCall { id, name, args } => {
                        pending_tools.insert(id.clone(), name.clone());
                        let _ = events_tx.send(AgentEvent::ToolCall { tool_call_id: id, name, args, mission_id: Some(mission_id) });
                    }
                    DriverEvent::ToolResult { id, name, result } => {
                        pending_tools.remove(&id);
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
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            terminate_child_tree(&mut child).await;
            return AgentResult::failure("Mission cancelled", 0)
                .with_terminal_reason(TerminalReason::Cancelled);
        }
        status = tokio::time::timeout(Duration::from_secs(5), child.wait()) => status,
    };
    let status = match status {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            return AgentResult::failure(format!("chatgpt_ui driver wait failed: {error}"), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
        Err(_) => {
            terminate_child_tree(&mut child).await;
            return AgentResult::failure("chatgpt_ui driver did not exit after completion", 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };
    if !pending_tools.is_empty() {
        return AgentResult::failure("chatgpt_ui ended with unresolved tool calls", 0)
            .with_terminal_reason(TerminalReason::LlmError);
    }
    if !completed || output.trim().is_empty() || status.is_none_or(|status| !status.success()) {
        return AgentResult::failure("chatgpt_ui driver exited without a completed response", 0)
            .with_terminal_reason(TerminalReason::LlmError);
    }
    let mut result = AgentResult::success(output, 0)
        .with_terminal_reason(TerminalReason::TurnComplete)
        .with_data(serde_json::json!({"backend": "chatgpt_ui", "usage_source": "chatgpt_web_subscription"}));
    if let Some(model) = model_used {
        result = result.with_model(model);
    }
    result
}

async fn terminate_child_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_ok()
        {
            return;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
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
}
