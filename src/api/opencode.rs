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
use crate::util::{internal_error, read_json_config, resolve_config_path, write_json_config};

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

/// Resolve the path to opencode.json configuration file.
fn resolve_opencode_config_path() -> std::path::PathBuf {
    resolve_config_path(
        "OPENCODE_CONFIG",
        "OPENCODE_CONFIG_DIR",
        "opencode.json",
        ".config/opencode/opencode.json",
    )
}

/// GET /api/opencode/config - Read opencode.json settings.
pub async fn get_opencode_config() -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_opencode_config_path();

    // Fall back to .jsonc variant if the .json file doesn't exist.
    let read_path = if config_path.exists() {
        config_path.clone()
    } else {
        let jsonc_path = config_path
            .parent()
            .map(|p| p.join("opencode.jsonc"))
            .unwrap_or_else(|| config_path.with_extension("jsonc"));
        if jsonc_path.exists() {
            jsonc_path
        } else {
            return Ok(Json(serde_json::json!({})));
        }
    };

    read_json_config(&read_path, "opencode config")
        .await
        .map(Json)
}

/// PUT /api/opencode/config - Write opencode.json settings.
pub async fn update_opencode_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_opencode_config_path();
    write_json_config(&path, &config, "opencode config").await?;
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

/// Vanilla opencode ships with these primary agents.  They are surfaced in the
/// dashboard's agent dropdown even when the Library defines no `.opencode/agents/`
/// files, so that picking "build" or "plan" Just Works.
const VANILLA_OPENCODE_AGENTS: &[&str] = &["build", "plan"];

/// Fetch agents from Library configuration (no central server needed).
///
/// The returned list merges:
///   1. vanilla opencode's built-in primary agents ("build", "plan"),
///   2. native `.opencode/agents/*.md` files in the Library profile.
pub async fn fetch_opencode_agents(state: &super::routes::AppState) -> Result<Value, String> {
    fetch_opencode_agents_for_profile(state, None).await
}

/// Read native opencode agent names from `<profile>/.opencode/agents/*.md`.
/// Returns the filename stem of each `.md` file (e.g. "ana" from "ana.md").
async fn read_native_agent_names(lib: &crate::library::LibraryStore, profile: &str) -> Vec<String> {
    let agents_dir = lib
        .config_profile_path(profile)
        .join(".opencode")
        .join("agents");
    let mut names: Vec<String> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names
}

/// Fetch agents for a specific config profile.  See [`fetch_opencode_agents`].
pub async fn fetch_opencode_agents_for_profile(
    state: &super::routes::AppState,
    profile: Option<&str>,
) -> Result<Value, String> {
    let library_guard = state.library.read().await;
    let Some(lib) = library_guard.as_ref() else {
        tracing::debug!("Library not configured, no agents available");
        return Ok(Value::Array(
            VANILLA_OPENCODE_AGENTS
                .iter()
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ));
    };

    let profile_name = profile.unwrap_or("default");

    // 1. Start with vanilla opencode's built-in primary agents.
    let mut agents: Vec<String> = VANILLA_OPENCODE_AGENTS
        .iter()
        .map(|s| s.to_string())
        .collect();

    // 2. Merge native `.opencode/agents/*.md` files from the Library profile.
    for name in read_native_agent_names(lib, profile_name).await {
        if !agents.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
            agents.push(name);
        }
    }

    tracing::debug!(
        profile = %profile_name,
        count = agents.len(),
        "Resolved opencode agents"
    );

    Ok(Value::Array(
        agents.into_iter().map(Value::String).collect(),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn not_found_connection(id: Uuid) -> (StatusCode, String) {
    (
        StatusCode::NOT_FOUND,
        format!("Connection {} not found", id),
    )
}

async fn require_connection(
    store: &crate::opencode_config::OpenCodeStore,
    id: Uuid,
) -> Result<OpenCodeConnection, (StatusCode, String)> {
    store.get(id).await.ok_or_else(|| not_found_connection(id))
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
        .map_err(internal_error)?;

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
    let connection = require_connection(&state.opencode_connections, id).await?;
    Ok(Json(connection.into()))
}

/// PUT /api/opencode/connections/:id - Update a connection.
async fn update_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateConnectionRequest>,
) -> Result<Json<ConnectionResponse>, (StatusCode, String)> {
    let mut connection = require_connection(&state.opencode_connections, id).await?;

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
        .ok_or_else(|| not_found_connection(id))?;

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
        Err(not_found_connection(id))
    }
}

/// POST /api/opencode/connections/:id/test - Test a connection.
async fn test_connection(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<TestConnectionResponse>, (StatusCode, String)> {
    let connection = require_connection(&state.opencode_connections, id).await?;

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
        return Err(not_found_connection(id));
    }

    let connection = require_connection(&state.opencode_connections, id).await?;

    tracing::info!(
        "Set default OpenCode connection: {} ({})",
        connection.name,
        id
    );

    Ok(Json(connection.into()))
}
