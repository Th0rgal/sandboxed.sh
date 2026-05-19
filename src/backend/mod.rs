pub mod claudecode;
pub mod codex;
pub mod events;
pub mod gemini;
pub mod grok;
pub mod native_loops;
pub mod opencode;
pub mod registry;
pub mod shared;

use std::path::Path;

use anyhow::Error;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use events::ExecutionEvent;

/// Read-only context passed to `Backend::check_auth_configured` so each
/// backend can decide if its credentials are present without taking a
/// dependency on `AppState`.
pub struct AuthContext<'a> {
    /// Server working directory (root of `.sandboxed-sh/`).
    pub working_dir: &'a Path,
    /// Persisted backend settings JSON (may carry an `api_key` field, etc.).
    pub settings: &'a serde_json::Value,
    /// Optional reference to the secrets store, when one is configured.
    pub secrets: Option<&'a crate::secrets::SecretsStore>,
}

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
    /// CLI binary names this backend can launch, in preference order.
    /// A persisted `cli_path` setting overrides the first entry.
    /// Default empty = no CLI probe required.
    fn cli_names(&self) -> &'static [&'static str] {
        &[]
    }
    /// Whether the backend has usable credentials for the supplied context.
    /// `None` means "not applicable / no check defined"; the API surfaces this
    /// by omitting the field from the response.
    async fn check_auth_configured(&self, _ctx: &AuthContext<'_>) -> Option<bool> {
        None
    }
    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error>;
    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error>;
    async fn send_message_streaming(
        &self,
        session: &Session,
        message: &str,
    ) -> Result<(mpsc::Receiver<ExecutionEvent>, JoinHandle<()>), Error>;
}
