//! OpenAI-compatible proxy endpoint.
//!
//! Receives `POST /v1/chat/completions` requests, resolves the model name
//! to a chain of provider+account entries, and forwards the request through
//! the chain until one succeeds. Pre-stream 429/529 errors trigger instant
//! failover to the next entry in the chain.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::ai_providers::ProviderType;
use crate::provider_health::CooldownReason;

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

// ─────────────────────────────────────────────────────────────────────────────
// Routes
// ─────────────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/chat/completions", post(chat_completions))
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

    let is_stream = req.stream.unwrap_or(false);
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

        // Custom providers may work without an API key (base_url only),
        // but standard providers always require one.
        if entry.api_key.is_none() && provider_type != ProviderType::Custom {
            continue;
        }

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
        if let Some(api_key) = &entry.api_key {
            upstream_req = upstream_req.header("Authorization", format!("Bearer {}", api_key));
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
                    .record_failure(entry.account_id, reason, None)
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
                .record_failure(entry.account_id, reason, retry_after)
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
                .record_failure(entry.account_id, CooldownReason::ServerError, None)
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
                .record_failure(entry.account_id, CooldownReason::AuthError, None)
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
                    .record_failure(entry.account_id, CooldownReason::ServerError, None)
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
                    .record_failure(entry.account_id, reason, None)
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
            let tracked_stream =
                track_stream_health(byte_stream, health_tracker, account_id, rate_limit_snapshot);

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
                                .record_failure(entry.account_id, reason, None)
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
                    state.health_tracker.record_success(entry.account_id).await;

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
                    .record_failure(entry.account_id, CooldownReason::ServerError, None)
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

/// Rewrite the `model` field in the JSON request body.
fn rewrite_model(body: &[u8], new_model: &str) -> Result<bytes::Bytes, String> {
    let mut value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    value["model"] = serde_json::Value::String(new_model.to_string());
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
fn track_stream_health(
    inner: impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
    health_tracker: crate::provider_health::SharedProviderHealthTracker,
    account_id: uuid::Uuid,
    rate_limit_snapshot: Option<crate::provider_health::RateLimitSnapshot>,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = std::pin::pin!(inner);
        let mut errored = false;
        let mut received_any = false;
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        while let Some(item) = stream.next().await {
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
        if errored || !received_any {
            health_tracker
                .record_failure(account_id, CooldownReason::ServerError, None)
                .await;
        } else {
            health_tracker.record_success(account_id).await;
            if input_tokens > 0 || output_tokens > 0 {
                health_tracker.record_token_usage(account_id, input_tokens, output_tokens).await;
            }
            if let Some(snapshot) = rate_limit_snapshot {
                health_tracker.record_rate_limits(account_id, snapshot).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(secs >= 55 && secs <= 65, "got {} seconds", secs);
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
        assert!(secs >= 25 && secs <= 35, "got {} seconds", secs);
    }
}
