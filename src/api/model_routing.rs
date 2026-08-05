//! Model routing API endpoints.
//!
//! Provides endpoints for managing model chains and viewing provider health:
//! - List/create/update/delete model chains
//! - View provider health status and cooldowns
//! - Resolve a chain into ordered entries (for debugging)
//! - Clear cooldowns

use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::provider_health::{ChainEntry, ModelChain};

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
        .route("/chains/:id/test", post(test_chain))
        // Health tracking
        .route("/health", get(list_health))
        .route("/health/:account_id", get(get_account_health))
        .route("/health/:account_id/clear", post(clear_cooldown))
        // Observability
        .route("/events", get(list_fallback_events))
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
    strip_thinking: bool,
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
            strip_thinking: chain.strip_thinking,
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
    #[serde(default)]
    strip_thinking: bool,
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
        strip_thinking: req.strip_thinking,
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
    strip_thinking: Option<bool>,
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

    if let Some(strip_thinking) = req.strip_thinking {
        chain.strip_thinking = strip_thinking;
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
    has_credentials: bool,
    auth_kind: &'static str,
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
                auth_kind: if e.api_key.is_some() {
                    "api_key"
                } else if e.has_oauth {
                    "oauth"
                } else {
                    "none"
                },
                provider_id: e.provider_id,
                model_id: e.model_id,
                account_id: e.account_id.to_string(),
                has_credentials: e.api_key.is_some() || e.has_oauth,
                has_base_url: e.base_url.is_some(),
            })
            .collect(),
    ))
}

/// POST /api/model-routing/chains/:id/test - Send a 1-token test request
/// through a chain.
///
/// Runs behind the dashboard JWT (unlike `/v1/chat/completions`, which
/// requires a proxy bearer), so the debug panel can exercise a chain without
/// holding a proxy key.
#[derive(Debug, Deserialize, Default)]
pub struct TestChainQuery {
    /// Probe every entry instead of stopping at the first that answers.
    #[serde(default)]
    pub all: bool,
}

/// One entry's liveness, from a real 1-token request to that exact upstream.
#[derive(Debug, Serialize)]
struct EntryProbe {
    provider_id: String,
    model_id: String,
    ok: bool,
    status: u16,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Probe one `provider/model` directly, bypassing chain fallback.
///
/// Addressing the entry by its qualified id is what makes this a per-entry
/// test: sending the chain id would let the proxy fall through to whichever
/// entry still works, which is exactly the blindness this exists to remove.
async fn probe_entry(
    state: &Arc<super::routes::AppState>,
    provider_id: &str,
    model_id: &str,
) -> EntryProbe {
    let qualified = format!("{provider_id}/{model_id}");
    let body = serde_json::json!({
        "model": qualified,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
    });
    let body_bytes = bytes::Bytes::from(serde_json::to_vec(&body).expect("probe body serializes"));

    let started = std::time::Instant::now();
    let response = super::proxy::chat_completions_inner(
        state.clone(),
        axum::http::HeaderMap::new(),
        body_bytes,
    )
    .await;
    let status = response.status();
    let latency_ms = started.elapsed().as_millis() as u64;

    // Only failures carry their body: a success adds nothing an operator
    // needs, and the point of this endpoint is to make failures legible.
    let error = if status.is_success() {
        None
    } else {
        axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).chars().take(300).collect())
    };

    EntryProbe {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        ok: status.is_success(),
        status: status.as_u16(),
        latency_ms,
        error,
    }
}

async fn test_chain(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<TestChainQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.chain_store.get(&id).await.is_none() {
        return Err((StatusCode::NOT_FOUND, format!("Chain '{}' not found", id)));
    }

    if query.all {
        // A chain "works" as soon as its first entry answers, which says
        // nothing about the ones behind it. Measured on prod 2026-08-05:
        // builtin/smart reported ok=true through MiniMax while its last
        // resort, spark/gemma-4, could not serve at all — no vLLM image was
        // installed on the node, so every model there was inactive. The
        // fleet was one MiniMax hiccup from a full stop and no surface said
        // so. Probing each entry is the only way to see fallback depth.
        let standard_accounts =
            super::ai_providers::read_standard_accounts(&state.config.working_dir);
        let resolved = state
            .chain_store
            .resolve_chain(
                &id,
                &state.ai_providers,
                &standard_accounts,
                &state.health_tracker,
            )
            .await;

        let mut entries = Vec::with_capacity(resolved.len());
        for entry in &resolved {
            entries.push(probe_entry(&state, &entry.provider_id, &entry.model_id).await);
        }
        let live = entries.iter().filter(|e| e.ok).count();
        return Ok(Json(serde_json::json!({
            // `ok` keeps its meaning — the chain can serve a request — so a
            // caller that only checks `ok` is not silently given a stricter
            // answer than it asked for. Depth is reported alongside.
            "ok": live > 0,
            "entries_total": entries.len(),
            "entries_live": live,
            "entries": entries,
        })));
    }

    let body = serde_json::json!({
        "model": id,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
    });
    let body_bytes = bytes::Bytes::from(serde_json::to_vec(&body).expect("test body serializes"));
    let response = super::proxy::chat_completions_inner(
        state.clone(),
        axum::http::HeaderMap::new(),
        body_bytes,
    )
    .await;

    let status = response.status();
    let collected = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read test response: {}", e),
            )
        })?;
    let payload: serde_json::Value = serde_json::from_slice(&collected).unwrap_or_else(
        |_| serde_json::json!({ "raw": String::from_utf8_lossy(&collected).to_string() }),
    );

    Ok(Json(serde_json::json!({
        "ok": status.is_success(),
        "status": status.as_u16(),
        "response": payload,
    })))
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

#[cfg(test)]
mod chain_probe_tests {
    use super::*;

    /// `all` must default to false so the existing single-shot behaviour is
    /// what an unchanged caller keeps getting.
    #[test]
    fn probing_every_entry_is_opt_in() {
        let default: TestChainQuery = serde_json::from_str("{}").expect("empty query");
        assert!(!default.all);

        let explicit: TestChainQuery = serde_json::from_str(r#"{"all":true}"#).expect("query");
        assert!(explicit.all);
    }

    /// A probe is addressed to `provider/model`, never to the chain id.
    /// Sending the chain id would let the proxy fall through to whichever
    /// entry still answers — which is exactly the blindness being removed:
    /// on prod, builtin/smart reported ok=true through MiniMax while its last
    /// resort could not serve at all.
    #[test]
    fn a_probe_targets_one_entry_not_the_chain() {
        let source = include_str!("model_routing.rs");
        let probe = source
            .split("async fn probe_entry")
            .nth(1)
            .expect("probe_entry exists");
        let body = probe.split("async fn test_chain").next().unwrap_or(probe);
        assert!(
            body.contains(r#"format!("{provider_id}/{model_id}")"#),
            "probe_entry must address the qualified entry id"
        );
        assert!(
            !body.contains(r#""model": id"#),
            "probe_entry must not send the chain id, which would fall through"
        );
    }

    #[test]
    fn a_failing_entry_reports_its_status_and_a_bounded_error() {
        let probe = EntryProbe {
            provider_id: "spark".into(),
            model_id: "gemma-4".into(),
            ok: false,
            status: 502,
            latency_ms: 6_000,
            error: Some("Image nvcr.io/nvidia/vllm missing".into()),
        };
        let json = serde_json::to_value(&probe).expect("probe serializes");
        assert_eq!(json["ok"], false);
        assert_eq!(json["status"], 502);
        assert_eq!(json["provider_id"], "spark");
        assert!(json["error"].as_str().unwrap().contains("missing"));
    }

    #[test]
    fn a_healthy_entry_carries_no_error_field() {
        // Success bodies add nothing an operator needs; the endpoint exists to
        // make failures legible, so a healthy entry stays terse.
        let probe = EntryProbe {
            provider_id: "minimax".into(),
            model_id: "MiniMax-M3".into(),
            ok: true,
            status: 200,
            latency_ms: 1_000,
            error: None,
        };
        let json = serde_json::to_value(&probe).expect("probe serializes");
        assert!(json.get("error").is_none(), "healthy probes stay terse");
    }
}
