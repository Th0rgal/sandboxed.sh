//! Provider catalog API.
//!
//! Provides endpoints for listing available providers and their models for UI selection.
//! Only returns providers that are actually configured and authenticated.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::routes::AppState;
use crate::ai_providers::{AIProvider, AIProviderStore, ProviderType};
use crate::util::{auth_entry_has_credentials, home_dir, AI_PROVIDERS_PATH};

/// Cached model catalog fetched from provider APIs and public catalogs at startup.
/// Maps provider ID (e.g. "anthropic") -> Vec<CatalogEntry>.
pub type ModelCatalog = Arc<RwLock<HashMap<String, Vec<CatalogEntry>>>>;

#[derive(Debug, Clone)]
struct CodexProbeCacheEntry {
    visible_models: HashSet<String>,
    expires_at: Instant,
}

static CODEX_MODEL_PROBE_CACHE: OnceLock<RwLock<HashMap<String, CodexProbeCacheEntry>>> =
    OnceLock::new();

const CODEX_MODEL_PROBE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
struct KimiModelCacheEntry {
    models: Vec<ProviderModel>,
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct KimiModelCache {
    entries: HashMap<u64, KimiModelCacheEntry>,
    generations: HashMap<u64, u64>,
}

static KIMI_MODEL_CACHE: OnceLock<RwLock<KimiModelCache>> = OnceLock::new();
static KIMI_CATALOG_GENERATION: AtomicU64 = AtomicU64::new(0);
const KIMI_MODEL_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const RETIRED_KIMI_MODEL_IDS: &[&str] = &["kimi-k2.6", "kimi-k2-thinking"];

/// Provider IDs that are part of the default catalog and should not be duplicated
/// from the AIProviderStore.
pub const DEFAULT_CATALOG_PROVIDER_IDS: &[&str] = &[
    "anthropic",
    "openai",
    "open-router",
    "google",
    "xai",
    "cerebras",
    "zai",
    "minimax",
    "kimi",
];

/// Upper bound on models merged for a single provider from live `/v1/models`
/// and from models.dev. OpenRouter has no usable prefix filter and exposes a
/// large, fast-growing catalog; without a bound it can flood the routing
/// picker (and the cached catalog). Prefix-filtered providers stay well under
/// this and are left uncapped.
const MAX_CATALOG_MODELS_PER_PROVIDER: usize = 100;

const OPENROUTER_PROVIDER_ID: &str = "open-router";

/// Best-effort seed slugs kept in the default config; prioritized when capping
/// the models.dev OpenRouter catalog (which has no popularity sort).
const OPENROUTER_SEED_MODEL_IDS: &[&str] = &[
    "anthropic/claude-opus-5",
    "anthropic/claude-sonnet-4.6",
    "google/gemini-3.1-pro-preview",
    "openai/gpt-5.6",
    "openai/gpt-5.6-sol",
    "openai/gpt-5.5",
    "meta-llama/llama-3.3-70b-instruct:free",
];

/// Text/code model IDs accepted by the native Grok CLI backend for the current
/// OAuth-based Grok Build path. This is intentionally narrower than xAI's
/// OpenAI-compatible `/v1/models` catalog: API-routable models such as
/// rolling aliases can still appear for the custom router, but the `grok`
/// backend only offers canonical IDs documented for Grok Build. Actual access
/// remains account/region-dependent and is diagnosed by the CLI at runtime.
const GROK_CLI_TEXT_MODEL_IDS: &[&str] = &[
    "grok-4.5",
    "grok-build-0.1",
    "grok-4.3",
    "grok-4.20-0309-reasoning",
    "grok-4.20-0309-non-reasoning",
    "grok-4.20-multi-agent-0309",
];

/// Maximum length (in `char`s) of a model description surfaced from a
/// `/v1/models` entry. OpenRouter descriptions can run several paragraphs; the
/// picker only needs a short blurb.
const MAX_MODEL_DESCRIPTION_CHARS: usize = 200;

/// Where a catalog entry came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    ProviderApi,
    ModelsDev,
    Docs,
    HardcodedFallback,
    SmokeTest,
}

/// How confidently a model can be selected for the configured account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogAvailability {
    /// Returned by the provider API or otherwise verified for this account.
    Available,
    /// Known from a public catalog, not confirmed for this account.
    Known,
    /// Discovered from docs or other weak signals and needs validation.
    Candidate,
    /// Previously tested and failed.
    Failed,
}

/// Internal merged catalog entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub sources: Vec<CatalogSource>,
    pub availability: CatalogAvailability,
    pub last_checked_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CatalogEntry {
    fn from_provider_model(
        provider_id: impl Into<String>,
        model: ProviderModel,
        source: CatalogSource,
        availability: CatalogAvailability,
        last_checked_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            id: model.id,
            name: model.name,
            provider_id: provider_id.into(),
            sources: vec![source],
            availability,
            last_checked_at,
            description: model.description,
        }
    }

    fn to_provider_model(&self) -> ProviderModel {
        ProviderModel {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
        }
    }

    fn is_selectable_by_default(&self) -> bool {
        self.availability == CatalogAvailability::Available
    }
}

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

/// Models documented for Kimi Code and returned by its coding `/v1/models`
/// endpoint as of July 2026. These are only a resilient startup fallback:
/// connected accounts are refreshed from the live endpoint, so future Kimi
/// models become selectable without a Sandboxed.sh release.
pub(crate) fn kimi_fallback_models() -> Vec<ProviderModel> {
    vec![
        ProviderModel {
            id: "kimi-for-coding".to_string(),
            name: "K2.7 Coding".to_string(),
            description: Some("Stable Kimi K2.7 coding alias".to_string()),
        },
        ProviderModel {
            id: "kimi-for-coding-highspeed".to_string(),
            name: "K2.7 Coding Highspeed".to_string(),
            description: Some("High-speed Kimi K2.7 coding alias".to_string()),
        },
        ProviderModel {
            id: "k3".to_string(),
            name: "K3".to_string(),
            description: Some("Kimi K3 with up to a 1M-token context window".to_string()),
        },
        ProviderModel {
            id: "k3-256k".to_string(),
            name: "K3-256K".to_string(),
            description: Some("Kimi K3 with a 256K-token context window".to_string()),
        },
    ]
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
    /// Include public-catalog models that have not been verified for this account.
    #[serde(default)]
    pub include_unverified: bool,
}

/// Response for the providers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersResponse {
    pub providers: Vec<Provider>,
    /// Provider ids that currently have working credentials for this account.
    /// When `include_all` is set the `providers` list also contains
    /// unconfigured catalog providers (so chains can be pre-built); clients use
    /// this set to mark which are actually connected.
    #[serde(default)]
    pub configured_ids: Vec<String>,
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

/// One model in the full supported-model catalog, ready to drop into a
/// model-routing chain (`value` is the provider-prefixed id, e.g. `xai/grok-4.5`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogModelOption {
    pub provider_id: String,
    pub provider_name: String,
    /// Bare model id (e.g. `grok-4.5`).
    pub id: String,
    /// Chain-ready value (`provider_id/model_id`).
    pub value: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this provider is currently configured/authenticated for this account.
    pub configured: bool,
}

pub(crate) async fn catalog_model_options_for_state(
    state: &AppState,
    include_unverified: bool,
    configured_only: bool,
) -> Vec<CatalogModelOption> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    let cached = state.model_catalog.read().await.clone();
    merge_cached_provider_models(&mut config, &cached, include_unverified);

    let mut configured = get_configured_provider_ids(state.config.working_dir.as_path());
    let store_providers = state.ai_providers.list().await;
    for provider in &store_providers {
        if !provider.enabled || !provider.has_credentials() {
            continue;
        }
        let id = if provider.provider_type == ProviderType::Custom {
            sanitize_custom_provider_id(&provider.name)
        } else {
            provider.provider_type.id().to_string()
        };
        configured.insert(id);
    }

    let mut providers = if configured_only {
        config
            .providers
            .into_iter()
            .filter(|provider| configured.contains(&provider.id))
            .collect()
    } else {
        config.providers
    };
    merge_store_provider_models(&mut providers, &store_providers, !configured_only);
    apply_live_authoritative_provider_models(
        &mut providers,
        &store_providers,
        &cached,
        include_unverified,
    );
    drop(cached);

    let mut models = Vec::new();
    for provider in &providers {
        let is_configured = configured.contains(&provider.id);
        if configured_only && !is_configured {
            continue;
        }
        for model in &provider.models {
            models.push(CatalogModelOption {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                id: model.id.clone(),
                value: format!("{}/{}", provider.id, model.id),
                name: model.name.clone(),
                description: model.description.clone(),
                configured: is_configured,
            });
        }
    }

    models.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then_with(|| a.name.cmp(&b.name))
    });
    models
}

/// Response for the full model catalog endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullCatalogResponse {
    pub count: usize,
    pub models: Vec<CatalogModelOption>,
}

/// Query parameters for backend models endpoint.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BackendModelsQuery {
    /// Include providers even if they are not configured/authenticated.
    #[serde(default)]
    pub include_all: bool,
    /// Include public-catalog models that have not been verified for this account.
    #[serde(default)]
    pub include_unverified: bool,
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
            Ok(mut config) => {
                merge_default_provider_models(&mut config);
                config
            }
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

fn merge_default_provider_models(config: &mut ProvidersConfig) {
    let defaults = default_providers_config();
    for default_provider in defaults.providers {
        if let Some(existing) = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == default_provider.id)
        {
            // Old generated/provider configuration survives binary upgrades.
            // Do not let Kimi's retired hardcoded entries remain selectable
            // forever merely because defaults use additive merge semantics.
            if existing.id == "kimi" {
                existing
                    .models
                    .retain(|model| !RETIRED_KIMI_MODEL_IDS.contains(&model.id.as_str()));
            }
            merge_provider_models(&mut existing.models, default_provider.models);
            continue;
        }

        config.providers.push(default_provider);
    }
}

fn merge_provider_models(
    models: &mut Vec<ProviderModel>,
    incoming: impl IntoIterator<Item = ProviderModel>,
) {
    let mut seen: HashSet<String> = models.iter().map(|model| model.id.clone()).collect();
    for model in incoming {
        if seen.insert(model.id.clone()) {
            models.push(model);
        }
    }
}

fn normalize_model_id(id: &str) -> String {
    id.trim().to_ascii_lowercase()
}

fn models_dev_provider_key(provider_id: &str) -> &str {
    match provider_id {
        // Sandboxed/OpenCode use `open-router`; models.dev uses `openrouter`.
        "open-router" => "openrouter",
        _ => provider_id,
    }
}

/// Cap a catalog slice, keeping `priority_ids` first (in that order) then filling
/// from the remainder in stable id order until `limit`.
fn cap_catalog_entries(
    mut entries: Vec<CatalogEntry>,
    limit: usize,
    priority_ids: &[&str],
) -> Vec<CatalogEntry> {
    if entries.len() <= limit {
        return entries;
    }

    let priority_normalized: HashSet<String> = priority_ids
        .iter()
        .map(|id| normalize_model_id(id))
        .collect();
    let mut priority_entries = Vec::new();
    let mut rest = Vec::new();
    for entry in entries.drain(..) {
        if priority_normalized.contains(&normalize_model_id(&entry.id)) {
            priority_entries.push(entry);
        } else {
            rest.push(entry);
        }
    }
    priority_entries.sort_by_key(|entry| {
        priority_ids
            .iter()
            .position(|id| normalize_model_id(id) == normalize_model_id(&entry.id))
            .unwrap_or(usize::MAX)
    });
    rest.sort_by(|a, b| a.id.cmp(&b.id));

    let mut capped = priority_entries;
    for entry in rest {
        if capped.len() >= limit {
            break;
        }
        capped.push(entry);
    }
    capped.truncate(limit);
    capped
}

fn merge_catalog_entries(
    catalog: &mut HashMap<String, Vec<CatalogEntry>>,
    provider_id: &str,
    incoming: impl IntoIterator<Item = CatalogEntry>,
) {
    let entries = catalog.entry(provider_id.to_string()).or_default();
    for entry in incoming {
        let normalized = normalize_model_id(&entry.id);
        if let Some(existing) = entries
            .iter_mut()
            .find(|model| normalize_model_id(&model.id) == normalized)
        {
            if existing.availability != CatalogAvailability::Available
                && entry.availability == CatalogAvailability::Available
            {
                existing.availability = CatalogAvailability::Available;
                existing.last_checked_at = entry.last_checked_at;
            }
            if existing.description.is_none() {
                existing.description = entry.description.clone();
            }
            if existing.name == existing.id && entry.name != entry.id {
                existing.name = entry.name.clone();
            }
            for source in entry.sources {
                if !existing.sources.contains(&source) {
                    existing.sources.push(source);
                }
            }
            continue;
        }

        entries.push(entry);
    }
}

fn merge_cached_provider_models(
    config: &mut ProvidersConfig,
    cached: &HashMap<String, Vec<CatalogEntry>>,
    include_unverified: bool,
) {
    for provider in &mut config.providers {
        if let Some(entries) = cached.get(&provider.id) {
            let models = entries
                .iter()
                .filter(|entry| include_unverified || entry.is_selectable_by_default())
                .map(CatalogEntry::to_provider_model);
            merge_provider_models(&mut provider.models, models);
        }
    }
}

pub(crate) fn sanitize_custom_provider_id(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
        .replace('-', "_")
}

fn store_custom_models(provider: &AIProvider) -> Vec<ProviderModel> {
    provider
        .custom_models
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|model| ProviderModel {
            id: model.id,
            name: model.name.unwrap_or_else(|| "Custom model".to_string()),
            description: None,
        })
        .collect()
}

fn merge_store_provider_models(
    providers: &mut Vec<Provider>,
    store_providers: &[AIProvider],
    include_all: bool,
) {
    for provider in store_providers {
        if !provider.enabled {
            continue;
        }
        if !include_all && !provider.has_credentials() {
            continue;
        }

        let store_models = store_custom_models(provider);
        if store_models.is_empty() {
            continue;
        }

        let id = if provider.provider_type == ProviderType::Custom {
            sanitize_custom_provider_id(&provider.name)
        } else {
            provider.provider_type.id().to_string()
        };

        if let Some(existing) = providers.iter_mut().find(|p| p.id == id) {
            let mut seen: HashSet<String> = existing.models.iter().map(|m| m.id.clone()).collect();
            for model in store_models {
                if seen.insert(model.id.clone()) {
                    existing.models.push(model);
                }
            }
            continue;
        }

        providers.push(Provider {
            id,
            name: provider.name.clone(),
            billing: "custom".to_string(),
            description: "Custom provider".to_string(),
            models: store_models,
        });
    }
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
                    // Check Anthropic's current model IDs here:
                    // https://platform.claude.com/docs/en/about-claude/models/overview
                    ProviderModel {
                        id: "claude-opus-5".to_string(),
                        name: "Claude Opus 5".to_string(),
                        description: Some(
                            "Default for complex agentic coding, adaptive thinking, 1M context"
                                .to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-fable-5".to_string(),
                        name: "Claude Fable 5".to_string(),
                        description: Some(
                            "Most capable widely released model, adaptive thinking, 1M context"
                                .to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-opus-4-8".to_string(),
                        name: "Claude Opus 4.8".to_string(),
                        description: Some(
                            "Previous-generation Opus model retained for explicit compatibility"
                                .to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-opus-4-7".to_string(),
                        name: "Claude Opus 4.7".to_string(),
                        description: Some(
                            "Most capable, recommended for complex tasks".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-opus-4-6".to_string(),
                        name: "Claude Opus 4.6".to_string(),
                        description: Some(
                            "Most capable, recommended for complex tasks".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "claude-sonnet-4-6".to_string(),
                        name: "Claude Sonnet 4.6".to_string(),
                        description: Some(
                            "Latest Sonnet, balanced speed and capability".to_string(),
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
                    // Only current models. OpenAI's ChatGPT-account Codex keeps
                    // a small recent set, so the older codex variants (gpt-5-codex
                    // … gpt-5.3-codex) and stale generics (gpt-5.1/5.2/5.3) were
                    // removed — gpt-5.3-codex now 404s ("model not supported when
                    // using Codex with a ChatGPT account"), and everything older
                    // than it is dead too. Newest first.
                    ProviderModel {
                        id: "gpt-5.6".to_string(),
                        name: "GPT-5.6".to_string(),
                        description: Some(
                            "Alias for GPT-5.6 Sol, OpenAI's flagship reasoning and coding model"
                                .to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gpt-5.6-sol".to_string(),
                        name: "GPT-5.6 Sol".to_string(),
                        description: Some(
                            "Flagship GPT-5.6 model for complex reasoning and coding".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gpt-5.6-terra".to_string(),
                        name: "GPT-5.6 Terra".to_string(),
                        description: Some(
                            "Preview GPT-5.6 model with lower cost than Sol".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gpt-5.6-luna".to_string(),
                        name: "GPT-5.6 Luna".to_string(),
                        description: Some(
                            "Preview GPT-5.6 model optimized for speed and cost".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gpt-5.5".to_string(),
                        name: "GPT-5.5".to_string(),
                        description: Some(
                            "Latest frontier coding model in Codex (Spud, 2026-04)".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "gpt-5.5-pro".to_string(),
                        name: "GPT-5.5 Pro".to_string(),
                        description: Some("Highest-capability GPT-5.5 model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.5-codex".to_string(),
                        name: "GPT-5.5 Codex".to_string(),
                        description: Some("Latest Codex-optimized coding model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.4".to_string(),
                        name: "GPT-5.4".to_string(),
                        description: Some("Previous frontier coding model in Codex".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.4-pro".to_string(),
                        name: "GPT-5.4 Pro".to_string(),
                        description: Some("Highest-capability GPT-5.4 model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.4-mini".to_string(),
                        name: "GPT-5.4 Mini".to_string(),
                        description: Some("Smaller, lower-latency GPT-5.4 model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.4-nano".to_string(),
                        name: "GPT-5.4 Nano".to_string(),
                        description: Some("Smallest, lowest-cost GPT-5.4 model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-5.3-codex".to_string(),
                        name: "GPT-5.3 Codex".to_string(),
                        description: Some("Codex-specialized model".to_string()),
                    },
                ],
            },
            Provider {
                id: "open-router".to_string(),
                name: "OpenRouter (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "Aggregator routing through OpenRouter".to_string(),
                models: vec![
                    // OpenRouter model IDs are vendor-prefixed (provider/model).
                    // The live catalog is merged from models.dev and /v1/models
                    // when available; these keep the picker useful before the
                    // background catalog finishes. IDs verified against
                    // OpenRouter's public catalog on 2026-07-25 — treat as
                    // best-effort seeds that the live catalog supersedes (slugs
                    // can drift as models are retired).
                    ProviderModel {
                        id: "anthropic/claude-opus-5".to_string(),
                        name: "Claude Opus 5".to_string(),
                        description: Some("Anthropic Claude via OpenRouter".to_string()),
                    },
                    ProviderModel {
                        id: "anthropic/claude-sonnet-4.6".to_string(),
                        name: "Claude Sonnet 4.6".to_string(),
                        description: Some("Anthropic Claude via OpenRouter".to_string()),
                    },
                    ProviderModel {
                        id: "google/gemini-3.1-pro-preview".to_string(),
                        name: "Gemini 3.1 Pro Preview".to_string(),
                        description: Some("Google Gemini via OpenRouter".to_string()),
                    },
                    ProviderModel {
                        id: "openai/gpt-5.6".to_string(),
                        name: "GPT-5.6".to_string(),
                        description: Some("OpenAI GPT via OpenRouter".to_string()),
                    },
                    ProviderModel {
                        id: "meta-llama/llama-3.3-70b-instruct:free".to_string(),
                        name: "Llama 3.3 70B Instruct (free)".to_string(),
                        description: Some("Meta Llama via OpenRouter".to_string()),
                    },
                ],
            },
            Provider {
                id: "google".to_string(),
                name: "Google AI (OAuth)".to_string(),
                billing: "subscription".to_string(),
                description: "Gemini models via Google OAuth".to_string(),
                models: vec![
                    // Check Gemini model IDs here:
                    // https://ai.google.dev/gemini-api/docs/models
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
                        id: "grok-4.5".to_string(),
                        name: "Grok 4.5".to_string(),
                        description: Some("Latest flagship Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-4.5-latest".to_string(),
                        name: "Grok 4.5 (Latest)".to_string(),
                        description: Some("Rolling alias for Grok 4.5".to_string()),
                    },
                    ProviderModel {
                        id: "grok-build-latest".to_string(),
                        name: "Grok Build (Latest)".to_string(),
                        description: Some(
                            "Rolling Grok Build alias backed by Grok 4.5".to_string(),
                        ),
                    },
                    // Legacy IDs retained for accounts that still advertise them.
                    // Native Grok CLI choices are filtered separately below.
                    ProviderModel {
                        id: "grok-build-0.1".to_string(),
                        name: "Grok Build".to_string(),
                        description: Some(
                            "Grok Build coding model (xAI's \"Composer\"-class agent model; \
                             marketing name \"Composer 2.5\" is NOT a valid API id)"
                                .to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "grok-4.3".to_string(),
                        name: "Grok 4.3".to_string(),
                        description: Some("Previous flagship Grok model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-4.20-0309-reasoning".to_string(),
                        name: "Grok 4.20 (Reasoning)".to_string(),
                        description: Some("Grok 4.20 reasoning model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-4.20-0309-non-reasoning".to_string(),
                        name: "Grok 4.20 (Non-Reasoning)".to_string(),
                        description: Some("Grok 4.20 non-reasoning model".to_string()),
                    },
                    ProviderModel {
                        id: "grok-4.20-multi-agent-0309".to_string(),
                        name: "Grok 4.20 Multi-Agent".to_string(),
                        description: Some("Grok 4.20 multi-agent model".to_string()),
                    },
                ],
            },
            Provider {
                id: "cerebras".to_string(),
                name: "Cerebras (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "Ultra-fast inference via Cerebras".to_string(),
                models: vec![
                    // Keep aligned with the live catalog: GET /v1/models only
                    // serves these two IDs now (Qwen and the `-cs` aliases are
                    // retired).
                    ProviderModel {
                        id: "zai-glm-4.7".to_string(),
                        name: "GLM-4.7 (Cerebras)".to_string(),
                        description: Some("Most capable Cerebras-hosted model".to_string()),
                    },
                    ProviderModel {
                        id: "gpt-oss-120b".to_string(),
                        name: "GPT-OSS 120B".to_string(),
                        description: Some("Open-weight reasoning model, ultra-fast".to_string()),
                    },
                ],
            },
            Provider {
                id: "zai".to_string(),
                name: "Z.AI (API Key)".to_string(),
                billing: "pay-per-token".to_string(),
                description: "GLM models via Z.AI API key".to_string(),
                models: vec![
                    // Check Z.AI / GLM model IDs here:
                    // https://docs.z.ai/guides/llm/glm
                    ProviderModel {
                        id: "glm-5.2".to_string(),
                        name: "GLM-5.2".to_string(),
                        description: Some("Most capable GLM reasoning model".to_string()),
                    },
                    ProviderModel {
                        id: "glm-5.1".to_string(),
                        name: "GLM-5.1".to_string(),
                        description: Some("Previous flagship GLM reasoning model".to_string()),
                    },
                    ProviderModel {
                        id: "glm-5-turbo".to_string(),
                        name: "GLM-5 Turbo".to_string(),
                        description: Some("Fast reasoning model with deep thinking".to_string()),
                    },
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
                    // Check MiniMax text model IDs here:
                    // https://platform.minimaxi.com/document/ChatCompletion%20v2
                    ProviderModel {
                        id: "MiniMax-M3".to_string(),
                        name: "MiniMax M3".to_string(),
                        description: Some(
                            "Latest MiniMax coding and agentic model with 1M context".to_string(),
                        ),
                    },
                    ProviderModel {
                        id: "MiniMax-M2.7".to_string(),
                        name: "MiniMax M2.7".to_string(),
                        description: Some("Previous flagship MiniMax model".to_string()),
                    },
                    ProviderModel {
                        id: "MiniMax-M2.5".to_string(),
                        name: "MiniMax M2.5".to_string(),
                        description: Some("Previous flagship MiniMax model".to_string()),
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
            Provider {
                id: "kimi".to_string(),
                name: "Kimi (Subscription)".to_string(),
                billing: "subscription".to_string(),
                description: "Kimi Code via Moonshot OAuth (device login)".to_string(),
                models: kimi_fallback_models(),
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

/// Trim a model description to a sane length for the routing picker.
/// OpenRouter (and some other catalogs) return multi-paragraph descriptions;
/// keep a short blurb and append an ellipsis when truncated. Operates on
/// `char` boundaries so multibyte text isn't split mid-codepoint.
fn truncate_description(description: &str) -> String {
    let trimmed = description.trim();
    if trimmed.chars().count() <= MAX_MODEL_DESCRIPTION_CHARS {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(MAX_MODEL_DESCRIPTION_CHARS).collect();
    format!("{}…", truncated.trim_end())
}

/// Fetch models from an OpenAI-compatible /v1/models endpoint.
/// Filters results by the given prefix (e.g. "grok-", "glm-").
/// Returns model IDs and generated display names.
///
/// `models_query` is appended after `/models` (e.g. `?sort=most-popular` for
/// OpenRouter). When `sort_results_by_id` is false the API response order is
/// preserved (used for server-side popularity sorts).
pub async fn fetch_openai_compatible_models(
    base_url: &str,
    api_key: &str,
    prefix_filters: &[&str],
    models_query: Option<&str>,
    sort_results_by_id: bool,
) -> Result<Vec<ProviderModel>, String> {
    fetch_openai_compatible_models_with_headers(
        base_url,
        api_key,
        prefix_filters,
        models_query,
        sort_results_by_id,
        &[],
    )
    .await
}

async fn fetch_openai_compatible_models_with_headers(
    base_url: &str,
    api_key: &str,
    prefix_filters: &[&str],
    models_query: Option<&str>,
    sort_results_by_id: bool,
    extra_headers: &[(&str, &str)],
) -> Result<Vec<ProviderModel>, String> {
    let client = reqwest::Client::new();
    let base = base_url.trim_end_matches('/');
    let url = match models_query.filter(|query| !query.is_empty()) {
        Some(query) => format!("{base}/models{query}"),
        None => format!("{base}/models"),
    };

    let mut request = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10));
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    let resp = request
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

    parse_openai_compatible_models(body, prefix_filters, sort_results_by_id)
}

fn parse_openai_compatible_models(
    body: serde_json::Value,
    prefix_filters: &[&str],
    sort_results_by_id: bool,
) -> Result<Vec<ProviderModel>, String> {
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
            let name = entry
                .get("display_name")
                .or_else(|| entry.get("name"))
                .and_then(|n| n.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| model_id_to_display_name(id));
            let description = entry
                .get("description")
                .and_then(|d| d.as_str())
                .map(truncate_description)
                .filter(|d| !d.is_empty());
            Some(ProviderModel {
                id: id.to_string(),
                name,
                description,
            })
        })
        .collect();

    if sort_results_by_id {
        models.sort_by(|a, b| a.id.cmp(&b.id));
    }
    Ok(models)
}

fn kimi_model_cache() -> &'static RwLock<KimiModelCache> {
    KIMI_MODEL_CACHE.get_or_init(|| RwLock::new(KimiModelCache::default()))
}

fn kimi_route_fingerprint(
    provider: &crate::ai_providers::AIProvider,
) -> Result<(u64, &str, &str), String> {
    if provider.provider_type != ProviderType::Kimi {
        return Err("Cannot fetch Kimi models for a non-Kimi provider".to_string());
    }
    let access_token = provider
        .oauth
        .as_ref()
        .map(|oauth| oauth.access_token.trim())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Kimi OAuth access token is missing".to_string())?;
    let base_url = provider
        .base_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or(crate::api::ai_providers::KIMI_API_BASE_URL);
    Ok((
        hash_u64(&format!("{}\0{base_url}", provider.id)),
        base_url,
        access_token,
    ))
}

/// Return the last account-specific Kimi catalog without performing I/O.
///
/// Workspace preparation is latency-sensitive and must not wait on a provider
/// outage. The background model-catalog refresher owns live discovery; callers
/// on the workspace path use this snapshot or fall back immediately.
pub(crate) async fn cached_kimi_models(
    provider: &crate::ai_providers::AIProvider,
) -> Option<Vec<ProviderModel>> {
    let (route_fingerprint, _, _) = kimi_route_fingerprint(provider).ok()?;
    kimi_model_cache()
        .read()
        .await
        .entries
        .get(&route_fingerprint)
        .map(|entry| entry.models.clone())
}

/// Drop the cached catalog before replacing a Kimi provider's credentials.
///
/// Reconnect keeps the local provider UUID but may select a different upstream
/// account with different model entitlements.
pub(crate) async fn invalidate_kimi_model_cache(provider: &crate::ai_providers::AIProvider) {
    if let Ok((route_fingerprint, _, _)) = kimi_route_fingerprint(provider) {
        let mut cache = kimi_model_cache().write().await;
        cache.entries.remove(&route_fingerprint);
        let generation = cache.generations.entry(route_fingerprint).or_default();
        *generation = generation.wrapping_add(1);
    }
}

pub(crate) fn advance_kimi_catalog_generation() {
    KIMI_CATALOG_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn kimi_catalog_generation() -> u64 {
    KIMI_CATALOG_GENERATION.load(Ordering::Acquire)
}

/// Re-populate Kimi's shared catalog after a reconnect without blocking the
/// OAuth callback. The caller removes the old entry before spawning this task.
pub(crate) async fn refresh_connected_kimi_catalog(catalog: ModelCatalog, provider: AIProvider) {
    let catalog_generation = kimi_catalog_generation();
    match fetch_kimi_models(&provider).await {
        Ok(models) => {
            let checked_at = chrono::Utc::now();
            let entries = models
                .into_iter()
                .map(|model| {
                    CatalogEntry::from_provider_model(
                        "kimi",
                        model,
                        CatalogSource::ProviderApi,
                        CatalogAvailability::Available,
                        checked_at,
                    )
                })
                .collect();
            let cache = kimi_model_cache().read().await;
            if kimi_catalog_generation() != catalog_generation {
                tracing::debug!(
                    provider_id = %provider.id,
                    "Discarded Kimi catalog from an obsolete preference generation"
                );
                return;
            }
            // Keep the generation read-lock until the catalog write commits.
            // Preference reconciliation advances cache generations before it
            // takes the catalog write-lock, so the final state cannot be an
            // obsolete account snapshot.
            catalog.write().await.insert("kimi".to_string(), entries);
            drop(cache);
        }
        Err(error) => {
            tracing::warn!(
                provider_id = %provider.id,
                error = %error,
                "Failed to refresh Kimi catalog after account reconnect"
            );
        }
    }
}

pub(crate) fn preferred_usable_kimi_provider(providers: &[AIProvider]) -> Option<AIProvider> {
    providers
        .iter()
        .filter(|provider| {
            provider.provider_type == ProviderType::Kimi
                && provider.enabled
                && provider.oauth.as_ref().is_some_and(|oauth| {
                    !oauth.access_token.trim().is_empty() && !oauth.refresh_token.trim().is_empty()
                })
        })
        .min_by_key(|provider| (provider.priority, provider.id))
        .cloned()
}

/// Discover the models available to a connected Kimi Code account.
///
/// Kimi is an OpenAI-compatible provider but requires its coding-agent
/// User-Agent on catalog requests. Cache successful responses briefly so
/// preparing many mission workspaces does not hit the subscription endpoint
/// once per mission. The cache key includes the stable provider identity and
/// route, so normal OAuth token rotation does not hide a catalog that is still
/// valid for that account.
pub(crate) async fn fetch_kimi_models(
    provider: &crate::ai_providers::AIProvider,
) -> Result<Vec<ProviderModel>, String> {
    let (route_fingerprint, base_url, access_token) = kimi_route_fingerprint(provider)?;

    let (stale_models, probe_generation) = {
        let cache = kimi_model_cache().read().await;
        if let Some(entry) = cache.entries.get(&route_fingerprint) {
            if entry.expires_at > Instant::now() {
                return Ok(entry.models.clone());
            }
            (
                Some(entry.models.clone()),
                cache
                    .generations
                    .get(&route_fingerprint)
                    .copied()
                    .unwrap_or_default(),
            )
        } else {
            (
                None,
                cache
                    .generations
                    .get(&route_fingerprint)
                    .copied()
                    .unwrap_or_default(),
            )
        }
    };

    let discovered = fetch_openai_compatible_models_with_headers(
        base_url,
        access_token,
        &[],
        None,
        true,
        &[("User-Agent", crate::api::ai_providers::KIMI_USER_AGENT)],
    )
    .await;
    let models = match discovered {
        Ok(models) if !models.is_empty() => models,
        Ok(_) => {
            if let Some(models) = stale_models {
                tracing::warn!("Kimi model catalog was empty; reusing stale catalog");
                store_kimi_models_if_current(
                    route_fingerprint,
                    probe_generation,
                    models.clone(),
                    Duration::from_secs(60),
                )
                .await?;
                return Ok(models);
            }
            return Err("Kimi model catalog was empty".to_string());
        }
        Err(error) => {
            if let Some(models) = stale_models {
                tracing::warn!(
                    error = %error,
                    "Kimi model discovery failed; reusing stale catalog"
                );
                store_kimi_models_if_current(
                    route_fingerprint,
                    probe_generation,
                    models.clone(),
                    Duration::from_secs(60),
                )
                .await?;
                return Ok(models);
            }
            return Err(error);
        }
    };

    store_kimi_models_if_current(
        route_fingerprint,
        probe_generation,
        models.clone(),
        KIMI_MODEL_CACHE_TTL,
    )
    .await?;
    Ok(models)
}

async fn store_kimi_models_if_current(
    route_fingerprint: u64,
    probe_generation: u64,
    models: Vec<ProviderModel>,
    ttl: Duration,
) -> Result<(), String> {
    let mut cache = kimi_model_cache().write().await;
    let current_generation = cache
        .generations
        .get(&route_fingerprint)
        .copied()
        .unwrap_or_default();
    if current_generation != probe_generation {
        return Err("Kimi account changed while its model catalog was being fetched".to_string());
    }
    cache.entries.insert(
        route_fingerprint,
        KimiModelCacheEntry {
            models,
            expires_at: Instant::now() + ttl,
        },
    );
    Ok(())
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

/// Fetch public model metadata from models.dev, which is the catalog used by
/// OpenCode. These entries are useful for discovery, but are not account-
/// verified, so they are marked as `known` rather than selectable by default.
pub async fn fetch_models_dev_catalog() -> Result<HashMap<String, Vec<CatalogEntry>>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://models.dev/api.json")
        .header("User-Agent", "Sandboxed.sh model catalog")
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
    let providers = body
        .as_object()
        .ok_or_else(|| "models.dev response was not an object".to_string())?;
    let now = chrono::Utc::now();
    let mut catalog: HashMap<String, Vec<CatalogEntry>> = HashMap::new();

    for provider_id in DEFAULT_CATALOG_PROVIDER_IDS {
        let Some(provider) = providers.get(models_dev_provider_key(provider_id)) else {
            continue;
        };
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };

        let mut entries: Vec<CatalogEntry> = models
            .iter()
            .map(|(model_id, value)| {
                let name = value
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or(model_id)
                    .to_string();
                CatalogEntry {
                    id: model_id.clone(),
                    name,
                    provider_id: (*provider_id).to_string(),
                    sources: vec![CatalogSource::ModelsDev],
                    availability: CatalogAvailability::Known,
                    last_checked_at: now,
                    description: None,
                }
            })
            .collect();

        if *provider_id == OPENROUTER_PROVIDER_ID && entries.len() > MAX_CATALOG_MODELS_PER_PROVIDER
        {
            let before = entries.len();
            entries = cap_catalog_entries(
                entries,
                MAX_CATALOG_MODELS_PER_PROVIDER,
                OPENROUTER_SEED_MODEL_IDS,
            );
            tracing::info!(
                "Capping open-router models.dev catalog from {} to {} models",
                before,
                entries.len()
            );
        }

        merge_catalog_entries(&mut catalog, provider_id, entries);
    }

    Ok(catalog)
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
            // OAuth access tokens can also be used as bearer tokens for some APIs.
            // ChatGPT/Codex and Grok Build OAuth are CLI/subscription credentials;
            // neither is a replacement for an API Platform key on the respective
            // OpenAI-compatible API.
            if !matches!(provider_type, ProviderType::OpenAI | ProviderType::Xai) {
                if let Some(ref oauth) = provider.oauth {
                    if !oauth.access_token.is_empty() {
                        return Some(oauth.access_token.clone());
                    }
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
                        if let Some(api_key) = provider_auth_entry_api_key(provider_type, entry) {
                            return Some(api_key);
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
            if let Some(api_key) = provider_auth_entry_api_key(provider_type, &value) {
                return Some(api_key);
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

fn provider_auth_entry_api_key(
    provider_type: ProviderType,
    entry: &serde_json::Value,
) -> Option<String> {
    let auth_type = entry
        .get("type")
        .or_else(|| entry.get("auth_mode"))
        .and_then(|value| value.as_str());
    if matches!(provider_type, ProviderType::OpenAI | ProviderType::Xai)
        && matches!(auth_type, Some("oauth" | "chatgpt" | "oidc"))
    {
        return None;
    }

    ["key", "api_key", "apiKey"].iter().find_map(|field| {
        entry
            .get(*field)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    })
}

/// Resolve an actual Anthropic API key for `/v1/models`.
///
/// Claude subscription OAuth access tokens are valid for Claude Code's bearer
/// flow but Anthropic's model catalog expects `x-api-key`. Passing an OAuth
/// access token there produces a harmless but noisy 401 every refresh cycle.
/// Store/API-key credentials take precedence; file and environment lookup is
/// retained, while OAuth-only store records are deliberately excluded.
fn get_enabled_store_api_key(
    provider_type: ProviderType,
    ai_providers: &[crate::ai_providers::AIProvider],
) -> Option<String> {
    ai_providers
        .iter()
        .find(|provider| {
            provider.provider_type == provider_type
                && provider.enabled
                && provider
                    .api_key
                    .as_ref()
                    .is_some_and(|key| !key.trim().is_empty())
        })
        .and_then(|provider| provider.api_key.clone())
}

fn get_anthropic_models_api_key(
    ai_providers: &[crate::ai_providers::AIProvider],
) -> Option<String> {
    get_enabled_store_api_key(ProviderType::Anthropic, ai_providers)
        .or_else(|| get_api_key_for_provider(ProviderType::Anthropic, &[]))
}

/// Resolve credentials for an OpenAI-compatible `/v1/models` catalog.
///
/// OpenAI ChatGPT OAuth tokens authenticate the Codex subscription endpoint,
/// not `api.openai.com`. Passing that token to the API Platform model catalog
/// yields a misleading 401 even while Codex is healthy. Only a real API key
/// may be used for OpenAI's Platform catalog.
fn get_openai_compatible_models_api_key(
    provider_type: ProviderType,
    ai_providers: &[crate::ai_providers::AIProvider],
) -> Option<String> {
    if provider_type == ProviderType::OpenAI {
        return get_enabled_store_api_key(provider_type, ai_providers)
            .or_else(|| get_api_key_for_provider(provider_type, &[]));
    }

    get_api_key_for_provider(provider_type, ai_providers)
}

/// Fetch model lists from all supported provider APIs concurrently.
///
/// Returns a map of provider ID -> fetched models. Providers that fail
/// or lack credentials are simply omitted (hardcoded defaults will be used).
/// Fetch the catalog once and replace the cached snapshot, returning
/// `(provider_count, model_count)`.
async fn refresh_catalog_once(
    catalog: &ModelCatalog,
    ai_providers: &AIProviderStore,
    working_dir: &Path,
) -> (usize, usize) {
    let (mut fetched, fetched_kimi_generation) =
        fetch_model_catalog(ai_providers, working_dir).await;
    let mut current = catalog.write().await;
    if kimi_catalog_generation() != fetched_kimi_generation {
        fetched.remove("kimi");
        if let Some(kimi) = current.get("kimi").cloned() {
            fetched.insert("kimi".to_string(), kimi);
        }
    }
    let provider_count = fetched.len();
    let model_count: usize = fetched.values().map(|v| v.len()).sum();
    *current = fetched;
    (provider_count, model_count)
}

/// Spawn the background task that keeps the model catalog populated. Performs an
/// initial fetch immediately, then refreshes on an interval so newly-added
/// provider models (e.g. a custom router exposing a new model via `/v1/models`)
/// appear without a backend restart.
///
/// The interval is controlled by `MODEL_CATALOG_REFRESH_SECS` (default 600s).
/// Set it to `0` to disable periodic refresh and keep the startup-only snapshot.
pub fn spawn_model_catalog_refresh(
    catalog: ModelCatalog,
    ai_providers: Arc<AIProviderStore>,
    working_dir: std::path::PathBuf,
) {
    let refresh_secs = std::env::var("MODEL_CATALOG_REFRESH_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(600);

    tokio::spawn(async move {
        loop {
            let (providers, models) =
                refresh_catalog_once(&catalog, &ai_providers, &working_dir).await;
            tracing::info!(
                "Model catalog populated: {} models from {} providers (refresh every {}s)",
                models,
                providers,
                refresh_secs
            );

            if refresh_secs == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(refresh_secs)).await;
        }
    });
}

#[derive(Debug, Serialize)]
pub struct RefreshCatalogResponse {
    pub refreshed: bool,
    pub providers: usize,
    pub models: usize,
}

/// Force an immediate refetch of the model catalog from all provider APIs and
/// custom routers, replacing the cached snapshot. Useful right after adding a
/// model to a custom provider's `/v1/models` without waiting for the periodic
/// refresh.
pub async fn refresh_model_catalog(
    State(state): State<Arc<AppState>>,
) -> Json<RefreshCatalogResponse> {
    let working_dir = state.config.working_dir.clone();
    let (providers, models) =
        refresh_catalog_once(&state.model_catalog, &state.ai_providers, &working_dir).await;
    tracing::info!(
        "Model catalog manually refreshed: {} models from {} providers",
        models,
        providers
    );
    Json(RefreshCatalogResponse {
        refreshed: true,
        providers,
        models,
    })
}

async fn fetch_model_catalog(
    ai_providers: &AIProviderStore,
    _working_dir: &Path,
) -> (HashMap<String, Vec<CatalogEntry>>, u64) {
    // Kimi access tokens are short-lived. Refresh a due store credential
    // synchronously before taking the provider snapshot so the initial catalog
    // probe cannot race the independent refresh task at startup.
    let (_, refreshed) = crate::api::ai_providers::refresh_due_store_oauth(
        ai_providers,
        ProviderType::Kimi,
        10 * 60 * 1000,
    )
    .await;
    if refreshed > 0 {
        tracing::info!(
            refreshed,
            "Refreshed Kimi OAuth credentials before model catalog discovery"
        );
    }

    let catalog_generation = kimi_catalog_generation();
    let providers_list = ai_providers.list().await;
    let mut result = HashMap::new();

    // Define fetchable providers (Google uses OAuth which is complex, skip it)
    struct FetchTarget {
        provider_type: ProviderType,
        provider_id: &'static str,
        base_url: &'static str,
        prefix_filters: Vec<&'static str>,
        /// Extra query on `/models` (e.g. OpenRouter `?sort=most-popular`).
        models_query: Option<&'static str>,
        /// When false, keep the API response order (popularity sorts).
        sort_results_by_id: bool,
        /// Fetch even when no API key is configured (public catalog endpoints).
        allow_unauthenticated: bool,
        /// Upper bound on models merged from this provider's `/v1/models`.
        /// Set for prefix-less, large catalogs (OpenRouter) to keep the
        /// routing picker bounded; `None` means no cap.
        max_models: Option<usize>,
    }

    let targets = vec![
        FetchTarget {
            provider_type: ProviderType::OpenAI,
            provider_id: "openai",
            base_url: "https://api.openai.com/v1",
            prefix_filters: vec!["gpt-", "o1-", "o3-", "o4-", "chatgpt-"],
            models_query: None,
            sort_results_by_id: true,
            allow_unauthenticated: false,
            max_models: None,
        },
        FetchTarget {
            provider_type: ProviderType::OpenRouter,
            provider_id: "open-router",
            base_url: "https://openrouter.ai/api/v1",
            // No usable prefix; ask OpenRouter for weekly token volume order.
            prefix_filters: vec![],
            models_query: Some("?sort=most-popular"),
            sort_results_by_id: false,
            allow_unauthenticated: true,
            max_models: Some(MAX_CATALOG_MODELS_PER_PROVIDER),
        },
        FetchTarget {
            provider_type: ProviderType::Xai,
            provider_id: "xai",
            base_url: "https://api.x.ai/v1",
            prefix_filters: vec!["grok-"],
            models_query: None,
            sort_results_by_id: true,
            allow_unauthenticated: false,
            max_models: None,
        },
        FetchTarget {
            provider_type: ProviderType::Cerebras,
            provider_id: "cerebras",
            base_url: "https://api.cerebras.ai/v1",
            prefix_filters: vec![],
            models_query: None,
            sort_results_by_id: true,
            allow_unauthenticated: false,
            max_models: None,
        },
        FetchTarget {
            provider_type: ProviderType::Zai,
            provider_id: "zai",
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            prefix_filters: vec!["glm-"],
            models_query: None,
            sort_results_by_id: true,
            allow_unauthenticated: false,
            max_models: None,
        },
        FetchTarget {
            provider_type: ProviderType::Minimax,
            provider_id: "minimax",
            base_url: "https://api.minimax.io/v1",
            prefix_filters: vec!["MiniMax-"],
            models_query: None,
            sort_results_by_id: true,
            allow_unauthenticated: false,
            max_models: None,
        },
    ];

    // Resolve API keys for all targets + Anthropic
    let anthropic_key = get_anthropic_models_api_key(&providers_list);
    let target_keys: Vec<(FetchTarget, Option<String>)> = targets
        .into_iter()
        .map(|t| {
            let key = get_openai_compatible_models_api_key(t.provider_type, &providers_list);
            (t, key)
        })
        .collect();

    // Fetch public catalog in parallel with provider APIs. These entries are
    // not account-verified and are hidden unless include_unverified=true.
    let models_dev_handle = tokio::spawn(async move {
        match fetch_models_dev_catalog().await {
            Ok(catalog) => {
                let model_count: usize = catalog.values().map(Vec::len).sum();
                tracing::info!(
                    providers = catalog.len(),
                    models = model_count,
                    "Fetched public model metadata from models.dev"
                );
                Some(catalog)
            }
            Err(e) => {
                tracing::warn!("Failed to fetch models.dev catalog: {}", e);
                None
            }
        }
    });

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

    // Kimi has an OpenAI-compatible catalog but requires its coding User-Agent.
    // Use the shared, credential-aware cache so startup refresh also primes
    // mission workspace generation.
    let kimi_provider = preferred_usable_kimi_provider(&providers_list);
    let kimi_handle = tokio::spawn(async move {
        match kimi_provider {
            Some(provider) => match fetch_kimi_models(&provider).await {
                Ok(models) => {
                    tracing::info!("Fetched {} models from Kimi API", models.len());
                    Some(("kimi".to_string(), models))
                }
                Err(error) => {
                    tracing::warn!("Failed to fetch Kimi models: {}", error);
                    None
                }
            },
            None => {
                tracing::debug!("No Kimi OAuth account, skipping model fetch");
                None
            }
        }
    });

    // Fetch OpenAI-compatible providers concurrently
    let mut handles = vec![anthropic_handle, kimi_handle];
    for (target, key) in target_keys {
        let provider_id = target.provider_id.to_string();
        let base_url = target.base_url.to_string();
        let max_models = target.max_models;
        let models_query = target.models_query.map(str::to_string);
        let sort_results_by_id = target.sort_results_by_id;
        let allow_unauthenticated = target.allow_unauthenticated;
        let prefix_filters: Vec<String> = target
            .prefix_filters
            .iter()
            .map(|s| s.to_string())
            .collect();

        handles.push(tokio::spawn(async move {
            let api_key = match key {
                Some(k) if !k.is_empty() => Some(k),
                _ if allow_unauthenticated => Some(String::new()),
                _ => None,
            };
            match api_key {
                Some(api_key) => {
                    let filters: Vec<&str> = prefix_filters.iter().map(|s| s.as_str()).collect();
                    match fetch_openai_compatible_models(
                        &base_url,
                        &api_key,
                        &filters,
                        models_query.as_deref(),
                        sort_results_by_id,
                    )
                    .await
                    {
                        Ok(mut models) => {
                            // Bound large catalogs (OpenRouter). OpenRouter
                            // keeps the API's most-popular order; others are
                            // sorted by id before truncation.
                            if let Some(limit) = max_models {
                                if models.len() > limit {
                                    tracing::info!(
                                        "Capping {} catalog from {} to {} models",
                                        provider_id,
                                        models.len(),
                                        limit
                                    );
                                    models.truncate(limit);
                                }
                            }
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

    // Custom providers (self-hosted OpenAI-compatible routers, e.g. the
    // dgx-spark-router) expose /v1/models. Fetch their live model list so the
    // catalog reflects what the router actually serves instead of the
    // operator's hardcoded `custom_models`, which drift out of date.
    for provider in &providers_list {
        if provider.provider_type != ProviderType::Custom || !provider.enabled {
            continue;
        }
        let Some(base_url) = provider.base_url.clone().filter(|u| !u.trim().is_empty()) else {
            continue;
        };
        let provider_id = sanitize_custom_provider_id(&provider.name);
        // /v1/models is usually unauthenticated on these routers; send the key
        // when present, empty otherwise.
        let api_key = provider.api_key.clone().unwrap_or_default();
        handles.push(tokio::spawn(async move {
            match fetch_openai_compatible_models(&base_url, &api_key, &[], None, true).await {
                Ok(models) if !models.is_empty() => {
                    tracing::info!(
                        "Fetched {} models from custom provider {} ({})",
                        models.len(),
                        provider_id,
                        base_url
                    );
                    Some((provider_id, models))
                }
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(
                        "Failed to fetch custom provider {} models from {}: {}",
                        provider_id,
                        base_url,
                        e
                    );
                    None
                }
            }
        }));
    }

    if let Ok(Some(public_catalog)) = models_dev_handle.await {
        for (provider_id, entries) in public_catalog {
            merge_catalog_entries(&mut result, &provider_id, entries);
        }
    }

    // Collect provider API results. These are account-verified and selectable
    // by default.
    let now = chrono::Utc::now();
    for handle in handles {
        if let Ok(Some((provider_id, models))) = handle.await {
            if !models.is_empty() {
                let entries = models.into_iter().map(|model| {
                    CatalogEntry::from_provider_model(
                        provider_id.clone(),
                        model,
                        CatalogSource::ProviderApi,
                        CatalogAvailability::Available,
                        now,
                    )
                });
                merge_catalog_entries(&mut result, &provider_id, entries);
            }
        }
    }

    (result, catalog_generation)
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

    // 3. Check sandboxed.sh provider config (.sandboxed-sh/ai_providers.json)
    let ai_providers_path = working_dir.join(AI_PROVIDERS_PATH);
    if let Ok(contents) = std::fs::read_to_string(&ai_providers_path) {
        if let Ok(providers) =
            serde_json::from_str::<Vec<crate::ai_providers::AIProvider>>(&contents)
        {
            for provider in providers {
                if provider.enabled && provider.has_credentials() {
                    // Use the same id the provider listing exposes: custom
                    // providers are keyed by their sanitized name (e.g.
                    // "spark"), not the generic "custom" type id, so the
                    // dashboard's connected-marker lookup matches.
                    if provider.provider_type == ProviderType::Custom {
                        configured.insert(sanitize_custom_provider_id(&provider.name));
                    } else {
                        configured.insert(provider.provider_type.id().to_string());
                    }
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
/// Replace authoritative provider catalogs with their live `/v1/models` list.
///
/// Custom routers and Kimi return complete model catalogs, so merging would
/// leave removed/renamed models selectable forever. Other built-in providers
/// retain additive semantics because some subscription probes expose only a
/// subset of valid choices.
fn apply_live_authoritative_provider_models(
    providers: &mut [Provider],
    store_providers: &[crate::ai_providers::AIProvider],
    cached: &HashMap<String, Vec<CatalogEntry>>,
    include_unverified: bool,
) {
    let mut authoritative_provider_ids: HashSet<String> = store_providers
        .iter()
        .filter(|p| p.provider_type == ProviderType::Custom && p.enabled)
        .map(|p| sanitize_custom_provider_id(&p.name))
        .collect();
    if store_providers
        .iter()
        .any(|p| p.provider_type == ProviderType::Kimi && p.enabled)
    {
        authoritative_provider_ids.insert("kimi".to_string());
    }
    for provider in providers.iter_mut() {
        if !authoritative_provider_ids.contains(&provider.id) {
            continue;
        }
        if let Some(entries) = cached.get(&provider.id) {
            let live: Vec<ProviderModel> = entries
                .iter()
                .filter(|entry| {
                    if provider.id == "kimi" {
                        entry.availability == CatalogAvailability::Available
                    } else {
                        include_unverified || entry.is_selectable_by_default()
                    }
                })
                .map(CatalogEntry::to_provider_model)
                .collect();
            if !live.is_empty() {
                provider.models = live;
            }
        }
    }
}

pub async fn list_providers(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProvidersQuery>,
) -> Json<ProvidersResponse> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    // Extend hardcoded defaults with the dynamic catalog. Some subscription
    // catalog probes can return only a currently-selected model, so replacing
    // the defaults would hide valid choices such as newly released Claude Opus.
    let cached = state.model_catalog.read().await.clone();
    merge_cached_provider_models(&mut config, &cached, query.include_unverified);

    // Get the set of configured provider IDs
    let configured = get_configured_provider_ids(state.config.working_dir.as_path());

    let mut providers = if query.include_all {
        config.providers
    } else {
        // Filter providers to only include those that are configured
        config
            .providers
            .into_iter()
            .filter(|p| configured.contains(&p.id))
            .collect()
    };

    let store_providers = state.ai_providers.list().await;
    merge_store_provider_models(&mut providers, &store_providers, query.include_all);

    apply_live_authoritative_provider_models(
        &mut providers,
        &store_providers,
        &cached,
        query.include_unverified,
    );
    drop(cached);

    Json(ProvidersResponse {
        providers,
        configured_ids: configured.into_iter().collect(),
    })
}

/// Full catalog of every supported model across all providers — configured or
/// not, account-verified or public-catalog — as one flat list. Unlike
/// `/api/providers/backend-models` (which filters/groups per harness), this is
/// the complete "everything that is supported" view, intended for building
/// model-routing chains. Each entry's `value` is chain-ready (`provider/model`).
///
/// GET /api/providers/catalog
pub async fn list_full_model_catalog(
    State(state): State<Arc<AppState>>,
) -> Json<FullCatalogResponse> {
    // "Everything supported" => always include public-catalog (unverified)
    // models and providers that aren't configured yet.
    let models = catalog_model_options_for_state(&state, true, false).await;

    Json(FullCatalogResponse {
        count: models.len(),
        models,
    })
}

/// List model options grouped by backend.
///
/// This is used by the frontend to power per-harness model override pickers.
pub async fn list_backend_model_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BackendModelsQuery>,
) -> Json<BackendModelOptionsResponse> {
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    // Extend hardcoded defaults with the dynamic catalog.
    let cached = state.model_catalog.read().await.clone();
    merge_cached_provider_models(&mut config, &cached, query.include_unverified);

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

    let store_providers = state.ai_providers.list().await;
    merge_store_provider_models(&mut providers, &store_providers, query.include_all);
    apply_live_authoritative_provider_models(
        &mut providers,
        &store_providers,
        &cached,
        query.include_unverified,
    );
    drop(cached);

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
    // Codex model catalog includes codex-* IDs plus current GPT-5 family
    // API slugs. The Codex CLI
    // passes `--model <slug>` straight through to OpenAI's backend, so
    // a new slug starts working as soon as the backend recognizes it
    // — there is no hard dependency on the CLI's embedded catalog
    // being up-to-date.
    let codex_filter: &dyn Fn(&str) -> bool = &|id: &str| {
        id.contains("codex")
            || matches!(
                id,
                "gpt-5.5"
                    | "gpt-5.6"
                    | "gpt-5.6-sol"
                    | "gpt-5.6-terra"
                    | "gpt-5.6-luna"
                    | "gpt-5.5-pro"
                    | "gpt-5.4"
                    | "gpt-5.4-pro"
                    | "gpt-5.4-mini"
                    | "gpt-5.4-nano"
            )
    };
    push_options("codex", Some(&["openai"]), false, Some(codex_filter));
    push_options("gemini", Some(&["google"]), false, None);
    push_options("opencode", None, true, None);
    let grok_filter: &dyn Fn(&str) -> bool = &|id: &str| is_grok_backend_model_id(id);
    push_options("grok", Some(&["xai"]), false, Some(grok_filter));

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
    let working_dir = state.config.working_dir.to_string_lossy().to_string();
    let mut config = load_providers_config(&working_dir);

    // Extend hardcoded defaults with the dynamic catalog.
    let cached = state.model_catalog.read().await.clone();
    merge_cached_provider_models(&mut config, &cached, true);

    // Load all providers (including configured and non-default)
    let mut providers = config.providers;
    let store_providers = state.ai_providers.list().await;
    merge_store_provider_models(&mut providers, &store_providers, true);
    apply_live_authoritative_provider_models(&mut providers, &store_providers, &cached, true);
    drop(cached);

    match backend {
        "opencode" => {
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
                // Plain model name without '/' — could be a routing chain ID
                // (e.g. "grok"), a builtin model alias, or a custom model name.
                // Allow through as an escape hatch.
                Ok(())
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
                        "Anthropic provider not configured. Expected a Claude model ID (e.g., 'claude-opus-5'), got '{}'",
                        model_override
                    ))
                }
            }
        }
        "codex" => {
            reject_known_unsupported_codex_model(model_override)?;
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
        "gemini" => {
            // Gemini expects raw model IDs from Google
            let google = providers.iter().find(|p| p.id == "google");
            if let Some(provider) = google {
                if !provider.models.iter().any(|m| m.id == model_override) {
                    // Allow unknown Gemini models (escape hatch for new models)
                    if model_override.starts_with("gemini-") {
                        Ok(())
                    } else {
                        Err(format!(
                            "Model '{}' not found in Google catalog. Available models: {}. For custom Gemini models, use format 'gemini-*'",
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
                // Google not configured, but allow if it looks like a Gemini model
                if model_override.starts_with("gemini-") {
                    Ok(())
                } else {
                    Err(format!(
                        "Google provider not configured. Expected a Gemini model ID (e.g., 'gemini-3.1-pro-preview'), got '{}'",
                        model_override
                    ))
                }
            }
        }
        "grok" => {
            let xai = providers.iter().find(|p| p.id == "xai");
            if let Some(provider) = xai {
                let cli_models: Vec<&ProviderModel> = provider
                    .models
                    .iter()
                    .filter(|model| is_grok_backend_model_id(&model.id))
                    .collect();
                if cli_models.iter().any(|m| m.id == model_override) {
                    Ok(())
                } else {
                    Err(format!(
                        "Model '{}' not found in xAI/Grok CLI catalog. Available models: {}. Run `grok models` on the server to verify account-level availability; 'composer-*' / 'composer-2.5' is a product name, not a valid xAI API id.",
                        model_override,
                        cli_models
                            .iter()
                            .map(|m| m.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            } else {
                Err(format!(
                    "xAI provider not configured. Expected a cataloged Grok model ID (e.g., 'grok-build-0.1'), got '{}'",
                    model_override
                ))
            }
        }
        _ => {
            // Unknown backend - skip validation
            Ok(())
        }
    }
}

fn reject_known_unsupported_codex_model(model_id: &str) -> Result<(), String> {
    if model_id == "gpt-5.5-sol" {
        return Err(
            "Model 'gpt-5.5-sol' is not supported by Codex with ChatGPT authentication. Use 'gpt-5.6-terra' (recommended with model_effort='medium') or select an account-supported model explicitly."
                .to_string(),
        );
    }
    Ok(())
}

fn is_grok_backend_model_id(model_id: &str) -> bool {
    GROK_CLI_TEXT_MODEL_IDS.contains(&model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_catalog_skips_a_higher_priority_account_without_oauth() {
        let mut disconnected = AIProvider::new(ProviderType::Kimi, "Disconnected Kimi".to_string());
        disconnected.priority = 0;

        let mut connected = AIProvider::new(ProviderType::Kimi, "Connected Kimi".to_string());
        connected.priority = 10;
        connected.oauth = Some(crate::ai_providers::OAuthCredentials {
            access_token: "connected-access".to_string(),
            refresh_token: "connected-refresh".to_string(),
            expires_at: i64::MAX,
        });

        let selected = preferred_usable_kimi_provider(&[disconnected, connected.clone()])
            .expect("connected Kimi account should remain selectable");
        assert_eq!(selected.id, connected.id);
    }

    #[tokio::test]
    async fn kimi_catalog_rejects_a_result_from_before_reconnect() {
        let mut provider = AIProvider::new(ProviderType::Kimi, "Kimi".to_string());
        provider.oauth = Some(crate::ai_providers::OAuthCredentials {
            access_token: "old-account-access".to_string(),
            refresh_token: "old-account-refresh".to_string(),
            expires_at: i64::MAX,
        });
        provider.base_url = Some("https://example.invalid".to_string());

        let (route_fingerprint, _, _) = kimi_route_fingerprint(&provider).unwrap();
        let probe_generation = kimi_model_cache()
            .read()
            .await
            .generations
            .get(&route_fingerprint)
            .copied()
            .unwrap_or_default();

        invalidate_kimi_model_cache(&provider).await;
        let result = store_kimi_models_if_current(
            route_fingerprint,
            probe_generation,
            vec![ProviderModel {
                id: "old-account-only".to_string(),
                name: "Old account only".to_string(),
                description: None,
            }],
            KIMI_MODEL_CACHE_TTL,
        )
        .await;

        assert!(result.is_err());
        assert!(cached_kimi_models(&provider).await.is_none());
    }

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

    #[test]
    fn kimi_fallback_catalog_matches_current_coding_models() {
        let ids: Vec<String> = kimi_fallback_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "kimi-for-coding",
                "kimi-for-coding-highspeed",
                "k3",
                "k3-256k"
            ]
        );
        assert!(!ids.iter().any(|id| id == "kimi-k2.6"));
        assert!(!ids.iter().any(|id| id == "kimi-k2-thinking"));
    }

    #[test]
    fn provider_config_upgrade_removes_retired_kimi_models() {
        let mut config = ProvidersConfig {
            providers: vec![Provider {
                id: "kimi".to_string(),
                name: "Kimi".to_string(),
                billing: "subscription".to_string(),
                description: "stale config".to_string(),
                models: vec![
                    ProviderModel {
                        id: "kimi-k2.6".to_string(),
                        name: "Kimi K2.6".to_string(),
                        description: None,
                    },
                    ProviderModel {
                        id: "operator-model".to_string(),
                        name: "Operator model".to_string(),
                        description: None,
                    },
                ],
            }],
        };

        merge_default_provider_models(&mut config);

        let ids: Vec<&str> = config.providers[0]
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(!ids.contains(&"kimi-k2.6"));
        assert!(ids.contains(&"operator-model"));
        assert!(ids.contains(&"k3"));
    }

    #[test]
    fn live_kimi_catalog_replaces_fallback_and_removed_models() {
        let mut providers = default_providers_config().providers;
        let kimi_account =
            crate::ai_providers::AIProvider::new(ProviderType::Kimi, "Kimi".to_string());
        let cached = HashMap::from([(
            "kimi".to_string(),
            vec![CatalogEntry::from_provider_model(
                "kimi",
                ProviderModel {
                    id: "k4-future".to_string(),
                    name: "K4 Future".to_string(),
                    description: None,
                },
                CatalogSource::ProviderApi,
                CatalogAvailability::Available,
                chrono::Utc::now(),
            )],
        )]);

        apply_live_authoritative_provider_models(&mut providers, &[kimi_account], &cached, true);

        let kimi = providers
            .iter()
            .find(|provider| provider.id == "kimi")
            .unwrap();
        assert_eq!(kimi.models.len(), 1);
        assert_eq!(kimi.models[0].id, "k4-future");
    }

    #[test]
    fn openai_compatible_catalog_parser_accepts_future_kimi_models() {
        let models = parse_openai_compatible_models(
            serde_json::json!({
                "data": [
                    {"id": "k3", "display_name": "K3"},
                    {"id": "k4-future", "display_name": "K4 Future"}
                ]
            }),
            &[],
            true,
        )
        .unwrap();

        assert_eq!(models[0].id, "k3");
        assert_eq!(models[1].id, "k4-future");
        assert_eq!(models[1].name, "K4 Future");
    }

    #[test]
    fn default_anthropic_catalog_leads_with_opus_5() {
        let defaults = default_providers_config();
        let anthropic = defaults
            .providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .expect("anthropic provider");
        assert_eq!(anthropic.models[0].id, "claude-opus-5");
        assert!(anthropic
            .models
            .iter()
            .any(|model| model.id == "claude-opus-4-8"));
    }

    #[test]
    fn anthropic_catalog_key_ignores_oauth_only_store_accounts() {
        let mut oauth =
            crate::ai_providers::AIProvider::new(ProviderType::Anthropic, "OAuth".into());
        oauth.oauth = Some(crate::ai_providers::OAuthCredentials {
            access_token: "oauth-access-token".into(),
            refresh_token: "oauth-refresh-token".into(),
            expires_at: chrono::Utc::now().timestamp_millis() + 60_000,
        });
        assert_eq!(
            get_enabled_store_api_key(ProviderType::Anthropic, &[oauth]),
            None
        );

        let mut api =
            crate::ai_providers::AIProvider::new(ProviderType::Anthropic, "API key".into());
        api.api_key = Some("sk-ant-api-key".into());
        assert_eq!(
            get_enabled_store_api_key(ProviderType::Anthropic, &[api]).as_deref(),
            Some("sk-ant-api-key")
        );
    }

    #[test]
    fn openai_platform_catalog_key_ignores_codex_oauth_only_accounts() {
        let mut oauth =
            crate::ai_providers::AIProvider::new(ProviderType::OpenAI, "Codex OAuth".into());
        oauth.oauth = Some(crate::ai_providers::OAuthCredentials {
            access_token: "chatgpt-oauth-access-token".into(),
            refresh_token: "chatgpt-oauth-refresh-token".into(),
            expires_at: chrono::Utc::now().timestamp_millis() + 60_000,
        });
        assert_eq!(
            get_openai_compatible_models_api_key(ProviderType::OpenAI, &[oauth]),
            None
        );

        let mut api =
            crate::ai_providers::AIProvider::new(ProviderType::OpenAI, "API Platform".into());
        api.api_key = Some("sk-openai-api-key".into());
        assert_eq!(
            get_openai_compatible_models_api_key(ProviderType::OpenAI, &[api]).as_deref(),
            Some("sk-openai-api-key")
        );

        let legacy_oauth = serde_json::json!({
            "type": "oauth",
            "key": "legacy-chatgpt-access-token"
        });
        assert_eq!(
            provider_auth_entry_api_key(ProviderType::OpenAI, &legacy_oauth),
            None
        );
        let platform_key = serde_json::json!({
            "type": "api_key",
            "key": "sk-openai-api-key"
        });
        assert_eq!(
            provider_auth_entry_api_key(ProviderType::OpenAI, &platform_key).as_deref(),
            Some("sk-openai-api-key")
        );
    }

    #[test]
    fn default_openai_catalog_includes_current_gpt_family() {
        let defaults = default_providers_config();
        let openai = defaults
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .expect("openai provider");
        let ids = openai
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();

        for id in [
            "gpt-5.6",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-pro",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
        ] {
            assert!(ids.contains(&id), "missing OpenAI model {id}");
        }

        for invalid_id in ["sol", "terra", "luna"] {
            assert!(
                !ids.contains(&invalid_id),
                "bare/non-API slug should not be exposed: {invalid_id}"
            );
        }
    }

    #[test]
    fn rejects_invented_gpt_55_sol_variant_before_dispatch() {
        let error = reject_known_unsupported_codex_model("gpt-5.5-sol")
            .expect_err("invented model variant must be rejected");
        assert!(error.contains("gpt-5.6-terra"));
        assert!(reject_known_unsupported_codex_model("gpt-5.6-terra").is_ok());
    }

    #[test]
    fn default_openrouter_catalog_uses_vendor_prefixed_model_ids() {
        let defaults = default_providers_config();
        let openrouter = defaults
            .providers
            .iter()
            .find(|provider| provider.id == "open-router")
            .expect("openrouter provider");

        assert!(openrouter
            .models
            .iter()
            .any(|model| model.id == "anthropic/claude-opus-5"));
        assert!(openrouter.models.iter().all(|model| model.id.contains('/')));
    }

    #[test]
    fn models_dev_provider_key_maps_openrouter() {
        assert_eq!(models_dev_provider_key("open-router"), "openrouter");
        assert_eq!(models_dev_provider_key("anthropic"), "anthropic");
    }

    #[test]
    fn cap_catalog_entries_prioritizes_seeds_then_truncates() {
        let now = chrono::Utc::now();
        let make = |id: &str| CatalogEntry {
            id: id.to_string(),
            name: id.to_string(),
            provider_id: OPENROUTER_PROVIDER_ID.to_string(),
            sources: vec![CatalogSource::ModelsDev],
            availability: CatalogAvailability::Known,
            last_checked_at: now,
            description: None,
        };
        let mut entries: Vec<CatalogEntry> = (0..150)
            .map(|i| make(&format!("vendor/model-{i:03}")))
            .collect();
        entries.push(make("anthropic/claude-opus-5"));
        entries.push(make("openai/gpt-5.6"));
        entries.push(make("openai/gpt-5.5"));

        let capped = cap_catalog_entries(
            entries,
            MAX_CATALOG_MODELS_PER_PROVIDER,
            OPENROUTER_SEED_MODEL_IDS,
        );
        assert_eq!(capped.len(), MAX_CATALOG_MODELS_PER_PROVIDER);
        assert_eq!(capped[0].id, "anthropic/claude-opus-5");
        assert!(capped.iter().any(|entry| entry.id == "openai/gpt-5.6"));
        assert!(capped.iter().any(|entry| entry.id == "openai/gpt-5.5"));
    }

    #[test]
    fn truncate_description_bounds_long_text() {
        // Short descriptions pass through unchanged (after trimming).
        assert_eq!(truncate_description("  Short blurb  "), "Short blurb");

        // Long descriptions are capped and get an ellipsis.
        let long = "x".repeat(MAX_MODEL_DESCRIPTION_CHARS + 50);
        let out = truncate_description(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_MODEL_DESCRIPTION_CHARS + 1);

        // Multibyte text isn't split mid-codepoint.
        let emoji = "🚀".repeat(MAX_MODEL_DESCRIPTION_CHARS + 10);
        let out = truncate_description(&emoji);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_MODEL_DESCRIPTION_CHARS + 1);
    }

    #[test]
    fn default_xai_catalog_includes_grok_build_for_model_routing() {
        let defaults = default_providers_config();
        let xai = defaults
            .providers
            .iter()
            .find(|provider| provider.id == "xai")
            .expect("xai provider");
        // The CLI-valid coding model id (bare `grok-build` is rejected by
        // current CLIs). `composer-2.5` is intentionally NOT here: it's a
        // product name, not a valid xAI API id, and the API rejects it.
        assert!(xai.models.iter().any(|model| model.id == "grok-build-0.1"));
        assert!(xai.models.iter().any(|model| model.id == "grok-4.5"));
        assert!(xai.models.iter().any(|model| model.id == "grok-4.5-latest"));
        assert!(xai
            .models
            .iter()
            .any(|model| model.id == "grok-build-latest"));
        assert!(!xai.models.iter().any(|model| model.id == "composer-2.5"));
    }

    #[test]
    fn grok_backend_model_ids_are_cli_text_allowlist() {
        assert!(is_grok_backend_model_id("grok-build-0.1"));
        assert!(is_grok_backend_model_id("grok-4.20-0309-reasoning"));
        assert!(is_grok_backend_model_id("grok-4.20-0309-non-reasoning"));
        assert!(is_grok_backend_model_id("grok-4.20-multi-agent-0309"));
        assert!(is_grok_backend_model_id("grok-4.5"));
        // API rolling aliases are not assumed to be native CLI model IDs.
        assert!(!is_grok_backend_model_id("grok-4.5-latest"));
        assert!(!is_grok_backend_model_id("grok-build-latest"));
        assert!(!is_grok_backend_model_id("grok-imagine-image"));
        // `composer-*` is a product name, not a Grok CLI model id.
        assert!(!is_grok_backend_model_id("composer-2.5"));
        assert!(!is_grok_backend_model_id("claude-opus-4-7"));
    }

    #[test]
    fn grok_backend_model_options_exclude_composer() {
        let defaults = default_providers_config();
        let xai = defaults
            .providers
            .iter()
            .find(|provider| provider.id == "xai")
            .expect("xai provider");
        let option_ids: Vec<&str> = xai
            .models
            .iter()
            .map(|model| model.id.as_str())
            .filter(|id| is_grok_backend_model_id(id))
            .collect();

        assert!(option_ids.contains(&"grok-build-0.1"));
        assert!(option_ids.contains(&"grok-4.5"));
        assert!(!option_ids.contains(&"composer-2.5"));
    }

    #[test]
    fn merge_default_provider_models_adds_new_builtin_models_to_stale_config() {
        let mut config = ProvidersConfig {
            providers: vec![Provider {
                id: "anthropic".to_string(),
                name: "Claude (Subscription)".to_string(),
                billing: "subscription".to_string(),
                description: "Included in Claude Max".to_string(),
                models: vec![ProviderModel {
                    id: "claude-opus-4-6".to_string(),
                    name: "Claude Opus 4.6".to_string(),
                    description: None,
                }],
            }],
        };

        merge_default_provider_models(&mut config);

        let anthropic = config
            .providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .expect("anthropic provider");
        assert!(anthropic
            .models
            .iter()
            .any(|model| model.id == "claude-opus-5"));
    }

    #[test]
    fn merge_cached_provider_models_keeps_builtin_subscription_models() {
        let mut config = default_providers_config();
        let mut cached = HashMap::new();
        cached.insert(
            "anthropic".to_string(),
            vec![CatalogEntry::from_provider_model(
                "anthropic",
                ProviderModel {
                    id: "claude-opus-4-6".to_string(),
                    name: "Claude Opus 4.6".to_string(),
                    description: None,
                },
                CatalogSource::ProviderApi,
                CatalogAvailability::Available,
                chrono::Utc::now(),
            )],
        );

        merge_cached_provider_models(&mut config, &cached, false);

        let anthropic = config
            .providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .expect("anthropic provider");
        assert!(anthropic
            .models
            .iter()
            .any(|model| model.id == "claude-opus-5"));
        assert_eq!(
            anthropic
                .models
                .iter()
                .filter(|model| model.id == "claude-opus-4-6")
                .count(),
            1
        );
    }

    #[test]
    fn merge_cached_provider_models_hides_unverified_public_models_by_default() {
        let mut config = ProvidersConfig {
            providers: vec![Provider {
                id: "zai".to_string(),
                name: "Z.AI".to_string(),
                billing: "pay-per-token".to_string(),
                description: "GLM models".to_string(),
                models: vec![],
            }],
        };
        let mut cached = HashMap::new();
        cached.insert(
            "zai".to_string(),
            vec![CatalogEntry::from_provider_model(
                "zai",
                ProviderModel {
                    id: "glm-4.7-flash".to_string(),
                    name: "GLM 4.7 Flash".to_string(),
                    description: None,
                },
                CatalogSource::ModelsDev,
                CatalogAvailability::Known,
                chrono::Utc::now(),
            )],
        );

        merge_cached_provider_models(&mut config, &cached, false);
        assert!(config.providers[0].models.is_empty());

        merge_cached_provider_models(&mut config, &cached, true);
        assert_eq!(config.providers[0].models[0].id, "glm-4.7-flash");
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
                provider_id: "open-router",
                provider_type: ProviderType::OpenRouter,
                base_url: "https://openrouter.ai/api/v1",
                prefix_filters: vec![],
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
            let api_key = match get_api_key_for_provider(target.provider_type, &[]) {
                Some(k) => k,
                None if target.provider_id == OPENROUTER_PROVIDER_ID => String::new(),
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

            let (models_query, sort_results_by_id) = if target.provider_id == OPENROUTER_PROVIDER_ID
            {
                (Some("?sort=most-popular"), false)
            } else {
                (None, true)
            };

            let fetched = if target.is_anthropic {
                fetch_anthropic_models(&api_key).await
            } else {
                fetch_openai_compatible_models(
                    target.base_url,
                    &api_key,
                    &target.prefix_filters,
                    models_query,
                    sort_results_by_id,
                )
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
                "  ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY, XAI_API_KEY, CEREBRAS_API_KEY, ZHIPU_API_KEY"
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
