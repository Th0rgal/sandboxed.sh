pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::backend::events::ExecutionEvent;
use crate::backend::shared::convert_cli_event;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{ClaudeCodeClient, ClaudeCodeConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaudeStreamEndState {
    Complete,
    MissingTerminalResult,
    MissingTerminalResultWithPendingTools { pending_tool_names: Vec<String> },
}

fn classify_claude_stream_end(
    saw_terminal_result: bool,
    pending_tools: &HashMap<String, String>,
) -> ClaudeStreamEndState {
    if !pending_tools.is_empty() {
        let mut pending_tool_names: Vec<String> = pending_tools.values().cloned().collect();
        pending_tool_names.sort();
        pending_tool_names.dedup();
        return ClaudeStreamEndState::MissingTerminalResultWithPendingTools { pending_tool_names };
    }

    if saw_terminal_result {
        ClaudeStreamEndState::Complete
    } else {
        ClaudeStreamEndState::MissingTerminalResult
    }
}

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

    fn cli_names(&self) -> &'static [&'static str] {
        &["claude"]
    }

    async fn check_auth_configured(&self, ctx: &crate::backend::AuthContext<'_>) -> Option<bool> {
        // Claude Code keeps its api_key in the secrets store under the
        // "claudecode" namespace. A non-expired entry means we have auth.
        let Some(store) = ctx.secrets else {
            return Some(false);
        };
        let has_key = match store.list_secrets("claudecode").await {
            Ok(secrets) => secrets.iter().any(|s| s.key == "api_key" && !s.is_expired),
            Err(_) => false,
        };
        Some(has_key)
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
            // Track pending tool calls for name lookup AND completion tracking
            let mut pending_tools: HashMap<String, String> = HashMap::new();
            let mut saw_terminal_result = false;

            while let Some(event) = claude_rx.recv().await {
                if matches!(event, client::ClaudeEvent::Result(_)) {
                    saw_terminal_result = true;
                }
                let exec_events = convert_cli_event(event, &mut pending_tools);

                for exec_event in exec_events {
                    // Track tool completion to know when it's safe to send MessageComplete
                    if let ExecutionEvent::ToolResult { id, .. } = &exec_event {
                        pending_tools.remove(id);
                        debug!(
                            "Tool completed: {}. Remaining pending: {}",
                            id,
                            pending_tools.len()
                        );
                    }

                    if tx.send(exec_event).await.is_err() {
                        debug!("ExecutionEvent receiver dropped");
                        break;
                    }
                }
            }

            match classify_claude_stream_end(saw_terminal_result, &pending_tools) {
                ClaudeStreamEndState::Complete => {
                    let _ = tx
                        .send(ExecutionEvent::MessageComplete {
                            session_id: session_id.clone(),
                        })
                        .await;
                }
                ClaudeStreamEndState::MissingTerminalResult => {
                    warn!(
                        "Claude CLI stream ended without a terminal result event; treating as transport failure"
                    );
                    let _ = tx
                        .send(ExecutionEvent::Error {
                            message:
                                "Claude CLI stream ended before emitting a terminal result event"
                                    .to_string(),
                        })
                        .await;
                }
                ClaudeStreamEndState::MissingTerminalResultWithPendingTools {
                    pending_tool_names,
                } => {
                    warn!(
                        pending_tools = ?pending_tool_names,
                        "Claude CLI stream ended with pending tools before a terminal result event"
                    );
                    let _ = tx
                        .send(ExecutionEvent::Error {
                            message: format!(
                                "Claude CLI stream ended with pending tools before a terminal result event: {}",
                                pending_tool_names.join(", ")
                            ),
                        })
                        .await;
                }
            }

            // Note: claude_handle is dropped here, but the process is managed
            // by the ProcessHandle which will clean up when dropped
            drop(claude_handle);
        });

        Ok((rx, handle))
    }
}

/// Create a registry entry for the Claude Code backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(ClaudeCodeBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

    #[test]
    fn classify_claude_stream_end_requires_terminal_result_even_without_tools() {
        let pending = HashMap::new();
        assert_eq!(
            classify_claude_stream_end(false, &pending),
            ClaudeStreamEndState::MissingTerminalResult
        );
    }

    #[test]
    fn classify_claude_stream_end_reports_pending_tools_before_terminal_result() {
        let mut pending = HashMap::new();
        pending.insert("toolu_1".to_string(), "Bash".to_string());
        pending.insert("toolu_2".to_string(), "Read".to_string());

        assert_eq!(
            classify_claude_stream_end(false, &pending),
            ClaudeStreamEndState::MissingTerminalResultWithPendingTools {
                pending_tool_names: vec!["Bash".to_string(), "Read".to_string()],
            }
        );
    }

    #[test]
    fn classify_claude_stream_end_allows_completion_after_terminal_result() {
        let pending = HashMap::new();
        assert_eq!(
            classify_claude_stream_end(true, &pending),
            ClaudeStreamEndState::Complete
        );
    }
}
