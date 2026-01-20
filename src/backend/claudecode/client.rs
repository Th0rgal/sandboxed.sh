use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Handle to a running Claude CLI process.
/// Call `kill()` to terminate the process when cancelling a mission.
pub struct ClaudeProcessHandle {
    child: Arc<Mutex<Option<Child>>>,
    _task_handle: tokio::task::JoinHandle<()>,
}

impl ClaudeProcessHandle {
    /// Kill the underlying CLI process.
    pub async fn kill(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            if let Err(e) = child.kill().await {
                warn!("Failed to kill Claude CLI process: {}", e);
            } else {
                info!("Claude CLI process killed");
            }
        }
    }
}

/// Events emitted by the Claude CLI in stream-json mode.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeEvent {
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
    pub agents: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
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
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserEvent {
    pub message: UserMessage,
    pub session_id: String,
    #[serde(default)]
    pub tool_use_result: Option<ToolUseResultExtra>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolUseResultExtra {
    #[serde(default)]
    pub stdout: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub interrupted: bool,
    #[serde(default, rename = "isImage")]
    pub is_image: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    #[serde(default)]
    pub result: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u32>,
}

/// Configuration for the Claude Code client.
#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    pub cli_path: String,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            cli_path: std::env::var("CLAUDE_CLI_PATH").unwrap_or_else(|_| "claude".to_string()),
            api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            default_model: None,
        }
    }
}

/// Client for communicating with the Claude CLI.
pub struct ClaudeCodeClient {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeClient {
    pub fn new() -> Self {
        Self {
            config: ClaudeCodeConfig::default(),
        }
    }

    pub fn with_config(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }

    pub fn create_session_id(&self) -> String {
        Uuid::new_v4().to_string()
    }

    /// Execute a message and return a stream of events.
    /// Returns a tuple of (event receiver, process handle).
    /// Call `process_handle.kill()` to terminate the process on cancellation.
    pub async fn execute_message(
        &self,
        directory: &str,
        message: &str,
        model: Option<&str>,
        session_id: Option<&str>,
        agent: Option<&str>,
    ) -> Result<(mpsc::Receiver<ClaudeEvent>, ClaudeProcessHandle)> {
        let (tx, rx) = mpsc::channel(256);

        let mut cmd = Command::new(&self.config.cli_path);
        cmd.current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--include-partial-messages");
        // Note: --dangerously-skip-permissions cannot be used when running as root

        // Set API key or OAuth token if configured
        // OAuth tokens start with "sk-ant-oat" and must use CLAUDE_CODE_OAUTH_TOKEN
        // API keys start with "sk-ant-api" and use ANTHROPIC_API_KEY
        if let Some(ref key) = self.config.api_key {
            if key.starts_with("sk-ant-oat") {
                // OAuth access token
                cmd.env("CLAUDE_CODE_OAUTH_TOKEN", key);
                debug!("Using OAuth token for Claude CLI authentication");
            } else {
                // Regular API key
                cmd.env("ANTHROPIC_API_KEY", key);
                debug!("Using API key for Claude CLI authentication");
            }
        }

        // Model selection
        let effective_model = model.or(self.config.default_model.as_deref());
        if let Some(m) = effective_model {
            cmd.arg("--model").arg(m);
        }

        // Session ID for continuity
        if let Some(sid) = session_id {
            cmd.arg("--session-id").arg(sid);
        }

        // Agent selection
        if let Some(a) = agent {
            cmd.arg("--agent").arg(a);
        }

        info!(
            "Spawning Claude CLI: directory={}, model={:?}, session_id={:?}, agent={:?}",
            directory, effective_model, session_id, agent
        );

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn Claude CLI: {}", e);
            anyhow!(
                "Failed to spawn Claude CLI: {}. Is it installed at '{}'?",
                e,
                self.config.cli_path
            )
        })?;

        // Write message to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let msg = message.to_string();
            tokio::spawn(async move {
                if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                    error!("Failed to write to Claude stdin: {}", e);
                }
                // Close stdin to signal end of input
                drop(stdin);
            });
        }

        // Spawn task to read stdout and parse events
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture Claude stdout"))?;

        // Wrap child in Arc<Mutex> so it can be killed from outside the task
        let child_handle = Arc::new(Mutex::new(Some(child)));
        let child_for_task = Arc::clone(&child_handle);

        let task_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }

                match serde_json::from_str::<ClaudeEvent>(&line) {
                    Ok(event) => {
                        debug!("Claude event: {:?}", event);
                        if tx.send(event).await.is_err() {
                            debug!("Receiver dropped, stopping Claude event stream");
                            break;
                        }
                    }
                    Err(e) => {
                        // Log but don't fail - some lines might be non-JSON
                        warn!(
                            "Failed to parse Claude event: {} - line: {}",
                            e,
                            if line.len() > 200 {
                                format!("{}...", &line[..200])
                            } else {
                                line.clone()
                            }
                        );
                    }
                }
            }

            // Wait for process to finish (if it wasn't killed)
            if let Some(mut child) = child_for_task.lock().await.take() {
                match child.wait().await {
                    Ok(status) => {
                        if !status.success() {
                            warn!("Claude CLI exited with status: {}", status);
                        } else {
                            debug!("Claude CLI exited successfully");
                        }
                    }
                    Err(e) => {
                        error!("Failed to wait for Claude CLI: {}", e);
                    }
                }
            }
        });

        let process_handle = ClaudeProcessHandle {
            child: child_handle,
            _task_handle: task_handle,
        };

        Ok((rx, process_handle))
    }

    /// Get available agents from the Claude CLI.
    pub async fn list_agents(&self) -> Result<Vec<String>> {
        // Claude Code has built-in agents that are always available
        // These are discovered from the init event, but we can provide defaults
        Ok(vec![
            "general-purpose".to_string(),
            "Bash".to_string(),
            "Explore".to_string(),
            "Plan".to_string(),
        ])
    }
}

impl Default for ClaudeCodeClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_system_event() {
        let json = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"abc123","tools":["Bash","Read"],"model":"claude-sonnet-4-20250514","agents":["general-purpose","Bash"]}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::System(sys) => {
                assert_eq!(sys.subtype, "init");
                assert_eq!(sys.session_id, "abc123");
                assert_eq!(sys.agents.len(), 2);
            }
            _ => panic!("Expected System event"),
        }
    }

    #[test]
    fn test_parse_stream_event_delta() {
        let json = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}},"session_id":"abc123"}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::StreamEvent(wrapper) => {
                assert_eq!(wrapper.session_id, "abc123");
                match wrapper.event {
                    StreamEvent::ContentBlockDelta { delta, .. } => {
                        assert_eq!(delta.text, Some("Hello".to_string()));
                    }
                    _ => panic!("Expected ContentBlockDelta"),
                }
            }
            _ => panic!("Expected StreamEvent"),
        }
    }

    #[test]
    fn test_parse_assistant_with_tool_use() {
        let json = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_123","name":"Bash","input":{"command":"ls"}}],"stop_reason":"tool_use"},"session_id":"abc123"}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::Assistant(evt) => {
                assert_eq!(evt.message.stop_reason, Some("tool_use".to_string()));
                assert_eq!(evt.message.content.len(), 1);
                match &evt.message.content[0] {
                    ContentBlock::ToolUse { id, name, .. } => {
                        assert_eq!(id, "toolu_123");
                        assert_eq!(name, "Bash");
                    }
                    _ => panic!("Expected ToolUse content"),
                }
            }
            _ => panic!("Expected Assistant event"),
        }
    }

    #[test]
    fn test_parse_result_event() {
        let json = r#"{"type":"result","subtype":"success","result":"Done","session_id":"abc123","is_error":false,"total_cost_usd":0.05}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::Result(res) => {
                assert_eq!(res.subtype, "success");
                assert_eq!(res.result, Some("Done".to_string()));
                assert!(!res.is_error);
                assert_eq!(res.total_cost_usd, Some(0.05));
            }
            _ => panic!("Expected Result event"),
        }
    }
}
