//! ChatGPT web-UI harness metadata.
//!
//! Mission execution lives in `api::runners::chatgpt_ui`; this registry entry
//! exposes configuration/auth diagnostics without ever reading profile data.

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

pub struct ChatGptUiBackend;

impl ChatGptUiBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChatGptUiBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for ChatGptUiBackend {
    fn id(&self) -> &str {
        "chatgpt_ui"
    }

    fn name(&self) -> &str {
        "ChatGPT UI (experimental)"
    }

    async fn check_auth_configured(&self, _ctx: &crate::backend::AuthContext<'_>) -> Option<bool> {
        // A directory does not prove that ChatGPT is authenticated. Verifying
        // the session requires launching the browser; the driver reports an
        // explicit auth_required diagnostic at turn start.
        None
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        Ok(vec![AgentInfo {
            id: "chat".to_string(),
            name: "ChatGPT web conversation".to_string(),
        }])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        Ok(Session {
            id: uuid::Uuid::new_v4().to_string(),
            directory: config.directory,
            model: config.model,
            agent: config.agent,
        })
    }

    async fn send_message_streaming(
        &self,
        _session: &Session,
        _message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error> {
        anyhow::bail!("ChatGPT UI streaming is handled by the mission runner")
    }
}

pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(ChatGptUiBackend::new())
}
