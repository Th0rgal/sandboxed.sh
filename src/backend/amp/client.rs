use anyhow::{anyhow, Result};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

// Re-export shared types with Amp-specific aliases for backward compat.
pub use crate::backend::shared::{
    CliEvent as AmpEvent, ContentBlock, ProcessHandle as AmpProcessHandle, StreamEvent,
};

/// Configuration for the Amp CLI client.
#[derive(Debug, Clone, Default)]
pub struct AmpConfig {
    /// Path to the amp CLI binary (default: "amp")
    pub cli_path: Option<String>,
    /// Default model to use
    pub default_model: Option<String>,
    /// Default mode (smart, rush)
    pub default_mode: Option<String>,
    /// Amp API key for authentication
    pub api_key: Option<String>,
}

/// Client for interacting with the Amp CLI.
pub struct AmpClient {
    config: AmpConfig,
}

impl AmpClient {
    /// Create a new Amp client with default configuration.
    pub fn new() -> Self {
        Self {
            config: AmpConfig::default(),
        }
    }

    /// Create a new Amp client with custom configuration.
    pub fn with_config(config: AmpConfig) -> Self {
        Self { config }
    }

    /// Generate a session ID for thread management.
    pub fn create_session_id(&self) -> String {
        format!("T-{}", Uuid::new_v4())
    }

    /// Execute a message using the Amp CLI.
    ///
    /// Returns a receiver for streaming events and a handle to the process.
    pub async fn execute_message(
        &self,
        working_dir: &str,
        message: &str,
        model: Option<&str>,
        mode: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(mpsc::Receiver<AmpEvent>, AmpProcessHandle)> {
        let cli_path = self
            .config
            .cli_path
            .clone()
            .unwrap_or_else(|| "amp".to_string());

        let mut cmd = Command::new(&cli_path);
        cmd.current_dir(working_dir);

        // Core flags for headless execution
        cmd.arg("--execute");
        cmd.arg("--stream-json");
        cmd.arg("--dangerously-allow-all"); // Skip permission prompts

        // Optional mode (smart, rush)
        if let Some(m) = mode.or(self.config.default_mode.as_deref()) {
            cmd.arg("--mode");
            cmd.arg(m);
        }

        // The message is passed as the final argument
        cmd.arg(message);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        debug!(
            cli_path = %cli_path,
            working_dir = %working_dir,
            session_id = ?session_id,
            "Starting Amp CLI process"
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow!(
                "Failed to spawn Amp CLI at '{}': {}. Is Amp installed?",
                cli_path,
                e
            )
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow!("Failed to capture Amp stdout")
        })?;

        let stderr = child.stderr.take();

        let child_arc = Arc::new(Mutex::new(Some(child)));
        let child_for_task = Arc::clone(&child_arc);

        let (tx, rx) = mpsc::channel(256);

        // Spawn stderr reader for debugging
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        debug!(stderr = %line, "Amp CLI stderr");
                    }
                }
            });
        }

        // Spawn stdout reader for events
        let task_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                match serde_json::from_str::<AmpEvent>(&line) {
                    Ok(event) => {
                        if tx.send(event).await.is_err() {
                            debug!("Amp event receiver dropped");
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            line = %if line.len() > 200 { &line[..200] } else { &line },
                            "Failed to parse Amp event"
                        );
                    }
                }
            }

            // Wait for child to finish
            if let Some(mut child) = child_for_task.lock().await.take() {
                let _ = child.wait().await;
            }
        });

        Ok((rx, AmpProcessHandle::new(child_arc, task_handle)))
    }

    /// Continue an existing thread with a new message.
    pub async fn continue_thread(
        &self,
        working_dir: &str,
        thread_id: &str,
        message: &str,
        mode: Option<&str>,
    ) -> Result<(mpsc::Receiver<AmpEvent>, AmpProcessHandle)> {
        let cli_path = self
            .config
            .cli_path
            .clone()
            .unwrap_or_else(|| "amp".to_string());

        let mut cmd = Command::new(&cli_path);
        cmd.current_dir(working_dir);

        // Use threads continue subcommand
        cmd.arg("threads");
        cmd.arg("continue");
        cmd.arg(thread_id);

        // Core flags
        cmd.arg("--execute");
        cmd.arg("--stream-json");
        cmd.arg("--dangerously-allow-all");

        // Optional mode
        if let Some(m) = mode.or(self.config.default_mode.as_deref()) {
            cmd.arg("--mode");
            cmd.arg(m);
        }

        // Message
        cmd.arg(message);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        debug!(
            cli_path = %cli_path,
            working_dir = %working_dir,
            thread_id = %thread_id,
            "Continuing Amp thread"
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow!("Failed to spawn Amp CLI: {}", e)
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow!("Failed to capture Amp stdout")
        })?;

        let stderr = child.stderr.take();

        let child_arc = Arc::new(Mutex::new(Some(child)));
        let child_for_task = Arc::clone(&child_arc);

        let (tx, rx) = mpsc::channel(256);

        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        debug!(stderr = %line, "Amp CLI stderr");
                    }
                }
            });
        }

        let task_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                match serde_json::from_str::<AmpEvent>(&line) {
                    Ok(event) => {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse Amp event");
                    }
                }
            }

            if let Some(mut child) = child_for_task.lock().await.take() {
                let _ = child.wait().await;
            }
        });

        Ok((rx, AmpProcessHandle::new(child_arc, task_handle)))
    }
}

impl Default for AmpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_system_event() {
        let json = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"T-123","tools":["Bash"],"mcp_servers":[]}"#;
        let event: AmpEvent = serde_json::from_str(json).unwrap();
        match event {
            AmpEvent::System(sys) => {
                assert_eq!(sys.subtype, "init");
                assert_eq!(sys.session_id, "T-123");
            }
            _ => panic!("Expected System event"),
        }
    }

    #[test]
    fn test_parse_assistant_event() {
        let json = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn"},"session_id":"T-123"}"#;
        let event: AmpEvent = serde_json::from_str(json).unwrap();
        match event {
            AmpEvent::Assistant(evt) => {
                assert_eq!(evt.message.content.len(), 1);
            }
            _ => panic!("Expected Assistant event"),
        }
    }

    #[test]
    fn test_parse_result_event() {
        let json = r#"{"type":"result","subtype":"success","duration_ms":2906,"is_error":false,"num_turns":1,"result":"4","session_id":"T-123"}"#;
        let event: AmpEvent = serde_json::from_str(json).unwrap();
        match event {
            AmpEvent::Result(res) => {
                assert_eq!(res.subtype, "success");
                assert!(!res.is_error);
            }
            _ => panic!("Expected Result event"),
        }
    }
}
