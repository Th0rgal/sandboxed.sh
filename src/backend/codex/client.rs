use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    pub model_effort: Option<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            cli_path: std::env::var("CODEX_CLI_PATH").unwrap_or_else(|_| "codex".to_string()),
            oauth_token: std::env::var("OPENAI_OAUTH_TOKEN").ok(),
            default_model: None,
            model_effort: None,
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
        if let Some(effort) = self.config.model_effort.as_deref() {
            args.push("-c".to_string());
            args.push(format!("reasoning.effort=\"{}\"", effort));
        }

        // Read the prompt from stdin instead of placing the full mission
        // transcript on argv. Long resumed missions can otherwise produce very
        // large process command lines, especially through container wrappers.
        args.push("--".to_string());
        args.push("-".to_string());

        info!(
            "Spawning Codex CLI: directory={}, model={:?}, effort={:?}",
            directory, effective_model, self.config.model_effort
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

        // Spawn the child, asking for piped stdin so we can feed the prompt
        // via stdin. If for some reason the exec path returns a child
        // without a stdin pipe (e.g., a container wrapper that closes
        // stdin), fall back to passing the prompt on argv instead of
        // failing the mission outright.
        async fn spawn_codex_child(
            workspace_exec: Option<&WorkspaceExec>,
            directory: &str,
            program: &str,
            full_args: &[String],
            env: HashMap<String, String>,
            cli_path: &str,
        ) -> Result<tokio::process::Child> {
            if let Some(exec) = workspace_exec {
                exec.spawn_streaming(Path::new(directory), program, full_args, env)
                    .await
                    .map_err(|e| {
                        error!("Failed to spawn Codex CLI in workspace: {}", e);
                        anyhow!("Failed to spawn Codex CLI in workspace: {}", e)
                    })
            } else {
                let mut cmd = Command::new(program);
                cmd.current_dir(directory)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .args(full_args);
                if !env.is_empty() {
                    cmd.envs(env);
                }
                cmd.spawn().map_err(|e| {
                    error!("Failed to spawn Codex CLI: {}", e);
                    anyhow!(
                        "Failed to spawn Codex CLI: {}. Is it installed at '{}'?",
                        e,
                        cli_path
                    )
                })
            }
        }

        let mut child = spawn_codex_child(
            workspace_exec,
            directory,
            &program,
            &full_args,
            env.clone(),
            &self.config.cli_path,
        )
        .await?;

        let prompt = message.to_string();
        let stdin_task = if let Some(mut stdin) = child.stdin.take() {
            let prompt_for_stdin = prompt.clone();
            Some(tokio::spawn(async move {
                if let Err(e) = stdin.write_all(prompt_for_stdin.as_bytes()).await {
                    debug!("Failed to write Codex prompt to stdin: {}", e);
                    return;
                }
                if let Err(e) = stdin.shutdown().await {
                    debug!("Failed to close Codex stdin: {}", e);
                }
            }))
        } else {
            // Fallback path: no stdin pipe. Kill the child, rewrite the
            // `-` placeholder to the actual prompt, and respawn. Keeps
            // container-workspace Codex missions working even if the exec
            // layer doesn't forward stdin.
            warn!("Codex child had no stdin pipe; falling back to argv prompt");
            let _ = child.kill().await;
            if let Some(pos) = full_args.iter().rposition(|a| a == "-") {
                full_args[pos] = prompt.clone();
            } else {
                full_args.push(prompt.clone());
            }
            child = spawn_codex_child(
                workspace_exec,
                directory,
                &program,
                &full_args,
                env.clone(),
                &self.config.cli_path,
            )
            .await?;
            None
        };

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
        let stderr_task = tokio::spawn(async move {
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
                    let mut i = 400;
                    while i > 0 && !trimmed.is_char_boundary(i) {
                        i -= 1;
                    }
                    captured.push_str(&trimmed[..i]);
                    captured.push_str("...");
                } else {
                    captured.push_str(trimmed);
                }
            }
        });

        // Wrap child in Arc<Mutex> so it can be killed from outside the task
        let child_handle = Arc::new(Mutex::new(Some(child)));
        let child_for_task = Arc::clone(&child_handle);
        let stdout_non_json = Arc::new(Mutex::new(Vec::<String>::new()));
        let stdout_non_json_clone = Arc::clone(&stdout_non_json);

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
                        let mut captured = stdout_non_json_clone.lock().await;
                        if captured.len() < 10 {
                            if line.len() > 400 {
                                captured.push(format!(
                                    "{}...",
                                    line.chars().take(400).collect::<String>()
                                ));
                            } else {
                                captured.push(line);
                            }
                        }
                    }
                }
            }

            // Wait for process to finish (if it wasn't killed)
            let mut exit_status: Option<std::process::ExitStatus> = None;
            if let Some(mut child) = child_for_task.lock().await.take() {
                match child.wait().await {
                    Ok(status) => {
                        exit_status = Some(status);
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

            if let Some(task) = stdin_task {
                let _ = task.await;
            }
            let _ = stderr_task.await;

            let stderr_content = stderr_capture.lock().await;
            let non_json = stdout_non_json.lock().await;
            let exit_failed = exit_status.as_ref().is_some_and(|status| !status.success());
            let exit_status_text = exit_status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let stderr_excerpt = stderr_content
                .lines()
                .take(10)
                .collect::<Vec<_>>()
                .join(" | ");
            let stdout_excerpt = non_json.join(" | ");

            // If Codex exits unsuccessfully after having emitted partial JSON
            // events, do not let the runner mistake the last progress update for
            // a completed answer.
            if exit_failed {
                let _ = tx
                    .send(CodexEvent::Error {
                        message: format!(
                            "Codex CLI exited before completing the turn (exit_status: {}). Stderr: {} | Stdout: {}",
                            exit_status_text,
                            if stderr_excerpt.is_empty() { "<empty>" } else { &stderr_excerpt },
                            if stdout_excerpt.is_empty() { "<empty>" } else { &stdout_excerpt }
                        ),
                    })
                    .await;
            } else if !saw_any_event {
                if !stderr_content.trim().is_empty() || !non_json.is_empty() {
                    let _ = tx
                        .send(CodexEvent::Error {
                            message: format!(
                                "Codex CLI produced no JSON output (exit_status: {}). Stderr: {} | Stdout: {}",
                                exit_status_text,
                                if stderr_excerpt.is_empty() { "<empty>" } else { &stderr_excerpt },
                                if stdout_excerpt.is_empty() { "<empty>" } else { &stdout_excerpt }
                            ),
                        })
                        .await;
                } else {
                    let _ = tx
                        .send(CodexEvent::Error {
                            message: format!(
                                "Codex CLI produced no JSON output (exit_status: {}). No stderr/stdout captured.",
                                exit_status_text
                            ),
                        })
                        .await;
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
        /// Token usage reported by the Codex CLI at end of turn.
        #[serde(default)]
        usage: Option<CodexUsage>,
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

/// Token usage reported by the Codex CLI in `turn.completed` events.
/// Supports both OpenAI Responses API field names (`input_tokens`/`output_tokens`)
/// and legacy Chat Completions names (`prompt_tokens`/`completion_tokens`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CodexUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub completion_tokens: Option<u64>,
}

impl CodexUsage {
    /// Normalize to (input, output) regardless of field naming convention.
    pub fn normalized(&self) -> (u64, u64) {
        let input = self.input_tokens.or(self.prompt_tokens).unwrap_or(0);
        let output = self.output_tokens.or(self.completion_tokens).unwrap_or(0);
        (input, output)
    }
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

    #[test]
    fn test_parse_turn_completed_with_usage() {
        let json = r#"{"type":"turn.completed","summary":"done","usage":{"input_tokens":1000,"output_tokens":250}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnCompleted { summary, usage } => {
                assert_eq!(summary.as_deref(), Some("done"));
                let u = usage.unwrap();
                assert_eq!(u.normalized(), (1000, 250));
            }
            _ => panic!("Expected TurnCompleted event"),
        }
    }

    #[test]
    fn test_parse_turn_completed_without_usage() {
        let json = r#"{"type":"turn.completed","summary":"done"}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnCompleted { summary, usage } => {
                assert_eq!(summary.as_deref(), Some("done"));
                assert!(usage.is_none());
            }
            _ => panic!("Expected TurnCompleted event"),
        }
    }

    #[test]
    fn test_codex_usage_normalized_prefers_input_tokens() {
        let usage = CodexUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            prompt_tokens: Some(999),
            completion_tokens: Some(999),
        };
        // input_tokens/output_tokens take precedence over prompt/completion
        assert_eq!(usage.normalized(), (100, 50));
    }

    #[test]
    fn test_codex_usage_normalized_falls_back_to_prompt() {
        let usage = CodexUsage {
            input_tokens: None,
            output_tokens: None,
            prompt_tokens: Some(800),
            completion_tokens: Some(200),
        };
        assert_eq!(usage.normalized(), (800, 200));
    }
}
