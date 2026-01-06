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

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::ai_providers::{AIProvider, ProviderStatus, ProviderType};

/// Create AI provider routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_providers))
        .route("/", post(create_provider))
        .route("/types", get(list_provider_types))
        .route("/:id", get(get_provider))
        .route("/:id", put(update_provider))
        .route("/:id", delete(delete_provider))
        .route("/:id/auth", post(authenticate_provider))
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
    pub id: Uuid,
    pub provider_type: ProviderType,
    pub provider_type_name: String,
    pub name: String,
    pub has_api_key: bool,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub uses_oauth: bool,
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

impl From<ProviderStatus> for ProviderStatusResponse {
    fn from(status: ProviderStatus) -> Self {
        match status {
            ProviderStatus::Unknown => Self::Unknown,
            ProviderStatus::Connected => Self::Connected,
            ProviderStatus::NeedsAuth => Self::NeedsAuth { auth_url: None },
            ProviderStatus::Error(msg) => Self::Error { message: msg },
        }
    }
}

impl From<AIProvider> for ProviderResponse {
    fn from(p: AIProvider) -> Self {
        Self {
            id: p.id,
            provider_type: p.provider_type,
            provider_type_name: p.provider_type.display_name().to_string(),
            name: p.name,
            has_api_key: p.api_key.is_some(),
            base_url: p.base_url,
            enabled: p.enabled,
            is_default: p.is_default,
            uses_oauth: p.provider_type.uses_oauth(),
            status: p.status.into(),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub success: bool,
    pub message: String,
    /// OAuth URL to redirect user to (if OAuth flow required)
    pub auth_url: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

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
            uses_oauth: false,
            env_var: Some("OPENAI_API_KEY".to_string()),
        },
        ProviderTypeInfo {
            id: "google".to_string(),
            name: "Google AI".to_string(),
            uses_oauth: false,
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
    let providers = state.ai_providers.list().await;
    let responses: Vec<ProviderResponse> = providers.into_iter().map(Into::into).collect();
    Ok(Json(responses))
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

    let mut provider = AIProvider::new(req.provider_type, req.name);
    provider.api_key = req.api_key;
    provider.base_url = req.base_url;
    provider.enabled = req.enabled;

    // Set initial status
    if provider.provider_type.uses_oauth() && provider.api_key.is_none() {
        provider.status = ProviderStatus::NeedsAuth;
    } else if provider.api_key.is_some() {
        provider.status = ProviderStatus::Connected;
    }

    let id = state.ai_providers.add(provider.clone()).await;

    tracing::info!(
        "Created AI provider: {} ({}) [{}]",
        provider.name,
        provider.provider_type,
        id
    );

    // Refresh to get updated is_default flag
    let updated = state.ai_providers.get(id).await.unwrap_or(provider);

    Ok(Json(updated.into()))
}

/// GET /api/ai/providers/:id - Get provider details.
async fn get_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    state
        .ai_providers
        .get(id)
        .await
        .map(|p| Json(p.into()))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))
}

/// PUT /api/ai/providers/:id - Update a provider.
async fn update_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateProviderRequest>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    let mut provider = state
        .ai_providers
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    if let Some(name) = req.name {
        if name.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
        }
        provider.name = name;
    }

    if let Some(api_key) = req.api_key {
        provider.api_key = api_key;
        // Update status based on credentials
        if provider.api_key.is_some() {
            provider.status = ProviderStatus::Connected;
        } else if provider.provider_type.uses_oauth() {
            provider.status = ProviderStatus::NeedsAuth;
        }
    }

    if let Some(base_url) = req.base_url {
        if let Some(ref url) = base_url {
            if url::Url::parse(url).is_err() {
                return Err((StatusCode::BAD_REQUEST, "Invalid URL format".to_string()));
            }
        }
        provider.base_url = base_url;
    }

    if let Some(enabled) = req.enabled {
        provider.enabled = enabled;
    }

    let updated = state
        .ai_providers
        .update(id, provider)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    tracing::info!("Updated AI provider: {} ({})", updated.name, id);

    Ok(Json(updated.into()))
}

/// DELETE /api/ai/providers/:id - Delete a provider.
async fn delete_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    if state.ai_providers.delete(id).await {
        Ok((
            StatusCode::OK,
            format!("Provider {} deleted successfully", id),
        ))
    } else {
        Err((StatusCode::NOT_FOUND, format!("Provider {} not found", id)))
    }
}

/// POST /api/ai/providers/:id/auth - Initiate authentication for a provider.
async fn authenticate_provider(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AuthResponse>, (StatusCode, String)> {
    let provider = state
        .ai_providers
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    // For OAuth providers, we need to return an auth URL
    if provider.provider_type.uses_oauth() {
        let auth_url = match provider.provider_type {
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
                provider.provider_type.display_name()
            ),
            auth_url,
        }));
    }

    // For API key providers, check if key is set
    if provider.api_key.is_some() {
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
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ProviderResponse>, (StatusCode, String)> {
    if !state.ai_providers.set_default(id).await {
        return Err((StatusCode::NOT_FOUND, format!("Provider {} not found", id)));
    }

    let provider = state
        .ai_providers
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Provider {} not found", id)))?;

    tracing::info!("Set default AI provider: {} ({})", provider.name, id);

    Ok(Json(provider.into()))
}
