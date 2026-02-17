//! Model routing API endpoints.
//!
//! Provides endpoints for managing model chains and viewing provider health:
//! - List/create/update/delete model chains
//! - View provider health status and cooldowns
//! - Resolve a chain into ordered entries (for debugging)
//! - Clear cooldowns
//! - RTK token savings stats

use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::provider_health::{ChainEntry, ModelChain};
use crate::tools::terminal::rtk_stats;

/// Register model routing routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        // Chain management
        .route("/chains", get(list_chains))
        .route("/chains", post(create_chain))
        .route("/chains/:id", get(get_chain))
        .route("/chains/:id", put(update_chain))
        .route("/chains/:id", delete(delete_chain))
        .route("/chains/:id/resolve", get(resolve_chain))
        // Health tracking
        .route("/health", get(list_health))
        .route("/health/:account_id", get(get_account_health))
        .route("/health/:account_id/clear", post(clear_cooldown))
        // Observability
        .route("/events", get(list_fallback_events))
        // RTK stats
        .route("/rtk-stats", get(get_rtk_stats))
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain Management
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChainResponse {
    id: String,
    name: String,
    entries: Vec<ChainEntryResponse>,
    is_default: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
struct ChainEntryResponse {
    provider_id: String,
    model_id: String,
}

impl From<ModelChain> for ChainResponse {
    fn from(chain: ModelChain) -> Self {
        Self {
            id: chain.id,
            name: chain.name,
            entries: chain
                .entries
                .into_iter()
                .map(|e| ChainEntryResponse {
                    provider_id: e.provider_id,
                    model_id: e.model_id,
                })
                .collect(),
            is_default: chain.is_default,
            created_at: chain.created_at,
            updated_at: chain.updated_at,
        }
    }
}

/// GET /api/model-routing/chains - List all model chains.
async fn list_chains(
    State(state): State<Arc<super::routes::AppState>>,
) -> Json<Vec<ChainResponse>> {
    let chains = state.chain_store.list().await;
    Json(chains.into_iter().map(ChainResponse::from).collect())
}

/// GET /api/model-routing/chains/:id - Get a specific chain.
async fn get_chain(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ChainResponse>, (StatusCode, String)> {
    let chain = state
        .chain_store
        .get(&id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Chain '{}' not found", id)))?;
    Ok(Json(ChainResponse::from(chain)))
}

#[derive(Debug, Deserialize)]
struct CreateChainRequest {
    id: String,
    name: String,
    entries: Vec<ChainEntryRequest>,
    #[serde(default)]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
struct ChainEntryRequest {
    provider_id: String,
    model_id: String,
}

/// POST /api/model-routing/chains - Create a new chain.
async fn create_chain(
    State(state): State<Arc<super::routes::AppState>>,
    Json(req): Json<CreateChainRequest>,
) -> Result<Json<ChainResponse>, (StatusCode, String)> {
    if req.id.is_empty() || req.name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "id and name are required".to_string(),
        ));
    }

    if req.id.starts_with("builtin/") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Chain IDs starting with 'builtin/' are reserved".to_string(),
        ));
    }

    if req.entries.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one entry is required".to_string(),
        ));
    }
    // Reject entries with empty provider_id or model_id
    for e in &req.entries {
        if e.provider_id.trim().is_empty() || e.model_id.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Each entry must have a non-empty provider_id and model_id".to_string(),
            ));
        }
    }

    // Don't allow overwriting existing chains via create
    if state.chain_store.get(&req.id).await.is_some() {
        return Err((
            StatusCode::CONFLICT,
            format!("Chain '{}' already exists, use PUT to update", req.id),
        ));
    }

    let now = chrono::Utc::now();
    let chain = ModelChain {
        id: req.id,
        name: req.name,
        entries: req
            .entries
            .into_iter()
            .map(|e| ChainEntry {
                provider_id: e.provider_id,
                model_id: e.model_id,
            })
            .collect(),
        is_default: req.is_default,
        created_at: now,
        updated_at: now,
    };

    state.chain_store.upsert(chain.clone()).await;
    Ok(Json(ChainResponse::from(chain)))
}

#[derive(Debug, Deserialize)]
struct UpdateChainRequest {
    name: Option<String>,
    entries: Option<Vec<ChainEntryRequest>>,
    is_default: Option<bool>,
}

/// PUT /api/model-routing/chains/:id - Update a chain.
async fn update_chain(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<UpdateChainRequest>,
) -> Result<Json<ChainResponse>, (StatusCode, String)> {
    let mut chain = state
        .chain_store
        .get(&id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Chain '{}' not found", id)))?;

    if let Some(name) = req.name {
        if name.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "name cannot be empty".to_string()));
        }
        chain.name = name;
    }

    if let Some(entries) = req.entries {
        if entries.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "At least one entry is required".to_string(),
            ));
        }
        // Reject entries with empty provider_id or model_id
        for e in &entries {
            if e.provider_id.trim().is_empty() || e.model_id.trim().is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Each entry must have a non-empty provider_id and model_id".to_string(),
                ));
            }
        }
        chain.entries = entries
            .into_iter()
            .map(|e| ChainEntry {
                provider_id: e.provider_id,
                model_id: e.model_id,
            })
            .collect();
    }

    if let Some(is_default) = req.is_default {
        chain.is_default = is_default;
    }

    state.chain_store.upsert(chain.clone()).await;
    Ok(Json(ChainResponse::from(chain)))
}

/// DELETE /api/model-routing/chains/:id - Delete a chain.
async fn delete_chain(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if id.starts_with("builtin/") {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Cannot delete builtin chain '{}'", id),
        ));
    }

    match state.chain_store.delete(&id).await {
        Ok(true) => Ok(Json(serde_json::json!({ "deleted": true }))),
        Ok(false) => Err((StatusCode::NOT_FOUND, format!("Chain '{}' not found", id))),
        Err(msg) => Err((StatusCode::CONFLICT, msg.to_string())),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain Resolution
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ResolvedEntryResponse {
    provider_id: String,
    model_id: String,
    account_id: String,
    has_api_key: bool,
    has_base_url: bool,
}

/// GET /api/model-routing/chains/:id/resolve - Resolve a chain for debugging.
///
/// Returns the expanded, health-filtered list of entries ready for routing.
async fn resolve_chain(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<ResolvedEntryResponse>>, (StatusCode, String)> {
    // Read standard provider accounts from OpenCode config so chain resolution
    // can include them alongside custom providers from AIProviderStore.
    let standard_accounts = super::ai_providers::read_standard_accounts(&state.config.working_dir);

    let resolved = state
        .chain_store
        .resolve_chain(
            &id,
            &state.ai_providers,
            &standard_accounts,
            &state.health_tracker,
        )
        .await;

    if resolved.is_empty() {
        // Check if chain even exists
        if state.chain_store.get(&id).await.is_none() {
            return Err((StatusCode::NOT_FOUND, format!("Chain '{}' not found", id)));
        }
    }

    Ok(Json(
        resolved
            .into_iter()
            .map(|e| ResolvedEntryResponse {
                provider_id: e.provider_id,
                model_id: e.model_id,
                account_id: e.account_id.to_string(),
                has_api_key: e.api_key.is_some(),
                has_base_url: e.base_url.is_some(),
            })
            .collect(),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Health Tracking
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/model-routing/health - List health for all tracked accounts.
async fn list_health(
    State(state): State<Arc<super::routes::AppState>>,
) -> Json<Vec<crate::provider_health::AccountHealthSnapshot>> {
    Json(state.health_tracker.get_all_health().await)
}

/// GET /api/model-routing/health/:account_id - Get health for a specific account.
async fn get_account_health(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(account_id): AxumPath<String>,
) -> Result<Json<crate::provider_health::AccountHealthSnapshot>, (StatusCode, String)> {
    let uuid = uuid::Uuid::parse_str(&account_id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid UUID".to_string()))?;
    Ok(Json(state.health_tracker.get_health(uuid).await))
}

/// POST /api/model-routing/health/:account_id/clear - Clear cooldown for an account.
async fn clear_cooldown(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(account_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let uuid = uuid::Uuid::parse_str(&account_id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid UUID".to_string()))?;
    state.health_tracker.clear_cooldown(uuid).await;
    Ok(Json(serde_json::json!({ "cleared": true })))
}

// ─────────────────────────────────────────────────────────────────────────────
// Observability
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/model-routing/events - List recent fallback events.
///
/// Returns the most recent fallback events (up to the full ring buffer).
async fn list_fallback_events(
    State(state): State<Arc<super::routes::AppState>>,
) -> Json<Vec<crate::provider_health::FallbackEvent>> {
    Json(state.health_tracker.get_recent_events(200).await)
}

// ─────────────────────────────────────────────────────────────────────────────
// RTK Stats
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct RtkStatsResponse {
    commands_processed: u64,
    original_chars: u64,
    compressed_chars: u64,
    chars_saved: u64,
    savings_percent: f64,
}

/// GET /api/model-routing/rtk-stats - Get RTK token savings stats.
///
/// Returns statistics about CLI output compression via RTK.
async fn get_rtk_stats() -> Json<RtkStatsResponse> {
    let (commands, original, compressed): (u64, u64, u64) = rtk_stats();
    let chars_saved = original.saturating_sub(compressed);
    let savings_percent = if original > 0 {
        (chars_saved as f64 / original as f64) * 100.0
    } else {
        0.0
    };

    Json(RtkStatsResponse {
        commands_processed: commands,
        original_chars: original,
        compressed_chars: compressed,
        chars_saved,
        savings_percent,
    })
}
