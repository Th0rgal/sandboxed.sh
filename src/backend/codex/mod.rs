pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{CodexClient, CodexConfig, CodexEvent};

/// Codex backend that spawns the Codex CLI for mission execution.
pub struct CodexBackend {
    id: String,
    name: String,
    config: Arc<RwLock<CodexConfig>>,
}

impl CodexBackend {
    pub fn new() -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(CodexConfig::default())),
        }
    }

    pub fn with_config(config: CodexConfig) -> Self {
        Self {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: CodexConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> CodexConfig {
        self.config.read().await.clone()
    }
}

impl Default for CodexBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Codex doesn't have separate agent types like Claude Code
        // Return a single general-purpose agent
        Ok(vec![AgentInfo {
            id: "default".to_string(),
            name: "Codex Agent".to_string(),
        }])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let client = CodexClient::new();
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
        let client = CodexClient::with_config(config);

        let (mut codex_rx, codex_handle) = client
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
            // Track last seen content for each item to avoid duplication on ItemUpdated
            let mut item_content_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            'outer: while let Some(event) = codex_rx.recv().await {
                let exec_events = convert_codex_event(event, &mut item_content_cache);

                for exec_event in exec_events {
                    if tx.send(exec_event).await.is_err() {
                        debug!("ExecutionEvent receiver dropped");
                        break 'outer;
                    }
                }
            }

            // Ensure MessageComplete is sent
            let _ = tx
                .send(ExecutionEvent::MessageComplete {
                    session_id: session_id.clone(),
                })
                .await;

            // Drop the codex handle to clean up
            drop(codex_handle);
        });

        Ok((rx, handle))
    }
}

/// Convert a Codex event to backend-agnostic ExecutionEvents.
/// The cache parameter tracks last seen content for each item to avoid duplication on ItemUpdated.
fn convert_codex_event(
    event: CodexEvent,
    item_content_cache: &mut std::collections::HashMap<String, String>,
) -> Vec<ExecutionEvent> {
    let mut results = vec![];

    match event {
        CodexEvent::ThreadStarted { thread_id } => {
            debug!("Codex thread started: thread_id={}", thread_id);
        }

        CodexEvent::TurnStarted => {
            debug!("Codex turn started");
        }

        CodexEvent::TurnCompleted { summary } => {
            if let Some(summary_text) = summary {
                debug!("Codex turn completed: {}", summary_text);
            } else {
                debug!("Codex turn completed");
            }
        }

        CodexEvent::TurnFailed { error } => {
            results.push(ExecutionEvent::Error {
                message: error.message,
            });
        }

        CodexEvent::ItemCreated { item } | CodexEvent::ItemUpdated { item } => {
            // Handle different item types
            match item.item_type.as_str() {
                "message" => {
                    // Extract message content
                    if let Some(content) = item.data.get("content") {
                        if let Some(text) = content.as_str() {
                            if !text.is_empty() {
                                // Check if this is new content or an update
                                let last_content = item_content_cache.get(&item.id);
                                let new_content = if let Some(last) = last_content {
                                    // ItemUpdated: only emit the delta (new text)
                                    if text.starts_with(last) {
                                        text[last.len()..].to_string()
                                    } else {
                                        // Content was replaced, emit full text
                                        text.to_string()
                                    }
                                } else {
                                    // ItemCreated: emit full text
                                    text.to_string()
                                };

                                if !new_content.is_empty() {
                                    results.push(ExecutionEvent::TextDelta {
                                        content: new_content,
                                    });
                                }

                                // Update cache with current full content
                                item_content_cache.insert(item.id.clone(), text.to_string());
                            }
                        }
                    }
                }
                "reasoning" | "thinking" => {
                    // Extract thinking/reasoning content
                    if let Some(content) = item.data.get("content") {
                        if let Some(text) = content.as_str() {
                            if !text.is_empty() {
                                results.push(ExecutionEvent::Thinking {
                                    content: text.to_string(),
                                });
                            }
                        }
                    }
                }
                "command" | "tool" => {
                    // Extract tool/command execution
                    if let Some(name) = item.data.get("name").and_then(|v| v.as_str()) {
                        if let Some(args) = item.data.get("args") {
                            results.push(ExecutionEvent::ToolCall {
                                id: item.id.clone(),
                                name: name.to_string(),
                                args: args.clone(),
                            });
                        }
                    }
                }
                _ => {
                    debug!("Unknown Codex item type: {}", item.item_type);
                }
            }
        }

        CodexEvent::ItemCompleted { item } => {
            // Extract tool result if available
            if item.item_type == "command" || item.item_type == "tool" {
                if let Some(result) = item.data.get("result") {
                    if let Some(name) = item.data.get("name").and_then(|v| v.as_str()) {
                        results.push(ExecutionEvent::ToolResult {
                            id: item.id.clone(),
                            name: name.to_string(),
                            result: result.clone(),
                        });
                    }
                }
            }
        }

        CodexEvent::Error { message } => {
            results.push(ExecutionEvent::Error { message });
        }

        CodexEvent::Unknown => {
            debug!("Unknown Codex event type");
        }
    }

    results
}

/// Create a registry entry for the Codex backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(CodexBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_agents() {
        let backend = CodexBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "default");
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = CodexBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                agent: None,
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.directory, "/tmp");
    }
}
