//! Codex backend configuration.
//!
//! Historically this file housed the `codex exec` shell-out client. Path A
//! (PR #403) replaced that with the `codex app-server` JSON-RPC client in
//! `app_server.rs`. Only the configuration struct survives here; everything
//! else moved or was deleted.

/// Configuration for the Codex backend.
///
/// As of Path A (PR #403), all codex missions run through the
/// `codex app-server` JSON-RPC protocol — the legacy `codex exec` path
/// is removed because it doesn't parse slash commands and never arms
/// codex's goals.rs runtime.
#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub cli_path: String,
    pub default_model: Option<String>,
    pub model_effort: Option<String>,
    /// Enable Codex's higher-throughput ChatGPT credit tier for supported models.
    pub fast_mode: bool,
    /// ChatGPT OAuth account supplied by the host app. When set, the app-server
    /// uses external `chatgptAuthTokens` mode and asks the host to refresh.
    pub external_chatgpt_auth: Option<CodexExternalChatgptAuth>,
    /// Optional cancellation signal from mission_runner. The app-server task
    /// observes it directly so goal-mode cancellation can call
    /// `thread/goal/clear` against the live thread before shutdown.
    pub cancel_token: Option<tokio_util::sync::CancellationToken>,
    /// Extra environment variables exported to the codex app-server process
    /// (e.g. the per-mission DGX Spark offload vars). Empty by default.
    pub extra_env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct CodexExternalChatgptAuth {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
    pub working_dir: std::path::PathBuf,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            cli_path: std::env::var("CODEX_CLI_PATH").unwrap_or_else(|_| "codex".to_string()),
            default_model: None,
            model_effort: None,
            fast_mode: false,
            external_chatgpt_auth: None,
            cancel_token: None,
            extra_env: std::collections::HashMap::new(),
        }
    }
}
