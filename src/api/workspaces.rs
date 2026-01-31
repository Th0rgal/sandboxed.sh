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
use crate::workspace::{self, TailscaleMode, Workspace, WorkspaceStatus, WorkspaceType};

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
        .route("/:id/exec", post(exec_workspace_command))
        // Debug endpoints for template development
        .route("/:id/debug", get(get_workspace_debug))
        .route("/:id/rerun-init", post(rerun_init_script))
        .route("/:id/init-log", get(get_init_log))
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
    /// Whether to share the host network (default: true).
    /// Set to false for isolated networking (e.g., Tailscale).
    pub shared_network: Option<bool>,
    /// Tailscale networking mode when shared_network is false.
    pub tailscale_mode: Option<TailscaleMode>,
    /// MCP server names to enable for this workspace.
    /// Empty = use default MCPs (those with `default_enabled = true`).
    #[serde(default)]
    pub mcps: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceRequest {
    /// Human-readable name (optional update)
    pub name: Option<String>,
    /// Skill names from library to sync to this workspace
    pub skills: Option<Vec<String>>,
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
    /// Whether to share the host network (default: true).
    /// Set to false for isolated networking (e.g., Tailscale).
    pub shared_network: Option<bool>,
    /// Tailscale networking mode when shared_network is false.
    pub tailscale_mode: Option<TailscaleMode>,
    /// MCP server names to enable for this workspace.
    pub mcps: Option<Vec<String>>,
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
    pub plugins: Vec<String>,
    pub template: Option<String>,
    pub distro: Option<String>,
    pub env_vars: HashMap<String, String>,
    pub init_scripts: Vec<String>,
    pub init_script: Option<String>,
    pub shared_network: Option<bool>,
    pub tailscale_mode: Option<TailscaleMode>,
    pub mcps: Vec<String>,
    pub config_profile: Option<String>,
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
            plugins: w.plugins,
            template: w.template,
            distro: w.distro,
            env_vars: w.env_vars,
            init_scripts: w.init_scripts,
            init_script: w.init_script,
            shared_network: w.shared_network,
            tailscale_mode: w.tailscale_mode,
            mcps: w.mcps,
            config_profile: w.config_profile,
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
        // Templates always require an isolated (container) workspace
        workspace_type = WorkspaceType::Container;

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
            WorkspaceType::Container => {
                // Container workspaces go in a dedicated directory
                state
                    .config
                    .working_dir
                    .join(".sandboxed-sh/containers")
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

    // Init script fragments from template
    let init_scripts = template_data
        .as_ref()
        .map(|t| t.init_scripts.clone())
        .unwrap_or_default();

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

    // shared_network: request overrides template, default to true (None means true)
    let shared_network = req
        .shared_network
        .or_else(|| template_data.as_ref().and_then(|t| t.shared_network));

    // tailscale_mode: request overrides template
    let tailscale_mode = req
        .tailscale_mode
        .or_else(|| template_data.as_ref().and_then(|t| t.tailscale_mode));

    // MCPs: request overrides template
    let mcps = if !req.mcps.is_empty() {
        req.mcps.clone()
    } else {
        template_data
            .as_ref()
            .map(|t| t.mcps.clone())
            .unwrap_or_default()
    };

    // Config profile from template
    let config_profile = template_data
        .as_ref()
        .and_then(|t| t.config_profile.clone());

    let mut workspace = match workspace_type {
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
            init_scripts: init_scripts.clone(),
            init_script,
            created_at: chrono::Utc::now(),
            skills,
            plugins: req.plugins,
            shared_network,
            tailscale_mode,
            mcps: mcps.clone(),
            config_profile: config_profile.clone(),
        },
        WorkspaceType::Container => {
            let mut ws = Workspace::new_container(req.name, path);
            ws.skills = skills;
            ws.plugins = req.plugins;
            ws.template = req.template.clone();
            ws.distro = distro;
            ws.env_vars = env_vars;
            ws.init_scripts = init_scripts;
            ws.init_script = init_script;
            ws.shared_network = shared_network;
            ws.tailscale_mode = tailscale_mode;
            ws.mcps = mcps;
            ws.config_profile = config_profile;
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
    } else if !workspace.skills.is_empty() {
        tracing::warn!(
            workspace = %workspace.name,
            "Library not initialized, cannot sync skills"
        );
    }
    drop(library_guard);

    // Auto-start build for template-based container workspaces
    // This improves UX by not requiring a separate build API call
    if workspace.workspace_type == WorkspaceType::Container && req.template.is_some() {
        let distro = workspace
            .distro
            .as_ref()
            .map(|d| parse_distro(d))
            .transpose()
            .ok()
            .flatten();

        // Set status to Building
        workspace.status = WorkspaceStatus::Building;
        state.workspaces.update(workspace.clone()).await;

        // Spawn build task
        let workspaces_store = Arc::clone(&state.workspaces);
        let working_dir = state.config.working_dir.clone();
        let mut workspace_for_build = workspace.clone();
        // Get library for init script assembly
        let library = {
            let guard = state.library.read().await;
            guard.as_ref().map(Arc::clone)
        };

        tokio::spawn(async move {
            let result = crate::workspace::build_container_workspace(
                &mut workspace_for_build,
                distro,
                false, // don't force rebuild
                &working_dir,
                library.as_deref(),
            )
            .await;

            if let Err(e) = result {
                tracing::error!(
                    workspace = %workspace_for_build.name,
                    error = %e,
                    "Failed to auto-build container workspace"
                );
            }

            workspaces_store.update(workspace_for_build).await;
        });

        tracing::info!(
            "Created and auto-building workspace: {} ({})",
            workspace.name,
            id
        );
    } else {
        tracing::info!("Created workspace: {} ({})", workspace.name, id);
    }

    let response: WorkspaceResponse = workspace.into();

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

    // Update shared_network if explicitly set in the request.
    // The request uses Option<Option<bool>> pattern - outer Option for "field present",
    // inner Option for the actual value including null for "reset to default".
    // However, serde flattens this, so we check if the field was explicitly provided.
    // For now, we only update if Some() is passed; to reset, pass null explicitly via the UI.
    if req.shared_network.is_some() {
        workspace.shared_network = req.shared_network;
    }

    // Update tailscale_mode if explicitly set in the request
    if req.tailscale_mode.is_some() {
        workspace.tailscale_mode = req.tailscale_mode;
    }

    // Update MCPs if provided
    if let Some(mcps) = req.mcps {
        workspace.mcps = mcps;
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
    } else if skills_changed && !workspace.skills.is_empty() {
        tracing::warn!(
            workspace = %workspace.name,
            "Library not initialized, cannot sync skills"
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

    tracing::info!("Synced skills to workspace: {} ({})", workspace.name, id);

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
        if ws.workspace_type == WorkspaceType::Container {
            if let Err(e) = crate::workspace::destroy_container_workspace(&ws).await {
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

/// Escape a string for safe use in shell commands.
fn shell_escape(s: &str) -> String {
    // Use single quotes and escape any single quotes in the string
    format!("'{}'", s.replace('\'', "'\\''"))
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

    if workspace.workspace_type != WorkspaceType::Container {
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
    // Get library for init script assembly
    let library = {
        let guard = state.library.read().await;
        guard.as_ref().map(Arc::clone)
    };

    tokio::spawn(async move {
        let result = crate::workspace::build_container_workspace(
            &mut workspace_for_build,
            distro,
            force_rebuild,
            &working_dir,
            library.as_deref(),
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

// ─────────────────────────────────────────────────────────────────────────────
// Command Execution
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExecCommandRequest {
    /// The shell command to execute
    pub command: String,
    /// Optional working directory (relative to workspace or absolute within workspace)
    pub cwd: Option<String>,
    /// Timeout in seconds (default: 300, max: 600)
    pub timeout_secs: Option<u64>,
    /// Environment variables to set for the command
    pub env: Option<HashMap<String, String>>,
    /// Optional input to pass to stdin
    pub stdin: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecCommandResponse {
    /// Exit code of the command
    pub exit_code: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Whether the command timed out
    pub timed_out: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Debug Types (for template development)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorkspaceDebugInfo {
    /// Workspace ID
    pub id: Uuid,
    /// Workspace name
    pub name: String,
    /// Current status
    pub status: WorkspaceStatus,
    /// Container/workspace path
    pub path: String,
    /// Whether the container directory exists
    pub path_exists: bool,
    /// Size of the container in bytes (if applicable)
    pub size_bytes: Option<u64>,
    /// Key directories that exist in the container
    pub directories: Vec<DirectoryInfo>,
    /// Whether bash is available
    pub has_bash: bool,
    /// Whether the init script file exists in the container
    pub init_script_exists: bool,
    /// Last modification time of init script
    pub init_script_modified: Option<String>,
    /// Distro information
    pub distro: Option<String>,
    /// Any error message from last build
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DirectoryInfo {
    /// Directory path
    pub path: String,
    /// Whether it exists
    pub exists: bool,
    /// Approximate file count (if exists)
    pub file_count: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct InitLogResponse {
    /// Whether the log file exists
    pub exists: bool,
    /// Log content (last N lines)
    pub content: String,
    /// Total lines in log
    pub total_lines: Option<u32>,
    /// Log file path inside container
    pub log_path: String,
}

#[derive(Debug, Serialize)]
pub struct RerunInitResponse {
    /// Whether the rerun was successful
    pub success: bool,
    /// Exit code from the script
    pub exit_code: i32,
    /// Standard output from the script
    pub stdout: String,
    /// Standard error from the script
    pub stderr: String,
    /// Execution time in seconds
    pub duration_secs: f64,
}

/// POST /api/workspaces/:id/exec - Execute a command in a workspace.
async fn exec_workspace_command(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<ExecCommandRequest>,
) -> Result<Json<ExecCommandResponse>, (StatusCode, String)> {
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    // For container workspaces, ensure container is ready
    if workspace.workspace_type == WorkspaceType::Container {
        if workspace.status != WorkspaceStatus::Ready {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Container workspace is not ready (status: {:?}). Build it first.",
                    workspace.status
                ),
            ));
        }
    }

    let timeout = Duration::from_secs(req.timeout_secs.unwrap_or(300).min(600));

    // Determine working directory
    let cwd = match &req.cwd {
        Some(path) => {
            let path = Path::new(path);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.path.join(path)
            }
        }
        None => workspace.path.clone(),
    };

    let (program, args) = match workspace.workspace_type {
        WorkspaceType::Host => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            (shell, vec!["-c".to_string(), req.command.clone()])
        }
        WorkspaceType::Container => {
            // For container workspaces, use systemd-nspawn
            let container_root = workspace.path.clone();
            let rel_cwd = if cwd.starts_with(&container_root) {
                let rel = cwd.strip_prefix(&container_root).unwrap_or(Path::new(""));
                if rel.as_os_str().is_empty() {
                    "/".to_string()
                } else {
                    format!("/{}", rel.to_string_lossy())
                }
            } else {
                "/root/work".to_string()
            };

            let mut nspawn_args = vec![
                "-D".to_string(),
                container_root.to_string_lossy().to_string(),
                "--quiet".to_string(),
                "--timezone=off".to_string(),
                "--chdir".to_string(),
                rel_cwd.clone(),
            ];

            // Check network isolation settings
            let use_shared_network = workspace.shared_network.unwrap_or(true);
            let tailscale_mode = workspace.tailscale_mode.unwrap_or(TailscaleMode::ExitNode);
            let tailscale_requested = crate::nspawn::tailscale_enabled(&workspace.env_vars);

            // Add network isolation flags if shared_network is false
            let tailscale_enabled = if use_shared_network {
                // Shared network: bind DNS from host
                nspawn_args.push("--bind-ro=/etc/resolv.conf".to_string());
                false
            } else if tailscale_requested {
                // Isolated network with Tailscale
                nspawn_args.push("--network-veth".to_string());
                nspawn_args.push("--capability=CAP_NET_ADMIN,CAP_NET_RAW".to_string());
                if Path::new("/dev/net/tun").exists() {
                    nspawn_args.push("--bind=/dev/net/tun".to_string());
                }
                true
            } else {
                // Isolated network without Tailscale - still bind DNS
                nspawn_args.push("--bind-ro=/etc/resolv.conf".to_string());
                false
            };

            // Add workspace env vars
            for (key, value) in &workspace.env_vars {
                nspawn_args.push(format!("--setenv={}={}", key, value));
            }

            // Add request env vars
            if let Some(env) = &req.env {
                for (key, value) in env {
                    nspawn_args.push(format!("--setenv={}={}", key, value));
                }
            }

            // Build the command to run
            let final_command = if tailscale_enabled {
                // Wrap command with Tailscale bootstrap script
                let tailnet_only = tailscale_mode == TailscaleMode::TailnetOnly;
                let mut bootstrap_cmd = String::new();

                // Export TS_* env vars for the bootstrap script
                for (k, v) in &workspace.env_vars {
                    if k.starts_with("TS_") && !v.trim().is_empty() {
                        bootstrap_cmd.push_str(&format!("export {}={}; ", k, shell_escape(v)));
                    }
                }

                // Run the Tailscale bootstrap script
                bootstrap_cmd.push_str(
                    "if [ -x /usr/local/bin/sandboxed-tailscale-up ]; then \
                     /usr/local/bin/sandboxed-tailscale-up >/dev/null 2>&1 || true; \
                     fi; ",
                );

                // For tailnet_only mode, set up host gateway routing for internet
                if tailnet_only {
                    bootstrap_cmd.push_str(
                        "_oa_ip=$(ip -4 addr show host0 2>/dev/null | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p' | head -1); \
                         _oa_gw=\"${_oa_ip%.*}.1\"; \
                         if [ -n \"$_oa_ip\" ]; then \
                           ip route del default 2>/dev/null || true; \
                           ip route add default via \"$_oa_gw\" 2>/dev/null || true; \
                         fi; \
                         if [ ! -s /etc/resolv.conf ]; then \
                           printf 'nameserver 8.8.8.8\\nnameserver 1.1.1.1\\n' > /etc/resolv.conf 2>/dev/null || true; \
                         fi; "
                    );
                }

                // Change to working directory and run the actual command
                bootstrap_cmd.push_str(&format!(
                    "cd {} 2>/dev/null || true; ",
                    shell_escape(&rel_cwd)
                ));
                bootstrap_cmd.push_str(&req.command);

                bootstrap_cmd
            } else {
                req.command.clone()
            };

            nspawn_args.extend(["/bin/bash".to_string(), "-c".to_string(), final_command]);

            ("systemd-nspawn".to_string(), nspawn_args)
        }
    };

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Set environment for host workspaces
    if workspace.workspace_type == WorkspaceType::Host {
        cmd.current_dir(&cwd);
        for (key, value) in &workspace.env_vars {
            cmd.env(key, value);
        }
        if let Some(env) = &req.env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }
    }

    let mut child = cmd.spawn().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to spawn command: {}", e),
        )
    })?;

    // Write stdin if provided
    if let Some(input) = &req.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
    }

    // Take stdout/stderr handles before waiting
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    // Wait with timeout
    let wait_result = tokio::time::timeout(timeout, child.wait()).await;

    match wait_result {
        Ok(Ok(status)) => {
            // Read output after process completes
            let stdout = if let Some(mut handle) = stdout_handle {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let _ = handle.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            let stderr = if let Some(mut handle) = stderr_handle {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let _ = handle.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            let exit_code = status.code().unwrap_or(-1);

            Ok(Json(ExecCommandResponse {
                exit_code,
                stdout,
                stderr,
                timed_out: false,
            }))
        }
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Command execution failed: {}", e),
        )),
        Err(_) => {
            // Timeout - try to kill the process
            let _ = child.kill().await;
            Ok(Json(ExecCommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Command timed out after {} seconds", timeout.as_secs()),
                timed_out: true,
            }))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Debug Endpoints (for template development)
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/workspaces/:id/debug - Get debug information about a workspace container.
///
/// Returns detailed information about the container state useful for debugging
/// init script issues: directory structure, file existence, sizes, etc.
async fn get_workspace_debug(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<WorkspaceDebugInfo>, (StatusCode, String)> {
    let workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    let path = &workspace.path;
    let path_exists = path.exists();

    // Calculate container size (only for container workspaces)
    // Use du -sk (kilobytes) for portability, then convert to bytes
    let size_bytes = if workspace.workspace_type == WorkspaceType::Container && path_exists {
        let output = tokio::process::Command::new("du")
            .args(["-sk", &path.to_string_lossy()])
            .output()
            .await
            .ok();

        output.and_then(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let kb = stdout.split_whitespace().next()?.parse::<u64>().ok()?;
            Some(kb * 1024) // Convert KB to bytes
        })
    } else {
        None
    };

    // Check key directories
    let key_dirs = ["bin", "usr", "etc", "var", "var/log", "root", "tmp"];
    let directories: Vec<DirectoryInfo> = key_dirs
        .iter()
        .map(|dir| {
            let full_path = path.join(dir);
            let exists = full_path.exists() && full_path.is_dir();
            let file_count = if exists {
                std::fs::read_dir(&full_path)
                    .map(|entries| entries.count() as u32)
                    .ok()
            } else {
                None
            };
            DirectoryInfo {
                path: dir.to_string(),
                exists,
                file_count,
            }
        })
        .collect();

    // Check for bash
    let has_bash = path.join("bin/bash").exists() || path.join("usr/bin/bash").exists();

    // Check for init script
    let init_script_path = path.join("sandboxed-init.sh");
    let init_script_exists = init_script_path.exists();
    let init_script_modified = if init_script_exists {
        std::fs::metadata(&init_script_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t)
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string()
            })
    } else {
        None
    };

    Ok(Json(WorkspaceDebugInfo {
        id: workspace.id,
        name: workspace.name.clone(),
        status: workspace.status.clone(),
        path: path.to_string_lossy().to_string(),
        path_exists,
        size_bytes,
        directories,
        has_bash,
        init_script_exists,
        init_script_modified,
        distro: workspace.distro.clone(),
        last_error: workspace.error_message.clone(),
    }))
}

/// GET /api/workspaces/:id/init-log - Get the init script log from inside the container.
///
/// Reads /var/log/sandboxed-init.log from inside the container to show what
/// the init script has logged. Useful for debugging template issues.
async fn get_init_log(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<InitLogResponse>, (StatusCode, String)> {
    let workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    let log_path = "/var/log/sandboxed-init.log";
    let host_log_path = workspace.path.join("var/log/sandboxed-init.log");

    // During debootstrap, the container filesystem doesn't exist yet so
    // var/log/sandboxed-init.log won't be there. Check the build log sibling
    // file that debootstrap streams output to.
    let effective_log_path = if host_log_path.exists() {
        host_log_path
    } else {
        let build_log = crate::nspawn::build_log_path_for(&workspace.path);
        if build_log.exists() {
            build_log
        } else {
            return Ok(Json(InitLogResponse {
                exists: false,
                content: String::new(),
                total_lines: None,
                log_path: log_path.to_string(),
            }));
        }
    };

    // Read the log file
    let content = tokio::fs::read_to_string(&effective_log_path)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read log file: {}", e),
            )
        })?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u32;

    // Return last 500 lines max
    let start = if lines.len() > 500 {
        lines.len() - 500
    } else {
        0
    };
    let truncated_content = lines[start..].join("\n");

    Ok(Json(InitLogResponse {
        exists: true,
        content: truncated_content,
        total_lines: Some(total_lines),
        log_path: log_path.to_string(),
    }))
}

/// POST /api/workspaces/:id/rerun-init - Re-run the init script without rebuilding the container.
///
/// This allows template developers to iterate on their init script without
/// waiting for a full container rebuild. The container must already exist.
async fn rerun_init_script(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<RerunInitResponse>, (StatusCode, String)> {
    let mut workspace = state
        .workspaces
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Workspace {} not found", id)))?;

    // Only works for container workspaces
    if workspace.workspace_type != WorkspaceType::Container {
        return Err((
            StatusCode::BAD_REQUEST,
            "Rerun init only works for container workspaces".to_string(),
        ));
    }

    // Container must exist (at least have basic structure)
    if !workspace.path.join("bin").exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Container doesn't exist yet. Build it first.".to_string(),
        ));
    }

    // Must have an init script configured
    let init_script = workspace.init_script.clone().unwrap_or_default();
    if init_script.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No init script configured for this workspace".to_string(),
        ));
    }

    // Write the init script to the container
    let script_path = workspace.path.join("sandboxed-init.sh");
    tokio::fs::write(&script_path, &init_script)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write init script: {}", e),
            )
        })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&script_path, perms)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to set script permissions: {}", e),
                )
            })?;
    }

    // Update status to building
    workspace.status = WorkspaceStatus::Building;
    workspace.error_message = None;
    state.workspaces.update(workspace.clone()).await;

    // Run the init script
    let start_time = std::time::Instant::now();

    let shell = if workspace.path.join("bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };

    let mut config = crate::nspawn::NspawnConfig::default();
    config.env = workspace.env_vars.clone();

    let command = vec![shell.to_string(), "/sandboxed-init.sh".to_string()];
    let output_result =
        crate::nspawn::execute_in_container(&workspace.path, &command, &config).await;

    let duration_secs = start_time.elapsed().as_secs_f64();

    // Clean up the script file regardless of success/failure
    let _ = tokio::fs::remove_file(&script_path).await;

    // Handle container execution failure - revert status and return error
    let output = match output_result {
        Ok(out) => out,
        Err(e) => {
            // Revert workspace status to Error so it's not stuck in Building
            workspace.status = WorkspaceStatus::Error;
            workspace.error_message = Some(format!("Failed to execute init script: {}", e));
            state.workspaces.update(workspace).await;
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to execute init script: {}", e),
            ));
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    // Update workspace status
    if success {
        workspace.status = WorkspaceStatus::Ready;
        workspace.error_message = None;
    } else {
        workspace.status = WorkspaceStatus::Error;
        let mut error_msg = String::new();
        if !stderr.trim().is_empty() {
            error_msg.push_str(stderr.trim());
        }
        if !stdout.trim().is_empty() {
            if !error_msg.is_empty() {
                error_msg.push_str(" | ");
            }
            error_msg.push_str(stdout.trim());
        }
        if error_msg.is_empty() {
            error_msg = format!("Init script failed with exit code {}", exit_code);
        }
        workspace.error_message = Some(format!("Init script failed: {}", error_msg));
    }

    state.workspaces.update(workspace).await;

    Ok(Json(RerunInitResponse {
        success,
        exit_code,
        stdout,
        stderr,
        duration_secs,
    }))
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
