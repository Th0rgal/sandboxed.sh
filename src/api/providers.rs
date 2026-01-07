//! Provider catalog API.
//!
//! Provides endpoints for listing available providers and their models for UI selection.

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use super::routes::AppState;

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
    let config_path = format!("{}/.openagent/providers.json", working_dir);
    let legacy_path = format!("{}/.open_agent/providers.json", working_dir);

    let contents =
        std::fs::read_to_string(&config_path).or_else(|_| std::fs::read_to_string(&legacy_path));

    match contents {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!("Failed to parse providers.json: {}. Using defaults.", e);
                default_providers_config()
            }
        },
        Err(_) => {
            tracing::info!(
                "No providers.json found at {} or {}. Using defaults.",
                config_path,
                legacy_path
            );
            default_providers_config()
        }
    }
}

/// Default provider configuration.
fn default_providers_config() -> ProvidersConfig {
    ProvidersConfig {
        providers: vec![Provider {
            id: "anthropic".to_string(),
            name: "Claude (Subscription)".to_string(),
            billing: "subscription".to_string(),
            description: "Included in Claude Max".to_string(),
            models: vec![
                ProviderModel {
                    id: "claude-opus-4-5-20251101".to_string(),
                    name: "Claude Opus 4.5".to_string(),
                    description: Some("Most capable, recommended for complex tasks".to_string()),
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
        }],
    }
}

/// List available providers and their models.
///
/// Returns a list of providers with their available models, billing type,
/// and descriptions. This endpoint is used by the frontend to render
/// a grouped model selector.
pub async fn list_providers(State(state): State<Arc<AppState>>) -> Json<ProvidersResponse> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let config = load_providers_config(&working_dir);

    Json(ProvidersResponse {
        providers: config.providers,
    })
}
