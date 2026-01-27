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

use client::{AmpClient, AmpConfig};

/// Amp backend that spawns the Amp CLI for mission execution.
pub struct AmpBackend {
    id: String,
    name: String,
    config: Arc<RwLock<AmpConfig>>,
}

impl AmpBackend {
    pub fn new() -> Self {
        Self {
            id: "amp".to_string(),
            name: "Amp".to_string(),
            config: Arc::new(RwLock::new(AmpConfig::default())),
        }
    }

    pub fn with_config(config: AmpConfig) -> Self {
        Self {
            id: "amp".to_string(),
            name: "Amp".to_string(),
            config: Arc::new(RwLock::new(config)),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: AmpConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> AmpConfig {
        self.config.read().await.clone()
    }
}

impl Default for AmpBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for AmpBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Amp has built-in modes rather than agents
        Ok(vec![
            AgentInfo {
                id: "smart".to_string(),
                name: "Smart Mode".to_string(),
            },
            AgentInfo {
                id: "rush".to_string(),
                name: "Rush Mode".to_string(),
            },
        ])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let client = AmpClient::new();
        Ok(Session {
            id: client.create_session_id(),
            directory: config.directory,
            model: config.model,
            agent: config.agent, // Used as "mode" for Amp
        })
    }

    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
        let config = self.config.read().await.clone();
        let client = AmpClient::with_config(config);

        let (mut amp_rx, amp_handle) = client
            .execute_message(
                &session.directory,
                message,
                session.model.as_deref(),
                session.agent.as_deref(), // mode
                Some(&session.id),
            )
            .await?;

        let (tx, rx) = mpsc::channel(256);
        let session_id = session.id.clone();

        // Spawn event conversion task
        let handle = tokio::spawn(async move {
            let mut pending_tools: HashMap<String, String> = HashMap::new();

            while let Some(event) = amp_rx.recv().await {
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

            drop(amp_handle);
        });

        Ok((rx, handle))
    }
}

/// Create a registry entry for the Amp backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(AmpBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_agents() {
        let backend = AmpBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert!(agents.len() >= 2);
        assert!(agents.iter().any(|a| a.id == "smart"));
        assert!(agents.iter().any(|a| a.id == "rush"));
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = AmpBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: None,
                agent: Some("smart".to_string()),
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert!(session.id.starts_with("T-"));
        assert_eq!(session.directory, "/tmp");
    }
}
