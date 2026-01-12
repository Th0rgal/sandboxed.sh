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
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::library::WorkspaceTemplate;
use crate::nspawn::NspawnDistro;
use crate::workspace::{self, Workspace, WorkspaceStatus, WorkspaceType};

/// Create workspace routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_workspaces))
        .route("/", post(create_workspace))
        .route("/:id", get(get_workspace))
        .route("/:id", put(update_workspace))
        .route("/:id", delete(delete_workspace))
        .route("/:id/build", post(build_workspace))
        .route("/:id/sync", post(sync_workspace))
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
    /// Skill names from library to sync to this workspace
    #[serde(default)]
    pub skills: Vec<String>,
    /// Tool names from library to sync to this workspace
    #[serde(default)]
    pub tools: Vec<String>,
    /// Plugin identifiers for hooks
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Optional workspace template name
    pub template: Option<String>,
    /// Preferred Linux distribution for container workspaces
    pub distro: Option<String>,
    /// Environment variables always loaded in this workspace
    pub env_vars: Option<HashMap<String, String>>,
    /// Init script to run when the workspace is built/rebuilt
    pub init_script: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceRequest {
    /// Human-readable name (optional update)
    pub name: Option<String>,
    /// Skill names from library to sync to this workspace
    pub skills: Option<Vec<String>>,
    /// Tool names from library to sync to this workspace
    pub tools: Option<Vec<String>>,
    /// Plugin identifiers for hooks
    pub plugins: Option<Vec<String>>,
    /// Optional workspace template name
    pub template: Option<String>,
    /// Preferred Linux distribution for container workspaces
    pub distro: Option<String>,
    /// Environment variables always loaded in this workspace
    pub env_vars: Option<HashMap<String, String>>,
    /// Init script to run when the workspace is built/rebuilt
    pub init_script: Option<String>,
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
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub plugins: Vec<String>,
    pub template: Option<String>,
    pub distro: Option<String>,
    pub env_vars: HashMap<String, String>,
    pub init_script: Option<String>,
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
            skills: w.skills,
            tools: w.tools,
            plugins: w.plugins,
            template: w.template,
            distro: w.distro,
            env_vars: w.env_vars,
            init_script: w.init_script,
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

    let mut workspace_type = req.workspace_type;
    let mut template_data: Option<WorkspaceTemplate> = None;

    if let Some(template_name) = req.template.as_ref() {
        // Templates always require an isolated (chroot) workspace
        workspace_type = WorkspaceType::Chroot;

        let library = {
            let guard = state.library.read().await;
            guard.as_ref().map(Arc::clone)
        }
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Library not initialized".to_string(),
            )
        })?;

        template_data = Some(
            library
                .get_workspace_template(template_name)
                .await
                .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?,
        );
    }

    // Host workspaces require a custom path - the root working directory is reserved
    // for the default host workspace (which is created automatically).
    if workspace_type == WorkspaceType::Host && req.path.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Host workspaces require a custom path. The root working directory is reserved for the default host workspace.".to_string(),
        ));
    }

    // Determine path
    let path = match &req.path {
        Some(custom_path) => resolve_custom_path(&state.config.working_dir, custom_path)?,
        None => match workspace_type {
            WorkspaceType::Host => {
                // This should be unreachable due to the check above, but keeping for safety
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Host workspaces require a custom path".to_string(),
                ));
            }
            WorkspaceType::Chroot => {
                // Container workspaces go in a dedicated directory
                state
                    .config
                    .working_dir
                    .join(".openagent/containers")
                    .join(&req.name)
            }
        },
    };

    let mut env_vars = template_data
        .as_ref()
        .map(|t| t.env_vars.clone())
        .unwrap_or_default();
    if let Some(custom_env) = req.env_vars.clone() {
        env_vars.extend(custom_env);
    }
    env_vars = sanitize_env_vars(env_vars);

    let mut skills = template_data
        .as_ref()
        .map(|t| t.skills.clone())
        .unwrap_or_default();
    if !req.skills.is_empty() {
        skills.extend(req.skills.clone());
    }
    skills = sanitize_skill_list(skills);

    let mut init_script = template_data.as_ref().map(|t| t.init_script.clone());
    if let Some(custom_script) = req.init_script.clone() {
        init_script = Some(custom_script);
    }
    init_script = normalize_init_script(init_script);

    let mut distro = template_data.as_ref().and_then(|t| t.distro.clone());
    if let Some(custom_distro) = req.distro.as_ref() {
        distro = Some(custom_distro.to_string());
    }
    let distro = match distro {
        Some(value) => Some(normalize_distro_value(&value)?),
        None => None,
    };

    let workspace = match workspace_type {
        WorkspaceType::Host => Workspace {
            id: Uuid::new_v4(),
            name: req.name,
            workspace_type: WorkspaceType::Host,
            path,
            status: WorkspaceStatus::Ready,
            error_message: None,
            config: serde_json::json!({}),
            template: req.template.clone(),
            distro,
            env_vars,
            init_script,
            created_at: chrono::Utc::now(),
            skills,
            tools: req.tools,
            plugins: req.plugins,
        },
        WorkspaceType::Chroot => {
            let mut ws = Workspace::new_chroot(req.name, path);
            ws.skills = skills;
            ws.tools = req.tools;
            ws.plugins = req.plugins;
            ws.template = req.template.clone();
            ws.distro = distro;
            ws.env_vars = env_vars;
            ws.init_script = init_script;
            ws
        }
    };

    let id = state.workspaces.add(workspace.clone()).await;

    // Sync skills and tools to workspace if any are specified
    let library_guard = state.library.read().await;
    if let Some(library) = library_guard.as_ref() {
        if !workspace.skills.is_empty() {
            if let Err(e) = workspace::sync_workspace_skills(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync skills to workspace during creation"
                );
            }
        }
        if !workspace.tools.is_empty() {
            if let Err(e) = workspace::sync_workspace_tools(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync tools to workspace during creation"
                );
            }
        }
    } else if !workspace.skills.is_empty() || !workspace.tools.is_empty() {
        tracing::warn!(
            workspace = %workspace.name,
            "Library not initialized, cannot sync skills/tools"
        );
    }
    drop(library_guard);

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

/// PUT /api/workspaces/:id - Update a workspace.
async fn update_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateWorkspaceRequest>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    let mut workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    // Validate name if provided
    if let Some(ref name) = req.name {
        validate_workspace_name(name)?;
        workspace.name = name.clone();
    }

    // Update skills if provided
    let skills_changed = if let Some(skills) = req.skills {
        workspace.skills = sanitize_skill_list(skills);
        true
    } else {
        false
    };

    // Update tools if provided
    let tools_changed = if let Some(tools) = req.tools {
        workspace.tools = tools;
        true
    } else {
        false
    };

    // Update plugins if provided
    if let Some(plugins) = req.plugins {
        workspace.plugins = plugins;
    }

    if let Some(template) = req.template {
        let trimmed = template.trim();
        if trimmed.is_empty() {
            workspace.template = None;
        } else {
            workspace.template = Some(trimmed.to_string());
        }
    }

    if let Some(distro) = req.distro {
        let trimmed = distro.trim();
        if trimmed.is_empty() {
            workspace.distro = None;
        } else {
            workspace.distro = Some(normalize_distro_value(trimmed)?);
        }
    }

    if let Some(env_vars) = req.env_vars {
        workspace.env_vars = sanitize_env_vars(env_vars);
    }

    if let Some(init_script) = req.init_script {
        workspace.init_script = normalize_init_script(Some(init_script));
    }

    // Save the updated workspace
    state.workspaces.update(workspace.clone()).await;

    // Sync skills and tools if they changed
    let library_guard = state.library.read().await;
    if let Some(library) = library_guard.as_ref() {
        if skills_changed && !workspace.skills.is_empty() {
            if let Err(e) = workspace::sync_workspace_skills(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync skills to workspace during update"
                );
            }
        }
        if tools_changed && !workspace.tools.is_empty() {
            if let Err(e) = workspace::sync_workspace_tools(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync tools to workspace during update"
                );
            }
        }
    } else if (skills_changed && !workspace.skills.is_empty())
        || (tools_changed && !workspace.tools.is_empty())
    {
        tracing::warn!(
            workspace = %workspace.name,
            "Library not initialized, cannot sync skills/tools"
        );
    }

    tracing::info!("Updated workspace: {} ({})", workspace.name, id);

    Ok(Json(workspace.into()))
}

/// POST /api/workspaces/:id/sync - Manually sync skills and tools to workspace.
async fn sync_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    let workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    // Get library
    let library_guard = state.library.read().await;
    let library = library_guard.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Library not initialized".to_string(),
        )
    })?;

    // Sync skills to workspace
    workspace::sync_workspace_skills(&workspace, library)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to sync skills: {}", e),
            )
        })?;

    // Sync tools to workspace
    workspace::sync_workspace_tools(&workspace, library)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to sync tools: {}", e),
            )
        })?;

    tracing::info!(
        "Synced skills and tools to workspace: {} ({})",
        workspace.name,
        id
    );

    Ok(Json(workspace.into()))
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

    // If it's a container workspace, destroy the container first
    if let Some(ws) = state.workspaces.get(id).await {
        if ws.workspace_type == WorkspaceType::Chroot {
            if let Err(e) = crate::workspace::destroy_chroot_workspace(&ws).await {
                tracing::error!("Failed to destroy container for workspace {}: {}", id, e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Failed to destroy container: {}. Workspace not deleted to prevent orphaned state.",
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

#[derive(Debug, Deserialize)]
pub struct BuildWorkspaceRequest {
    /// Linux distribution to use (defaults to "ubuntu-noble")
    /// Options: "ubuntu-noble", "ubuntu-jammy", "debian-bookworm", "arch-linux"
    pub distro: Option<String>,
    /// Force rebuild even if the container already exists
    pub rebuild: Option<bool>,
}

/// Parse a distro string into a NspawnDistro enum.
fn parse_distro(s: &str) -> Result<NspawnDistro, String> {
    NspawnDistro::parse(s).ok_or_else(|| {
        format!(
            "Unknown distro '{}'. Supported: {}",
            s,
            NspawnDistro::supported_values().join(", ")
        )
    })
}

fn normalize_distro_value(value: &str) -> Result<String, (StatusCode, String)> {
    NspawnDistro::parse(value)
        .map(|d| d.api_value().to_string())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "Unknown distro '{}'. Supported: {}",
                    value,
                    NspawnDistro::supported_values().join(", ")
                ),
            )
        })
}

fn normalize_init_script(value: Option<String>) -> Option<String> {
    value.and_then(|script| {
        if script.trim().is_empty() {
            None
        } else {
            Some(script)
        }
    })
}

fn sanitize_env_vars(env_vars: HashMap<String, String>) -> HashMap<String, String> {
    env_vars
        .into_iter()
        .filter(|(key, _)| !key.trim().is_empty())
        .collect()
}

fn sanitize_skill_list(skills: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for skill in skills {
        let trimmed = skill.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// POST /api/workspaces/:id/build - Build a container workspace.
async fn build_workspace(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    body: Option<Json<BuildWorkspaceRequest>>,
) -> Result<Json<WorkspaceResponse>, (StatusCode, String)> {
    let mut workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err((
            StatusCode::BAD_REQUEST,
            "Workspace is not a container type".to_string(),
        ));
    }

    let force_rebuild = body.as_ref().and_then(|b| b.rebuild).unwrap_or(false);

    // Parse distro from request (or stored workspace default)
    let distro_override = body
        .as_ref()
        .and_then(|b| b.distro.as_ref())
        .map(|d| parse_distro(d))
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let distro = match distro_override {
        Some(distro) => {
            workspace.distro = Some(distro.api_value().to_string());
            Some(distro)
        }
        None => match workspace.distro.as_ref() {
            Some(value) => Some(parse_distro(value).map_err(|e| (StatusCode::BAD_REQUEST, e))?),
            None => None,
        },
    };

    // Check if already building (prevents concurrent builds)
    if workspace.status == WorkspaceStatus::Building {
        return Err((
            StatusCode::CONFLICT,
            "Workspace build already in progress".to_string(),
        ));
    }

    // Set status to Building immediately to prevent concurrent builds
    workspace.status = WorkspaceStatus::Building;
    state.workspaces.update(workspace.clone()).await;

    // Run the container build in the background so long builds aren't tied to the HTTP request
    let workspaces_store = Arc::clone(&state.workspaces);
    let working_dir = state.config.working_dir.clone();
    let mut workspace_for_build = workspace.clone();

    tokio::spawn(async move {
        let result = crate::workspace::build_chroot_workspace(
            &mut workspace_for_build,
            distro,
            force_rebuild,
            &working_dir,
        )
        .await;

        if let Err(e) = result {
            tracing::error!(
                workspace = %workspace_for_build.name,
                error = %e,
                "Failed to build container workspace"
            );
        }

        workspaces_store.update(workspace_for_build).await;
    });

    Ok(Json(workspace.into()))
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
        assert!(!path_within(
            base,
            Path::new("/tmp/working_dir/../../etc/passwd")
        ));
        assert!(!path_within(
            base,
            Path::new("/tmp/working_dir/subdir/../../../etc")
        ));
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
