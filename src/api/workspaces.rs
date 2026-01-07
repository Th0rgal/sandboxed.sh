//! Workspace management API endpoints.
//!
//! Provides endpoints for managing execution workspaces:
//! - List workspaces
//! - Create workspace
//! - Get workspace details
//! - Delete workspace

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::workspace::{Workspace, WorkspaceStatus, WorkspaceType};

/// Create workspace routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_workspaces))
        .route("/", post(create_workspace))
        .route("/:id", get(get_workspace))
        .route("/:id", delete(delete_workspace))
        .route("/:id/build", post(build_workspace))
}

// ─────────────────────────────────────────────────────────────────────────────
// Request/Response Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    /// Human-readable name
    pub name: String,
    /// Type of workspace (defaults to "host")
    #[serde(default)]
    pub workspace_type: WorkspaceType,
    /// Working directory path (optional, defaults based on type)
    pub path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub id: Uuid,
    pub name: String,
    pub workspace_type: WorkspaceType,
    pub path: PathBuf,
    pub status: WorkspaceStatus,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<Workspace> for WorkspaceResponse {
    fn from(w: Workspace) -> Self {
        Self {
            id: w.id,
            name: w.name,
            workspace_type: w.workspace_type,
            path: w.path,
            status: w.status,
            error_message: w.error_message,
            created_at: w.created_at,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/workspaces - List all workspaces.
async fn list_workspaces(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<Vec<WorkspaceResponse>>, (StatusCode, String)> {
    let workspaces = state.workspaces.list().await;
    let responses: Vec<WorkspaceResponse> = workspaces.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Validate workspace name to prevent path traversal.
fn validate_workspace_name(name: &str) -> Result<(), (StatusCode, String)> {
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err((
            StatusCode::BAD_REQUEST,
            "Name contains invalid characters".to_string(),
        ));
    }
    if name.starts_with('.') {
        return Err((
            StatusCode::BAD_REQUEST,
            "Name cannot start with a dot".to_string(),
        ));
    }
    Ok(())
}

/// Resolve and validate a custom workspace path.
fn resolve_custom_path(
    working_dir: &Path,
    custom_path: &Path,
) -> Result<PathBuf, (StatusCode, String)> {
    let resolved = if custom_path.is_absolute() {
        custom_path.to_path_buf()
    } else {
        working_dir.join(custom_path)
    };

    if !path_within(working_dir, &resolved) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Custom path must be within the working directory".to_string(),
        ));
    }

    Ok(resolved)
}

/// Check whether a target path is within a base directory, even if it doesn't exist yet.
/// Returns false if the path contains traversal sequences or escapes the base directory.
fn path_within(base: &Path, target: &Path) -> bool {
    use std::path::Component;

    // Reject any path containing parent directory components (..)
    // This prevents traversal attacks like "/base/../../etc/passwd"
    for component in target.components() {
        if matches!(component, Component::ParentDir) {
            return false;
        }
    }

    let base_canonical = match base.canonicalize() {
        Ok(p) => p,
        Err(_) => return false, // Base must exist and be resolvable
    };

    if target.exists() {
        // For existing paths, canonicalize resolves symlinks
        match target.canonicalize() {
            Ok(target_canonical) => target_canonical.starts_with(&base_canonical),
            Err(_) => false,
        }
    } else {
        // For non-existent paths, find the nearest existing parent and verify it's within base
        let mut current = target.to_path_buf();
        loop {
            if let Some(parent) = current.parent() {
                if parent.exists() {
                    return match parent.canonicalize() {
                        Ok(parent_canonical) => parent_canonical.starts_with(&base_canonical),
                        Err(_) => false,
                    };
                }
                current = parent.to_path_buf();
            } else {
                break;
            }
        }
        false
    }
}

/// POST /api/workspaces - Create a new workspace.
async fn create_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    // Validate workspace name for path traversal
    validate_workspace_name(&req.name)?;

    // Determine path
    let path = match &req.path {
        Some(custom_path) => resolve_custom_path(&state.config.working_dir, custom_path)?,
        None => match req.workspace_type {
            WorkspaceType::Host => state.config.working_dir.clone(),
            WorkspaceType::Chroot => {
                // Chroot workspaces go in a dedicated directory
                state
                    .config
                    .working_dir
                    .join(".openagent/chroots")
                    .join(&req.name)
            }
        },
    };

    let workspace = match req.workspace_type {
        WorkspaceType::Host => Workspace {
            id: Uuid::new_v4(),
            name: req.name,
            workspace_type: WorkspaceType::Host,
            path,
            status: WorkspaceStatus::Ready,
            error_message: None,
            config: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        },
        WorkspaceType::Chroot => Workspace::new_chroot(req.name, path),
    };

    let id = state.workspaces.add(workspace.clone()).await;
    let response: WorkspaceResponse = workspace.into();

    tracing::info!("Created workspace: {} ({})", response.name, id);

    Ok(Json(response))
}

/// GET /api/workspaces/:id - Get workspace details.
async fn get_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    state
        .workspaces
        .get(id)
        .await
        .map(|w| Json(w.into()))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))
}

/// DELETE /api/workspaces/:id - Delete a workspace.
async fn delete_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    if id == crate::workspace::DEFAULT_WORKSPACE_ID {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot delete default host workspace".to_string(),
        ));
    }

    // If it's a chroot workspace, destroy the chroot first
    if let Some(ws) = state.workspaces.get(id).await {
        if ws.workspace_type == WorkspaceType::Chroot {
            if let Err(e) = crate::workspace::destroy_chroot_workspace(&ws).await {
                tracing::error!("Failed to destroy chroot for workspace {}: {}", id, e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Failed to destroy chroot: {}. Workspace not deleted to prevent orphaned state.",
                        e
                    ),
                ));
            }
        }
    }

    if state.workspaces.delete(id).await {
        Ok((
            StatusCode::OK,
            format!("Workspace {} deleted successfully", id),
        ))
    } else {
        Err((StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))
    }
}

/// POST /api/workspaces/:id/build - Build a chroot workspace.
async fn build_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    let mut workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err((
            StatusCode::BAD_REQUEST,
            "Workspace is not a chroot type".to_string(),
        ));
    }

    // Check if already building (prevents concurrent builds)
    if workspace.status == WorkspaceStatus::Building {
        return Err((
            StatusCode::CONFLICT,
            "Workspace build already in progress".to_string(),
        ));
    }

    // Check if already ready
    if workspace.status == WorkspaceStatus::Ready {
        return Ok(Json(workspace.into()));
    }

    // Set status to Building immediately to prevent concurrent builds
    workspace.status = WorkspaceStatus::Building;
    state.workspaces.update(workspace.clone()).await;

    // Build the chroot
    match crate::workspace::build_chroot_workspace(&mut workspace, None).await {
        Ok(()) => {
            // Update in store
            state.workspaces.update(workspace.clone()).await;
            Ok(Json(workspace.into()))
        }
        Err(e) => {
            // Update status to error and save
            workspace.status = WorkspaceStatus::Error;
            workspace.error_message = Some(e.to_string());
            state.workspaces.update(workspace.clone()).await;

            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build chroot: {}", e),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_path_within_rejects_parent_traversal() {
        let base = Path::new("/tmp/working_dir");
        // Even if the literal path doesn't exist, the .. components should be rejected
        assert!(!path_within(base, Path::new("/tmp/working_dir/../etc")));
        assert!(!path_within(base, Path::new("/tmp/working_dir/../../etc/passwd")));
        assert!(!path_within(base, Path::new("/tmp/working_dir/subdir/../../../etc")));
    }

    #[test]
    fn test_path_within_rejects_relative_traversal() {
        let base = Path::new("/tmp/working_dir");
        // Relative paths with .. should be rejected
        assert!(!path_within(base, Path::new("../etc")));
        assert!(!path_within(base, Path::new("subdir/../../etc")));
    }

    #[test]
    fn test_path_within_allows_valid_subpaths() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path();

        // Create a subdirectory
        let sub = base.join("subdir");
        std::fs::create_dir(&sub).unwrap();

        // Valid paths within base
        assert!(path_within(base, &sub));
        assert!(path_within(base, &base.join("subdir/file.txt")));
        assert!(path_within(base, &base.join("newdir/newfile.txt")));
    }

    #[test]
    fn test_path_within_rejects_symlink_escape() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path();

        // Create a subdirectory and symlink pointing outside
        let sub = base.join("subdir");
        std::fs::create_dir(&sub).unwrap();

        let outside = TempDir::new().unwrap();
        let symlink_path = base.join("escape");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), &symlink_path).unwrap();
            // A path through the symlink should be rejected
            assert!(!path_within(base, &symlink_path.join("file.txt")));
        }
    }

    #[test]
    fn test_validate_workspace_name_valid() {
        assert!(validate_workspace_name("my-workspace").is_ok());
        assert!(validate_workspace_name("workspace_1").is_ok());
        assert!(validate_workspace_name("test123").is_ok());
    }

    #[test]
    fn test_validate_workspace_name_rejects_traversal() {
        assert!(validate_workspace_name("..").is_err());
        assert!(validate_workspace_name("../etc").is_err());
        assert!(validate_workspace_name("name/../etc").is_err());
        assert!(validate_workspace_name("name/subdir").is_err());
        assert!(validate_workspace_name("name\\subdir").is_err());
    }

    #[test]
    fn test_validate_workspace_name_rejects_hidden() {
        assert!(validate_workspace_name(".hidden").is_err());
        assert!(validate_workspace_name(".").is_err());
    }

    #[test]
    fn test_validate_workspace_name_rejects_empty() {
        assert!(validate_workspace_name("").is_err());
    }
}
