//! OpenAI-compatible proxy endpoint.
//!
//! Receives `POST /v1/chat/completions` requests, resolves the model name
//! to a chain of provider+account entries, and forwards the request through
//! the chain until one succeeds. Pre-stream 429/529 errors trigger instant
//! failover to the next entry in the chain.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::Path,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::ai_providers::ProviderType;
use crate::provider_health::CooldownReason;

#[derive(Clone)]
struct GoogleProjectCacheEntry {
    project_id: String,
    cached_at: Instant,
}

static GOOGLE_PROJECT_CACHE: OnceLock<
    tokio::sync::RwLock<HashMap<(uuid::Uuid, String), GoogleProjectCacheEntry>>,
> = OnceLock::new();
const GOOGLE_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";
const GOOGLE_API_CLIENT: &str = "gl-node/22.17.0";
const GOOGLE_CLIENT_METADATA: &str =
    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI";
const GOOGLE_PROJECT_CACHE_TTL: Duration = Duration::from_secs(600);
const DEFAULT_CLI_PROXY_API_BASE_URL: &str = "http://127.0.0.1:8317";

const TEXT_EVENT_STREAM: &str = "text/event-stream";
const NO_CACHE: &str = "no-cache";

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// OpenAI-compatible chat completion request (subset we need for proxying).
///
/// We deserialize only the fields we inspect (model, stream); the full JSON
/// body is forwarded as-is to the upstream provider after swapping `model`.
#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    #[serde(default)]
    stream: Option<bool>,
}

/// Minimal error response matching OpenAI's format.
#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    message: String,
    r#type: String,
    code: Option<String>,
}

fn error_response(status: StatusCode, message: String, code: &str) -> Response {
    let body = ErrorResponse {
        error: ErrorBody {
            message,
            r#type: "error".to_string(),
            code: Some(code.to_string()),
        },
    };
    (status, Json(body)).into_response()
}

#[derive(Serialize)]
struct DeferredAcceptedResponse {
    request_id: uuid::Uuid,
    status: &'static str,
    next_attempt_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct DeferredStatusResponse {
    request_id: uuid::Uuid,
    status: crate::api::deferred_proxy::DeferredRequestStatus,
    attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    next_attempt_at: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_payload: Option<crate::api::deferred_proxy::DeferredResponsePayload>,
}

fn header_truthy(headers: &HeaderMap, key: &str) -> bool {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            normalized == "1" || normalized == "true" || normalized == "yes" || normalized == "on"
        })
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider Base URLs
// ─────────────────────────────────────────────────────────────────────────────

/// Default base URL for OpenAI-compatible providers.
///
/// Returns `None` for providers that don't have an OpenAI-compatible API
/// (e.g., Google Gemini uses a different format).
fn default_base_url(provider_type: ProviderType) -> Option<&'static str> {
    match provider_type {
        ProviderType::OpenAI => Some("https://api.openai.com/v1"),
        ProviderType::Xai => Some("https://api.x.ai/v1"),
        ProviderType::Cerebras => Some("https://api.cerebras.ai/v1"),
        ProviderType::Zai => Some("https://api.z.ai/api/coding/paas/v4"),
        ProviderType::Minimax => Some("https://api.minimax.io/v1"),
        ProviderType::DeepInfra => Some("https://api.deepinfra.com/v1/openai"),
        ProviderType::Groq => Some("https://api.groq.com/openai/v1"),
        ProviderType::OpenRouter => Some("https://openrouter.ai/api/v1"),
        ProviderType::Mistral => Some("https://api.mistral.ai/v1"),
        ProviderType::TogetherAI => Some("https://api.together.xyz/v1"),
        ProviderType::Perplexity => Some("https://api.perplexity.ai"),
        ProviderType::Custom => None, // uses account's base_url
        // Non-OpenAI-compatible providers
        ProviderType::Anthropic => None,
        ProviderType::Google => None,
        ProviderType::AmazonBedrock => None,
        ProviderType::Azure => None,
        ProviderType::Cohere => None,
        ProviderType::GithubCopilot => None,
        ProviderType::Amp => None, // CLI-based, not proxy-compatible
    }
}

/// Get the chat completions URL for a resolved entry.
fn completions_url(provider_type: ProviderType, account_base_url: Option<&str>) -> Option<String> {
    // Account-level override takes precedence
    let base = account_base_url.or_else(|| default_base_url(provider_type))?;
    let base = base.trim_end_matches('/');
    Some(format!("{}/chat/completions", base))
}

fn cli_proxy_chat_completions_url() -> String {
    // Alias precedence lives in `util::CLI_PROXY_BASE_URL_ENV_VARS` so every
    // CLI-proxy code path agrees. `env_var_nonempty` (used by the helper)
    // skips blank values so a templated empty first alias doesn't collapse
    // the URL to just `/v1/chat/completions`.
    let base = crate::util::cli_proxy_base_url_from_env()
        .unwrap_or_else(|| DEFAULT_CLI_PROXY_API_BASE_URL.to_string());
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    }
}

fn build_cli_proxy_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(api_key) = crate::util::cli_proxy_api_key_from_env() {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", api_key)) {
            headers.insert(header::AUTHORIZATION, value);
        }
    }
    headers
}

pub(crate) fn has_routable_proxy_credentials(
    provider_type: ProviderType,
    has_api_key: bool,
    has_oauth: bool,
) -> bool {
    match provider_type {
        ProviderType::Custom => true,
        ProviderType::Anthropic => has_api_key || has_oauth,
        // OpenAI OAuth-only entries are only routable when the local
        // CLI proxy is usable (Codex credential on disk). The
        // CLI-proxy adapter doesn't forward the entry's own OAuth
        // token — it relies on the global Codex credential — so
        // without that we'd select the entry, fall through the
        // non-adapter path, send no Authorization header, and burn
        // through deterministic 401s. Keep it unroutable so chain
        // resolution skips it and picks the next provider instead.
        ProviderType::OpenAI => {
            has_api_key
                || (has_oauth && crate::api::ai_providers::openai_cli_proxy_account_available())
        }
        ProviderType::Google => has_api_key || has_oauth,
        _ => has_api_key,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Routes
// ─────────────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/chat/completions", post(chat_completions))
        .route("/deferred/:id", get(get_deferred_request))
        .route("/deferred/:id", delete(cancel_deferred_request))
        .route("/models", axum::routing::get(list_models))
}

// ─────────────────────────────────────────────────────────────────────────────
// GET /v1/models — list chains as virtual "models"
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelObject>,
}

#[derive(Serialize)]
struct ModelObject {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: &'static str,
}

/// Verify the proxy bearer token from the Authorization header.
///
/// Accepts either the internal `SANDBOXED_PROXY_SECRET` or any user-generated
/// proxy API key from the `ProxyApiKeyStore`.
async fn verify_proxy_auth(
    headers: &HeaderMap,
    state: &super::routes::AppState,
) -> Result<(), Response> {
    let expected = &state.proxy_secret;
    // Reject if the expected secret is empty — this should never happen since
    // the initialization code generates a UUID fallback, but guard anyway.
    if expected.is_empty() {
        return Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Proxy secret is not configured".to_string(),
            "configuration_error",
        ));
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(t) = token else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid or missing proxy authorization".to_string(),
            "authentication_error",
        ));
    };
    // Check the internal secret first (fast path for OpenCode / mission_runner).
    if super::auth::constant_time_eq(t, expected) {
        return Ok(());
    }
    // Check user-generated proxy API keys.
    if state.proxy_api_keys.verify(t).await {
        return Ok(());
    }
    Err(error_response(
        StatusCode::UNAUTHORIZED,
        "Invalid or missing proxy authorization".to_string(),
        "authentication_error",
    ))
}

async fn list_models(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = verify_proxy_auth(&headers, &state).await {
        return resp;
    }
    let chains = state.chain_store.list().await;
    let data = chains
        .into_iter()
        .map(|c| ModelObject {
            id: c.id,
            object: "model",
            created: c.created_at.timestamp(),
            owned_by: "sandboxed",
        })
        .collect();
    Json(ModelsResponse {
        object: "list",
        data,
    })
    .into_response()
}

async fn get_deferred_request(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    if let Err(resp) = verify_proxy_auth(&headers, &state).await {
        return resp;
    }

    let Some(rec) = state.deferred_requests.get(id).await else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("Deferred request '{}' was not found", id),
            "not_found",
        );
    };

    Json(DeferredStatusResponse {
        request_id: rec.id,
        status: rec.status,
        attempt_count: rec.attempt_count,
        last_error: rec.last_error,
        next_attempt_at: rec.next_attempt_at,
        created_at: rec.created_at,
        updated_at: rec.updated_at,
        expires_at: rec.expires_at,
        response_payload: rec.response_payload,
    })
    .into_response()
}

async fn cancel_deferred_request(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    if let Err(resp) = verify_proxy_auth(&headers, &state).await {
        return resp;
    }

    let Some(rec) = state.deferred_requests.cancel(id).await else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("Deferred request '{}' was not found", id),
            "not_found",
        );
    };

    Json(DeferredStatusResponse {
        request_id: rec.id,
        status: rec.status,
        attempt_count: rec.attempt_count,
        last_error: rec.last_error,
        next_attempt_at: rec.next_attempt_at,
        created_at: rec.created_at,
        updated_at: rec.updated_at,
        expires_at: rec.expires_at,
        response_payload: rec.response_payload,
    })
    .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler
// ─────────────────────────────────────────────────────────────────────────────

async fn chat_completions(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response {
    // 0. Verify proxy authorization
    if let Err(resp) = verify_proxy_auth(&headers, &state).await {
        return resp;
    }

    // 1. Parse the request to extract the model name
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {}", e),
                "invalid_request_error",
            );
        }
    };

    let defer_on_rate_limit = header_truthy(
        &headers,
        crate::api::deferred_proxy::DEFER_ON_RATE_LIMIT_HEADER,
    );
    let is_stream = req.stream.unwrap_or(false);
    if defer_on_rate_limit && is_stream {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Deferred mode does not support streaming requests".to_string(),
            "invalid_request_error",
        );
    }
    let requested_model = req.model.clone();

    // 2. Check if the model name maps to a chain ID.
    //    The @ai-sdk/openai-compatible adapter strips the provider prefix, so
    //    a model override "builtin/smart" arrives as just "smart".  We try:
    //      1. Exact match (e.g. "builtin/smart")
    //      2. "builtin/{model}" prefix (e.g. "smart" → "builtin/smart")
    //    Unknown models return an error — no silent fallback to the default
    //    chain, so typos and misconfigurations surface immediately.
    let chain_id = if state.chain_store.get(&requested_model).await.is_some() {
        requested_model.clone()
    } else {
        let prefixed = format!("builtin/{}", requested_model);
        if state.chain_store.get(&prefixed).await.is_some() {
            prefixed
        } else {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!(
                    "Model '{}' is not a known chain. Available chains can be listed at /api/model-routing/chains",
                    requested_model
                ),
                "model_not_found",
            );
        }
    };

    // 3. Resolve chain → expanded entries with health filtering
    let standard_accounts = super::ai_providers::read_standard_accounts(&state.config.working_dir);

    let entries = state
        .chain_store
        .resolve_chain(
            &chain_id,
            &state.ai_providers,
            &standard_accounts,
            &state.health_tracker,
        )
        .await;

    if entries.is_empty() {
        if defer_on_rate_limit {
            return enqueue_deferred_request(&state, &headers, &chain_id, &body).await;
        }
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "All providers in chain '{}' are currently in cooldown or unconfigured",
                chain_id
            ),
            "rate_limit_exceeded",
        );
    }

    // 4. Try each entry in order (waterfall)
    let mut rate_limit_count: u32 = 0;
    let mut client_error_count: u32 = 0;
    let mut server_error_count: u32 = 0;
    let mut pending_fallback_events: Vec<crate::provider_health::FallbackEvent> = Vec::new();

    let chain_length = entries.len() as u32;
    for (entry_idx, entry) in entries.iter().enumerate() {
        let provider_type = match ProviderType::from_id(&entry.provider_id) {
            Some(pt) => pt,
            None => continue,
        };

        // Custom providers may work without an API key (base_url only).
        // Standard providers require credentials (API key or provider OAuth).
        if !has_routable_proxy_credentials(provider_type, entry.api_key.is_some(), entry.has_oauth)
        {
            continue;
        }

        // The synthetic "anthropic-cli-proxy" account is the only Anthropic
        // entry without an api_key — `read_standard_accounts` hoists the
        // access_token into `api_key` for real Anthropic OAuth records so we
        // can forward it as a Bearer credential. Gate the CLI-proxy adapter on
        // that distinction, otherwise direct Anthropic OAuth accounts get sent
        // through the local CLI proxy with no credential and fail.
        let use_anthropic_oauth_cli_proxy_adapter =
            provider_type == ProviderType::Anthropic && entry.has_oauth && entry.api_key.is_none();
        let use_anthropic_adapter =
            provider_type == ProviderType::Anthropic && !use_anthropic_oauth_cli_proxy_adapter;
        // OpenAI OAuth (Codex ChatGPT Plus/Pro tokens) can't authenticate at
        // `api.openai.com/v1/chat/completions` directly — only at the Codex
        // `/v1/responses` endpoint. The local CLI proxy knows how to translate
        // between the two, so when we have OAuth but no `sk-...` key we route
        // through it instead of burning through 401s upstream.
        //
        // The CLI-proxy adapter does NOT forward the selected entry's OAuth
        // token — it relies on the global Codex credential on disk. If no
        // such credential is available, routing here would just produce
        // repeated 401/connection failures and cooldown churn, so also
        // require a usable Codex CLI-proxy account before picking this
        // adapter.
        let use_openai_oauth_cli_proxy_adapter = provider_type == ProviderType::OpenAI
            && entry.has_oauth
            && entry.api_key.is_none()
            && crate::api::ai_providers::openai_cli_proxy_account_available();
        let use_google_oauth_adapter = provider_type == ProviderType::Google && entry.has_oauth;
        let (url, upstream_body, extra_headers) = if use_anthropic_oauth_cli_proxy_adapter {
            let upstream_body = match rewrite_model_for_anthropic_cli_proxy(&body, &entry.model_id)
            {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("Failed to rewrite model in request body: {}", e);
                    server_error_count += 1;
                    continue;
                }
            };
            (
                cli_proxy_chat_completions_url(),
                upstream_body,
                build_cli_proxy_headers(),
            )
        } else if use_openai_oauth_cli_proxy_adapter {
            let upstream_body = match rewrite_model(&body, &entry.model_id) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("Failed to rewrite model in request body: {}", e);
                    server_error_count += 1;
                    continue;
                }
            };
            (
                cli_proxy_chat_completions_url(),
                upstream_body,
                build_cli_proxy_headers(),
            )
        } else if use_anthropic_adapter {
            let credential = match entry.api_key.as_deref() {
                Some(value) if !value.trim().is_empty() => value,
                _ => {
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        "Anthropic routing entry missing credential"
                    );
                    client_error_count += 1;
                    continue;
                }
            };
            let upstream_body =
                match build_anthropic_upstream_request(&body, &entry.model_id, is_stream) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("Failed to build Anthropic upstream request: {}", e);
                        server_error_count += 1;
                        continue;
                    }
                };
            let headers = build_anthropic_proxy_headers(credential, entry.has_oauth);
            (
                "https://api.anthropic.com/v1/messages".to_string(),
                upstream_body,
                headers,
            )
        } else if use_google_oauth_adapter {
            let access_token = match get_google_access_token().await {
                Ok(token) => token,
                Err(e) => {
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        error = %e,
                        "Google OAuth token unavailable for routing"
                    );
                    client_error_count += 1;
                    continue;
                }
            };
            let project_id =
                match get_google_project_id(&state.http_client, entry.account_id, &access_token)
                    .await
                {
                    Ok(project_id) => project_id,
                    Err(e) => {
                        tracing::warn!(
                            provider = %entry.provider_id,
                            account_id = %entry.account_id,
                            error = %e,
                            "Failed to resolve Google Code Assist project for routing"
                        );
                        client_error_count += 1;
                        continue;
                    }
                };
            let (google_url, google_body) =
                match build_google_upstream_request(&body, &entry.model_id, &project_id, is_stream)
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("Failed to build Google upstream request: {}", e);
                        server_error_count += 1;
                        continue;
                    }
                };
            let headers = build_google_proxy_headers(&access_token, is_stream);
            (google_url, google_body, headers)
        } else {
            let Some(url) = completions_url(provider_type, entry.base_url.as_deref()) else {
                tracing::debug!(
                    provider = %entry.provider_id,
                    "Skipping non-OpenAI-compatible provider in chain"
                );
                continue;
            };
            // Build the upstream request body: replace model with the real model ID
            let upstream_body = match rewrite_model(&body, &entry.model_id) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("Failed to rewrite model in request body: {}", e);
                    server_error_count += 1;
                    continue;
                }
            };
            (url, upstream_body, HeaderMap::new())
        };

        // Forward the request.
        //
        // For non-streaming requests, set a 300s timeout.  For streaming
        // requests, don't set a timeout — reqwest applies it to the full
        // response body, which would kill long-running LLM generations.
        let mut upstream_req = state
            .http_client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(upstream_body);
        if !use_google_oauth_adapter
            && !use_anthropic_adapter
            && !use_anthropic_oauth_cli_proxy_adapter
            && !use_openai_oauth_cli_proxy_adapter
        {
            if let Some(api_key) = &entry.api_key {
                upstream_req = upstream_req.header("Authorization", format!("Bearer {}", api_key));
            }
        }
        for (name, value) in &extra_headers {
            upstream_req = upstream_req.header(name, value);
        }
        if !is_stream {
            upstream_req = upstream_req.timeout(std::time::Duration::from_secs(300));
        }

        // Forward select client headers
        if let Some(org) = headers.get("openai-organization") {
            upstream_req = upstream_req.header("OpenAI-Organization", org);
        }

        // Ensure the health tracker knows which provider this account belongs to.
        state
            .health_tracker
            .set_provider_id(entry.account_id, &entry.provider_id)
            .await;

        tracing::debug!(
            provider = %entry.provider_id,
            model = %entry.model_id,
            account_id = %entry.account_id,
            url = %url,
            "Trying upstream provider"
        );

        let request_start = std::time::Instant::now();
        let upstream_resp = match upstream_req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                tracing::warn!(
                    provider = %entry.provider_id,
                    account_id = %entry.account_id,
                    error = %e,
                    latency_ms = elapsed_ms,
                    "Upstream request failed (network error)"
                );
                let reason = if e.is_timeout() {
                    CooldownReason::Timeout
                } else {
                    CooldownReason::ServerError
                };
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, reason, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                server_error_count += 1;
                continue;
            }
        };

        let status = upstream_resp.status();

        if use_anthropic_adapter {
            if is_stream && status.is_success() {
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(TEXT_EVENT_STREAM),
                );
                response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE));

                let stream_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
                let stream_created = chrono::Utc::now().timestamp();
                let model_id = entry.model_id.clone();
                let response_stream = transform_anthropic_sse_to_openai(
                    upstream_resp.bytes_stream(),
                    stream_id,
                    stream_created,
                    model_id,
                );

                let ttft_ms = request_start.elapsed().as_millis() as u64;
                state
                    .health_tracker
                    .record_latency(entry.account_id, ttft_ms)
                    .await;
                let account_id = entry.account_id;
                let health_tracker = state.health_tracker.clone();
                let tracked_stream = track_stream_health(
                    response_stream,
                    health_tracker,
                    account_id,
                    None,
                    entry.subscription_key.clone(),
                );

                let success_provider = entry.provider_id.clone();
                for evt in &mut pending_fallback_events {
                    if evt.to_provider.is_none() {
                        evt.to_provider = Some(success_provider.clone());
                    }
                }
                for evt in pending_fallback_events {
                    state.health_tracker.record_fallback_event(evt).await;
                }

                return (status, response_headers, Body::from_stream(tracked_stream))
                    .into_response();
            }

            let response_headers = upstream_resp.headers().clone();
            let resp_body = match upstream_resp.bytes().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    let elapsed_ms = request_start.elapsed().as_millis() as u64;
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        error = %e,
                        "Failed to read Anthropic upstream response body"
                    );
                    let cooldown = state
                        .health_tracker
                        .record_entry_failure(entry, CooldownReason::ServerError, None)
                        .await;
                    pending_fallback_events.push(crate::provider_health::FallbackEvent {
                        timestamp: chrono::Utc::now(),
                        chain_id: chain_id.clone(),
                        from_provider: entry.provider_id.clone(),
                        from_model: entry.model_id.clone(),
                        from_account_id: entry.account_id,
                        reason: CooldownReason::ServerError,
                        cooldown_secs: Some(cooldown.as_secs_f64()),
                        to_provider: None,
                        latency_ms: Some(elapsed_ms),
                        attempt_number: (entry_idx + 1) as u32,
                        chain_length,
                    });
                    server_error_count += 1;
                    continue;
                }
            };

            if status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529 {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let retry_after = parse_rate_limit_headers(&response_headers, provider_type);
                let reason = if status.as_u16() == 529 {
                    CooldownReason::Overloaded
                } else {
                    CooldownReason::RateLimit
                };
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, reason, retry_after)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                rate_limit_count += 1;
                continue;
            }

            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::AuthError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::AuthError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                client_error_count += 1;
                continue;
            }

            if status.is_server_error() {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::ServerError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::ServerError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                server_error_count += 1;
                continue;
            }

            if status.is_client_error() {
                // 4xx outside 429/529/401/403 (e.g., 400 malformed request).
                // Still a provider failure — track it so cooldown/backoff and
                // FallbackEvent reporting kick in instead of silently burning
                // through retries.
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::ClientError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::ClientError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                client_error_count += 1;
                continue;
            }

            let translated = translate_anthropic_json_to_openai(
                &resp_body,
                &entry.model_id,
                chrono::Utc::now().timestamp(),
            );
            let (translated_body, usage) = match translated {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        error = %e,
                        "Failed to translate Anthropic response to OpenAI format"
                    );
                    server_error_count += 1;
                    continue;
                }
            };
            let elapsed_ms = request_start.elapsed().as_millis() as u64;
            state
                .health_tracker
                .record_latency(entry.account_id, elapsed_ms)
                .await;
            state.health_tracker.record_entry_success(entry).await;
            if let Some((input, output)) = usage {
                state
                    .health_tracker
                    .record_token_usage(entry.account_id, input, output)
                    .await;
            }
            let success_provider = entry.provider_id.clone();
            for evt in &mut pending_fallback_events {
                if evt.to_provider.is_none() {
                    evt.to_provider = Some(success_provider.clone());
                }
            }
            for evt in pending_fallback_events {
                state.health_tracker.record_fallback_event(evt).await;
            }

            let mut builder = Response::builder().status(StatusCode::OK);
            if let Some(ct) = response_headers.get(header::CONTENT_TYPE) {
                builder = builder.header(header::CONTENT_TYPE, ct);
            }
            return builder
                .body(Body::from(translated_body))
                .unwrap_or_else(|_| {
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to build response".to_string(),
                        "internal_error",
                    )
                });
        }

        if use_google_oauth_adapter {
            if is_stream && status.is_success() {
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(TEXT_EVENT_STREAM),
                );
                response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE));

                let stream_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
                let stream_created = chrono::Utc::now().timestamp();
                let model_id = entry.model_id.clone();
                let response_stream = transform_google_sse_to_openai(
                    upstream_resp.bytes_stream(),
                    stream_id,
                    stream_created,
                    model_id,
                );

                let ttft_ms = request_start.elapsed().as_millis() as u64;
                state
                    .health_tracker
                    .record_latency(entry.account_id, ttft_ms)
                    .await;
                let account_id = entry.account_id;
                let health_tracker = state.health_tracker.clone();
                let tracked_stream = track_stream_health(
                    response_stream,
                    health_tracker,
                    account_id,
                    None,
                    entry.subscription_key.clone(),
                );

                let success_provider = entry.provider_id.clone();
                for evt in &mut pending_fallback_events {
                    if evt.to_provider.is_none() {
                        evt.to_provider = Some(success_provider.clone());
                    }
                }
                for evt in pending_fallback_events {
                    state.health_tracker.record_fallback_event(evt).await;
                }

                return (status, response_headers, Body::from_stream(tracked_stream))
                    .into_response();
            }

            let response_headers = upstream_resp.headers().clone();
            let resp_body = match upstream_resp.bytes().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    let elapsed_ms = request_start.elapsed().as_millis() as u64;
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        error = %e,
                        "Failed to read Google upstream response body"
                    );
                    let cooldown = state
                        .health_tracker
                        .record_entry_failure(entry, CooldownReason::ServerError, None)
                        .await;
                    pending_fallback_events.push(crate::provider_health::FallbackEvent {
                        timestamp: chrono::Utc::now(),
                        chain_id: chain_id.clone(),
                        from_provider: entry.provider_id.clone(),
                        from_model: entry.model_id.clone(),
                        from_account_id: entry.account_id,
                        reason: CooldownReason::ServerError,
                        cooldown_secs: Some(cooldown.as_secs_f64()),
                        to_provider: None,
                        latency_ms: Some(elapsed_ms),
                        attempt_number: (entry_idx + 1) as u32,
                        chain_length,
                    });
                    server_error_count += 1;
                    continue;
                }
            };

            if status == StatusCode::TOO_MANY_REQUESTS {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let retry_after = parse_google_retry_after(&response_headers, &resp_body)
                    .or_else(|| parse_rate_limit_headers(&response_headers, provider_type));
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::RateLimit, retry_after)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::RateLimit,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                rate_limit_count += 1;
                continue;
            }

            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let maybe_reason = classify_google_error_reason(&resp_body);
                let reason = maybe_reason.unwrap_or(CooldownReason::AuthError);
                let retry_after = if matches!(
                    reason,
                    CooldownReason::RateLimit | CooldownReason::Overloaded
                ) {
                    parse_google_retry_after(&response_headers, &resp_body)
                        .or_else(|| parse_rate_limit_headers(&response_headers, provider_type))
                } else {
                    None
                };
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, reason, retry_after)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                match reason {
                    CooldownReason::RateLimit | CooldownReason::Overloaded => rate_limit_count += 1,
                    CooldownReason::AuthError => client_error_count += 1,
                    _ => server_error_count += 1,
                }
                continue;
            }

            if status.is_server_error() {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::ServerError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::ServerError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                server_error_count += 1;
                continue;
            }

            if status.is_client_error() {
                client_error_count += 1;
                continue;
            }

            let translated = translate_google_json_to_openai(
                &resp_body,
                &entry.model_id,
                chrono::Utc::now().timestamp(),
            );
            let (translated_body, usage) = match translated {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        provider = %entry.provider_id,
                        account_id = %entry.account_id,
                        error = %e,
                        "Failed to translate Google response to OpenAI format"
                    );
                    server_error_count += 1;
                    continue;
                }
            };
            let elapsed_ms = request_start.elapsed().as_millis() as u64;
            state
                .health_tracker
                .record_latency(entry.account_id, elapsed_ms)
                .await;
            state.health_tracker.record_entry_success(entry).await;
            if let Some((input, output)) = usage {
                state
                    .health_tracker
                    .record_token_usage(entry.account_id, input, output)
                    .await;
            }
            let success_provider = entry.provider_id.clone();
            for evt in &mut pending_fallback_events {
                if evt.to_provider.is_none() {
                    evt.to_provider = Some(success_provider.clone());
                }
            }
            for evt in pending_fallback_events {
                state.health_tracker.record_fallback_event(evt).await;
            }

            let mut builder = Response::builder().status(StatusCode::OK);
            if let Some(ct) = response_headers.get(header::CONTENT_TYPE) {
                builder = builder.header(header::CONTENT_TYPE, ct);
            }
            return builder
                .body(Body::from(translated_body))
                .unwrap_or_else(|_| {
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to build response".to_string(),
                        "internal_error",
                    )
                });
        }

        // Pre-stream error handling: 429, 529, 5xx → cooldown + try next
        if status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529 {
            let elapsed_ms = request_start.elapsed().as_millis() as u64;
            let retry_after = parse_rate_limit_headers(upstream_resp.headers(), provider_type);
            let reason = if status.as_u16() == 529 {
                CooldownReason::Overloaded
            } else {
                CooldownReason::RateLimit
            };
            tracing::info!(
                provider = %entry.provider_id,
                account_id = %entry.account_id,
                status = %status,
                retry_after_secs = ?retry_after.map(|d| d.as_secs_f64()),
                "Upstream rate limited, trying next entry"
            );
            let cooldown = state
                .health_tracker
                .record_entry_failure(entry, reason, retry_after)
                .await;
            pending_fallback_events.push(crate::provider_health::FallbackEvent {
                timestamp: chrono::Utc::now(),
                chain_id: chain_id.clone(),
                from_provider: entry.provider_id.clone(),
                from_model: entry.model_id.clone(),
                from_account_id: entry.account_id,
                reason,
                cooldown_secs: Some(cooldown.as_secs_f64()),
                to_provider: None,
                latency_ms: Some(elapsed_ms),
                attempt_number: (entry_idx + 1) as u32,
                chain_length,
            });
            rate_limit_count += 1;
            continue;
        }

        if status.is_server_error() {
            let elapsed_ms = request_start.elapsed().as_millis() as u64;
            tracing::warn!(
                provider = %entry.provider_id,
                account_id = %entry.account_id,
                status = %status,
                "Upstream server error, trying next entry"
            );
            let cooldown = state
                .health_tracker
                .record_entry_failure(entry, CooldownReason::ServerError, None)
                .await;
            pending_fallback_events.push(crate::provider_health::FallbackEvent {
                timestamp: chrono::Utc::now(),
                chain_id: chain_id.clone(),
                from_provider: entry.provider_id.clone(),
                from_model: entry.model_id.clone(),
                from_account_id: entry.account_id,
                reason: CooldownReason::ServerError,
                cooldown_secs: Some(cooldown.as_secs_f64()),
                to_provider: None,
                latency_ms: Some(elapsed_ms),
                attempt_number: (entry_idx + 1) as u32,
                chain_length,
            });
            server_error_count += 1;
            continue;
        }

        // Auth errors (401/403) — bad credentials, try next account
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            let elapsed_ms = request_start.elapsed().as_millis() as u64;
            tracing::warn!(
                provider = %entry.provider_id,
                account_id = %entry.account_id,
                status = %status,
                "Upstream auth error, trying next entry"
            );
            let cooldown = state
                .health_tracker
                .record_entry_failure(entry, CooldownReason::AuthError, None)
                .await;
            pending_fallback_events.push(crate::provider_health::FallbackEvent {
                timestamp: chrono::Utc::now(),
                chain_id: chain_id.clone(),
                from_provider: entry.provider_id.clone(),
                from_model: entry.model_id.clone(),
                from_account_id: entry.account_id,
                reason: CooldownReason::AuthError,
                cooldown_secs: Some(cooldown.as_secs_f64()),
                to_provider: None,
                latency_ms: Some(elapsed_ms),
                attempt_number: (entry_idx + 1) as u32,
                chain_length,
            });
            client_error_count += 1;
            continue;
        }

        // Other 4xx errors (404 model not found, 422 invalid params, etc.)
        // are provider-specific issues — the next entry may use a different
        // model that works.  Don't set cooldown since this isn't a transient
        // failure, and don't return the upstream error to avoid leaking
        // internal provider details.
        if status.is_client_error() {
            tracing::warn!(
                provider = %entry.provider_id,
                account_id = %entry.account_id,
                model = %entry.model_id,
                status = %status,
                "Upstream client error (possibly wrong model), trying next entry"
            );
            client_error_count += 1;
            continue;
        }

        // Stream the response back to the client.
        if is_stream && status.is_success() {
            // Extract headers before consuming the response with bytes_stream()
            let upstream_headers = upstream_resp.headers().clone();
            // Peek at the first SSE data line to detect in-stream errors.
            // Some providers (e.g. MiniMax) return HTTP 200 but send an error
            // payload as the first SSE event.
            let mut byte_stream = Box::pin(upstream_resp.bytes_stream());
            let mut peek_buf = Vec::new();
            let mut is_stream_error = false;

            // Read enough of the stream to find the first data line
            let mut peek_failed = false;
            'peek: while peek_buf.len() < 4096 {
                match byte_stream.next().await {
                    Some(Ok(chunk)) => {
                        peek_buf.extend_from_slice(&chunk);
                        // Check if we have a complete data line with valid JSON
                        if let Ok(text) = std::str::from_utf8(&peek_buf) {
                            for line in text.lines() {
                                if let Some(json_str) = line.strip_prefix("data: ") {
                                    // Only break when the JSON parses successfully.
                                    // A partial JSON (split across chunks) will fail
                                    // to parse, and we'll keep reading more data.
                                    if let Ok(v) =
                                        serde_json::from_str::<serde_json::Value>(json_str)
                                    {
                                        if v.get("type").and_then(|t| t.as_str()) == Some("error")
                                            || v.get("error").is_some()
                                        {
                                            is_stream_error = true;
                                        }
                                        break 'peek;
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!(
                            provider = %entry.provider_id,
                            account_id = %entry.account_id,
                            error = %e,
                            "Stream peek failed (network error), trying next entry"
                        );
                        peek_failed = true;
                        break;
                    }
                    None => {
                        tracing::warn!(
                            provider = %entry.provider_id,
                            account_id = %entry.account_id,
                            "Stream ended before first data chunk, trying next entry"
                        );
                        peek_failed = true;
                        break;
                    }
                }
            }

            if peek_failed {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::ServerError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::ServerError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                server_error_count += 1;
                continue;
            }

            if is_stream_error {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                // Parse the peeked data to classify the error type.
                let reason = std::str::from_utf8(&peek_buf)
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .find_map(|line| line.strip_prefix("data: "))
                            .and_then(|json_str| {
                                serde_json::from_str::<serde_json::Value>(json_str).ok()
                            })
                    })
                    .map(|v| classify_embedded_error(&v))
                    .unwrap_or(CooldownReason::ServerError);
                tracing::warn!(
                    provider = %entry.provider_id,
                    account_id = %entry.account_id,
                    model = %entry.model_id,
                    reason = %reason,
                    "Upstream returned in-stream error, trying next entry"
                );
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, reason, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                match reason {
                    CooldownReason::RateLimit | CooldownReason::Overloaded => rate_limit_count += 1,
                    CooldownReason::AuthError => client_error_count += 1,
                    _ => server_error_count += 1,
                }
                continue;
            }

            // Record time-to-first-token latency (time until we confirmed a valid stream)
            let ttft_ms = request_start.elapsed().as_millis() as u64;
            state
                .health_tracker
                .record_latency(entry.account_id, ttft_ms)
                .await;

            // Set to_provider on any pending fallback events from this request
            let success_provider = entry.provider_id.clone();
            for evt in &mut pending_fallback_events {
                if evt.to_provider.is_none() {
                    evt.to_provider = Some(success_provider.clone());
                }
            }
            for evt in pending_fallback_events {
                state.health_tracker.record_fallback_event(evt).await;
            }

            // Don't record success yet — defer until the stream finishes
            // so that mid-stream failures don't incorrectly clear cooldown.
            let account_id = entry.account_id;
            let health_tracker = state.health_tracker.clone();

            // Extract rate-limit snapshot to record after stream completes
            let rate_limit_snapshot = extract_rate_limit_snapshot(&upstream_headers, provider_type);

            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
            response_headers.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());

            // Prepend the peeked bytes, then stream the rest
            let peek_stream = futures::stream::once(async {
                Ok::<_, reqwest::Error>(bytes::Bytes::from(peek_buf))
            });
            let combined = peek_stream.chain(byte_stream);
            let byte_stream = normalize_sse_stream(combined);

            // Wrap the stream to record success/failure on completion.
            let tracked_stream = track_stream_health(
                byte_stream,
                health_tracker,
                account_id,
                rate_limit_snapshot,
                entry.subscription_key.clone(),
            );

            return (status, response_headers, Body::from_stream(tracked_stream)).into_response();
        }

        // Non-streaming: read full body before recording success, so a
        // body-read failure doesn't incorrectly clear cooldown state.
        let response_headers = upstream_resp.headers().clone();
        match upstream_resp.bytes().await {
            Ok(resp_body) => {
                // Check for in-body errors (some providers return 200 with
                // an error payload in the JSON body).
                if status.is_success() {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&resp_body) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("error")
                            || v.get("error").is_some()
                        {
                            let elapsed_ms = request_start.elapsed().as_millis() as u64;
                            let reason = classify_embedded_error(&v);
                            tracing::warn!(
                                provider = %entry.provider_id,
                                account_id = %entry.account_id,
                                model = %entry.model_id,
                                reason = %reason,
                                "Upstream returned 200 with error body, trying next entry"
                            );
                            let cooldown = state
                                .health_tracker
                                .record_entry_failure(entry, reason, None)
                                .await;
                            pending_fallback_events.push(crate::provider_health::FallbackEvent {
                                timestamp: chrono::Utc::now(),
                                chain_id: chain_id.clone(),
                                from_provider: entry.provider_id.clone(),
                                from_model: entry.model_id.clone(),
                                from_account_id: entry.account_id,
                                reason,
                                cooldown_secs: Some(cooldown.as_secs_f64()),
                                to_provider: None,
                                latency_ms: Some(elapsed_ms),
                                attempt_number: (entry_idx + 1) as u32,
                                chain_length,
                            });
                            match reason {
                                CooldownReason::RateLimit | CooldownReason::Overloaded => {
                                    rate_limit_count += 1
                                }
                                CooldownReason::AuthError => client_error_count += 1,
                                _ => server_error_count += 1,
                            }
                            continue;
                        }
                    }
                    // Record latency and success
                    let elapsed_ms = request_start.elapsed().as_millis() as u64;
                    state
                        .health_tracker
                        .record_latency(entry.account_id, elapsed_ms)
                        .await;
                    state.health_tracker.record_entry_success(entry).await;

                    // Extract rate-limit quota snapshot from response headers
                    if let Some(snapshot) =
                        extract_rate_limit_snapshot(&response_headers, provider_type)
                    {
                        state
                            .health_tracker
                            .record_rate_limits(entry.account_id, snapshot)
                            .await;
                    }

                    // Extract token usage from the response
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&resp_body) {
                        if let Some(usage) = v.get("usage") {
                            let input = usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let output = usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            if input > 0 || output > 0 {
                                state
                                    .health_tracker
                                    .record_token_usage(entry.account_id, input, output)
                                    .await;
                            }
                        }
                    }

                    // Set to_provider on any pending fallback events
                    let success_provider = entry.provider_id.clone();
                    for evt in &mut pending_fallback_events {
                        if evt.to_provider.is_none() {
                            evt.to_provider = Some(success_provider.clone());
                        }
                    }
                    for evt in pending_fallback_events {
                        state.health_tracker.record_fallback_event(evt).await;
                    }
                }
                let mut builder = Response::builder().status(status);
                if let Some(ct) = response_headers.get(header::CONTENT_TYPE) {
                    builder = builder.header(header::CONTENT_TYPE, ct);
                }
                return builder.body(Body::from(resp_body)).unwrap_or_else(|_| {
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to build response".to_string(),
                        "internal_error",
                    )
                });
            }
            Err(e) => {
                let elapsed_ms = request_start.elapsed().as_millis() as u64;
                tracing::warn!(
                    provider = %entry.provider_id,
                    account_id = %entry.account_id,
                    error = %e,
                    "Failed to read upstream response body"
                );
                let cooldown = state
                    .health_tracker
                    .record_entry_failure(entry, CooldownReason::ServerError, None)
                    .await;
                pending_fallback_events.push(crate::provider_health::FallbackEvent {
                    timestamp: chrono::Utc::now(),
                    chain_id: chain_id.clone(),
                    from_provider: entry.provider_id.clone(),
                    from_model: entry.model_id.clone(),
                    from_account_id: entry.account_id,
                    reason: CooldownReason::ServerError,
                    cooldown_secs: Some(cooldown.as_secs_f64()),
                    to_provider: None,
                    latency_ms: Some(elapsed_ms),
                    attempt_number: (entry_idx + 1) as u32,
                    chain_length,
                });
                server_error_count += 1;
                continue;
            }
        }
    }

    // All entries exhausted — record pending fallback events (to_provider stays None)
    for evt in pending_fallback_events {
        state.health_tracker.record_fallback_event(evt).await;
    }

    // Choose status/message based on failure types
    tracing::warn!(
        chain = %chain_id,
        total_entries = entries.len(),
        rate_limit_count,
        client_error_count,
        server_error_count,
        "All chain entries exhausted"
    );

    let attempted = rate_limit_count + client_error_count + server_error_count;

    if attempted == 0 {
        // No upstream requests were made — every entry was skipped due to
        // missing credentials, unknown provider type, or incompatible API.
        // This is a configuration error, not a rate limit.
        error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "All {} providers in chain '{}' were skipped (missing credentials or incompatible)",
                entries.len(),
                chain_id
            ),
            "provider_configuration_error",
        )
    } else if client_error_count > 0 && rate_limit_count == 0 && server_error_count == 0 {
        // All failures were client errors (4xx / auth) — likely a configuration
        // or credentials issue, not a transient rate limit.
        error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "All {} providers in chain '{}' rejected the request (client/auth errors)",
                entries.len(),
                chain_id
            ),
            "upstream_error",
        )
    } else if server_error_count > 0 && rate_limit_count == 0 {
        // All failures were server/network errors — upstream outage, not throttling.
        error_response(
            StatusCode::BAD_GATEWAY,
            format!(
                "All {} providers in chain '{}' are unavailable (server/network errors)",
                entries.len(),
                chain_id
            ),
            "upstream_unavailable",
        )
    } else {
        if defer_on_rate_limit {
            return enqueue_deferred_request(&state, &headers, &chain_id, &body).await;
        }
        error_response(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "All {} providers in chain '{}' are rate-limited or unavailable",
                entries.len(),
                chain_id
            ),
            "rate_limit_exceeded",
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

async fn enqueue_deferred_request(
    state: &super::routes::AppState,
    headers: &HeaderMap,
    chain_id: &str,
    body: &[u8],
) -> Response {
    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {}", err),
                "invalid_request_error",
            );
        }
    };
    let openai_organization = headers
        .get("openai-organization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let next_attempt_at = crate::api::deferred_proxy::estimate_next_attempt_at(state).await;
    let record = state
        .deferred_requests
        .enqueue(
            chain_id.to_string(),
            payload,
            openai_organization,
            next_attempt_at,
        )
        .await;

    (
        StatusCode::ACCEPTED,
        Json(DeferredAcceptedResponse {
            request_id: record.id,
            status: "queued",
            next_attempt_at: record.next_attempt_at,
        }),
    )
        .into_response()
}

/// Rewrite the `model` field in the JSON request body.
fn rewrite_model(body: &[u8], new_model: &str) -> Result<bytes::Bytes, String> {
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    value["model"] = serde_json::Value::String(new_model.to_string());
    serde_json::to_vec(&value)
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize: {}", e))
}

fn rewrite_model_for_anthropic_cli_proxy(
    body: &[u8],
    new_model: &str,
) -> Result<bytes::Bytes, String> {
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    value["model"] = serde_json::Value::String(new_model.to_string());
    if new_model.contains("claude-opus-4-7") {
        if let Some(obj) = value.as_object_mut() {
            for key in ["temperature", "top_p", "top_k"] {
                obj.remove(key);
            }
        }
    }
    serde_json::to_vec(&value)
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize: {}", e))
}

/// Extract the best cooldown duration from provider-specific rate limit headers.
///
/// Different providers include different headers in their 429 responses:
///
/// - **OpenAI / xAI / Groq**: `x-ratelimit-reset-requests` and
///   `x-ratelimit-reset-tokens` (e.g. "2s", "1m30s", "200ms"), plus
///   standard `retry-after` (seconds).
/// - **Anthropic**: `retry-after` (seconds).
/// - **Minimax / Cerebras / Others**: `retry-after` (seconds).
///
/// We pick the *shortest* of the provider-specific reset durations (since
/// that's when the first limit clears and the request can be retried),
/// falling back to the generic `Retry-After` header.
fn parse_rate_limit_headers(
    headers: &HeaderMap,
    provider_type: ProviderType,
) -> Option<std::time::Duration> {
    match provider_type {
        // Providers that send x-ratelimit-reset-* duration strings
        ProviderType::OpenAI
        | ProviderType::Xai
        | ProviderType::Groq
        | ProviderType::OpenRouter => {
            let mut best: Option<std::time::Duration> = None;
            for key in &[
                "x-ratelimit-reset-requests",
                "x-ratelimit-reset-tokens",
                "x-ratelimit-reset",
            ] {
                if let Some(d) = headers
                    .get(*key)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_duration_string)
                {
                    best = Some(best.map_or(d, |b: std::time::Duration| b.min(d)));
                }
            }
            best.or_else(|| parse_retry_after_secs(headers))
        }
        // Anthropic sends ISO 8601 timestamps in anthropic-ratelimit-*-reset headers
        ProviderType::Anthropic => {
            let mut best: Option<std::time::Duration> = None;
            for key in &[
                "anthropic-ratelimit-requests-reset",
                "anthropic-ratelimit-tokens-reset",
                "anthropic-ratelimit-input-tokens-reset",
                "anthropic-ratelimit-output-tokens-reset",
            ] {
                if let Some(d) = headers
                    .get(*key)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_iso_timestamp_as_duration)
                {
                    best = Some(best.map_or(d, |b: std::time::Duration| b.min(d)));
                }
            }
            best.or_else(|| parse_retry_after_secs(headers))
        }
        // All other providers: use standard Retry-After only
        _ => parse_retry_after_secs(headers),
    }
}

/// Extract full rate-limit quota snapshot from provider response headers.
///
/// Called on every successful response to track remaining quotas.
/// Different providers include different header formats:
///
/// - **OpenAI / xAI / Groq**: `x-ratelimit-limit-requests`, `x-ratelimit-remaining-requests`,
///   `x-ratelimit-limit-tokens`, `x-ratelimit-remaining-tokens`, `x-ratelimit-reset-*`
/// - **Anthropic**: `anthropic-ratelimit-requests-limit`, `anthropic-ratelimit-requests-remaining`,
///   `anthropic-ratelimit-tokens-limit`, `anthropic-ratelimit-tokens-remaining`,
///   `anthropic-ratelimit-input-tokens-*`, `anthropic-ratelimit-output-tokens-*`
fn extract_rate_limit_snapshot(
    headers: &HeaderMap,
    provider_type: ProviderType,
) -> Option<crate::provider_health::RateLimitSnapshot> {
    let now = chrono::Utc::now();

    match provider_type {
        ProviderType::OpenAI
        | ProviderType::Xai
        | ProviderType::Groq
        | ProviderType::OpenRouter => {
            let requests_limit = headers
                .get("x-ratelimit-limit-requests")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let requests_remaining = headers
                .get("x-ratelimit-remaining-requests")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let tokens_limit = headers
                .get("x-ratelimit-limit-tokens")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let tokens_remaining = headers
                .get("x-ratelimit-remaining-tokens")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let requests_reset = headers
                .get("x-ratelimit-reset-requests")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| parse_reset_timestamp(s, &now));
            let tokens_reset = headers
                .get("x-ratelimit-reset-tokens")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| parse_reset_timestamp(s, &now));

            if requests_limit.is_none()
                && requests_remaining.is_none()
                && tokens_limit.is_none()
                && tokens_remaining.is_none()
            {
                return None;
            }

            Some(crate::provider_health::RateLimitSnapshot {
                requests_limit,
                requests_remaining,
                requests_reset,
                tokens_limit,
                tokens_remaining,
                tokens_reset,
                input_tokens_limit: None,
                input_tokens_remaining: None,
                output_tokens_limit: None,
                output_tokens_remaining: None,
                updated_at: now,
            })
        }
        ProviderType::Anthropic => {
            let requests_limit = headers
                .get("anthropic-ratelimit-requests-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let requests_remaining = headers
                .get("anthropic-ratelimit-requests-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let tokens_limit = headers
                .get("anthropic-ratelimit-tokens-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let tokens_remaining = headers
                .get("anthropic-ratelimit-tokens-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let input_tokens_limit = headers
                .get("anthropic-ratelimit-input-tokens-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let input_tokens_remaining = headers
                .get("anthropic-ratelimit-input-tokens-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let output_tokens_limit = headers
                .get("anthropic-ratelimit-output-tokens-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let output_tokens_remaining = headers
                .get("anthropic-ratelimit-output-tokens-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok());
            let requests_reset = headers
                .get("anthropic-ratelimit-requests-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_iso_timestamp);
            let tokens_reset = headers
                .get("anthropic-ratelimit-tokens-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_iso_timestamp);

            if requests_limit.is_none()
                && requests_remaining.is_none()
                && tokens_limit.is_none()
                && tokens_remaining.is_none()
                && input_tokens_limit.is_none()
                && input_tokens_remaining.is_none()
                && output_tokens_limit.is_none()
                && output_tokens_remaining.is_none()
            {
                return None;
            }

            Some(crate::provider_health::RateLimitSnapshot {
                requests_limit,
                requests_remaining,
                requests_reset,
                tokens_limit,
                tokens_remaining,
                tokens_reset,
                input_tokens_limit,
                input_tokens_remaining,
                output_tokens_limit,
                output_tokens_remaining,
                updated_at: now,
            })
        }
        _ => None,
    }
}

/// Parse an ISO 8601 timestamp and return as DateTime.
fn parse_iso_timestamp(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Parse a reset timestamp and convert to DateTime.
/// Handles both ISO 8601 timestamps and duration strings (e.g., "2s", "1m30s").
///
/// Note: Uses uncapped duration parsing since rate-limit reset windows can legitimately
/// span many hours (e.g., OpenAI daily limits reset in ~24h).
fn parse_reset_timestamp(
    s: &str,
    now: &chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }

    if let Some(duration) = parse_rate_limit_duration(s) {
        return Some(*now + chrono::Duration::from_std(duration).ok()?);
    }

    None
}

/// Parse a standard `Retry-After` header as numeric seconds.
fn parse_retry_after_secs(headers: &HeaderMap) -> Option<std::time::Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    let secs: f64 = value.parse().ok()?;
    if secs > 0.0 {
        Some(std::time::Duration::from_secs_f64(
            secs.min(MAX_HEADER_COOLDOWN_SECS),
        ))
    } else {
        None
    }
}

/// Parse an ISO 8601 timestamp and return duration from now.
/// Used for Anthropic's `anthropic-ratelimit-*-reset` headers.
fn parse_iso_timestamp_as_duration(s: &str) -> Option<std::time::Duration> {
    let dt = chrono::DateTime::parse_from_rfc3339(s.trim()).ok()?;
    let now = chrono::Utc::now();
    let diff = dt.signed_duration_since(now);
    if diff.num_seconds() > 0 {
        let secs = (diff.num_seconds() as f64).min(MAX_HEADER_COOLDOWN_SECS);
        Some(std::time::Duration::from_secs_f64(secs))
    } else {
        None // already passed
    }
}

/// Maximum cooldown we'll ever set from a provider header (1 hour).
/// Prevents catastrophic values from buggy headers or misinterpreted timestamps.
const MAX_HEADER_COOLDOWN_SECS: f64 = 3600.0;

/// Parse a human-friendly duration string like "2s", "1m30s", "200ms", "0.5s".
///
/// Supports the formats returned by OpenAI-family rate limit headers:
///   `Xh`, `Xm`, `Xs`, `Xms` and combinations like `1m30s`.
///
/// Also detects Unix epoch timestamps (values > 1e9) and converts them to
/// duration-from-now, to avoid catastrophic multi-year cooldowns.
fn parse_duration_string(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Try plain numeric value first (some providers send "60" instead of "60s")
    if let Ok(secs) = s.parse::<f64>() {
        if secs <= 0.0 {
            return None;
        }
        // Values > 1e9 are almost certainly Unix epoch timestamps, not seconds.
        // Convert to duration-from-now.
        if secs > 1_000_000_000.0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let remaining = (secs - now).clamp(0.0, MAX_HEADER_COOLDOWN_SECS);
            return if remaining > 0.0 {
                Some(std::time::Duration::from_secs_f64(remaining))
            } else {
                None // timestamp is in the past
            };
        }
        let capped = secs.min(MAX_HEADER_COOLDOWN_SECS);
        return Some(std::time::Duration::from_secs_f64(capped));
    }

    let mut total_ms: f64 = 0.0;
    let mut num_buf = String::new();
    let mut chars = s.chars().peekable();

    while chars.peek().is_some() {
        // Collect digits and decimal point
        num_buf.clear();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || c == '.' {
                num_buf.push(c);
                chars.next();
            } else {
                break;
            }
        }

        if num_buf.is_empty() {
            return None; // unexpected non-numeric character
        }

        let num: f64 = num_buf.parse().ok()?;

        // Collect unit suffix
        let mut unit = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphabetic() {
                unit.push(c);
                chars.next();
            } else {
                break;
            }
        }

        total_ms += match unit.as_str() {
            "h" => num * 3_600_000.0,
            "m" => num * 60_000.0,
            "s" => num * 1_000.0,
            "ms" => num,
            "" => num * 1_000.0, // bare number = seconds
            _ => return None,    // unknown unit
        };
    }

    if total_ms > 0.0 {
        let secs = (total_ms / 1000.0).min(MAX_HEADER_COOLDOWN_SECS);
        Some(std::time::Duration::from_secs_f64(secs))
    } else {
        None
    }
}

/// Parse a duration string for rate-limit quota tracking (no 1-hour cap).
///
/// Unlike `parse_duration_string`, this function does NOT cap durations at 1 hour
/// because rate-limit reset windows can legitimately span many hours (e.g., OpenAI
/// daily limits reset in ~24h).
fn parse_rate_limit_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    if let Ok(secs) = s.parse::<f64>() {
        if secs <= 0.0 {
            return None;
        }
        if secs > 1_000_000_000.0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let remaining = secs - now;
            return if remaining > 0.0 {
                Some(std::time::Duration::from_secs_f64(remaining))
            } else {
                None
            };
        }
        return Some(std::time::Duration::from_secs_f64(secs));
    }

    let mut total_ms: f64 = 0.0;
    let mut num_buf = String::new();
    let mut chars = s.chars().peekable();

    while chars.peek().is_some() {
        num_buf.clear();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || c == '.' {
                num_buf.push(c);
                chars.next();
            } else {
                break;
            }
        }

        if num_buf.is_empty() {
            return None;
        }

        let num: f64 = num_buf.parse().ok()?;

        let mut unit = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphabetic() {
                unit.push(c);
                chars.next();
            } else {
                break;
            }
        }

        total_ms += match unit.as_str() {
            "h" => num * 3_600_000.0,
            "m" => num * 60_000.0,
            "s" => num * 1_000.0,
            "ms" => num,
            "" => num * 1_000.0,
            _ => return None,
        };
    }

    if total_ms > 0.0 {
        Some(std::time::Duration::from_secs_f64(total_ms / 1000.0))
    } else {
        None
    }
}

/// Classify an error embedded in a JSON response body.
///
/// Providers sometimes return HTTP 200 with an error payload.  This function
/// inspects the parsed JSON to determine the appropriate cooldown reason
/// instead of blindly treating every such error as a rate limit.
fn classify_embedded_error(v: &serde_json::Value) -> CooldownReason {
    let error_obj = v.get("error");

    // Try string-based classification first:
    //   {"error": {"type": "rate_limit_error"}}          (Anthropic)
    //   {"type": "error", "error": {"type": "..."}}      (Anthropic streaming)
    //   {"error": {"code": "rate_limit_exceeded"}}        (OpenAI-compat)
    //   {"error": {"status": "RESOURCE_EXHAUSTED"}}       (Google)
    let error_type = error_obj
        .and_then(|e| {
            e.get("type")
                .or_else(|| e.get("code"))
                .or_else(|| e.get("status"))
                .and_then(|t| t.as_str())
        })
        .unwrap_or("");

    let error_type_lower = error_type.to_ascii_lowercase();

    if error_type_lower.contains("rate_limit")
        || error_type_lower.contains("rate-limit")
        || error_type_lower.contains("resource_exhausted")
    {
        return CooldownReason::RateLimit;
    } else if error_type_lower.contains("overload") {
        return CooldownReason::Overloaded;
    } else if error_type_lower.contains("auth") || error_type_lower.contains("permission") {
        return CooldownReason::AuthError;
    }

    // Handle numeric error codes (e.g. Google: {"error": {"code": 429}})
    if let Some(code) = error_obj
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_i64())
    {
        return match code {
            429 => CooldownReason::RateLimit,
            529 => CooldownReason::Overloaded,
            401 | 403 => CooldownReason::AuthError,
            500..=599 => CooldownReason::ServerError,
            _ => CooldownReason::ServerError,
        };
    }

    // Unknown embedded error — treat as a server error so it doesn't
    // inflate rate_limit_count and mislead the exhausted-chain classifier.
    CooldownReason::ServerError
}

/// Normalize an SSE byte stream to fix provider-specific quirks.
///
/// Processes `data:` lines, parses the JSON chunk, and strips fields that
/// break OpenAI-compatible clients (e.g. MiniMax sending `delta.role: ""`).
fn normalize_sse_stream(
    inner: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    futures::stream::unfold(
        (Box::pin(inner), Vec::<u8>::new()),
        |(mut stream, mut buf)| async move {
            loop {
                // Check if we have a complete line in the buffer
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let normalized = normalize_sse_line(&line);
                    return Some((Ok(bytes::Bytes::from(normalized)), (stream, buf)));
                }

                // Need more data
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        buf.extend_from_slice(&chunk);
                    }
                    Some(Err(e)) => {
                        return Some((Err(std::io::Error::other(e.to_string())), (stream, buf)));
                    }
                    None => {
                        // Stream ended — flush remaining buffer
                        if buf.is_empty() {
                            return None;
                        }
                        let remaining = std::mem::take(&mut buf);
                        let normalized = normalize_sse_line(&remaining);
                        return Some((Ok(bytes::Bytes::from(normalized)), (stream, buf)));
                    }
                }
            }
        },
    )
}

/// Normalize a single SSE line.  If it's a `data: {...}` line, parse and
/// fix known provider quirks; otherwise pass through unchanged.
fn normalize_sse_line(line: &[u8]) -> Vec<u8> {
    let trimmed = line
        .strip_suffix(b"\r\n")
        .or_else(|| line.strip_suffix(b"\n"))
        .unwrap_or(line);
    let data_prefix = b"data: ";

    if !trimmed.starts_with(data_prefix) {
        return line.to_vec();
    }

    let json_bytes = &trimmed[data_prefix.len()..];

    // "data: [DONE]" — pass through
    let json_trimmed: &[u8] = {
        let s = std::str::from_utf8(json_bytes).unwrap_or("");
        s.trim().as_bytes()
    };
    if json_trimmed == b"[DONE]" {
        return line.to_vec();
    }

    let mut chunk: serde_json::Value = match serde_json::from_slice(json_bytes) {
        Ok(v) => v,
        Err(_) => return line.to_vec(), // not valid JSON, pass through
    };

    let mut modified = false;

    // Fix MiniMax: strip empty `delta.role` field
    if let Some(choices) = chunk.get_mut("choices").and_then(|v| v.as_array_mut()) {
        for choice in choices {
            if let Some(delta) = choice.get_mut("delta").and_then(|v| v.as_object_mut()) {
                if delta.get("role").and_then(|v| v.as_str()) == Some("") {
                    delta.remove("role");
                    modified = true;
                }
            }
        }
    }

    if !modified {
        return line.to_vec();
    }

    // Re-serialize and preserve the original line ending
    let suffix = if line.ends_with(b"\r\n") {
        &b"\r\n"[..]
    } else if line.ends_with(b"\n") {
        &b"\n"[..]
    } else {
        &b""[..]
    };
    let mut out = Vec::from(&b"data: "[..]);
    let _ = serde_json::to_writer(&mut out, &chunk);
    out.extend_from_slice(suffix);
    out
}

/// Wrap a streaming response to defer health tracking until the stream finishes.
///
/// Records `record_success` when the stream ends cleanly, or `record_failure`
/// if the stream terminates with an I/O error mid-flight.
/// Idle gap allowed between SSE chunks before we treat the upstream as stalled.
/// LLM token streaming should produce a chunk every few seconds at most; if a
/// provider goes silent for longer than this mid-stream, the connection is
/// effectively dead. We close it so the account is marked failed and downstream
/// retry logic can engage, instead of letting the harness's 120s text-idle
/// timeout fire and fail the whole mission turn.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn track_stream_health(
    inner: impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
    health_tracker: crate::provider_health::SharedProviderHealthTracker,
    account_id: uuid::Uuid,
    rate_limit_snapshot: Option<crate::provider_health::RateLimitSnapshot>,
    subscription_key: Option<crate::provider_health::SubscriptionKey>,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = std::pin::pin!(inner);
        let mut errored = false;
        let mut received_any = false;
        let mut idle_timeout = false;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        loop {
            match tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next()).await {
                Err(_) => {
                    tracing::warn!(
                        account_id = %account_id,
                        idle_secs = STREAM_IDLE_TIMEOUT.as_secs(),
                        "Upstream stream stalled mid-stream; closing connection"
                    );
                    errored = true;
                    idle_timeout = true;
                    yield Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "upstream stream idle for >{}s",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        ),
                    ));
                    break;
                }
                Ok(None) => break,
                Ok(Some(item)) => {
                    received_any = true;
                    match &item {
                        Ok(chunk) => {
                            // Scan SSE data lines for usage in the final chunk.
                            // OpenAI-compatible providers include a `usage` object
                            // in the last `data:` event of the stream.
                            if let Ok(text) = std::str::from_utf8(chunk) {
                                for line in text.lines() {
                                    if let Some(json_str) = line.strip_prefix("data: ") {
                                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                                            if let Some(usage) = v.get("usage") {
                                                if let Some(pt) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                                                    input_tokens = pt;
                                                }
                                                if let Some(ct) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                                                    output_tokens = ct;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => errored = true,
                    }
                    yield item;
                }
            }
        }
        if errored || !received_any {
            let reason = if idle_timeout {
                CooldownReason::Timeout
            } else {
                CooldownReason::ServerError
            };
            health_tracker
                .record_failure_with_subscription(
                    account_id,
                    subscription_key.as_ref(),
                    reason,
                    None,
                )
                .await;
        } else {
            health_tracker
                .record_success_with_subscription(account_id, subscription_key.as_ref())
                .await;
            if input_tokens > 0 || output_tokens > 0 {
                health_tracker.record_token_usage(account_id, input_tokens, output_tokens).await;
            }
            if let Some(snapshot) = rate_limit_snapshot {
                health_tracker.record_rate_limits(account_id, snapshot).await;
            }
        }
    }
}

fn get_google_project_cache(
) -> &'static tokio::sync::RwLock<HashMap<(uuid::Uuid, String), GoogleProjectCacheEntry>> {
    GOOGLE_PROJECT_CACHE.get_or_init(|| tokio::sync::RwLock::new(HashMap::new()))
}

fn apply_google_client_headers(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder
        .header(header::USER_AGENT, GOOGLE_USER_AGENT)
        .header("X-Goog-Api-Client", GOOGLE_API_CLIENT)
        .header("Client-Metadata", GOOGLE_CLIENT_METADATA)
}

fn build_anthropic_proxy_headers(credential: &str, has_oauth: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    if has_oauth {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", credential)) {
            headers.insert(header::AUTHORIZATION, v);
        }
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20"),
        );
    } else if let Ok(v) = HeaderValue::from_str(credential) {
        headers.insert("x-api-key", v);
    }
    headers
}

fn build_anthropic_upstream_request(
    body: &[u8],
    model_id: &str,
    is_stream: bool,
) -> Result<bytes::Bytes, String> {
    let req: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let mut out = serde_json::Map::new();
    out.insert("model".to_string(), serde_json::json!(model_id));
    out.insert(
        "max_tokens".to_string(),
        req.get("max_tokens")
            .or_else(|| req.get("max_completion_tokens"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!(4096)),
    );
    if is_stream {
        out.insert("stream".to_string(), serde_json::Value::Bool(true));
    }
    let omit_sampling_params = model_id.contains("claude-opus-4-7");
    for key in ["temperature", "top_p", "top_k"] {
        if omit_sampling_params {
            continue;
        }
        if let Some(value) = req.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    if let Some(stop) = req.get("stop") {
        let stop_sequences = if let Some(s) = stop.as_str() {
            serde_json::json!([s])
        } else {
            stop.clone()
        };
        out.insert("stop_sequences".to_string(), stop_sequences);
    }

    let (system, messages) = anthropic_messages_from_openai(
        req.get("messages")
            .and_then(|v| v.as_array())
            .map(|v| v.as_slice())
            .unwrap_or(&[]),
    );
    if !system.is_empty() {
        out.insert("system".to_string(), serde_json::Value::String(system));
    }
    out.insert("messages".to_string(), serde_json::Value::Array(messages));

    if let Some(tools) = anthropic_tools_from_openai(req.get("tools")) {
        out.insert("tools".to_string(), tools);
    }
    if let Some(tool_choice) = anthropic_tool_choice_from_openai(req.get("tool_choice")) {
        out.insert("tool_choice".to_string(), tool_choice);
    }

    serde_json::to_vec(&serde_json::Value::Object(out))
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize Anthropic request body: {}", e))
}

fn anthropic_messages_from_openai(
    messages: &[serde_json::Value],
) -> (String, Vec<serde_json::Value>) {
    let mut system_parts = Vec::new();
    let mut out: Vec<serde_json::Value> = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        match role {
            "system" => {
                let text = extract_openai_message_text(message.get("content"));
                if !text.trim().is_empty() {
                    system_parts.push(text);
                }
            }
            "assistant" => {
                let mut content = anthropic_content_blocks_from_openai(message.get("content"));
                if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in tool_calls {
                        let id = call
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("toolu_compat");
                        let Some(function) = call.get("function").and_then(|v| v.as_object())
                        else {
                            continue;
                        };
                        let name = function
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool");
                        let input = function
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                            .unwrap_or_else(|| serde_json::json!({}));
                        content.push(serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                }
                push_anthropic_message(&mut out, "assistant", content);
            }
            "tool" => {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("toolu_compat");
                let content_text = extract_openai_message_text(message.get("content"));
                push_anthropic_message(
                    &mut out,
                    "user",
                    vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_text
                    })],
                );
            }
            _ => {
                let content = anthropic_content_blocks_from_openai(message.get("content"));
                push_anthropic_message(&mut out, "user", content);
            }
        }
    }

    if out.is_empty() {
        out.push(serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": ""}]
        }));
    }

    (system_parts.join("\n\n"), out)
}

fn push_anthropic_message(
    out: &mut Vec<serde_json::Value>,
    role: &str,
    mut content: Vec<serde_json::Value>,
) {
    if content.is_empty() {
        content.push(serde_json::json!({ "type": "text", "text": "" }));
    }
    if let Some(last) = out.last_mut() {
        let same_role = last.get("role").and_then(|v| v.as_str()) == Some(role);
        if same_role {
            if let Some(existing) = last.get_mut("content").and_then(|v| v.as_array_mut()) {
                existing.extend(content);
                return;
            }
        }
    }
    out.push(serde_json::json!({ "role": role, "content": content }));
}

fn anthropic_content_blocks_from_openai(
    content: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    let Some(content) = content else {
        return Vec::new();
    };
    if let Some(text) = content.as_str() {
        return vec![serde_json::json!({ "type": "text", "text": text })];
    }
    let Some(parts) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in parts {
        match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    out.push(serde_json::json!({ "type": "text", "text": text }));
                }
            }
            "image_url" => {
                if let Some(url) = part
                    .get("image_url")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                {
                    // data URIs → Anthropic base64 source; regular URLs → url source.
                    if let Some(rest) = url.strip_prefix("data:") {
                        if let Some((meta, data)) = rest.split_once(',') {
                            let media_type = meta
                                .split(';')
                                .next()
                                .filter(|m| !m.is_empty())
                                .unwrap_or("image/jpeg");
                            out.push(serde_json::json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": media_type,
                                    "data": data,
                                }
                            }));
                            continue;
                        }
                    }
                    out.push(serde_json::json!({
                        "type": "image",
                        "source": { "type": "url", "url": url }
                    }));
                }
            }
            _ => {}
        }
    }
    out
}

fn anthropic_tools_from_openai(tools: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let mut out = Vec::new();
    for tool in tools.and_then(|v| v.as_array())? {
        if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
            continue;
        }
        let Some(function) = tool.get("function").and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(name) = function.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut converted = serde_json::Map::new();
        converted.insert("name".to_string(), serde_json::json!(name));
        if let Some(description) = function.get("description").and_then(|v| v.as_str()) {
            converted.insert("description".to_string(), serde_json::json!(description));
        }
        converted.insert(
            "input_schema".to_string(),
            function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
        );
        out.push(serde_json::Value::Object(converted));
    }
    if out.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(out))
    }
}

fn anthropic_tool_choice_from_openai(
    tool_choice: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    let tool_choice = tool_choice?;
    if let Some(choice) = tool_choice.as_str() {
        return match choice {
            "none" => Some(serde_json::json!({ "type": "none" })),
            "required" => Some(serde_json::json!({ "type": "any" })),
            "auto" => Some(serde_json::json!({ "type": "auto" })),
            _ => None,
        };
    }
    tool_choice
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .map(|name| serde_json::json!({ "type": "tool", "name": name }))
}

fn finish_reason_from_anthropic(s: Option<&str>) -> &'static str {
    match s.unwrap_or("end_turn") {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "stop_sequence" | "end_turn" => "stop",
        _ => "stop",
    }
}

fn translate_anthropic_json_to_openai(
    body: &[u8],
    model_id: &str,
    created: i64,
) -> Result<(bytes::Bytes, Option<(u64, u64)>), String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;

    let mut content = String::new();
    let mut tool_calls = Vec::new();
    if let Some(parts) = parsed.get("content").and_then(|v| v.as_array()) {
        for part in parts {
            match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "text" => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        content.push_str(text);
                    }
                }
                "tool_use" => {
                    let id = part
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("toolu_compat");
                    let name = part.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let input = part
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let arguments =
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments }
                    }));
                }
                _ => {}
            }
        }
    }

    let stop_reason = parsed.get("stop_reason").and_then(|v| v.as_str());
    let input_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let openai = serde_json::json!({
        "id": parsed
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4())),
        "object": "chat.completion",
        "created": created,
        "model": model_id,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": if content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content) },
                "tool_calls": if tool_calls.is_empty() { serde_json::Value::Null } else { serde_json::Value::Array(tool_calls) },
            },
            "finish_reason": finish_reason_from_anthropic(stop_reason),
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        }
    });
    let bytes = serde_json::to_vec(&openai)
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize translated response: {}", e))?;
    Ok((bytes, Some((input_tokens, output_tokens))))
}

fn transform_anthropic_sse_to_openai(
    inner: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    stream_id: String,
    created: i64,
    model_id: String,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    // tool_blocks maps an Anthropic content_block index to the OpenAI
    // tool_calls[].index we assigned for it. next_tool_idx is the next free
    // OpenAI tool index. Tool indexing is independent of Anthropic block
    // indexing because Anthropic interleaves text and tool_use blocks.
    futures::stream::unfold(
        (
            Box::pin(inner),
            Vec::<u8>::new(),
            false,
            stream_id,
            model_id,
            created,
            std::collections::HashMap::<u64, u32>::new(),
            0u32,
            false,
            false,
        ),
        |(
            mut stream,
            mut buf,
            mut sent_role,
            stream_id,
            model_id,
            created,
            mut tool_blocks,
            mut next_tool_idx,
            mut stream_ended,
            mut done_emitted,
        )| async move {
            if stream_ended {
                return None;
            }
            loop {
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let trimmed = line
                        .strip_suffix(b"\r\n")
                        .or_else(|| line.strip_suffix(b"\n"))
                        .unwrap_or(&line);
                    if !trimmed.starts_with(b"data: ") {
                        continue;
                    }
                    let payload = &trimmed[6..];
                    let parsed = match serde_json::from_slice::<serde_json::Value>(payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let event_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let mut chunks = Vec::new();
                    if !sent_role {
                        let first = serde_json::json!({
                            "id": stream_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model_id,
                            "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": serde_json::Value::Null }],
                        });
                        chunks.push(format!("data: {}\n\n", first));
                        sent_role = true;
                    }
                    match event_type {
                        "content_block_start" => {
                            let block_index =
                                parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                            let block = parsed.get("content_block");
                            if block.and_then(|b| b.get("type")).and_then(|v| v.as_str())
                                == Some("tool_use")
                            {
                                let tool_idx = next_tool_idx;
                                next_tool_idx += 1;
                                tool_blocks.insert(block_index, tool_idx);
                                let id = block
                                    .and_then(|b| b.get("id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let name = block
                                    .and_then(|b| b.get("name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let chunk = serde_json::json!({
                                    "id": stream_id,
                                    "object": "chat.completion.chunk",
                                    "created": created,
                                    "model": model_id,
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "tool_calls": [{
                                                "index": tool_idx,
                                                "id": id,
                                                "type": "function",
                                                "function": { "name": name, "arguments": "" }
                                            }]
                                        },
                                        "finish_reason": serde_json::Value::Null
                                    }],
                                });
                                chunks.push(format!("data: {}\n\n", chunk));
                            }
                        }
                        "content_block_delta" => {
                            let delta = parsed.get("delta");
                            let delta_type = delta
                                .and_then(|d| d.get("type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            match delta_type {
                                "text_delta" | "" => {
                                    if let Some(text) =
                                        delta.and_then(|d| d.get("text")).and_then(|v| v.as_str())
                                    {
                                        if !text.is_empty() {
                                            let chunk = serde_json::json!({
                                                "id": stream_id,
                                                "object": "chat.completion.chunk",
                                                "created": created,
                                                "model": model_id,
                                                "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": serde_json::Value::Null }],
                                            });
                                            chunks.push(format!("data: {}\n\n", chunk));
                                        }
                                    }
                                }
                                "input_json_delta" => {
                                    let block_index =
                                        parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let tool_idx = match tool_blocks.get(&block_index) {
                                        Some(idx) => *idx,
                                        None => continue,
                                    };
                                    if let Some(partial) = delta
                                        .and_then(|d| d.get("partial_json"))
                                        .and_then(|v| v.as_str())
                                    {
                                        if !partial.is_empty() {
                                            let chunk = serde_json::json!({
                                                "id": stream_id,
                                                "object": "chat.completion.chunk",
                                                "created": created,
                                                "model": model_id,
                                                "choices": [{
                                                    "index": 0,
                                                    "delta": {
                                                        "tool_calls": [{
                                                            "index": tool_idx,
                                                            "function": { "arguments": partial }
                                                        }]
                                                    },
                                                    "finish_reason": serde_json::Value::Null
                                                }],
                                            });
                                            chunks.push(format!("data: {}\n\n", chunk));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        "message_delta" => {
                            let finish_reason = finish_reason_from_anthropic(
                                parsed
                                    .get("delta")
                                    .and_then(|d| d.get("stop_reason"))
                                    .and_then(|v| v.as_str()),
                            );
                            let chunk = serde_json::json!({
                                "id": stream_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_id,
                                "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason }],
                            });
                            chunks.push(format!("data: {}\n\n", chunk));
                        }
                        "message_stop" if !done_emitted => {
                            chunks.push("data: [DONE]\n\n".to_string());
                            done_emitted = true;
                            stream_ended = true;
                        }
                        "error" => {
                            // Anthropic stream-time error. Surface as an OpenAI
                            // error chunk followed by [DONE] so callers see the
                            // failure instead of a silent empty completion.
                            let err_obj = parsed.get("error").cloned().unwrap_or_else(
                                || serde_json::json!({ "message": "upstream stream error" }),
                            );
                            let chunk = serde_json::json!({
                                "id": stream_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_id,
                                "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                                "error": err_obj,
                            });
                            chunks.push(format!("data: {}\n\n", chunk));
                            if !done_emitted {
                                chunks.push("data: [DONE]\n\n".to_string());
                                done_emitted = true;
                            }
                            stream_ended = true;
                        }
                        _ => {}
                    }
                    if chunks.is_empty() {
                        continue;
                    }
                    return Some((
                        Ok(bytes::Bytes::from(chunks.concat())),
                        (
                            stream,
                            buf,
                            sent_role,
                            stream_id,
                            model_id,
                            created,
                            tool_blocks,
                            next_tool_idx,
                            stream_ended,
                            done_emitted,
                        ),
                    ));
                }

                match stream.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    Some(Err(e)) => {
                        return Some((
                            Err(std::io::Error::other(e.to_string())),
                            (
                                stream,
                                buf,
                                sent_role,
                                stream_id,
                                model_id,
                                created,
                                tool_blocks,
                                next_tool_idx,
                                stream_ended,
                                done_emitted,
                            ),
                        ));
                    }
                    None => {
                        // Upstream closed. Promote any buffered bytes into a
                        // final line (they may be a complete `data:` event
                        // that just missed a trailing `\n`) so we don't drop
                        // the last event, then terminate the stream. Only
                        // synthesize `[DONE]` if we haven't already emitted
                        // one via `message_stop`.
                        stream_ended = true;
                        if !buf.is_empty() && !buf.ends_with(b"\n") {
                            buf.push(b'\n');
                            stream_ended = false;
                            continue;
                        }
                        if done_emitted {
                            return None;
                        }
                        done_emitted = true;
                        return Some((
                            Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n")),
                            (
                                stream,
                                buf,
                                sent_role,
                                stream_id,
                                model_id,
                                created,
                                tool_blocks,
                                next_tool_idx,
                                stream_ended,
                                done_emitted,
                            ),
                        ));
                    }
                }
            }
        },
    )
}

fn build_google_proxy_headers(access_token: &str, is_stream: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", access_token)) {
        headers.insert(header::AUTHORIZATION, v);
    }
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_static(GOOGLE_USER_AGENT),
    );
    headers.insert(
        "X-Goog-Api-Client",
        HeaderValue::from_static(GOOGLE_API_CLIENT),
    );
    headers.insert(
        "Client-Metadata",
        HeaderValue::from_static(GOOGLE_CLIENT_METADATA),
    );
    if is_stream {
        headers.insert(header::ACCEPT, HeaderValue::from_static(TEXT_EVENT_STREAM));
    }
    headers
}

async fn get_google_access_token() -> Result<String, String> {
    super::ai_providers::ensure_google_oauth_token_valid().await?;
    super::ai_providers::read_google_oauth_access_token()
        .ok_or_else(|| "Google OAuth access token not found".to_string())
}

async fn get_google_project_id(
    http_client: &reqwest::Client,
    account_id: uuid::Uuid,
    access_token: &str,
) -> Result<String, String> {
    let cache_key = (account_id, access_token.to_string());
    if let Some(cached) = get_google_project_cache()
        .read()
        .await
        .get(&cache_key)
        .cloned()
    {
        if cached.cached_at.elapsed() < GOOGLE_PROJECT_CACHE_TTL {
            return Ok(cached.project_id);
        }
    }

    let load_body = serde_json::json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }
    });
    let resp = apply_google_client_headers(
        http_client
            .post("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", access_token)),
    )
    .json(&load_body)
    .send()
    .await
    .map_err(|e| format!("loadCodeAssist request failed: {}", e))?;

    let status = resp.status();
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("loadCodeAssist body read failed: {}", e))?;
    if !status.is_success() {
        return Err(format!(
            "loadCodeAssist failed ({}): {}",
            status,
            String::from_utf8_lossy(&body)
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("Invalid loadCodeAssist JSON: {}", e))?;
    let project = value
        .get("cloudaicompanionProject")
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .get("cloudaicompanionProject")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "loadCodeAssist did not return a managed project".to_string())?
        .to_string();

    let mut cache = get_google_project_cache().write().await;
    cache.retain(|(cached_account_id, _), _| *cached_account_id != account_id);
    cache.insert(
        cache_key,
        GoogleProjectCacheEntry {
            project_id: project.clone(),
            cached_at: Instant::now(),
        },
    );
    Ok(project)
}

fn build_google_upstream_request(
    openai_body: &[u8],
    model_id: &str,
    project_id: &str,
    is_stream: bool,
) -> Result<(String, bytes::Bytes), String> {
    let mut value: serde_json::Value =
        serde_json::from_slice(openai_body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let req = value
        .as_object_mut()
        .ok_or_else(|| "Request body must be a JSON object".to_string())?;

    let mut contents: Vec<serde_json::Value> = Vec::new();
    let mut system_text_parts: Vec<String> = Vec::new();

    for message in req
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let role = message
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user")
            .to_string();
        if role == "system" {
            let text = extract_openai_message_text(message.get("content"));
            if !text.is_empty() {
                system_text_parts.push(text);
            }
            continue;
        }

        let gemini_role = match role.as_str() {
            "assistant" => "model",
            _ => "user",
        };
        let mut parts: Vec<serde_json::Value> = if role == "tool" {
            Vec::new()
        } else {
            extract_openai_parts(message.get("content"))
        };

        if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let function = tc.get("function").and_then(|f| f.as_object());
                let name = function
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                let args_value = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                parts.push(serde_json::json!({
                    "functionCall": {
                        "name": name,
                        "args": args_value,
                    },
                    "thoughtSignature": "skip_thought_signature_validator"
                }));
            }
        }

        if role == "tool" {
            let name = message
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            let content = extract_openai_message_text(message.get("content"));
            parts.push(serde_json::json!({
                "functionResponse": {
                    "name": name,
                    "response": { "output": content }
                }
            }));
        }

        if parts.is_empty() {
            continue;
        }
        contents.push(serde_json::json!({
            "role": gemini_role,
            "parts": parts,
        }));
    }

    let mut request = serde_json::Map::new();
    request.insert("contents".to_string(), serde_json::Value::Array(contents));

    if !system_text_parts.is_empty() {
        request.insert(
            "systemInstruction".to_string(),
            serde_json::json!({
                "parts": system_text_parts
                    .into_iter()
                    .map(|t| serde_json::json!({ "text": t }))
                    .collect::<Vec<_>>(),
            }),
        );
    }

    let mut generation_config = serde_json::Map::new();
    if let Some(v) = req.get("temperature").and_then(|v| v.as_f64()) {
        generation_config.insert("temperature".to_string(), serde_json::json!(v));
    }
    if let Some(v) = req.get("top_p").and_then(|v| v.as_f64()) {
        generation_config.insert("topP".to_string(), serde_json::json!(v));
    }
    if let Some(v) = req.get("max_tokens").and_then(|v| v.as_u64()) {
        generation_config.insert("maxOutputTokens".to_string(), serde_json::json!(v));
    }
    if let Some(v) = req.get("stop") {
        if let Some(arr) = v.as_array() {
            let stops: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if !stops.is_empty() {
                generation_config.insert("stopSequences".to_string(), serde_json::json!(stops));
            }
        } else if let Some(s) = v.as_str() {
            generation_config.insert("stopSequences".to_string(), serde_json::json!([s]));
        }
    }
    if !generation_config.is_empty() {
        request.insert(
            "generationConfig".to_string(),
            serde_json::Value::Object(generation_config),
        );
    }

    if let Some(tools) = req.get("tools").and_then(|v| v.as_array()) {
        let mut function_decls = Vec::new();
        for tool in tools {
            if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
                continue;
            }
            let Some(func) = tool.get("function").and_then(|v| v.as_object()) else {
                continue;
            };
            let Some(name) = func.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let mut decl = serde_json::Map::new();
            decl.insert("name".to_string(), serde_json::json!(name));
            if let Some(desc) = func.get("description").and_then(|v| v.as_str()) {
                decl.insert("description".to_string(), serde_json::json!(desc));
            }
            if let Some(params) = func.get("parameters") {
                decl.insert("parameters".to_string(), params.clone());
            }
            function_decls.push(serde_json::Value::Object(decl));
        }
        if !function_decls.is_empty() {
            request.insert(
                "tools".to_string(),
                serde_json::json!([{ "functionDeclarations": function_decls }]),
            );
        }
    }

    if let Some(tool_choice) = req.get("tool_choice") {
        let tool_cfg = if let Some(s) = tool_choice.as_str() {
            match s {
                "none" => Some(serde_json::json!({ "functionCallingConfig": { "mode": "NONE" } })),
                "required" => {
                    Some(serde_json::json!({ "functionCallingConfig": { "mode": "ANY" } }))
                }
                _ => None,
            }
        } else {
            tool_choice
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .map(|name| {
                    serde_json::json!({
                        "functionCallingConfig": {
                            "mode": "ANY",
                            "allowedFunctionNames": [name]
                        }
                    })
                })
        };
        if let Some(cfg) = tool_cfg {
            request.insert("toolConfig".to_string(), cfg);
        }
    }

    let payload = serde_json::json!({
        "project": project_id,
        "model": model_id,
        "request": serde_json::Value::Object(request),
    });
    let body = serde_json::to_vec(&payload)
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize Google request body: {}", e))?;
    let action = if is_stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    Ok((
        format!("https://cloudcode-pa.googleapis.com/v1internal:{}", action),
        body,
    ))
}

fn extract_openai_parts(content: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    let Some(content) = content else {
        return Vec::new();
    };
    if let Some(s) = content.as_str() {
        if s.is_empty() {
            return Vec::new();
        }
        return vec![serde_json::json!({ "text": s })];
    }
    let Some(arr) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in arr {
        let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match ptype {
            "text" => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    out.push(serde_json::json!({ "text": text }));
                }
            }
            "image_url" => {
                if let Some(url) = part
                    .get("image_url")
                    .and_then(|v| v.get("url"))
                    .and_then(|v| v.as_str())
                {
                    out.push(serde_json::json!({ "text": format!("[image:{}]", url) }));
                }
            }
            _ => {}
        }
    }
    out
}

fn extract_openai_message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(v) if v.is_string() => v.as_str().unwrap_or_default().to_string(),
        Some(v) if v.is_array() => v
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn finish_reason_from_google(s: Option<&str>) -> &'static str {
    match s.unwrap_or("STOP") {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "BLOCKLIST" => "content_filter",
        _ => "stop",
    }
}

fn translate_google_json_to_openai(
    body: &[u8],
    model_id: &str,
    created: i64,
) -> Result<(bytes::Bytes, Option<(u64, u64)>), String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let response = parsed.get("response").unwrap_or(&parsed);
    let candidate = response
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| "Google response missing candidates".to_string())?;

    let mut content = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    if let Some(parts) = candidate
        .get("content")
        .and_then(|v| v.get("parts"))
        .and_then(|v| v.as_array())
    {
        for (idx, part) in parts.iter().enumerate() {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                content.push_str(text);
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                let args = fc
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(serde_json::json!({
                    "id": format!("call_{}", idx),
                    "type": "function",
                    "function": { "name": name, "arguments": args_str }
                }));
            }
        }
    }
    let finish_reason = finish_reason_from_google(
        candidate
            .get("finishReason")
            .and_then(|v| v.as_str())
            .or(Some("STOP")),
    );
    let has_tool_calls = !tool_calls.is_empty();

    let prompt_tokens = response
        .get("usageMetadata")
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = response
        .get("usageMetadata")
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = response
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt_tokens + completion_tokens);

    let openai = serde_json::json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": created,
        "model": model_id,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": if content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content) },
                "tool_calls": if tool_calls.is_empty() { serde_json::Value::Null } else { serde_json::Value::Array(tool_calls) },
            },
            "finish_reason": if has_tool_calls { "tool_calls" } else { finish_reason },
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
        }
    });
    let bytes = serde_json::to_vec(&openai)
        .map(bytes::Bytes::from)
        .map_err(|e| format!("Failed to serialize translated response: {}", e))?;
    Ok((bytes, Some((prompt_tokens, completion_tokens))))
}

fn transform_google_sse_to_openai(
    inner: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    stream_id: String,
    created: i64,
    model_id: String,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    futures::stream::unfold(
        (
            Box::pin(inner),
            Vec::<u8>::new(),
            false, // sent role chunk
            false, // emitted terminal chunk
            false, // emitted tool call
            stream_id,
            model_id,
            created,
        ),
        |(
            mut stream,
            mut buf,
            mut sent_role,
            mut emitted_done,
            mut emitted_tool_call,
            stream_id,
            model_id,
            created,
        )| async move {
            loop {
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let trimmed = line
                        .strip_suffix(b"\r\n")
                        .or_else(|| line.strip_suffix(b"\n"))
                        .unwrap_or(&line);
                    if !trimmed.starts_with(b"data: ") {
                        continue;
                    }
                    let payload = &trimmed[6..];
                    if payload == b"[DONE]" {
                        if !emitted_done {
                            emitted_done = true;
                            return Some((
                                Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n")),
                                (
                                    stream,
                                    buf,
                                    sent_role,
                                    emitted_done,
                                    emitted_tool_call,
                                    stream_id,
                                    model_id,
                                    created,
                                ),
                            ));
                        }
                        continue;
                    }
                    let parsed = match serde_json::from_slice::<serde_json::Value>(payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let resp = parsed.get("response").unwrap_or(&parsed);
                    let candidate = resp
                        .get("candidates")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.first())
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let mut chunks: Vec<String> = Vec::new();
                    if !sent_role {
                        let first = serde_json::json!({
                            "id": stream_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model_id,
                            "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": serde_json::Value::Null }],
                        });
                        chunks.push(format!("data: {}\n\n", first));
                        sent_role = true;
                    }
                    if let Some(parts) = candidate
                        .get("content")
                        .and_then(|v| v.get("parts"))
                        .and_then(|v| v.as_array())
                    {
                        for (idx, part) in parts.iter().enumerate() {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    let chunk = serde_json::json!({
                                        "id": stream_id,
                                        "object": "chat.completion.chunk",
                                        "created": created,
                                        "model": model_id,
                                        "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": serde_json::Value::Null }],
                                    });
                                    chunks.push(format!("data: {}\n\n", chunk));
                                }
                            }
                            if let Some(fc) = part.get("functionCall") {
                                let name =
                                    fc.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                                let args = fc
                                    .get("args")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}));
                                let args_str = serde_json::to_string(&args)
                                    .unwrap_or_else(|_| "{}".to_string());
                                let chunk = serde_json::json!({
                                    "id": stream_id,
                                    "object": "chat.completion.chunk",
                                    "created": created,
                                    "model": model_id,
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "tool_calls": [{
                                                "index": idx,
                                                "id": format!("call_{}", idx),
                                                "type": "function",
                                                "function": { "name": name, "arguments": args_str }
                                            }]
                                        },
                                        "finish_reason": serde_json::Value::Null
                                    }],
                                });
                                chunks.push(format!("data: {}\n\n", chunk));
                                emitted_tool_call = true;
                            }
                        }
                    }

                    if let Some(fr) = candidate.get("finishReason").and_then(|v| v.as_str()) {
                        let mut finish_reason = finish_reason_from_google(Some(fr)).to_string();
                        if emitted_tool_call && finish_reason == "stop" {
                            finish_reason = "tool_calls".to_string();
                        }
                        let finish_chunk = serde_json::json!({
                            "id": stream_id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model_id,
                            "choices": [{
                                "index": 0,
                                "delta": {},
                                "finish_reason": finish_reason,
                            }],
                        });
                        chunks.push(format!("data: {}\n\n", finish_chunk));
                        if !emitted_done {
                            chunks.push("data: [DONE]\n\n".to_string());
                            emitted_done = true;
                        }
                    }
                    if chunks.is_empty() {
                        continue;
                    }
                    return Some((
                        Ok(bytes::Bytes::from(chunks.concat())),
                        (
                            stream,
                            buf,
                            sent_role,
                            emitted_done,
                            emitted_tool_call,
                            stream_id,
                            model_id,
                            created,
                        ),
                    ));
                }

                match stream.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    Some(Err(e)) => {
                        return Some((
                            Err(std::io::Error::other(e.to_string())),
                            (
                                stream,
                                buf,
                                sent_role,
                                emitted_done,
                                emitted_tool_call,
                                stream_id,
                                model_id,
                                created,
                            ),
                        ));
                    }
                    None => {
                        if emitted_done {
                            return None;
                        }
                        return Some((
                            Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n")),
                            (
                                stream,
                                buf,
                                sent_role,
                                true,
                                emitted_tool_call,
                                stream_id,
                                model_id,
                                created,
                            ),
                        ));
                    }
                }
            }
        },
    )
}

fn classify_google_error_reason(body: &[u8]) -> Option<CooldownReason> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    if let Some(details) = error.get("details").and_then(|v| v.as_array()) {
        for detail in details {
            let r#type = detail.get("@type").and_then(|v| v.as_str()).unwrap_or("");
            if r#type == "type.googleapis.com/google.rpc.ErrorInfo" {
                let reason = detail
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_ascii_uppercase();
                if reason == "RATE_LIMIT_EXCEEDED" {
                    return Some(CooldownReason::RateLimit);
                }
                if reason == "QUOTA_EXHAUSTED" {
                    return Some(CooldownReason::Overloaded);
                }
            }
        }
    }
    let status = error
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    if status == "RESOURCE_EXHAUSTED" {
        return Some(CooldownReason::RateLimit);
    }
    let code = error.get("code").and_then(|v| v.as_u64())?;
    match code {
        429 => Some(CooldownReason::RateLimit),
        529 => Some(CooldownReason::Overloaded),
        401 | 403 => Some(CooldownReason::AuthError),
        _ => None,
    }
}

fn parse_google_retry_after(headers: &HeaderMap, body: &[u8]) -> Option<std::time::Duration> {
    parse_retry_after_secs(headers).or_else(|| {
        let value: serde_json::Value = serde_json::from_slice(body).ok()?;
        let error = value.get("error")?;
        let details = error.get("details").and_then(|v| v.as_array())?;
        for detail in details {
            let r#type = detail.get("@type").and_then(|v| v.as_str()).unwrap_or("");
            if r#type != "type.googleapis.com/google.rpc.RetryInfo" {
                continue;
            }
            let retry_delay = detail.get("retryDelay")?;
            if let Some(s) = retry_delay.as_str() {
                if let Some(d) = parse_duration_string(s) {
                    return Some(d);
                }
            } else if let Some(obj) = retry_delay.as_object() {
                let secs = obj.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let nanos = obj.get("nanos").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let total = secs + (nanos / 1e9);
                if total > 0.0 {
                    return Some(std::time::Duration::from_secs_f64(
                        total.min(MAX_HEADER_COOLDOWN_SECS),
                    ));
                }
            }
        }
        error
            .get("message")
            .and_then(|v| v.as_str())
            .and_then(extract_google_retry_from_message)
    })
}

fn extract_google_retry_from_message(message: &str) -> Option<std::time::Duration> {
    let lower = message.to_ascii_lowercase();
    for marker in ["please retry in ", "after "] {
        if let Some(idx) = lower.find(marker) {
            let rem = &message[idx + marker.len()..];
            let token = rem
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ',' || c == '.');
            if let Some(d) = parse_duration_string(token) {
                return Some(d);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::StreamExt;

    #[test]
    fn parse_duration_simple_seconds() {
        assert_eq!(
            parse_duration_string("2s"),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(
            parse_duration_string("0.5s"),
            Some(std::time::Duration::from_millis(500))
        );
    }

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(
            parse_duration_string("200ms"),
            Some(std::time::Duration::from_millis(200))
        );
    }

    #[test]
    fn parse_duration_minutes_seconds() {
        assert_eq!(
            parse_duration_string("1m30s"),
            Some(std::time::Duration::from_secs(90))
        );
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(
            parse_duration_string("1h"),
            Some(std::time::Duration::from_secs(3600))
        );
    }

    #[test]
    fn parse_duration_plain_numeric() {
        // Plain number treated as seconds (Retry-After format)
        assert_eq!(
            parse_duration_string("60"),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[test]
    fn parse_duration_empty_and_zero() {
        assert_eq!(parse_duration_string(""), None);
        assert_eq!(parse_duration_string("0"), None);
        assert_eq!(parse_duration_string("0s"), None);
    }

    #[test]
    fn parse_duration_whitespace() {
        assert_eq!(
            parse_duration_string("  2s  "),
            Some(std::time::Duration::from_secs(2))
        );
    }

    #[test]
    fn parse_rate_limit_headers_openai() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset-requests", "2s".parse().unwrap());
        headers.insert("x-ratelimit-reset-tokens", "30s".parse().unwrap());
        // Should pick the shortest (2s)
        let d = parse_rate_limit_headers(&headers, ProviderType::OpenAI);
        assert_eq!(d, Some(std::time::Duration::from_secs(2)));
    }

    #[test]
    fn parse_rate_limit_headers_fallback_to_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "10".parse().unwrap());
        // Non-OpenAI provider should use Retry-After
        let d = parse_rate_limit_headers(&headers, ProviderType::Minimax);
        assert_eq!(d, Some(std::time::Duration::from_secs(10)));
    }

    #[test]
    fn parse_rate_limit_headers_openai_falls_back_to_retry_after() {
        let mut headers = HeaderMap::new();
        // No x-ratelimit-reset-* headers, only Retry-After
        headers.insert("retry-after", "5".parse().unwrap());
        let d = parse_rate_limit_headers(&headers, ProviderType::OpenAI);
        assert_eq!(d, Some(std::time::Duration::from_secs(5)));
    }

    #[test]
    fn parse_rate_limit_headers_no_headers() {
        let headers = HeaderMap::new();
        assert_eq!(
            parse_rate_limit_headers(&headers, ProviderType::OpenAI),
            None
        );
        assert_eq!(parse_rate_limit_headers(&headers, ProviderType::Zai), None);
    }

    #[test]
    fn parse_duration_unix_timestamp() {
        // A value > 1e9 should be treated as a Unix epoch timestamp.
        // Use a timestamp 60 seconds in the future.
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let d = parse_duration_string(&future.to_string());
        assert!(d.is_some());
        let secs = d.unwrap().as_secs();
        // Should be roughly 60 seconds, with some tolerance
        assert!((55..=65).contains(&secs), "got {} seconds", secs);
    }

    #[test]
    fn parse_duration_unix_timestamp_in_past() {
        // A past timestamp (year 2001, but > 1e9) should return None
        assert_eq!(parse_duration_string("1000000001"), None);
    }

    #[test]
    fn parse_duration_caps_at_max() {
        // Very large seconds value should be capped at MAX_HEADER_COOLDOWN_SECS
        let d = parse_duration_string("999999").unwrap();
        assert_eq!(
            d,
            std::time::Duration::from_secs(MAX_HEADER_COOLDOWN_SECS as u64)
        );
    }

    #[test]
    fn parse_duration_compound_caps_at_max() {
        // A compound "100h" should be capped
        let d = parse_duration_string("100h").unwrap();
        assert_eq!(
            d,
            std::time::Duration::from_secs(MAX_HEADER_COOLDOWN_SECS as u64)
        );
    }

    #[test]
    fn parse_rate_limit_headers_anthropic() {
        let mut headers = HeaderMap::new();
        // Anthropic sends ISO 8601 timestamps
        let future = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
        headers.insert(
            "anthropic-ratelimit-requests-reset",
            future.parse().unwrap(),
        );
        let d = parse_rate_limit_headers(&headers, ProviderType::Anthropic);
        assert!(d.is_some());
        let secs = d.unwrap().as_secs();
        assert!((25..=35).contains(&secs), "got {} seconds", secs);
    }

    #[test]
    fn parse_google_retry_after_from_retry_info_detail() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "message": "rate limited",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "7s"
                }]
            }
        });
        let d = parse_google_retry_after(&headers, serde_json::to_vec(&body).unwrap().as_slice());
        assert_eq!(d, Some(std::time::Duration::from_secs(7)));
    }

    #[test]
    fn parse_google_retry_after_from_message_hint() {
        let headers = HeaderMap::new();
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "message": "You have exhausted your capacity on this model. Your quota will reset after 28s.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "RATE_LIMIT_EXCEEDED",
                    "domain": "cloudcode-pa.googleapis.com",
                    "metadata": {
                        "model": "gemini-2.5-flash",
                        "uiMessage": "true"
                    }
                }]
            }
        });
        let d = parse_google_retry_after(&headers, serde_json::to_vec(&body).unwrap().as_slice());
        assert_eq!(d, Some(std::time::Duration::from_secs(28)));
    }

    #[test]
    fn classify_google_rate_limit_error_info() {
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "RATE_LIMIT_EXCEEDED",
                    "domain": "cloudcode-pa.googleapis.com"
                }]
            }
        });
        let reason =
            classify_google_error_reason(serde_json::to_vec(&body).unwrap().as_slice()).unwrap();
        assert!(matches!(reason, CooldownReason::RateLimit));
    }

    #[test]
    fn classify_google_quota_exhausted_error_info() {
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "QUOTA_EXHAUSTED",
                    "domain": "cloudcode-pa.googleapis.com"
                }]
            }
        });
        let reason =
            classify_google_error_reason(serde_json::to_vec(&body).unwrap().as_slice()).unwrap();
        assert!(matches!(reason, CooldownReason::Overloaded));
    }

    #[test]
    fn build_google_request_tool_message_uses_only_function_response_part() {
        let body = serde_json::json!({
            "messages": [
                {
                    "role": "tool",
                    "name": "read_file",
                    "content": "file content"
                }
            ]
        });

        let (_, payload_bytes) = build_google_upstream_request(
            serde_json::to_vec(&body).unwrap().as_slice(),
            "gemini-2.5-pro",
            "project-123",
            false,
        )
        .unwrap();

        let payload: serde_json::Value = serde_json::from_slice(payload_bytes.as_ref()).unwrap();
        let parts = payload
            .get("request")
            .and_then(|v| v.get("contents"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("parts"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap();

        assert_eq!(parts.len(), 1);
        assert!(parts[0].get("functionResponse").is_some());
        assert!(parts[0].get("text").is_none());
    }

    #[test]
    fn google_stream_finish_reason_maps_to_tool_calls_when_function_call_seen() {
        let sse_payload = serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "name": "search",
                                "args": { "q": "test" }
                            }
                        }]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let sse_bytes = Bytes::from(format!("data: {}\n\n", sse_payload));
        let input = futures::stream::iter(vec![Ok(sse_bytes)]);

        let out = futures::executor::block_on(async move {
            transform_google_sse_to_openai(
                input,
                "chatcmpl-test".to_string(),
                1,
                "gemini-2.5-pro".to_string(),
            )
            .collect::<Vec<_>>()
            .await
        });

        let text = out
            .into_iter()
            .map(|item| String::from_utf8(item.unwrap().to_vec()).unwrap())
            .collect::<String>();

        assert!(text.contains("\"finish_reason\":\"tool_calls\""));
    }

    #[test]
    fn build_anthropic_request_maps_openai_tools_and_tool_choice() {
        let body = serde_json::json!({
            "model": "opus",
            "messages": [
                { "role": "system", "content": "Be brief." },
                { "role": "user", "content": "Use the echo tool." }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "echo",
                    "description": "Echo text",
                    "parameters": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"]
                    }
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": { "name": "echo" }
            },
            "max_tokens": 32,
            "temperature": 0.0
        });

        let payload_bytes = build_anthropic_upstream_request(
            serde_json::to_vec(&body).unwrap().as_slice(),
            "claude-opus-4-7",
            false,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(payload_bytes.as_ref()).unwrap();

        assert_eq!(payload["model"], "claude-opus-4-7");
        assert!(payload.get("temperature").is_none());
        assert_eq!(payload["system"], "Be brief.");
        assert_eq!(payload["messages"][0]["role"], "user");
        assert_eq!(payload["tools"][0]["name"], "echo");
        assert_eq!(payload["tools"][0]["input_schema"]["required"][0], "text");
        assert_eq!(
            payload["tool_choice"],
            serde_json::json!({
                "type": "tool",
                "name": "echo"
            })
        );
    }

    #[test]
    fn translate_anthropic_response_maps_tool_use_to_openai_tool_calls() {
        let body = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-7",
            "content": [{
                "type": "tool_use",
                "id": "toolu_123",
                "name": "echo",
                "input": { "text": "ok" }
            }],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4
            }
        });

        let (translated, usage) = translate_anthropic_json_to_openai(
            serde_json::to_vec(&body).unwrap().as_slice(),
            "claude-opus-4-7",
            1,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(translated.as_ref()).unwrap();

        assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            payload["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "echo"
        );
        assert_eq!(payload["usage"]["prompt_tokens"], 10);
        assert_eq!(payload["usage"]["completion_tokens"], 4);
        assert_eq!(usage, Some((10, 4)));
    }

    #[test]
    fn proxy_credential_gating_is_provider_aware_for_oauth() {
        // OpenAI OAuth-only routability now also depends on whether a
        // Codex CLI-proxy credential is available on disk — the
        // adapter doesn't forward the entry's own OAuth token, so
        // without a Codex cred the request would 401 anyway. The test
        // environment has no such credential, so OpenAI OAuth-only
        // should be unroutable here.
        assert!(!has_routable_proxy_credentials(
            ProviderType::OpenAI,
            false,
            true
        ));
        // Anthropic and Google OAuth entries keep the old contract —
        // Anthropic CLI-proxy routing has its own fallback path and
        // Google uses a proper refresh flow.
        assert!(has_routable_proxy_credentials(
            ProviderType::Anthropic,
            false,
            true
        ));
        assert!(has_routable_proxy_credentials(
            ProviderType::Google,
            false,
            true
        ));
        // API-key entries are always routable regardless of OAuth state.
        assert!(has_routable_proxy_credentials(
            ProviderType::OpenAI,
            true,
            false
        ));
        assert!(has_routable_proxy_credentials(
            ProviderType::Custom,
            false,
            false
        ));
    }

    #[test]
    fn cli_proxy_chat_completions_url_accepts_proxy_root_or_v1_base() {
        // `CLAUDE_CODE_PROXY_BASE_URL` is the highest-priority alias, so the
        // test must isolate it too — otherwise an ambient value from the
        // test runner shadows everything this test sets and the assertions
        // silently verify the wrong env var.
        let original_claude_code = std::env::var("CLAUDE_CODE_PROXY_BASE_URL").ok();
        let original_cli_proxy = std::env::var("CLI_PROXY_API_BASE_URL").ok();
        let original_clip = std::env::var("CLIPROXY_API_BASE_URL").ok();
        let original_legacy = std::env::var("CLIPROXY_BASE_URL").ok();

        std::env::remove_var("CLAUDE_CODE_PROXY_BASE_URL");
        std::env::remove_var("CLIPROXY_API_BASE_URL");
        std::env::remove_var("CLIPROXY_BASE_URL");

        std::env::set_var("CLI_PROXY_API_BASE_URL", "http://127.0.0.1:8317");
        assert_eq!(
            cli_proxy_chat_completions_url(),
            "http://127.0.0.1:8317/v1/chat/completions"
        );

        std::env::set_var("CLI_PROXY_API_BASE_URL", "http://127.0.0.1:8317/v1/");
        assert_eq!(
            cli_proxy_chat_completions_url(),
            "http://127.0.0.1:8317/v1/chat/completions"
        );

        std::env::set_var(
            "CLI_PROXY_API_BASE_URL",
            "http://127.0.0.1:8317/v1/chat/completions",
        );
        assert_eq!(
            cli_proxy_chat_completions_url(),
            "http://127.0.0.1:8317/v1/chat/completions"
        );

        match original_claude_code {
            Some(value) => std::env::set_var("CLAUDE_CODE_PROXY_BASE_URL", value),
            None => std::env::remove_var("CLAUDE_CODE_PROXY_BASE_URL"),
        }
        match original_cli_proxy {
            Some(value) => std::env::set_var("CLI_PROXY_API_BASE_URL", value),
            None => std::env::remove_var("CLI_PROXY_API_BASE_URL"),
        }
        match original_clip {
            Some(value) => std::env::set_var("CLIPROXY_API_BASE_URL", value),
            None => std::env::remove_var("CLIPROXY_API_BASE_URL"),
        }
        match original_legacy {
            Some(value) => std::env::set_var("CLIPROXY_BASE_URL", value),
            None => std::env::remove_var("CLIPROXY_BASE_URL"),
        }
    }

    #[test]
    fn anthropic_cli_proxy_rewrite_drops_deprecated_opus_47_sampling_params() {
        let body = serde_json::json!({
            "model": "opus",
            "messages": [{ "role": "user", "content": "ok" }],
            "max_tokens": 16,
            "temperature": 0,
            "top_p": 1,
            "top_k": 1,
            "thinking": { "type": "disabled" }
        });
        let payload = rewrite_model_for_anthropic_cli_proxy(
            serde_json::to_vec(&body).unwrap().as_slice(),
            "claude-opus-4-7",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(payload.as_ref()).unwrap();

        assert_eq!(value["model"], "claude-opus-4-7");
        assert!(value.get("temperature").is_none());
        assert!(value.get("top_p").is_none());
        assert!(value.get("top_k").is_none());
        assert_eq!(value["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    #[tokio::test(start_paused = true)]
    async fn track_stream_health_times_out_when_upstream_stalls() {
        let tracker = std::sync::Arc::new(crate::provider_health::ProviderHealthTracker::new());
        let account_id = uuid::Uuid::new_v4();

        // Stream that emits one chunk then sleeps far longer than the
        // idle timeout. With the paused tokio clock the sleep only
        // advances when the test advances time, so the idle watchdog
        // inside `track_stream_health` should win the race.
        let inner = async_stream::stream! {
            yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from("data: {}\n\n"));
            tokio::time::sleep(STREAM_IDLE_TIMEOUT * 10).await;
            yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from("never sent"));
        };

        let tracked = track_stream_health(inner, tracker.clone(), account_id, None, None);
        let mut tracked = std::pin::pin!(tracked);

        // First chunk should pass through immediately.
        let first = tracked.next().await.expect("first chunk").expect("ok");
        assert_eq!(first.as_ref(), b"data: {}\n\n");

        // Advance virtual time past the idle timeout. The watchdog
        // inside the stream should fire and yield a TimedOut error.
        tokio::time::advance(STREAM_IDLE_TIMEOUT + std::time::Duration::from_secs(1)).await;

        let second = tracked.next().await.expect("error item");
        let err = second.expect_err("idle timeout should produce error");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

        // Stream is closed after the error.
        assert!(tracked.next().await.is_none());

        // Health tracker should have recorded a Timeout-cause failure.
        let h = tracker.get_health(account_id).await;
        assert_eq!(h.last_failure_reason.as_deref(), Some("timeout"));
        assert!(!h.is_healthy, "account should be in cooldown after timeout");
    }

    #[tokio::test(start_paused = true)]
    async fn track_stream_health_passes_through_when_upstream_streams_normally() {
        let tracker = std::sync::Arc::new(crate::provider_health::ProviderHealthTracker::new());
        let account_id = uuid::Uuid::new_v4();

        let inner = async_stream::stream! {
            for i in 0..3 {
                yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(format!(
                    "data: {{\"chunk\":{}}}\n\n",
                    i
                )));
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        };

        let tracked = track_stream_health(inner, tracker.clone(), account_id, None, None);
        let mut tracked = std::pin::pin!(tracked);
        let mut count = 0;
        while let Some(item) = tracked.next().await {
            let bytes = item.expect("chunk should be ok");
            assert!(bytes.starts_with(b"data:"));
            count += 1;
            tokio::time::advance(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(count, 3);

        // Healthy account should not be cooled down.
        let h = tracker.get_health(account_id).await;
        assert_eq!(h.last_failure_reason, None);
        assert!(h.is_healthy);
    }
}
