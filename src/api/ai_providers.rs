//! AI Provider management API endpoints.
//!
//! Provides endpoints for managing inference providers:
//! - List providers
//! - Create provider
//! - Get provider details
//! - Update provider
//! - Delete provider
//! - Authenticate provider (OAuth flow)
//! - Set default provider

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::ai_providers::{AuthMethod, PendingOAuth, ProviderType};

/// Anthropic OAuth client ID (from opencode-anthropic-auth plugin)
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_CONSOLE_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// OpenAI OAuth client ID (Codex OAuth flow)
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_SCOPE: &str = "openid profile email offline_access";

/// Google/Gemini OAuth constants (from opencode-gemini-auth plugin)
const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_REDIRECT_URI: &str = "http://localhost:8085/oauth2callback";
const GOOGLE_SCOPES: &str =
    "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";

fn google_client_id() -> String {
    env::var("GOOGLE_CLIENT_ID").unwrap_or_else(|_| "missing-google-client-id".to_string())
}

fn google_client_secret() -> String {
    env::var("GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| "missing-google-client-secret".to_string())
}

fn anthropic_client_id() -> String {
    ANTHROPIC_CLIENT_ID
        .strip_prefix("urn:uuid:")
        .unwrap_or(ANTHROPIC_CLIENT_ID)
        .to_string()
}

fn anthropic_redirect_uri(mode: &str, client_id: &str) -> String {
    if mode == "max" {
        format!("urn:uuid:{}", client_id)
    } else {
        ANTHROPIC_CONSOLE_REDIRECT_URI.to_string()
    }
}

fn openai_authorize_url(challenge: &str, state: &str) -> Result<String, String> {
    let mut url =
        url::Url::parse(OPENAI_AUTHORIZE_URL).map_err(|e| format!("Failed to parse URL: {}", e))?;

    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OPENAI_CLIENT_ID)
        .append_pair("redirect_uri", OPENAI_REDIRECT_URI)
        .append_pair("scope", OPENAI_SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "codex_cli_rs");

    Ok(url.to_string())
}

fn google_authorize_url(challenge: &str, state: &str) -> Result<String, String> {
    let mut url =
        url::Url::parse(GOOGLE_AUTHORIZE_URL).map_err(|e| format!("Failed to parse URL: {}", e))?;
    let client_id = google_client_id();

    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", GOOGLE_REDIRECT_URI)
        .append_pair("scope", GOOGLE_SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");

    Ok(url.to_string())
}

/// Create AI provider routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_providers))
        .route("/", post(create_provider))
        .route("/types", get(list_provider_types))
        .route("/opencode-auth", get(get_opencode_auth))
        .route("/opencode-auth", post(set_opencode_auth))
        .route("/:id", get(get_provider))
        .route("/:id", put(update_provider))
        .route("/:id", delete(delete_provider))
        .route("/:id/auth", post(authenticate_provider))
        .route("/:id/auth/methods", get(get_auth_methods))
        .route("/:id/oauth/authorize", post(oauth_authorize))
        .route("/:id/oauth/callback", post(oauth_callback))
        .route("/:id/default", post(set_default))
}

// ─────────────────────────────────────────────────────────────────────────────
// Request/Response Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProviderTypeInfo {
    pub id: String,
    pub name: String,
    pub uses_oauth: bool,
    pub env_var: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub provider_type: ProviderType,
    pub name: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub api_key: Option<Option<String>>,
    pub base_url: Option<Option<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ProviderResponse {
    pub id: String,
    pub provider_type: ProviderType,
    pub provider_type_name: String,
    pub name: String,
    pub has_api_key: bool,
    pub has_oauth: bool,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub uses_oauth: bool,
    pub auth_methods: Vec<AuthMethod>,
    pub status: ProviderStatusResponse,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ProviderStatusResponse {
    Unknown,
    Connected,
    NeedsAuth { auth_url: Option<String> },
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    ApiKey,
    OAuth,
}

#[derive(Debug, Clone)]
struct ProviderConfigEntry {
    name: Option<String>,
    base_url: Option<String>,
    enabled: Option<bool>,
}

fn build_provider_response(
    provider_type: ProviderType,
    config: Option<ProviderConfigEntry>,
    auth: Option<AuthKind>,
    default_provider: Option<ProviderType>,
) -> ProviderResponse {
    let now = chrono::Utc::now();
    let name = config
        .as_ref()
        .and_then(|c| c.name.clone())
        .unwrap_or_else(|| provider_type.display_name().to_string());
    let base_url = config.as_ref().and_then(|c| c.base_url.clone());
    let enabled = config.as_ref().and_then(|c| c.enabled).unwrap_or(true);
    let is_default = default_provider
        .map(|p| p == provider_type)
        .unwrap_or(false);
    let status = match auth {
        Some(AuthKind::ApiKey) | Some(AuthKind::OAuth) => ProviderStatusResponse::Connected,
        None => {
            if provider_type.uses_oauth() {
                ProviderStatusResponse::NeedsAuth { auth_url: None }
            } else {
                ProviderStatusResponse::NeedsAuth { auth_url: None }
            }
        }
    };

    ProviderResponse {
        id: provider_type.id().to_string(),
        provider_type,
        provider_type_name: provider_type.display_name().to_string(),
        name,
        has_api_key: matches!(auth, Some(AuthKind::ApiKey)),
        has_oauth: matches!(auth, Some(AuthKind::OAuth)),
        base_url,
        enabled,
        is_default,
        uses_oauth: provider_type.uses_oauth(),
        auth_methods: provider_type.auth_methods(),
        status,
        created_at: now,
        updated_at: now,
    }
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub success: bool,
    pub message: String,
    /// OAuth URL to redirect user to (if OAuth flow required)
    pub auth_url: Option<String>,
}

/// Request to initiate OAuth authorization.
#[derive(Debug, Deserialize)]
pub struct OAuthAuthorizeRequest {
    /// Index of the auth method to use (0-indexed)
    pub method_index: usize,
}

/// Response from OAuth authorization initiation.
#[derive(Debug, Serialize)]
pub struct OAuthAuthorizeResponse {
    /// URL to redirect user to for authorization
    pub url: String,
    /// Instructions to show the user
    pub instructions: String,
    /// Method for callback: "code" means user pastes code
    pub method: String,
}

/// Request to exchange OAuth code for credentials.
#[derive(Debug, Deserialize)]
pub struct OAuthCallbackRequest {
    /// Index of the auth method used
    pub method_index: usize,
    /// Authorization code from the OAuth flow
    pub code: String,
}

/// Request to set OpenCode auth credentials directly.
#[derive(Debug, Deserialize)]
pub struct SetOpenCodeAuthRequest {
    /// Provider type (e.g., "anthropic")
    pub provider: String,
    /// Refresh token
    pub refresh_token: String,
    /// Access token
    pub access_token: String,
    /// Token expiry timestamp in milliseconds
    pub expires_at: i64,
}

/// Response for OpenCode auth operations.
#[derive(Debug, Serialize)]
pub struct OpenCodeAuthResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<serde_json::Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Auth Sync
// ─────────────────────────────────────────────────────────────────────────────

/// Sync OAuth credentials to OpenCode's auth.json file.
///
/// OpenCode stores auth in `~/.local/share/opencode/auth.json` with format:
/// ```json
/// {
///   "anthropic": {
///     "type": "oauth",
///     "refresh": "sk-ant-ort01-...",
///     "access": "sk-ant-oat01-...",
///     "expires": 1767743285144
///   }
/// }
/// ```
fn sync_to_opencode_auth(
    provider_type: ProviderType,
    refresh_token: &str,
    access_token: &str,
    expires_at: i64,
) -> Result<(), String> {
    let auth_path = get_opencode_auth_path();

    // Ensure parent directory exists
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create OpenCode auth directory: {}", e))?;
    }

    // Read existing auth or start fresh
    let mut auth: serde_json::Map<String, serde_json::Value> = if auth_path.exists() {
        let contents = std::fs::read_to_string(&auth_path)
            .map_err(|e| format!("Failed to read OpenCode auth: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    // Map our provider type to OpenCode's key
    let key = opencode_auth_key(provider_type)
        .ok_or_else(|| "Provider does not map to an OpenCode auth key".to_string())?;

    // Create the auth entry in OpenCode format
    let entry = serde_json::json!({
        "type": "oauth",
        "refresh": refresh_token,
        "access": access_token,
        "expires": expires_at
    });

    auth.insert(key.to_string(), entry.clone());

    // Write back to file
    let contents = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;

    if provider_type == ProviderType::OpenAI {
        if let Err(e) = write_opencode_provider_auth_file(provider_type, &entry) {
            tracing::error!("Failed to write OpenCode provider auth file: {}", e);
        }
    }

    tracing::info!(
        "Synced OAuth credentials to OpenCode auth.json for provider: {}",
        key
    );

    Ok(())
}

/// Sync an API key to OpenCode's auth.json file.
fn sync_api_key_to_opencode_auth(provider_type: ProviderType, api_key: &str) -> Result<(), String> {
    let auth_path = get_opencode_auth_path();

    // Ensure parent directory exists
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create OpenCode auth directory: {}", e))?;
    }

    let mut auth: serde_json::Map<String, serde_json::Value> = if auth_path.exists() {
        let contents = std::fs::read_to_string(&auth_path)
            .map_err(|e| format!("Failed to read OpenCode auth: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    let key = match opencode_auth_key(provider_type) {
        Some(key) => key,
        None => return Ok(()),
    };

    let entry = serde_json::json!({
        "type": "api_key",
        "key": api_key
    });

    auth.insert(key.to_string(), entry);

    let contents = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;

    if provider_type == ProviderType::OpenAI {
        let provider_entry = serde_json::json!({
            "type": "api_key",
            "key": api_key
        });
        if let Err(e) = write_opencode_provider_auth_file(provider_type, &provider_entry) {
            tracing::error!("Failed to write OpenCode provider auth file: {}", e);
        }
    }

    tracing::info!("Synced API key to OpenCode auth.json for provider: {}", key);

    Ok(())
}

/// Remove a provider entry from OpenCode's auth.json file.
fn remove_opencode_auth_entry(provider_type: ProviderType) -> Result<(), String> {
    let auth_path = get_opencode_auth_path();
    if !auth_path.exists() {
        // Still attempt to remove provider-specific auth file if present.
        if provider_type == ProviderType::OpenAI {
            let provider_path = get_opencode_provider_auth_path(provider_type);
            if provider_path.exists() {
                std::fs::remove_file(&provider_path)
                    .map_err(|e| format!("Failed to remove OpenCode provider auth: {}", e))?;
            }
        }
        return Ok(());
    }

    let mut auth: serde_json::Map<String, serde_json::Value> = {
        let contents = std::fs::read_to_string(&auth_path)
            .map_err(|e| format!("Failed to read OpenCode auth: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    };

    let key = match opencode_auth_key(provider_type) {
        Some(key) => key,
        None => return Ok(()),
    };

    if auth.remove(key).is_some() {
        let contents = serde_json::to_string_pretty(&auth)
            .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
        std::fs::write(&auth_path, contents)
            .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;
    }

    if provider_type == ProviderType::OpenAI {
        let provider_path = get_opencode_provider_auth_path(provider_type);
        if provider_path.exists() {
            std::fs::remove_file(&provider_path)
                .map_err(|e| format!("Failed to remove OpenCode provider auth: {}", e))?;
        }
    }

    Ok(())
}

/// Get the path to OpenCode's auth.json file.
fn get_opencode_auth_path() -> PathBuf {
    let data_home = std::env::var("XDG_DATA_HOME").ok();
    let base = if let Some(data_home) = data_home {
        PathBuf::from(data_home).join("opencode")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(&home).join(".local/share/opencode")
    };

    base.join("auth.json")
}

fn get_opencode_provider_auth_path(provider_type: ProviderType) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".opencode")
        .join("auth")
        .join(format!("{}.json", provider_type.id()))
}

fn read_opencode_provider_auth(provider_type: ProviderType) -> Result<Option<AuthKind>, String> {
    let auth_path = get_opencode_provider_auth_path(provider_type);
    if !auth_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&auth_path)
        .map_err(|e| format!("Failed to read OpenCode provider auth: {}", e))?;
    let value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse OpenCode provider auth: {}", e))?;
    Ok(auth_kind_from_value(&value))
}

fn write_opencode_provider_auth_file(
    provider_type: ProviderType,
    entry: &serde_json::Value,
) -> Result<(), String> {
    let auth_path = get_opencode_provider_auth_path(provider_type);
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create OpenCode provider auth directory: {}", e))?;
    }

    let contents = serde_json::to_string_pretty(entry)
        .map_err(|e| format!("Failed to serialize OpenCode provider auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode provider auth: {}", e))?;

    Ok(())
}

fn opencode_auth_key(provider_type: ProviderType) -> Option<&'static str> {
    match provider_type {
        ProviderType::Custom => None,
        _ => Some(provider_type.id()),
    }
}

fn get_opencode_config_path(working_dir: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("OPENCODE_CONFIG") {
        return PathBuf::from(path);
    }
    working_dir.join("opencode.json")
}

fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    while let Some(n) = chars.next() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                    continue;
                }
                _ => {}
            }
        }

        out.push(c);
    }

    out
}

fn strip_openagent_key(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("openagent");
    }
    value
}

fn read_opencode_config(path: &Path) -> Result<serde_json::Value, String> {
    if !path.exists() {
        return Ok(serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": {}
        }));
    }

    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read OpenCode config: {}", e))?;

    match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => Ok(strip_openagent_key(value)),
        Err(_) => {
            let stripped = strip_jsonc_comments(&contents);
            serde_json::from_str(&stripped)
                .map(strip_openagent_key)
                .map_err(|e| format!("Failed to parse OpenCode config: {}", e))
        }
    }
}

fn write_opencode_config(path: &Path, config: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create OpenCode config directory: {}", e))?;
    }

    let contents = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize OpenCode config: {}", e))?;
    std::fs::write(path, contents)
        .map_err(|e| format!("Failed to write OpenCode config: {}", e))?;
    Ok(())
}

fn get_provider_config_entry(
    config: &serde_json::Value,
    provider: ProviderType,
) -> Option<ProviderConfigEntry> {
    let providers = config.get("provider")?.as_object()?;
    let entry = providers.get(provider.id())?.as_object()?;
    let name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let base_url = entry
        .get("baseURL")
        .or_else(|| entry.get("baseUrl"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let enabled = entry.get("enabled").and_then(|v| v.as_bool());
    Some(ProviderConfigEntry {
        name,
        base_url,
        enabled,
    })
}

fn set_provider_config_entry(
    config: &mut serde_json::Value,
    provider: ProviderType,
    name: Option<String>,
    base_url: Option<Option<String>>,
    enabled: Option<bool>,
) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    let root = config.as_object_mut().expect("config object");
    let providers_value = root
        .entry("provider")
        .or_insert_with(|| serde_json::json!({}));
    if !providers_value.is_object() {
        *providers_value = serde_json::json!({});
    }
    let providers = providers_value.as_object_mut().expect("provider object");
    let entry = providers
        .entry(provider.id().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let entry_obj = entry.as_object_mut().expect("provider entry object");

    if let Some(name) = name {
        entry_obj.insert("name".to_string(), serde_json::Value::String(name));
    }

    if let Some(base_url) = base_url {
        match base_url {
            Some(url) => {
                entry_obj.insert("baseURL".to_string(), serde_json::Value::String(url));
            }
            None => {
                entry_obj.remove("baseURL");
                entry_obj.remove("baseUrl");
            }
        }
    }

    // OpenCode's config schema doesn't accept "enabled" under provider entries.
    // We treat providers as enabled when present and avoid writing this field.
    let _ = enabled;
    entry_obj.remove("enabled");
}

fn remove_provider_config_entry(config: &mut serde_json::Value, provider: ProviderType) {
    if let Some(root) = config.as_object_mut() {
        if let Some(providers_value) = root.get_mut("provider") {
            if let Some(providers) = providers_value.as_object_mut() {
                providers.remove(provider.id());
            }
        }
    }
}

fn get_default_provider(config: &serde_json::Value) -> Option<ProviderType> {
    let model = config.get("model").and_then(|v| v.as_str())?;
    let provider = model.splitn(2, '/').next()?.trim();
    ProviderType::from_id(provider)
}

fn default_provider_state_path(working_dir: &Path) -> PathBuf {
    working_dir.join(".openagent").join("default_provider.json")
}

fn read_default_provider_state(working_dir: &Path) -> Option<ProviderType> {
    let path = default_provider_state_path(working_dir);
    let contents = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    value
        .get("default_provider")
        .and_then(|v| v.as_str())
        .and_then(ProviderType::from_id)
}

fn write_default_provider_state(working_dir: &Path, provider: ProviderType) -> Result<(), String> {
    let path = default_provider_state_path(working_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create default provider directory: {}", e))?;
    }
    let payload = serde_json::json!({
        "default_provider": provider.id(),
    });
    let contents = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize default provider: {}", e))?;
    std::fs::write(path, contents)
        .map_err(|e| format!("Failed to write default provider: {}", e))?;
    Ok(())
}

fn clear_default_provider_state(working_dir: &Path) -> Result<(), String> {
    let path = default_provider_state_path(working_dir);
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| format!("Failed to remove default provider file: {}", e))?;
    }
    Ok(())
}

/// Read OpenCode's current auth.json contents.
fn read_opencode_auth() -> Result<serde_json::Value, String> {
    let auth_path = get_opencode_auth_path();
    if !auth_path.exists() {
        return Ok(serde_json::json!({}));
    }

    let contents = std::fs::read_to_string(&auth_path)
        .map_err(|e| format!("Failed to read OpenCode auth: {}", e))?;
    serde_json::from_str(&contents).map_err(|e| format!("Failed to parse OpenCode auth: {}", e))
}

fn auth_kind_from_value(value: &serde_json::Value) -> Option<AuthKind> {
    match value.get("type").and_then(|v| v.as_str()) {
        Some("oauth") => Some(AuthKind::OAuth),
        Some("api_key") | Some("api") => Some(AuthKind::ApiKey),
        _ => {
            if value.get("refresh").is_some() || value.get("access").is_some() {
                Some(AuthKind::OAuth)
            } else if value.get("key").is_some() || value.get("api_key").is_some() {
                Some(AuthKind::ApiKey)
            } else {
                None
            }
        }
    }
}

fn read_opencode_auth_map() -> Result<HashMap<ProviderType, AuthKind>, String> {
    let auth = read_opencode_auth()?;
    let mut out = HashMap::new();
    let Some(map) = auth.as_object() else {
        return Ok(out);
    };

    for (key, value) in map {
        let Some(provider_type) = ProviderType::from_id(key.as_str()) else {
            continue;
        };
        let kind = auth_kind_from_value(value);
        if let Some(kind) = kind {
            out.insert(provider_type, kind);
        }
    }

    if !out.contains_key(&ProviderType::OpenAI) {
        if let Ok(Some(kind)) = read_opencode_provider_auth(ProviderType::OpenAI) {
            out.insert(ProviderType::OpenAI, kind);
        }
    }

    Ok(out)
}

/// Write to OpenCode's auth.json file.
fn write_opencode_auth(auth: &serde_json::Value) -> Result<(), String> {
    let auth_path = get_opencode_auth_path();

    // Ensure parent directory exists
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create OpenCode auth directory: {}", e))?;
    }

    let contents = serde_json::to_string_pretty(auth)
        .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/ai/providers/opencode-auth - Get current OpenCode auth credentials.
async fn get_opencode_auth() -> Result<Json<OpenCodeAuthResponse>, (StatusCode, String)> {
    match read_opencode_auth() {
        Ok(auth) => Ok(Json(OpenCodeAuthResponse {
            success: true,
            message: "OpenCode auth retrieved".to_string(),
            auth: Some(auth),
        })),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/ai/providers/opencode-auth - Set OpenCode auth credentials directly.
async fn set_opencode_auth(
    Json(req): Json<SetOpenCodeAuthRequest>,
) -> Result<Json<OpenCodeAuthResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&req.provider).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid provider: {}", req.provider),
        )
    })?;
    if !provider_type.uses_oauth() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Provider {} does not use OAuth", req.provider),
        ));
    }

    // Read existing auth
    let mut auth = read_opencode_auth().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Create the auth entry in OpenCode format
    let entry = serde_json::json!({
        "type": "oauth",
        "refresh": req.refresh_token,
        "access": req.access_token,
        "expires": req.expires_at
    });
    let entry_clone = entry.clone();

    // Update the auth object
    if let Some(obj) = auth.as_object_mut() {
        obj.insert(req.provider.clone(), entry);
    } else {
        auth = serde_json::json!({
            req.provider.clone(): entry
        });
    }

    // Write back to file
    write_opencode_auth(&auth).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if provider_type == ProviderType::OpenAI {
        if let Err(e) = write_opencode_provider_auth_file(provider_type, &entry_clone) {
            tracing::error!("Failed to write OpenCode provider auth file: {}", e);
        }
    }

    tracing::info!(
        "Set OpenCode auth credentials for provider: {}",
        req.provider
    );

    Ok(Json(OpenCodeAuthResponse {
        success: true,
        message: format!(
            "OpenCode auth credentials set for provider: {}",
            req.provider
        ),
        auth: Some(auth),
    }))
}

/// GET /api/ai/providers/types - List available provider types.
async fn list_provider_types() -> Json<Vec<ProviderTypeInfo>> {
    let types = vec![
        ProviderTypeInfo {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            uses_oauth: true,
            env_var: Some("ANTHROPIC_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            uses_oauth: true,
            env_var: Some("OPENAI_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "google".to_string(),
            name: "Google AI".to_string(),
            uses_oauth: true,
            env_var: Some("GOOGLE_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "amazon-bedrock".to_string(),
            name: "Amazon Bedrock".to_string(),
            uses_oauth: false,
            env_var: None,
        },
        ProviderTypeInfo {
            id: "azure".to_string(),
            name: "Azure OpenAI".to_string(),
            uses_oauth: false,
            env_var: Some("AZURE_OPENAI_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "open-router".to_string(),
            name: "OpenRouter".to_string(),
            uses_oauth: false,
            env_var: Some("OPENROUTER_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "mistral".to_string(),
            name: "Mistral AI".to_string(),
            uses_oauth: false,
            env_var: Some("MISTRAL_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "groq".to_string(),
            name: "Groq".to_string(),
            uses_oauth: false,
            env_var: Some("GROQ_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "xai".to_string(),
            name: "xAI".to_string(),
            uses_oauth: false,
            env_var: Some("XAI_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "github-copilot".to_string(),
            name: "GitHub Copilot".to_string(),
            uses_oauth: true,
            env_var: None,
        },
    ];
    Json(types)
}

/// GET /api/ai/providers - List all providers.
async fn list_providers(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<Vec<ProviderResponse>>, (StatusCode, String)> {
    let config_path = get_opencode_config_path(&state.config.working_dir);
    let opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let auth_map = read_opencode_auth_map().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let default_provider = read_default_provider_state(&state.config.working_dir)
        .or_else(|| get_default_provider(&opencode_config));

    let mut provider_ids: BTreeSet<String> = BTreeSet::new();
    for provider in auth_map.keys() {
        provider_ids.insert(provider.id().to_string());
    }
    if let Some(provider_map) = opencode_config.get("provider").and_then(|v| v.as_object()) {
        for key in provider_map.keys() {
            provider_ids.insert(key.to_string());
        }
    }
    if let Some(provider) = default_provider {
        provider_ids.insert(provider.id().to_string());
    }

    let mut providers: Vec<ProviderResponse> = provider_ids
        .into_iter()
        .filter_map(|provider_id| {
            let provider_type = ProviderType::from_id(&provider_id)?;
            let config_entry = get_provider_config_entry(&opencode_config, provider_type);
            let auth_kind = auth_map.get(&provider_type).copied();
            Some(build_provider_response(
                provider_type,
                config_entry,
                auth_kind,
                default_provider,
            ))
        })
        .collect();

    providers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(providers))
}

/// POST /api/ai/providers - Create a new provider.
async fn create_provider(
    State(state): State<Arc<super::routes::AppState>>,
    Json(req): Json<CreateProviderRequest>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    if req.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
    }

    // Validate base URL if provided
    if let Some(ref url) = req.base_url {
        if url::Url::parse(url).is_err() {
            return Err((StatusCode::BAD_REQUEST, "Invalid URL format".to_string()));
        }
    }

    let provider_type = req.provider_type;
    let config_path = get_opencode_config_path(&state.config.working_dir);
    let mut opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    set_provider_config_entry(
        &mut opencode_config,
        provider_type,
        Some(req.name),
        Some(req.base_url),
        Some(req.enabled),
    );

    write_opencode_config(&config_path, &opencode_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if let Some(ref api_key) = req.api_key {
        if let Err(e) = sync_api_key_to_opencode_auth(provider_type, api_key) {
            tracing::error!("Failed to sync API key to OpenCode: {}", e);
        }
    }

    let auth_kind = if req.api_key.is_some() {
        Some(AuthKind::ApiKey)
    } else {
        None
    };
    let default_provider = read_default_provider_state(&state.config.working_dir)
        .or_else(|| get_default_provider(&opencode_config));
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let response =
        build_provider_response(provider_type, config_entry, auth_kind, default_provider);

    tracing::info!(
        "Created AI provider config for: {} ({})",
        response.name,
        response.provider_type
    );

    Ok(Json(response))
}

/// GET /api/ai/providers/:id - Get provider details.
async fn get_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;
    let config_path = get_opencode_config_path(&state.config.working_dir);
    let opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let auth_map = read_opencode_auth_map().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let default_provider = read_default_provider_state(&state.config.working_dir)
        .or_else(|| get_default_provider(&opencode_config));
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let response =
        build_provider_response(provider_type, config_entry, auth_kind, default_provider);
    Ok(Json(response))
}

/// PUT /api/ai/providers/:id - Update a provider.
async fn update_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    if let Some(ref name) = req.name {
        if name.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
        }
    }

    if let Some(base_url) = req.base_url.as_ref() {
        if let Some(ref url) = base_url {
            if url::Url::parse(url).is_err() {
                return Err((StatusCode::BAD_REQUEST, "Invalid URL format".to_string()));
            }
        }
    }

    let config_path = get_opencode_config_path(&state.config.working_dir);
    let mut opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    set_provider_config_entry(
        &mut opencode_config,
        provider_type,
        req.name,
        req.base_url,
        req.enabled,
    );

    write_opencode_config(&config_path, &opencode_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if let Some(api_key_update) = req.api_key {
        match api_key_update {
            Some(api_key) => {
                if let Err(e) = sync_api_key_to_opencode_auth(provider_type, &api_key) {
                    tracing::error!("Failed to sync API key to OpenCode: {}", e);
                }
            }
            None => {
                if let Err(e) = remove_opencode_auth_entry(provider_type) {
                    tracing::error!("Failed to remove OpenCode auth entry: {}", e);
                }
            }
        }
    }

    let auth_map = read_opencode_auth_map().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let default_provider = read_default_provider_state(&state.config.working_dir)
        .or_else(|| get_default_provider(&opencode_config));
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let response =
        build_provider_response(provider_type, config_entry, auth_kind, default_provider);

    tracing::info!("Updated AI provider config: {} ({})", response.name, id);

    Ok(Json(response))
}

/// DELETE /api/ai/providers/:id - Delete a provider.
async fn delete_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;
    let config_path = get_opencode_config_path(&state.config.working_dir);
    let mut opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    remove_provider_config_entry(&mut opencode_config, provider_type);
    write_opencode_config(&config_path, &opencode_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if let Err(e) = remove_opencode_auth_entry(provider_type) {
        tracing::error!("Failed to remove OpenCode auth entry: {}", e);
    }

    if read_default_provider_state(&state.config.working_dir) == Some(provider_type) {
        if let Err(e) = clear_default_provider_state(&state.config.working_dir) {
            tracing::error!("Failed to clear default provider state: {}", e);
        }
    }

    Ok((
        StatusCode::OK,
        format!("Provider {} deleted successfully", id),
    ))
}

/// POST /api/ai/providers/:id/auth - Initiate authentication for a provider.
async fn authenticate_provider(
    State(_state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;
    let auth_map = read_opencode_auth_map().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // For OAuth providers, we need to return an auth URL
    if provider_type.uses_oauth() {
        let auth_url = match provider_type {
            ProviderType::Anthropic => {
                // For Anthropic/Claude, this would typically use Claude's OAuth flow
                // For now, we'll indicate that manual auth is needed
                Some("https://console.anthropic.com/settings/keys".to_string())
            }
            ProviderType::GithubCopilot => {
                // GitHub Copilot uses device code flow
                Some("https://github.com/login/device".to_string())
            }
            _ => None,
        };

        return Ok(Json(AuthResponse {
            success: false,
            message: format!(
                "Please authenticate with {} to connect this provider",
                provider_type.display_name()
            ),
            auth_url,
        }));
    }

    // For API key providers, check if key is set
    if auth_map.get(&provider_type) == Some(&AuthKind::ApiKey) {
        Ok(Json(AuthResponse {
            success: true,
            message: "Provider is authenticated".to_string(),
            auth_url: None,
        }))
    } else {
        Ok(Json(AuthResponse {
            success: false,
            message: "API key is required for this provider".to_string(),
            auth_url: None,
        }))
    }
}

/// POST /api/ai/providers/:id/default - Set as default provider.
async fn set_default(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;
    write_default_provider_state(&state.config.working_dir, provider_type)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let config_path = get_opencode_config_path(&state.config.working_dir);
    let opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let auth_map = read_opencode_auth_map().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let default_provider = Some(provider_type);
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let response =
        build_provider_response(provider_type, config_entry, auth_kind, default_provider);

    tracing::info!("Set default AI provider: {} ({})", response.name, id);

    Ok(Json(response))
}

/// GET /api/ai/providers/:id/auth/methods - Get available auth methods for a provider.
async fn get_auth_methods(
    State(_state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<AuthMethod>>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;
    Ok(Json(provider_type.auth_methods()))
}

/// Generate PKCE code verifier and challenge.
fn generate_pkce() -> (String, String) {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let verifier: String = (0..43)
        .map(|_| {
            let idx = rng.gen_range(0..62);
            let chars: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
            chars[idx] as char
        })
        .collect();

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(hash);

    (verifier, challenge)
}

/// Generate a random OAuth state value.
fn generate_state() -> String {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Parse OpenAI OAuth input (URL, code#state, query string, or code).
fn parse_openai_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(url) = url::Url::parse(value) {
        let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v);
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v);
        return (code.map(|v| v.to_string()), state.map(|v| v.to_string()));
    }

    if value.contains('#') {
        let mut parts = value.splitn(2, '#');
        let code = parts.next().map(|v| v.to_string());
        let state = parts.next().map(|v| v.to_string());
        return (code, state);
    }

    if value.contains("code=") {
        let params = url::form_urlencoded::parse(value.as_bytes())
            .into_owned()
            .collect::<HashMap<String, String>>();
        return (params.get("code").cloned(), params.get("state").cloned());
    }

    (Some(value.to_string()), None)
}

/// POST /api/ai/providers/:id/oauth/authorize - Initiate OAuth authorization.
async fn oauth_authorize(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<OAuthAuthorizeRequest>,
) -> Result<Json<OAuthAuthorizeResponse>, (StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    let auth_methods = provider_type.auth_methods();
    let method = auth_methods
        .get(req.method_index)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Invalid method index".to_string()))?;

    match provider_type {
        ProviderType::Anthropic => {
            // Generate PKCE
            let (verifier, challenge) = generate_pkce();

            // Determine mode based on method label
            let mode = if method.label.contains("Pro") || method.label.contains("Max") {
                "max"
            } else {
                "console"
            };

            // Build OAuth URL
            let base_url = if mode == "max" {
                "https://claude.ai/oauth/authorize"
            } else {
                "https://console.anthropic.com/oauth/authorize"
            };

            let mut url = url::Url::parse(base_url).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to parse URL: {}", e),
                )
            })?;
            let client_id = anthropic_client_id();
            let redirect_uri = anthropic_redirect_uri(mode, &client_id);

            url.query_pairs_mut()
                .append_pair("code", "true")
                .append_pair("client_id", &client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", "org:create_api_key user:profile user:inference")
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &verifier);

            // Store pending OAuth
            {
                let mut pending = state.pending_oauth.write().await;
                pending.insert(
                    provider_type,
                    PendingOAuth {
                        verifier,
                        mode: mode.to_string(),
                        state: None,
                        created_at: std::time::Instant::now(),
                    },
                );
            }

            Ok(Json(OAuthAuthorizeResponse {
                url: url.to_string(),
                instructions: "Visit the link above and paste the authorization code here"
                    .to_string(),
                method: "code".to_string(),
            }))
        }
        ProviderType::OpenAI => {
            let (verifier, challenge) = generate_pkce();
            let state_value = generate_state();

            let url = openai_authorize_url(&challenge, &state_value)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

            let instructions = if method.label.contains("Manual") {
                "After logging in, copy the full redirect URL and paste it here".to_string()
            } else {
                "A browser window should open. If it doesn't, copy the URL and open it manually."
                    .to_string()
            };

            {
                let mut pending = state.pending_oauth.write().await;
                pending.insert(
                    provider_type,
                    PendingOAuth {
                        verifier,
                        mode: "openai".to_string(),
                        state: Some(state_value),
                        created_at: std::time::Instant::now(),
                    },
                );
            }

            Ok(Json(OAuthAuthorizeResponse {
                url,
                instructions,
                method: "code".to_string(),
            }))
        }
        ProviderType::Google => {
            let (verifier, challenge) = generate_pkce();
            let state_value = generate_state();

            let url = google_authorize_url(&challenge, &state_value)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

            {
                let mut pending = state.pending_oauth.write().await;
                pending.insert(
                    provider_type,
                    PendingOAuth {
                        verifier,
                        mode: "google".to_string(),
                        state: Some(state_value),
                        created_at: std::time::Instant::now(),
                    },
                );
            }

            Ok(Json(OAuthAuthorizeResponse {
                url,
                instructions:
                    "Complete OAuth in your browser, then paste the full redirected URL (e.g., http://localhost:8085/oauth2callback?code=...&state=...) or just the authorization code."
                        .to_string(),
                method: "code".to_string(),
            }))
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            "OAuth not supported for this provider".to_string(),
        )),
    }
}

/// POST /api/ai/providers/:id/oauth/callback - Exchange OAuth code for credentials.
async fn oauth_callback(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<OAuthCallbackRequest>,
) -> axum::response::Response {
    match oauth_callback_inner(State(state), AxumPath(id), Json(req)).await {
        Ok(json) => json.into_response(),
        Err((status, message)) => (status, message).into_response(),
    }
}

async fn oauth_callback_inner(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<OAuthCallbackRequest>,
) -> Result<Json<ProviderResponse>, (axum::http::StatusCode, String)> {
    let provider_type = ProviderType::from_id(&id).ok_or_else(|| {
        (
            axum::http::StatusCode::NOT_FOUND,
            format!("Provider {} not found", id),
        )
    })?;

    // Get pending OAuth state
    let pending = {
        let mut pending_oauth = state.pending_oauth.write().await;
        pending_oauth.remove(&provider_type)
    }
    .ok_or_else(|| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            "No pending OAuth authorization. Please start the OAuth flow again.".to_string(),
        )
    })?;

    // Check if OAuth hasn't expired (10 minutes)
    if pending.created_at.elapsed() > std::time::Duration::from_secs(600) {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "OAuth authorization expired. Please start again.".to_string(),
        ));
    }

    match provider_type {
        ProviderType::Anthropic => {
            let client_id = anthropic_client_id();
            let redirect_uri = anthropic_redirect_uri(&pending.mode, &client_id);
            // Exchange code for tokens
            let code = req.code.clone();
            let splits: Vec<&str> = code.split('#').collect();
            let code_part = splits.first().copied().unwrap_or(&code);
            let state_part = splits.get(1).copied();

            let client = reqwest::Client::new();
            let token_response = client
                .post("https://console.anthropic.com/v1/oauth/token")
                .json(&serde_json::json!({
                    "code": code_part,
                    "state": state_part,
                    "grant_type": "authorization_code",
                    "client_id": client_id,
                    "redirect_uri": redirect_uri,
                    "code_verifier": pending.verifier
                }))
                .send()
                .await
                .map_err(|e| {
                    (
                        axum::http::StatusCode::BAD_GATEWAY,
                        format!("Failed to exchange code: {}", e),
                    )
                })?;

            if !token_response.status().is_success() {
                let error_text = token_response.text().await.unwrap_or_default();
                return Err((
                    axum::http::StatusCode::BAD_GATEWAY,
                    format!("OAuth token exchange failed: {}", error_text),
                ));
            }

            let token_data: serde_json::Value = token_response.json().await.map_err(|e| {
                (
                    axum::http::StatusCode::BAD_GATEWAY,
                    format!("Failed to parse token response: {}", e),
                )
            })?;

            let auth_methods = provider_type.auth_methods();
            let method = auth_methods.get(req.method_index);

            // Check if this is "Create an API Key" method
            let is_create_api_key = method
                .map(|m| m.label.contains("Create") && m.label.contains("API Key"))
                .unwrap_or(false);

            if is_create_api_key {
                // Create an API key using the access token
                let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                    (
                        StatusCode::BAD_GATEWAY,
                        "No access token in response".to_string(),
                    )
                })?;

                let api_key_response = client
                    .post("https://api.anthropic.com/api/oauth/claude_cli/create_api_key")
                    .header("Authorization", format!("Bearer {}", access_token))
                    .header("Content-Type", "application/json")
                    .send()
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::BAD_GATEWAY,
                            format!("Failed to create API key: {}", e),
                        )
                    })?;

                if !api_key_response.status().is_success() {
                    let error_text = api_key_response.text().await.unwrap_or_default();
                    return Err((
                        StatusCode::BAD_GATEWAY,
                        format!("API key creation failed: {}", error_text),
                    ));
                }

                let api_key_data: serde_json::Value =
                    api_key_response.json().await.map_err(|e| {
                        (
                            StatusCode::BAD_GATEWAY,
                            format!("Failed to parse API key response: {}", e),
                        )
                    })?;

                let api_key = api_key_data["raw_key"].as_str().ok_or_else(|| {
                    (
                        StatusCode::BAD_GATEWAY,
                        "No API key in response".to_string(),
                    )
                })?;

                // Store the API key
                if let Err(e) = sync_api_key_to_opencode_auth(provider_type, api_key) {
                    tracing::error!("Failed to sync API key to OpenCode: {}", e);
                }

                let config_path = get_opencode_config_path(&state.config.working_dir);
                let opencode_config = read_opencode_config(&config_path)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
                let default_provider = get_default_provider(&opencode_config);
                let config_entry = get_provider_config_entry(&opencode_config, provider_type);
                let response = build_provider_response(
                    provider_type,
                    config_entry,
                    Some(AuthKind::ApiKey),
                    default_provider,
                );

                tracing::info!("Created API key for provider: {} ({})", response.name, id);

                Ok(Json(response))
            } else {
                // Store OAuth credentials (Claude Pro/Max mode)
                let refresh_token = token_data["refresh_token"].as_str().ok_or_else(|| {
                    (
                        StatusCode::BAD_GATEWAY,
                        "No refresh token in response".to_string(),
                    )
                })?;

                let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                    (
                        StatusCode::BAD_GATEWAY,
                        "No access token in response".to_string(),
                    )
                })?;

                let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
                let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

                tracing::info!(
                    "OAuth credentials saved for provider: {} ({})",
                    provider_type,
                    id
                );

                // Sync to OpenCode's auth.json so OpenCode can use these credentials
                if let Err(e) =
                    sync_to_opencode_auth(provider_type, refresh_token, access_token, expires_at)
                {
                    tracing::error!("Failed to sync credentials to OpenCode: {}", e);
                    // Don't fail the request, but log the error
                }

                let config_path = get_opencode_config_path(&state.config.working_dir);
                let opencode_config = read_opencode_config(&config_path)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
                let default_provider = get_default_provider(&opencode_config);
                let config_entry = get_provider_config_entry(&opencode_config, provider_type);
                let response = build_provider_response(
                    provider_type,
                    config_entry,
                    Some(AuthKind::OAuth),
                    default_provider,
                );

                Ok(Json(response))
            }
        }
        ProviderType::OpenAI => {
            let (code_opt, state_opt) = parse_openai_authorization_input(&req.code);
            let Some(code) = code_opt else {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Authorization code not found. Paste the full redirect URL or code."
                        .to_string(),
                ));
            };

            if let (Some(expected), Some(actual)) = (pending.state.as_ref(), state_opt.as_ref()) {
                if expected != actual {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "OAuth state mismatch. Please start the OAuth flow again.".to_string(),
                    ));
                }
            }

            let client = reqwest::Client::new();
            let token_body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "authorization_code")
                .append_pair("client_id", OPENAI_CLIENT_ID)
                .append_pair("code", &code)
                .append_pair("code_verifier", &pending.verifier)
                .append_pair("redirect_uri", OPENAI_REDIRECT_URI)
                .finish();

            let token_response = client
                .post(OPENAI_TOKEN_URL)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(token_body)
                .send()
                .await
                .map_err(|e| {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("Failed to exchange code: {}", e),
                    )
                })?;

            if !token_response.status().is_success() {
                let error_text = token_response.text().await.unwrap_or_default();
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("OAuth token exchange failed: {}", error_text),
                ));
            }

            let token_data: serde_json::Value = token_response.json().await.map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Failed to parse token response: {}", e),
                )
            })?;

            let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                (
                    axum::http::StatusCode::BAD_GATEWAY,
                    "No access token in response".to_string(),
                )
            })?;

            let refresh_token = token_data["refresh_token"].as_str().ok_or_else(|| {
                (
                    axum::http::StatusCode::BAD_GATEWAY,
                    "No refresh token in response".to_string(),
                )
            })?;

            let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
            let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

            if let Err(e) =
                sync_to_opencode_auth(provider_type, refresh_token, access_token, expires_at)
            {
                tracing::error!("Failed to sync credentials to OpenCode: {}", e);
            }

            let config_path = get_opencode_config_path(&state.config.working_dir);
            let opencode_config = read_opencode_config(&config_path)
                .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e))?;
            let default_provider = get_default_provider(&opencode_config);
            let config_entry = get_provider_config_entry(&opencode_config, provider_type);
            let response = build_provider_response(
                provider_type,
                config_entry,
                Some(AuthKind::OAuth),
                default_provider,
            );

            Ok(Json(response))
        }
        ProviderType::Google => {
            // Parse the callback input (URL or code)
            let (code_opt, state_opt) = parse_openai_authorization_input(&req.code);
            let Some(code) = code_opt else {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Authorization code not found. Paste the full redirect URL or code."
                        .to_string(),
                ));
            };

            // Validate state if present
            if let (Some(expected), Some(actual)) = (pending.state.as_ref(), state_opt.as_ref()) {
                if expected != actual {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "OAuth state mismatch. Please start the OAuth flow again.".to_string(),
                    ));
                }
            }

            // Exchange code for tokens
            let client = reqwest::Client::new();
            let client_id = google_client_id();
            let client_secret = google_client_secret();
            let token_body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("client_id", &client_id)
                .append_pair("client_secret", &client_secret)
                .append_pair("code", &code)
                .append_pair("grant_type", "authorization_code")
                .append_pair("redirect_uri", GOOGLE_REDIRECT_URI)
                .append_pair("code_verifier", &pending.verifier)
                .finish();

            let token_response = client
                .post(GOOGLE_TOKEN_URL)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(token_body)
                .send()
                .await
                .map_err(|e| {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("Failed to exchange code: {}", e),
                    )
                })?;

            if !token_response.status().is_success() {
                let error_text = token_response.text().await.unwrap_or_default();
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("OAuth token exchange failed: {}", error_text),
                ));
            }

            let token_data: serde_json::Value = token_response.json().await.map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Failed to parse token response: {}", e),
                )
            })?;

            let access_token = token_data["access_token"].as_str().ok_or_else(|| {
                (
                    StatusCode::BAD_GATEWAY,
                    "No access token in response".to_string(),
                )
            })?;

            let refresh_token = token_data["refresh_token"].as_str().ok_or_else(|| {
                (
                    StatusCode::BAD_GATEWAY,
                    "No refresh token in response".to_string(),
                )
            })?;

            let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
            let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

            // Sync to OpenCode's auth.json
            if let Err(e) =
                sync_to_opencode_auth(provider_type, refresh_token, access_token, expires_at)
            {
                tracing::error!("Failed to sync Google credentials to OpenCode: {}", e);
            }

            let config_path = get_opencode_config_path(&state.config.working_dir);
            let opencode_config = read_opencode_config(&config_path)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            let default_provider = get_default_provider(&opencode_config);
            let config_entry = get_provider_config_entry(&opencode_config, provider_type);
            let response = build_provider_response(
                provider_type,
                config_entry,
                Some(AuthKind::OAuth),
                default_provider,
            );

            tracing::info!("Google OAuth credentials saved for provider: {}", id);

            Ok(Json(response))
        }
        _ => Err((
            axum::http::StatusCode::BAD_REQUEST,
            "OAuth not supported for this provider".to_string(),
        )),
    }
}
