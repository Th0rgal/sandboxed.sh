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
use std::sync::Arc;
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
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

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
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid URL format".to_string(),
        ));
    }

    let mut connection = OpenCodeConnection::new(req.name, req.base_url);
    connection.agent = req.agent;
    connection.permissive = req.permissive;
    connection.enabled = req.enabled;

    let id = state.opencode_connections.add(connection.clone()).await;

    tracing::info!("Created OpenCode connection: {} ({})", connection.name, id);

    // Refresh the connection to get updated is_default flag
    let updated = state.opencode_connections.get(id).await.unwrap_or(connection);

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
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connection {} not found", id)))
}

/// PUT /api/opencode/connections/:id - Update a connection.
async fn update_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateConnectionRequest>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    let mut connection = state
        .opencode_connections
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connection {} not found", id)))?;

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
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid URL format".to_string(),
            ));
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
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connection {} not found", id)))?;

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
        Err((StatusCode::NOT_FOUND, format!("Connection {} not found", id)))
    }
}

/// POST /api/opencode/connections/:id/test - Test a connection.
async fn test_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<TestConnectionResponse>, (StatusCode, String)> {
    let connection = state
        .opencode_connections
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connection {} not found", id)))?;

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
                let version = resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()));

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
                Err(_) => {
                    Ok(Json(TestConnectionResponse {
                        success: false,
                        message: format!("Connection failed: {}", e),
                        version: None,
                    }))
                }
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
        return Err((StatusCode::NOT_FOUND, format!("Connection {} not found", id)));
    }

    let connection = state
        .opencode_connections
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connection {} not found", id)))?;

    tracing::info!("Set default OpenCode connection: {} ({})", connection.name, id);

    Ok(Json(connection.into()))
}
