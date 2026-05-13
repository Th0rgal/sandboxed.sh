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

const MAX_PROXY_API_KEYS: usize = 128;
const MAX_PROXY_KEY_NAME_LEN: usize = 128;

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
        let mut keys: Vec<ProxyApiKey> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        sanitize_loaded_keys(&mut keys);
        Ok(keys)
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
        let name = validate_key_name(&name)?;
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
        };

        let mut keys = self.keys.write().await;
        if keys.len() >= MAX_PROXY_API_KEYS {
            return Err(format!(
                "Maximum number of proxy API keys reached ({})",
                MAX_PROXY_API_KEYS
            ));
        }
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
            .map(|k| KeySummary {
                id: k.id,
                name: k.name.clone(),
                key_prefix: k.key_prefix.clone(),
                created_at: k.created_at,
            })
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
    pub async fn verify(&self, token: &str) -> bool {
        let token_hash = hex_sha256(token);
        let keys = self.keys.read().await;
        // Compare against all key hashes to avoid timing leaks on which key
        // matched. We still iterate all entries even after a match.
        let mut matched = false;
        for key in keys.iter() {
            if super::auth::constant_time_eq(&token_hash, &key.key_hash) {
                matched = true;
            }
        }
        matched
    }
}

fn hex_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn validate_key_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Name is required".to_string());
    }
    if trimmed.chars().count() > MAX_PROXY_KEY_NAME_LEN {
        return Err(format!(
            "Name is too long (max {} characters)",
            MAX_PROXY_KEY_NAME_LEN
        ));
    }
    if trimmed.contains('\0') {
        return Err("Name cannot contain NUL bytes".to_string());
    }
    Ok(trimmed.to_string())
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sanitize_loaded_keys(keys: &mut Vec<ProxyApiKey>) {
    keys.retain(|key| {
        key.name.chars().count() <= MAX_PROXY_KEY_NAME_LEN
            && !key.name.trim().is_empty()
            && !key.name.contains('\0')
            && valid_hash(&key.key_hash)
            && key.key_prefix.starts_with("sk-proxy-")
            && key.key_prefix.len() <= 32
    });
    keys.sort_by_key(|key| key.created_at);
    if keys.len() > MAX_PROXY_API_KEYS {
        let drop_count = keys.len() - MAX_PROXY_API_KEYS;
        keys.drain(0..drop_count);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// API Handlers
// ─────────────────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_keys))
        .route("/", post(create_key))
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
    match state.proxy_api_keys.create(req.name).await {
        Ok(resp) => Ok((StatusCode::CREATED, Json(resp))),
        Err(e) if e.contains("Name") => Err((StatusCode::BAD_REQUEST, e)),
        Err(e) if e.contains("Maximum number") => Err((StatusCode::CONFLICT, e)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn proxy_key_store_rejects_empty_and_long_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ProxyApiKeyStore::new(temp.path().join("proxy_keys.json")).await;

        let err = store
            .create("   ".to_string())
            .await
            .expect_err("empty names should be rejected");
        assert!(err.contains("required"));

        let err = store
            .create("x".repeat(MAX_PROXY_KEY_NAME_LEN + 1))
            .await
            .expect_err("long names should be rejected");
        assert!(err.contains("too long"));
    }

    #[tokio::test]
    async fn proxy_key_store_enforces_capacity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ProxyApiKeyStore::new(temp.path().join("proxy_keys.json")).await;

        for idx in 0..MAX_PROXY_API_KEYS {
            store
                .create(format!("key-{idx}"))
                .await
                .expect("key should be created below capacity");
        }

        let err = store
            .create("overflow".to_string())
            .await
            .expect_err("capacity overflow should be rejected");
        assert!(err.contains("Maximum number"));
    }

    #[tokio::test]
    async fn proxy_key_store_sanitizes_loaded_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("proxy_keys.json");
        let now = chrono::Utc::now();
        let mut keys = Vec::new();
        keys.push(ProxyApiKey {
            id: Uuid::new_v4(),
            name: "valid".to_string(),
            key_hash: "a".repeat(64),
            key_prefix: "sk-proxy-1234567".to_string(),
            created_at: now,
        });
        keys.push(ProxyApiKey {
            id: Uuid::new_v4(),
            name: "\0bad".to_string(),
            key_hash: "b".repeat(64),
            key_prefix: "sk-proxy-1234567".to_string(),
            created_at: now,
        });
        keys.push(ProxyApiKey {
            id: Uuid::new_v4(),
            name: "bad-hash".to_string(),
            key_hash: "not-hex".to_string(),
            key_prefix: "sk-proxy-1234567".to_string(),
            created_at: now,
        });
        std::fs::write(&path, serde_json::to_string(&keys).expect("json")).expect("write");

        let store = ProxyApiKeyStore::new(path).await;
        let loaded = store.list().await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "valid");
    }
}
