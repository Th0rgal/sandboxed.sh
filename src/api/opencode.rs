//! OpenCode connection management API endpoints.
//!
//! Provides endpoints for managing OpenCode server connections:
//! - List connections
//! - Create connection
//! - Get connection details
//! - Update connection
//! - Delete connection
//! - Test connection
//! - Set default connection

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::opencode_config::OpenCodeConnection;

/// Create OpenCode connection routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_connections))
        .route("/", post(create_connection))
        .route("/:id", get(get_connection))
        .route("/:id", put(update_connection))
        .route("/:id", delete(delete_connection))
        .route("/:id/test", post(test_connection))
        .route("/:id/default", post(set_default))
}

/// Resolve the path to oh-my-opencode.json configuration file.
fn resolve_oh_my_opencode_path() -> std::path::PathBuf {
    // Check OPENCODE_CONFIG_DIR first
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir).join("oh-my-opencode.json");
        }
    }
    // Fall back to ~/.config/opencode/oh-my-opencode.json
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home)
        .join(".config")
        .join("opencode")
        .join("oh-my-opencode.json")
}

/// GET /api/opencode/settings - Read oh-my-opencode settings.
pub async fn get_opencode_settings() -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_oh_my_opencode_path();

    if !config_path.exists() {
        // Return empty object if file doesn't exist
        return Ok(Json(serde_json::json!({})));
    }

    let contents = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read oh-my-opencode.json: {}", e),
        )
    })?;

    let config: Value = serde_json::from_str(&contents).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Invalid JSON in oh-my-opencode.json: {}", e),
        )
    })?;

    Ok(Json(config))
}

/// PUT /api/opencode/settings - Write oh-my-opencode settings.
pub async fn update_opencode_settings(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_oh_my_opencode_path();

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create config directory: {}", e),
            )
        })?;
    }

    // Write the config
    let contents = serde_json::to_string_pretty(&config)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    tokio::fs::write(&config_path, contents)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write oh-my-opencode.json: {}", e),
            )
        })?;

    tracing::info!(path = %config_path.display(), "Updated oh-my-opencode settings");

    Ok(Json(config))
}

/// POST /api/opencode/restart - Restart the OpenCode service.
pub async fn restart_opencode_service() -> Result<Json<Value>, (StatusCode, String)> {
    tracing::info!("Restarting OpenCode service...");

    let output = tokio::process::Command::new("systemctl")
        .args(["restart", "opencode.service"])
        .output()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to execute systemctl: {}", e),
            )
        })?;

    if output.status.success() {
        tracing::info!("OpenCode service restarted successfully");
        Ok(Json(serde_json::json!({
            "success": true,
            "message": "OpenCode service restarted successfully"
        })))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("Failed to restart OpenCode service: {}", stderr);
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to restart OpenCode service: {}", stderr),
        ))
    }
}

const AGENTS_CACHE_TTL: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
pub struct OpenCodeAgentsCache {
    pub fetched_at: Option<Instant>,
    pub payload: Option<Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Request/Response Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateConnectionRequest {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "default_true")]
    pub permissive: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateConnectionRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub agent: Option<Option<String>>,
    pub permissive: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ConnectionResponse {
    pub id: Uuid,
    pub name: String,
    pub base_url: String,
    pub agent: Option<String>,
    pub permissive: bool,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<OpenCodeConnection> for ConnectionResponse {
    fn from(c: OpenCodeConnection) -> Self {
        Self {
            id: c.id,
            name: c.name,
            base_url: c.base_url,
            agent: c.agent,
            permissive: c.permissive,
            enabled: c.enabled,
            is_default: c.is_default,
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TestConnectionResponse {
    pub success: bool,
    pub message: String,
    pub version: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Fetch agents from Library configuration (no central server needed).
/// Reads agents from Library's oh-my-opencode.json, falling back to defaults.
pub async fn fetch_opencode_agents(state: &super::routes::AppState) -> Result<Value, String> {
    // Try to read agents from Library's oh-my-opencode.json
    let library_guard = state.library.read().await;
    if let Some(lib) = library_guard.as_ref() {
        match lib.get_opencode_settings().await {
            Ok(settings) => {
                // Extract agents from the settings
                if let Some(agents) = settings.get("agents") {
                    if agents.is_object() && !agents.as_object().unwrap().is_empty() {
                        // Return agent names as array
                        let agent_names: Vec<Value> = agents
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|k| Value::String(k.clone()))
                            .collect();
                        tracing::debug!("Loaded {} agents from Library", agent_names.len());
                        return Ok(Value::Array(agent_names));
                    }
                }
                tracing::debug!("No agents in Library oh-my-opencode.json");
            }
            Err(e) => {
                tracing::warn!("Failed to read Library opencode settings: {}", e);
            }
        }
    } else {
        tracing::debug!("Library not configured, no agents available");
    }

    // No hardcoded fallback — Library is the source of truth
    Ok(Value::Array(vec![]))
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/opencode/agents - Return OpenCode agent list from Library.
pub async fn list_agents(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Check cache first
    let now = Instant::now();
    if let Some(payload) = {
        let cache = state.opencode_agents_cache.read().await;
        if let (Some(payload), Some(fetched_at)) = (&cache.payload, cache.fetched_at) {
            if now.duration_since(fetched_at) < AGENTS_CACHE_TTL {
                Some(payload.clone())
            } else {
                None
            }
        } else {
            None
        }
    } {
        return Ok(Json(payload));
    }

    // Fetch from Library (no HTTP call needed)
    let payload = fetch_opencode_agents(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Update cache
    {
        let mut cache = state.opencode_agents_cache.write().await;
        cache.payload = Some(payload.clone());
        cache.fetched_at = Some(Instant::now());
    }

    Ok(Json(payload))
}

/// GET /api/opencode/connections - List all connections.
async fn list_connections(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<Vec<ConnectionResponse>>, (StatusCode, String)> {
    let connections = state.opencode_connections.list().await;
    let responses: Vec<ConnectionResponse> = connections.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// POST /api/opencode/connections - Create a new connection.
async fn create_connection(
    State(state): State<Arc<super::routes::AppState>>,
    Json(req): Json<CreateConnectionRequest>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    if req.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
    }

    if req.base_url.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Base URL cannot be empty".to_string(),
        ));
    }

    // Validate URL format
    if url::Url::parse(&req.base_url).is_err() {
        return Err((StatusCode::BAD_REQUEST, "Invalid URL format".to_string()));
    }

    let mut connection = OpenCodeConnection::new(req.name, req.base_url);
    connection.agent = req.agent;
    connection.permissive = req.permissive;
    connection.enabled = req.enabled;

    let id = state.opencode_connections.add(connection.clone()).await;

    tracing::info!("Created OpenCode connection: {} ({})", connection.name, id);

    // Refresh the connection to get updated is_default flag
    let updated = state
        .opencode_connections
        .get(id)
        .await
        .unwrap_or(connection);

    Ok(Json(updated.into()))
}

/// GET /api/opencode/connections/:id - Get connection details.
async fn get_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    state
        .opencode_connections
        .get(id)
        .await
        .map(|c| Json(c.into()))
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Connection {} not found", id),
            )
        })
}

/// PUT /api/opencode/connections/:id - Update a connection.
async fn update_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateConnectionRequest>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    let mut connection = state.opencode_connections.get(id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Connection {} not found", id),
        )
    })?;

    if let Some(name) = req.name {
        if name.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
        }
        connection.name = name;
    }

    if let Some(base_url) = req.base_url {
        if base_url.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Base URL cannot be empty".to_string(),
            ));
        }
        if url::Url::parse(&base_url).is_err() {
            return Err((StatusCode::BAD_REQUEST, "Invalid URL format".to_string()));
        }
        connection.base_url = base_url;
    }

    if let Some(agent) = req.agent {
        connection.agent = agent;
    }

    if let Some(permissive) = req.permissive {
        connection.permissive = permissive;
    }

    if let Some(enabled) = req.enabled {
        connection.enabled = enabled;
    }

    let updated = state
        .opencode_connections
        .update(id, connection)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Connection {} not found", id),
            )
        })?;

    tracing::info!("Updated OpenCode connection: {} ({})", updated.name, id);

    Ok(Json(updated.into()))
}

/// DELETE /api/opencode/connections/:id - Delete a connection.
async fn delete_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    if state.opencode_connections.delete(id).await {
        Ok((
            StatusCode::OK,
            format!("Connection {} deleted successfully", id),
        ))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("Connection {} not found", id),
        ))
    }
}

/// POST /api/opencode/connections/:id/test - Test a connection.
async fn test_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<TestConnectionResponse>, (StatusCode, String)> {
    let connection = state.opencode_connections.get(id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Connection {} not found", id),
        )
    })?;

    // Try to connect to the OpenCode server
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // Try health endpoint first, then session endpoint
    let health_url = format!("{}/health", connection.base_url);

    match client.get(&health_url).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                // Try to parse version from response
                let version = resp.json::<serde_json::Value>().await.ok().and_then(|v| {
                    v.get("version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });

                Ok(Json(TestConnectionResponse {
                    success: true,
                    message: "Connection successful".to_string(),
                    version,
                }))
            } else {
                Ok(Json(TestConnectionResponse {
                    success: false,
                    message: format!("Server returned status: {}", resp.status()),
                    version: None,
                }))
            }
        }
        Err(e) => {
            // Try session endpoint as fallback (some OpenCode servers don't have /health)
            let session_url = format!("{}/session", connection.base_url);
            match client.get(&session_url).send().await {
                Ok(_resp) => {
                    // Even a 4xx response means the server is reachable
                    Ok(Json(TestConnectionResponse {
                        success: true,
                        message: "Connection successful (via session endpoint)".to_string(),
                        version: None,
                    }))
                }
                Err(_) => Ok(Json(TestConnectionResponse {
                    success: false,
                    message: format!("Connection failed: {}", e),
                    version: None,
                })),
            }
        }
    }
}

/// POST /api/opencode/connections/:id/default - Set as default connection.
async fn set_default(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    if !state.opencode_connections.set_default(id).await {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Connection {} not found", id),
        ));
    }

    let connection = state.opencode_connections.get(id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Connection {} not found", id),
        )
    })?;

    tracing::info!(
        "Set default OpenCode connection: {} ({})",
        connection.name,
        id
    );

    Ok(Json(connection.into()))
}
