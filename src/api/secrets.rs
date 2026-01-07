//! API endpoints for secrets management.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;

use crate::secrets::{
    InitializeKeysResult, InitializeRequest, RegistryInfo, SecretInfo, SecretsStatus,
    SecretsStore, SetSecretRequest, UnlockRequest,
};

use super::routes::AppState;

/// Shared secrets store type.
pub type SharedSecretsStore = Arc<SecretsStore>;

/// Create the secrets API routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/status", get(get_status))
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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    secrets
        .initialize(&req.key_id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/secrets/unlock
/// Unlock the secrets system with a passphrase.
async fn unlock(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnlockRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    secrets
        .unlock(&req.passphrase)
        .await
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/secrets/lock
/// Lock the secrets system (clear passphrase).
async fn lock(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    secrets.lock().await;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// GET /api/secrets/registries
/// List all secret registries.
async fn list_registries(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RegistryInfo>>, (StatusCode, String)> {
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    Ok(Json(secrets.list_registries().await))
}

/// GET /api/secrets/registries/:name
/// List secrets in a registry (metadata only).
async fn list_secrets(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<SecretInfo>>, (StatusCode, String)> {
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    let value = secrets
        .get_secret(&name, &key)
        .await
        .map_err(|e| {
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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

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
    let secrets = state
        .secrets
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Secrets system not available".to_string()))?;

    secrets
        .delete_secret(&name, &key)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(serde_json::json!({ "success": true })))
}
