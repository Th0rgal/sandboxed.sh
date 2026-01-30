//! Provider catalog API.
//!
//! Provides endpoints for listing available providers and their models for UI selection.
//! Only returns providers that are actually configured and authenticated.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use super::routes::AppState;
use crate::ai_providers::ProviderType;

/// A model available from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModel {
    /// Model identifier (e.g., "claude-opus-4-5-20251101")
    pub id: String,
    /// Human-readable name (e.g., "Claude Opus 4.5")
    pub name: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

/// A provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Provider identifier (e.g., "anthropic")
    pub id: String,
    /// Human-readable name (e.g., "Claude (Subscription)")
    pub name: String,
    /// Billing type: "subscription" or "pay-per-token"
    pub billing: String,
    /// Description of the provider
    pub description: String,
    /// Available models from this provider
    pub models: Vec<ProviderModel>,
}

/// Query parameters for providers endpoint.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProvidersQuery {
    /// Include providers even if they are not configured/authenticated.
    #[serde(default)]
    pub include_all: bool,
}

/// Response for the providers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersResponse {
    pub providers: Vec<Provider>,
}

/// Configuration file structure for providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub providers: Vec<Provider>,
}

/// Load providers configuration from file.
fn load_providers_config(working_dir: &str) -> ProvidersConfig {
    let config_path = format!("{}/.sandboxed-sh/providers.json", working_dir);

    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!("Failed to parse providers.json: {}. Using defaults.", e);
                default_providers_config()
            }
        },
        Err(_) => {
            tracing::info!(
                "No providers.json found at {}. Using defaults.",
                config_path
            );
            default_providers_config()
        }
    }
}

/// Default provider configuration.
fn default_providers_config() -> ProvidersConfig {
    ProvidersConfig {
        providers: vec![
            Provider {
                id: "anthropic".to_string(),
                name: "Claude (Subscription)".to_string(),
                billing: "subscription".to_string(),
                description: "Included in Claude Max".to_string(),
                models: vec![
                    ProviderModel {
                        id: "claude-opus-4-5-20251101".to_string(),
                        name: "Claude Opus 4.5".to_string(),
                        description: Some(
                            "Most capable, recommended for complex tasks".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-sonnet-4-20250514".to_string(),
                        name: "Claude Sonnet 4".to_string(),
                        description: Some("Good balance of speed and capability".to_string()),
                    },
                    ProviderModel {
                        id: "claude-3-5-haiku-20241022".to_string(),
                        name: "Claude Haiku 3.5".to_string(),
                        description: Some("Fastest, most economical".to_string()),
                    },
                ],
            },
            Provider {
                id: "openai".to_string(),
                name: "OpenAI (Subscription)".to_string(),
                billing: "subscription".to_string(),
                description: "ChatGPT Plus/Pro via OAuth".to_string(),
                models: vec![
                    ProviderModel {
                        id: "gpt-5.2-codex".to_string(),
                        name: "GPT-5.2 Codex".to_string(),
                        description: Some("Optimized for coding workflows".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.1-codex".to_string(),
                        name: "GPT-5.1 Codex".to_string(),
                        description: Some("Balanced capability and speed".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.1-codex-max".to_string(),
                        name: "GPT-5.1 Codex Max".to_string(),
                        description: Some("Highest reasoning capacity".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.1-codex-mini".to_string(),
                        name: "GPT-5.1 Codex Mini".to_string(),
                        description: Some("Fast and economical".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.2".to_string(),
                        name: "GPT-5.2".to_string(),
                        description: Some("General-purpose GPT-5.2".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.1".to_string(),
                        name: "GPT-5.1".to_string(),
                        description: Some("General-purpose GPT-5.1".to_string()),
                    },
                ],
            },
            Provider {
                id: "google".to_string(),
                name: "Google AI (OAuth)".to_string(),
                billing: "subscription".to_string(),
                description: "Gemini models via Google OAuth".to_string(),
                models: vec![
                    ProviderModel {
                        id: "gemini-2.5-pro-preview-06-05".to_string(),
                        name: "Gemini 2.5 Pro".to_string(),
                        description: Some("Most capable Gemini model".to_string()),
                    },
                    ProviderModel {
                        id: "gemini-2.5-flash-preview-05-20".to_string(),
                        name: "Gemini 2.5 Flash".to_string(),
                        description: Some("Fast and efficient".to_string()),
                    },
                    ProviderModel {
                        id: "gemini-3-flash-preview".to_string(),
                        name: "Gemini 3 Flash Preview".to_string(),
                        description: Some("Latest Gemini 3 preview".to_string()),
                    },
                ],
            },
            Provider {
                id: "xai".to_string(),
                name: "xAI (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "Grok models via xAI API key".to_string(),
                models: vec![
                    ProviderModel {
                        id: "grok-2".to_string(),
                        name: "Grok 2".to_string(),
                        description: Some("Most capable Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-2-mini".to_string(),
                        name: "Grok 2 Mini".to_string(),
                        description: Some("Faster, lighter Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-2-vision".to_string(),
                        name: "Grok 2 Vision".to_string(),
                        description: Some("Vision-capable Grok model".to_string()),
                    },
                ],
            },
        ],
    }
}

/// Check if a JSON value contains valid auth credentials.
fn has_valid_auth(value: &serde_json::Value) -> bool {
    // Check for OAuth tokens (various field names used by different providers)
    let has_oauth = value.get("refresh").is_some()
        || value.get("refresh_token").is_some()
        || value.get("access").is_some()
        || value.get("access_token").is_some();
    // Check for API key (various field names)
    let has_api_key = value.get("key").is_some()
        || value.get("api_key").is_some()
        || value.get("apiKey").is_some();
    has_oauth || has_api_key
}

/// Get the set of configured provider IDs from OpenCode's auth files.
fn get_configured_provider_ids(working_dir: &std::path::Path) -> HashSet<String> {
    let mut configured = HashSet::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());

    // 1. Read OpenCode auth.json (~/.local/share/opencode/auth.json)
    let auth_path = {
        let data_home = std::env::var("XDG_DATA_HOME").ok();
        let base = if let Some(data_home) = data_home {
            std::path::PathBuf::from(data_home).join("opencode")
        } else {
            std::path::PathBuf::from(&home).join(".local/share/opencode")
        };
        base.join("auth.json")
    };

    tracing::debug!("Checking OpenCode auth file: {:?}", auth_path);
    if let Ok(contents) = std::fs::read_to_string(&auth_path) {
        if let Ok(auth) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(map) = auth.as_object() {
                for (key, value) in map {
                    if has_valid_auth(value) {
                        tracing::debug!("Found valid auth for provider '{}' in auth.json", key);
                        let normalized = if key == "codex" { "openai" } else { key };
                        configured.insert(normalized.to_string());
                    }
                }
            }
        }
    }

    // 2. Check provider-specific auth files (~/.opencode/auth/{provider}.json)
    // This is where OpenAI stores its auth (separate from the main auth.json)
    let provider_auth_dir = std::path::PathBuf::from(&home).join(".opencode/auth");
    tracing::debug!("Checking provider auth dir: {:?}", provider_auth_dir);
    for provider_type in [
        ProviderType::Anthropic,
        ProviderType::OpenAI,
        ProviderType::Google,
        ProviderType::GithubCopilot,
        ProviderType::Xai,
    ] {
        let auth_file = provider_auth_dir.join(format!("{}.json", provider_type.id()));
        if let Ok(contents) = std::fs::read_to_string(&auth_file) {
            tracing::debug!(
                "Found auth file for {}: {:?}",
                provider_type.id(),
                auth_file
            );
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
                if has_valid_auth(&value) {
                    tracing::debug!(
                        "Found valid auth for provider '{}' in {:?}",
                        provider_type.id(),
                        auth_file
                    );
                    configured.insert(provider_type.id().to_string());
                }
            }
        }
    }

    // 3. Check Open Agent provider config (.sandboxed-sh/ai_providers.json)
    let ai_providers_path = working_dir.join(".sandboxed-sh").join("ai_providers.json");
    if let Ok(contents) = std::fs::read_to_string(&ai_providers_path) {
        if let Ok(providers) =
            serde_json::from_str::<Vec<crate::ai_providers::AIProvider>>(&contents)
        {
            for provider in providers {
                if provider.enabled && provider.has_credentials() {
                    configured.insert(provider.provider_type.id().to_string());
                }
            }
        }
    }

    tracing::debug!("Configured providers: {:?}", configured);
    configured
}

/// List available providers and their models.
///
/// Returns a list of providers with their available models, billing type,
/// and descriptions. Only includes providers that are actually configured
/// and authenticated. This endpoint is used by the frontend to render
/// a grouped model selector.
pub async fn list_providers(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProvidersQuery>,
) -> Json<ProvidersResponse> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let config = load_providers_config(&working_dir);

    // Get the set of configured provider IDs
    let configured = get_configured_provider_ids(state.config.working_dir.as_path());

    let providers = if query.include_all {
        config.providers
    } else {
        // Filter providers to only include those that are configured
        config
            .providers
            .into_iter()
            .filter(|p| configured.contains(&p.id))
            .collect()
    };

    Json(ProvidersResponse { providers })
}
