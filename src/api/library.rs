//! Library management API endpoints.
//!
//! Provides endpoints for managing the configuration library:
//! - Git operations (status, sync, commit, push)
//! - MCP server CRUD
//! - Skills CRUD
//! - Commands CRUD
//! - Plugins CRUD
//! - Rules CRUD
//! - Library Agents CRUD
//! - Library Tools CRUD
//! - OpenCode settings (oh-my-opencode.json)
//! - OpenAgent config (agent visibility, defaults)
//! - Migration

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
    rename::{ItemType, RenameResult},
    ClaudeCodeConfig, Command, CommandSummary, GitAuthor, LibraryAgent, LibraryAgentSummary,
    LibraryStatus, LibraryStore, LibraryTool, LibraryToolSummary, McpServer, MigrationReport,
    OpenAgentConfig, Plugin, Rule, RuleSummary, Skill, SkillSummary, WorkspaceTemplate,
    WorkspaceTemplateSummary,
};
use crate::nspawn::NspawnDistro;
use crate::workspace::{self, WorkspaceType, DEFAULT_WORKSPACE_ID};

/// Shared library state.
pub type SharedLibrary = Arc<RwLock<Option<Arc<LibraryStore>>>>;

const LIBRARY_REMOTE_HEADER: &str = "x-openagent-library-remote";
const GIT_AUTHOR_NAME_HEADER: &str = "x-openagent-git-author-name";
const GIT_AUTHOR_EMAIL_HEADER: &str = "x-openagent-git-author-email";

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
        if is_default_host_workspace(&workspace) || !workspace.tools.is_empty() {
            if let Err(e) = workspace::sync_workspace_tools(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync tools after library update"
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

async fn sync_tool_to_workspaces(
    state: &super::routes::AppState,
    library: &LibraryStore,
    tool_name: &str,
) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if is_default_host_workspace(&workspace) || workspace.tools.iter().any(|t| t == tool_name) {
            if let Err(e) = workspace::sync_workspace_tools(&workspace, library).await {
                tracing::warn!(
                    workspace = %workspace.name,
                    tool = %tool_name,
                    error = %e,
                    "Failed to sync tool to workspace"
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
        // Rules
        .route("/rule", get(list_rules))
        .route("/rule/:name", get(get_rule))
        .route("/rule/:name", put(save_rule))
        .route("/rule/:name", delete(delete_rule))
        // Library Agents
        .route("/agent", get(list_library_agents))
        .route("/agent/:name", get(get_library_agent))
        .route("/agent/:name", put(save_library_agent))
        .route("/agent/:name", delete(delete_library_agent))
        // Library Tools
        .route("/tool", get(list_library_tools))
        .route("/tool/:name", get(get_library_tool))
        .route("/tool/:name", put(save_library_tool))
        .route("/tool/:name", delete(delete_library_tool))
        // Workspace Templates
        .route("/workspace-template", get(list_workspace_templates))
        .route("/workspace-template/:name", get(get_workspace_template))
        .route("/workspace-template/:name", put(save_workspace_template))
        .route(
            "/workspace-template/:name",
            delete(delete_workspace_template),
        )
        // Migration
        .route("/migrate", post(migrate_library))
        // Rename (works for all item types)
        .route("/rename/:item_type/:name", post(rename_item))
        // OpenCode Settings (oh-my-opencode.json)
        .route("/opencode/settings", get(get_opencode_settings))
        .route("/opencode/settings", put(save_opencode_settings))
        // OpenAgent Config
        .route("/openagent/config", get(get_openagent_config))
        .route("/openagent/config", put(save_openagent_config))
        .route("/openagent/agents", get(get_visible_agents))
        // Claude Code Config
        .route("/claudecode/config", get(get_claudecode_config))
        .route("/claudecode/config", put(save_claudecode_config))
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
    /// Git repository URL
    url: String,
    /// Optional path within the repository (for monorepos)
    path: Option<String>,
    /// Target skill name (defaults to last path component)
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SaveWorkspaceTemplateRequest {
    pub description: Option<String>,
    pub distro: Option<String>,
    pub skills: Option<Vec<String>>,
    pub env_vars: Option<HashMap<String, String>>,
    pub encrypted_keys: Option<Vec<String>>,
    pub init_script: Option<String>,
    /// Whether to share the host network (default: true).
    /// Set to false for isolated networking (e.g., Tailscale).
    pub shared_network: Option<bool>,
    /// MCP server names to enable for workspaces created from this template.
    #[serde(default)]
    pub mcps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    /// The new name for the item.
    pub new_name: String,
    /// If true, return what would be changed without actually changing anything.
    #[serde(default)]
    pub dry_run: bool,
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync plugins to global OpenCode config
    let plugins = library
        .get_plugins()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    crate::opencode_config::sync_global_plugins(&plugins)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Sync OpenCode settings (oh-my-opencode.json) from Library to system
    if let Err(e) = workspace::sync_opencode_settings(&library).await {
        tracing::warn!(error = %e, "Failed to sync oh-my-opencode settings during library sync");
    }

    // Sync OpenAgent config from Library to working directory
    if let Err(e) = workspace::sync_openagent_config(&library, &state.config.working_dir).await {
        tracing::warn!(error = %e, "Failed to sync openagent config during library sync");
    }

    // Sync skills and tools to workspaces
    sync_all_workspaces(&state, library.as_ref()).await;

    Ok((StatusCode::OK, "Synced successfully".to_string()))
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

/// POST /api/library/skills/import - Import a skill from a Git URL.
async fn import_skill(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(req): Json<ImportSkillRequest>,
) -> Result<Json<Skill>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;

    // Determine target name
    let target_name = req.name.clone().unwrap_or_else(|| {
        // Extract from path or URL
        if let Some(ref path) = req.path {
            path.rsplit('/')
                .next()
                .unwrap_or("imported-skill")
                .to_string()
        } else {
            req.url
                .rsplit('/')
                .next()
                .map(|s| s.trim_end_matches(".git"))
                .unwrap_or("imported-skill")
                .to_string()
        }
    });

    let skill = library
        .import_skill_from_git(&req.url, req.path.as_deref(), &target_name)
        .await
        .map_err(|e| {
            if e.to_string().contains("already exists") {
                (StatusCode::CONFLICT, e.to_string())
            } else if e.to_string().contains("No SKILL.md found") {
                (StatusCode::BAD_REQUEST, e.to_string())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        })?;
    sync_skill_to_workspaces(&state, library.as_ref(), &target_name).await;
    Ok(Json(skill))
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
            description: Some("Start self-referential development loop until completion".to_string()),
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
            description: Some("Intelligent refactoring with LSP, AST-grep, and TDD verification".to_string()),
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
// Rules
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/rule - List all rules.
async fn list_rules(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<RuleSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_rules()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/rule/:name - Get a rule by name.
async fn get_rule(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Rule>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library.get_rule(&name).await.map(Json).map_err(|e| {
        if e.to_string().contains("not found") {
            (StatusCode::NOT_FOUND, e.to_string())
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    })
}

/// PUT /api/library/rule/:name - Save a rule.
async fn save_rule(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_rule(&name, &req.content)
        .await
        .map(|_| (StatusCode::OK, "Rule saved successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// DELETE /api/library/rule/:name - Delete a rule.
async fn delete_rule(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_rule(&name)
        .await
        .map(|_| (StatusCode::OK, "Rule deleted successfully".to_string()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
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
// Library Tools
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/tool - List all library tools.
async fn list_library_tools(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<LibraryToolSummary>>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .list_library_tools()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/library/tool/:name - Get a library tool by name.
async fn get_library_tool(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<LibraryTool>, (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .get_library_tool(&name)
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

/// PUT /api/library/tool/:name - Save a library tool.
async fn save_library_tool(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SaveContentRequest>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .save_library_tool(&name, &req.content)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sync_tool_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Tool saved successfully".to_string()))
}

/// DELETE /api/library/tool/:name - Delete a library tool.
async fn delete_library_tool(
    State(state): State<Arc<super::routes::AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    let library = ensure_library(&state, &headers).await?;
    library
        .delete_library_tool(&name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sync_tool_to_workspaces(&state, library.as_ref(), &name).await;
    Ok((StatusCode::OK, "Tool deleted successfully".to_string()))
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
        init_script: req.init_script.unwrap_or_default(),
        shared_network: req.shared_network,
        mcps: req.mcps.unwrap_or_default(),
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
// OpenAgent Config
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library/openagent/config - Get OpenAgent config from Library.
async fn get_openagent_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
) -> Result<Json<OpenAgentConfig>, (StatusCode, String)> {
    match ensure_library(&state, &headers).await {
        Ok(library) => library
            .get_openagent_config()
            .await
            .map(Json)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {
            let config = workspace::read_openagent_config(&state.config.working_dir).await;
            Ok(Json(config))
        }
        Err(e) => Err(e),
    }
}

/// PUT /api/library/openagent/config - Save OpenAgent config to Library.
async fn save_openagent_config(
    State(state): State<Arc<super::routes::AppState>>,
    headers: HeaderMap,
    Json(config): Json<OpenAgentConfig>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    match ensure_library(&state, &headers).await {
        Ok(library) => {
            library
                .save_openagent_config(&config)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            // Sync to working directory
            if let Err(e) = workspace::sync_openagent_config(&library, &state.config.working_dir)
                .await
            {
                tracing::warn!(error = %e, "Failed to sync openagent config to working dir");
            }

            Ok((
                StatusCode::OK,
                "OpenAgent config saved successfully".to_string(),
            ))
        }
        Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {
            if let Err(e) = workspace::write_openagent_config(&state.config.working_dir, &config)
                .await
            {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to write openagent config locally: {}", e),
                ));
            }
            Ok((
                StatusCode::OK,
                "OpenAgent config saved locally (Library not configured)".to_string(),
            ))
        }
        Err(e) => Err(e),
    }
}

/// GET /api/library/openagent/agents - Get filtered list of visible agents.
/// Fetches agents from OpenCode and filters by hidden_agents config.
async fn get_visible_agents(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Read current config from working directory
    let config = workspace::read_openagent_config(&state.config.working_dir).await;

    // Fetch all agents from OpenCode
    let all_agents = crate::api::opencode::fetch_opencode_agents(&state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let visible_agents = filter_visible_agents_with_fallback(all_agents.clone(), &config);

    Ok(Json(visible_agents))
}

fn filter_visible_agents_with_fallback(
    agents: serde_json::Value,
    config: &OpenAgentConfig,
) -> serde_json::Value {
    filter_agents_by_config(agents, config)
}

/// Filter agents based on OpenAgent config hidden_agents list.
fn filter_agents_by_config(
    agents: serde_json::Value,
    config: &OpenAgentConfig,
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
    let config = crate::workspace::read_openagent_config(&state.config.working_dir).await;
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
        "rule" => ItemType::Rule,
        "agent" => ItemType::Agent,
        "tool" => ItemType::Tool,
        "workspace-template" => ItemType::WorkspaceTemplate,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid item type '{}'. Valid types: skill, command, rule, agent, tool, workspace-template",
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
            ItemType::Tool => {
                // Update workspace tool lists
                update_workspace_tool_references(&state, &name, &req.new_name).await;
                // Sync tools to workspaces
                sync_tool_to_workspaces(&state, library.as_ref(), &req.new_name).await;
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

/// Update workspace tool references when a tool is renamed.
async fn update_workspace_tool_references(
    state: &super::routes::AppState,
    old_name: &str,
    new_name: &str,
) {
    let workspaces = state.workspaces.list().await;
    for workspace in workspaces {
        if workspace.tools.contains(&old_name.to_string()) {
            let mut updated_workspace = workspace.clone();
            updated_workspace.tools = updated_workspace
                .tools
                .iter()
                .map(|t| {
                    if t == old_name {
                        new_name.to_string()
                    } else {
                        t.clone()
                    }
                })
                .collect();

            let workspace_name = workspace.name.clone();
            if !state.workspaces.update(updated_workspace).await {
                tracing::warn!(
                    workspace = %workspace_name,
                    "Failed to update workspace tool reference"
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
