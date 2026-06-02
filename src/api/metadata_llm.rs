//! Lightweight LLM client for generating mission metadata (titles & descriptions).
//!
//! Uses a cheap/fast model via OpenAI-compatible chat completions to produce
//! concise mission titles and status descriptions from conversation history.
//! Falls back gracefully when no provider is configured.

use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

/// Global metadata LLM client, initialized once at startup.
static METADATA_LLM: OnceLock<Arc<MetadataLlmClient>> = OnceLock::new();

/// API format for the metadata LLM provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFormat {
    /// OpenAI-compatible `/chat/completions` endpoint.
    OpenAI,
    /// Anthropic `/v1/messages` endpoint.
    Anthropic,
}

/// Configuration for the metadata LLM.
#[derive(Debug, Clone)]
pub struct MetadataLlmConfig {
    /// Base URL (e.g. `https://openrouter.ai/api/v1` or `https://api.anthropic.com`).
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Model ID (e.g. `google/gemini-2.0-flash-001`).
    pub model: String,
    /// API format to use.
    pub api_format: ApiFormat,
    /// For reasoning models (e.g. Cerebras `gpt-oss-120b`): the OpenAI-style
    /// `reasoning_effort` ("low"/"medium"/"high"). When set it's sent on the
    /// request and the token budget is raised — reasoning models spend the
    /// first tokens on hidden reasoning and would otherwise return empty
    /// `content`. `None` for plain chat models (gemini-flash, gpt-4.1-nano, …).
    pub reasoning_effort: Option<String>,
}

/// Lightweight client for metadata summarization.
pub struct MetadataLlmClient {
    config: RwLock<Option<MetadataLlmConfig>>,
    ai_providers: RwLock<Option<Arc<crate::ai_providers::AIProviderStore>>>,
    http: reqwest::Client,
}

impl std::fmt::Debug for MetadataLlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataLlmClient").finish()
    }
}

impl MetadataLlmClient {
    fn new(http: reqwest::Client) -> Self {
        Self {
            config: RwLock::new(None),
            ai_providers: RwLock::new(None),
            http,
        }
    }

    /// Update the LLM configuration (called when providers change).
    pub async fn set_config(&self, config: Option<MetadataLlmConfig>) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Store a reference to the AI provider store for self-refresh.
    pub async fn set_ai_providers(&self, providers: Arc<crate::ai_providers::AIProviderStore>) {
        let mut store = self.ai_providers.write().await;
        *store = Some(providers);
    }

    /// Refresh the LLM config from the AI provider store (picks up new OAuth tokens).
    async fn ensure_config_fresh(&self) {
        let store = self.ai_providers.read().await;
        if let Some(providers) = store.as_ref() {
            let new_config = try_build_config_from_providers(providers).await;
            let mut cfg = self.config.write().await;
            *cfg = new_config;
        }
    }

    /// Generate a title and short description for a mission.
    ///
    /// Returns `(title, short_description)` — either or both may be `None` if
    /// the LLM is unavailable or the call fails.
    pub async fn summarize_mission(
        &self,
        user_message: &str,
        assistant_reply: &str,
        existing_title: Option<&str>,
        is_refresh: bool,
    ) -> (Option<String>, Option<String>) {
        // Re-read provider config to pick up refreshed OAuth tokens
        self.ensure_config_fresh().await;

        let cfg = {
            let guard = self.config.read().await;
            match guard.as_ref() {
                Some(c) if !c.api_key.is_empty() => c.clone(),
                _ => return (None, None),
            }
        }; // lock released here before HTTP call

        let user_excerpt = truncate_to(user_message, 600);
        let assistant_excerpt = truncate_to(assistant_reply, 600);

        let system_prompt = if is_refresh && existing_title.is_some() {
            format!(
                "You summarize coding missions. The current title is: \"{}\"\n\
                 Based on the latest conversation, generate:\n\
                 1. A short title (3-7 words) summarizing the mission goal. Keep it if still accurate, or update if the focus changed.\n\
                 2. A one-sentence status description (max 15 words) of what's currently happening.\n\n\
                 Reply ONLY in this exact format:\n\
                 TITLE: <title>\nSTATUS: <status>",
                existing_title.unwrap_or("")
            )
        } else {
            "You summarize coding missions. Given a user request and assistant response, generate:\n\
             1. A short title (3-7 words) summarizing the mission goal.\n\
             2. A one-sentence status description (max 15 words) of what's currently happening.\n\n\
             Reply ONLY in this exact format:\n\
             TITLE: <title>\nSTATUS: <status>"
                .to_string()
        };

        let user_content = format!(
            "User request:\n{}\n\nAssistant response:\n{}",
            user_excerpt, assistant_excerpt
        );

        let (url, body, auth_header) = match cfg.api_format {
            ApiFormat::Anthropic => {
                let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
                let body = serde_json::json!({
                    "model": cfg.model,
                    "system": system_prompt,
                    "messages": [
                        { "role": "user", "content": user_content }
                    ],
                    "max_tokens": 80,
                    "temperature": 0.2,
                });
                (url, body, ("x-api-key".to_string(), cfg.api_key.clone()))
            }
            ApiFormat::OpenAI => {
                let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
                // Reasoning models emit hidden reasoning before the visible
                // answer, so an 80-token cap leaves `content` empty. Give them
                // room; plain chat models hit the EOS well before this.
                let max_tokens = if cfg.reasoning_effort.is_some() {
                    512
                } else {
                    80
                };
                let mut body = serde_json::json!({
                    "model": cfg.model,
                    "messages": [
                        { "role": "system", "content": system_prompt },
                        { "role": "user", "content": user_content }
                    ],
                    "max_tokens": max_tokens,
                    "temperature": 0.2,
                });
                if let Some(effort) = &cfg.reasoning_effort {
                    body["reasoning_effort"] = serde_json::json!(effort);
                }
                (
                    url,
                    body,
                    (
                        "Authorization".to_string(),
                        format!("Bearer {}", cfg.api_key),
                    ),
                )
            }
        };

        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header(&auth_header.0, &auth_header.1)
            .timeout(std::time::Duration::from_secs(10));

        if cfg.api_format == ApiFormat::Anthropic {
            req = req.header("anthropic-version", "2023-06-01");
        }

        let result = req.json(&body).send().await;

        let resp = match result {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::debug!("[MetadataLLM] Request failed with status {}", r.status());
                return (None, None);
            }
            Err(e) => {
                tracing::debug!("[MetadataLLM] Request error: {}", e);
                return (None, None);
            }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[MetadataLLM] Failed to parse response: {}", e);
                return (None, None);
            }
        };

        let text = match cfg.api_format {
            ApiFormat::Anthropic => {
                // Anthropic: {"content": [{"type": "text", "text": "..."}]}
                json["content"][0]["text"].as_str().unwrap_or("").trim()
            }
            ApiFormat::OpenAI => json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .trim(),
        };

        parse_title_status(text)
    }
}

/// Parse the `TITLE: ...\nSTATUS: ...` format from the LLM response.
fn parse_title_status(text: &str) -> (Option<String>, Option<String>) {
    let mut title: Option<String> = None;
    let mut status: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("TITLE:") {
            let t = rest.trim().trim_matches('"').trim();
            if !t.is_empty() && t.len() <= 100 {
                title = Some(t.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("STATUS:") {
            let s = rest.trim().trim_matches('"').trim();
            if !s.is_empty() && s.len() <= 200 {
                status = Some(s.to_string());
            }
        }
    }

    (title, status)
}

fn truncate_to(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── Global initialization & access ──────────────────────────────────────────

/// Initialize the global metadata LLM client. Call once at startup.
pub fn init_metadata_llm(http: reqwest::Client) {
    let _ = METADATA_LLM.set(Arc::new(MetadataLlmClient::new(http)));
}

/// Get a reference to the global metadata LLM client.
pub fn metadata_llm() -> Option<&'static Arc<MetadataLlmClient>> {
    METADATA_LLM.get()
}

/// Reconfigure the metadata LLM from the current AI provider store.
/// Called at startup and whenever providers are updated.
pub async fn refresh_metadata_llm_config(ai_providers: &crate::ai_providers::AIProviderStore) {
    let client = match metadata_llm() {
        Some(c) => c,
        None => return,
    };

    // Prefer OpenRouter (cheap, fast models), then fall back to default provider.
    let config = try_build_config_from_providers(ai_providers).await;
    client.set_config(config).await;
}

async fn try_build_config_from_providers(
    ai_providers: &crate::ai_providers::AIProviderStore,
) -> Option<MetadataLlmConfig> {
    use crate::ai_providers::ProviderType;

    /// Resolve the API key/token for a provider: use the stored key first,
    /// then OAuth credentials from disk, then the provider type's env var.
    fn resolve_api_key(provider: &crate::ai_providers::AIProvider) -> Option<String> {
        if let Some(ref key) = provider.api_key {
            return Some(key.clone());
        }
        // Check OAuth credentials from disk (source of truth, updated by
        // background refresh). The store's oauth.access_token can be stale.
        if let Some(entry) =
            crate::api::ai_providers::read_oauth_token_entry(provider.provider_type)
        {
            if !entry.access_token.is_empty()
                && !crate::api::ai_providers::oauth_token_expired(entry.expires_at)
            {
                return Some(entry.access_token);
            }
        }
        if let Some(env_var) = provider.provider_type.env_var_name() {
            if let Ok(key) = std::env::var(env_var) {
                if !key.trim().is_empty() {
                    return Some(key);
                }
            }
        }
        None
    }

    // Provider candidates in priority order (cheapest/fastest first).
    // (provider_type, default_base_url, model, api_format, reasoning_effort)
    let candidates: &[(ProviderType, &str, &str, ApiFormat, Option<&str>)] = &[
        (
            ProviderType::OpenRouter,
            "https://openrouter.ai/api/v1",
            "google/gemini-2.0-flash-001",
            ApiFormat::OpenAI,
            None,
        ),
        (
            ProviderType::Groq,
            "https://api.groq.com/openai/v1",
            "llama-3.3-70b-versatile",
            ApiFormat::OpenAI,
            None,
        ),
        (
            // Cerebras only serves reasoning models now (gpt-oss-120b,
            // zai-glm-4.7); the old `llama3.1-8b` 404s. gpt-oss-120b with
            // reasoning_effort=low returns a clean TITLE/STATUS in ~300ms.
            ProviderType::Cerebras,
            "https://api.cerebras.ai/v1",
            "gpt-oss-120b",
            ApiFormat::OpenAI,
            Some("low"),
        ),
        (
            ProviderType::OpenAI,
            "https://api.openai.com/v1",
            "gpt-4.1-nano",
            ApiFormat::OpenAI,
            None,
        ),
        (
            ProviderType::Anthropic,
            "https://api.anthropic.com",
            "claude-haiku-4-5-20251001",
            ApiFormat::Anthropic,
            None,
        ),
    ];

    for (provider_type, default_base_url, model, api_format, reasoning_effort) in candidates {
        if let Some(provider) = ai_providers.get_by_type(*provider_type).await {
            if let Some(api_key) = resolve_api_key(&provider) {
                tracing::info!(
                    "[MetadataLLM] Using {} provider",
                    provider_type.display_name()
                );
                return Some(MetadataLlmConfig {
                    base_url: provider
                        .base_url
                        .clone()
                        .unwrap_or_else(|| default_base_url.to_string()),
                    api_key,
                    model: model.to_string(),
                    api_format: *api_format,
                    reasoning_effort: reasoning_effort.map(|s| s.to_string()),
                });
            }
        }
    }

    // Try Google Gemini via OAuth (OpenAI-compatible endpoint).
    // Read from credential files (source of truth) rather than the provider
    // store, since the store's oauth.access_token is not updated when the
    // background refresh task rotates tokens.
    if ai_providers
        .get_by_type(ProviderType::Google)
        .await
        .is_some()
    {
        if let Some(entry) = crate::api::ai_providers::read_oauth_token_entry(ProviderType::Google)
        {
            if !entry.access_token.is_empty()
                && !crate::api::ai_providers::oauth_token_expired(entry.expires_at)
            {
                tracing::info!("[MetadataLLM] Using Google Gemini via OAuth");
                return Some(MetadataLlmConfig {
                    base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                    api_key: entry.access_token,
                    model: "gemini-2.0-flash".to_string(),
                    api_format: ApiFormat::OpenAI,
                    reasoning_effort: None,
                });
            }
        }
    }

    // Final fallback: check environment variables for providers not in the store
    // (env_var, base_url, model, api_format, reasoning_effort)
    let env_providers: &[(&str, &str, &str, ApiFormat, Option<&str>)] = &[
        (
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/v1",
            "google/gemini-2.0-flash-001",
            ApiFormat::OpenAI,
            None,
        ),
        (
            "CEREBRAS_API_KEY",
            "https://api.cerebras.ai/v1",
            "gpt-oss-120b",
            ApiFormat::OpenAI,
            Some("low"),
        ),
        (
            "GROQ_API_KEY",
            "https://api.groq.com/openai/v1",
            "llama-3.3-70b-versatile",
            ApiFormat::OpenAI,
            None,
        ),
        (
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            "gpt-4.1-nano",
            ApiFormat::OpenAI,
            None,
        ),
        (
            "ANTHROPIC_API_KEY",
            "https://api.anthropic.com",
            "claude-haiku-4-5-20251001",
            ApiFormat::Anthropic,
            None,
        ),
    ];
    for (env_var, base_url, model, api_format, reasoning_effort) in env_providers {
        if let Ok(api_key) = std::env::var(env_var) {
            if !api_key.trim().is_empty() {
                tracing::info!("[MetadataLLM] Using {} from environment", env_var);
                return Some(MetadataLlmConfig {
                    base_url: base_url.to_string(),
                    api_key,
                    model: model.to_string(),
                    api_format: *api_format,
                    reasoning_effort: reasoning_effort.map(|s| s.to_string()),
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_title_status_basic() {
        let (title, status) =
            parse_title_status("TITLE: Fix CI Pipeline Flaky Tests\nSTATUS: Investigating intermittent test failures in auth module");
        assert_eq!(title.as_deref(), Some("Fix CI Pipeline Flaky Tests"));
        assert_eq!(
            status.as_deref(),
            Some("Investigating intermittent test failures in auth module")
        );
    }

    #[test]
    fn test_parse_title_status_with_quotes() {
        let (title, status) = parse_title_status(
            "TITLE: \"Refactor Database Layer\"\nSTATUS: \"Migrating from raw SQL to ORM\"",
        );
        assert_eq!(title.as_deref(), Some("Refactor Database Layer"));
        assert_eq!(status.as_deref(), Some("Migrating from raw SQL to ORM"));
    }

    #[test]
    fn test_parse_title_status_missing_status() {
        let (title, status) = parse_title_status("TITLE: Quick Fix\n");
        assert_eq!(title.as_deref(), Some("Quick Fix"));
        assert!(status.is_none());
    }

    #[test]
    fn test_parse_title_status_empty() {
        let (title, status) = parse_title_status("");
        assert!(title.is_none());
        assert!(status.is_none());
    }

    #[test]
    fn test_truncate_to() {
        assert_eq!(truncate_to("hello world", 5), "hello");
        assert_eq!(truncate_to("hello", 10), "hello");
        // Unicode boundary safety
        assert_eq!(truncate_to("héllo", 2), "h");
    }
}
