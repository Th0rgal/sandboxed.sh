mod client;

use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use serde_json::Value;
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

    async fn fetch_agents(&self) -> Result<Value, Error> {
        let base_url = self.client.base_url().trim_end_matches('/');
        if base_url.is_empty() {
            return Err(anyhow!("OpenCode base URL is not configured"));
        }
        let url = format!("{}/agent", base_url);
        let resp = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .context("Failed to call OpenCode /agent")?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("OpenCode /agent failed: {}", text));
        }
        resp.json::<Value>()
            .await
            .context("Failed to parse OpenCode agent payload")
    }

    fn parse_agents(payload: Value) -> Vec<AgentInfo> {
        let raw = match payload {
            Value::Array(arr) => arr,
            Value::Object(mut obj) => obj
                .remove("agents")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        raw.into_iter()
            .filter_map(|entry| match entry {
                Value::String(name) => Some(AgentInfo {
                    id: name.clone(),
                    name,
                }),
                Value::Object(mut obj) => {
                    let name = obj
                        .remove("name")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .or_else(|| {
                            obj.remove("id")
                                .and_then(|v| v.as_str().map(|s| s.to_string()))
                        });
                    name.map(|name| AgentInfo {
                        id: name.clone(),
                        name,
                    })
                }
                _ => None,
            })
            .collect()
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

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        match self.fetch_agents().await {
            Ok(payload) => Ok(Self::parse_agents(payload)),
            Err(err) => {
                tracing::warn!(
                    "OpenCode /agent unavailable ({}). Returning empty agent list.",
                    err
                );
                Ok(vec![])
            }
        }
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
