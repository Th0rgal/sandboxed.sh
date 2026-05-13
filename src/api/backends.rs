//! Backend management API endpoints.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::backend::registry::BackendInfo;

use super::auth::AuthUser;
use super::routes::AppState;

/// Backend information returned by API
#[derive(Debug, Clone, Serialize)]
pub struct BackendResponse {
    pub id: String,
    pub name: String,
}

impl From<BackendInfo> for BackendResponse {
    fn from(info: BackendInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
        }
    }
}

/// Agent information returned by API
#[derive(Debug, Clone, Serialize)]
pub struct AgentResponse {
    pub id: String,
    pub name: String,
}

/// List all available backends
pub async fn list_backends(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Json<Vec<BackendResponse>> {
    let registry = state.backend_registry.read().await;
    let backends: Vec<BackendResponse> = registry.list().into_iter().map(Into::into).collect();
    Json(backends)
}

/// Get a specific backend by ID
pub async fn get_backend(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<BackendResponse>, (StatusCode, String)> {
    let registry = state.backend_registry.read().await;
    match registry.get(&id) {
        Some(backend) => Ok(Json(BackendResponse {
            id: backend.id().to_string(),
            name: backend.name().to_string(),
        })),
        None => Err((StatusCode::NOT_FOUND, format!("Backend {} not found", id))),
    }
}

/// List agents for a specific backend
pub async fn list_backend_agents(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AgentResponse>>, (StatusCode, String)> {
    let registry = state.backend_registry.read().await;
    let backend = registry
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Backend {} not found", id)))?;

    match backend.list_agents().await {
        Ok(agents) => {
            let agents: Vec<AgentResponse> = agents
                .into_iter()
                .map(|a| AgentResponse {
                    id: a.id,
                    name: a.name,
                })
                .collect();
            Ok(Json(agents))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list agents: {}", e),
        )),
    }
}

/// Backend configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub settings: serde_json::Value,
    /// Whether the CLI for this backend is available on the system
    #[serde(default)]
    pub cli_available: bool,
    /// Whether authentication for this backend is configured (None = not applicable / not checked)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_configured: Option<bool>,
}

/// Check if a CLI command is available on the system
fn check_cli_available(cli_name: &str) -> bool {
    use std::process::Command;

    // Check if it's an absolute path
    if cli_name.starts_with('/') {
        return std::path::Path::new(cli_name).exists();
    }

    // Check using `which` command
    Command::new("which")
        .arg(cli_name)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check whether Gemini has any usable credentials available.
///
/// Mirrors the precedence used by `get_google_credentials_for_gemini` in
/// `mission_runner.rs` but returns only a boolean: env vars, the AI provider
/// store (Google provider targeting the gemini backend with an API key),
/// the sandboxed-sh credentials store, and OpenCode's auth.json.
fn check_gemini_auth_configured(working_dir: &std::path::Path) -> bool {
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

/// Get backend configuration
pub async fn get_backend_config(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<BackendConfig>, (StatusCode, String)> {
    let registry = state.backend_registry.read().await;
    let backend = registry
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Backend {} not found", id)))?;
    drop(registry);

    let config_entry = state.backend_configs.get(&id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Backend {} not configured", id),
        )
    })?;

    let mut settings = config_entry.settings.clone();

    if id == "claudecode" {
        let api_key_configured = if let Some(store) = state.secrets.as_ref() {
            match store.list_secrets("claudecode").await {
                Ok(secrets) => secrets.iter().any(|s| s.key == "api_key" && !s.is_expired),
                Err(_) => false,
            }
        } else {
            false
        };

        let mut obj = settings.as_object().cloned().unwrap_or_default();
        obj.insert(
            "api_key_configured".to_string(),
            serde_json::Value::Bool(api_key_configured),
        );
        settings = serde_json::Value::Object(obj);
    }

    // For amp backend, mask the api_key but indicate if configured
    if id == "amp" {
        let mut obj = settings.as_object().cloned().unwrap_or_default();
        let has_api_key = obj
            .get("api_key")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty() && !s.starts_with("[REDACTED") && s != "********")
            .unwrap_or(false);
        obj.insert(
            "api_key_configured".to_string(),
            serde_json::Value::Bool(has_api_key),
        );
        // Mask the actual api_key value if present and valid, or clear invalid values
        if has_api_key {
            obj.insert(
                "api_key".to_string(),
                serde_json::Value::String("********".to_string()),
            );
        } else {
            // Clear invalid/redacted values so frontend shows empty field
            obj.remove("api_key");
        }
        settings = serde_json::Value::Object(obj);
    }

    // Auth-state check for backends that ship without account-level guards.
    let auth_configured: Option<bool> = match id.as_str() {
        "gemini" => Some(check_gemini_auth_configured(&state.config.working_dir)),
        _ => None,
    };

    // Check CLI availability based on backend type
    let cli_available = match id.as_str() {
        "claudecode" => {
            // Check for custom cli_path first, then default 'claude'
            let cli_path = settings
                .get("cli_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("claude");
            check_cli_available(cli_path)
        }
        "amp" => {
            let cli_path = settings
                .get("cli_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("amp");
            check_cli_available(cli_path)
        }
        "codex" => {
            let cli_path = settings
                .get("cli_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("codex");
            check_cli_available(cli_path)
        }
        "opencode" => {
            // OpenCode uses oh-my-opencode or opencode CLI
            check_cli_available("oh-my-opencode") || check_cli_available("opencode")
        }
        _ => true,
    };

    Ok(Json(BackendConfig {
        id: backend.id().to_string(),
        name: backend.name().to_string(),
        enabled: config_entry.enabled,
        settings,
        cli_available,
        auth_configured,
    }))
}

/// Request to update backend configuration
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBackendConfigRequest {
    pub settings: serde_json::Value,
    pub enabled: Option<bool>,
}

/// Update backend configuration
pub async fn update_backend_config(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateBackendConfigRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let registry = state.backend_registry.read().await;
    if registry.get(&id).is_none() {
        return Err((StatusCode::NOT_FOUND, format!("Backend {} not found", id)));
    }
    drop(registry);

    let updated_settings = match id.as_str() {
        "opencode" => {
            let settings = req.settings.as_object().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid settings payload".to_string(),
                )
            })?;
            let base_url = settings
                .get("base_url")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "base_url is required".to_string()))?;
            let default_agent = settings
                .get("default_agent")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let permissive = settings
                .get("permissive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            serde_json::json!({
                "base_url": base_url,
                "default_agent": default_agent,
                "permissive": permissive,
            })
        }
        "claudecode" => {
            let mut settings = req.settings.clone();
            if let Some(api_key) = settings.get("api_key").and_then(|v| v.as_str()) {
                let store = state.secrets.as_ref().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        "Secrets store not available".to_string(),
                    )
                })?;
                store
                    .set_secret("claudecode", "api_key", api_key, None)
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::BAD_REQUEST,
                            format!("Failed to store API key: {}", e),
                        )
                    })?;
            }
            if let Some(obj) = settings.as_object_mut() {
                obj.remove("api_key");
            }
            settings
        }
        "amp" => {
            let settings = req.settings.as_object().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid settings payload".to_string(),
                )
            })?;

            tracing::debug!("Amp config update - received settings: {:?}", req.settings);

            // Get current config to preserve api_key if not being updated
            let current_config = state.backend_configs.get(&id).await;
            let current_api_key = current_config
                .as_ref()
                .and_then(|c| c.settings.get("api_key"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && !s.starts_with("[REDACTED") && *s != "********")
                .map(|s| s.to_string());

            // Get the new api_key if provided and valid (not masked/redacted)
            let new_api_key = settings
                .get("api_key")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && !s.starts_with("[REDACTED") && *s != "********")
                .map(|s| s.to_string());

            // Use new key if provided, otherwise keep existing
            let api_key = new_api_key.or(current_api_key);

            tracing::debug!(
                "Amp config update - api_key present: {}, api_key_len: {:?}",
                api_key.is_some(),
                api_key.as_ref().map(|k| k.len())
            );

            let cli_path = settings
                .get("cli_path")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let default_mode = settings
                .get("default_mode")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "smart".to_string());
            let permissive = settings
                .get("permissive")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            serde_json::json!({
                "api_key": api_key,
                "cli_path": cli_path,
                "default_mode": default_mode,
                "permissive": permissive,
            })
        }
        _ => req.settings.clone(),
    };

    let updated = state
        .backend_configs
        .update_settings(&id, updated_settings, req.enabled)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to persist backend config: {}", e),
            )
        })?;

    if updated.is_none() {
        return Err((StatusCode::NOT_FOUND, format!("Backend {} not found", id)));
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Backend configuration updated."
    })))
}
