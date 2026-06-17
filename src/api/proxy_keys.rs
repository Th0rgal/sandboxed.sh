//! Proxy API key management — generate, list, and revoke long-lived API keys
//! for external tools to authenticate against the `/v1` proxy endpoint.
//!
//! Keys are persisted to `{working_dir}/.sandboxed-sh/proxy_api_keys.json`.
//! The internal `SANDBOXED_PROXY_SECRET` (used by mission_runner / OpenCode)
//! continues to work alongside user-generated keys.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::routes::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// A proxy API key record (persisted to disk).
///
/// The raw key value is only returned once at creation time. On disk we store
/// a SHA-256 hash so that a leaked JSON file does not expose usable keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyApiKey {
    pub id: Uuid,
    /// Human-readable label (e.g. "Cursor", "Windsurf", "CI").
    pub name: String,
    /// SHA-256 hex digest of the raw key value.
    pub key_hash: String,
    /// First 8 characters of the raw key for display (e.g. "sk-proxy-a1b2c3d4…").
    pub key_prefix: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last time this key successfully authenticated a proxy request.
    /// `None` for keys that predate usage tracking or were never used.
    #[serde(default)]
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Request body for creating a new key.
#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    /// Human-readable label for the key.
    pub name: String,
}

/// Response returned when a key is created (includes the raw key once).
#[derive(Debug, Serialize)]
pub struct CreateKeyResponse {
    pub id: Uuid,
    pub name: String,
    /// The full API key — shown only at creation time.
    pub key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Summary returned when listing keys (no raw value, just metadata).
#[derive(Debug, Serialize)]
pub struct KeySummary {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<&ProxyApiKey> for KeySummary {
    fn from(k: &ProxyApiKey) -> Self {
        Self {
            id: k.id,
            name: k.name.clone(),
            key_prefix: k.key_prefix.clone(),
            created_at: k.created_at,
            last_used_at: k.last_used_at,
        }
    }
}

/// Request body for cleaning up unused keys.
#[derive(Debug, Deserialize)]
pub struct CleanupRequest {
    /// Keys with no activity for at least this many days are candidates.
    /// Defaults to 7.
    pub max_age_days: Option<u32>,
    /// When true, only report candidates without deleting anything.
    pub dry_run: Option<bool>,
}

/// Response for a cleanup request.
#[derive(Debug, Serialize)]
pub struct CleanupResponse {
    pub dry_run: bool,
    /// Keys whose last activity predates this instant were selected.
    pub cutoff: chrono::DateTime<chrono::Utc>,
    /// The candidate keys (dry run) or the keys that were deleted.
    pub keys: Vec<KeySummary>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Store
// ─────────────────────────────────────────────────────────────────────────────

pub type SharedProxyApiKeyStore = Arc<ProxyApiKeyStore>;

#[derive(Debug)]
pub struct ProxyApiKeyStore {
    keys: RwLock<Vec<ProxyApiKey>>,
    storage_path: PathBuf,
}

impl ProxyApiKeyStore {
    pub async fn new(storage_path: PathBuf) -> Self {
        let store = Self {
            keys: RwLock::new(Vec::new()),
            storage_path,
        };
        if let Ok(loaded) = store.load_from_disk() {
            let mut keys = store.keys.write().await;
            *keys = loaded;
        }
        store
    }

    fn load_from_disk(&self) -> Result<Vec<ProxyApiKey>, std::io::Error> {
        if !self.storage_path.exists() {
            return Ok(Vec::new());
        }
        let contents = std::fs::read_to_string(&self.storage_path)?;
        serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    fn save_to_disk(&self, keys: &[ProxyApiKey]) -> Result<(), std::io::Error> {
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(keys)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp_path = self.storage_path.with_extension("tmp");
        std::fs::write(&tmp_path, &contents)?;
        std::fs::rename(&tmp_path, &self.storage_path)?;
        Ok(())
    }

    /// Create a new API key. Returns the raw key value (only available once).
    pub async fn create(&self, name: String) -> Result<CreateKeyResponse, String> {
        let id = Uuid::new_v4();
        let raw_key = format!("sk-proxy-{}", Uuid::new_v4().as_simple());
        let key_hash = hex_sha256(&raw_key);
        let key_prefix = raw_key[..16].to_string();
        let now = chrono::Utc::now();

        let record = ProxyApiKey {
            id,
            name: name.clone(),
            key_hash,
            key_prefix: key_prefix.clone(),
            created_at: now,
            last_used_at: None,
        };

        let mut keys = self.keys.write().await;
        keys.push(record);
        self.save_to_disk(&keys)
            .map_err(|e| format!("Failed to persist proxy API key: {}", e))?;

        Ok(CreateKeyResponse {
            id,
            name,
            key: raw_key,
            created_at: now,
        })
    }

    /// List all keys (metadata only, no raw values).
    pub async fn list(&self) -> Vec<KeySummary> {
        self.keys
            .read()
            .await
            .iter()
            .map(KeySummary::from)
            .collect()
    }

    /// Delete a key by ID. Returns true if found and removed.
    pub async fn delete(&self, id: Uuid) -> Result<bool, String> {
        let mut keys = self.keys.write().await;
        let len_before = keys.len();
        keys.retain(|k| k.id != id);
        if keys.len() == len_before {
            return Ok(false);
        }
        self.save_to_disk(&keys)
            .map_err(|e| format!("Failed to persist proxy API key deletion: {}", e))?;
        Ok(true)
    }

    /// Check whether a bearer token matches any stored API key (constant-time).
    ///
    /// On a match, records the key's `last_used_at` so unused keys can be
    /// identified and cleaned up later.
    pub async fn verify(&self, token: &str) -> bool {
        let token_hash = hex_sha256(token);
        let mut matched_ids: Vec<Uuid> = Vec::new();
        {
            let keys = self.keys.read().await;
            // Compare against all key hashes to avoid timing leaks on which key
            // matched. We still iterate all entries even after a match.
            for key in keys.iter() {
                if super::auth::constant_time_eq(&token_hash, &key.key_hash) {
                    matched_ids.push(key.id);
                }
            }
        }
        if matched_ids.is_empty() {
            return false;
        }
        self.touch(&matched_ids).await;
        true
    }

    /// Find the stored key matching a raw token, without recording usage.
    ///
    /// Used by infrastructure that re-reads its own provisioned key (e.g. the
    /// Hermes gateway env) to check it is still valid before reusing it.
    pub async fn find_by_token(&self, token: &str) -> Option<KeySummary> {
        let token_hash = hex_sha256(token);
        let keys = self.keys.read().await;
        let mut found = None;
        for key in keys.iter() {
            if super::auth::constant_time_eq(&token_hash, &key.key_hash) {
                found = Some(KeySummary::from(key));
            }
        }
        found
    }

    /// Delete every key whose name starts with `prefix`, except `keep`.
    /// Returns the deleted keys. Used to garbage-collect keys leaked by
    /// repeated provisioning runs (e.g. one "Hermes Assistant …" key per
    /// adopt invocation).
    pub async fn delete_named_except(
        &self,
        prefix: &str,
        keep: Option<Uuid>,
    ) -> Result<Vec<KeySummary>, String> {
        let doomed = |k: &ProxyApiKey| k.name.starts_with(prefix) && keep != Some(k.id);
        let mut keys = self.keys.write().await;
        let deleted: Vec<KeySummary> = keys
            .iter()
            .filter(|k| doomed(k))
            .map(KeySummary::from)
            .collect();
        if deleted.is_empty() {
            return Ok(deleted);
        }
        keys.retain(|k| !doomed(k));
        self.save_to_disk(&keys)
            .map_err(|e| format!("Failed to persist proxy API key cleanup: {}", e))?;
        Ok(deleted)
    }

    /// Record usage for the given keys. Always updates in-memory; persists to
    /// disk at most once per minute per key so the hot proxy path doesn't pay
    /// a disk write on every request.
    async fn touch(&self, ids: &[Uuid]) {
        let now = chrono::Utc::now();
        let mut keys = self.keys.write().await;
        let mut persist = false;
        for key in keys.iter_mut().filter(|k| ids.contains(&k.id)) {
            if key
                .last_used_at
                .is_none_or(|t| now - t >= chrono::Duration::seconds(60))
            {
                persist = true;
            }
            key.last_used_at = Some(now);
        }
        if persist {
            if let Err(e) = self.save_to_disk(&keys) {
                tracing::warn!("Failed to persist proxy key usage timestamp: {}", e);
            }
        }
    }

    /// Find keys whose last activity is older than `cutoff`, optionally
    /// deleting them. A key's last activity is `last_used_at`, falling back to
    /// `created_at` for keys that have never authenticated a request (or
    /// predate usage tracking).
    pub async fn cleanup_unused(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
        dry_run: bool,
    ) -> Result<Vec<KeySummary>, String> {
        let is_stale =
            |k: &ProxyApiKey| -> bool { k.last_used_at.unwrap_or(k.created_at) < cutoff };
        let mut keys = self.keys.write().await;
        let stale: Vec<KeySummary> = keys
            .iter()
            .filter(|k| is_stale(k))
            .map(KeySummary::from)
            .collect();
        if dry_run || stale.is_empty() {
            return Ok(stale);
        }
        keys.retain(|k| !is_stale(k));
        self.save_to_disk(&keys)
            .map_err(|e| format!("Failed to persist proxy API key cleanup: {}", e))?;
        Ok(stale)
    }
}

fn hex_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ─────────────────────────────────────────────────────────────────────────────
// API Handlers
// ─────────────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_keys))
        .route("/", post(create_key))
        .route("/cleanup", post(cleanup_keys))
        .route("/:id", delete(delete_key))
}

async fn list_keys(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<KeySummary>>, StatusCode> {
    Ok(Json(state.proxy_api_keys.list().await))
}

async fn create_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<CreateKeyResponse>), (StatusCode, String)> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Name is required".to_string()));
    }
    match state.proxy_api_keys.create(name).await {
        Ok(resp) => Ok((StatusCode::CREATED, Json(resp))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    match state.proxy_api_keys.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Delete (or, with `dry_run`, list) keys that have seen no activity for
/// `max_age_days` days (default 7).
async fn cleanup_keys(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CleanupRequest>,
) -> Result<Json<CleanupResponse>, (StatusCode, String)> {
    let max_age_days = req.max_age_days.unwrap_or(7);
    if max_age_days == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "max_age_days must be at least 1".to_string(),
        ));
    }
    let dry_run = req.dry_run.unwrap_or(false);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(max_age_days));
    match state.proxy_api_keys.cleanup_unused(cutoff, dry_run).await {
        Ok(keys) => Ok(Json(CleanupResponse {
            dry_run,
            cutoff,
            keys,
        })),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn key(name: &str, created_days_ago: i64, used_days_ago: Option<i64>) -> ProxyApiKey {
        let now = chrono::Utc::now();
        ProxyApiKey {
            id: Uuid::new_v4(),
            name: name.to_string(),
            key_hash: hex_sha256(name),
            key_prefix: "sk-proxy-test0000".to_string(),
            created_at: now - chrono::Duration::days(created_days_ago),
            last_used_at: used_days_ago.map(|d| now - chrono::Duration::days(d)),
        }
    }

    async fn store_with(keys: Vec<ProxyApiKey>) -> (ProxyApiKeyStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = ProxyApiKeyStore::new(tmp.path().join("proxy_api_keys.json")).await;
        *store.keys.write().await = keys;
        (store, tmp)
    }

    #[tokio::test]
    async fn cleanup_selects_by_last_activity_with_created_fallback() {
        let (store, _tmp) = store_with(vec![
            // Old but recently used — kept.
            key("old-active", 100, Some(1)),
            // Old and last used long ago — stale.
            key("old-stale", 100, Some(30)),
            // Never used, created long ago — stale via created_at fallback.
            key("old-never-used", 30, None),
            // Never used but recent — kept.
            key("fresh", 2, None),
        ])
        .await;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

        // Dry run reports candidates without deleting.
        let candidates = store.cleanup_unused(cutoff, true).await.unwrap();
        let names: Vec<_> = candidates.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, vec!["old-stale", "old-never-used"]);
        assert_eq!(store.list().await.len(), 4);

        // Real run deletes them and persists.
        let deleted = store.cleanup_unused(cutoff, false).await.unwrap();
        assert_eq!(deleted.len(), 2);
        let remaining: Vec<_> = store.list().await.into_iter().map(|k| k.name).collect();
        assert_eq!(remaining, vec!["old-active", "fresh"]);
        let on_disk = store.load_from_disk().unwrap();
        assert_eq!(on_disk.len(), 2);
    }

    #[tokio::test]
    async fn verify_records_last_used() {
        let (store, _tmp) = store_with(vec![ProxyApiKey {
            id: Uuid::new_v4(),
            name: "k".to_string(),
            key_hash: hex_sha256("sk-proxy-raw"),
            key_prefix: "sk-proxy-raw00000".to_string(),
            created_at: chrono::Utc::now() - chrono::Duration::days(10),
            last_used_at: None,
        }])
        .await;

        assert!(!store.verify("wrong-token").await);
        assert!(store.list().await[0].last_used_at.is_none());

        assert!(store.verify("sk-proxy-raw").await);
        assert!(store.list().await[0].last_used_at.is_some());
        // The first touch persists to disk.
        assert!(store.load_from_disk().unwrap()[0].last_used_at.is_some());
    }

    #[tokio::test]
    async fn find_by_token_does_not_record_usage() {
        let (store, _tmp) = store_with(vec![ProxyApiKey {
            id: Uuid::new_v4(),
            name: "k".to_string(),
            key_hash: hex_sha256("sk-proxy-raw"),
            key_prefix: "sk-proxy-raw00000".to_string(),
            created_at: chrono::Utc::now(),
            last_used_at: None,
        }])
        .await;

        assert!(store.find_by_token("nope").await.is_none());
        let found = store.find_by_token("sk-proxy-raw").await.unwrap();
        assert_eq!(found.name, "k");
        assert!(store.list().await[0].last_used_at.is_none());
    }

    #[tokio::test]
    async fn delete_named_except_keeps_the_kept_key_and_other_names() {
        let keep = key("Hermes Assistant (prod)", 1, None);
        let keep_id = keep.id;
        let (store, _tmp) = store_with(vec![
            key("Hermes Assistant 2026-05-29T13:02:50+00:00", 6, None),
            key("Hermes Assistant 2026-05-30T08:15:23+00:00", 5, None),
            keep,
            // Different prefix — must survive.
            key("Hermes debug smoke", 6, None),
            key("Cursor", 90, None),
        ])
        .await;

        let deleted = store
            .delete_named_except("Hermes Assistant", Some(keep_id))
            .await
            .unwrap();
        assert_eq!(deleted.len(), 2);

        let remaining: Vec<_> = store.list().await.into_iter().map(|k| k.name).collect();
        assert_eq!(
            remaining,
            vec!["Hermes Assistant (prod)", "Hermes debug smoke", "Cursor"]
        );
        // Persisted.
        assert_eq!(store.load_from_disk().unwrap().len(), 3);
    }
}
