//! Workspace management for OpenCode sessions.
//!
//! Open Agent acts as a workspace host for OpenCode. This module creates
//! per-task/mission workspace directories and writes `opencode.json`
//! with the currently configured MCP servers.
//!
//! ## Workspace Types
//!
//! - **Host**: Execute directly on the remote host environment
//! - **Chroot**: Execute inside an isolated chroot environment

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use crate::chroot::{self, ChrootDistro};
use crate::config::Config;
use crate::mcp::{McpRegistry, McpServerConfig, McpTransport};

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Types
// ─────────────────────────────────────────────────────────────────────────────

/// The nil UUID represents the default "host" workspace.
pub const DEFAULT_WORKSPACE_ID: Uuid = Uuid::nil();

/// Type of workspace execution environment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceType {
    /// Execute directly on remote host
    Host,
    /// Execute inside isolated chroot environment
    Chroot,
}

impl Default for WorkspaceType {
    fn default() -> Self {
        Self::Host
    }
}

/// Status of a workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    /// Chroot not yet built
    Pending,
    /// Chroot build in progress
    Building,
    /// Ready for execution
    Ready,
    /// Build failed
    Error,
}

impl Default for WorkspaceStatus {
    fn default() -> Self {
        Self::Ready
    }
}

/// A workspace definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique identifier
    pub id: Uuid,
    /// Human-readable name
    pub name: String,
    /// Type of workspace (Host or Chroot)
    pub workspace_type: WorkspaceType,
    /// Working directory within the workspace
    pub path: PathBuf,
    /// Current status
    pub status: WorkspaceStatus,
    /// Error message if status is Error
    pub error_message: Option<String>,
    /// Additional configuration
    #[serde(default)]
    pub config: serde_json::Value,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl Workspace {
    /// Create the default host workspace.
    pub fn default_host(working_dir: PathBuf) -> Self {
        Self {
            id: DEFAULT_WORKSPACE_ID,
            name: "host".to_string(),
            workspace_type: WorkspaceType::Host,
            path: working_dir,
            status: WorkspaceStatus::Ready,
            error_message: None,
            config: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }

    /// Create a new chroot workspace (pending build).
    pub fn new_chroot(name: String, path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            workspace_type: WorkspaceType::Chroot,
            path,
            status: WorkspaceStatus::Pending,
            error_message: None,
            config: serde_json::json!({}),
            created_at: Utc::now(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Store
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory store for workspaces.
pub struct WorkspaceStore {
    workspaces: RwLock<HashMap<Uuid, Workspace>>,
}

impl WorkspaceStore {
    /// Create a new workspace store with the default host workspace.
    pub fn new(working_dir: PathBuf) -> Self {
        let mut workspaces = HashMap::new();
        let host = Workspace::default_host(working_dir);
        workspaces.insert(host.id, host);

        Self {
            workspaces: RwLock::new(workspaces),
        }
    }

    /// List all workspaces.
    pub async fn list(&self) -> Vec<Workspace> {
        let guard = self.workspaces.read().await;
        let mut list: Vec<_> = guard.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Get a workspace by ID.
    pub async fn get(&self, id: Uuid) -> Option<Workspace> {
        let guard = self.workspaces.read().await;
        guard.get(&id).cloned()
    }

    /// Get the default host workspace.
    pub async fn get_default(&self) -> Workspace {
        self.get(DEFAULT_WORKSPACE_ID)
            .await
            .expect("Default workspace should always exist")
    }

    /// Add a new workspace.
    pub async fn add(&self, workspace: Workspace) -> Uuid {
        let id = workspace.id;
        let mut guard = self.workspaces.write().await;
        guard.insert(id, workspace);
        id
    }

    /// Update a workspace.
    pub async fn update(&self, workspace: Workspace) -> bool {
        let mut guard = self.workspaces.write().await;
        if guard.contains_key(&workspace.id) {
            guard.insert(workspace.id, workspace);
            true
        } else {
            false
        }
    }

    /// Delete a workspace (cannot delete the default host workspace).
    pub async fn delete(&self, id: Uuid) -> bool {
        if id == DEFAULT_WORKSPACE_ID {
            return false; // Cannot delete default workspace
        }
        let mut guard = self.workspaces.write().await;
        guard.remove(&id).is_some()
    }
}

/// Shared workspace store type.
pub type SharedWorkspaceStore = Arc<WorkspaceStore>;

// ─────────────────────────────────────────────────────────────────────────────
// Original Workspace Utilities
// ─────────────────────────────────────────────────────────────────────────────

fn sanitize_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
        .replace('-', "_")
}

fn unique_key(base: &str, used: &mut std::collections::HashSet<String>) -> String {
    if !used.contains(base) {
        used.insert(base.to_string());
        return base.to_string();
    }
    let mut i = 2;
    loop {
        let candidate = format!("{}_{}", base, i);
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        i += 1;
    }
}

/// Root directory for Open Agent config data (versioned with repo).
pub fn config_root(working_dir: &Path) -> PathBuf {
    working_dir.join(".openagent")
}

/// Root directory for workspace folders.
pub fn workspaces_root(working_dir: &Path) -> PathBuf {
    working_dir.join("workspaces")
}

/// Root directory for workspace folders under a specific workspace path.
pub fn workspaces_root_for(root: &Path) -> PathBuf {
    root.join("workspaces")
}

/// Workspace directory for a mission.
pub fn mission_workspace_dir(working_dir: &Path, mission_id: Uuid) -> PathBuf {
    mission_workspace_dir_for_root(working_dir, mission_id)
}

/// Workspace directory for a task.
pub fn task_workspace_dir(working_dir: &Path, task_id: Uuid) -> PathBuf {
    task_workspace_dir_for_root(working_dir, task_id)
}

/// Workspace directory for a mission under a specific workspace root.
pub fn mission_workspace_dir_for_root(root: &Path, mission_id: Uuid) -> PathBuf {
    let short_id = &mission_id.to_string()[..8];
    workspaces_root_for(root).join(format!("mission-{}", short_id))
}

/// Workspace directory for a task under a specific workspace root.
pub fn task_workspace_dir_for_root(root: &Path, task_id: Uuid) -> PathBuf {
    let short_id = &task_id.to_string()[..8];
    workspaces_root_for(root).join(format!("task-{}", short_id))
}

fn opencode_entry_from_mcp(config: &McpServerConfig, workspace_dir: &Path) -> serde_json::Value {
    match &config.transport {
        McpTransport::Http { endpoint } => json!({
            "type": "http",
            "endpoint": endpoint,
            "enabled": config.enabled,
        }),
        McpTransport::Stdio { command, args, env } => {
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), json!("local"));
            let mut cmd = vec![command.clone()];
            cmd.extend(args.clone());
            entry.insert("command".to_string(), json!(cmd));
            entry.insert("enabled".to_string(), json!(config.enabled));
            let mut merged_env = env.clone();
            merged_env
                .entry("OPEN_AGENT_WORKSPACE".to_string())
                .or_insert_with(|| workspace_dir.to_string_lossy().to_string());
            if !merged_env.is_empty() {
                entry.insert("environment".to_string(), json!(merged_env));
            }
            serde_json::Value::Object(entry)
        }
    }
}

async fn write_opencode_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
) -> anyhow::Result<()> {
    let mut mcp_map = serde_json::Map::new();
    let mut used = std::collections::HashSet::new();

    for config in mcp_configs.into_iter().filter(|c| c.enabled) {
        let base = sanitize_key(&config.name);
        let key = unique_key(&base, &mut used);
        mcp_map.insert(key, opencode_entry_from_mcp(&config, workspace_dir));
    }

    let config_json = json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": mcp_map,
    });

    let config_path = workspace_dir.join("opencode.json");
    tokio::fs::write(config_path, serde_json::to_string_pretty(&config_json)?).await?;
    Ok(())
}

async fn prepare_workspace_dir(path: &Path) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(path.join("output")).await?;
    tokio::fs::create_dir_all(path.join("temp")).await?;
    Ok(path.to_path_buf())
}

/// Prepare a custom workspace directory and write `opencode.json`.
pub async fn prepare_custom_workspace(
    _config: &Config,
    mcp: &McpRegistry,
    workspace_dir: PathBuf,
) -> anyhow::Result<PathBuf> {
    prepare_workspace_dir(&workspace_dir).await?;
    let mcp_configs = mcp.list_configs().await;
    write_opencode_config(&workspace_dir, mcp_configs).await?;
    Ok(workspace_dir)
}

/// Prepare a workspace directory for a mission and write `opencode.json`.
pub async fn prepare_mission_workspace(
    config: &Config,
    mcp: &McpRegistry,
    mission_id: Uuid,
) -> anyhow::Result<PathBuf> {
    prepare_mission_workspace_in(&config.working_dir, mcp, mission_id).await
}

/// Prepare a workspace directory for a mission under a specific workspace root.
pub async fn prepare_mission_workspace_in(
    workspace_root: &Path,
    mcp: &McpRegistry,
    mission_id: Uuid,
) -> anyhow::Result<PathBuf> {
    let dir = mission_workspace_dir_for_root(workspace_root, mission_id);
    prepare_workspace_dir(&dir).await?;
    let mcp_configs = mcp.list_configs().await;
    write_opencode_config(&dir, mcp_configs).await?;
    Ok(dir)
}

/// Prepare a workspace directory for a task and write `opencode.json`.
pub async fn prepare_task_workspace(
    config: &Config,
    mcp: &McpRegistry,
    task_id: Uuid,
) -> anyhow::Result<PathBuf> {
    let dir = task_workspace_dir_for_root(&config.working_dir, task_id);
    prepare_workspace_dir(&dir).await?;
    let mcp_configs = mcp.list_configs().await;
    write_opencode_config(&dir, mcp_configs).await?;
    Ok(dir)
}

/// Regenerate `opencode.json` for all workspace directories.
pub async fn sync_all_workspaces(config: &Config, mcp: &McpRegistry) -> anyhow::Result<usize> {
    let root = workspaces_root(&config.working_dir);
    if !root.exists() {
        return Ok(0);
    }

    let mut count = 0;
    let mcp_configs = mcp.list_configs().await;

    let mut entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if write_opencode_config(&path, mcp_configs.clone())
            .await
            .is_ok()
        {
            count += 1;
        }
    }

    Ok(count)
}

/// Resolve the workspace root path for a mission.
/// Falls back to `config.working_dir` if the workspace is missing.
pub async fn resolve_workspace_root(
    workspaces: &SharedWorkspaceStore,
    config: &Config,
    workspace_id: Option<Uuid>,
) -> PathBuf {
    let id = workspace_id.unwrap_or(DEFAULT_WORKSPACE_ID);
    match workspaces.get(id).await {
        Some(ws) => ws.path,
        None => {
            warn!(
                "Workspace {} not found; using default working_dir {}",
                id,
                config.working_dir.display()
            );
            config.working_dir.clone()
        }
    }
}

/// Build a chroot workspace.
pub async fn build_chroot_workspace(
    workspace: &mut Workspace,
    distro: Option<ChrootDistro>,
) -> anyhow::Result<()> {
    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err(anyhow::anyhow!(
            "Workspace is not a chroot type"
        ));
    }

    // Check if already built
    if chroot::is_chroot_created(&workspace.path).await {
        tracing::info!("Chroot already exists at {}", workspace.path.display());
        workspace.status = WorkspaceStatus::Ready;
        return Ok(());
    }

    // Update status to building
    workspace.status = WorkspaceStatus::Building;

    let distro = distro.unwrap_or_default();

    tracing::info!(
        "Building chroot workspace at {} with distro {}",
        workspace.path.display(),
        distro.as_str()
    );

    // Create the chroot
    match chroot::create_chroot(&workspace.path, distro).await {
        Ok(()) => {
            workspace.status = WorkspaceStatus::Ready;
            workspace.error_message = None;
            tracing::info!("Chroot workspace built successfully");
            Ok(())
        }
        Err(e) => {
            workspace.status = WorkspaceStatus::Error;
            workspace.error_message = Some(format!("Chroot build failed: {}", e));
            tracing::error!("Failed to build chroot: {}", e);
            Err(anyhow::anyhow!("Chroot build failed: {}", e))
        }
    }
}

/// Destroy a chroot workspace.
pub async fn destroy_chroot_workspace(workspace: &Workspace) -> anyhow::Result<()> {
    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err(anyhow::anyhow!(
            "Workspace is not a chroot type"
        ));
    }

    tracing::info!("Destroying chroot workspace at {}", workspace.path.display());

    chroot::destroy_chroot(&workspace.path).await?;

    Ok(())
}
