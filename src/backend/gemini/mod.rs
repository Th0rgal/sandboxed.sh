pub mod client;

use anyhow::Error;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::debug;

use std::collections::HashMap;

use crate::backend::events::ExecutionEvent;
use crate::backend::{AgentInfo, Backend, Session, SessionConfig};

use client::{GeminiClient, GeminiConfig, GeminiEvent};

/// Check whether Gemini has any usable credentials available.
///
/// Mirrors the precedence used by `get_google_credentials_for_gemini` in
/// `mission_runner.rs` but returns only a boolean: env vars, the AI provider
/// store (Google provider targeting the gemini backend with an API key),
/// the sandboxed-sh credentials store, and OpenCode's auth.json.
pub fn check_gemini_auth_configured(working_dir: &std::path::Path) -> bool {
    // 1. Environment variables
    for var in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_GENERATIVE_AI_API_KEY",
    ] {
        if let Ok(key) = std::env::var(var) {
            if !key.trim().is_empty() {
                return true;
            }
        }
    }

    // 2. AI provider store: Google provider targeting "gemini" with a non-empty api_key
    if crate::api::ai_providers::provider_targets_backend(
        working_dir,
        crate::ai_providers::ProviderType::Google,
        "gemini",
    ) {
        let store_path = working_dir.join(crate::util::AI_PROVIDERS_PATH);
        if let Ok(store) = std::fs::read_to_string(&store_path) {
            if let Ok(providers) = serde_json::from_str::<serde_json::Value>(&store) {
                if let Some(arr) = providers.as_array() {
                    for provider in arr {
                        if provider.get("provider_type").and_then(|v| v.as_str()) != Some("google")
                        {
                            continue;
                        }
                        let enabled = provider
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        if !enabled {
                            continue;
                        }
                        if let Some(key) = provider.get("api_key").and_then(|v| v.as_str()) {
                            if !key.is_empty() {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Sandboxed-sh credentials store (Google OAuth)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let candidates = [
        std::path::PathBuf::from(&home)
            .join(".sandboxed-sh")
            .join("credentials.json"),
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".sandboxed-sh")
            .join("credentials.json"),
    ];
    if let Some(creds_path) = candidates.iter().find(|p| p.exists()) {
        if let Ok(contents) = std::fs::read_to_string(creds_path) {
            if let Ok(auth) = serde_json::from_str::<serde_json::Value>(&contents) {
                for key_name in ["google", "gemini"] {
                    if let Some(entry) = auth.get(key_name) {
                        let access = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
                        let refresh = entry.get("refresh").and_then(|v| v.as_str()).unwrap_or("");
                        if !access.is_empty() && !refresh.is_empty() {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // 4. OpenCode's auth.json (API key or OAuth)
    let mut opencode_candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        opencode_candidates.push(
            std::path::PathBuf::from(data_home)
                .join("opencode")
                .join("auth.json"),
        );
    }
    opencode_candidates.push(
        std::path::PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    opencode_candidates.push(
        std::path::PathBuf::from("/var/lib/opencode")
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    if let Some(auth_path) = opencode_candidates.iter().find(|p| p.exists()) {
        if let Ok(contents) = std::fs::read_to_string(auth_path) {
            if let Ok(auth) = serde_json::from_str::<serde_json::Value>(&contents) {
                for key_name in ["google", "gemini"] {
                    if let Some(entry) = auth.get(key_name) {
                        for field in ["key", "api_key"] {
                            if let Some(key) = entry.get(field).and_then(|v| v.as_str()) {
                                if !key.is_empty()
                                    && entry.get("type").and_then(|v| v.as_str()) != Some("oauth")
                                {
                                    return true;
                                }
                            }
                        }
                        let access = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
                        let refresh = entry.get("refresh").and_then(|v| v.as_str()).unwrap_or("");
                        if !access.is_empty() && !refresh.is_empty() {
                            return true;
                        }
                    }
                }
            }
        }
    }

    false
}

/// Gemini CLI backend that spawns the Gemini CLI for mission execution.
pub struct GeminiBackend {
    id: String,
    name: String,
    config: Arc<RwLock<GeminiConfig>>,
    workspace_exec: Option<crate::workspace_exec::WorkspaceExec>,
    /// Handle to the most recently spawned child process, used for kill-on-cancel.
    #[allow(clippy::type_complexity)]
    last_child:
        Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>>>>,
}

impl GeminiBackend {
    pub fn new() -> Self {
        Self {
            id: "gemini".to_string(),
            name: "Gemini CLI".to_string(),
            config: Arc::new(RwLock::new(GeminiConfig::default())),
            workspace_exec: None,
            last_child: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub fn with_config(config: GeminiConfig) -> Self {
        Self {
            id: "gemini".to_string(),
            name: "Gemini CLI".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: None,
            last_child: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub fn with_config_and_workspace(
        config: GeminiConfig,
        workspace_exec: crate::workspace_exec::WorkspaceExec,
    ) -> Self {
        Self {
            id: "gemini".to_string(),
            name: "Gemini CLI".to_string(),
            config: Arc::new(RwLock::new(config)),
            workspace_exec: Some(workspace_exec),
            last_child: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Update the backend configuration.
    pub async fn update_config(&self, config: GeminiConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get the current configuration.
    pub async fn get_config(&self) -> GeminiConfig {
        self.config.read().await.clone()
    }

    /// Kill the most recently spawned Gemini CLI process.
    pub async fn kill(&self) {
        if let Some(child_arc) = self.last_child.lock().await.take() {
            if let Some(mut child) = child_arc.lock().await.take() {
                if let Err(e) = child.kill().await {
                    tracing::warn!("Failed to kill Gemini CLI process: {}", e);
                } else {
                    tracing::info!("Gemini CLI process killed");
                }
            }
        }
    }
}

impl Default for GeminiBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for GeminiBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn cli_names(&self) -> &'static [&'static str] {
        &["gemini"]
    }

    async fn check_auth_configured(&self, ctx: &crate::backend::AuthContext<'_>) -> Option<bool> {
        Some(check_gemini_auth_configured(ctx.working_dir))
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>, Error> {
        // Gemini CLI doesn't have separate agent types
        // Return a single general-purpose agent
        Ok(vec![AgentInfo {
            id: "default".to_string(),
            name: "Gemini Agent".to_string(),
        }])
    }

    async fn create_session(&self, config: SessionConfig) -> Result<Session, Error> {
        let client = GeminiClient::new();
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
        let client = GeminiClient::with_config(config);
        let workspace_exec = self.workspace_exec.as_ref();

        // Don't pass session_id for --resume: each invocation is a fresh CLI
        // process with the full conversation provided inline.  Gemini's --resume
        // requires a session that actually exists on disk (unlike Claude's
        // --session-id which creates-or-continues), so passing our generated
        // UUID would cause a "session not found" error.
        let (mut gemini_rx, gemini_handle) = client
            .execute_message(
                &session.directory,
                message,
                session.model.as_deref(),
                None,
                session.agent.as_deref(),
                workspace_exec,
            )
            .await?;

        // Store child handle for kill-on-cancel
        {
            let mut last = self.last_child.lock().await;
            *last = Some(gemini_handle.child_arc());
        }

        let (tx, rx) = mpsc::channel(256);
        let session_id = session.id.clone();

        // Spawn event conversion task
        let handle = tokio::spawn(async move {
            // Track tool_id -> tool_name so ToolResult can use the real name
            let mut tool_names: HashMap<String, String> = HashMap::new();
            let mut receiver_dropped = false;

            while let Some(event) = gemini_rx.recv().await {
                let exec_events = convert_gemini_event(event, &mut tool_names);

                for exec_event in exec_events {
                    if tx.send(exec_event).await.is_err() {
                        debug!("ExecutionEvent receiver dropped");
                        receiver_dropped = true;
                        break;
                    }
                }
                if receiver_dropped {
                    break;
                }
            }

            // Ensure MessageComplete is sent
            let _ = tx
                .send(ExecutionEvent::MessageComplete {
                    session_id: session_id.clone(),
                })
                .await;

            // Drop the gemini handle to clean up
            drop(gemini_handle);
        });

        Ok((rx, handle))
    }
}

/// Convert a Gemini CLI event to backend-agnostic ExecutionEvents.
///
/// `tool_names` is a mutable map of tool_id -> tool_name, populated by ToolUse
/// events and consumed by ToolResult events so the result carries the real tool
/// name instead of the opaque tool ID.
fn convert_gemini_event(
    event: GeminiEvent,
    tool_names: &mut HashMap<String, String>,
) -> Vec<ExecutionEvent> {
    let mut results = vec![];

    match event {
        GeminiEvent::Init { session_id, model } => {
            debug!(
                "Gemini session init: session_id={:?}, model={:?}",
                session_id, model
            );
        }

        GeminiEvent::Message {
            role,
            content,
            delta: _,
        } => {
            // Only emit assistant messages as text deltas
            if role.as_deref() == Some("assistant") {
                if let Some(text) = content {
                    if !text.is_empty() {
                        results.push(ExecutionEvent::TextDelta { content: text });
                    }
                }
            }
        }

        GeminiEvent::ToolUse {
            tool_name,
            tool_id,
            parameters,
        } => {
            if let Some(name) = tool_name {
                let id = tool_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let args =
                    parameters.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                // Remember tool_id -> tool_name for later ToolResult events
                tool_names.insert(id.clone(), name.clone());
                results.push(ExecutionEvent::ToolCall { id, name, args });
            }
        }

        GeminiEvent::ToolResult {
            tool_id,
            status: _,
            output,
            error,
        } => {
            if let Some(id) = tool_id {
                // Build result value, including error if present
                let result = if let Some(err) = error {
                    serde_json::json!({
                        "error": {
                            "type": err.error_type,
                            "message": err.message,
                        },
                        "output": output,
                    })
                } else {
                    output.unwrap_or(serde_json::Value::Null)
                };

                // Look up the real tool name from the preceding ToolUse event;
                // fall back to the tool_id if we never saw a matching ToolUse.
                let name = tool_names.get(&id).cloned().unwrap_or_else(|| id.clone());
                results.push(ExecutionEvent::ToolResult { id, name, result });
            }
        }

        GeminiEvent::Error { severity, message } => {
            // Treat warnings as debug logs, errors as execution errors
            if severity.as_deref() == Some("warning") {
                debug!("Gemini warning: {}", message);
            } else {
                results.push(ExecutionEvent::Error { message });
            }
        }

        GeminiEvent::Result {
            status,
            error,
            stats,
        } => {
            // Check if the result status indicates an error
            if let Some(ref s) = status {
                if s != "success" && s != "ok" {
                    let detail = error
                        .as_ref()
                        .and_then(|e| e.message.as_deref())
                        .unwrap_or("no details provided");
                    let error_type = error
                        .as_ref()
                        .and_then(|e| e.error_type.as_deref())
                        .unwrap_or("unknown");
                    results.push(ExecutionEvent::Error {
                        message: format!(
                            "Gemini CLI finished with status: {} (type: {}, detail: {})",
                            s, error_type, detail
                        ),
                    });
                }
            }

            // Extract token usage from final stats
            if let Some(stats) = stats {
                let input = stats.total_input_tokens.unwrap_or(0);
                let output = stats.total_output_tokens.unwrap_or(0);
                if input > 0 || output > 0 {
                    results.push(ExecutionEvent::Usage {
                        input_tokens: input,
                        output_tokens: output,
                    });
                }
            }
        }

        GeminiEvent::Thought { content } => {
            if let Some(text) = content {
                if !text.is_empty() {
                    results.push(ExecutionEvent::Thinking { content: text });
                }
            }
        }

        GeminiEvent::Unknown => {
            debug!("Unknown Gemini event type");
        }
    }

    results
}

/// Create a registry entry for the Gemini backend.
pub fn registry_entry() -> Arc<dyn Backend> {
    Arc::new(GeminiBackend::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_agents() {
        let backend = GeminiBackend::new();
        let agents = backend.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "default");
    }

    #[tokio::test]
    async fn test_create_session() {
        let backend = GeminiBackend::new();
        let session = backend
            .create_session(SessionConfig {
                directory: "/tmp".to_string(),
                title: Some("Test".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
                agent: None,
            })
            .await
            .unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.directory, "/tmp");
    }

    #[test]
    fn convert_gemini_event_init_no_events() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Init {
            session_id: Some("s1".to_string()),
            model: Some("gemini-2.5-flash".to_string()),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert!(events.is_empty(), "Init should produce no execution events");
    }

    #[test]
    fn convert_gemini_event_assistant_message() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Message {
            role: Some("assistant".to_string()),
            content: Some("Hello world".to_string()),
            delta: Some(true),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::TextDelta { content } => {
                assert_eq!(content, "Hello world");
            }
            other => panic!("Expected TextDelta, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_user_message_ignored() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Message {
            role: Some("user".to_string()),
            content: Some("User message".to_string()),
            delta: Some(false),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert!(events.is_empty(), "User messages should be ignored");
    }

    #[test]
    fn convert_gemini_event_tool_use() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::ToolUse {
            tool_name: Some("read_file".to_string()),
            tool_id: Some("tc1".to_string()),
            parameters: Some(serde_json::json!({"path": "/tmp/test.txt"})),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolCall { id, name, args } => {
                assert_eq!(id, "tc1");
                assert_eq!(name, "read_file");
                assert_eq!(args["path"], "/tmp/test.txt");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
        // Verify tool name was stored
        assert_eq!(tool_names.get("tc1").unwrap(), "read_file");
    }

    #[test]
    fn convert_gemini_event_tool_result_uses_stored_name() {
        let mut tool_names = HashMap::new();
        // Simulate a preceding ToolUse that populated the map
        tool_names.insert("tc1".to_string(), "read_file".to_string());

        let event = GeminiEvent::ToolResult {
            tool_id: Some("tc1".to_string()),
            status: Some("success".to_string()),
            output: Some(serde_json::json!("file contents")),
            error: None,
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tc1");
                assert_eq!(name, "read_file");
                assert_eq!(result, "file contents");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_tool_result_falls_back_to_id() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::ToolResult {
            tool_id: Some("tc1".to_string()),
            status: Some("success".to_string()),
            output: Some(serde_json::json!("file contents")),
            error: None,
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::ToolResult { id, name, result } => {
                assert_eq!(id, "tc1");
                assert_eq!(name, "tc1"); // Falls back to ID
                assert_eq!(result, "file contents");
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_error() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Error {
            severity: Some("error".to_string()),
            message: "Something failed".to_string(),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Error { message } => {
                assert_eq!(message, "Something failed");
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_warning_ignored() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Error {
            severity: Some("warning".to_string()),
            message: "Just a warning".to_string(),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert!(events.is_empty(), "Warnings should not produce events");
    }

    #[test]
    fn convert_gemini_event_result_with_usage() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Result {
            status: Some("success".to_string()),
            error: None,
            stats: Some(client::GeminiStats {
                total_input_tokens: Some(1500),
                total_output_tokens: Some(300),
                models: None,
            }),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(*input_tokens, 1500);
                assert_eq!(*output_tokens, 300);
            }
            other => panic!("Expected Usage, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_result_error_status() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Result {
            status: Some("error".to_string()),
            error: None,
            stats: None,
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Error { message } => {
                assert!(message.contains("error"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_result_error_with_detail() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Result {
            status: Some("error".to_string()),
            error: Some(client::GeminiErrorDetail {
                error_type: Some("api_error".to_string()),
                message: Some("Rate limit exceeded".to_string()),
            }),
            stats: None,
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ExecutionEvent::Error { message } => {
                assert!(message.contains("api_error"));
                assert!(message.contains("Rate limit exceeded"));
            }
            other => panic!("Expected Error, got {:?}", other),
        }
    }

    #[test]
    fn convert_gemini_event_result_zero_usage() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Result {
            status: Some("success".to_string()),
            error: None,
            stats: Some(client::GeminiStats {
                total_input_tokens: Some(0),
                total_output_tokens: Some(0),
                models: None,
            }),
        };
        let events = convert_gemini_event(event, &mut tool_names);
        assert!(events.is_empty(), "Zero usage should not emit Usage event");
    }

    #[test]
    fn convert_gemini_event_unknown_no_events() {
        let mut tool_names = HashMap::new();
        let event = GeminiEvent::Unknown;
        let events = convert_gemini_event(event, &mut tool_names);
        assert!(events.is_empty());
    }
}
