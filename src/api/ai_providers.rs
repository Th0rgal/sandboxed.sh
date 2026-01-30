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
//! - Get provider credentials for specific backend (Claude Code)

use std::collections::{BTreeSet, HashMap};
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

/// Google/Gemini OAuth constants (from opencode-gemini-auth plugin / Gemini CLI)
const GOOGLE_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const GOOGLE_CLIENT_SECRET: &str = "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";
const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_REDIRECT_URI: &str = "http://localhost:8085/oauth2callback";
const GOOGLE_SCOPES: &str =
    "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";

fn google_client_id() -> &'static str {
    GOOGLE_CLIENT_ID
}

fn google_client_secret() -> &'static str {
    GOOGLE_CLIENT_SECRET
}

fn anthropic_client_id() -> String {
    ANTHROPIC_CLIENT_ID
        .strip_prefix("urn:uuid:")
        .unwrap_or(ANTHROPIC_CLIENT_ID)
        .to_string()
}

/// Default localhost port for Claude Max/Pro OAuth callback.
/// This matches what Claude Code uses. Since there's no server listening,
/// the user copies the redirect URL from their browser's address bar.
const ANTHROPIC_MAX_REDIRECT_PORT: u16 = 9876;

fn anthropic_redirect_uri(mode: &str, _client_id: &str) -> String {
    if mode == "max" {
        format!("http://localhost:{}/callback", ANTHROPIC_MAX_REDIRECT_PORT)
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
        .route("/for-backend/:backend_id", get(get_provider_for_backend))
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
// Public API for Backend Access
// ─────────────────────────────────────────────────────────────────────────────

/// Claude Code authentication material.
#[derive(Debug, Clone)]
pub enum ClaudeCodeAuth {
    ApiKey(String),
    OAuthToken(String),
}

/// Claude Code authentication with expiry info for comparing freshness.
#[derive(Debug, Clone)]
pub struct ClaudeCodeAuthWithExpiry {
    pub auth: ClaudeCodeAuth,
    /// Expiry timestamp in milliseconds. None for API keys (never expire).
    pub expires_at: Option<i64>,
}

/// Get the Anthropic API key or OAuth access token for the Claude Code backend.
///
/// This checks if the Anthropic provider has "claudecode" in its use_for_backends
/// configuration and returns the API key or OAuth access token if available.
///
/// Credential sources checked (in order):
/// 1. OpenCode auth.json (API key or OAuth)
/// 2. Open Agent ai_providers.json (API key or OAuth)
///
/// Returns None if:
/// - Anthropic provider is not configured for claudecode
/// - No credentials are available (neither API key nor OAuth)
/// - Any error occurs reading the config
pub fn get_anthropic_auth_for_claudecode(working_dir: &Path) -> Option<ClaudeCodeAuth> {
    // Read the provider backends state to check use_for_backends
    let backends_state = read_provider_backends_state(working_dir);
    tracing::debug!(
        working_dir = %working_dir.display(),
        backends_state = ?backends_state,
        "Claude Code auth lookup: read provider backends state"
    );

    // Check if Anthropic provider has claudecode in use_for_backends
    let anthropic_backends = backends_state.get(ProviderType::Anthropic.id());
    let use_for_claudecode = anthropic_backends
        .map(|backends| backends.iter().any(|b| b == "claudecode"))
        .unwrap_or(false);
    tracing::debug!(
        anthropic_backends = ?anthropic_backends,
        use_for_claudecode = use_for_claudecode,
        "Claude Code auth lookup: checked backends"
    );

    if !use_for_claudecode {
        tracing::debug!("Claude Code not in Anthropic backends, trying fallback auth sources");
        if let Some(auth) = get_anthropic_auth_from_opencode_auth()
            .or_else(|| get_anthropic_auth_from_ai_providers(working_dir))
            .or_else(get_anthropic_auth_from_claude_cli_credentials)
        {
            tracing::warn!(
                "Anthropic credentials found but not marked for Claude Code; using them anyway"
            );
            return Some(auth);
        }
        tracing::debug!("No Anthropic credentials found in fallback sources");
        return None;
    }

    // Try to get credentials from OpenCode auth.json first
    if let Some(auth) = get_anthropic_auth_from_opencode_auth() {
        tracing::debug!("Found Anthropic credentials in OpenCode auth.json");
        return Some(auth);
    }
    tracing::debug!("No Anthropic credentials in OpenCode auth.json, trying ai_providers.json");

    // Fall back to ai_providers.json
    if let Some(auth) = get_anthropic_auth_from_ai_providers(working_dir) {
        return Some(auth);
    }
    tracing::debug!(
        "No Anthropic credentials found in ai_providers.json, trying Claude CLI credentials"
    );

    // Fall back to Claude CLI's own credentials file
    let result = get_anthropic_auth_from_claude_cli_credentials();
    if result.is_none() {
        tracing::debug!("No Anthropic credentials found in Claude CLI credentials either");
    }
    result
}

/// Get Anthropic auth from a workspace's OpenCode auth file.
///
/// For container workspaces, the auth is stored inside the container filesystem at:
/// `<workspace_root>/root/.opencode/auth/anthropic.json`
///
/// This function handles:
/// - Container workspaces: checks `<workspace_root>/root/.opencode/auth/anthropic.json`
/// - Host workspaces: checks nothing (standard paths are handled by get_anthropic_auth_from_opencode_auth)
///
/// Returns auth with expiry info to enable freshness comparison.
pub fn get_anthropic_auth_from_workspace(
    workspace_root: &std::path::Path,
) -> Option<ClaudeCodeAuthWithExpiry> {
    // For container workspaces, look inside the container's root filesystem
    // The auth file is at: <workspace_root>/root/.opencode/auth/anthropic.json
    let auth_path = workspace_root
        .join("root")
        .join(".opencode")
        .join("auth")
        .join("anthropic.json");

    if !auth_path.exists() {
        tracing::debug!(
            auth_path = %auth_path.display(),
            "No workspace auth file found"
        );
        return None;
    }

    tracing::debug!(
        auth_path = %auth_path.display(),
        "Found workspace auth file"
    );

    let contents = match std::fs::read_to_string(&auth_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                auth_path = %auth_path.display(),
                error = %e,
                "Failed to read workspace auth file"
            );
            return None;
        }
    };

    let anthropic_auth: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                auth_path = %auth_path.display(),
                error = %e,
                "Failed to parse workspace auth file"
            );
            return None;
        }
    };

    // Extract expiry timestamp (for OAuth tokens)
    let expires_at = anthropic_auth.get("expires").and_then(|v| v.as_i64());

    // Check auth type and extract credentials
    let auth_type = anthropic_auth.get("type").and_then(|v| v.as_str());

    // Determine if this is an OAuth token (for expiry handling)
    let is_oauth = matches!(auth_type, Some("oauth"))
        || (auth_type.is_none()
            && anthropic_auth.get("access").is_some()
            && anthropic_auth.get("key").is_none());

    let auth = match auth_type {
        Some("api_key") | Some("api") => anthropic_auth
            .get("key")
            .or_else(|| anthropic_auth.get("api_key"))
            .and_then(|v| v.as_str())
            .map(|s| ClaudeCodeAuth::ApiKey(s.to_string())),
        Some("oauth") => anthropic_auth
            .get("access")
            .and_then(|v| v.as_str())
            .map(|s| ClaudeCodeAuth::OAuthToken(s.to_string())),
        _ => {
            // Try key first, then OAuth access token
            if let Some(key) = anthropic_auth.get("key").and_then(|v| v.as_str()) {
                Some(ClaudeCodeAuth::ApiKey(key.to_string()))
            } else {
                anthropic_auth
                    .get("access")
                    .and_then(|v| v.as_str())
                    .map(|s| ClaudeCodeAuth::OAuthToken(s.to_string()))
            }
        }
    };

    auth.map(|a| ClaudeCodeAuthWithExpiry {
        auth: a,
        // API keys don't expire, OAuth tokens have expiry
        expires_at: if is_oauth { expires_at } else { None },
    })
}

/// Get the path to the workspace auth file for container workspaces.
pub fn get_workspace_auth_path(workspace_root: &std::path::Path) -> std::path::PathBuf {
    workspace_root
        .join("root")
        .join(".opencode")
        .join("auth")
        .join("anthropic.json")
}

/// Read an OAuth token entry from a container workspace's OpenCode auth file.
fn read_oauth_entry_from_workspace_auth(
    workspace_root: &std::path::Path,
) -> Option<OAuthTokenEntry> {
    let auth_path = get_workspace_auth_path(workspace_root);
    if !auth_path.exists() {
        return None;
    }

    let contents = std::fs::read_to_string(&auth_path).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&contents).ok()?;

    let auth_type = auth.get("type").and_then(|v| v.as_str());
    if auth_type != Some("oauth") {
        return None;
    }

    let refresh_token = auth.get("refresh").and_then(|v| v.as_str())?;
    let access_token = auth.get("access").and_then(|v| v.as_str()).unwrap_or("");
    let expires_at = auth.get("expires").and_then(|v| v.as_i64()).unwrap_or(0);

    tracing::debug!(
        auth_path = %auth_path.display(),
        expires_at = expires_at,
        "Found OAuth token entry in container workspace auth"
    );

    Some(OAuthTokenEntry {
        refresh_token: refresh_token.to_string(),
        access_token: access_token.to_string(),
        expires_at,
    })
}

/// Get Anthropic auth from host OpenCode auth.json with expiry info.
pub fn get_anthropic_auth_from_host_with_expiry() -> Option<ClaudeCodeAuthWithExpiry> {
    let entry = read_oauth_token_entry(ProviderType::Anthropic)?;

    // If there's an OAuth entry with access token
    if !entry.access_token.is_empty() {
        return Some(ClaudeCodeAuthWithExpiry {
            auth: ClaudeCodeAuth::OAuthToken(entry.access_token),
            expires_at: Some(entry.expires_at),
        });
    }

    // Otherwise try to get auth from OpenCode auth.json (might be API key)
    get_anthropic_auth_from_opencode_auth().map(|auth| ClaudeCodeAuthWithExpiry {
        auth,
        expires_at: None, // API keys don't expire
    })
}

/// Refresh an expired workspace Anthropic OAuth token.
/// Reads the refresh token from the workspace auth file, refreshes it via Anthropic API,
/// and writes the new token back to the same file.
pub async fn refresh_workspace_anthropic_auth(
    workspace_root: &std::path::Path,
) -> Result<ClaudeCodeAuthWithExpiry, String> {
    let auth_path = get_workspace_auth_path(workspace_root);
    if !auth_path.exists() {
        return Err("No workspace auth file found".to_string());
    }

    // Read the current auth file
    let contents = std::fs::read_to_string(&auth_path)
        .map_err(|e| format!("Failed to read workspace auth file: {}", e))?;
    let anthropic_auth: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse workspace auth file: {}", e))?;

    // Check if it's OAuth and get the refresh token
    let auth_type = anthropic_auth.get("type").and_then(|v| v.as_str());
    if auth_type != Some("oauth") {
        return Err("Workspace auth is not OAuth".to_string());
    }

    let refresh_token = anthropic_auth
        .get("refresh")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No refresh token in workspace auth".to_string())?;

    tracing::info!(
        workspace_path = %workspace_root.display(),
        "Refreshing expired workspace Anthropic OAuth token"
    );

    // Exchange refresh token for new access token
    let client = reqwest::Client::new();
    let token_response = client
        .post("https://console.anthropic.com/v1/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", ANTHROPIC_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let error_text = token_response.text().await.unwrap_or_default();
        tracing::error!(
            "Workspace token refresh failed with status {}: {}",
            status,
            error_text
        );
        // If invalid_grant, delete the stale workspace auth file
        let lower = error_text.to_lowercase();
        if (status == reqwest::StatusCode::BAD_REQUEST
            || status == reqwest::StatusCode::UNAUTHORIZED)
            && lower.contains("invalid_grant")
        {
            if let Err(e) = std::fs::remove_file(&auth_path) {
                tracing::warn!(
                    path = %auth_path.display(),
                    error = %e,
                    "Failed to remove invalid workspace auth file"
                );
            } else {
                tracing::info!(
                    path = %auth_path.display(),
                    "Removed invalid workspace auth file"
                );
            }
        }
        return Err(format!(
            "Token refresh failed ({}): {}. You may need to re-authenticate.",
            status, error_text
        ));
    }

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let new_access_token = token_data["access_token"]
        .as_str()
        .ok_or_else(|| "No access token in refresh response".to_string())?;

    let new_refresh_token = token_data["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token); // Use old refresh token if not provided

    let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    // Write the new token back to the workspace auth file
    let new_auth = serde_json::json!({
        "type": "oauth",
        "access": new_access_token,
        "refresh": new_refresh_token,
        "expires": expires_at
    });

    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create workspace auth directory: {}", e))?;
    }

    let contents = serde_json::to_string_pretty(&new_auth)
        .map_err(|e| format!("Failed to serialize auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write workspace auth file: {}", e))?;

    // Also sync to host paths so future missions can use it
    if let Err(e) = sync_to_opencode_auth(
        ProviderType::Anthropic,
        new_refresh_token,
        new_access_token,
        expires_at,
    ) {
        tracing::warn!("Failed to sync refreshed token to host paths: {}", e);
    }

    tracing::info!(
        workspace_path = %workspace_root.display(),
        "Successfully refreshed workspace Anthropic OAuth token, expires in {} seconds",
        expires_in
    );

    Ok(ClaudeCodeAuthWithExpiry {
        auth: ClaudeCodeAuth::OAuthToken(new_access_token.to_string()),
        expires_at: Some(expires_at),
    })
}

/// Get Anthropic API key or OAuth access token from OpenCode auth.json.
fn get_anthropic_auth_from_opencode_auth() -> Option<ClaudeCodeAuth> {
    let auth = match read_opencode_auth() {
        Ok(a) => a,
        Err(e) => {
            tracing::debug!("Failed to read OpenCode auth.json: {}", e);
            return None;
        }
    };
    let anthropic_auth = match auth.get("anthropic") {
        Some(a) => a,
        None => {
            tracing::debug!(
                "No 'anthropic' key in OpenCode auth.json (keys: {:?})",
                auth.as_object().map(|o| o.keys().collect::<Vec<_>>())
            );
            return None;
        }
    };

    // Check for API key first
    let auth_type = anthropic_auth.get("type").and_then(|v| v.as_str());
    match auth_type {
        Some("api_key") | Some("api") => anthropic_auth
            .get("key")
            .or_else(|| anthropic_auth.get("api_key"))
            .and_then(|v| v.as_str())
            .map(|s| ClaudeCodeAuth::ApiKey(s.to_string())),
        Some("oauth") => {
            // Return OAuth access token - Claude CLI can use this
            anthropic_auth
                .get("access")
                .and_then(|v| v.as_str())
                .map(|s| ClaudeCodeAuth::OAuthToken(s.to_string()))
        }
        _ => {
            // Check without type field - try key first, then OAuth access token
            if let Some(key) = anthropic_auth.get("key").and_then(|v| v.as_str()) {
                return Some(ClaudeCodeAuth::ApiKey(key.to_string()));
            }
            // Fall back to OAuth access token
            anthropic_auth
                .get("access")
                .and_then(|v| v.as_str())
                .map(|s| ClaudeCodeAuth::OAuthToken(s.to_string()))
        }
    }
}

/// Get Anthropic API key or OAuth access token from Open Agent's ai_providers.json.
fn get_anthropic_auth_from_ai_providers(working_dir: &Path) -> Option<ClaudeCodeAuth> {
    let ai_providers_path = working_dir.join(".sandboxed-sh/ai_providers.json");
    if !ai_providers_path.exists() {
        return None;
    }

    let contents = std::fs::read_to_string(&ai_providers_path).ok()?;
    let providers: Vec<serde_json::Value> = serde_json::from_str(&contents).ok()?;

    // Find Anthropic provider
    for provider in providers {
        let provider_type = provider.get("provider_type").and_then(|v| v.as_str());
        if provider_type != Some("anthropic") {
            continue;
        }

        // Check for API key first
        if let Some(api_key) = provider.get("api_key").and_then(|v| v.as_str()) {
            if !api_key.is_empty() {
                return Some(ClaudeCodeAuth::ApiKey(api_key.to_string()));
            }
        }

        // Check for OAuth access token
        if let Some(oauth) = provider.get("oauth") {
            if let Some(access_token) = oauth.get("access_token").and_then(|v| v.as_str()) {
                if !access_token.is_empty() {
                    return Some(ClaudeCodeAuth::OAuthToken(access_token.to_string()));
                }
            }
        }
    }

    None
}

/// Get Anthropic auth from Claude CLI's own credentials file.
///
/// The Claude CLI stores OAuth credentials in `~/.claude/.credentials.json` with format:
/// ```json
/// {
///   "claudeAiOauth": {
///     "accessToken": "sk-ant-oat01-...",
///     "expiresAt": 1769395897294,
///     "refreshToken": "sk-ant-ort01-...",
///     "scopes": ["user:inference", "user:profile"]
///   }
/// }
/// ```
///
/// This function checks multiple possible locations:
/// - /var/lib/opencode/.claude/.credentials.json (isolated OpenCode home)
/// - /root/.claude/.credentials.json (standard root home)
/// - $HOME/.claude/.credentials.json (current user's home)
fn get_anthropic_auth_from_claude_cli_credentials() -> Option<ClaudeCodeAuth> {
    let locations = [
        // OpenCode isolated home (used when OPENCODE_CONFIG_DIR is set)
        std::path::PathBuf::from("/var/lib/opencode/.claude/.credentials.json"),
        // Standard root home
        std::path::PathBuf::from("/root/.claude/.credentials.json"),
    ];

    // Also try HOME env var
    let home_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".claude/.credentials.json"));

    for path in locations.iter().chain(home_path.iter()) {
        if !path.exists() {
            continue;
        }

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "Failed to read Claude CLI credentials file"
                );
                continue;
            }
        };

        let creds: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "Failed to parse Claude CLI credentials file"
                );
                continue;
            }
        };

        // Look for claudeAiOauth.accessToken
        if let Some(oauth) = creds.get("claudeAiOauth") {
            if let Some(access_token) = oauth.get("accessToken").and_then(|v| v.as_str()) {
                if !access_token.is_empty() {
                    tracing::info!(
                        path = %path.display(),
                        "Found Anthropic OAuth token in Claude CLI credentials file"
                    );
                    return Some(ClaudeCodeAuth::OAuthToken(access_token.to_string()));
                }
            }
        }
    }

    None
}

/// Check if the Anthropic provider is configured for the Claude Code backend.
pub fn is_anthropic_configured_for_claudecode(working_dir: &Path) -> bool {
    let backends_state = read_provider_backends_state(working_dir);
    backends_state
        .get(ProviderType::Anthropic.id())
        .map(|backends| backends.iter().any(|b| b == "claudecode"))
        .unwrap_or(false)
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
    /// Optional Google Cloud project ID (for Google provider)
    #[serde(default)]
    pub google_project_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Which backends this provider is used for (e.g., ["opencode", "claudecode"])
    /// Only applicable for Anthropic provider. Defaults to ["opencode"].
    #[serde(default)]
    pub use_for_backends: Option<Vec<String>>,
    /// Custom models for custom providers
    #[serde(default)]
    pub custom_models: Option<Vec<crate::ai_providers::CustomModel>>,
    /// Custom environment variable name for API key (for custom providers)
    #[serde(default)]
    pub custom_env_var: Option<String>,
    /// NPM package for custom provider (defaults to @ai-sdk/openai-compatible)
    #[serde(default)]
    pub npm_package: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    /// Optional Google Cloud project ID update (for Google provider)
    pub google_project_id: Option<Option<String>>,
    pub api_key: Option<Option<String>>,
    pub base_url: Option<Option<String>>,
    pub enabled: Option<bool>,
    /// Which backends this provider is used for (e.g., ["opencode", "claudecode"])
    pub use_for_backends: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ProviderResponse {
    pub id: String,
    pub provider_type: ProviderType,
    pub provider_type_name: String,
    pub name: String,
    pub google_project_id: Option<String>,
    pub has_api_key: bool,
    pub has_oauth: bool,
    pub base_url: Option<String>,
    /// Custom models for custom providers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_models: Option<Vec<crate::ai_providers::CustomModel>>,
    /// Custom environment variable name for API key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_env_var: Option<String>,
    /// NPM package for custom provider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm_package: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub uses_oauth: bool,
    pub auth_methods: Vec<AuthMethod>,
    pub status: ProviderStatusResponse,
    /// Which backends this provider is used for (e.g., ["opencode", "claudecode"])
    pub use_for_backends: Vec<String>,
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
    google_project_id: Option<String>,
}

fn build_provider_response(
    provider_type: ProviderType,
    config: Option<ProviderConfigEntry>,
    auth: Option<AuthKind>,
    default_provider: Option<ProviderType>,
    backends: Option<Vec<String>>,
) -> ProviderResponse {
    let now = chrono::Utc::now();
    let name = config
        .as_ref()
        .and_then(|c| c.name.clone())
        .unwrap_or_else(|| provider_type.display_name().to_string());
    let base_url = config.as_ref().and_then(|c| c.base_url.clone());
    let enabled = config.as_ref().and_then(|c| c.enabled).unwrap_or(true);
    let google_project_id = config.as_ref().and_then(|c| c.google_project_id.clone());
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

    // For Anthropic, use configured backends or default to ["opencode"]
    // For other providers, always use ["opencode"]
    let use_for_backends = if provider_type == ProviderType::Anthropic {
        backends.unwrap_or_else(|| vec!["opencode".to_string()])
    } else {
        vec!["opencode".to_string()]
    };

    ProviderResponse {
        id: provider_type.id().to_string(),
        provider_type,
        provider_type_name: provider_type.display_name().to_string(),
        name,
        google_project_id,
        has_api_key: matches!(auth, Some(AuthKind::ApiKey)),
        has_oauth: matches!(auth, Some(AuthKind::OAuth)),
        base_url,
        custom_models: None,
        custom_env_var: None,
        npm_package: None,
        enabled,
        is_default,
        uses_oauth: provider_type.uses_oauth(),
        auth_methods: provider_type.auth_methods(),
        status,
        use_for_backends,
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

/// Response for provider credentials for a specific backend.
#[derive(Debug, Serialize)]
pub struct BackendProviderResponse {
    /// Whether a provider is configured for this backend
    pub configured: bool,
    /// The provider type (e.g., "anthropic")
    pub provider_type: Option<String>,
    /// The provider name
    pub provider_name: Option<String>,
    /// API key (if using API key auth)
    pub api_key: Option<String>,
    /// OAuth credentials (if using OAuth)
    pub oauth: Option<BackendOAuthCredentials>,
    /// Whether the provider has valid credentials
    pub has_credentials: bool,
}

/// OAuth credentials for backend provider.
#[derive(Debug, Serialize)]
pub struct BackendOAuthCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
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
    /// Which backends to use this provider for (e.g., ["opencode", "claudecode"])
    pub use_for_backends: Option<Vec<String>>,
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

    // Map our provider type to OpenCode's key(s)
    let keys = opencode_auth_keys(provider_type);
    if keys.is_empty() {
        return Err("Provider does not map to an OpenCode auth key".to_string());
    }

    // Create the auth entry in OpenCode format
    let entry = serde_json::json!({
        "type": "oauth",
        "refresh": refresh_token,
        "access": access_token,
        "expires": expires_at
    });

    for key in &keys {
        auth.insert((*key).to_string(), entry.clone());
    }

    // Write back to file
    let contents = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;

    if matches!(
        provider_type,
        ProviderType::OpenAI | ProviderType::Anthropic | ProviderType::Google
    ) {
        if let Err(e) = write_opencode_provider_auth_file(provider_type, &entry) {
            tracing::error!("Failed to write OpenCode provider auth file: {}", e);
        }
    }

    tracing::info!(
        "Synced OAuth credentials to OpenCode auth.json for provider keys: {:?}",
        keys
    );

    // Also write to Open Agent's canonical credential store
    if let Err(e) =
        write_sandboxed_credential(provider_type, refresh_token, access_token, expires_at)
    {
        tracing::warn!("Failed to write Open Agent credentials: {}", e);
    }

    // For Anthropic, also sync to Claude Code's .credentials.json
    if matches!(provider_type, ProviderType::Anthropic) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let claude_dir = PathBuf::from(home).join(".claude");
        if let Err(e) = write_claudecode_credentials_from_entry(
            &claude_dir,
            access_token,
            refresh_token,
            expires_at,
        ) {
            tracing::warn!("Failed to sync Claude Code credentials: {}", e);
        }
    }

    Ok(())
}

/// Write Claude Code credentials from explicit values (avoids re-reading from auth.json).
fn write_claudecode_credentials_from_entry(
    credentials_dir: &std::path::Path,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
) -> Result<(), String> {
    let credentials_path = credentials_dir.join(".credentials.json");

    std::fs::create_dir_all(credentials_dir)
        .map_err(|e| format!("Failed to create Claude credentials directory: {}", e))?;

    // Read-modify-write to preserve other entries in the credentials file
    let mut credentials: serde_json::Value = if credentials_path.exists() {
        let existing = std::fs::read_to_string(&credentials_path)
            .map_err(|e| format!("Failed to read Claude credentials: {}", e))?;
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    credentials["claudeAiOauth"] = serde_json::json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at,
        "scopes": ["user:inference", "user:profile"]
    });

    let contents = serde_json::to_string_pretty(&credentials)
        .map_err(|e| format!("Failed to serialize Claude credentials: {}", e))?;

    std::fs::write(&credentials_path, contents)
        .map_err(|e| format!("Failed to write Claude credentials: {}", e))?;

    tracing::info!(
        path = %credentials_path.display(),
        expires_at = expires_at,
        "Synced Claude Code credentials from token refresh"
    );

    Ok(())
}

#[derive(Debug, Clone)]
struct OAuthTokenEntry {
    refresh_token: String,
    access_token: String,
    expires_at: i64,
}

/// Path to Open Agent's canonical credential store.
fn get_sandboxed_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".sandboxed-sh")
        .join("credentials.json")
}

/// Read an OAuth credential from Open Agent's canonical credential store.
/// The file uses the same format as OpenCode's auth.json:
/// ```json
/// {
///   "anthropic": { "type": "oauth", "refresh": "...", "access": "...", "expires": 123 }
/// }
/// ```
fn read_sandboxed_credential(provider_type: ProviderType) -> Option<OAuthTokenEntry> {
    let path = get_sandboxed_credentials_path();
    if !path.exists() {
        return None;
    }

    let contents = std::fs::read_to_string(&path).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&contents).ok()?;

    for key in opencode_auth_keys(provider_type) {
        let entry = match auth.get(key) {
            Some(entry) => entry,
            None => continue,
        };
        if entry.get("type").and_then(|v| v.as_str()) != Some("oauth") {
            continue;
        }
        let refresh_token = match entry.get("refresh").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let access_token = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
        let expires_at = entry.get("expires").and_then(|v| v.as_i64()).unwrap_or(0);

        tracing::debug!(
            provider = ?provider_type,
            path = %path.display(),
            expires_at = expires_at,
            "Found OAuth token in Open Agent credentials"
        );

        return Some(OAuthTokenEntry {
            refresh_token: refresh_token.to_string(),
            access_token: access_token.to_string(),
            expires_at,
        });
    }

    None
}

/// Write an OAuth credential to Open Agent's canonical credential store.
/// Read-modify-write to preserve entries for other providers.
fn write_sandboxed_credential(
    provider_type: ProviderType,
    refresh_token: &str,
    access_token: &str,
    expires_at: i64,
) -> Result<(), String> {
    let path = get_sandboxed_credentials_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create ~/.sandboxed-sh directory: {}", e))?;
    }

    let mut auth: serde_json::Map<String, serde_json::Value> = if path.exists() {
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read Open Agent credentials: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    let entry = serde_json::json!({
        "type": "oauth",
        "refresh": refresh_token,
        "access": access_token,
        "expires": expires_at
    });

    let keys = opencode_auth_keys(provider_type);
    for key in &keys {
        auth.insert((*key).to_string(), entry.clone());
    }

    let contents = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize Open Agent credentials: {}", e))?;
    std::fs::write(&path, contents)
        .map_err(|e| format!("Failed to write Open Agent credentials: {}", e))?;

    tracing::info!(
        path = %path.display(),
        keys = ?keys,
        "Synced OAuth credentials to Open Agent credentials.json"
    );

    Ok(())
}

/// Remove a provider entry from Open Agent's credential store.
fn remove_sandboxed_credential(provider_type: ProviderType) -> Result<(), String> {
    let path = get_sandboxed_credentials_path();
    if !path.exists() {
        return Ok(());
    }

    let mut auth: serde_json::Map<String, serde_json::Value> = {
        let contents = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read Open Agent credentials: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    };

    let keys = opencode_auth_keys(provider_type);
    let mut changed = false;
    for key in &keys {
        if auth.remove(*key).is_some() {
            changed = true;
        }
    }

    if changed {
        let contents = serde_json::to_string_pretty(&auth)
            .map_err(|e| format!("Failed to serialize Open Agent credentials: {}", e))?;
        std::fs::write(&path, contents)
            .map_err(|e| format!("Failed to write Open Agent credentials: {}", e))?;
    }

    Ok(())
}

/// Read Anthropic OAuth credentials from Claude Code's `.credentials.json`.
/// Checks `$HOME/.claude/.credentials.json` and `/var/lib/opencode/.claude/.credentials.json`.
/// Parses the `claudeAiOauth` format and converts to `OAuthTokenEntry`.
fn read_anthropic_from_claude_credentials() -> Option<OAuthTokenEntry> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let candidates = vec![
        PathBuf::from(&home)
            .join(".claude")
            .join(".credentials.json"),
        PathBuf::from("/var/lib/opencode")
            .join(".claude")
            .join(".credentials.json"),
    ];

    for path in candidates {
        if !path.exists() {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let creds: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let oauth = match creds.get("claudeAiOauth") {
            Some(v) => v,
            None => continue,
        };

        let refresh_token = match oauth.get("refreshToken").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let access_token = oauth
            .get("accessToken")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let expires_at = oauth.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);

        tracing::debug!(
            path = %path.display(),
            expires_at = expires_at,
            "Found Anthropic OAuth token in Claude credentials"
        );

        return Some(OAuthTokenEntry {
            refresh_token: refresh_token.to_string(),
            access_token: access_token.to_string(),
            expires_at,
        });
    }

    None
}

fn read_oauth_token_entry(provider_type: ProviderType) -> Option<OAuthTokenEntry> {
    // Tier 1: Open Agent's canonical credential store
    if let Some(entry) = read_sandboxed_credential(provider_type) {
        return Some(entry);
    }

    // Tier 2: OpenCode auth.json paths (legacy / external auth flows)
    if let Some(entry) = read_from_opencode_auth_paths(provider_type) {
        // Auto-sync to Open Agent's file so future reads hit tier 1
        let _ = write_sandboxed_credential(
            provider_type,
            &entry.refresh_token,
            &entry.access_token,
            entry.expires_at,
        );
        return Some(entry);
    }

    // Tier 3: Claude .credentials.json (Anthropic only, from Claude CLI auth)
    if matches!(provider_type, ProviderType::Anthropic) {
        if let Some(entry) = read_anthropic_from_claude_credentials() {
            // Auto-sync to Open Agent's file
            let _ = write_sandboxed_credential(
                provider_type,
                &entry.refresh_token,
                &entry.access_token,
                entry.expires_at,
            );
            return Some(entry);
        }
    }

    None
}

/// Read an OAuth token entry from OpenCode auth.json paths (tier 2 fallback).
fn read_from_opencode_auth_paths(provider_type: ProviderType) -> Option<OAuthTokenEntry> {
    let auth_paths = get_all_opencode_auth_paths();

    for auth_path in auth_paths {
        if !auth_path.exists() {
            continue;
        }

        let contents = match std::fs::read_to_string(&auth_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let auth: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(a) => a,
            Err(_) => continue,
        };

        for key in opencode_auth_keys(provider_type) {
            let entry = match auth.get(key) {
                Some(entry) => entry,
                None => continue,
            };
            let auth_type = entry.get("type").and_then(|v| v.as_str());
            if auth_type != Some("oauth") {
                continue;
            }

            let refresh_token = match entry.get("refresh").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => continue,
            };
            let access_token = entry.get("access").and_then(|v| v.as_str()).unwrap_or("");
            let expires_at = entry.get("expires").and_then(|v| v.as_i64()).unwrap_or(0);

            tracing::debug!(
                provider = ?provider_type,
                auth_path = %auth_path.display(),
                expires_at = expires_at,
                "Found OAuth token entry in OpenCode auth"
            );

            return Some(OAuthTokenEntry {
                refresh_token: refresh_token.to_string(),
                access_token: access_token.to_string(),
                expires_at,
            });
        }
    }

    None
}

/// Get all potential OpenCode auth.json paths to search.
fn get_all_opencode_auth_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        paths.push(PathBuf::from(data_home).join("opencode").join("auth.json"));
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    paths.push(
        PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );

    // OpenCode server's auth path (runs as opencode user)
    paths.push(
        PathBuf::from("/var/lib/opencode")
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );

    paths
}

fn oauth_token_expired(expires_at: i64) -> bool {
    let now = chrono::Utc::now().timestamp_millis();
    let buffer = 5 * 60 * 1000; // 5 minutes in milliseconds
    expires_at < (now + buffer)
}

fn is_oauth_token_expired(provider_type: ProviderType) -> bool {
    read_oauth_token_entry(provider_type)
        .map(|entry| oauth_token_expired(entry.expires_at))
        .unwrap_or(false)
}

/// Check if the Anthropic OAuth token is expired or about to expire.
/// Returns true if the token is expired or will expire in the next 5 minutes.
fn is_anthropic_oauth_token_expired() -> bool {
    is_oauth_token_expired(ProviderType::Anthropic)
}

/// Refresh the Anthropic OAuth token using the refresh token.
/// Updates auth.json with the new access token and expiry.
pub async fn refresh_anthropic_oauth_token() -> Result<(), String> {
    let entry = read_oauth_token_entry(ProviderType::Anthropic)
        .ok_or_else(|| "No Anthropic OAuth entry".to_string())?;
    let refresh_token = entry.refresh_token;

    tracing::info!("Refreshing Anthropic OAuth token");

    // Exchange refresh token for new access token
    let client = reqwest::Client::new();
    let token_response = client
        .post("https://console.anthropic.com/v1/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", ANTHROPIC_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let error_text = token_response.text().await.unwrap_or_default();
        tracing::error!(
            "Token refresh failed with status {}: {}",
            status,
            error_text
        );
        let lower = error_text.to_lowercase();
        if (status == reqwest::StatusCode::BAD_REQUEST
            || status == reqwest::StatusCode::UNAUTHORIZED)
            && lower.contains("invalid_grant")
        {
            if let Err(e) = remove_opencode_auth_entry(ProviderType::Anthropic) {
                tracing::warn!(
                    "Failed to clear Anthropic auth entry after invalid_grant: {}",
                    e
                );
            }
        }
        return Err(format!(
            "Token refresh failed ({}): {}. You may need to re-authenticate.",
            status, error_text
        ));
    }

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let new_access_token = token_data["access_token"]
        .as_str()
        .ok_or_else(|| "No access token in refresh response".to_string())?;

    let new_refresh_token = token_data["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token.as_str()); // Use old refresh token if not provided

    let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    // Update auth.json with new tokens
    sync_to_opencode_auth(
        ProviderType::Anthropic,
        new_refresh_token,
        new_access_token,
        expires_at,
    )?;

    tracing::info!(
        "Successfully refreshed Anthropic OAuth token, expires in {} seconds",
        expires_in
    );

    Ok(())
}

/// Ensure the Anthropic OAuth token is valid, refreshing if needed.
/// This should be called before starting a mission that uses Claude Code.
pub async fn ensure_anthropic_oauth_token_valid() -> Result<(), String> {
    if !is_anthropic_oauth_token_expired() {
        return Ok(());
    }

    tracing::info!("Anthropic OAuth token is expired or expiring soon, refreshing...");
    refresh_anthropic_oauth_token().await
}

/// Refresh the OpenAI OAuth token using the refresh token.
/// Updates auth.json with the new access token and expiry.
pub async fn refresh_openai_oauth_token() -> Result<(), String> {
    let entry = read_oauth_token_entry(ProviderType::OpenAI)
        .ok_or_else(|| "No OpenAI OAuth entry".to_string())?;
    let refresh_token = entry.refresh_token;

    tracing::info!("Refreshing OpenAI OAuth token");

    let client = reqwest::Client::new();
    let token_body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", OPENAI_CLIENT_ID)
        .append_pair("refresh_token", &refresh_token)
        .finish();

    let token_response = client
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let error_text = token_response.text().await.unwrap_or_default();
        tracing::error!(
            "OpenAI token refresh failed with status {}: {}",
            status,
            error_text
        );
        let lower = error_text.to_lowercase();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            if lower.contains("invalid_grant") || lower.contains("refresh_token_reused") {
                if let Err(e) = remove_opencode_auth_entry(ProviderType::OpenAI) {
                    tracing::warn!(
                        "Failed to clear OpenAI auth entry after refresh failure: {}",
                        e
                    );
                }
            }
        }
        return Err(format!(
            "Token refresh failed ({}): {}. You may need to re-authenticate.",
            status, error_text
        ));
    }

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let new_access_token = token_data["access_token"]
        .as_str()
        .ok_or_else(|| "No access token in refresh response".to_string())?;

    let new_refresh_token = token_data["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token.as_str());

    let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    sync_to_opencode_auth(
        ProviderType::OpenAI,
        new_refresh_token,
        new_access_token,
        expires_at,
    )?;

    tracing::info!(
        "Successfully refreshed OpenAI OAuth token, expires in {} seconds",
        expires_in
    );

    Ok(())
}

/// Ensure the OpenAI OAuth token is valid, refreshing if needed.
pub async fn ensure_openai_oauth_token_valid() -> Result<(), String> {
    if !is_oauth_token_expired(ProviderType::OpenAI) {
        return Ok(());
    }

    tracing::info!("OpenAI OAuth token is expired or expiring soon, refreshing...");
    refresh_openai_oauth_token().await
}

/// Refresh the Google OAuth token using the refresh token.
/// Updates auth.json with the new access token and expiry.
pub async fn refresh_google_oauth_token() -> Result<(), String> {
    let entry = read_oauth_token_entry(ProviderType::Google)
        .ok_or_else(|| "No Google OAuth entry".to_string())?;
    let refresh_token = entry.refresh_token;

    tracing::info!("Refreshing Google OAuth token");

    let client = reqwest::Client::new();
    let token_body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", google_client_id())
        .append_pair("client_secret", google_client_secret())
        .append_pair("refresh_token", &refresh_token)
        .append_pair("grant_type", "refresh_token")
        .finish();

    let token_response = client
        .post(GOOGLE_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(token_body)
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let error_text = token_response.text().await.unwrap_or_default();
        tracing::error!(
            "Google token refresh failed with status {}: {}",
            status,
            error_text
        );
        let lower = error_text.to_lowercase();
        if (status == reqwest::StatusCode::BAD_REQUEST
            || status == reqwest::StatusCode::UNAUTHORIZED)
            && lower.contains("invalid_grant")
        {
            if let Err(e) = remove_opencode_auth_entry(ProviderType::Google) {
                tracing::warn!(
                    "Failed to clear Google auth entry after invalid_grant: {}",
                    e
                );
            }
        }
        return Err(format!(
            "Token refresh failed ({}): {}. You may need to re-authenticate.",
            status, error_text
        ));
    }

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {}", e))?;

    let new_access_token = token_data["access_token"]
        .as_str()
        .ok_or_else(|| "No access token in refresh response".to_string())?;

    let new_refresh_token = token_data["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token.as_str());

    let expires_in = token_data["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    sync_to_opencode_auth(
        ProviderType::Google,
        new_refresh_token,
        new_access_token,
        expires_at,
    )?;

    tracing::info!(
        "Successfully refreshed Google OAuth token, expires in {} seconds",
        expires_in
    );

    Ok(())
}

/// Ensure the Google OAuth token is valid, refreshing if needed.
pub async fn ensure_google_oauth_token_valid() -> Result<(), String> {
    if !is_oauth_token_expired(ProviderType::Google) {
        return Ok(());
    }

    tracing::info!("Google OAuth token is expired or expiring soon, refreshing...");
    refresh_google_oauth_token().await
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude Code Credentials File
// ─────────────────────────────────────────────────────────────────────────────

/// Write OAuth credentials to Claude Code's credentials file.
///
/// Claude Code stores auth in `~/.claude/.credentials.json` with format:
/// ```json
/// {
///   "claudeAiOauth": {
///     "accessToken": "sk-ant-oat01-...",
///     "refreshToken": "sk-ant-ort01-...",
///     "expiresAt": 1748658860401,
///     "scopes": ["user:inference", "user:profile"]
///   }
/// }
/// ```
///
/// This allows Claude Code to refresh tokens automatically during long-running missions.
pub fn write_claudecode_credentials_to_path(
    credentials_dir: &std::path::Path,
) -> Result<(), String> {
    let entry = read_oauth_token_entry(ProviderType::Anthropic)
        .ok_or_else(|| "No Anthropic OAuth entry found".to_string())?;

    let credentials_path = credentials_dir.join(".credentials.json");

    // Ensure parent directory exists
    std::fs::create_dir_all(credentials_dir)
        .map_err(|e| format!("Failed to create Claude credentials directory: {}", e))?;

    // Read-modify-write to preserve other entries in the credentials file
    let mut credentials: serde_json::Value = if credentials_path.exists() {
        let existing = std::fs::read_to_string(&credentials_path)
            .map_err(|e| format!("Failed to read Claude credentials: {}", e))?;
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    credentials["claudeAiOauth"] = serde_json::json!({
        "accessToken": entry.access_token,
        "refreshToken": entry.refresh_token,
        "expiresAt": entry.expires_at,
        "scopes": ["user:inference", "user:profile"]
    });

    let contents = serde_json::to_string_pretty(&credentials)
        .map_err(|e| format!("Failed to serialize Claude credentials: {}", e))?;

    std::fs::write(&credentials_path, contents)
        .map_err(|e| format!("Failed to write Claude credentials: {}", e))?;

    tracing::info!(
        path = %credentials_path.display(),
        expires_at = entry.expires_at,
        "Wrote Claude Code credentials file with refresh token"
    );

    Ok(())
}

/// Write Claude Code credentials to a workspace.
///
/// For container workspaces, writes to the container's root home directory.
/// For host workspaces, writes to the host's home directory.
pub fn write_claudecode_credentials_for_workspace(
    workspace: &crate::workspace::Workspace,
) -> Result<(), String> {
    use crate::workspace::WorkspaceType;

    let entry = read_oauth_token_entry(ProviderType::Anthropic)
        .or_else(|| {
            if workspace.workspace_type == WorkspaceType::Container {
                if let Some(entry) = read_oauth_entry_from_workspace_auth(&workspace.path) {
                    // Best-effort sync so future reads hit the canonical store.
                    let _ = write_sandboxed_credential(
                        ProviderType::Anthropic,
                        &entry.refresh_token,
                        &entry.access_token,
                        entry.expires_at,
                    );
                    return Some(entry);
                }
            }
            None
        })
        .ok_or_else(|| "No Anthropic OAuth entry found".to_string())?;

    let claude_dir = match workspace.workspace_type {
        WorkspaceType::Container => {
            // Container workspaces: write to /root/.claude inside the container
            workspace.path.join("root").join(".claude")
        }
        WorkspaceType::Host => {
            // Host workspaces: write to $HOME/.claude
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            std::path::PathBuf::from(home).join(".claude")
        }
    };

    write_claudecode_credentials_from_entry(
        &claude_dir,
        &entry.access_token,
        &entry.refresh_token,
        entry.expires_at,
    )?;

    tracing::info!(
        workspace_type = ?workspace.workspace_type,
        claude_dir = %claude_dir.display(),
        expires_at = entry.expires_at,
        "Prepared Claude Code credentials for workspace"
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

    let keys = opencode_auth_keys(provider_type);
    if keys.is_empty() {
        return Ok(());
    }

    let entry = serde_json::json!({
        "type": "api_key",
        "key": api_key
    });

    for key in &keys {
        auth.insert((*key).to_string(), entry.clone());
    }

    let contents = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
    std::fs::write(&auth_path, contents)
        .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;

    if matches!(
        provider_type,
        ProviderType::OpenAI | ProviderType::Anthropic | ProviderType::Google
    ) {
        let provider_entry = serde_json::json!({
            "type": "api_key",
            "key": api_key
        });
        if let Err(e) = write_opencode_provider_auth_file(provider_type, &provider_entry) {
            tracing::error!("Failed to write OpenCode provider auth file: {}", e);
        }
    }

    tracing::info!(
        "Synced API key to OpenCode auth.json for provider keys: {:?}",
        keys
    );

    Ok(())
}

/// Remove a provider entry from OpenCode's auth.json file.
fn remove_opencode_auth_entry(provider_type: ProviderType) -> Result<(), String> {
    let auth_path = get_opencode_auth_path();
    if !auth_path.exists() {
        // Still attempt to remove provider-specific auth file if present.
        if matches!(
            provider_type,
            ProviderType::OpenAI | ProviderType::Anthropic | ProviderType::Google
        ) {
            let provider_path = get_opencode_provider_auth_path(provider_type);
            if provider_path.exists() {
                std::fs::remove_file(&provider_path)
                    .map_err(|e| format!("Failed to remove OpenCode provider auth: {}", e))?;
            }
        }
        // Also clean Open Agent's credential store
        let _ = remove_sandboxed_credential(provider_type);
        return Ok(());
    }

    let mut auth: serde_json::Map<String, serde_json::Value> = {
        let contents = std::fs::read_to_string(&auth_path)
            .map_err(|e| format!("Failed to read OpenCode auth: {}", e))?;
        serde_json::from_str(&contents).unwrap_or_default()
    };

    let keys = opencode_auth_keys(provider_type);
    if keys.is_empty() {
        return Ok(());
    }

    let mut changed = false;
    for key in &keys {
        if auth.remove(*key).is_some() {
            changed = true;
        }
    }

    if changed {
        let contents = serde_json::to_string_pretty(&auth)
            .map_err(|e| format!("Failed to serialize OpenCode auth: {}", e))?;
        std::fs::write(&auth_path, contents)
            .map_err(|e| format!("Failed to write OpenCode auth: {}", e))?;
    }

    if matches!(
        provider_type,
        ProviderType::OpenAI | ProviderType::Anthropic | ProviderType::Google
    ) {
        let provider_path = get_opencode_provider_auth_path(provider_type);
        if provider_path.exists() {
            std::fs::remove_file(&provider_path)
                .map_err(|e| format!("Failed to remove OpenCode provider auth: {}", e))?;
        }
    }

    // Also clean Open Agent's credential store
    if let Err(e) = remove_sandboxed_credential(provider_type) {
        tracing::warn!("Failed to remove Open Agent credential entry: {}", e);
    }

    Ok(())
}

/// Get the path to OpenCode's auth.json file.
fn get_opencode_auth_path() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(data_home).join("opencode").join("auth.json"));
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    candidates.push(
        PathBuf::from(&home)
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );
    candidates.push(
        PathBuf::from("/var/lib/opencode")
            .join(".local")
            .join("share")
            .join("opencode")
            .join("auth.json"),
    );

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("/var/lib/opencode/.local/share/opencode/auth.json"))
}

fn get_opencode_provider_auth_path(provider_type: ProviderType) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let mut candidates = vec![
        PathBuf::from(&home)
            .join(".opencode")
            .join("auth")
            .join(format!("{}.json", provider_type.id())),
        PathBuf::from("/var/lib/opencode")
            .join(".opencode")
            .join("auth")
            .join(format!("{}.json", provider_type.id())),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }

    candidates.into_iter().next().unwrap_or_else(|| {
        PathBuf::from(home)
            .join(".opencode")
            .join("auth")
            .join(format!("{}.json", provider_type.id()))
    })
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

fn opencode_auth_keys(provider_type: ProviderType) -> Vec<&'static str> {
    match provider_type {
        ProviderType::Custom => Vec::new(),
        ProviderType::OpenAI => vec!["openai", "codex"],
        _ => vec![provider_type.id()],
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

fn strip_sandboxed_key(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sandboxed");
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
        Ok(value) => Ok(strip_sandboxed_key(value)),
        Err(_) => {
            let stripped = strip_jsonc_comments(&contents);
            serde_json::from_str(&stripped)
                .map(strip_sandboxed_key)
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
    let google_project_id = if provider == ProviderType::Google {
        entry
            .get("options")
            .and_then(|v| v.as_object())
            .and_then(|opts| opts.get("projectId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    // Note: use_for_backends is now stored separately in .sandboxed-sh/provider_backends.json
    // and should be read using read_provider_backends_state() instead
    Some(ProviderConfigEntry {
        name,
        base_url,
        enabled,
        google_project_id,
    })
}

fn set_provider_config_entry(
    config: &mut serde_json::Value,
    provider: ProviderType,
    name: Option<String>,
    base_url: Option<Option<String>>,
    enabled: Option<bool>,
    use_for_backends: Option<Vec<String>>,
    google_project_id: Option<Option<String>>,
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

    // OpenCode's config schema doesn't accept "useForBackends" under provider entries.
    // This field is now stored separately in .sandboxed-sh/provider_backends.json.
    // Remove any existing useForBackends for migration/cleanup.
    let _ = use_for_backends;
    entry_obj.remove("useForBackends");

    if provider == ProviderType::Google {
        if let Some(project_id) = google_project_id {
            match project_id {
                Some(value) => {
                    let options = entry_obj
                        .entry("options".to_string())
                        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                    if let Some(options_obj) = options.as_object_mut() {
                        options_obj
                            .insert("projectId".to_string(), serde_json::Value::String(value));
                    }
                }
                None => {
                    if let Some(options) = entry_obj.get_mut("options") {
                        if let Some(options_obj) = options.as_object_mut() {
                            options_obj.remove("projectId");
                        }
                        if options.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                            entry_obj.remove("options");
                        }
                    }
                }
            }
        }
    }
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
    working_dir
        .join(".sandboxed-sh")
        .join("default_provider.json")
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

/// Path to the provider backends state file.
/// This stores which backends each provider is used for (e.g., opencode, claudecode).
/// This is stored separately from the OpenCode config because OpenCode doesn't recognize this field.
fn provider_backends_state_path(working_dir: &Path) -> PathBuf {
    working_dir
        .join(".sandboxed-sh")
        .join("provider_backends.json")
}

/// Read provider backends state from the separate state file.
/// Returns a map of provider_id -> backends (e.g., "anthropic" -> ["opencode", "claudecode"])
fn read_provider_backends_state(working_dir: &Path) -> HashMap<String, Vec<String>> {
    let path = provider_backends_state_path(working_dir);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let value: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return HashMap::new(),
    };
    obj.iter()
        .filter_map(|(k, v)| {
            v.as_array().map(|arr| {
                let backends: Vec<String> = arr
                    .iter()
                    .filter_map(|b| b.as_str().map(|s| s.to_string()))
                    .collect();
                (k.clone(), backends)
            })
        })
        .collect()
}

/// Write provider backends state to the separate state file.
fn write_provider_backends_state(
    working_dir: &Path,
    backends: &HashMap<String, Vec<String>>,
) -> Result<(), String> {
    let path = provider_backends_state_path(working_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create provider backends directory: {}", e))?;
    }
    let payload = serde_json::json!(backends);
    let contents = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize provider backends: {}", e))?;
    std::fs::write(path, contents)
        .map_err(|e| format!("Failed to write provider backends: {}", e))?;
    Ok(())
}

/// Update backends for a specific provider in the state file.
fn update_provider_backends(
    working_dir: &Path,
    provider_id: &str,
    backends: Vec<String>,
) -> Result<(), String> {
    let mut state = read_provider_backends_state(working_dir);
    state.insert(provider_id.to_string(), backends);
    write_provider_backends_state(working_dir, &state)
}

/// Remove a provider from the backends state file.
fn remove_provider_backends(working_dir: &Path, provider_id: &str) -> Result<(), String> {
    let mut state = read_provider_backends_state(working_dir);
    state.remove(provider_id);
    write_provider_backends_state(working_dir, &state)
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

    let keys = opencode_auth_keys(provider_type);
    if keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Provider {} does not map to OpenCode auth keys",
                req.provider
            ),
        ));
    }

    // Update the auth object
    if let Some(obj) = auth.as_object_mut() {
        for key in &keys {
            obj.insert((*key).to_string(), entry.clone());
        }
    } else {
        let mut map = serde_json::Map::new();
        for key in &keys {
            map.insert((*key).to_string(), entry.clone());
        }
        auth = serde_json::Value::Object(map);
    }

    // Write back to file
    write_opencode_auth(&auth).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if matches!(
        provider_type,
        ProviderType::OpenAI | ProviderType::Anthropic | ProviderType::Google
    ) {
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
            id: "zai".to_string(),
            name: "Z.AI".to_string(),
            uses_oauth: false,
            env_var: Some("ZHIPU_API_KEY".to_string()),
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
    let backends_state = read_provider_backends_state(&state.config.working_dir);

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
            let backends = backends_state.get(provider_type.id()).cloned();
            Some(build_provider_response(
                provider_type,
                config_entry,
                auth_kind,
                default_provider,
                backends,
            ))
        })
        .collect();

    // Also include custom providers from AIProviderStore
    let custom_providers = state.ai_providers.list().await;
    for provider in custom_providers {
        if provider.provider_type == ProviderType::Custom && provider.enabled {
            let now = chrono::Utc::now();
            providers.push(ProviderResponse {
                id: provider.id.to_string(),
                provider_type: ProviderType::Custom,
                provider_type_name: "Custom".to_string(),
                name: provider.name.clone(),
                google_project_id: None,
                has_api_key: provider.api_key.is_some(),
                has_oauth: false,
                base_url: provider.base_url.clone(),
                custom_models: provider.custom_models.clone(),
                custom_env_var: provider.custom_env_var.clone(),
                npm_package: provider.npm_package.clone(),
                enabled: provider.enabled,
                is_default: provider.is_default,
                uses_oauth: false,
                auth_methods: vec![],
                status: if provider.base_url.is_some() {
                    ProviderStatusResponse::Connected
                } else {
                    ProviderStatusResponse::NeedsAuth { auth_url: None }
                },
                use_for_backends: vec!["opencode".to_string()], // Custom providers work with OpenCode
                created_at: now,
                updated_at: now,
            });
        }
    }

    providers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(providers))
}

/// GET /api/ai/providers/for-backend/:backend_id - Get provider credentials for a specific backend.
///
/// For Claude Code backend, this returns the Anthropic provider that has "claudecode" in use_for_backends.
async fn get_provider_for_backend(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(backend_id): AxumPath<String>,
) -> Result<Json<BackendProviderResponse>, (StatusCode, String)> {
    // Currently only "claudecode" backend uses this endpoint
    if backend_id != "claudecode" {
        return Ok(Json(BackendProviderResponse {
            configured: false,
            provider_type: None,
            provider_name: None,
            api_key: None,
            oauth: None,
            has_credentials: false,
        }));
    }

    // Read the provider backends state to find provider with claudecode in use_for_backends
    let backends_state = read_provider_backends_state(&state.config.working_dir);

    // Check if Anthropic provider has claudecode in use_for_backends
    let use_for_claudecode = backends_state
        .get(ProviderType::Anthropic.id())
        .map(|backends| backends.iter().any(|b| b == "claudecode"))
        .unwrap_or(false);

    if !use_for_claudecode {
        return Ok(Json(BackendProviderResponse {
            configured: false,
            provider_type: None,
            provider_name: None,
            api_key: None,
            oauth: None,
            has_credentials: false,
        }));
    }

    // Get the Anthropic provider credentials from auth.json
    let auth = read_opencode_auth().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let anthropic_auth = auth.get("anthropic");

    let (api_key, oauth, has_credentials) = if let Some(auth_entry) = anthropic_auth {
        let auth_type = auth_entry.get("type").and_then(|v| v.as_str());
        match auth_type {
            Some("api_key") | Some("api") => {
                let key = auth_entry
                    .get("key")
                    .or_else(|| auth_entry.get("api_key"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                (key, None, true)
            }
            Some("oauth") => {
                let oauth_creds = BackendOAuthCredentials {
                    access_token: auth_entry
                        .get("access")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    refresh_token: auth_entry
                        .get("refresh")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    expires_at: auth_entry
                        .get("expires")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                };
                (None, Some(oauth_creds), true)
            }
            _ => {
                // Check for OAuth credentials without type field
                if auth_entry.get("refresh").is_some() {
                    let oauth_creds = BackendOAuthCredentials {
                        access_token: auth_entry
                            .get("access")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        refresh_token: auth_entry
                            .get("refresh")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        expires_at: auth_entry
                            .get("expires")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                    };
                    (None, Some(oauth_creds), true)
                } else if auth_entry.get("key").is_some() {
                    let key = auth_entry
                        .get("key")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (key, None, true)
                } else {
                    (None, None, false)
                }
            }
        }
    } else {
        (None, None, false)
    };

    // Get provider name from OpenCode config if available
    let config_path = get_opencode_config_path(&state.config.working_dir);
    let provider_name = read_opencode_config(&config_path)
        .ok()
        .and_then(|config| get_provider_config_entry(&config, ProviderType::Anthropic))
        .and_then(|entry| entry.name)
        .unwrap_or_else(|| "Anthropic".to_string());

    Ok(Json(BackendProviderResponse {
        configured: true,
        provider_type: Some("anthropic".to_string()),
        provider_name: Some(provider_name),
        api_key,
        oauth,
        has_credentials,
    }))
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

    // For custom providers, store in AIProviderStore (ai_providers.json)
    // so that workspace preparation can read custom models and base URL
    if provider_type == ProviderType::Custom {
        let mut provider = crate::ai_providers::AIProvider::new(provider_type, req.name.clone());
        provider.base_url = req.base_url.clone();
        provider.api_key = req.api_key.clone();
        provider.custom_models = req.custom_models.clone();
        provider.custom_env_var = req.custom_env_var.clone();
        provider.npm_package = req.npm_package.clone();
        provider.enabled = req.enabled;

        state.ai_providers.add(provider.clone()).await;

        tracing::info!(
            "Created custom AI provider: {} with {} models",
            req.name,
            req.custom_models.as_ref().map(|m| m.len()).unwrap_or(0)
        );

        // Return a response for custom provider
        let now = chrono::Utc::now();
        return Ok(Json(ProviderResponse {
            id: provider.id.to_string(),
            provider_type: ProviderType::Custom,
            provider_type_name: "Custom".to_string(),
            name: req.name,
            google_project_id: None,
            has_api_key: req.api_key.is_some(),
            has_oauth: false,
            base_url: req.base_url,
            custom_models: req.custom_models,
            custom_env_var: req.custom_env_var,
            npm_package: req.npm_package,
            enabled: req.enabled,
            is_default: false,
            uses_oauth: false,
            auth_methods: vec![],
            status: if req.api_key.is_some() || provider.base_url.is_some() {
                ProviderStatusResponse::Connected
            } else {
                ProviderStatusResponse::NeedsAuth { auth_url: None }
            },
            use_for_backends: vec!["opencode".to_string()], // Custom providers work with OpenCode
            created_at: now,
            updated_at: now,
        }));
    }

    let config_path = get_opencode_config_path(&state.config.working_dir);
    let mut opencode_config =
        read_opencode_config(&config_path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // For Anthropic, default use_for_backends to ["opencode"] if not specified
    let use_for_backends = if provider_type == ProviderType::Anthropic {
        req.use_for_backends
            .or_else(|| Some(vec!["opencode".to_string()]))
    } else {
        None
    };

    set_provider_config_entry(
        &mut opencode_config,
        provider_type,
        Some(req.name),
        Some(req.base_url),
        Some(req.enabled),
        use_for_backends.clone(),
        req.google_project_id.map(Some),
    );

    write_opencode_config(&config_path, &opencode_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Save backends to separate state file (not in opencode.json)
    if let Some(ref backends) = use_for_backends {
        if let Err(e) = update_provider_backends(
            &state.config.working_dir,
            provider_type.id(),
            backends.clone(),
        ) {
            tracing::error!("Failed to save provider backends: {}", e);
        }
    }

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
    let response = build_provider_response(
        provider_type,
        config_entry,
        auth_kind,
        default_provider,
        use_for_backends,
    );

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
    let backends_state = read_provider_backends_state(&state.config.working_dir);
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let backends = backends_state.get(provider_type.id()).cloned();
    let response = build_provider_response(
        provider_type,
        config_entry,
        auth_kind,
        default_provider,
        backends,
    );
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
        req.use_for_backends.clone(),
        req.google_project_id,
    );

    write_opencode_config(&config_path, &opencode_config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Save backends to separate state file if provided
    if let Some(ref backends) = req.use_for_backends {
        if let Err(e) = update_provider_backends(
            &state.config.working_dir,
            provider_type.id(),
            backends.clone(),
        ) {
            tracing::error!("Failed to save provider backends: {}", e);
        }
    }

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
    let backends_state = read_provider_backends_state(&state.config.working_dir);
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let backends = backends_state.get(provider_type.id()).cloned();
    let response = build_provider_response(
        provider_type,
        config_entry,
        auth_kind,
        default_provider,
        backends,
    );

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

    // Remove provider backends state
    if let Err(e) = remove_provider_backends(&state.config.working_dir, provider_type.id()) {
        tracing::error!("Failed to remove provider backends state: {}", e);
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
    let backends_state = read_provider_backends_state(&state.config.working_dir);
    let default_provider = Some(provider_type);
    let config_entry = get_provider_config_entry(&opencode_config, provider_type);
    let auth_kind = auth_map.get(&provider_type).copied();
    let backends = backends_state.get(provider_type.id()).cloned();
    let response = build_provider_response(
        provider_type,
        config_entry,
        auth_kind,
        default_provider,
        backends,
    );

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

            // Claude Max/Pro requires additional scope for sessions
            let scope = if mode == "max" {
                "org:create_api_key user:profile user:inference user:sessions:claude_code"
            } else {
                "org:create_api_key user:profile user:inference"
            };

            url.query_pairs_mut()
                .append_pair("code", "true")
                .append_pair("client_id", &client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", scope)
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

            let instructions = if mode == "max" {
                "1. Click 'Authorize' on the Claude page\n2. After authorization, your browser will redirect to a page that won't load (localhost)\n3. Copy the FULL URL from your browser's address bar\n4. Paste the URL here and click Connect"
            } else {
                "1. Click 'Authorize' on the Claude page\n2. Copy the authorization code shown\n3. Paste the code here and click Connect"
            };

            Ok(Json(OAuthAuthorizeResponse {
                url: url.to_string(),
                instructions: instructions.to_string(),
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

            // Parse the authorization input - could be:
            // 1. A full URL: http://localhost:9876/callback?code=...&state=...
            // 2. The old format: code#state
            // 3. Just the code
            let input = req.code.trim();
            let (code_string, state_string): (String, Option<String>) =
                if let Ok(url) = url::Url::parse(input) {
                    // Parse as URL
                    let code = url
                        .query_pairs()
                        .find(|(k, _)| k == "code")
                        .map(|(_, v)| v.to_string());
                    let state = url
                        .query_pairs()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.to_string());
                    (code.unwrap_or_default(), state)
                } else if input.contains('#') {
                    // Old format: code#state
                    let mut parts = input.splitn(2, '#');
                    let code = parts.next().unwrap_or(input).to_string();
                    let state = parts.next().map(|s| s.to_string());
                    (code, state)
                } else {
                    // Just the code
                    (input.to_string(), None)
                };

            if code_string.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Authorization code not found. Please paste the full URL from your browser's address bar.".to_string(),
                ));
            }

            let code_part = code_string.as_str();
            let state_part = state_string.as_deref();

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
                let mut opencode_config = read_opencode_config(&config_path)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

                // Update use_for_backends if specified
                if let Some(ref backends) = req.use_for_backends {
                    set_provider_config_entry(
                        &mut opencode_config,
                        provider_type,
                        None,
                        None,
                        None,
                        req.use_for_backends.clone(),
                        None,
                    );
                    if let Err(e) = write_opencode_config(&config_path, &opencode_config) {
                        tracing::error!("Failed to write OpenCode config: {}", e);
                    }
                    // Save backends to separate state file
                    if let Err(e) = update_provider_backends(
                        &state.config.working_dir,
                        provider_type.id(),
                        backends.clone(),
                    ) {
                        tracing::error!("Failed to save provider backends: {}", e);
                    }
                }

                let default_provider = get_default_provider(&opencode_config);
                let backends_state = read_provider_backends_state(&state.config.working_dir);
                let config_entry = get_provider_config_entry(&opencode_config, provider_type);
                let backends = backends_state.get(provider_type.id()).cloned();
                let response = build_provider_response(
                    provider_type,
                    config_entry,
                    Some(AuthKind::ApiKey),
                    default_provider,
                    backends,
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
                let mut opencode_config = read_opencode_config(&config_path)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

                // Update use_for_backends if specified
                if let Some(ref backends) = req.use_for_backends {
                    set_provider_config_entry(
                        &mut opencode_config,
                        provider_type,
                        None,
                        None,
                        None,
                        req.use_for_backends.clone(),
                        None,
                    );
                    if let Err(e) = write_opencode_config(&config_path, &opencode_config) {
                        tracing::error!("Failed to write OpenCode config: {}", e);
                    }
                    // Save backends to separate state file
                    if let Err(e) = update_provider_backends(
                        &state.config.working_dir,
                        provider_type.id(),
                        backends.clone(),
                    ) {
                        tracing::error!("Failed to save provider backends: {}", e);
                    }
                }

                let default_provider = get_default_provider(&opencode_config);
                let backends_state = read_provider_backends_state(&state.config.working_dir);
                let config_entry = get_provider_config_entry(&opencode_config, provider_type);
                let backends = backends_state.get(provider_type.id()).cloned();
                let response = build_provider_response(
                    provider_type,
                    config_entry,
                    Some(AuthKind::OAuth),
                    default_provider,
                    backends,
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
            let backends_state = read_provider_backends_state(&state.config.working_dir);
            let default_provider = get_default_provider(&opencode_config);
            let config_entry = get_provider_config_entry(&opencode_config, provider_type);
            let backends = backends_state.get(provider_type.id()).cloned();
            let response = build_provider_response(
                provider_type,
                config_entry,
                Some(AuthKind::OAuth),
                default_provider,
                backends,
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
            let backends_state = read_provider_backends_state(&state.config.working_dir);
            let default_provider = get_default_provider(&opencode_config);
            let config_entry = get_provider_config_entry(&opencode_config, provider_type);
            let backends = backends_state.get(provider_type.id()).cloned();
            let response = build_provider_response(
                provider_type,
                config_entry,
                Some(AuthKind::OAuth),
                default_provider,
                backends,
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
