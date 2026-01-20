pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{ClaudeCodeClient, ClaudeCodeConfig, ClaudeEvent, ContentBlock, StreamEvent};

/// Claude Code backend that spawns the Claude CLI for mission execution.
pub struct ClaudeCodeBackend {
    id: String,
    name: String,
    config: Arc<RwLock<ClaudeCodeConfig>>,
}

impl ClaudeCodeBackend {
    pub fn new() -> Self {
        Self {
            id: "claudecode".to_string(),
            name: "Claude Code".to_string(),
            config: Arc::new(RwLock::new(ClaudeCodeConfig::default())),
        }
    }

    pub fn with_config(config: ClaudeCodeConfig) -> Self {
        Self {
            id: "claudecode".to_string(),
            name: "Claude Code".to_string(),
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: ClaudeCodeConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> ClaudeCodeConfig {
        self.config.read().await.clone()
    }
}

impl Default for ClaudeCodeBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for ClaudeCodeBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Claude Code has built-in agents
        Ok(vec![
            AgentInfo {
                id: "general-purpose".to_string(),
                name: "General Purpose".to_string(),
            },
            AgentInfo {
                id: "Bash".to_string(),
                name: "Bash Specialist".to_string(),
            },
            AgentInfo {
                id: "Explore".to_string(),
                name: "Codebase Explorer".to_string(),
            },
            AgentInfo {
                id: "Plan".to_string(),
                name: "Planner".to_string(),
            },
        ])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let client = ClaudeCodeClient::new();
        Ok(Session {
            id: client.create_session_id(),
            directory: config.directory,
            model: config.model,
            agent: config.agent,
        })
    }

    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
        let config = self.config.read().await.clone();
        let client = ClaudeCodeClient::with_config(config);

        let (mut claude_rx, claude_handle) = client
            .execute_message(
                &session.directory,
                message,
                session.model.as_deref(),
                Some(&session.id),
                session.agent.as_deref(),
            )
            .await?;

        let (tx, rx) = mpsc::channel(256);
        let session_id = session.id.clone();

        // Spawn event conversion task
        let handle = tokio::spawn(async move {
            // Track pending tool calls for name lookup
            let mut pending_tools: HashMap<String, String> = HashMap::new();

            while let Some(event) = claude_rx.recv().await {
                let exec_events = convert_claude_event(event, &mut pending_tools);

                for exec_event in exec_events {
                    if tx.send(exec_event).await.is_err() {
                        debug!("ExecutionEvent receiver dropped");
                        break;
                    }
                }
            }

            // Ensure MessageComplete is sent
            let _ = tx
                .send(ExecutionEvent::MessageComplete {
                    session_id: session_id.clone(),
                })
                .await;

            // Note: claude_handle is dropped here, but the process is managed
            // by the ClaudeProcessHandle which will clean up when dropped
            drop(claude_handle);
        });

        Ok((rx, handle))
    }
}

/// Convert a Claude CLI event to one or more ExecutionEvents.
fn convert_claude_event(
    event: ClaudeEvent,
    pending_tools: &mut HashMap<String, String>,
) -> Vec<ExecutionEvent> {
    let mut results = vec![];

    match event {
        ClaudeEvent::System(sys) => {
            debug!(
                "Claude session initialized: session_id={}, model={:?}, agents={:?}",
                sys.session_id, sys.model, sys.agents
            );
            // System init doesn't map to an ExecutionEvent
        }

        ClaudeEvent::StreamEvent(wrapper) => {
            match wrapper.event {
                StreamEvent::ContentBlockDelta { delta, .. } => {
                    // Text streaming
                    if let Some(text) = delta.text {
                        if !text.is_empty() {
                            results.push(ExecutionEvent::TextDelta { content: text });
                        }
                    }
                    // Tool input streaming (partial JSON)
                    if let Some(partial) = delta.partial_json {
                        debug!("Tool input delta: {}", partial);
                    }
                }
                StreamEvent::ContentBlockStart { content_block, .. } => {
                    // Track tool use starts
                    if content_block.block_type == "tool_use" {
                        if let (Some(id), Some(name)) = (content_block.id, content_block.name) {
                            pending_tools.insert(id, name);
                        }
                    }
                }
                _ => {
                    // Other stream events (message_start, message_stop, etc.)
                }
            }
        }

        ClaudeEvent::Assistant(evt) => {
            for block in evt.message.content {
                match block {
                    ContentBlock::Text { text } => {
                        // Complete text block - emit as thinking
                        if !text.is_empty() {
                            results.push(ExecutionEvent::Thinking { content: text });
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        // Track tool for result mapping
                        pending_tools.insert(id.clone(), name.clone());
                        results.push(ExecutionEvent::ToolCall {
                            id,
                            name,
                            args: input,
                        });
                    }
                    ContentBlock::Thinking { thinking } => {
                        if !thinking.is_empty() {
                            results.push(ExecutionEvent::Thinking { content: thinking });
                        }
                    }
                    ContentBlock::ToolResult { .. } => {
                        // Tool results in assistant messages are unusual
                    }
                }
            }
        }

        ClaudeEvent::User(evt) => {
            // User events contain tool results
            for block in evt.message.content {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = block
                {
                    // Look up tool name
                    let name = pending_tools
                        .get(&tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());

                    // Include extra result info if available
                    let result_value = if let Some(ref extra) = evt.tool_use_result {
                        serde_json::json!({
                            "content": content,
                            "stdout": extra.stdout,
                            "stderr": extra.stderr,
                            "is_error": is_error,
                            "interrupted": extra.interrupted,
                        })
                    } else {
                        Value::String(content)
                    };

                    results.push(ExecutionEvent::ToolResult {
                        id: tool_use_id,
                        name,
                        result: result_value,
                    });
                }
            }
        }

        ClaudeEvent::Result(res) => {
            if res.is_error || res.subtype == "error" {
                results.push(ExecutionEvent::Error {
                    message: res.result.unwrap_or_else(|| "Unknown error".to_string()),
                });
            } else {
                debug!(
                    "Claude result: subtype={}, cost={:?}, duration={:?}ms",
                    res.subtype, res.total_cost_usd, res.duration_ms
                );
            }
            // MessageComplete is sent after the loop
        }
    }

    results
}

/// Create a registry entry for the Claude Code backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(ClaudeCodeBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_agents() {
        let backend = ClaudeCodeBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert!(agents.len() >= 4);
        assert!(agents.iter().any(|a| a.id == "general-purpose"));
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = ClaudeCodeBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: Some("claude-sonnet-4-20250514".to_string()),
                agent: None,
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.directory, "/tmp");
    }
}
