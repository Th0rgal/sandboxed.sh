use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

/// Grok Build backend that launches the `grok` CLI for mission execution.
pub struct GrokBackend {
    id: String,
    name: String,
}

impl GrokBackend {
    pub fn new() -> Self {
        Self {
            id: "grok".to_string(),
            name: "Grok Build".to_string(),
        }
    }
}

impl Default for GrokBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for GrokBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn cli_names(&self) -> &'static [&'static str] {
        &["grok"]
    }

    async fn check_auth_configured(&self, ctx: &crate::backend::AuthContext<'_>) -> Option<bool> {
        if std::env::var("GROK_CODE_XAI_API_KEY")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
            || std::env::var("XAI_API_KEY")
                .ok()
                .is_some_and(|v| !v.trim().is_empty())
            || crate::api::ai_providers::get_xai_api_key_for_grok(ctx.working_dir).is_some()
        {
            return Some(true);
        }

        // Grok Build can also authenticate with `grok login`/first-launch X
        // OAuth, and that cache is owned by the CLI. Do not hide the backend
        // just because Sandboxed.sh cannot inspect that token yet.
        None
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        Ok(vec![
            AgentInfo {
                id: "build".to_string(),
                name: "Build".to_string(),
            },
            AgentInfo {
                id: "plan".to_string(),
                name: "Plan".to_string(),
            },
        ])
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
        anyhow::bail!("Grok Build streaming is handled by the mission runner")
    }
}

pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(GrokBackend::new())
}
