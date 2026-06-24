//! Backend management API endpoints.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
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

/// Query parameters for listing backend agents.
#[derive(Debug, Deserialize)]
pub struct ListBackendAgentsQuery {
    /// Library config profile to resolve native agents from (OpenCode only).
    pub profile: Option<String>,
}

/// List agents for a specific backend
pub async fn list_backend_agents(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(query): Query<ListBackendAgentsQuery>,
) -> Result<Json<Vec<AgentResponse>>, (StatusCode, String)> {
    if id == "opencode" {
        let payload =
            super::opencode::fetch_opencode_agents_for_profile(&state, query.profile.as_deref())
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to list agents: {}", e),
                    )
                })?;
        let agents = payload
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                serde_json::Value::String(name) => Some(AgentResponse {
                    id: name.clone(),
                    name,
                }),
                serde_json::Value::Object(obj) => {
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| obj.get("id").and_then(|v| v.as_str()))?;
                    Some(AgentResponse {
                        id: name.to_string(),
                        name: name.to_string(),
                    })
                }
                _ => None,
            })
            .collect();
        return Ok(Json(agents));
    }

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

/// Probe a backend's declared CLI names — true if any are on PATH.
///
/// Honours an explicit `cli_path` override in `settings`, otherwise tries each
/// name from `declared` (typically `Backend::cli_names()`) in order.
fn probe_backend_cli(settings: &serde_json::Value, declared: &[&'static str]) -> bool {
    if let Some(custom) = settings
        .get("cli_path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return check_cli_available(custom);
    }
    declared.iter().any(|name| check_cli_available(name))
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

    let auth_ctx = crate::backend::AuthContext {
        working_dir: &state.config.working_dir,
        settings: &settings,
        secrets: state.secrets.as_deref(),
    };
    let auth_configured = backend.check_auth_configured(&auth_ctx).await;

    // Per-backend settings shaping: surface "api_key_configured" for the
    // backends whose frontend cards still read it.
    if id == "claudecode" {
        let mut obj = settings.as_object().cloned().unwrap_or_default();
        obj.insert(
            "api_key_configured".to_string(),
            serde_json::Value::Bool(auth_configured.unwrap_or(false)),
        );
        settings = serde_json::Value::Object(obj);
    }

    let cli_names = backend.cli_names();
    let cli_available = if cli_names.is_empty() {
        true
    } else {
        probe_backend_cli(&settings, cli_names)
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

// ---- FLEET-003: normalized backend quota ----

/// A vendor-neutral quota snapshot for one provider account serving a backend.
///
/// Providers expose rate-limit state through different header families
/// (Anthropic input/output token windows, OpenAI request/token windows, …).
/// This struct presents a single normalized shape so callers don't branch on
/// the vendor. `raw` carries the untouched [`RateLimitSnapshot`] for anyone
/// who needs the provider-specific detail.
#[derive(Debug, Clone, Serialize)]
pub struct BackendQuota {
    pub backend_id: String,
    pub provider_id: String,
    /// Account-scoped health id the snapshot came from.
    pub account_id: uuid::Uuid,
    /// Amount consumed in the reported window (`limit - remaining`), if both known.
    pub used: Option<u64>,
    /// Amount left in the reported window.
    pub remaining: Option<u64>,
    /// Window maximum.
    pub limit: Option<u64>,
    /// When the reported window resets.
    pub reset_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Which window the normalized numbers describe
    /// (`input_tokens` | `tokens` | `requests`).
    pub window_kind: String,
    /// Untouched provider snapshot for vendor-specific detail.
    pub raw: serde_json::Value,
}

/// The normalized window extracted from a provider snapshot.
struct NormalizedQuota {
    used: Option<u64>,
    remaining: Option<u64>,
    limit: Option<u64>,
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
    window_kind: &'static str,
}

type QuotaNormalizer = fn(&crate::provider_health::RateLimitSnapshot) -> NormalizedQuota;

fn build_window(
    remaining: Option<u64>,
    limit: Option<u64>,
    reset: Option<chrono::DateTime<chrono::Utc>>,
    window_kind: &'static str,
) -> NormalizedQuota {
    let used = match (limit, remaining) {
        (Some(l), Some(r)) => Some(l.saturating_sub(r)),
        _ => None,
    };
    NormalizedQuota {
        used,
        remaining,
        limit,
        reset_at: reset,
        window_kind,
    }
}

/// Anthropic reports per-input/output token windows; prefer the input window
/// and fall back to the combined token window.
fn normalize_anthropic(s: &crate::provider_health::RateLimitSnapshot) -> NormalizedQuota {
    if s.input_tokens_limit.is_some() || s.input_tokens_remaining.is_some() {
        build_window(
            s.input_tokens_remaining,
            s.input_tokens_limit,
            s.tokens_reset,
            "input_tokens",
        )
    } else {
        build_window(s.tokens_remaining, s.tokens_limit, s.tokens_reset, "tokens")
    }
}

/// OpenAI reports request and token windows; prefer the token window.
fn normalize_openai(s: &crate::provider_health::RateLimitSnapshot) -> NormalizedQuota {
    if s.tokens_limit.is_some() || s.tokens_remaining.is_some() {
        build_window(s.tokens_remaining, s.tokens_limit, s.tokens_reset, "tokens")
    } else {
        build_window(
            s.requests_remaining,
            s.requests_limit,
            s.requests_reset,
            "requests",
        )
    }
}

/// Generic fallback: take whichever window the provider populated.
fn normalize_generic(s: &crate::provider_health::RateLimitSnapshot) -> NormalizedQuota {
    if s.tokens_limit.is_some() || s.tokens_remaining.is_some() {
        build_window(s.tokens_remaining, s.tokens_limit, s.tokens_reset, "tokens")
    } else if s.requests_limit.is_some() || s.requests_remaining.is_some() {
        build_window(
            s.requests_remaining,
            s.requests_limit,
            s.requests_reset,
            "requests",
        )
    } else {
        build_window(
            s.input_tokens_remaining,
            s.input_tokens_limit,
            s.tokens_reset,
            "input_tokens",
        )
    }
}

/// Select the per-provider normalizer. A small dispatch table keeps the
/// vendor-specific logic in named functions instead of one sprawling match.
fn normalizer_for(provider_id: &str) -> QuotaNormalizer {
    match provider_id {
        "anthropic" => normalize_anthropic,
        "openai" => normalize_openai,
        _ => normalize_generic,
    }
}

/// GET /api/backends/:id/quota — normalized quota snapshot(s) for the
/// provider account(s) that serve this backend (FLEET-003).
///
/// A backend maps to one or more providers via each provider's
/// `use_for_backends`. For every such provider account that has reported
/// rate-limit headers, we emit one normalized [`BackendQuota`]. Returns an
/// empty list (200) for a known backend with no quota data yet, and 404 for
/// an unknown backend.
pub async fn get_backend_quota(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Vec<BackendQuota>>, (StatusCode, String)> {
    {
        let registry = state.backend_registry.read().await;
        if registry.get(&id).is_none() {
            return Err((StatusCode::NOT_FOUND, format!("Backend {} not found", id)));
        }
    }

    // Provider type ids whose `use_for_backends` includes this backend.
    let provider_ids: std::collections::HashSet<String> = state
        .ai_providers
        .list()
        .await
        .into_iter()
        .filter(|p| p.enabled)
        .filter(|p| {
            p.use_for_backends
                .as_ref()
                .map(|bs| bs.iter().any(|b| b == &id))
                .unwrap_or(false)
        })
        .map(|p| p.provider_type.id().to_string())
        .collect();

    if provider_ids.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let mut quotas = Vec::new();
    for health in state.health_tracker.get_all_health().await {
        let Some(provider_id) = health.provider_id.clone() else {
            continue;
        };
        if !provider_ids.contains(&provider_id) {
            continue;
        }
        let Some(snapshot) = health.rate_limit_snapshot.as_ref() else {
            continue;
        };
        let normalized = normalizer_for(&provider_id)(snapshot);
        quotas.push(BackendQuota {
            backend_id: id.clone(),
            provider_id,
            account_id: health.account_id,
            used: normalized.used,
            remaining: normalized.remaining,
            limit: normalized.limit,
            reset_at: normalized.reset_at,
            window_kind: normalized.window_kind.to_string(),
            raw: serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null),
        });
    }

    Ok(Json(quotas))
}

#[cfg(test)]
mod quota_tests {
    use super::*;
    use crate::provider_health::RateLimitSnapshot;

    /// FLEET-003: Anthropic snapshots normalize off the input-token window and
    /// derive `used` as `limit - remaining`.
    #[test]
    fn test_anthropic_quota_normalization() {
        let snap = RateLimitSnapshot {
            input_tokens_limit: Some(1000),
            input_tokens_remaining: Some(250),
            tokens_reset: chrono::DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .ok()
                .map(|t| t.with_timezone(&chrono::Utc)),
            ..Default::default()
        };
        let n = normalize_anthropic(&snap);
        assert_eq!(n.window_kind, "input_tokens");
        assert_eq!(n.limit, Some(1000));
        assert_eq!(n.remaining, Some(250));
        assert_eq!(n.used, Some(750));
        assert!(n.reset_at.is_some());
    }

    /// FLEET-003: OpenAI snapshots prefer the token window; `used` is absent
    /// when the limit is unknown.
    #[test]
    fn test_openai_quota_normalization() {
        let snap = RateLimitSnapshot {
            tokens_remaining: Some(500),
            requests_remaining: Some(9),
            requests_limit: Some(10),
            ..Default::default()
        };
        let n = normalize_openai(&snap);
        assert_eq!(n.window_kind, "tokens");
        assert_eq!(n.remaining, Some(500));
        assert_eq!(n.limit, None);
        assert_eq!(n.used, None);
    }

    /// FLEET-003: the dispatch table routes by provider id and falls back to
    /// the generic normalizer for unknown providers.
    #[test]
    fn test_normalizer_dispatch() {
        let snap = RateLimitSnapshot {
            requests_limit: Some(100),
            requests_remaining: Some(40),
            ..Default::default()
        };
        let n = normalizer_for("some-unknown-provider")(&snap);
        assert_eq!(n.window_kind, "requests");
        assert_eq!(n.used, Some(60));
    }
}
