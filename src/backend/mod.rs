pub mod amp;
pub mod claudecode;
pub mod events;
pub mod opencode;
pub mod registry;
pub mod shared;

use anyhow::Error;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use events::ExecutionEvent;

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub directory: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub directory: String,
    pub model: Option<String>,
    pub agent: Option<String>,
}

#[async_trait]
pub trait Backend: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error>;
    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error>;
    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error>;
}
