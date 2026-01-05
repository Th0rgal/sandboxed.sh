//! Library management API endpoints.
//!
//! Provides endpoints for managing the configuration library:
//! - Git operations (status, sync, commit, push)
//! - MCP server CRUD
//! - Skills CRUD
//! - Commands CRUD

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::library::{
    Command, CommandSummary, LibraryStatus, LibraryStore, McpServer, Skill, SkillSummary,
};

/// Shared library state.
pub type SharedLibrary = Arc<RwLock<Option<Arc<LibraryStore>>>>;

const LIBRARY_REMOTE_HEADER: &str = "x-openagent-library-remote";

fn extract_library_remote(headers: &HeaderMap) -> Option<String> {
    headers
        .get(LIBRARY_REMOTE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

async fn ensure_library(
    state: &super::routes::AppState,
    headers: &HeaderMap,
) -> Result<Arc<LibraryStore>, (StatusCode, String)> {
    let remote = extract_library_remote(headers).or_else(|| state.config.library_remote.clone());
    let remote = remote.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Library not configured. Set a Git repo in Settings.".to_string(),
        )
    })?;

    {
        let library_guard = state.library.read().await;
        if let Some(library) = library_guard.as_ref() {
            if library.remote() == remote {
                return Ok(Arc::clone(library));
            }
        }
    }

    let mut library_guard = state.library.write().await;
    if let Some(library) = library_guard.as_ref() {
        if library.remote() == remote {
            return Ok(Arc::clone(library));
        }
    }

    match LibraryStore::new(state.config.library_path.clone(), &remote).await {
        Ok(store) => {
            let store = Arc::new(store);
            *library_guard = Some(Arc::clone(&store));
            Ok(store)
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to initialize library: {}", e),
        )),
    }
}

/// Create library routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        // Git operations
        .route("/status", get(get_status))
        .route("/sync", post(sync_library))
        .route("/commit", post(commit_library))
        .route("/push", post(push_library))
        // MCP servers
        .route("/mcps", get(get_mcps))
        .route("/mcps", put(save_mcps))
        // Skills
        .route("/skills", get(list_skills))
        .route("/skills/:name", get(get_skill))
        .route("/skills/:name", put(save_skill))
        .route("/skills/:name", delete(delete_skill))
        .route("/skills/:name/references/*path", get(get_skill_reference))
        .route("/skills/:name/references/*path", put(save_skill_reference))
        // Commands
        .route("/commands", get(list_commands))
        .route("/commands/:name", get(get_command))
        .route("/commands/:name", put(save_command))
        .route("/commands/:name", delete(delete_command))
}

// ─────────────────────────────────────────────────────────────────────────────
// Request/Response Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CommitRequest {
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveContentRequest {
    content: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Git Operations
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/status - Get git status of the library.
async fn get_status(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryStatus>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .status()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/library/sync - Pull latest changes from remote.
async fn sync_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .sync()
        .await
        .map(|_| (StatusCode::OK, "Synced successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/library/commit - Commit all changes.
async fn commit_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(req): Json<CommitRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .commit(&req.message)
        .await
        .map(|_| (StatusCode::OK, "Committed successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/library/push - Push changes to remote.
async fn push_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .push()
        .await
        .map(|_| (StatusCode::OK, "Pushed successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// MCP Servers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/mcps - Get all MCP server definitions.
async fn get_mcps(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<HashMap<String, McpServer>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_mcp_servers()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/mcps - Save all MCP server definitions.
async fn save_mcps(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(servers): Json<HashMap<String, McpServer>>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_mcp_servers(&servers)
        .await
        .map(|_| (StatusCode::OK, "MCPs saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Skills
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/skills - List all skills.
async fn list_skills(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<SkillSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_skills()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/skills/:name - Get a skill by name.
async fn get_skill(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Skill>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_skill(&name)
        .await
        .map(Json)
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// PUT /api/library/skills/:name - Save a skill.
async fn save_skill(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_skill(&name, &req.content)
        .await
        .map(|_| (StatusCode::OK, "Skill saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/skills/:name - Delete a skill.
async fn delete_skill(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_skill(&name)
        .await
        .map(|_| (StatusCode::OK, "Skill deleted successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/skills/:name/references/*path - Get a reference file.
async fn get_skill_reference(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_skill_reference(&name, &path)
        .await
        .map(|content| (StatusCode::OK, content))
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// PUT /api/library/skills/:name/references/*path - Save a reference file.
async fn save_skill_reference(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, path)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_skill_reference(&name, &path, &req.content)
        .await
        .map(|_| (StatusCode::OK, "Reference saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Commands
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/commands - List all commands.
async fn list_commands(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<CommandSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_commands()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/commands/:name - Get a command by name.
async fn get_command(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Command>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_command(&name)
        .await
        .map(Json)
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// PUT /api/library/commands/:name - Save a command.
async fn save_command(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_command(&name, &req.content)
        .await
        .map(|_| (StatusCode::OK, "Command saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/commands/:name - Delete a command.
async fn delete_command(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_command(&name)
        .await
        .map(|_| (StatusCode::OK, "Command deleted successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
