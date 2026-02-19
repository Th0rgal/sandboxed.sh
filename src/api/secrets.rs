//! API endpoints for secrets management.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use serde::{Deserialize, Serialize};

use crate::library::env_crypto;
use crate::secrets::{
    InitializeKeysResult, InitializeRequest, RegistryInfo, SecretInfo, SecretsStatus, SecretsStore,
    SetSecretRequest, UnlockRequest,
};
use crate::util::internal_error;

use super::routes::AppState;

/// Shared secrets store type.
pub type SharedSecretsStore = Arc<SecretsStore>;

/// Create the secrets API routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(get_status))
        .route("/encryption", get(get_encryption_status))
        .route("/encryption/key", get(get_private_key))
        .route("/encryption/key", put(set_private_key))
        .route("/initialize", post(initialize))
        .route("/unlock", post(unlock))
        .route("/lock", post(lock))
        .route("/registries", get(list_registries))
        .route("/registries/:name", get(list_secrets))
        .route("/registries/:name", delete(delete_registry))
        .route("/registries/:name/:key", get(get_secret))
        .route("/registries/:name/:key", post(set_secret))
        .route("/registries/:name/:key", delete(delete_secret))
        .route("/registries/:name/:key/reveal", get(reveal_secret))
}

/// Response for encryption status.
#[derive(Debug, Serialize)]
pub struct EncryptionStatus {
    pub key_available: bool,
    pub key_source: Option<String>,
    pub key_file_path: Option<String>,
}

/// Response for get private key (hex-encoded).
#[derive(Debug, Serialize)]
pub struct PrivateKeyResponse {
    pub key_hex: Option<String>,
    pub key_source: Option<String>,
}

/// Request to set/update private key.
#[derive(Debug, Deserialize)]
pub struct SetPrivateKeyRequest {
    pub key_hex: String,
}

/// Response for set private key.
#[derive(Debug, Serialize)]
pub struct SetPrivateKeyResponse {
    pub success: bool,
    pub message: String,
    pub reencrypted_count: usize,
    pub failed_count: usize,
}

/// GET /api/secrets/encryption
/// Get the status of skill content encryption (PRIVATE_KEY).
async fn get_encryption_status(State(state): State<Arc<AppState>>) -> Json<EncryptionStatus> {
    // Check if key is available from environment
    if let Ok(Some(_)) = env_crypto::load_private_key_from_env() {
        return Json(EncryptionStatus {
            key_available: true,
            key_source: Some("environment".to_string()),
            key_file_path: None,
        });
    }

    // Check if key file exists
    let key_file = state
        .config
        .working_dir
        .join(".sandboxed-sh")
        .join("private_key");
    if key_file.exists() {
        // Try to read and validate the key
        if let Ok(contents) = tokio::fs::read_to_string(&key_file).await {
            if !contents.trim().is_empty() {
                return Json(EncryptionStatus {
                    key_available: true,
                    key_source: Some("file".to_string()),
                    key_file_path: Some(key_file.display().to_string()),
                });
            }
        }
    }

    Json(EncryptionStatus {
        key_available: false,
        key_source: None,
        key_file_path: None,
    })
}

/// GET /api/secrets/encryption/key
/// Get the current private key (hex-encoded).
async fn get_private_key(State(state): State<Arc<AppState>>) -> Json<PrivateKeyResponse> {
    // Check environment variable first
    if let Some(key_hex) = env_crypto::get_private_key_hex() {
        return Json(PrivateKeyResponse {
            key_hex: Some(key_hex),
            key_source: Some("environment".to_string()),
        });
    }

    // Check key file
    let key_file = state
        .config
        .working_dir
        .join(".sandboxed-sh")
        .join("private_key");
    if key_file.exists() {
        if let Ok(contents) = tokio::fs::read_to_string(&key_file).await {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Json(PrivateKeyResponse {
                    key_hex: Some(trimmed.to_string()),
                    key_source: Some("file".to_string()),
                });
            }
        }
    }

    Json(PrivateKeyResponse {
        key_hex: None,
        key_source: None,
    })
}

/// PUT /api/secrets/encryption/key
/// Set or update the private key, re-encrypting existing skill content.
async fn set_private_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetPrivateKeyRequest>,
) -> Result<Json<SetPrivateKeyResponse>, (StatusCode, String)> {
    let new_key_hex = req.key_hex.trim();

    // Validate the new key format
    let new_key = env_crypto::parse_key_hex(new_key_hex).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid key format: {}", e),
        )
    })?;

    // Get the old key (if any) for re-encryption
    let old_key = env_crypto::load_private_key_from_env()
        .ok()
        .flatten()
        .or_else(|| {
            // Try reading from file
            let key_file = state
                .config
                .working_dir
                .join(".sandboxed-sh")
                .join("private_key");
            if key_file.exists() {
                std::fs::read_to_string(&key_file)
                    .ok()
                    .and_then(|s| env_crypto::parse_key_hex(s.trim()).ok())
            } else {
                None
            }
        });

    // Re-encrypt library content if we have both old and new keys
    let (reencrypted_count, failed_count) = if let Some(old_key) = old_key {
        if old_key != new_key {
            // Re-encrypt all skills in the library
            reencrypt_library_skills(&state, &old_key, &new_key).await
        } else {
            // Same key, no re-encryption needed
            (0, 0)
        }
    } else {
        // No old key, nothing to re-encrypt
        (0, 0)
    };

    // Save the new key
    env_crypto::set_private_key_hex(new_key_hex)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save key: {}", e),
            )
        })?;

    let message = if reencrypted_count > 0 || failed_count > 0 {
        format!(
            "Key updated. Re-encrypted {} items, {} failed.",
            reencrypted_count, failed_count
        )
    } else {
        "Key updated successfully.".to_string()
    };

    Ok(Json(SetPrivateKeyResponse {
        success: true,
        message,
        reencrypted_count,
        failed_count,
    }))
}

/// Re-encrypt all skill content in the library with a new key.
async fn reencrypt_library_skills(
    state: &Arc<AppState>,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> (usize, usize) {
    let library_guard = state.library.read().await;
    let Some(library) = library_guard.as_ref() else {
        return (0, 0);
    };

    let mut reencrypted = 0;
    let mut failed = 0;

    // Get all skills
    let skills = match library.list_skills().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Failed to list skills for re-encryption: {}", e);
            return (0, 0);
        }
    };

    for skill_summary in skills {
        // Read the skill content
        let skill = match library.get_skill(&skill_summary.name).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Failed to read skill {} for re-encryption: {}",
                    skill_summary.name,
                    e
                );
                failed += 1;
                continue;
            }
        };

        // Check if content has encrypted tags
        if !env_crypto::has_encrypted_tags(&skill.content) {
            continue;
        }

        // Try to decrypt with old key and re-encrypt with new key
        let decrypted = match env_crypto::decrypt_content_tags(old_key, &skill.content) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "Failed to decrypt skill {} with old key: {}",
                    skill_summary.name,
                    e
                );
                failed += 1;
                continue;
            }
        };

        // Re-encrypt with new key
        let reencrypted_content = match env_crypto::encrypt_content_tags(new_key, &decrypted) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Failed to re-encrypt skill {} with new key: {}",
                    skill_summary.name,
                    e
                );
                failed += 1;
                continue;
            }
        };

        // Save the re-encrypted content
        if let Err(e) = library
            .save_skill(&skill_summary.name, &reencrypted_content)
            .await
        {
            tracing::warn!(
                "Failed to save re-encrypted skill {}: {}",
                skill_summary.name,
                e
            );
            failed += 1;
            continue;
        }

        reencrypted += 1;
        tracing::info!("Re-encrypted skill: {}", skill_summary.name);
    }

    (reencrypted, failed)
}

/// GET /api/secrets/status
/// Get the status of the secrets system.
async fn get_status(State(state): State<Arc<AppState>>) -> Json<SecretsStatus> {
    let Some(secrets) = &state.secrets else {
        return Json(SecretsStatus {
            initialized: false,
            can_decrypt: false,
            registries: vec![],
            default_key: None,
        });
    };

    Json(secrets.status().await)
}

/// POST /api/secrets/initialize
/// Initialize the secrets system with a new key.
async fn initialize(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InitializeRequest>,
) -> Result<Json<InitializeKeysResult>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .initialize(&req.key_id)
        .await
        .map(Json)
        .map_err(internal_error)
}

/// POST /api/secrets/unlock
/// Unlock the secrets system with a passphrase.
async fn unlock(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnlockRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .unlock(&req.passphrase)
        .await
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/secrets/lock
/// Lock the secrets system (clear passphrase).
async fn lock(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets.lock().await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// GET /api/secrets/registries
/// List all secret registries.
async fn list_registries(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RegistryInfo>>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    Ok(Json(secrets.list_registries().await))
}

/// GET /api/secrets/registries/:name
/// List secrets in a registry (metadata only).
async fn list_secrets(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<SecretInfo>>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .list_secrets(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

/// DELETE /api/secrets/registries/:name
/// Delete a registry and all its secrets.
async fn delete_registry(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .delete_registry(&name)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// Path parameters for secret operations.
#[derive(Deserialize)]
struct SecretPath {
    name: String,
    key: String,
}

/// GET /api/secrets/registries/:name/:key
/// Get secret metadata (not the value).
async fn get_secret(
    State(state): State<Arc<AppState>>,
    Path(SecretPath { name, key }): Path<SecretPath>,
) -> Result<Json<SecretInfo>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    let list = secrets
        .list_secrets(&name)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    list.into_iter()
        .find(|s| s.key == key)
        .map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("Secret not found: {}", key)))
}

/// GET /api/secrets/registries/:name/:key/reveal
/// Reveal (decrypt) a secret value.
async fn reveal_secret(
    State(state): State<Arc<AppState>>,
    Path(SecretPath { name, key }): Path<SecretPath>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    let value = secrets.get_secret(&name, &key).await.map_err(|e| {
        if e.to_string().contains("locked") {
            (StatusCode::UNAUTHORIZED, e.to_string())
        } else {
            (StatusCode::NOT_FOUND, e.to_string())
        }
    })?;

    Ok(Json(serde_json::json!({ "value": value })))
}

/// POST /api/secrets/registries/:name/:key
/// Set (create or update) a secret.
async fn set_secret(
    State(state): State<Arc<AppState>>,
    Path(SecretPath { name, key }): Path<SecretPath>,
    Json(req): Json<SetSecretRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .set_secret(&name, &key, &req.value, req.metadata)
        .await
        .map_err(|e| {
            if e.to_string().contains("locked") {
                (StatusCode::UNAUTHORIZED, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// DELETE /api/secrets/registries/:name/:key
/// Delete a secret.
async fn delete_secret(
    State(state): State<Arc<AppState>>,
    Path(SecretPath { name, key }): Path<SecretPath>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state.secrets.as_ref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Secrets system not available".to_string(),
    ))?;

    secrets
        .delete_secret(&name, &key)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}
