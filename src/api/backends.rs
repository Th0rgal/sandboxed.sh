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
    /// First line of `<cli> --version` on the host (None = unavailable or
    /// version probe failed). Cached for a few minutes server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
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

/// TTL for the host CLI version cache. Version probes spawn the CLI itself
/// (`--version`), which can take ~1s for node-based harnesses, so results are
/// reused across requests.
const CLI_VERSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

type CliVersionCache =
    tokio::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Option<String>)>>;

fn cli_version_cache() -> &'static CliVersionCache {
    static CACHE: std::sync::OnceLock<CliVersionCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()))
}

/// First line of `<cli> --version` on the host, cached for
/// [`CLI_VERSION_CACHE_TTL`]. Returns None when the CLI is missing, errors,
/// or takes longer than 10s.
async fn probe_host_cli_version(cli: &str) -> Option<String> {
    {
        let cache = cli_version_cache().lock().await;
        if let Some((probed_at, version)) = cache.get(cli) {
            if probed_at.elapsed() < CLI_VERSION_CACHE_TTL {
                return version.clone();
            }
        }
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(cli).arg("--version").output(),
    )
    .await;
    let version = match result {
        Ok(Ok(output)) if output.status.success() => {
            parse_cli_version_output(&String::from_utf8_lossy(&output.stdout))
        }
        _ => None,
    };

    cli_version_cache().lock().await.insert(
        cli.to_string(),
        (std::time::Instant::now(), version.clone()),
    );
    version
}

/// Extract the version from `--version` output: first non-empty line, trimmed.
fn parse_cli_version_output(raw: &str) -> Option<String> {
    raw.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Pick the CLI to version-probe: explicit `cli_path` override first,
/// otherwise the first declared name that is actually available.
fn version_probe_target(settings: &serde_json::Value, declared: &[&'static str]) -> Option<String> {
    if let Some(custom) = settings
        .get("cli_path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(custom.to_string());
    }
    declared
        .iter()
        .find(|name| check_cli_available(name))
        .map(|name| name.to_string())
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
    let cli_version = if cli_available && !cli_names.is_empty() {
        match version_probe_target(&settings, cli_names) {
            Some(cli) => probe_host_cli_version(&cli).await,
            None => None,
        }
    } else {
        None
    };

    Ok(Json(BackendConfig {
        id: backend.id().to_string(),
        name: backend.name().to_string(),
        enabled: config_entry.enabled,
        settings,
        cli_available,
        cli_version,
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
        "chatgpt_ui" => {
            let settings = req.settings.as_object().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid settings payload".to_string(),
                )
            })?;
            let profile = settings
                .get("profile_dir")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let profile_dirs = settings
                .get("profile_dirs")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if profile.is_none() && profile_dirs.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "profile_dir or profile_dirs is required".to_string(),
                ));
            }
            let stored_profile = profile.map(str::to_string);
            let stored_profile_dirs = profile_dirs
                .iter()
                .map(|profile| (*profile).to_string())
                .collect::<Vec<_>>();
            let canonical_working_dir =
                state.config.working_dir.canonicalize().map_err(|error| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to resolve server working directory: {error}"),
                    )
                })?;
            for profile in profile.into_iter().chain(profile_dirs) {
                let profile_path = std::path::Path::new(profile);
                if !profile_path.is_absolute() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "ChatGPT UI profile directories must be absolute paths".to_string(),
                    ));
                }
                if !profile_path.is_dir() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("ChatGPT UI profile directory does not exist: {profile}"),
                    ));
                }
                let canonical_profile = profile_path.canonicalize().map_err(|error| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("failed to resolve ChatGPT UI profile directory: {error}"),
                    )
                })?;
                if canonical_profile.starts_with(&canonical_working_dir) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "ChatGPT UI profile directories must be outside the sandboxed.sh working directory".to_string(),
                    ));
                }
            }
            let driver_path = settings
                .get("driver_path")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let driver_path = driver_path.ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "driver_path is required".to_string(),
                )
            })?;
            if !std::path::Path::new(driver_path).is_absolute() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "driver_path must be absolute when set".to_string(),
                ));
            }
            if !std::path::Path::new(driver_path).is_file() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "driver_path must name an existing file".to_string(),
                ));
            }
            let python_path = settings
                .get("python_path")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("python3");
            let timeout_secs = settings
                .get("timeout_secs")
                .and_then(|value| value.as_u64())
                .unwrap_or(14_400);
            if !(30..=86_400).contains(&timeout_secs) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "timeout_secs must be between 30 and 86400".to_string(),
                ));
            }
            let browser = settings
                .get("browser")
                .and_then(|value| value.as_str())
                .unwrap_or("chromium");
            if !matches!(browser, "chromium" | "firefox" | "webkit") {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "browser must be chromium, firefox, or webkit".to_string(),
                ));
            }
            let model = settings
                .get("model")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if model.is_some_and(|value| value.len() > 200) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "model label must be at most 200 bytes".to_string(),
                ));
            }
            let proxy_server = settings
                .get("proxy_server")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(proxy_server) = proxy_server {
                let parsed = url::Url::parse(proxy_server).map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "proxy_server must be a URL".to_string(),
                    )
                })?;
                if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "proxy_server must use http, https, socks5, or socks5h".to_string(),
                    ));
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "proxy_server must not contain credentials".to_string(),
                    ));
                }
            }
            let headless = settings
                .get("headless")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let display = settings
                .get("display")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if !headless {
                let Some(display) = display else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "display is required when headless is false".to_string(),
                    ));
                };
                let valid = display.strip_prefix(':').is_some_and(|rest| {
                    let mut parts = rest.split('.');
                    let number = parts.next().unwrap_or_default();
                    let screen = parts.next();
                    !number.is_empty()
                        && number.chars().all(|character| character.is_ascii_digit())
                        && screen.is_none_or(|value| {
                            !value.is_empty()
                                && value.chars().all(|character| character.is_ascii_digit())
                        })
                        && parts.next().is_none()
                });
                if !valid {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "display must use X11 syntax such as :93".to_string(),
                    ));
                }
            }
            serde_json::json!({
                "profile_dir": stored_profile,
                "profile_dirs": stored_profile_dirs,
                "driver_path": driver_path,
                "python_path": python_path,
                "browser": browser,
                "headless": headless,
                "display": display,
                "timeout_secs": timeout_secs,
                "model": model,
                "proxy_server": proxy_server,
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

/// Active ChatGPT UI profile-slot telemetry.
///
/// Slot names are directory basenames only; full operator paths and profile
/// contents are never exposed here.
#[derive(Debug, Serialize)]
pub struct ChatgptUiProfilePoolResponse {
    pub slots: Vec<crate::api::runners::chatgpt_ui::profile_pool::ProfileSlotStatus>,
    pub backend_circuit: crate::api::runners::chatgpt_ui::profile_pool::BackendCircuitStatus,
}

/// GET /api/backends/chatgpt_ui/profile-pool — per-slot pool state
/// (available / in use / quarantined) for the ChatGPT UI browser pool.
pub async fn chatgpt_ui_profile_pool(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<ChatgptUiProfilePoolResponse>, (StatusCode, String)> {
    let profile_dirs =
        crate::api::runners::chatgpt_ui::configured_profile_dirs(&state.config.working_dir)
            .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(ChatgptUiProfilePoolResponse {
        slots: crate::api::runners::chatgpt_ui::profile_pool::pool_snapshot(&profile_dirs),
        backend_circuit: crate::api::runners::chatgpt_ui::profile_pool::backend_circuit_status(
            &profile_dirs,
        ),
    }))
}

/// Durable ChatGPT UI job-ledger health.
///
/// Summaries expose only job state, counters, and allowlisted error codes.
/// Conversation routes, prompt fingerprints, and message content are never
/// serialized here.
#[derive(Debug, Serialize)]
pub struct ChatgptUiDurabilityResponse {
    pub jobs: Vec<crate::api::runners::chatgpt_ui_jobs::JobStatusSummary>,
}

/// GET /api/backends/chatgpt_ui/durability — persisted long-turn job records
/// used for restart/disconnect reconciliation of ChatGPT UI missions.
pub async fn chatgpt_ui_durability(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<ChatgptUiDurabilityResponse>, (StatusCode, String)> {
    let working_dir = state.config.working_dir.clone();
    let jobs = tokio::task::spawn_blocking(move || {
        // Reconcile on read so the dashboard reflects stale-job expiry even
        // when no new turn has run since a restart.
        let _ = crate::api::runners::chatgpt_ui_jobs::reconcile_jobs(&working_dir);
        crate::api::runners::chatgpt_ui_jobs::jobs_snapshot(&working_dir)
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(ChatgptUiDurabilityResponse { jobs }))
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

#[cfg(test)]
mod cli_version_tests {
    use super::*;

    #[test]
    fn parse_cli_version_output_takes_first_nonempty_line() {
        assert_eq!(
            parse_cli_version_output("2.1.139 (Claude Code)\nextra\n"),
            Some("2.1.139 (Claude Code)".to_string())
        );
        assert_eq!(
            parse_cli_version_output("\n  codex-cli 0.48.0  \n"),
            Some("codex-cli 0.48.0".to_string())
        );
        assert_eq!(parse_cli_version_output("\n \n"), None);
    }

    #[test]
    fn version_probe_target_prefers_cli_path_override() {
        let settings = serde_json::json!({"cli_path": "/opt/custom/claude"});
        assert_eq!(
            version_probe_target(&settings, &["claude"]),
            Some("/opt/custom/claude".to_string())
        );
        // Blank override falls through to declared names (availability-gated,
        // so a nonexistent declared CLI yields None).
        let settings = serde_json::json!({"cli_path": "  "});
        assert_eq!(
            version_probe_target(&settings, &["definitely-not-a-real-cli-xyz"]),
            None
        );
    }
}
