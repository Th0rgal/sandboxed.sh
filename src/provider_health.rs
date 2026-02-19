//! Provider health tracking and model chain definitions.
//!
//! Implements per-account cooldown tracking with exponential backoff,
//! model fallback chain definitions, and chain resolution logic.
//!
//! Used by the OpenAI-compatible proxy to route requests through fallback
//! chains, and by credential rotation in backend runners.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────────────
// Health Tracking
// ─────────────────────────────────────────────────────────────────────────────

/// Reason an account was placed into cooldown.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CooldownReason {
    /// HTTP 429 rate limit
    RateLimit,
    /// HTTP 529 overloaded
    Overloaded,
    /// Connection timeout or network error
    Timeout,
    /// Server error (5xx other than 529)
    ServerError,
    /// Authentication/authorization error (401/403)
    AuthError,
}

impl std::fmt::Display for CooldownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimit => write!(f, "rate_limit"),
            Self::Overloaded => write!(f, "overloaded"),
            Self::Timeout => write!(f, "timeout"),
            Self::ServerError => write!(f, "server_error"),
            Self::AuthError => write!(f, "auth_error"),
        }
    }
}

/// Snapshot of rate-limit quota state from provider response headers.
///
/// Providers send rate-limit information on every response (not just 429s).
/// This struct captures the current quota state for display in the health dashboard.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RateLimitSnapshot {
    /// Maximum requests allowed in the current window.
    pub requests_limit: Option<u64>,
    /// Requests remaining in the current window.
    pub requests_remaining: Option<u64>,
    /// When the request quota resets (ISO 8601 timestamp).
    pub requests_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// Maximum tokens allowed in the current window.
    pub tokens_limit: Option<u64>,
    /// Tokens remaining in the current window.
    pub tokens_remaining: Option<u64>,
    /// When the token quota resets (ISO 8601 timestamp).
    pub tokens_reset: Option<chrono::DateTime<chrono::Utc>>,
    /// Maximum input tokens allowed (Anthropic-specific).
    pub input_tokens_limit: Option<u64>,
    /// Input tokens remaining (Anthropic-specific).
    pub input_tokens_remaining: Option<u64>,
    /// Maximum output tokens allowed (Anthropic-specific).
    pub output_tokens_limit: Option<u64>,
    /// Output tokens remaining (Anthropic-specific).
    pub output_tokens_remaining: Option<u64>,
    /// When this snapshot was captured.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Health state for a single provider account.
#[derive(Debug, Clone, Default)]
pub struct AccountHealth {
    /// Provider identifier (e.g. "openai", "zai") — set on first interaction.
    pub provider_id: Option<String>,
    /// When the cooldown expires (None = healthy).
    pub cooldown_until: Option<std::time::Instant>,
    /// Number of consecutive failures (for exponential backoff).
    pub consecutive_failures: u32,
    /// Last failure reason.
    pub last_failure_reason: Option<CooldownReason>,
    /// Last failure timestamp (wall clock, for API responses).
    pub last_failure_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Total requests routed to this account.
    pub total_requests: u64,
    /// Total successful requests.
    pub total_successes: u64,
    /// Total rate-limited requests.
    pub total_rate_limits: u64,
    /// Total errors (non-rate-limit).
    pub total_errors: u64,
    /// Sum of all recorded latencies in milliseconds (for computing averages).
    pub total_latency_ms: u64,
    /// Number of latency samples recorded.
    pub latency_samples: u64,
    /// Total input (prompt) tokens consumed.
    pub total_input_tokens: u64,
    /// Total output (completion) tokens consumed.
    pub total_output_tokens: u64,
    /// Latest rate-limit quota snapshot from provider headers.
    pub rate_limit_snapshot: Option<RateLimitSnapshot>,
}

impl AccountHealth {
    /// Whether this account is currently in cooldown.
    pub fn is_in_cooldown(&self) -> bool {
        self.cooldown_until
            .map(|until| std::time::Instant::now() < until)
            .unwrap_or(false)
    }

    /// Remaining cooldown duration, if any.
    pub fn remaining_cooldown(&self) -> Option<std::time::Duration> {
        self.cooldown_until.and_then(|until| {
            let now = std::time::Instant::now();
            if now < until {
                Some(until - now)
            } else {
                None
            }
        })
    }
}

/// Backoff configuration for a provider type.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Base delay for first failure.
    pub base_delay: std::time::Duration,
    /// Maximum backoff cap.
    pub max_delay: std::time::Duration,
    /// Multiplier per consecutive failure (typically 2.0).
    pub multiplier: f64,
    /// After this many consecutive failures, the account is "degraded" and
    /// gets a much longer cooldown (max_delay × degraded_multiplier).
    pub circuit_breaker_threshold: u32,
    /// Multiplier applied to max_delay when circuit breaker trips.
    pub degraded_multiplier: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base_delay: std::time::Duration::from_secs(5),
            max_delay: std::time::Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            circuit_breaker_threshold: 5,
            degraded_multiplier: 6.0, // 5 min × 6 = 30 min when degraded
        }
    }
}

impl BackoffConfig {
    /// Calculate the cooldown duration for a given number of consecutive failures.
    ///
    /// Uses exponential backoff capped at `max_delay`. Once the circuit breaker
    /// threshold is reached, the cap is raised to `max_delay × degraded_multiplier`
    /// to avoid wasting requests on persistently failing accounts (e.g. quota
    /// exhaustion).
    pub fn cooldown_for(&self, consecutive_failures: u32) -> std::time::Duration {
        let delay_secs =
            self.base_delay.as_secs_f64() * self.multiplier.powi(consecutive_failures as i32);
        let cap = if consecutive_failures >= self.circuit_breaker_threshold {
            self.max_delay.as_secs_f64() * self.degraded_multiplier
        } else {
            self.max_delay.as_secs_f64()
        };
        let capped = delay_secs.min(cap);
        std::time::Duration::from_secs_f64(capped)
    }
}

/// A single fallback event: when the proxy failed over from one provider to the next.
#[derive(Debug, Clone, Serialize)]
pub struct FallbackEvent {
    /// When this event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// The chain being resolved.
    pub chain_id: String,
    /// Provider that failed.
    pub from_provider: String,
    /// Model that was being requested.
    pub from_model: String,
    /// Account that failed.
    pub from_account_id: Uuid,
    /// Why it failed.
    pub reason: CooldownReason,
    /// Cooldown duration set (seconds), if any.
    pub cooldown_secs: Option<f64>,
    /// The provider that ultimately succeeded (filled in after chain completes).
    pub to_provider: Option<String>,
    /// Request latency in milliseconds until the failure was detected.
    pub latency_ms: Option<u64>,
    /// 1-indexed position of this entry in the chain.
    pub attempt_number: u32,
    /// Total number of entries in the chain.
    pub chain_length: u32,
}

/// Maximum number of fallback events to keep in the ring buffer.
const MAX_FALLBACK_EVENTS: usize = 200;

/// Global health tracker for all provider accounts.
///
/// Thread-safe, shared across the proxy endpoint and all backend runners.
/// Keyed by account UUID so the same tracker works for AIProviderStore accounts
/// and for non-store accounts identified by synthetic UUIDs.
#[derive(Debug, Clone)]
pub struct ProviderHealthTracker {
    accounts: Arc<RwLock<HashMap<Uuid, AccountHealth>>>,
    backoff_config: BackoffConfig,
    /// Recent fallback events (ring buffer, newest last).
    fallback_events: Arc<RwLock<Vec<FallbackEvent>>>,
}

/// Serializable snapshot of account health for API responses.
#[derive(Debug, Clone, Serialize)]
pub struct AccountHealthSnapshot {
    pub account_id: Uuid,
    /// Provider identifier (e.g. "openai", "zai"). None if never used.
    pub provider_id: Option<String>,
    pub is_healthy: bool,
    pub cooldown_remaining_secs: Option<f64>,
    pub consecutive_failures: u32,
    pub last_failure_reason: Option<String>,
    pub last_failure_at: Option<chrono::DateTime<chrono::Utc>>,
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_rate_limits: u64,
    pub total_errors: u64,
    /// Average latency in milliseconds (None if no samples).
    pub avg_latency_ms: Option<f64>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Whether the circuit breaker has tripped (consecutive failures exceeded threshold).
    pub is_degraded: bool,
    /// Latest rate-limit quota snapshot from provider headers.
    pub rate_limit_snapshot: Option<RateLimitSnapshot>,
}

impl Default for ProviderHealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderHealthTracker {
    pub fn new() -> Self {
        Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            backoff_config: BackoffConfig::default(),
            fallback_events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn with_backoff(backoff_config: BackoffConfig) -> Self {
        Self {
            accounts: Arc::new(RwLock::new(HashMap::new())),
            backoff_config,
            fallback_events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Check whether an account is currently healthy (not in cooldown).
    pub async fn is_healthy(&self, account_id: Uuid) -> bool {
        let accounts = self.accounts.read().await;
        accounts
            .get(&account_id)
            .map(|h| !h.is_in_cooldown())
            .unwrap_or(true) // Unknown accounts are healthy by default
    }

    /// Set the provider identifier for an account (no-op if already set).
    pub async fn set_provider_id(&self, account_id: Uuid, provider_id: &str) {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();
        if health.provider_id.is_none() {
            health.provider_id = Some(provider_id.to_string());
        }
    }

    /// Record a successful request for an account.
    pub async fn record_success(&self, account_id: Uuid) {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();
        health.total_requests += 1;
        health.total_successes += 1;
        // Reset consecutive failures on success
        health.consecutive_failures = 0;
        health.cooldown_until = None;
    }

    /// Record a failure and place the account into cooldown.
    ///
    /// If `retry_after` is provided (from response headers), use that as the
    /// cooldown duration instead of exponential backoff.
    ///
    /// Returns the actual cooldown duration applied.
    pub async fn record_failure(
        &self,
        account_id: Uuid,
        reason: CooldownReason,
        retry_after: Option<std::time::Duration>,
    ) -> std::time::Duration {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();

        health.total_requests += 1;
        match &reason {
            CooldownReason::RateLimit | CooldownReason::Overloaded => health.total_rate_limits += 1,
            _ => health.total_errors += 1,
        }

        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        let is_auth_error = matches!(reason, CooldownReason::AuthError);
        health.last_failure_reason = Some(reason);
        health.last_failure_at = Some(chrono::Utc::now());

        // Use retry_after from headers if available, else exponential backoff.
        // Auth errors (401/403) are almost always permanent (bad API key,
        // revoked credentials), so use a long fixed cooldown instead of
        // short exponential backoff that implies eventual recovery.
        let cooldown = retry_after.unwrap_or_else(|| {
            if is_auth_error {
                std::time::Duration::from_secs(3600) // 1 hour
            } else {
                self.backoff_config
                    .cooldown_for(health.consecutive_failures.saturating_sub(1))
            }
        });

        health.cooldown_until = Some(std::time::Instant::now() + cooldown);

        let is_degraded =
            health.consecutive_failures >= self.backoff_config.circuit_breaker_threshold;
        if is_degraded {
            tracing::warn!(
                account_id = %account_id,
                consecutive_failures = health.consecutive_failures,
                cooldown_secs = cooldown.as_secs_f64(),
                "Circuit breaker tripped — account degraded with extended cooldown"
            );
        } else {
            tracing::info!(
                account_id = %account_id,
                consecutive_failures = health.consecutive_failures,
                cooldown_secs = cooldown.as_secs_f64(),
                "Account placed in cooldown"
            );
        }

        cooldown
    }

    /// Record a latency sample for an account (in milliseconds).
    pub async fn record_latency(&self, account_id: Uuid, latency_ms: u64) {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();
        health.total_latency_ms += latency_ms;
        health.latency_samples += 1;
    }

    /// Record token usage for an account.
    pub async fn record_token_usage(
        &self,
        account_id: Uuid,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();
        health.total_input_tokens += input_tokens;
        health.total_output_tokens += output_tokens;
    }

    /// Record rate-limit quota snapshot from provider response headers.
    ///
    /// Called on every successful response to track remaining quota.
    pub async fn record_rate_limits(&self, account_id: Uuid, snapshot: RateLimitSnapshot) {
        let mut accounts = self.accounts.write().await;
        let health = accounts.entry(account_id).or_default();
        health.rate_limit_snapshot = Some(snapshot);
    }

    /// Record a fallback event (provider failover).
    pub async fn record_fallback_event(&self, event: FallbackEvent) {
        let mut events = self.fallback_events.write().await;
        events.push(event);
        // Trim to ring buffer size
        if events.len() > MAX_FALLBACK_EVENTS {
            let excess = events.len() - MAX_FALLBACK_EVENTS;
            events.drain(..excess);
        }
    }

    /// Get recent fallback events (newest last).
    pub async fn get_recent_events(&self, limit: usize) -> Vec<FallbackEvent> {
        let events = self.fallback_events.read().await;
        let start = events.len().saturating_sub(limit);
        events[start..].to_vec()
    }

    /// Helper to build an `AccountHealthSnapshot` from an `AccountHealth`.
    fn snapshot(
        account_id: Uuid,
        health: &AccountHealth,
        backoff_config: &BackoffConfig,
    ) -> AccountHealthSnapshot {
        AccountHealthSnapshot {
            account_id,
            provider_id: health.provider_id.clone(),
            is_healthy: !health.is_in_cooldown(),
            cooldown_remaining_secs: health.remaining_cooldown().map(|d| d.as_secs_f64()),
            consecutive_failures: health.consecutive_failures,
            last_failure_reason: health.last_failure_reason.as_ref().map(|r| r.to_string()),
            last_failure_at: health.last_failure_at,
            total_requests: health.total_requests,
            total_successes: health.total_successes,
            total_rate_limits: health.total_rate_limits,
            total_errors: health.total_errors,
            avg_latency_ms: if health.latency_samples > 0 {
                Some(health.total_latency_ms as f64 / health.latency_samples as f64)
            } else {
                None
            },
            total_input_tokens: health.total_input_tokens,
            total_output_tokens: health.total_output_tokens,
            is_degraded: health.consecutive_failures >= backoff_config.circuit_breaker_threshold,
            rate_limit_snapshot: health.rate_limit_snapshot.clone(),
        }
    }

    /// Get a snapshot of health state for an account (for API responses).
    pub async fn get_health(&self, account_id: Uuid) -> AccountHealthSnapshot {
        let accounts = self.accounts.read().await;
        match accounts.get(&account_id) {
            Some(health) => Self::snapshot(account_id, health, &self.backoff_config),
            None => AccountHealthSnapshot {
                account_id,
                provider_id: None,
                is_healthy: true,
                cooldown_remaining_secs: None,
                consecutive_failures: 0,
                last_failure_reason: None,
                last_failure_at: None,
                total_requests: 0,
                total_successes: 0,
                total_rate_limits: 0,
                total_errors: 0,
                avg_latency_ms: None,
                total_input_tokens: 0,
                total_output_tokens: 0,
                is_degraded: false,
                rate_limit_snapshot: None,
            },
        }
    }

    /// Get health snapshots for all tracked accounts.
    pub async fn get_all_health(&self) -> Vec<AccountHealthSnapshot> {
        let accounts = self.accounts.read().await;
        accounts
            .iter()
            .map(|(&id, health)| Self::snapshot(id, health, &self.backoff_config))
            .collect()
    }

    /// Clear cooldown for an account (e.g., after manual recovery).
    pub async fn clear_cooldown(&self, account_id: Uuid) {
        let mut accounts = self.accounts.write().await;
        if let Some(health) = accounts.get_mut(&account_id) {
            health.cooldown_until = None;
            health.consecutive_failures = 0;
        }
    }
}

/// Shared tracker type.
pub type SharedProviderHealthTracker = Arc<ProviderHealthTracker>;

// ─────────────────────────────────────────────────────────────────────────────
// Model Chain Definitions
// ─────────────────────────────────────────────────────────────────────────────

/// A single entry in a model chain: a provider + model pair.
///
/// When the chain is resolved, each entry is expanded into N entries —
/// one per configured account for that provider, ordered by account priority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainEntry {
    /// Provider type ID (e.g., "zai", "minimax", "anthropic").
    pub provider_id: String,
    /// Model ID to use with this provider (e.g., "glm-5", "minimax-2.5").
    pub model_id: String,
}

/// A named model chain (fallback sequence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChain {
    /// Unique chain ID (e.g., "builtin/smart", "user/fast").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Ordered list of provider+model entries (first = highest priority).
    pub entries: Vec<ChainEntry>,
    /// Whether this is the default chain.
    #[serde(default)]
    pub is_default: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A standard (non-custom) provider account read from OpenCode's config.
///
/// Standard providers live in `opencode.json` + `auth.json`, not in
/// `AIProviderStore`. This struct lets the chain resolver include them
/// without coupling to OpenCode's config format.
#[derive(Debug, Clone)]
pub struct StandardAccount {
    /// Stable UUID for health tracking (derived from provider type ID).
    pub account_id: Uuid,
    /// Which provider type this account belongs to.
    pub provider_type: crate::ai_providers::ProviderType,
    /// API key from auth.json (None if OAuth-only or unconfigured).
    pub api_key: Option<String>,
    /// Base URL override from opencode.json (if any).
    pub base_url: Option<String>,
}

/// Derive a deterministic UUID from a provider type ID string.
///
/// Uses SHA-256 to hash a fixed namespace + provider_id, then takes the first
/// 16 bytes as a UUID (similar to UUID v5 but with SHA-256 instead of SHA-1).
/// This ensures collision resistance even for short, similar provider IDs
/// like "xai" and "zai".
pub fn stable_provider_uuid(provider_id: &str) -> Uuid {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // Fixed namespace so different input domains don't collide
    hasher.update(b"sandboxed.sh:provider:");
    hasher.update(provider_id.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // Set UUID version 4 and variant 1 bits for structural validity
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// A resolved chain entry: a specific account + model ready for routing.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// The provider type.
    pub provider_id: String,
    /// The model ID.
    pub model_id: String,
    /// The specific account UUID.
    pub account_id: Uuid,
    /// The account's API key (if available).
    pub api_key: Option<String>,
    /// The account's base URL (if custom).
    pub base_url: Option<String>,
}

/// In-memory store for model chains, persisted to disk as JSON.
#[derive(Debug, Clone)]
pub struct ModelChainStore {
    chains: Arc<RwLock<Vec<ModelChain>>>,
    storage_path: PathBuf,
}

impl ModelChainStore {
    pub async fn new(storage_path: PathBuf) -> Self {
        let store = Self {
            chains: Arc::new(RwLock::new(Vec::new())),
            storage_path,
        };

        match store.load_from_disk() {
            Ok(loaded) => {
                let mut chains = store.chains.write().await;
                *chains = loaded;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No file yet — will be created on first write.
            }
            Err(e) => {
                tracing::error!(
                    "Failed to load model chains from {}: {}. Starting with empty chain store — \
                     user-defined chains may have been lost.",
                    store.storage_path.display(),
                    e
                );
            }
        }

        // Ensure default chain exists (check + insert under write lock)
        store.ensure_default_chain().await;

        store
    }

    /// Ensure the builtin default chains exist.
    ///
    /// Idempotent: skips any chain whose ID is already present.
    /// Checks and inserts under a single write lock to avoid TOCTOU races.
    async fn ensure_default_chain(&self) {
        let mut chains = self.chains.write().await;

        let now = chrono::Utc::now();
        let mut changed = false;

        if !chains.iter().any(|c| c.id == "builtin/smart") {
            chains.push(ModelChain {
                id: "builtin/smart".to_string(),
                name: "Smart (Default)".to_string(),
                entries: vec![
                    ChainEntry {
                        provider_id: "zai".to_string(),
                        model_id: "glm-5".to_string(),
                    },
                    ChainEntry {
                        provider_id: "minimax".to_string(),
                        model_id: "MiniMax-M2.5".to_string(),
                    },
                ],
                is_default: true,
                created_at: now,
                updated_at: now,
            });
            changed = true;
        }

        if !chains.iter().any(|c| c.id == "builtin/cheap") {
            chains.push(ModelChain {
                id: "builtin/cheap".to_string(),
                name: "Cheap".to_string(),
                entries: vec![ChainEntry {
                    provider_id: "zai".to_string(),
                    model_id: "glm-4.7".to_string(),
                }],
                is_default: false,
                created_at: now,
                updated_at: now,
            });
            changed = true;
        }

        if changed {
            if let Err(e) = self.save_chains_to_disk(&chains) {
                tracing::error!("Failed to save default model chains: {}", e);
            }
        }
    }

    fn load_from_disk(&self) -> Result<Vec<ModelChain>, std::io::Error> {
        let contents = std::fs::read_to_string(&self.storage_path)?;
        serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Serialize `chains` to JSON and write to disk atomically (write to
    /// temp file, then rename). Caller should pass the chains data directly
    /// so this can be called while the caller still holds the write lock,
    /// avoiding TOCTOU races between concurrent upsert/delete operations.
    fn save_chains_to_disk(&self, chains: &[ModelChain]) -> Result<(), std::io::Error> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(chains)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Write to a temp file then rename for atomic replacement.
        let tmp_path = self.storage_path.with_extension("tmp");
        std::fs::write(&tmp_path, contents)?;
        std::fs::rename(&tmp_path, &self.storage_path)?;
        Ok(())
    }

    /// List all chains.
    pub async fn list(&self) -> Vec<ModelChain> {
        self.chains.read().await.clone()
    }

    /// Get a chain by ID.
    pub async fn get(&self, id: &str) -> Option<ModelChain> {
        self.chains
            .read()
            .await
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    /// Get the default chain.
    pub async fn get_default(&self) -> Option<ModelChain> {
        let chains = self.chains.read().await;
        chains
            .iter()
            .find(|c| c.is_default)
            .or_else(|| chains.first())
            .cloned()
    }

    /// Add or update a chain.
    pub async fn upsert(&self, mut chain: ModelChain) {
        chain.updated_at = chrono::Utc::now();
        let mut chains = self.chains.write().await;

        // If setting as default, clear others
        if chain.is_default {
            for c in chains.iter_mut() {
                c.is_default = false;
            }
        }

        if let Some(existing) = chains.iter_mut().find(|c| c.id == chain.id) {
            *existing = chain;
        } else {
            chains.push(chain);
        }

        // Serialize while still holding the write lock to avoid TOCTOU races.
        if let Err(e) = self.save_chains_to_disk(&chains) {
            tracing::error!("Failed to save model chains: {}", e);
        }
    }

    /// Delete a chain by ID.
    ///
    /// Returns:
    /// - `Ok(true)` if deleted successfully
    /// - `Ok(false)` if chain not found
    /// - `Err(msg)` if deletion is not allowed (e.g., last chain)
    pub async fn delete(&self, id: &str) -> Result<bool, &'static str> {
        let mut chains = self.chains.write().await;

        if !chains.iter().any(|c| c.id == id) {
            return Ok(false);
        }

        if chains.len() <= 1 {
            return Err("Cannot delete the last remaining chain");
        }

        let was_default = chains.iter().any(|c| c.id == id && c.is_default);
        chains.retain(|c| c.id != id);

        // If we deleted the default chain, promote the first remaining chain.
        if was_default {
            if let Some(first) = chains.first_mut() {
                first.is_default = true;
            }
        }

        // Serialize while still holding the write lock to avoid TOCTOU races.
        if let Err(e) = self.save_chains_to_disk(&chains) {
            tracing::error!("Failed to save model chains after delete: {}", e);
        }
        Ok(true)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Chain Resolution
    // ─────────────────────────────────────────────────────────────────────

    /// Resolve a chain into an ordered list of (account, model) entries,
    /// expanding each chain entry across all configured accounts for that
    /// provider and filtering out accounts currently in cooldown.
    ///
    /// Accounts come from two sources:
    /// 1. `AIProviderStore` — custom providers and future multi-account standard providers
    /// 2. `standard_accounts` — standard providers from OpenCode's config files
    ///
    /// Returns entries in priority order, ready for waterfall routing.
    pub async fn resolve_chain(
        &self,
        chain_id: &str,
        ai_providers: &crate::ai_providers::AIProviderStore,
        standard_accounts: &[StandardAccount],
        health_tracker: &ProviderHealthTracker,
    ) -> Vec<ResolvedEntry> {
        let chain = match self.get(chain_id).await {
            Some(c) => c,
            None => return Vec::new(),
        };

        let mut resolved = Vec::new();

        for entry in &chain.entries {
            let provider_type = match crate::ai_providers::ProviderType::from_id(&entry.provider_id)
            {
                Some(pt) => pt,
                None => {
                    tracing::warn!(
                        provider_id = %entry.provider_id,
                        "Unknown provider type in chain, skipping"
                    );
                    continue;
                }
            };

            // Collect account IDs we've already added to avoid duplicates
            // when both store and standard accounts exist for the same provider.
            let mut seen_account_ids = std::collections::HashSet::new();

            // 1. Check AIProviderStore (custom providers, multi-account)
            let store_accounts = ai_providers.get_all_by_type(provider_type).await;

            for account in &store_accounts {
                if !health_tracker.is_healthy(account.id).await {
                    tracing::debug!(
                        account_id = %account.id,
                        provider = %entry.provider_id,
                        "Skipping account in cooldown"
                    );
                    continue;
                }
                if !account.has_credentials() {
                    continue;
                }
                seen_account_ids.insert(account.id);
                resolved.push(ResolvedEntry {
                    provider_id: entry.provider_id.clone(),
                    model_id: entry.model_id.clone(),
                    account_id: account.id,
                    api_key: account.api_key.clone(),
                    base_url: account.base_url.clone(),
                });
            }

            // 2. Also check standard accounts from OpenCode config.
            // These complement store accounts — a user may have both custom
            // multi-account entries AND standard credentials from auth.json.
            for sa in standard_accounts {
                if sa.provider_type != provider_type {
                    continue;
                }
                if seen_account_ids.contains(&sa.account_id) {
                    continue;
                }
                if !health_tracker.is_healthy(sa.account_id).await {
                    tracing::debug!(
                        account_id = %sa.account_id,
                        provider = %entry.provider_id,
                        "Skipping standard account in cooldown"
                    );
                    continue;
                }
                // Standard accounts must have an API key to be usable
                if sa.api_key.is_none() {
                    continue;
                }
                resolved.push(ResolvedEntry {
                    provider_id: entry.provider_id.clone(),
                    model_id: entry.model_id.clone(),
                    account_id: sa.account_id,
                    api_key: sa.api_key.clone(),
                    base_url: sa.base_url.clone(),
                });
            }
        }

        resolved
    }
}

/// Shared chain store type.
pub type SharedModelChainStore = Arc<ModelChainStore>;
