use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Handle to a running Amp CLI process.
/// Call `kill()` to terminate the process when cancelling a mission.
pub struct AmpProcessHandle {
    child: Arc<Mutex<Option<Child>>>,
    _task_handle: tokio::task::JoinHandle<()>,
}

impl AmpProcessHandle {
    /// Kill the underlying CLI process.
    pub async fn kill(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            if let Err(e) = child.kill().await {
                warn!("Failed to kill Amp CLI process: {}", e);
            } else {
                info!("Amp CLI process killed");
            }
        }
    }
}

/// Events emitted by the Amp CLI in stream-json mode.
/// Amp's format is Claude Code compatible with some extensions.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AmpEvent {
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "stream_event")]
    StreamEvent(StreamEventWrapper),
    #[serde(rename = "assistant")]
    Assistant(AssistantEvent),
    #[serde(rename = "user")]
    User(UserEvent),
    #[serde(rename = "result")]
    Result(ResultEvent),
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamEventWrapper {
    pub event: StreamEvent,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: Value },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockInfo,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: Delta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: Value, usage: Option<Value> },
    #[serde(rename = "message_stop")]
    MessageStop,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlockInfo {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
    /// Thinking content for thinking_delta events (extended thinking).
    #[serde(default)]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
}

/// Tool result content can be a string or an array of content items.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Items(Vec<ToolResultItem>),
}

impl ToolResultContent {
    /// Convert to a lossy string representation.
    pub fn to_string_lossy(&self) -> String {
        match self {
            ToolResultContent::Text(s) => s.clone(),
            ToolResultContent::Items(items) => {
                items
                    .iter()
                    .filter_map(|item| match item {
                        ToolResultItem::Text { text } => Some(text.clone()),
                        ToolResultItem::Image { .. } => Some("[image]".to_string()),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: Value },
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEvent {
    pub message: UserMessage,
    pub session_id: String,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResultInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolUseResultInfo {
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub interrupted: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub session_id: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub num_turns: Option<u32>,
    #[serde(default)]
    pub result: Option<String>,
}

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

        Ok((
            rx,
            AmpProcessHandle {
                child: child_arc,
                _task_handle: task_handle,
            },
        ))
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

        Ok((
            rx,
            AmpProcessHandle {
                child: child_arc,
                _task_handle: task_handle,
            },
        ))
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
