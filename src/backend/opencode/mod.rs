mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};
use client::OpenCodeClient;

pub struct OpenCodeBackend {
    id: String,
    name: String,
    client: OpenCodeClient,
}

impl OpenCodeBackend {
    pub fn new(base_url: String, default_agent: Option<String>, permissive: bool) -> Self {
        Self {
            id: "opencode".to_string(),
            name: "OpenCode".to_string(),
            client: OpenCodeClient::new(base_url, default_agent, permissive),
        }
    }

    pub fn client(&self) -> &OpenCodeClient {
        &self.client
    }
}

#[async_trait]
impl Backend for OpenCodeBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn cli_names(&self) -> &'static [&'static str] {
        &["opencode"]
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        Ok(["build", "plan"]
            .into_iter()
            .map(|name| AgentInfo {
                id: name.to_string(),
                name: name.to_string(),
            })
            .collect())
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let session = self
            .client
            .create_session(&config.directory, config.title.as_deref())
            .await?;
        Ok(Session {
            id: session.id,
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
        let (rx, handle) = self
            .client
            .send_message_streaming(
                &session.id,
                &session.directory,
                message,
                session.model.as_deref(),
                session.agent.as_deref(),
            )
            .await?;
        let join_handle = tokio::spawn(async move {
            let _ = handle.await;
        });
        Ok((rx, join_handle))
    }
}

pub fn registry_entry(
    base_url: String,
    default_agent: Option<String>,
    permissive: bool,
) -> Arc<dyn Backend> {
    Arc::new(OpenCodeBackend::new(base_url, default_agent, permissive))
}
