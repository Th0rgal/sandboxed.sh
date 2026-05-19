//! Shared in-memory cache of per-provider rate-limit / usage info.
//!
//! Populated as a side-effect of `/api/ai/providers/:id/usage` (a live call
//! to the provider) and read instantly by the dashboard via
//! `/api/ai/providers/usage` (bulk) or the same single-provider endpoint
//! with `?cached=1`.
//!
//! A background task refreshes stale entries by re-issuing the same live
//! fetch the dashboard does, so the cache stays fresh without any user
//! interaction. The cache is process-local — restarts repopulate lazily.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::RwLock;

/// How fresh a cached entry is considered before background refresh kicks in.
pub const REFRESH_AFTER: Duration = Duration::from_secs(120);

/// How old an entry can be before a cached read returns it as "stale" rather
/// than fresh. Reads still succeed, just with a stale flag in the response.
pub const STALE_AFTER: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct CachedUsage {
    pub value: Value,
    pub fetched_at: Instant,
    /// ISO-8601 fetched timestamp used for client display.
    pub fetched_at_iso: String,
}

impl CachedUsage {
    pub fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < REFRESH_AFTER
    }
    pub fn is_stale(&self) -> bool {
        self.fetched_at.elapsed() >= STALE_AFTER
    }
}

#[derive(Debug, Default)]
pub struct ProviderUsageCache {
    /// Key is whatever identifier was used to fetch (UUID string for stored
    /// providers, ProviderType id for OpenCode-auth-backed lookups).
    entries: RwLock<HashMap<String, CachedUsage>>,
}

impl ProviderUsageCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn get(&self, key: &str) -> Option<CachedUsage> {
        self.entries.read().await.get(key).cloned()
    }

    pub async fn snapshot(&self) -> HashMap<String, CachedUsage> {
        self.entries.read().await.clone()
    }

    pub async fn insert(&self, key: String, value: Value) {
        let fetched_at_iso = chrono::Utc::now().to_rfc3339();
        let entry = CachedUsage {
            value,
            fetched_at: Instant::now(),
            fetched_at_iso,
        };
        self.entries.write().await.insert(key, entry);
    }

    /// Snapshot only the keys present in the cache (cheap; for the refresh
    /// loop to know which providers to revisit).
    pub async fn keys(&self) -> Vec<String> {
        self.entries.read().await.keys().cloned().collect()
    }

    /// Keys whose entry is older than REFRESH_AFTER (or that don't have an
    /// entry yet — caller may pass `extra_candidates` to seed the list).
    pub async fn stale_keys(&self, extra_candidates: &[String]) -> Vec<String> {
        let map = self.entries.read().await;
        let mut out: Vec<String> = Vec::new();
        for cand in extra_candidates {
            match map.get(cand) {
                Some(entry) if entry.is_fresh() => {}
                _ => out.push(cand.clone()),
            }
        }
        out
    }
}
