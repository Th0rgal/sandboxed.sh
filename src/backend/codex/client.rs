use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::backend::shared::ProcessHandle;
use crate::workspace_exec::WorkspaceExec;

/// Configuration for the Codex client.
#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub cli_path: String,
    pub oauth_token: Option<String>,
    pub default_model: Option<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            cli_path: std::env::var("CODEX_CLI_PATH").unwrap_or_else(|_| "codex".to_string()),
            oauth_token: std::env::var("OPENAI_OAUTH_TOKEN").ok(),
            default_model: None,
        }
    }
}

/// Client for communicating with the Codex CLI.
pub struct CodexClient {
    config: CodexConfig,
}

impl CodexClient {
    pub fn new() -> Self {
        Self {
            config: CodexConfig::default(),
        }
    }

    pub fn with_config(config: CodexConfig) -> Self {
        Self { config }
    }

    pub fn create_session_id(&self) -> String {
        Uuid::new_v4().to_string()
    }

    /// Execute a message and return a stream of events.
    /// Returns a tuple of (event receiver, process handle).
    pub async fn execute_message(
        &self,
        directory: &str,
        message: &str,
        model: Option<&str>,
        _session_id: Option<&str>, // Codex doesn't support session IDs like Claude
        _agent: Option<&str>,      // Codex doesn't have agent types like Claude
        workspace_exec: Option<&WorkspaceExec>,
    ) -> Result<(mpsc::Receiver<CodexEvent>, ProcessHandle)> {
        let (tx, rx) = mpsc::channel(256);

        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "--skip-git-repo-check".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];

        let mut env: HashMap<String, String> = HashMap::new();
        // Set OAuth token if configured
        if let Some(ref token) = self.config.oauth_token {
            env.insert("OPENAI_OAUTH_TOKEN".to_string(), token.clone());
            debug!("Using OAuth token for Codex CLI authentication");
        }

        // Model selection
        let effective_model = model.or(self.config.default_model.as_deref());
        if let Some(m) = effective_model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        // Add the message as a positional arg (guard prompts starting with '-')
        args.push("--".to_string());
        args.push(message.to_string());

        info!(
            "Spawning Codex CLI: directory={}, model={:?}",
            directory, effective_model
        );

        let (program, mut full_args) = if self.config.cli_path.contains(' ') {
            let parts: Vec<&str> = self.config.cli_path.splitn(2, ' ').collect();
            let program = parts[0].to_string();
            let mut full_args = if parts.len() > 1 {
                vec![parts[1].to_string()]
            } else {
                vec![]
            };
            full_args.extend(args.clone());
            (program, full_args)
        } else {
            (self.config.cli_path.clone(), args.clone())
        };

        let mut child = if let Some(exec) = workspace_exec {
            exec.spawn_streaming(Path::new(directory), &program, &full_args, env)
                .await
                .map_err(|e| {
                    error!("Failed to spawn Codex CLI in workspace: {}", e);
                    anyhow!("Failed to spawn Codex CLI in workspace: {}", e)
                })?
        } else {
            let mut cmd = Command::new(&program);
            cmd.current_dir(directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .args(&full_args);
            if !env.is_empty() {
                cmd.envs(env);
            }
            cmd.spawn().map_err(|e| {
                error!("Failed to spawn Codex CLI: {}", e);
                anyhow!(
                    "Failed to spawn Codex CLI: {}. Is it installed at '{}'?",
                    e,
                    self.config.cli_path
                )
            })?
        };

        // Close stdin immediately since we don't need to write to it
        // (message is passed as CLI argument)
        drop(child.stdin.take());

        // Spawn task to read stdout and parse events
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture Codex stdout"))?;

        // Spawn task to consume stderr to prevent deadlock
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture Codex stderr"))?;

        let stderr_capture = Arc::new(Mutex::new(String::new()));
        let stderr_capture_clone = Arc::clone(&stderr_capture);
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                debug!("Codex stderr: {}", trimmed);

                // Keep a small excerpt to surface in "No response" cases.
                let mut captured = stderr_capture_clone.lock().await;
                if captured.len() > 4096 {
                    continue;
                }
                if !captured.is_empty() {
                    captured.push('\n');
                }
                // Avoid exploding logs from very long lines.
                if trimmed.len() > 400 {
                    captured.push_str(&trimmed[..400]);
                    captured.push_str("...");
                } else {
                    captured.push_str(trimmed);
                }
            }
        });

        // Wrap child in Arc<Mutex> so it can be killed from outside the task
        let child_handle = Arc::new(Mutex::new(Some(child)));
        let child_for_task = Arc::clone(&child_handle);

        let task_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut saw_any_event = false;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                // Skip stderr logs that leak into stdout
                if line.starts_with("20") && line.contains(" ERROR ") {
                    debug!("Skipping stderr line: {}", line);
                    continue;
                }

                match serde_json::from_str::<CodexEvent>(&line) {
                    Ok(event) => {
                        saw_any_event = true;
                        debug!("Codex event: {:?}", event);
                        if tx.send(event).await.is_err() {
                            debug!("Receiver dropped, stopping Codex event stream");
                            break;
                        }
                    }
                    Err(e) => {
                        // Log but don't fail - some lines might be non-JSON
                        warn!(
                            "Failed to parse Codex event: {} - line: {}",
                            e,
                            if line.len() > 200 {
                                format!("{}...", line.chars().take(200).collect::<String>())
                            } else {
                                line.clone()
                            }
                        );
                    }
                }
            }

            // If the CLI exited without emitting any JSON events, surface stderr as an error.
            if !saw_any_event {
                let stderr_content = stderr_capture.lock().await;
                if !stderr_content.trim().is_empty() {
                    let _ = tx
                        .send(CodexEvent::Error {
                            message: format!(
                                "Codex CLI produced no JSON output. Stderr: {}",
                                stderr_content
                                    .lines()
                                    .take(10)
                                    .collect::<Vec<_>>()
                                    .join(" | ")
                            ),
                        })
                        .await;
                }
            }

            // Wait for process to finish (if it wasn't killed)
            if let Some(mut child) = child_for_task.lock().await.take() {
                match child.wait().await {
                    Ok(status) => {
                        if !status.success() {
                            warn!("Codex CLI exited with status: {}", status);
                        } else {
                            debug!("Codex CLI exited successfully");
                        }
                    }
                    Err(e) => {
                        error!("Failed to wait for Codex CLI: {}", e);
                    }
                }
            }
        });

        Ok((rx, ProcessHandle::new(child_handle, task_handle)))
    }
}

impl Default for CodexClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Events emitted by Codex CLI in --json mode.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum CodexEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },

    #[serde(rename = "turn.started")]
    TurnStarted,

    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(default)]
        summary: Option<String>,
    },

    #[serde(rename = "turn.failed")]
    TurnFailed { error: ErrorInfo },

    #[serde(rename = "item.created")]
    ItemCreated { item: Item },

    #[serde(rename = "item.updated")]
    ItemUpdated { item: Item },

    #[serde(rename = "item.completed")]
    ItemCompleted { item: Item },

    #[serde(rename = "error")]
    Error { message: String },

    // Catch-all for unknown event types
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ErrorInfo {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Item {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(flatten)]
    pub data: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_thread_started() {
        let json =
            r#"{"type":"thread.started","thread_id":"019c21ae-c46c-7a40-a5f5-36ab53521a27"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ThreadStarted { thread_id } => {
                assert_eq!(thread_id, "019c21ae-c46c-7a40-a5f5-36ab53521a27");
            }
            _ => panic!("Expected ThreadStarted event"),
        }
    }

    #[test]
    fn test_parse_turn_started() {
        let json = r#"{"type":"turn.started"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnStarted => {}
            _ => panic!("Expected TurnStarted event"),
        }
    }

    #[test]
    fn test_parse_error() {
        let json = r#"{"type":"error","message":"unexpected status 401 Unauthorized: "}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::Error { message } => {
                assert!(message.contains("401 Unauthorized"));
            }
            _ => panic!("Expected Error event"),
        }
    }

    #[test]
    fn test_parse_turn_failed() {
        let json =
            r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: "}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnFailed { error } => {
                assert!(error.message.contains("401 Unauthorized"));
            }
            _ => panic!("Expected TurnFailed event"),
        }
    }
}
