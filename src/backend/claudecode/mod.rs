pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::backend::events::ExecutionEvent;
use crate::backend::shared::convert_cli_event;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{ClaudeCodeClient, ClaudeCodeConfig};

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
                let exec_events = convert_cli_event(event, &mut pending_tools);

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
