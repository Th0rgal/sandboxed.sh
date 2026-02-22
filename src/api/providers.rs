//! Provider catalog API.
//!
//! Provides endpoints for listing available providers and their models for UI selection.
//! Only returns providers that are actually configured and authenticated.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::routes::AppState;
use crate::ai_providers::{AIProviderStore, ProviderType};
use crate::util::{auth_entry_has_credentials, home_dir, AI_PROVIDERS_PATH};

/// Cached model lists fetched from provider APIs at startup.
/// Maps provider ID (e.g. "anthropic") -> Vec<ProviderModel>
pub type ModelCatalog = Arc<RwLock<HashMap<String, Vec<ProviderModel>>>>;

#[derive(Debug, Clone)]
struct CodexProbeCacheEntry {
    visible_models: HashSet<String>,
    expires_at: Instant,
}

static CODEX_MODEL_PROBE_CACHE: OnceLock<RwLock<HashMap<String, CodexProbeCacheEntry>>> =
    OnceLock::new();

const CODEX_MODEL_PROBE_TTL: Duration = Duration::from_secs(10 * 60);

/// Provider IDs that are part of the default catalog and should not be duplicated
/// from the AIProviderStore.
pub const DEFAULT_CATALOG_PROVIDER_IDS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "xai",
    "cerebras",
    "zai",
    "minimax",
];

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

/// Model option for a specific backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendModelOption {
    /// Model value to submit (raw model id or provider/model)
    pub value: String,
    /// UI label
    pub label: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
    /// Provider ID (for custom providers, shows the sanitized ID used in config)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// Response for backend model options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendModelOptionsResponse {
    pub backends: std::collections::HashMap<String, Vec<BackendModelOption>>,
}

/// Query parameters for backend models endpoint.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BackendModelsQuery {
    /// Include providers even if they are not configured/authenticated.
    #[serde(default)]
    pub include_all: bool,
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

fn sanitize_custom_provider_id(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
        .replace('-', "_")
}

fn codex_model_probe_cache() -> &'static RwLock<HashMap<String, CodexProbeCacheEntry>> {
    CODEX_MODEL_PROBE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn hash_u64(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn codex_probe_signature(api_keys: &[String], candidates: &[String]) -> String {
    let mut key_hashes: Vec<u64> = api_keys.iter().map(|k| hash_u64(k)).collect();
    key_hashes.sort_unstable();
    let mut models = candidates.to_vec();
    models.sort_unstable();
    format!(
        "keys:{}|models:{}",
        key_hashes
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(","),
        models.join(",")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexProbeOutcome {
    Supported,
    Unsupported,
    Inconclusive,
}

async fn probe_codex_model_access(
    client: &reqwest::Client,
    api_key: &str,
    model_id: &str,
) -> CodexProbeOutcome {
    let response = match client
        .post("https://api.openai.com/v1/responses")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model_id,
            "input": "Ping",
            "max_output_tokens": 1
        }))
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            tracing::debug!(
                model = %model_id,
                error = %err,
                "Codex model probe failed to reach OpenAI"
            );
            return CodexProbeOutcome::Inconclusive;
        }
    };

    if response.status().is_success() {
        return CodexProbeOutcome::Supported;
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let lower = body.to_lowercase();
    let definitive_model_error = lower.contains("does not exist or you do not have access")
        || lower.contains("model_not_found")
        || lower.contains("does_not_exist")
        || lower.contains("unknown model");

    if definitive_model_error || status.as_u16() == 404 {
        CodexProbeOutcome::Unsupported
    } else {
        tracing::debug!(
            model = %model_id,
            status = %status,
            body_preview = %body.chars().take(240).collect::<String>(),
            "Codex model probe was inconclusive"
        );
        CodexProbeOutcome::Inconclusive
    }
}

async fn resolve_visible_codex_models(
    state: &AppState,
    candidates: &[String],
) -> Option<HashSet<String>> {
    let api_keys =
        crate::api::ai_providers::get_all_openai_keys_for_codex(state.config.working_dir.as_path());
    if api_keys.is_empty() {
        return None;
    }

    let signature = codex_probe_signature(&api_keys, candidates);
    let now = Instant::now();
    if let Some(entry) = codex_model_probe_cache()
        .read()
        .await
        .get(&signature)
        .cloned()
    {
        if entry.expires_at > now {
            return Some(entry.visible_models);
        }
    }

    let mut visible_models = HashSet::new();
    for model_id in candidates {
        let mut is_supported = false;
        let mut saw_inconclusive = false;

        for api_key in &api_keys {
            match probe_codex_model_access(&state.http_client, api_key, model_id).await {
                CodexProbeOutcome::Supported => {
                    is_supported = true;
                    break;
                }
                CodexProbeOutcome::Unsupported => {}
                CodexProbeOutcome::Inconclusive => {
                    saw_inconclusive = true;
                }
            }
        }

        if is_supported || saw_inconclusive {
            visible_models.insert(model_id.clone());
        }
    }

    codex_model_probe_cache().write().await.insert(
        signature,
        CodexProbeCacheEntry {
            visible_models: visible_models.clone(),
            expires_at: now + CODEX_MODEL_PROBE_TTL,
        },
    );

    tracing::info!(
        total_candidates = candidates.len(),
        visible = visible_models.len(),
        "Resolved account-specific Codex model visibility"
    );

    Some(visible_models)
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
                        id: "claude-opus-4-6".to_string(),
                        name: "Claude Opus 4.6".to_string(),
                        description: Some(
                            "Most capable, recommended for complex tasks".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-sonnet-4-5-20250929".to_string(),
                        name: "Claude Sonnet 4.5".to_string(),
                        description: Some("Balanced speed and capability".to_string()),
                    },
                    ProviderModel {
                        id: "claude-opus-4-5-20251101".to_string(),
                        name: "Claude Opus 4.5".to_string(),
                        description: Some(
                            "Most capable, recommended for complex tasks".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-sonnet-5".to_string(),
                        name: "Claude Sonnet 5".to_string(),
                        description: Some("Balanced speed and capability".to_string()),
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
                    // Codex-optimized models (for Codex CLI)
                    ProviderModel {
                        id: "gpt-5-codex".to_string(),
                        name: "GPT-5 Codex".to_string(),
                        description: Some("Purpose-built for agentic coding".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5-codex-mini".to_string(),
                        name: "GPT-5 Codex Mini".to_string(),
                        description: Some("Smaller, cost-effective variant".to_string()),
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
                        id: "gpt-5.2-codex".to_string(),
                        name: "GPT-5.2 Codex".to_string(),
                        description: Some("Smart and precise coding agent".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.3-codex".to_string(),
                        name: "GPT-5.3 Codex".to_string(),
                        description: Some("Newest Codex coding model".to_string()),
                    },
                    // General-purpose models (API key only)
                    ProviderModel {
                        id: "gpt-5.3".to_string(),
                        name: "GPT-5.3".to_string(),
                        description: Some("General-purpose GPT-5.3".to_string()),
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
                        id: "gemini-3.1-pro-preview".to_string(),
                        name: "Gemini 3.1 Pro Preview".to_string(),
                        description: Some(
                            "Advanced reasoning with three-tier thinking".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gemini-3-pro-preview".to_string(),
                        name: "Gemini 3 Pro Preview".to_string(),
                        description: Some("State-of-the-art reasoning and multimodal".to_string()),
                    },
                    ProviderModel {
                        id: "gemini-3-flash-preview".to_string(),
                        name: "Gemini 3 Flash Preview".to_string(),
                        description: Some("Fast frontier-class performance".to_string()),
                    },
                    ProviderModel {
                        id: "gemini-2.5-pro".to_string(),
                        name: "Gemini 2.5 Pro".to_string(),
                        description: Some("Advanced reasoning and long context".to_string()),
                    },
                    ProviderModel {
                        id: "gemini-2.5-flash".to_string(),
                        name: "Gemini 2.5 Flash".to_string(),
                        description: Some("Fast and efficient with thinking".to_string()),
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
                        id: "grok-4-fast".to_string(),
                        name: "Grok 4 Fast".to_string(),
                        description: Some("Most capable Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-3".to_string(),
                        name: "Grok 3".to_string(),
                        description: Some("Balanced capability and speed".to_string()),
                    },
                    ProviderModel {
                        id: "grok-3-fast".to_string(),
                        name: "Grok 3 Fast".to_string(),
                        description: Some("Fast Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-3-mini".to_string(),
                        name: "Grok 3 Mini".to_string(),
                        description: Some("Fast and economical".to_string()),
                    },
                ],
            },
            Provider {
                id: "cerebras".to_string(),
                name: "Cerebras (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "Ultra-fast inference via Cerebras".to_string(),
                models: vec![
                    ProviderModel {
                        id: "gpt-oss-120b-cs".to_string(),
                        name: "GPT-OSS 120B".to_string(),
                        description: Some("Most capable Cerebras model".to_string()),
                    },
                    ProviderModel {
                        id: "zai-glm-4.6-cs".to_string(),
                        name: "GLM-4.6 (Cerebras)".to_string(),
                        description: Some("GLM-4.6 via Cerebras inference".to_string()),
                    },
                ],
            },
            Provider {
                id: "zai".to_string(),
                name: "Z.AI (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "GLM models via Z.AI API key".to_string(),
                models: vec![
                    ProviderModel {
                        id: "glm-4.7".to_string(),
                        name: "GLM-4.7".to_string(),
                        description: Some("Most capable GLM model".to_string()),
                    },
                    ProviderModel {
                        id: "glm-4.6".to_string(),
                        name: "GLM-4.6".to_string(),
                        description: Some("Balanced capability and speed".to_string()),
                    },
                    ProviderModel {
                        id: "glm-4.5".to_string(),
                        name: "GLM-4.5".to_string(),
                        description: Some("Fast and economical".to_string()),
                    },
                    ProviderModel {
                        id: "glm-4.6v-flash".to_string(),
                        name: "GLM-4.6V Flash".to_string(),
                        description: Some("Vision model, fast variant".to_string()),
                    },
                ],
            },
            Provider {
                id: "minimax".to_string(),
                name: "Minimax (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "MiniMax models via Minimax API key".to_string(),
                models: vec![
                    ProviderModel {
                        id: "MiniMax-M2.5".to_string(),
                        name: "MiniMax M2.5".to_string(),
                        description: Some("Most capable MiniMax model".to_string()),
                    },
                    ProviderModel {
                        id: "MiniMax-M2.5-highspeed".to_string(),
                        name: "MiniMax M2.5 Highspeed".to_string(),
                        description: Some("Fast variant of M2.5".to_string()),
                    },
                    ProviderModel {
                        id: "MiniMax-M2.1".to_string(),
                        name: "MiniMax M2.1".to_string(),
                        description: Some("Balanced capability and speed".to_string()),
                    },
                    ProviderModel {
                        id: "MiniMax-M2".to_string(),
                        name: "MiniMax M2".to_string(),
                        description: Some("Fast and economical".to_string()),
                    },
                ],
            },
        ],
    }
}

// ==================== Dynamic Model Catalog Fetching ====================

/// Convert a model ID to a human-readable display name by title-casing segments.
/// e.g. "glm-5" -> "GLM 5", "grok-4-fast" -> "Grok 4 Fast", "gpt-5.3-codex" -> "GPT 5.3 Codex"
fn model_id_to_display_name(id: &str) -> String {
    id.split('-')
        .map(|segment| {
            // If the segment is all-alpha and <= 3 chars, uppercase it (likely an acronym: gpt, glm, etc.)
            if segment.chars().all(|c| c.is_ascii_alphabetic()) && segment.len() <= 3 {
                segment.to_uppercase()
            } else {
                // Title-case: capitalize first letter
                let mut chars = segment.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        let mut s = first.to_uppercase().to_string();
                        s.extend(chars);
                        s
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fetch models from an OpenAI-compatible /v1/models endpoint.
/// Filters results by the given prefix (e.g. "grok-", "glm-").
/// Returns model IDs and generated display names.
pub async fn fetch_openai_compatible_models(
    base_url: &str,
    api_key: &str,
    prefix_filters: &[&str],
) -> Result<Vec<ProviderModel>, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "Missing 'data' array in response".to_string())?;

    let mut models: Vec<ProviderModel> = data
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            // Apply prefix filter if any
            if !prefix_filters.is_empty()
                && !prefix_filters.iter().any(|prefix| id.starts_with(prefix))
            {
                return None;
            }
            Some(ProviderModel {
                id: id.to_string(),
                name: model_id_to_display_name(id),
                description: None,
            })
        })
        .collect();

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

/// Fetch models from the Anthropic /v1/models endpoint.
/// Uses Anthropic's custom auth headers and `display_name` field.
pub async fn fetch_anthropic_models(api_key: &str) -> Result<Vec<ProviderModel>, String> {
    let client = reqwest::Client::new();
    let url = "https://api.anthropic.com/v1/models?limit=100";

    let resp = client
        .get(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let data = body
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "Missing 'data' array in response".to_string())?;

    let mut models: Vec<ProviderModel> = data
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let display_name = entry
                .get("display_name")
                .and_then(|n| n.as_str())
                .unwrap_or(id);
            Some(ProviderModel {
                id: id.to_string(),
                name: display_name.to_string(),
                description: None,
            })
        })
        .collect();

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

/// Resolve an API key for a given provider type.
///
/// Checks three sources in order:
/// 1. AIProviderStore (custom providers with stored keys)
/// 2. OpenCode auth files (~/.local/share/opencode/auth.json, ~/.opencode/auth/{provider}.json)
/// 3. Environment variable (e.g. ANTHROPIC_API_KEY)
pub fn get_api_key_for_provider(
    provider_type: ProviderType,
    ai_providers: &[crate::ai_providers::AIProvider],
) -> Option<String> {
    // 1. Check AIProviderStore entries
    for provider in ai_providers {
        if provider.provider_type == provider_type && provider.enabled {
            if let Some(ref key) = provider.api_key {
                if !key.is_empty() {
                    return Some(key.clone());
                }
            }
            // OAuth access tokens can also be used as bearer tokens for some APIs
            if let Some(ref oauth) = provider.oauth {
                if !oauth.access_token.is_empty() {
                    return Some(oauth.access_token.clone());
                }
            }
        }
    }

    // 2. Check OpenCode auth.json
    let home = home_dir();
    let auth_paths = {
        let mut paths = Vec::new();
        if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
            paths.push(
                std::path::PathBuf::from(data_home)
                    .join("opencode")
                    .join("auth.json"),
            );
        }
        paths.push(std::path::PathBuf::from(&home).join(".local/share/opencode/auth.json"));
        paths
    };

    let auth_keys: Vec<&str> = match provider_type {
        ProviderType::OpenAI => vec!["openai", "codex"],
        ProviderType::Custom => vec![],
        _ => vec![provider_type.id()],
    };

    for auth_path in &auth_paths {
        if let Ok(contents) = std::fs::read_to_string(auth_path) {
            if let Ok(auth) = serde_json::from_str::<serde_json::Value>(&contents) {
                for key in &auth_keys {
                    if let Some(entry) = auth.get(*key) {
                        // Check for API key fields
                        for field in &["key", "api_key", "apiKey"] {
                            if let Some(val) = entry.get(*field).and_then(|v| v.as_str()) {
                                if !val.is_empty() {
                                    return Some(val.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Also check provider-specific auth files (~/.opencode/auth/{provider}.json)
    let provider_auth_file = std::path::PathBuf::from(&home)
        .join(".opencode/auth")
        .join(format!("{}.json", provider_type.id()));
    if let Ok(contents) = std::fs::read_to_string(&provider_auth_file) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
            for field in &["key", "api_key", "apiKey"] {
                if let Some(val) = value.get(*field).and_then(|v| v.as_str()) {
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }

    // 3. Check environment variable
    if let Some(env_var) = provider_type.env_var_name() {
        if let Ok(val) = std::env::var(env_var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }

    None
}

/// Fetch model lists from all supported provider APIs concurrently.
///
/// Returns a map of provider ID -> fetched models. Providers that fail
/// or lack credentials are simply omitted (hardcoded defaults will be used).
pub async fn fetch_model_catalog(
    ai_providers: &AIProviderStore,
    _working_dir: &Path,
) -> HashMap<String, Vec<ProviderModel>> {
    let providers_list = ai_providers.list().await;
    let mut result = HashMap::new();

    // Define fetchable providers (Google uses OAuth which is complex, skip it)
    struct FetchTarget {
        provider_type: ProviderType,
        provider_id: &'static str,
        base_url: &'static str,
        prefix_filters: Vec<&'static str>,
    }

    let targets = vec![
        FetchTarget {
            provider_type: ProviderType::OpenAI,
            provider_id: "openai",
            base_url: "https://api.openai.com/v1",
            prefix_filters: vec!["gpt-", "o1-", "o3-", "o4-", "chatgpt-"],
        },
        FetchTarget {
            provider_type: ProviderType::Xai,
            provider_id: "xai",
            base_url: "https://api.x.ai/v1",
            prefix_filters: vec!["grok-"],
        },
        FetchTarget {
            provider_type: ProviderType::Cerebras,
            provider_id: "cerebras",
            base_url: "https://api.cerebras.ai/v1",
            prefix_filters: vec![],
        },
        FetchTarget {
            provider_type: ProviderType::Zai,
            provider_id: "zai",
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            prefix_filters: vec!["glm-"],
        },
        FetchTarget {
            provider_type: ProviderType::Minimax,
            provider_id: "minimax",
            base_url: "https://api.minimax.io/v1",
            prefix_filters: vec!["MiniMax-"],
        },
    ];

    // Resolve API keys for all targets + Anthropic
    let anthropic_key = get_api_key_for_provider(ProviderType::Anthropic, &providers_list);
    let target_keys: Vec<(FetchTarget, Option<String>)> = targets
        .into_iter()
        .map(|t| {
            let key = get_api_key_for_provider(t.provider_type, &providers_list);
            (t, key)
        })
        .collect();

    // Fetch Anthropic (special format)
    let anthropic_handle = tokio::spawn(async move {
        match anthropic_key {
            Some(key) => match fetch_anthropic_models(&key).await {
                Ok(models) => {
                    tracing::info!("Fetched {} models from Anthropic API", models.len());
                    Some(("anthropic".to_string(), models))
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch Anthropic models: {}", e);
                    None
                }
            },
            None => {
                tracing::debug!("No API key for Anthropic, skipping model fetch");
                None
            }
        }
    });

    // Fetch OpenAI-compatible providers concurrently
    let mut handles = vec![anthropic_handle];
    for (target, key) in target_keys {
        let provider_id = target.provider_id.to_string();
        let base_url = target.base_url.to_string();
        let prefix_filters: Vec<String> = target
            .prefix_filters
            .iter()
            .map(|s| s.to_string())
            .collect();

        handles.push(tokio::spawn(async move {
            match key {
                Some(api_key) => {
                    let filters: Vec<&str> = prefix_filters.iter().map(|s| s.as_str()).collect();
                    match fetch_openai_compatible_models(&base_url, &api_key, &filters).await {
                        Ok(models) => {
                            tracing::info!(
                                "Fetched {} models from {} API",
                                models.len(),
                                provider_id
                            );
                            Some((provider_id, models))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to fetch {} models: {}", provider_id, e);
                            None
                        }
                    }
                }
                None => {
                    tracing::debug!("No API key for {}, skipping model fetch", provider_id);
                    None
                }
            }
        }));
    }

    // Collect results
    for handle in handles {
        if let Ok(Some((provider_id, models))) = handle.await {
            if !models.is_empty() {
                result.insert(provider_id, models);
            }
        }
    }

    result
}

/// Check if a JSON value contains valid auth credentials.
/// Get the set of configured provider IDs from OpenCode's auth files.
fn get_configured_provider_ids(working_dir: &std::path::Path) -> HashSet<String> {
    let mut configured = HashSet::new();
    let home = home_dir();

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
                    if auth_entry_has_credentials(value) {
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
                if auth_entry_has_credentials(&value) {
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
    let ai_providers_path = working_dir.join(AI_PROVIDERS_PATH);
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
    let mut config = load_providers_config(&working_dir);

    // Merge cached models from dynamic catalog (replaces hardcoded model lists)
    let cached = state.model_catalog.read().await;
    for provider in &mut config.providers {
        if let Some(models) = cached.get(&provider.id) {
            provider.models = models.clone();
        }
    }
    drop(cached);

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

/// List model options grouped by backend (claudecode, codex, opencode).
///
/// This is used by the frontend to power per-harness model override pickers.
pub async fn list_backend_model_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BackendModelsQuery>,
) -> Json<BackendModelOptionsResponse> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    // Merge cached models from dynamic catalog
    let cached = state.model_catalog.read().await;
    for provider in &mut config.providers {
        if let Some(models) = cached.get(&provider.id) {
            provider.models = models.clone();
        }
    }
    drop(cached);

    let configured = get_configured_provider_ids(state.config.working_dir.as_path());
    let mut providers = if query.include_all {
        config.providers
    } else {
        config
            .providers
            .into_iter()
            .filter(|p| configured.contains(&p.id))
            .collect()
    };

    // Add non-default providers from AIProviderStore (Custom, Cerebras, Zai, etc.)
    let default_provider_ids = DEFAULT_CATALOG_PROVIDER_IDS;
    let custom_providers = state.ai_providers.list().await;
    for provider in custom_providers {
        // Skip disabled providers and those already in the default catalog
        if !provider.enabled || default_provider_ids.contains(&provider.provider_type.id()) {
            continue;
        }
        if !query.include_all && !provider.has_credentials() {
            continue;
        }
        // Use the canonical provider type ID for known types, sanitized name for Custom
        let id = if provider.provider_type == ProviderType::Custom {
            sanitize_custom_provider_id(&provider.name)
        } else {
            provider.provider_type.id().to_string()
        };
        let models = provider
            .custom_models
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|model| ProviderModel {
                id: model.id,
                name: model.name.unwrap_or_else(|| "Custom model".to_string()),
                description: None,
            })
            .collect();
        providers.push(Provider {
            id,
            name: provider.name.clone(),
            billing: "custom".to_string(),
            description: "Custom provider".to_string(),
            models,
        });
    }

    let mut backends: std::collections::HashMap<String, Vec<BackendModelOption>> =
        std::collections::HashMap::new();

    let mut push_options =
        |backend: &str,
         allowlist: Option<&[&str]>,
         use_provider_prefix: bool,
         model_filter: Option<&dyn Fn(&str) -> bool>| {
            let mut options = Vec::new();
            for provider in &providers {
                if let Some(allowed) = allowlist {
                    if !allowed.iter().any(|id| *id == provider.id) {
                        continue;
                    }
                }
                // Determine if this is a custom provider (billing type "custom")
                let is_custom = provider.billing == "custom";
                for model in &provider.models {
                    if let Some(ref filter) = model_filter {
                        if !filter(&model.id) {
                            continue;
                        }
                    }
                    let value = if use_provider_prefix {
                        format!("{}/{}", provider.id, model.id)
                    } else {
                        model.id.clone()
                    };
                    options.push(BackendModelOption {
                        value,
                        label: format!("{} — {}", provider.name, model.name),
                        description: model.description.clone(),
                        // Include provider_id for custom providers to show the resolved ID
                        provider_id: if is_custom {
                            Some(provider.id.clone())
                        } else {
                            None
                        },
                    });
                }
            }
            backends.insert(backend.to_string(), options);
        };

    push_options("claudecode", Some(&["anthropic"]), false, None);
    // Only show Codex-optimized models (contain "codex" in the ID).
    let codex_filter: &dyn Fn(&str) -> bool = &|id: &str| id.contains("codex");
    push_options("codex", Some(&["openai"]), false, Some(codex_filter));
    push_options("opencode", None, true, None);
    backends.entry("amp".to_string()).or_default();

    let codex_candidates: Vec<String> = backends
        .get("codex")
        .map(|opts| opts.iter().map(|o| o.value.clone()).collect())
        .unwrap_or_default();
    if !codex_candidates.is_empty() {
        if let Some(visible_models) = resolve_visible_codex_models(&state, &codex_candidates).await
        {
            if let Some(options) = backends.get_mut("codex") {
                let before = options.len();
                options.retain(|opt| visible_models.contains(&opt.value));
                tracing::info!(
                    before,
                    after = options.len(),
                    "Filtered Codex model options to account-supported models"
                );
            }
        }
    }

    // Prepend model routing chains to opencode options so they appear first
    let chains = state.chain_store.list().await;
    if !chains.is_empty() {
        let opencode_opts = backends.entry("opencode".to_string()).or_default();
        let mut chain_options: Vec<BackendModelOption> = chains
            .iter()
            .map(|c| {
                let entries_desc: Vec<String> = c
                    .entries
                    .iter()
                    .map(|e| format!("{}/{}", e.provider_id, e.model_id))
                    .collect();
                BackendModelOption {
                    value: c.id.clone(),
                    label: format!("Routing — {}", c.name),
                    description: Some(entries_desc.join(" → ")),
                    provider_id: None,
                }
            })
            .collect();
        chain_options.append(opencode_opts);
        *opencode_opts = chain_options;
    }

    Json(BackendModelOptionsResponse { backends })
}

/// Validate a model override for a specific backend.
/// Returns Ok(()) if valid, Err with user-friendly error message if invalid.
/// Allows custom/unknown models (escape hatch) but validates known providers.
pub async fn validate_model_override(
    state: &AppState,
    backend: &str,
    model_override: &str,
) -> Result<(), String> {
    // Amp ignores model overrides, so no validation needed
    if backend == "amp" {
        return Ok(());
    }

    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    // Merge cached models from dynamic catalog
    let cached = state.model_catalog.read().await;
    for provider in &mut config.providers {
        if let Some(models) = cached.get(&provider.id) {
            provider.models = models.clone();
        }
    }
    drop(cached);

    // Load all providers (including configured and non-default)
    let mut providers = config.providers;
    let default_provider_ids = DEFAULT_CATALOG_PROVIDER_IDS;
    let custom_providers = state.ai_providers.list().await;
    for provider in custom_providers {
        // Skip disabled providers and those already in the default catalog
        if !provider.enabled || default_provider_ids.contains(&provider.provider_type.id()) {
            continue;
        }
        let id = if provider.provider_type == ProviderType::Custom {
            sanitize_custom_provider_id(&provider.name)
        } else {
            provider.provider_type.id().to_string()
        };
        let models = provider
            .custom_models
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|model| ProviderModel {
                id: model.id,
                name: model.name.unwrap_or_else(|| "Custom model".to_string()),
                description: None,
            })
            .collect();
        providers.push(Provider {
            id,
            name: provider.name.clone(),
            billing: "custom".to_string(),
            description: "Custom provider".to_string(),
            models,
        });
    }

    match backend {
        "opencode" => {
            // OpenCode expects "provider/model" format
            if let Some((provider_id, model_id)) = model_override.split_once('/') {
                // Check if this is a known provider with a model catalog
                if let Some(provider) = providers.iter().find(|p| p.id == provider_id) {
                    // Only validate if the provider has a non-empty model list.
                    // Providers with no catalog (e.g. typed providers without custom_models)
                    // get the same escape-hatch treatment as unknown providers.
                    if !provider.models.is_empty()
                        && !provider.models.iter().any(|m| m.id == model_id)
                    {
                        return Err(format!(
                            "Model '{}' not found for provider '{}'. Available models: {}",
                            model_id,
                            provider_id,
                            provider
                                .models
                                .iter()
                                .map(|m| &m.id)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                // Unknown provider - allow as custom (escape hatch)
                Ok(())
            } else {
                Err(format!(
                    "Invalid format for OpenCode model override. Expected 'provider/model' (e.g., 'openai/gpt-4'), got '{}'",
                    model_override
                ))
            }
        }
        "claudecode" => {
            // Claude Code expects raw model IDs from Anthropic
            let anthropic = providers.iter().find(|p| p.id == "anthropic");
            if let Some(provider) = anthropic {
                if !provider.models.iter().any(|m| m.id == model_override) {
                    // Check if it looks like a Claude model (starts with "claude-")
                    if model_override.starts_with("claude-") {
                        // Allow unknown Claude models (escape hatch for new models)
                        Ok(())
                    } else {
                        Err(format!(
                            "Model '{}' not found in Anthropic catalog. Available models: {}. For custom Claude models, use format 'claude-*'",
                            model_override,
                            provider
                                .models
                                .iter()
                                .map(|m| &m.id)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                } else {
                    Ok(())
                }
            } else {
                // Anthropic not configured, but allow if it looks like a Claude model
                if model_override.starts_with("claude-") {
                    Ok(())
                } else {
                    Err(format!(
                        "Anthropic provider not configured. Expected a Claude model ID (e.g., 'claude-opus-4-6'), got '{}'",
                        model_override
                    ))
                }
            }
        }
        "codex" => {
            // Codex expects raw model IDs from OpenAI
            let openai = providers.iter().find(|p| p.id == "openai");
            if let Some(provider) = openai {
                if !provider.models.iter().any(|m| m.id == model_override) {
                    // Check if it looks like an OpenAI/Codex model (common prefixes)
                    if model_override.starts_with("gpt-")
                        || model_override.starts_with("o1-")
                        || model_override.starts_with("codex-")
                    {
                        // Allow unknown OpenAI models (escape hatch for new models)
                        Ok(())
                    } else {
                        Err(format!(
                            "Model '{}' not found in OpenAI catalog. Available models: {}. For custom OpenAI models, use format 'gpt-*', 'o1-*', or 'codex-*'",
                            model_override,
                            provider
                                .models
                                .iter()
                                .map(|m| &m.id)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    }
                } else {
                    Ok(())
                }
            } else {
                // OpenAI not configured, but allow if it looks like an OpenAI/Codex model
                if model_override.starts_with("gpt-")
                    || model_override.starts_with("o1-")
                    || model_override.starts_with("codex-")
                {
                    Ok(())
                } else {
                    Err(format!(
                        "OpenAI provider not configured. Expected an OpenAI model ID (e.g., 'gpt-4', 'o1-*', or 'codex-*'), got '{}'",
                        model_override
                    ))
                }
            }
        }
        _ => {
            // Unknown backend - skip validation
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_id_to_display_name() {
        assert_eq!(model_id_to_display_name("glm-5"), "GLM 5");
        assert_eq!(model_id_to_display_name("grok-4-fast"), "Grok 4 Fast");
        assert_eq!(model_id_to_display_name("gpt-5.3-codex"), "GPT 5.3 Codex");
        assert_eq!(
            model_id_to_display_name("claude-opus-4-6"),
            "Claude Opus 4 6"
        );
        // Acronyms <= 3 chars get uppercased
        assert_eq!(model_id_to_display_name("gpt-4"), "GPT 4");
        assert_eq!(model_id_to_display_name("glm-4.6v-flash"), "GLM 4.6v Flash");
    }

    /// Fetch models from all provider APIs that have credentials available,
    /// then compare against the hardcoded defaults to detect staleness.
    ///
    /// Run with: `cargo test check_hardcoded_model_staleness -- --nocapture --ignored`
    ///
    /// This test is `#[ignore]` by default because it requires network access
    /// and valid API keys. It prints warnings for any mismatches found.
    #[tokio::test]
    #[ignore]
    async fn check_hardcoded_model_staleness() {
        let defaults = default_providers_config();
        let defaults_by_id: HashMap<String, Vec<String>> = defaults
            .providers
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.models.iter().map(|m| m.id.clone()).collect(),
                )
            })
            .collect();

        // Providers we can fetch from (provider_id, base_url, prefix_filters, is_anthropic)
        struct TestTarget {
            provider_id: &'static str,
            provider_type: ProviderType,
            base_url: &'static str,
            prefix_filters: Vec<&'static str>,
            is_anthropic: bool,
        }

        let targets = vec![
            TestTarget {
                provider_id: "anthropic",
                provider_type: ProviderType::Anthropic,
                base_url: "",
                prefix_filters: vec![],
                is_anthropic: true,
            },
            TestTarget {
                provider_id: "openai",
                provider_type: ProviderType::OpenAI,
                base_url: "https://api.openai.com/v1",
                prefix_filters: vec!["gpt-", "o1-", "o3-", "o4-", "chatgpt-"],
                is_anthropic: false,
            },
            TestTarget {
                provider_id: "xai",
                provider_type: ProviderType::Xai,
                base_url: "https://api.x.ai/v1",
                prefix_filters: vec!["grok-"],
                is_anthropic: false,
            },
            TestTarget {
                provider_id: "cerebras",
                provider_type: ProviderType::Cerebras,
                base_url: "https://api.cerebras.ai/v1",
                prefix_filters: vec![],
                is_anthropic: false,
            },
            TestTarget {
                provider_id: "zai",
                provider_type: ProviderType::Zai,
                base_url: "https://open.bigmodel.cn/api/paas/v4",
                prefix_filters: vec!["glm-"],
                is_anthropic: false,
            },
            TestTarget {
                provider_id: "minimax",
                provider_type: ProviderType::Minimax,
                base_url: "https://api.minimax.io/v1",
                prefix_filters: vec!["MiniMax-"],
                is_anthropic: false,
            },
        ];

        let mut any_checked = false;
        let mut any_stale = false;

        for target in &targets {
            let api_key = get_api_key_for_provider(target.provider_type, &[]);
            let api_key = match api_key {
                Some(k) => k,
                None => {
                    eprintln!(
                        "[SKIP] {}: no API key found (set {} or configure in OpenCode auth)",
                        target.provider_id,
                        target.provider_type.env_var_name().unwrap_or("N/A"),
                    );
                    continue;
                }
            };

            any_checked = true;

            let fetched = if target.is_anthropic {
                fetch_anthropic_models(&api_key).await
            } else {
                fetch_openai_compatible_models(target.base_url, &api_key, &target.prefix_filters)
                    .await
            };

            match fetched {
                Ok(models) => {
                    let fetched_ids: HashSet<String> =
                        models.iter().map(|m| m.id.clone()).collect();
                    let hardcoded_ids: HashSet<String> = defaults_by_id
                        .get(target.provider_id)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect();

                    // Models in API but not in hardcoded list (new models)
                    let new_models: Vec<&String> = fetched_ids.difference(&hardcoded_ids).collect();
                    // Models in hardcoded list but not in API (possibly removed/renamed)
                    let removed_models: Vec<&String> =
                        hardcoded_ids.difference(&fetched_ids).collect();

                    if !new_models.is_empty() || !removed_models.is_empty() {
                        any_stale = true;
                    }

                    eprintln!("\n=== {} ===", target.provider_id);
                    eprintln!(
                        "  API returned {} models, hardcoded has {}",
                        fetched_ids.len(),
                        hardcoded_ids.len()
                    );

                    if new_models.is_empty() && removed_models.is_empty() {
                        eprintln!("  [OK] Hardcoded list is up to date");
                    }

                    if !new_models.is_empty() {
                        eprintln!(
                            "  [WARN] {} NEW models not in hardcoded list:",
                            new_models.len()
                        );
                        let mut sorted = new_models;
                        sorted.sort();
                        for id in &sorted {
                            eprintln!("    + {}", id);
                        }

                        eprintln!("\n  Suggested additions to default_providers_config():");
                        let mut new_sorted: Vec<_> = models
                            .iter()
                            .filter(|m| !hardcoded_ids.contains(&m.id))
                            .collect();
                        new_sorted.sort_by(|a, b| a.id.cmp(&b.id));
                        for model in new_sorted {
                            eprintln!("    ProviderModel {{");
                            eprintln!("        id: \"{}\".to_string(),", model.id);
                            eprintln!("        name: \"{}\".to_string(),", model.name);
                            eprintln!("        description: None,");
                            eprintln!("    }},");
                        }
                    }

                    if !removed_models.is_empty() {
                        eprintln!(
                            "  [WARN] {} hardcoded models NOT found in API (possibly removed/renamed):",
                            removed_models.len()
                        );
                        let mut sorted: Vec<_> = removed_models;
                        sorted.sort();
                        for id in sorted {
                            eprintln!("    - {}", id);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[ERROR] {}: failed to fetch: {}", target.provider_id, e);
                }
            }
        }

        if !any_checked {
            eprintln!("\n[INFO] No API keys were available. Set environment variables to check staleness:");
            eprintln!(
                "  ANTHROPIC_API_KEY, OPENAI_API_KEY, XAI_API_KEY, CEREBRAS_API_KEY, ZHIPU_API_KEY"
            );
        }

        if any_stale {
            eprintln!(
                "\n[WARN] Hardcoded model catalog is STALE — update default_providers_config()"
            );
            eprintln!(
                "  (This is a warning, not a failure. Dynamic fetching covers the gap at runtime.)"
            );
        }
    }
}
