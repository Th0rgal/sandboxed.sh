//! Library management API endpoints.
//!
//! Provides endpoints for managing the configuration library:
//! - Git operations (status, sync, commit, push)
//! - MCP server CRUD
//! - Skills CRUD
//! - Commands CRUD
//! - Plugins CRUD
//! - Library Agents CRUD
//! - OpenCode settings (oh-my-opencode.json)
//! - Sandboxed config (agent visibility, defaults)
//! - Migration

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::library::{
    rename::{ItemType, RenameResult},
    AmpCodeConfig, ClaudeCodeConfig, Command, CommandSummary, ConfigProfile, ConfigProfileSummary,
    GitAuthor, InitScript, InitScriptSummary, LibraryAgent, LibraryAgentSummary, LibraryStatus,
    LibraryStore, McpServer, MigrationReport, Plugin, SandboxedConfig, Skill, SkillSummary,
    WorkspaceTemplate, WorkspaceTemplateSummary,
};
use crate::nspawn::NspawnDistro;
use crate::workspace::{self, WorkspaceType, DEFAULT_WORKSPACE_ID};

/// Shared library state.
pub type SharedLibrary = Arc<RwLock<Option<Arc<LibraryStore>>>>;

const LIBRARY_REMOTE_HEADER: &str = "x-sandboxed-library-remote";
const GIT_AUTHOR_NAME_HEADER: &str = "x-sandboxed-git-author-name";
const GIT_AUTHOR_EMAIL_HEADER: &str = "x-sandboxed-git-author-email";

fn extract_library_remote(headers: &HeaderMap) -> Option<String> {
    headers
        .get(LIBRARY_REMOTE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn extract_git_author(headers: &HeaderMap) -> Option<GitAuthor> {
    let name = headers
        .get(GIT_AUTHOR_NAME_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let email = headers
        .get(GIT_AUTHOR_EMAIL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if name.is_some() || email.is_some() {
        Some(GitAuthor::new(name, email))
    } else {
        None
    }
}

fn is_default_host_workspace(workspace: &workspace::Workspace) -> bool {
    workspace.id == DEFAULT_WORKSPACE_ID && workspace.workspace_type == WorkspaceType::Host
}

async fn sync_all_workspaces(state: &super::routes::AppState, library: &LibraryStore) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if is_default_host_workspace(&workspace) || !workspace.skills.is_empty() {
            if let Err(e) = workspace::sync_workspace_skills(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync skills after library update"
                );
            }
        }
    }
}

async fn sync_skill_to_workspaces(
    state: &super::routes::AppState,
    library: &LibraryStore,
    skill_name: &str,
) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if is_default_host_workspace(&workspace) || workspace.skills.iter().any(|s| s == skill_name)
        {
            if let Err(e) = workspace::sync_workspace_skills(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    skill = %skill_name,
                    error = %e,
                    "Failed to sync skill to workspace"
                );
            }
        }
    }
}

async fn ensure_library(
    state: &super::routes::AppState,
    headers: &HeaderMap,
) -> Result<Arc<LibraryStore>, (StatusCode, String)> {
    // Check HTTP header override first, then fall back to settings store
    let remote = match extract_library_remote(headers) {
        Some(r) => Some(r),
        None => state.settings.get_library_remote().await,
    };
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
            drop(library_guard);
            sync_all_workspaces(state, store.as_ref()).await;
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
        .route("/force-sync", post(force_sync_library))
        .route("/force-push", post(force_push_library))
        .route("/commit", post(commit_library))
        .route("/push", post(push_library))
        // MCP servers
        .route("/mcps", get(get_mcps))
        .route("/mcps", put(save_mcps))
        // Skills
        .route("/skill", get(list_skills))
        .route("/skill/import", post(import_skill))
        .route("/skill/:name", get(get_skill))
        .route("/skill/:name", put(save_skill))
        .route("/skill/:name", delete(delete_skill))
        .route("/skill/:name/files/*path", get(get_skill_reference))
        .route("/skill/:name/files/*path", put(save_skill_reference))
        .route("/skill/:name/files/*path", delete(delete_skill_reference))
        // Legacy skills routes (dashboard still calls /skills)
        .route("/skills", get(list_skills))
        .route("/skills/import", post(import_skill))
        .route("/skills/:name", get(get_skill))
        .route("/skills/:name", put(save_skill))
        .route("/skills/:name", delete(delete_skill))
        .route("/skills/:name/references/*path", get(get_skill_reference))
        .route("/skills/:name/references/*path", put(save_skill_reference))
        .route(
            "/skills/:name/references/*path",
            delete(delete_skill_reference),
        )
        // Commands
        .route("/command", get(list_commands))
        .route("/command/:name", get(get_command))
        .route("/command/:name", put(save_command))
        .route("/command/:name", delete(delete_command))
        // Legacy commands routes (dashboard still calls /commands)
        .route("/commands", get(list_commands))
        .route("/commands/:name", get(get_command))
        .route("/commands/:name", put(save_command))
        .route("/commands/:name", delete(delete_command))
        // Builtin commands (runtime-specific slash commands)
        .route("/builtin-commands", get(get_builtin_commands))
        // Plugins
        .route("/plugins", get(get_plugins))
        .route("/plugins", put(save_plugins))
        // Library Agents
        .route("/agent", get(list_library_agents))
        .route("/agent/:name", get(get_library_agent))
        .route("/agent/:name", put(save_library_agent))
        .route("/agent/:name", delete(delete_library_agent))
        // Workspace Templates
        .route("/workspace-template", get(list_workspace_templates))
        .route("/workspace-template/:name", get(get_workspace_template))
        .route("/workspace-template/:name", put(save_workspace_template))
        .route(
            "/workspace-template/:name",
            delete(delete_workspace_template),
        )
        // Init Scripts
        .route("/init-script", get(list_init_scripts))
        .route("/init-script/:name", get(get_init_script))
        .route("/init-script/:name", put(save_init_script))
        .route("/init-script/:name", delete(delete_init_script))
        // Migration
        .route("/migrate", post(migrate_library))
        // Rename (works for all item types)
        .route("/rename/:item_type/:name", post(rename_item))
        // OpenCode Settings (oh-my-opencode.json)
        .route("/opencode/settings", get(get_opencode_settings))
        .route("/opencode/settings", put(save_opencode_settings))
        // Sandboxed Config
        .route("/sandboxed-sh/config", get(get_sandboxed_config))
        .route("/sandboxed-sh/config", put(save_sandboxed_config))
        .route("/sandboxed-sh/agents", get(get_visible_agents))
        // Claude Code Config
        .route("/claudecode/config", get(get_claudecode_config))
        .route("/claudecode/config", put(save_claudecode_config))
        // Config Profiles
        .route("/config-profile", get(list_config_profiles))
        .route("/config-profile", post(create_config_profile))
        .route("/config-profile/:name", get(get_config_profile))
        .route("/config-profile/:name", put(save_config_profile))
        .route("/config-profile/:name", delete(delete_config_profile))
        // Profile-specific config endpoints
        .route(
            "/config-profile/:name/opencode/settings",
            get(get_opencode_settings_for_profile),
        )
        .route(
            "/config-profile/:name/opencode/settings",
            put(save_opencode_settings_for_profile),
        )
        .route(
            "/config-profile/:name/sandboxed-sh/config",
            get(get_sandboxed_config_for_profile),
        )
        .route(
            "/config-profile/:name/sandboxed-sh/config",
            put(save_sandboxed_config_for_profile),
        )
        .route(
            "/config-profile/:name/claudecode/config",
            get(get_claudecode_config_for_profile),
        )
        .route(
            "/config-profile/:name/claudecode/config",
            put(save_claudecode_config_for_profile),
        )
        .route(
            "/config-profile/:name/ampcode/config",
            get(get_ampcode_config_for_profile),
        )
        .route(
            "/config-profile/:name/ampcode/config",
            put(save_ampcode_config_for_profile),
        )
        // File-based config profile editing
        .route(
            "/config-profile/:name/files",
            get(list_config_profile_files),
        )
        .route(
            "/config-profile/:name/file/*file_path",
            get(get_config_profile_file),
        )
        .route(
            "/config-profile/:name/file/*file_path",
            put(save_config_profile_file),
        )
        .route(
            "/config-profile/:name/file/*file_path",
            delete(delete_config_profile_file),
        )
        // Harness defaults (library base configs)
        .route("/harness-default/:harness", get(list_harness_default_files))
        .route(
            "/harness-default/:harness/*file_name",
            get(get_harness_default_file),
        )
        // Skills Registry (skills.sh)
        .route("/skill/registry/search", get(search_registry))
        .route("/skill/registry/list/:identifier", get(list_repo_skills))
        .route("/skill/registry/install", post(install_from_registry))
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

#[derive(Debug, Deserialize)]
pub struct ImportSkillRequest {
    /// Skill name (required for file upload)
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct RegistrySearchQuery {
    /// Search query
    q: String,
}

#[derive(Debug, Deserialize)]
pub struct InstallFromRegistryRequest {
    /// Repository identifier (e.g., "vercel-labs/agent-skills")
    identifier: String,
    /// Specific skill names to install (optional, installs all if empty)
    #[serde(default)]
    skills: Vec<String>,
    /// Target name for the skill in the library (defaults to skill name)
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveWorkspaceTemplateRequest {
    pub description: Option<String>,
    pub distro: Option<String>,
    pub skills: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,
    pub encrypted_keys: Option<Vec<String>>,
    /// Init script fragment names to include (executed in order)
    #[serde(default)]
    pub init_scripts: Option<Vec<String>>,
    /// Custom init script to run on build (appended after fragments)
    pub init_script: Option<String>,
    /// Whether to share the host network (default: true).
    /// Set to false for isolated networking (e.g., Tailscale).
    pub shared_network: Option<bool>,
    /// Tailscale networking mode (only relevant when shared_network is false).
    pub tailscale_mode: Option<crate::workspace::TailscaleMode>,
    /// MCP server names to enable for workspaces created from this template.
    #[serde(default)]
    pub mcps: Option<Vec<String>>,
    /// Config profile to use for workspaces created from this template.
    #[serde(default)]
    pub config_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    /// The new name for the item.
    pub new_name: String,
    /// If true, return what would be changed without actually changing anything.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateConfigProfileRequest {
    /// Name for the new profile
    pub name: String,
    /// Optional base profile to copy settings from
    #[serde(default)]
    pub base_profile: Option<String>,
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

// ─────────────────────────────────────────────────────────────────────────────
// Git Operations
// ─────────────────────────────────────────────────────────────────────────────

/// Sync all library configurations after a git sync/pull operation.
/// This includes plugins, OpenCode settings, Sandboxed config, and workspaces.
async fn sync_library_configs(
    state: &Arc<super::routes::AppState>,
    library: &LibraryStore,
) -> Result<(), (StatusCode, String)> {
    // Sync plugins to global OpenCode config
    let plugins = library
        .get_plugins()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::opencode_config::sync_global_plugins(&plugins)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync OpenCode settings (oh-my-opencode.json) from Library to system
    if let Err(e) = workspace::sync_opencode_settings(library).await {
        tracing::warn!(error = %e, "Failed to sync oh-my-opencode settings during library sync");
    }

    // Sync Sandboxed config from Library to working directory
    if let Err(e) = workspace::sync_sandboxed_config(library, &state.config.working_dir).await {
        tracing::warn!(error = %e, "Failed to sync sandboxed config during library sync");
    }

    // Sync skills and tools to workspaces
    sync_all_workspaces(state, library).await;

    Ok(())
}

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
///
/// Returns 409 Conflict if history has diverged (e.g., after force push).
/// In that case, use /force-sync to reset to remote or /force-push to overwrite remote.
async fn sync_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    // Try to sync - check for diverged history error
    if let Err(e) = library.sync().await {
        let error_msg = e.to_string();
        if error_msg.starts_with("DIVERGED_HISTORY:") {
            // Return 409 Conflict with a structured error message
            let msg = error_msg
                .strip_prefix("DIVERGED_HISTORY: ")
                .unwrap_or(&error_msg);
            return Err((StatusCode::CONFLICT, format!("DIVERGED_HISTORY: {}", msg)));
        }
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error_msg));
    }

    // Sync all library configurations
    sync_library_configs(&state, library.as_ref()).await?;

    Ok((StatusCode::OK, "Synced successfully".to_string()))
}

/// POST /api/library/force-sync - Force reset local branch to match remote.
///
/// Use this when local and remote histories have diverged (e.g., after a force push on remote).
/// This discards any local changes and resets to the remote state.
async fn force_sync_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .force_sync()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync all library configurations
    sync_library_configs(&state, library.as_ref()).await?;

    Ok((
        StatusCode::OK,
        "Force synced successfully - local branch reset to remote".to_string(),
    ))
}

/// POST /api/library/force-push - Force push local changes to remote.
///
/// Use this when you want to keep local changes and overwrite the remote history.
/// Uses --force-with-lease for safety.
async fn force_push_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .force_push()
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Force pushed successfully - remote updated with local changes".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/library/commit - Commit all changes.
async fn commit_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(req): Json<CommitRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    let author = extract_git_author(&headers);
    library
        .commit(&req.message, author.as_ref())
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
    library.get_skill(&name).await.map(Json).map_err(|e| {
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sync_skill_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Skill saved successfully".to_string()))
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sync_skill_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Skill deleted successfully".to_string()))
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sync_skill_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Reference saved successfully".to_string()))
}

/// DELETE /api/library/skills/:name/references/*path - Delete a reference file.
async fn delete_skill_reference(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_skill_reference(&name, &path)
        .await
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else if e.to_string().contains("Cannot delete SKILL.md") {
                (StatusCode::BAD_REQUEST, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })?;
    sync_skill_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Reference deleted successfully".to_string()))
}

/// POST /api/library/skills/import - Import a skill from a file upload (.zip or .md).
///
/// Accepts multipart form data with:
/// - `name`: skill name (query parameter)
/// - `file`: the uploaded file (.zip or .md)
///
/// For .md files: creates a skill with the file as SKILL.md
/// For .zip files: extracts and looks for SKILL.md in the archive
async fn import_skill(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Query(req): Query<ImportSkillRequest>,
    mut multipart: Multipart,
) -> Result<Json<Skill>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    // Validate skill name
    let skill_name = req.name.trim().to_lowercase();
    if skill_name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Skill name is required".to_string(),
        ));
    }
    if !skill_name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Skill name must contain only lowercase letters, numbers, and hyphens".to_string(),
        ));
    }

    // Check if skill already exists
    let skill_dir = library.path().join("skill").join(&skill_name);
    if skill_dir.exists() {
        return Err((
            StatusCode::CONFLICT,
            format!("Skill '{}' already exists", skill_name),
        ));
    }

    // Extract file from multipart
    let mut file_data: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to read upload: {}", e),
        )
    })? {
        if field.name() == Some("file") {
            let filename = field
                .file_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "upload".to_string());
            let data = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Failed to read file: {}", e),
                )
            })?;
            file_data = Some((filename, data.to_vec()));
            break;
        }
    }

    let (filename, data) =
        file_data.ok_or_else(|| (StatusCode::BAD_REQUEST, "No file uploaded".to_string()))?;

    // Create skill directory
    tokio::fs::create_dir_all(&skill_dir).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create skill directory: {}", e),
        )
    })?;

    // Handle based on file type
    let filename_lower = filename.to_lowercase();
    if filename_lower.ends_with(".zip") {
        // Extract ZIP file
        import_skill_from_zip(&skill_dir, &data)
            .await
            .map_err(|e| {
                // Clean up on error
                let _ = std::fs::remove_dir_all(&skill_dir);
                (StatusCode::BAD_REQUEST, e)
            })?;
    } else if filename_lower.ends_with(".md") {
        // Single markdown file - save as SKILL.md
        let skill_md_path = skill_dir.join("SKILL.md");
        tokio::fs::write(&skill_md_path, &data).await.map_err(|e| {
            let _ = std::fs::remove_dir_all(&skill_dir);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write SKILL.md: {}", e),
            )
        })?;
    } else {
        let _ = std::fs::remove_dir_all(&skill_dir);
        return Err((
            StatusCode::BAD_REQUEST,
            "Unsupported file type. Please upload a .zip or .md file".to_string(),
        ));
    }

    // Verify SKILL.md exists
    let skill_md_path = skill_dir.join("SKILL.md");
    if !skill_md_path.exists() {
        let _ = std::fs::remove_dir_all(&skill_dir);
        return Err((
            StatusCode::BAD_REQUEST,
            "No SKILL.md found in the uploaded archive".to_string(),
        ));
    }

    // Load and return the skill
    let skill = library.get_skill(&skill_name).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load imported skill: {}", e),
        )
    })?;

    sync_skill_to_workspaces(&state, library.as_ref(), &skill_name).await;
    Ok(Json(skill))
}

/// Extract a ZIP file into the skill directory.
async fn import_skill_from_zip(skill_dir: &std::path::Path, data: &[u8]) -> Result<(), String> {
    use std::io::{Cursor, Read};

    let cursor = Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Invalid ZIP file: {}", e))?;

    // Find the common prefix (for archives with a single root folder)
    let prefix = find_zip_prefix(&mut archive);

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read ZIP entry: {}", e))?;

        let name = file.name().to_string();

        // Skip directories and hidden files
        if name.ends_with('/') || name.contains("/.") || name.starts_with('.') {
            continue;
        }

        // Remove common prefix if present
        let relative_path = if let Some(ref p) = prefix {
            name.strip_prefix(p).unwrap_or(&name)
        } else {
            &name
        };

        if relative_path.is_empty() {
            continue;
        }

        let target_path = skill_dir.join(relative_path);

        // Create parent directories
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        // Extract file
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|e| format!("Failed to read file from ZIP: {}", e))?;
        std::fs::write(&target_path, contents)
            .map_err(|e| format!("Failed to write file: {}", e))?;
    }

    Ok(())
}

/// Find common prefix in ZIP archive (for archives with a single root folder).
fn find_zip_prefix(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Option<String> {
    let mut first_dir: Option<String> = None;

    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name();
            if let Some(slash_pos) = name.find('/') {
                let dir = &name[..=slash_pos];
                match &first_dir {
                    None => first_dir = Some(dir.to_string()),
                    Some(d) if d != dir => return None, // Multiple root dirs
                    _ => {}
                }
            } else {
                return None; // File at root level
            }
        }
    }

    first_dir
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
    library.get_command(&name).await.map(Json).map_err(|e| {
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

/// Response for builtin commands endpoint.
#[derive(Debug, serde::Serialize)]
struct BuiltinCommandsResponse {
    /// Commands for OpenCode (oh-my-opencode)
    opencode: Vec<CommandSummary>,
    /// Commands for Claude Code
    claudecode: Vec<CommandSummary>,
}

/// GET /api/library/builtin-commands - Get builtin slash commands for each backend.
///
/// Returns the native slash commands available for OpenCode and Claude Code.
/// These are runtime-specific commands that don't come from the Library.
async fn get_builtin_commands() -> Json<BuiltinCommandsResponse> {
    // OpenCode builtin commands (oh-my-opencode)
    let opencode_commands = vec![
        CommandSummary {
            name: "ralph-loop".to_string(),
            description: Some(
                "Start self-referential development loop until completion".to_string(),
            ),
            path: "builtin".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "cancel-ralph".to_string(),
            description: Some("Cancel active Ralph Loop".to_string()),
            path: "builtin".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "start-work".to_string(),
            description: Some("Start Sisyphus work session from Prometheus plan".to_string()),
            path: "builtin".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "refactor".to_string(),
            description: Some(
                "Intelligent refactoring with LSP, AST-grep, and TDD verification".to_string(),
            ),
            path: "builtin".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "init-deep".to_string(),
            description: Some("Initialize hierarchical AGENTS.md knowledge base".to_string()),
            path: "builtin".to_string(),
            params: vec![],
        },
    ];

    // Claude Code builtin commands
    let claudecode_commands = vec![
        CommandSummary {
            name: "plan".to_string(),
            description: Some("Enter plan mode to design an implementation approach".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "compact".to_string(),
            description: Some("Compact conversation history to save context".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "clear".to_string(),
            description: Some("Clear conversation history and start fresh".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "config".to_string(),
            description: Some("Show or modify configuration settings".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "cost".to_string(),
            description: Some("Show token usage and API costs".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "doctor".to_string(),
            description: Some("Check installation and diagnose issues".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "help".to_string(),
            description: Some("Show available commands and usage".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "memory".to_string(),
            description: Some("Manage persistent memories across sessions".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "mcp".to_string(),
            description: Some("Manage Model Context Protocol servers".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "review".to_string(),
            description: Some("Request a code review of recent changes".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "bug".to_string(),
            description: Some("Report a bug with diagnostic info".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "login".to_string(),
            description: Some("Log in to your Anthropic account".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "logout".to_string(),
            description: Some("Log out of your Anthropic account".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
        CommandSummary {
            name: "resume".to_string(),
            description: Some("Resume a previous conversation".to_string()),
            path: "builtin-claude".to_string(),
            params: vec![],
        },
    ];

    Json(BuiltinCommandsResponse {
        opencode: opencode_commands,
        claudecode: claudecode_commands,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugins
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/plugins - Get all plugins.
async fn get_plugins(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<HashMap<String, Plugin>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_plugins()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/plugins - Save all plugins.
async fn save_plugins(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(plugins): Json<HashMap<String, Plugin>>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_plugins(&plugins)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    crate::opencode_config::sync_global_plugins(&plugins)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((StatusCode::OK, "Plugins saved successfully".to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Agents
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/agent - List all library agents.
async fn list_library_agents(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<LibraryAgentSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_library_agents()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/agent/:name - Get a library agent by name.
async fn get_library_agent(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<LibraryAgent>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_library_agent(&name)
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

/// PUT /api/library/agent/:name - Save a library agent.
async fn save_library_agent(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(agent): Json<LibraryAgent>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_library_agent(&name, &agent)
        .await
        .map(|_| (StatusCode::OK, "Agent saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/agent/:name - Delete a library agent.
async fn delete_library_agent(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_library_agent(&name)
        .await
        .map(|_| (StatusCode::OK, "Agent deleted successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Templates
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/workspace-template - List workspace templates.
async fn list_workspace_templates(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<WorkspaceTemplateSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_workspace_templates()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/workspace-template/:name - Get workspace template.
async fn get_workspace_template(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<WorkspaceTemplate>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_workspace_template(&name)
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

/// PUT /api/library/workspace-template/:name - Save workspace template.
async fn save_workspace_template(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveWorkspaceTemplateRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    if let Some(distro) = req.distro.as_ref() {
        if NspawnDistro::parse(distro).is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Unknown distro '{}'. Supported: {}",
                    distro,
                    NspawnDistro::supported_values().join(", ")
                ),
            ));
        }
    }

    let library = ensure_library(&state, &headers).await?;
    let template = WorkspaceTemplate {
        name: name.clone(),
        description: req.description.clone(),
        path: format!("workspace-template/{}.json", name),
        distro: req.distro.clone(),
        skills: sanitize_skill_list(req.skills.unwrap_or_default()),
        env_vars: req.env_vars.unwrap_or_default(),
        encrypted_keys: req.encrypted_keys.unwrap_or_default(),
        init_scripts: req.init_scripts.unwrap_or_default(),
        init_script: req.init_script.unwrap_or_default(),
        shared_network: req.shared_network,
        tailscale_mode: req.tailscale_mode,
        mcps: req.mcps.unwrap_or_default(),
        config_profile: req.config_profile.clone(),
    };

    library
        .save_workspace_template(&name, &template)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Workspace template saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/workspace-template/:name - Delete workspace template.
async fn delete_workspace_template(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_workspace_template(&name)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Workspace template deleted successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Init Scripts
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/init-script - List all init script fragments.
async fn list_init_scripts(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<InitScriptSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_init_scripts()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/init-script/:name - Get an init script fragment by name.
async fn get_init_script(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<InitScript>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library.get_init_script(&name).await.map(Json).map_err(|e| {
        if e.to_string().contains("not found") {
            (StatusCode::NOT_FOUND, e.to_string())
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    })
}

/// PUT /api/library/init-script/:name - Save an init script fragment.
async fn save_init_script(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_init_script(&name, &req.content)
        .await
        .map(|_| (StatusCode::OK, "Init script saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/init-script/:name - Delete an init script fragment.
async fn delete_init_script(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_init_script(&name)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Init script deleted successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Migration
// ─────────────────────────────────────────────────────────────────────────────

/// POST /api/library/migrate - Migrate library structure to new format.
async fn migrate_library(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<MigrationReport>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .migrate_structure()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Settings (oh-my-opencode.json)
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/opencode/settings - Get oh-my-opencode settings from Library.
async fn get_opencode_settings(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_opencode_settings()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/opencode/settings - Save oh-my-opencode settings to Library.
async fn save_opencode_settings(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(settings): Json<serde_json::Value>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    // Validate that the input is a valid JSON object
    if !settings.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Settings must be a JSON object".to_string(),
        ));
    }

    library
        .save_opencode_settings(&settings)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync to system location
    if let Err(e) = workspace::sync_opencode_settings(&library).await {
        tracing::warn!(error = %e, "Failed to sync oh-my-opencode settings to system");
    }

    Ok((
        StatusCode::OK,
        "OpenCode settings saved successfully".to_string(),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Sandboxed Config
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/sandboxed-sh/config - Get Sandboxed config from Library.
async fn get_sandboxed_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<SandboxedConfig>, (StatusCode, String)> {
    match ensure_library(&state, &headers).await {
        Ok(library) => library
            .get_sandboxed_config()
            .await
            .map(Json)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {
            let config = workspace::read_sandboxed_config(&state.config.working_dir).await;
            Ok(Json(config))
        }
        Err(e) => Err(e),
    }
}

/// PUT /api/library/sandboxed-sh/config - Save Sandboxed config to Library.
async fn save_sandboxed_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(config): Json<SandboxedConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    match ensure_library(&state, &headers).await {
        Ok(library) => {
            library
                .save_sandboxed_config(&config)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            // Sync to working directory
            if let Err(e) =
                workspace::sync_sandboxed_config(&library, &state.config.working_dir).await
            {
                tracing::warn!(error = %e, "Failed to sync sandboxed config to working dir");
            }

            Ok((
                StatusCode::OK,
                "Sandboxed config saved successfully".to_string(),
            ))
        }
        Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {
            if let Err(e) =
                workspace::write_sandboxed_config(&state.config.working_dir, &config).await
            {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to write sandboxed config locally: {}", e),
                ));
            }
            Ok((
                StatusCode::OK,
                "Sandboxed config saved locally (Library not configured)".to_string(),
            ))
        }
        Err(e) => Err(e),
    }
}

/// GET /api/library/sandboxed-sh/agents - Get filtered list of visible agents.
/// Fetches agents from OpenCode and filters by hidden_agents config.
async fn get_visible_agents(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Read current config from working directory
    let config = workspace::read_sandboxed_config(&state.config.working_dir).await;

    // Fetch all agents from OpenCode
    let all_agents = crate::api::opencode::fetch_opencode_agents(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let visible_agents = filter_visible_agents_with_fallback(all_agents.clone(), &config);

    Ok(Json(visible_agents))
}

fn filter_visible_agents_with_fallback(
    agents: serde_json::Value,
    config: &SandboxedConfig,
) -> serde_json::Value {
    filter_agents_by_config(agents, config)
}

/// Filter agents based on Sandboxed config hidden_agents list.
fn filter_agents_by_config(
    agents: serde_json::Value,
    config: &SandboxedConfig,
) -> serde_json::Value {
    /// Extract agent name from an array entry (can be string or object with name/id)
    fn get_agent_name(entry: &serde_json::Value) -> Option<&str> {
        if let Some(s) = entry.as_str() {
            return Some(s);
        }
        if let Some(obj) = entry.as_object() {
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                return Some(name);
            }
            if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                return Some(id);
            }
        }
        None
    }

    /// Filter an array of agents
    fn filter_array(arr: &[serde_json::Value], hidden: &[String]) -> Vec<serde_json::Value> {
        arr.iter()
            .filter(|entry| {
                get_agent_name(entry)
                    .map(|name| !hidden.contains(&name.to_string()))
                    .unwrap_or(true)
            })
            .cloned()
            .collect()
    }

    // Handle different response formats from OpenCode:
    // 1. Object with "agents" array: {agents: [{name: "..."}, ...]}
    // 2. Direct array: [{name: "..."}, ...]
    // 3. Object with agent names as keys: {"AgentName": {...}, ...}

    if let Some(agents_obj) = agents.as_object() {
        // Check if it has an "agents" array property
        if let Some(agents_arr) = agents_obj.get("agents").and_then(|v| v.as_array()) {
            // Format: {agents: [...]}
            let filtered = filter_array(agents_arr, &config.hidden_agents);
            let mut result = agents_obj.clone();
            result.insert("agents".to_string(), serde_json::Value::Array(filtered));
            return serde_json::Value::Object(result);
        }

        // Format: object with agent names as keys
        let filtered: serde_json::Map<String, serde_json::Value> = agents_obj
            .iter()
            .filter(|(name, _)| !config.hidden_agents.contains(name))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        serde_json::Value::Object(filtered)
    } else if let Some(agents_arr) = agents.as_array() {
        // Format: direct array
        let filtered = filter_array(agents_arr, &config.hidden_agents);
        serde_json::Value::Array(filtered)
    } else {
        // Unknown format, return as-is
        agents
    }
}

/// Validate that an agent name exists in the visible agents list.
/// Returns Ok(()) if the agent exists, or Err with a descriptive message if not.
pub async fn validate_agent_exists(
    state: &super::routes::AppState,
    agent_name: &str,
) -> Result<(), String> {
    // Fetch all agents from OpenCode
    let all_agents = match crate::api::opencode::fetch_opencode_agents(state).await {
        Ok(agents) => agents,
        Err(e) => {
            // If we can't fetch agents, log warning but allow the request
            // (OpenCode will validate at runtime)
            tracing::warn!("Could not validate agent '{}': {}", agent_name, e);
            return Ok(());
        }
    };

    // Read config to get hidden agents list
    let config = crate::workspace::read_sandboxed_config(&state.config.working_dir).await;
    let visible_agents = filter_visible_agents_with_fallback(all_agents.clone(), &config);

    // Extract agent names from the visible agents list.
    // If all agents are hidden, fall back to the raw OpenCode list so OpenCode remains usable.
    let mut agent_names = extract_agent_names(&visible_agents);
    if agent_names.is_empty() {
        agent_names = extract_agent_names(&all_agents);
    }

    // Case-insensitive match for better UX
    let exists = agent_names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(agent_name));

    if exists {
        return Ok(());
    }

    // If the requested agent exists in the raw OpenCode list, allow it even if hidden.
    let raw_agent_names = extract_agent_names(&all_agents);
    if raw_agent_names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(agent_name))
    {
        return Ok(());
    }

    let suggestions = if agent_names.is_empty() {
        raw_agent_names.join(", ")
    } else {
        agent_names.join(", ")
    };
    Err(format!(
        "Agent '{}' not found. Available agents: {}",
        agent_name, suggestions
    ))
}

/// Extract agent names from the visible agents payload.
fn extract_agent_names(agents: &serde_json::Value) -> Vec<String> {
    fn get_name(entry: &serde_json::Value) -> Option<String> {
        if let Some(s) = entry.as_str() {
            return Some(s.to_string());
        }
        if let Some(obj) = entry.as_object() {
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                return Some(name.to_string());
            }
            if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
        None
    }

    if let Some(agents_obj) = agents.as_object() {
        if let Some(agents_arr) = agents_obj.get("agents").and_then(|v| v.as_array()) {
            return agents_arr.iter().filter_map(get_name).collect();
        }
        // Object with agent names as keys
        return agents_obj.keys().cloned().collect();
    }
    if let Some(agents_arr) = agents.as_array() {
        return agents_arr.iter().filter_map(get_name).collect();
    }
    Vec::new()
}

// ─────────────────────────────────────────────────────────────────────────────
// Rename
// ─────────────────────────────────────────────────────────────────────────────

/// POST /api/library/rename/:item_type/:name - Rename a library item.
/// Supports dry_run mode to preview changes before applying them.
async fn rename_item(
    State(state): State<Arc<super::routes::AppState>>,
    Path((item_type_str, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<RenameRequest>,
) -> Result<Json<RenameResult>, (StatusCode, String)> {
    // Parse item type
    let item_type = match item_type_str.as_str() {
        "skill" => ItemType::Skill,
        "command" => ItemType::Command,
        "agent" => ItemType::Agent,
        "tool" => ItemType::Tool,
        "workspace-template" => ItemType::WorkspaceTemplate,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid item type '{}'. Valid types: skill, command, agent, tool, workspace-template",
                    item_type_str
                ),
            ))
        }
    };

    let library = ensure_library(&state, &headers).await?;

    // Perform rename (or dry run)
    let result = library
        .rename_item(item_type, &name, &req.new_name, req.dry_run)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // If not dry run and successful, update workspace references
    if !req.dry_run && result.success {
        match item_type {
            ItemType::Skill => {
                // Update workspace skill lists
                update_workspace_skill_references(&state, &name, &req.new_name).await;
                // Sync skills to workspaces
                sync_skill_to_workspaces(&state, library.as_ref(), &req.new_name).await;
            }
            ItemType::WorkspaceTemplate => {
                // Update workspace template references
                update_workspace_template_references(&state, &name, &req.new_name).await;
            }
            _ => {}
        }
    }

    if !result.success {
        return Err((
            StatusCode::BAD_REQUEST,
            result
                .error
                .clone()
                .unwrap_or_else(|| "Rename failed".to_string()),
        ));
    }

    Ok(Json(result))
}

/// Update workspace skill references when a skill is renamed.
async fn update_workspace_skill_references(
    state: &super::routes::AppState,
    old_name: &str,
    new_name: &str,
) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if workspace.skills.contains(&old_name.to_string()) {
            let mut updated_workspace = workspace.clone();
            updated_workspace.skills = updated_workspace
                .skills
                .iter()
                .map(|s| {
                    if s == old_name {
                        new_name.to_string()
                    } else {
                        s.clone()
                    }
                })
                .collect();

            let workspace_name = workspace.name.clone();
            if !state.workspaces.update(updated_workspace).await {
                tracing::warn!(
                    workspace = %workspace_name,
                    "Failed to update workspace skill reference"
                );
            }
        }
    }
}

/// Update workspace template references when a template is renamed.
async fn update_workspace_template_references(
    state: &super::routes::AppState,
    old_name: &str,
    new_name: &str,
) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if workspace.template.as_deref() == Some(old_name) {
            let mut updated_workspace = workspace.clone();
            updated_workspace.template = Some(new_name.to_string());

            let workspace_name = workspace.name.clone();
            if !state.workspaces.update(updated_workspace).await {
                tracing::warn!(
                    workspace = %workspace_name,
                    "Failed to update workspace template reference"
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude Code Config
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/claudecode/config - Get Claude Code config from Library.
async fn get_claudecode_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<ClaudeCodeConfig>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_claudecode_config()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/claudecode/config - Save Claude Code config to Library.
async fn save_claudecode_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(config): Json<ClaudeCodeConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    library
        .save_claudecode_config(&config)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((
        StatusCode::OK,
        "Claude Code config saved successfully".to_string(),
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Config Profiles
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/config-profile - List all config profiles.
async fn list_config_profiles(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<ConfigProfileSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_config_profiles()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// POST /api/library/config-profile - Create a new config profile.
async fn create_config_profile(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateConfigProfileRequest>,
) -> Result<Json<ConfigProfile>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .create_config_profile(&req.name, req.base_profile.as_deref())
        .await
        .map(Json)
        .map_err(|e| {
            if e.to_string().contains("already exists") {
                (StatusCode::CONFLICT, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// GET /api/library/config-profile/:name - Get a config profile by name.
async fn get_config_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ConfigProfile>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_config_profile(&name)
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

/// PUT /api/library/config-profile/:name - Save a config profile.
async fn save_config_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(profile): Json<ConfigProfile>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_config_profile(&name, &profile)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Config profile saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/config-profile/:name - Delete a config profile.
async fn delete_config_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_config_profile(&name)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Config profile deleted successfully".to_string(),
            )
        })
        .map_err(|e| {
            if e.to_string().contains("Cannot delete") {
                (StatusCode::BAD_REQUEST, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// GET /api/library/config-profile/:name/opencode/settings - Get OpenCode settings for a profile.
async fn get_opencode_settings_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_opencode_settings_for_profile(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/config-profile/:name/opencode/settings - Save OpenCode settings for a profile.
async fn save_opencode_settings_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(settings): Json<serde_json::Value>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    if !settings.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Settings must be a JSON object".to_string(),
        ));
    }

    library
        .save_opencode_settings_for_profile(&name, &settings)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "OpenCode settings saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/config-profile/:name/sandboxed-sh/config - Get Sandboxed config for a profile.
async fn get_sandboxed_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SandboxedConfig>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_sandboxed_config_for_profile(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/config-profile/:name/sandboxed-sh/config - Save Sandboxed config for a profile.
async fn save_sandboxed_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(config): Json<SandboxedConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_sandboxed_config_for_profile(&name, &config)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Sandboxed config saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/config-profile/:name/claudecode/config - Get Claude Code config for a profile.
async fn get_claudecode_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ClaudeCodeConfig>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_claudecode_config_for_profile(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/config-profile/:name/claudecode/config - Save Claude Code config for a profile.
async fn save_claudecode_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(config): Json<ClaudeCodeConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_claudecode_config_for_profile(&name, &config)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Claude Code config saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/config-profile/:name/ampcode/config - Get Amp Code config for a profile.
async fn get_ampcode_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<AmpCodeConfig>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_ampcode_config_for_profile(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// PUT /api/library/config-profile/:name/ampcode/config - Save Amp Code config for a profile.
async fn save_ampcode_config_for_profile(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(config): Json<AmpCodeConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_ampcode_config_for_profile(&name, &config)
        .await
        .map(|_| {
            (
                StatusCode::OK,
                "Amp Code config saved successfully".to_string(),
            )
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/config-profile/:name/files - List all files in a config profile.
async fn list_config_profile_files(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_config_profile_files(&name)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/config-profile/:name/file/*file_path - Get a specific file from a config profile.
async fn get_config_profile_file(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, file_path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<String, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_config_profile_file(&name, &file_path)
        .await
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// PUT /api/library/config-profile/:name/file/*file_path - Save a specific file in a config profile.
async fn save_config_profile_file(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, file_path)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_config_profile_file(&name, &file_path, &body)
        .await
        .map(|_| (StatusCode::OK, "File saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/config-profile/:name/file/*file_path - Delete a specific file from a config profile.
async fn delete_config_profile_file(
    State(state): State<Arc<super::routes::AppState>>,
    Path((name, file_path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_config_profile_file(&name, &file_path)
        .await
        .map(|_| (StatusCode::OK, "File deleted successfully".to_string()))
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Harness Defaults Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/harness-default/:harness - List default files for a harness.
async fn list_harness_default_files(
    State(state): State<Arc<super::routes::AppState>>,
    Path(harness): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_harness_default_files(&harness)
        .await
        .map(Json)
        .map_err(|e| {
            if e.to_string().contains("Invalid harness") {
                (StatusCode::BAD_REQUEST, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

/// GET /api/library/harness-default/:harness/*file_name - Get a harness default file.
async fn get_harness_default_file(
    State(state): State<Arc<super::routes::AppState>>,
    Path((harness, file_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<String, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_harness_default_file(&harness, &file_name)
        .await
        .map_err(|e| {
            if e.to_string().contains("not found") {
                (StatusCode::NOT_FOUND, e.to_string())
            } else if e.to_string().contains("Invalid harness") {
                (StatusCode::BAD_REQUEST, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Skills Registry (skills.sh) Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/skill/registry/search?q=<query> - Search skills.sh registry.
async fn search_registry(
    axum::extract::Query(query): axum::extract::Query<RegistrySearchQuery>,
) -> Result<Json<Vec<crate::skills_registry::RegistrySkillListing>>, (StatusCode, String)> {
    let results = crate::skills_registry::search_skills(&query.q)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(results))
}

/// GET /api/library/skill/registry/list/:identifier - List skills in a repository.
async fn list_repo_skills(
    Path(identifier): Path<String>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let skills = crate::skills_registry::list_repo_skills(&identifier)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(skills))
}

/// POST /api/library/skill/registry/install - Install a skill from skills.sh.
async fn install_from_registry(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(request): Json<InstallFromRegistryRequest>,
) -> Result<Json<Skill>, (StatusCode, String)> {
    use crate::library::SkillSource;

    let library = ensure_library(&state, &headers).await?;

    // Create a temporary directory for the skills CLI to work in
    let temp_dir = library.path().join(".tmp-registry-install");
    if temp_dir.exists() {
        tokio::fs::remove_dir_all(&temp_dir)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Initialize a minimal structure for the skills CLI
    // The skills CLI expects certain directories to exist
    let claude_skills_dir = temp_dir.join(".claude").join("skills");
    tokio::fs::create_dir_all(&claude_skills_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Run the install command
    let skill_refs: Vec<&str> = request.skills.iter().map(|s| s.as_str()).collect();
    let skill_names = if skill_refs.is_empty() {
        None
    } else {
        Some(skill_refs.as_slice())
    };

    let result = crate::skills_registry::install_skill(&request.identifier, skill_names, &temp_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !result.errors.is_empty() {
        // Clean up temp dir
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Installation errors: {}", result.errors.join(", ")),
        ));
    }

    // Find the installed skill in .claude/skills/
    let mut installed_skill_dir = None;
    let mut entries = tokio::fs::read_dir(&claude_skills_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").exists() {
            installed_skill_dir = Some(path);
            break;
        }
    }

    let source_dir = installed_skill_dir.ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "No skill found after installation".to_string(),
        )
    })?;

    // Determine target name
    let skill_name = request.name.unwrap_or_else(|| {
        source_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "imported-skill".to_string())
    });

    // Copy to library
    let target_dir = library.path().join("skill").join(&skill_name);
    if target_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err((
            StatusCode::CONFLICT,
            format!("Skill '{}' already exists", skill_name),
        ));
    }

    copy_dir_recursive(&source_dir, &target_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Write the source metadata file
    let source = SkillSource::SkillsRegistry {
        identifier: request.identifier.clone(),
        skill_name: request.skills.first().cloned(),
        version: None,
        installed_at: Some(chrono::Utc::now().to_rfc3339()),
        updated_at: None,
    };
    let source_json = serde_json::to_string_pretty(&source)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tokio::fs::write(target_dir.join(".skill-source.json"), source_json)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Clean up temp directory
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;

    // Get and return the skill
    let skill = library
        .get_skill(&skill_name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync to workspaces
    sync_skill_to_workspaces(&state, &library, &skill_name).await;

    Ok(Json(skill))
}

/// Recursively copy a directory.
#[async_recursion::async_recursion]
async fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(dst).await?;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path).await?;
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    /// Create a ZIP archive in memory with the given files.
    fn create_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut writer = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();

            for (path, content) in files {
                writer.start_file(*path, options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn test_find_zip_prefix_single_root() {
        let zip_data = create_zip(&[
            ("my-skill/SKILL.md", "# Test"),
            ("my-skill/refs/file.md", "content"),
        ]);
        let cursor = Cursor::new(zip_data.as_slice());
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let prefix = find_zip_prefix(&mut archive);
        assert_eq!(prefix, Some("my-skill/".to_string()));
    }

    #[test]
    fn test_find_zip_prefix_multiple_roots() {
        let zip_data = create_zip(&[
            ("folder1/file.md", "content1"),
            ("folder2/file.md", "content2"),
        ]);
        let cursor = Cursor::new(zip_data.as_slice());
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let prefix = find_zip_prefix(&mut archive);
        assert_eq!(prefix, None);
    }

    #[test]
    fn test_find_zip_prefix_file_at_root() {
        let zip_data = create_zip(&[("SKILL.md", "# Test"), ("refs/file.md", "content")]);
        let cursor = Cursor::new(zip_data.as_slice());
        let mut archive = zip::ZipArchive::new(cursor).unwrap();
        let prefix = find_zip_prefix(&mut archive);
        assert_eq!(prefix, None);
    }

    #[tokio::test]
    async fn test_import_skill_from_zip_flat() {
        let zip_data = create_zip(&[
            ("SKILL.md", "---\ndescription: Test skill\n---\n\n# Test"),
            ("refs/example.md", "Example content"),
        ]);

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        import_skill_from_zip(&skill_dir, &zip_data).await.unwrap();

        // Check files were extracted correctly
        assert!(skill_dir.join("SKILL.md").exists());
        let content = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(content.contains("description: Test skill"));

        assert!(skill_dir.join("refs/example.md").exists());
    }

    #[tokio::test]
    async fn test_import_skill_from_zip_with_root_folder() {
        // Simulate a GitHub-style archive with a root folder
        let zip_data = create_zip(&[
            (
                "my-skill-main/SKILL.md",
                "---\ndescription: GitHub style\n---\n\n# Test",
            ),
            ("my-skill-main/refs/doc.md", "Documentation"),
        ]);

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        import_skill_from_zip(&skill_dir, &zip_data).await.unwrap();

        // Root folder prefix should be stripped
        assert!(skill_dir.join("SKILL.md").exists());
        let content = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(content.contains("GitHub style"));

        // Nested file should also have prefix stripped
        assert!(skill_dir.join("refs/doc.md").exists());
    }

    #[tokio::test]
    async fn test_import_skill_from_zip_skips_hidden_files() {
        let zip_data = create_zip(&[
            ("SKILL.md", "# Test"),
            (".gitignore", "*.log"),
            ("refs/.hidden", "secret"),
        ]);

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        import_skill_from_zip(&skill_dir, &zip_data).await.unwrap();

        // SKILL.md should exist
        assert!(skill_dir.join("SKILL.md").exists());

        // Hidden files should be skipped
        assert!(!skill_dir.join(".gitignore").exists());
        assert!(!skill_dir.join("refs/.hidden").exists());
    }
}
