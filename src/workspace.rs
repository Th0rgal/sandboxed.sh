//! Workspace management for OpenCode sessions.
//!
//! Open Agent acts as a workspace host for OpenCode. This module creates
//! per-task/mission workspace directories and writes `opencode.json`
//! with the currently configured MCP servers.
//!
//! ## Workspace Types
//!
//! - **Host**: Execute directly on the remote host environment
//! - **Chroot**: Execute inside an isolated container environment (systemd-nspawn)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_recursion::async_recursion;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use crate::config::Config;
use crate::library::LibraryStore;
use crate::mcp::{McpRegistry, McpScope, McpServerConfig, McpTransport};
use crate::nspawn::{self, NspawnDistro};

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
    /// Execute inside isolated container environment
    Chroot,
}

impl Default for WorkspaceType {
    fn default() -> Self {
        Self::Host
    }
}

impl WorkspaceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Chroot => "chroot",
        }
    }
}

/// Status of a workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    /// Container not yet built
    Pending,
    /// Container build in progress
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
    /// Type of workspace (Host or Container)
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
    /// Workspace template name (if created from a template)
    #[serde(default)]
    pub template: Option<String>,
    /// Preferred Linux distribution for container workspaces
    #[serde(default)]
    pub distro: Option<String>,
    /// Environment variables always loaded for this workspace
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    /// Init script to run when the workspace is built/rebuilt
    #[serde(default)]
    pub init_script: Option<String>,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Skill names from library to sync to this workspace
    #[serde(default)]
    pub skills: Vec<String>,
    /// Tool names from library to sync to this workspace
    #[serde(default)]
    pub tools: Vec<String>,
    /// Plugin identifiers for hooks
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Whether to share the host network (default: true).
    /// When true, bind-mounts /etc/resolv.conf for DNS.
    /// Set to false for isolated networking (e.g., Tailscale).
    #[serde(default)]
    pub shared_network: Option<bool>,
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
            template: None,
            distro: None,
            env_vars: HashMap::new(),
            init_script: None,
            created_at: Utc::now(),
            skills: Vec::new(),
            tools: Vec::new(),
            plugins: Vec::new(),
            shared_network: None,
        }
    }

    /// Create a new container workspace (pending build).
    pub fn new_chroot(name: String, path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            workspace_type: WorkspaceType::Chroot,
            path,
            status: WorkspaceStatus::Pending,
            error_message: None,
            config: serde_json::json!({}),
            template: None,
            distro: None,
            env_vars: HashMap::new(),
            init_script: None,
            created_at: Utc::now(),
            skills: Vec::new(),
            tools: Vec::new(),
            plugins: Vec::new(),
            shared_network: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Store
// ─────────────────────────────────────────────────────────────────────────────

/// Persistent store for workspaces with JSON file backing.
pub struct WorkspaceStore {
    workspaces: RwLock<HashMap<Uuid, Workspace>>,
    storage_path: PathBuf,
    working_dir: PathBuf,
}

impl WorkspaceStore {
    /// Create a new workspace store, loading existing data from disk.
    ///
    /// This also scans for orphaned container directories and restores them.
    pub async fn new(working_dir: PathBuf) -> Self {
        let storage_path = working_dir.join(".openagent/workspaces.json");

        let store = Self {
            workspaces: RwLock::new(HashMap::new()),
            storage_path,
            working_dir: working_dir.clone(),
        };

        // Load existing workspaces from disk
        let mut workspaces = match store.load_from_disk() {
            Ok(loaded) => loaded,
            Err(e) => {
                tracing::warn!("Failed to load workspaces from disk: {}", e);
                HashMap::new()
            }
        };

        // Ensure default host workspace exists
        if !workspaces.contains_key(&DEFAULT_WORKSPACE_ID) {
            let host = Workspace::default_host(working_dir.clone());
            workspaces.insert(host.id, host);
        }
        if let Some(host) = workspaces.get_mut(&DEFAULT_WORKSPACE_ID) {
            if !host.skills.is_empty() {
                host.skills.clear();
                tracing::info!(
                    workspace = %host.name,
                    "Cleared default host workspace skills list to allow all library skills"
                );
            }
        }

        // Scan for orphaned containers and restore them
        let orphaned = store.scan_orphaned_containers(&workspaces).await;
        for workspace in orphaned {
            tracing::info!(
                "Restored orphaned container workspace: {} at {}",
                workspace.name,
                workspace.path.display()
            );
            workspaces.insert(workspace.id, workspace);
        }

        // Store workspaces
        {
            let mut guard = store.workspaces.write().await;
            *guard = workspaces;
        }

        // Save to disk to persist any recovered workspaces
        if let Err(e) = store.save_to_disk().await {
            tracing::error!("Failed to save workspaces to disk: {}", e);
        }

        store
    }

    /// Load workspaces from disk.
    fn load_from_disk(&self) -> Result<HashMap<Uuid, Workspace>, std::io::Error> {
        if !self.storage_path.exists() {
            return Ok(HashMap::new());
        }

        let contents = std::fs::read_to_string(&self.storage_path)?;
        let workspaces: Vec<Workspace> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(workspaces.into_iter().map(|w| (w.id, w)).collect())
    }

    /// Save workspaces to disk.
    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let workspaces = self.workspaces.read().await;
        let workspaces_vec: Vec<&Workspace> = workspaces.values().collect();

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&workspaces_vec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }

    /// Scan for container directories that exist on disk but aren't in the store.
    async fn scan_orphaned_containers(&self, known: &HashMap<Uuid, Workspace>) -> Vec<Workspace> {
        let containers_dir = self.working_dir.join(".openagent/containers");

        if !containers_dir.exists() {
            return Vec::new();
        }

        // Get all known container paths
        let known_paths: std::collections::HashSet<PathBuf> = known
            .values()
            .filter(|w| w.workspace_type == WorkspaceType::Chroot)
            .map(|w| w.path.clone())
            .collect();

        let mut orphaned = Vec::new();

        for root in [containers_dir] {
            if !root.exists() {
                continue;
            }

            let entries = match std::fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(
                        "Failed to read containers directory {}: {}",
                        root.display(),
                        e
                    );
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();

                // Skip non-directories
                if !path.is_dir() {
                    continue;
                }

                // Check if this path is known
                if known_paths.contains(&path) {
                    continue;
                }

                // Get the directory name as workspace name
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                // Check if it looks like a valid container (has basic structure)
                let is_valid_container = path.join("etc").exists() || path.join("bin").exists();

                // Determine status based on filesystem state
                let status = if is_valid_container {
                    WorkspaceStatus::Ready
                } else {
                    // Incomplete container - might have been interrupted
                    WorkspaceStatus::Pending
                };

                let workspace = Workspace {
                    id: Uuid::new_v4(),
                    name,
                    workspace_type: WorkspaceType::Chroot,
                    path,
                    status,
                    error_message: None,
                    config: serde_json::json!({}),
                    template: None,
                    distro: None,
                    env_vars: HashMap::new(),
                    init_script: None,
                    created_at: Utc::now(), // We don't know the actual creation time
                    skills: Vec::new(),
                    tools: Vec::new(),
                    plugins: Vec::new(),
                    shared_network: None, // Default to shared network
                };

                orphaned.push(workspace);
            }
        }

        orphaned
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
        {
            let mut guard = self.workspaces.write().await;
            guard.insert(id, workspace);
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save workspaces to disk: {}", e);
        }

        id
    }

    /// Update a workspace.
    pub async fn update(&self, workspace: Workspace) -> bool {
        let updated = {
            let mut guard = self.workspaces.write().await;
            if guard.contains_key(&workspace.id) {
                guard.insert(workspace.id, workspace);
                true
            } else {
                false
            }
        };

        if updated {
            if let Err(e) = self.save_to_disk().await {
                tracing::error!("Failed to save workspaces to disk: {}", e);
            }
        }

        updated
    }

    /// Delete a workspace (cannot delete the default host workspace).
    pub async fn delete(&self, id: Uuid) -> bool {
        if id == DEFAULT_WORKSPACE_ID {
            return false; // Cannot delete default workspace
        }

        let existed = {
            let mut guard = self.workspaces.write().await;
            guard.remove(&id).is_some()
        };

        if existed {
            if let Err(e) = self.save_to_disk().await {
                tracing::error!("Failed to save workspaces to disk: {}", e);
            }
        }

        existed
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

fn opencode_entry_from_mcp(
    config: &McpServerConfig,
    workspace_dir: &Path,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    shared_network: Option<bool>,
) -> serde_json::Value {
    fn resolve_command_path(cmd: &str) -> String {
        let cmd_path = Path::new(cmd);
        if cmd_path.is_absolute() || cmd.contains('/') {
            return cmd.to_string();
        }

        let candidates = [
            Path::new("/usr/local/bin").join(cmd),
            Path::new("/usr/bin").join(cmd),
        ];

        for candidate in candidates.iter() {
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }

        cmd.to_string()
    }

    match &config.transport {
        McpTransport::Http { endpoint, headers } => {
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), json!("http"));
            entry.insert("endpoint".to_string(), json!(endpoint));
            entry.insert("enabled".to_string(), json!(config.enabled));
            if !headers.is_empty() {
                entry.insert("headers".to_string(), json!(headers));
            }
            json!(entry)
        }
        McpTransport::Stdio { command, args, env } => {
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), json!("local"));

            let mut merged_env = env.clone();
            if !workspace_env.is_empty() {
                for (key, value) in workspace_env {
                    merged_env
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                let workspace_env_json =
                    serde_json::to_string(workspace_env).unwrap_or_else(|_| "{}".to_string());
                merged_env
                    .entry("OPEN_AGENT_WORKSPACE_ENV_VARS".to_string())
                    .or_insert(workspace_env_json);
            }
            merged_env
                .entry("OPEN_AGENT_WORKSPACE".to_string())
                .or_insert_with(|| workspace_dir.to_string_lossy().to_string());
            merged_env
                .entry("OPEN_AGENT_WORKSPACE_ROOT".to_string())
                .or_insert_with(|| workspace_root.to_string_lossy().to_string());
            merged_env
                .entry("OPEN_AGENT_WORKSPACE_TYPE".to_string())
                .or_insert_with(|| workspace_type.as_str().to_string());
            merged_env
                .entry("WORKING_DIR".to_string())
                .or_insert_with(|| workspace_dir.to_string_lossy().to_string());
            if workspace_type == WorkspaceType::Chroot {
                if let Some(name) = workspace_root.file_name().and_then(|n| n.to_str()) {
                    if !name.trim().is_empty() {
                        merged_env
                            .entry("OPEN_AGENT_WORKSPACE_NAME".to_string())
                            .or_insert_with(|| name.to_string());
                    }
                }
            }
            if let Ok(runtime_workspace_file) = std::env::var("OPEN_AGENT_RUNTIME_WORKSPACE_FILE") {
                if !runtime_workspace_file.trim().is_empty() {
                    merged_env
                        .entry("OPEN_AGENT_RUNTIME_WORKSPACE_FILE".to_string())
                        .or_insert(runtime_workspace_file);
                }
            }

            let use_nspawn =
                config.scope == McpScope::Workspace && workspace_type == WorkspaceType::Chroot;

            if use_nspawn {
                let rel = workspace_dir
                    .strip_prefix(workspace_root)
                    .unwrap_or_else(|_| Path::new(""));
                let rel_str = if rel.as_os_str().is_empty() {
                    "/".to_string()
                } else {
                    format!("/{}", rel.to_string_lossy())
                };

                let mut nspawn_env = merged_env.clone();
                nspawn_env.insert("OPEN_AGENT_WORKSPACE".to_string(), rel_str.clone());
                nspawn_env.insert("OPEN_AGENT_WORKSPACE_ROOT".to_string(), "/".to_string());
                nspawn_env.insert("WORKING_DIR".to_string(), rel_str.clone());

                let mut cmd = vec![
                    resolve_command_path("systemd-nspawn"),
                    "-D".to_string(),
                    workspace_root.to_string_lossy().to_string(),
                    "--quiet".to_string(),
                    "--timezone=off".to_string(),
                    "--console=pipe".to_string(),
                    "--chdir".to_string(),
                    rel_str,
                ];
                // For chroot workspaces, bind-mount the GLOBAL context root into the container.
                // Mission context files are stored in the global context root (e.g., /root/context/{mission_id}),
                // NOT in a workspace-specific directory. The global context root must be bind-mounted
                // so that the symlink inside the container (`context -> /root/context/{mission_id}`) resolves.
                let context_dir_name = std::env::var("OPEN_AGENT_CONTEXT_DIR_NAME")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "context".to_string());
                // Get the global context root from env var, falling back to /root/context
                let global_context_root = std::env::var("OPEN_AGENT_CONTEXT_ROOT")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/root").join(&context_dir_name));
                // Create the context directory if it doesn't exist
                let _ = std::fs::create_dir_all(&global_context_root);
                if global_context_root.exists() {
                    cmd.push(format!(
                        "--bind={}:/root/context",
                        global_context_root.display()
                    ));
                    nspawn_env.insert(
                        "OPEN_AGENT_CONTEXT_ROOT".to_string(),
                        "/root/context".to_string(),
                    );
                    nspawn_env.insert("OPEN_AGENT_CONTEXT_DIR_NAME".to_string(), context_dir_name);
                }

                // Network configuration based on shared_network setting:
                // - shared_network=true (default): Share host network, bind-mount /etc/resolv.conf for DNS
                // - shared_network=false: Isolated network (--network-veth), used with Tailscale
                let use_shared_network = shared_network.unwrap_or(true);
                if use_shared_network {
                    // Bind-mount host's resolv.conf for DNS resolution in shared network mode
                    cmd.push("--bind-ro=/etc/resolv.conf".to_string());
                } else {
                    // Isolated network mode - check if Tailscale is configured
                    let tailscale_args = nspawn::tailscale_nspawn_extra_args(&merged_env);
                    if tailscale_args.is_empty() {
                        // Tailscale not configured - fall back to binding resolv.conf for DNS
                        // This ensures DNS works even if the user sets shared_network=false
                        // without proper Tailscale configuration
                        cmd.push("--bind-ro=/etc/resolv.conf".to_string());
                    } else {
                        // Tailscale configured - it handles networking and DNS
                        cmd.extend(tailscale_args);
                    }
                }
                for (key, value) in &nspawn_env {
                    cmd.push(format!("--setenv={}={}", key, value));
                }
                cmd.push(command.clone());
                cmd.extend(args.clone());
                entry.insert("command".to_string(), json!(cmd));
            } else {
                let mut cmd = vec![resolve_command_path(command)];
                cmd.extend(args.clone());
                entry.insert("command".to_string(), json!(cmd));
                if !merged_env.is_empty() {
                    entry.insert("environment".to_string(), json!(merged_env));
                }
            }
            entry.insert("enabled".to_string(), json!(config.enabled));
            serde_json::Value::Object(entry)
        }
    }
}

fn claude_entry_from_mcp(
    config: &McpServerConfig,
    workspace_dir: &Path,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    shared_network: Option<bool>,
) -> serde_json::Value {
    match &config.transport {
        McpTransport::Http { endpoint, headers } => {
            let mut entry = serde_json::Map::new();
            entry.insert("url".to_string(), json!(endpoint));
            if !headers.is_empty() {
                entry.insert("headers".to_string(), json!(headers));
            }
            serde_json::Value::Object(entry)
        }
        McpTransport::Stdio { .. } => {
            let opencode_entry = opencode_entry_from_mcp(
                config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            );

            let command_vec = opencode_entry
                .get("command")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let command = command_vec
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let args: Vec<String> = command_vec
                .iter()
                .skip(1)
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();

            let mut entry = serde_json::Map::new();
            entry.insert("command".to_string(), json!(command));
            entry.insert("args".to_string(), json!(args));

            if let Some(env) = opencode_entry
                .get("environment")
                .and_then(|v| v.as_object())
            {
                entry.insert("env".to_string(), serde_json::Value::Object(env.clone()));
            }

            serde_json::Value::Object(entry)
        }
    }
}

async fn write_opencode_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_allowlist: Option<&[String]>,
    shared_network: Option<bool>,
) -> anyhow::Result<()> {
    let mut mcp_map = serde_json::Map::new();
    let mut used = std::collections::HashSet::new();

    let filtered_configs = mcp_configs.into_iter().filter(|c| {
        if !c.enabled {
            return false;
        }
        true
    });

    for config in filtered_configs {
        let base = sanitize_key(&config.name);
        let key = unique_key(&base, &mut used);
        mcp_map.insert(
            key,
            opencode_entry_from_mcp(
                &config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            ),
        );
    }

    let mut config_json = serde_json::Map::new();
    config_json.insert(
        "$schema".to_string(),
        json!("https://opencode.ai/config.json"),
    );
    config_json.insert("mcp".to_string(), serde_json::Value::Object(mcp_map));

    if let Some(skills) = skill_allowlist {
        if !skills.is_empty() {
            let mut skill_permissions = serde_json::Map::new();
            skill_permissions.insert("*".to_string(), json!("deny"));
            for skill in skills {
                skill_permissions.insert(skill.clone(), json!("allow"));
            }
            let mut permission = serde_json::Map::new();
            permission.insert(
                "skill".to_string(),
                serde_json::Value::Object(skill_permissions),
            );
            config_json.insert(
                "permission".to_string(),
                serde_json::Value::Object(permission),
            );
        }
    }

    // Tool policy:
    // - We want shell/file effects scoped to the workspace by running the agent process
    //   inside the workspace execution context (host/chroot/ssh).
    // - Therefore, OpenCode built-in bash MUST be enabled for all workspace types.
    // - The legacy workspace-mcp/desktop-mcp proxy tools are no longer required for core flows.
    let mut tools = serde_json::Map::new();
    match workspace_type {
        WorkspaceType::Chroot => {
            // Container workspace: OpenCode runs inside the container, so built-in bash is safe.
            tools.insert("Bash".to_string(), json!(true));
            tools.insert("bash".to_string(), json!(true));
            // Disable legacy MCP tool namespaces by default.
            tools.insert("workspace_*".to_string(), json!(false));
            tools.insert("desktop_*".to_string(), json!(false));
            tools.insert("playwright_*".to_string(), json!(true));
            tools.insert("browser_*".to_string(), json!(true));
        }
        WorkspaceType::Host => {
            tools.insert("Bash".to_string(), json!(true));
            tools.insert("bash".to_string(), json!(true));
            tools.insert("workspace_*".to_string(), json!(false));
            tools.insert("desktop_*".to_string(), json!(false));
            tools.insert("playwright_*".to_string(), json!(false));
            tools.insert("browser_*".to_string(), json!(false));
        }
    }
    config_json.insert("tools".to_string(), serde_json::Value::Object(tools));

    let config_value = serde_json::Value::Object(config_json);
    let config_payload = serde_json::to_string_pretty(&config_value)?;

    // Write to workspace root
    let config_path = workspace_dir.join("opencode.json");
    tokio::fs::write(&config_path, &config_payload).await?;

    // Also write to .opencode/ for OpenCode config discovery
    let opencode_dir = workspace_dir.join(".opencode");
    tokio::fs::create_dir_all(&opencode_dir).await?;
    let opencode_config_path = opencode_dir.join("opencode.json");
    tokio::fs::write(opencode_config_path, config_payload).await?;
    Ok(())
}

/// Write Claude Code configuration to the workspace.
/// Generates `.claude/settings.local.json` and `CLAUDE.md` files.
async fn write_claudecode_config(
    workspace_dir: &Path,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_contents: Option<&[SkillContent]>,
    shared_network: Option<bool>,
) -> anyhow::Result<()> {
    // Create .claude directory
    let claude_dir = workspace_dir.join(".claude");
    tokio::fs::create_dir_all(&claude_dir).await?;

    // Build MCP servers config in Claude Code format
    let mut mcp_servers = serde_json::Map::new();
    let mut used = std::collections::HashSet::new();

    let filtered_configs = mcp_configs.into_iter().filter(|c| c.enabled);

    for config in filtered_configs {
        let base = sanitize_key(&config.name);
        let key = unique_key(&base, &mut used);
        mcp_servers.insert(
            key,
            claude_entry_from_mcp(
                &config,
                workspace_dir,
                workspace_root,
                workspace_type,
                workspace_env,
                shared_network,
            ),
        );
    }

    // Write settings.local.json
    // Add permissive settings to avoid permission prompts.
    //
    // IMPORTANT: Claude Code permission syntax:
    // - "Bash" (no parentheses) allows ALL bash commands
    // - "Bash(*)" does NOT work as a wildcard - it's a literal pattern
    // - "mcp__*" works for MCP tools as a wildcard
    //
    // Tool policy:
    // - Claude Code CLI is executed inside the workspace execution context.
    // - Therefore, built-in Bash is safe to allow for both host + container workspaces.
    // - Legacy MCP tools are still allowed as a wildcard for compatibility.
    let permissions: Vec<&str> = match workspace_type {
        WorkspaceType::Chroot => vec!["Bash", "Edit", "Write", "Read", "mcp__*"],
        WorkspaceType::Host => vec!["Bash", "Edit", "Write", "Read", "mcp__*"],
    };
    let settings = json!({
        "mcpServers": mcp_servers,
        "permissions": {
            "allow": permissions
        }
    });
    let settings_path = claude_dir.join("settings.local.json");
    let settings_content = serde_json::to_string_pretty(&settings)?;
    tokio::fs::write(&settings_path, settings_content).await?;

    // Generate CLAUDE.md from skills
    if let Some(skills) = skill_contents {
        let claude_md_path = workspace_dir.join("CLAUDE.md");
        if skills.is_empty() {
            let _ = tokio::fs::remove_file(&claude_md_path).await;
        } else {
            let mut claude_md = String::new();
            claude_md.push_str("# Project Context\n\n");
            claude_md.push_str(
                "This file was generated by Open Agent. It contains context from configured skills.\n\n",
            );

            // Add workspace instructions for using MCP bash tool
            match workspace_type {
                WorkspaceType::Chroot => {
                    claude_md.push_str("## Container Workspace\n\n");
                    claude_md.push_str(
                        "This is an isolated container workspace. Shell commands execute inside the container workspace context.\n\n\
                        Use the built-in `Bash` tool for shell commands.\n\n",
                    );
                }
                WorkspaceType::Host => {
                    claude_md.push_str("## Host Workspace\n\n");
                    claude_md.push_str(
                        "This is a host workspace. You can use the built-in `Bash` tool to run shell commands directly.\n\n",
                    );
                }
            }

            for skill in skills {
                claude_md.push_str(&format!("## {}\n\n", skill.name));
                // Strip frontmatter if present
                let content = if skill.content.starts_with("---") {
                    if let Some(end) = skill.content[3..].find("---") {
                        skill.content[3 + end + 3..].trim_start().to_string()
                    } else {
                        skill.content.clone()
                    }
                } else {
                    skill.content.clone()
                };
                claude_md.push_str(&content);
                claude_md.push_str("\n\n");
            }

            tokio::fs::write(&claude_md_path, claude_md).await?;
        }
    }

    Ok(())
}

/// Write backend-specific configuration to the workspace.
/// This is the main entry point for config generation.
pub async fn write_backend_config(
    workspace_dir: &Path,
    backend_id: &str,
    mcp_configs: Vec<McpServerConfig>,
    workspace_root: &Path,
    workspace_type: WorkspaceType,
    workspace_env: &HashMap<String, String>,
    skill_allowlist: Option<&[String]>,
    skill_contents: Option<&[SkillContent]>,
    shared_network: Option<bool>,
) -> anyhow::Result<()> {
    match backend_id {
        "opencode" => {
            write_opencode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                shared_network,
            )
            .await
        }
        "claudecode" => {
            // Keep OpenCode config in sync for compatibility with existing execution pipeline.
            write_opencode_config(
                workspace_dir,
                mcp_configs.clone(),
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                shared_network,
            )
            .await?;
            write_claudecode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_contents,
                shared_network,
            )
            .await
        }
        _ => {
            // Unknown backend - write OpenCode config as fallback
            tracing::warn!(
                backend = backend_id,
                "Unknown backend, falling back to OpenCode config"
            );
            write_opencode_config(
                workspace_dir,
                mcp_configs,
                workspace_root,
                workspace_type,
                workspace_env,
                skill_allowlist,
                shared_network,
            )
            .await
        }
    }
}

/// Skill content to be written to the workspace.
pub struct SkillContent {
    /// Skill name (folder name)
    pub name: String,
    /// Primary SKILL.md content
    pub content: String,
    /// Additional markdown files (relative path, content)
    /// Path preserves subdirectory structure (e.g., "references/guide.md")
    pub files: Vec<(String, String)>,
}

/// Ensure the skill content has a `name` field in the YAML frontmatter.
/// OpenCode requires `name` field for skill discovery.
fn ensure_skill_name_in_frontmatter(content: &str, skill_name: &str) -> String {
    // Check if the content starts with YAML frontmatter
    if !content.starts_with("---") {
        // No frontmatter, add it with name field
        return format!("---\nname: {}\n---\n{}", skill_name, content);
    }

    // Find the end of frontmatter
    if let Some(end_idx) = content[3..].find("---") {
        let frontmatter = &content[3..3 + end_idx];
        let rest = &content[3 + end_idx..];

        // Check if name field already exists
        let has_name = frontmatter.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("name:") || trimmed.starts_with("name :")
        });

        if has_name {
            // Name already present, return as-is
            return content.to_string();
        }

        // Insert name field after the opening ---
        // Ensure there's a newline before the closing ---
        return format!(
            "---\nname: {}\n{}\n{}",
            skill_name,
            frontmatter.trim(),
            rest.trim_start_matches('\n')
        );
    }

    // Malformed frontmatter, return as-is
    content.to_string()
}

/// Write skill files to the workspace's `.opencode/skill/` directory.
/// This makes skills available to OpenCode when running in this workspace.
/// OpenCode looks for skills in `.opencode/{skill,skills}/**/SKILL.md`
pub async fn write_skills_to_workspace(
    workspace_dir: &Path,
    skills: &[SkillContent],
) -> anyhow::Result<()> {
    if skills.is_empty() {
        return Ok(());
    }

    let skills_dir = workspace_dir.join(".opencode").join("skill");
    tokio::fs::create_dir_all(&skills_dir).await?;

    for skill in skills {
        let skill_dir = skills_dir.join(&skill.name);
        tokio::fs::create_dir_all(&skill_dir).await?;

        // Ensure skill content has required `name` field in frontmatter
        let content_with_name = ensure_skill_name_in_frontmatter(&skill.content, &skill.name);

        // Write SKILL.md
        let skill_md_path = skill_dir.join("SKILL.md");
        tokio::fs::write(&skill_md_path, &content_with_name).await?;

        // Write additional files (preserving subdirectory structure)
        for (relative_path, file_content) in &skill.files {
            let file_path = skill_dir.join(relative_path);
            // Create parent directories if needed (e.g., "references/guide.md")
            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&file_path, file_content).await?;
        }

        tracing::debug!(
            skill = %skill.name,
            workspace = %workspace_dir.display(),
            "Wrote skill to workspace"
        );
    }

    tracing::info!(
        count = skills.len(),
        workspace = %workspace_dir.display(),
        "Wrote skills to workspace"
    );

    Ok(())
}

async fn resolve_workspace_skill_names(
    workspace: &Workspace,
    library: &LibraryStore,
) -> anyhow::Result<Vec<String>> {
    if !workspace.skills.is_empty() {
        return Ok(workspace.skills.clone());
    }

    // Default host workspace should expose all library skills when none are explicitly configured.
    if workspace.id == DEFAULT_WORKSPACE_ID && workspace.workspace_type == WorkspaceType::Host {
        let skills = library.list_skills().await?;
        let names: Vec<String> = skills.into_iter().map(|skill| skill.name).collect();
        tracing::debug!(
            workspace = %workspace.name,
            count = names.len(),
            "Using all library skills for default host workspace"
        );
        return Ok(names);
    }

    Ok(Vec::new())
}

async fn resolve_workspace_tool_names(
    workspace: &Workspace,
    library: &LibraryStore,
) -> anyhow::Result<Vec<String>> {
    if !workspace.tools.is_empty() {
        return Ok(workspace.tools.clone());
    }

    // Default host workspace should expose all library tools when none are explicitly configured.
    if workspace.id == DEFAULT_WORKSPACE_ID && workspace.workspace_type == WorkspaceType::Host {
        let tools = library.list_library_tools().await?;
        let names: Vec<String> = tools.into_iter().map(|tool| tool.name).collect();
        tracing::debug!(
            workspace = %workspace.name,
            count = names.len(),
            "Using all library tools for default host workspace"
        );
        return Ok(names);
    }

    Ok(Vec::new())
}

/// Sync skills from library to workspace's `.opencode/skill/` directory.
/// Called when workspace is created, updated, or before mission execution.
pub async fn sync_workspace_skills(
    workspace: &Workspace,
    library: &LibraryStore,
) -> anyhow::Result<()> {
    let skill_names = resolve_workspace_skill_names(workspace, library).await?;
    sync_skills_to_dir(&workspace.path, &skill_names, &workspace.name, library).await
}

/// Sync skills from library to a specific directory's `.opencode/skill/` folder.
/// Used for syncing skills to mission directories.
/// This performs a full sync: adds new skills and removes skills no longer in the allowlist.
pub async fn sync_skills_to_dir(
    target_dir: &Path,
    skill_names: &[String],
    context_name: &str,
    library: &LibraryStore,
) -> anyhow::Result<()> {
    let skills_dir = target_dir.join(".opencode").join("skill");

    // Clean up skills that are no longer in the allowlist
    if skills_dir.exists() {
        let allowed: std::collections::HashSet<&str> =
            skill_names.iter().map(|s| s.as_str()).collect();

        if let Ok(mut entries) = tokio::fs::read_dir(&skills_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if !allowed.contains(name) {
                            tracing::info!(
                                skill = %name,
                                context = %context_name,
                                "Removing skill no longer in allowlist"
                            );
                            let _ = tokio::fs::remove_dir_all(&path).await;
                        }
                    }
                }
            }
        }
    }

    if skill_names.is_empty() {
        tracing::debug!(
            context = %context_name,
            "No skills to sync"
        );
        return Ok(());
    }

    let skills_to_write = collect_skill_contents(skill_names, context_name, library).await;

    write_skills_to_workspace(target_dir, &skills_to_write).await?;

    tracing::info!(
        context = %context_name,
        skills = ?skill_names,
        target = %target_dir.display(),
        "Synced skills to directory"
    );

    Ok(())
}

async fn collect_skill_contents(
    skill_names: &[String],
    context_name: &str,
    library: &LibraryStore,
) -> Vec<SkillContent> {
    let mut skills_to_write: Vec<SkillContent> = Vec::new();

    for skill_name in skill_names {
        match library.get_skill(skill_name).await {
            Ok(skill) => {
                skills_to_write.push(SkillContent {
                    name: skill.name,
                    content: skill.content,
                    // Use f.path to preserve subdirectory structure (e.g., "references/guide.md")
                    files: skill
                        .files
                        .into_iter()
                        .map(|f| (f.path, f.content))
                        .collect(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    skill = %skill_name,
                    context = %context_name,
                    error = %e,
                    "Failed to load skill from library, skipping"
                );
            }
        }
    }

    skills_to_write
}

/// Tool content to be written to the workspace.
pub struct ToolContent {
    /// Tool name (filename without .ts)
    pub name: String,
    /// Full TypeScript content
    pub content: String,
}

/// Agent content to be written to the workspace.
pub struct AgentContent {
    /// Agent name (filename without .md)
    pub name: String,
    /// Full markdown content (frontmatter + body)
    pub content: String,
}

/// Write agent files to the workspace's `.opencode/agent/` directory.
/// This makes library agents available to OpenCode when running in this workspace.
pub async fn write_agents_to_workspace(
    workspace_dir: &Path,
    agents: &[AgentContent],
) -> anyhow::Result<()> {
    if agents.is_empty() {
        return Ok(());
    }

    let agents_dir = workspace_dir.join(".opencode").join("agent");
    tokio::fs::create_dir_all(&agents_dir).await?;

    for agent in agents {
        let agent_path = agents_dir.join(format!("{}.md", agent.name));
        tokio::fs::write(&agent_path, &agent.content).await?;

        tracing::debug!(
            agent = %agent.name,
            workspace = %workspace_dir.display(),
            "Wrote agent to workspace"
        );
    }

    tracing::info!(
        count = agents.len(),
        workspace = %workspace_dir.display(),
        "Wrote agents to workspace"
    );

    Ok(())
}

/// Sync library agents to a specific directory's `.opencode/agent/` folder.
pub async fn sync_agents_to_dir(
    target_dir: &Path,
    agent_names: &[String],
    context_name: &str,
    library: &LibraryStore,
) -> anyhow::Result<()> {
    if agent_names.is_empty() {
        tracing::debug!(
            context = %context_name,
            "No agents to sync"
        );
        return Ok(());
    }

    let mut agents_to_write = Vec::new();
    for agent_name in agent_names {
        match library.get_library_agent(agent_name).await {
            Ok(agent) => {
                agents_to_write.push(AgentContent {
                    name: agent.name,
                    content: agent.content,
                });
            }
            Err(e) => {
                tracing::warn!(
                    agent = %agent_name,
                    context = %context_name,
                    error = %e,
                    "Failed to load library agent, skipping"
                );
            }
        }
    }

    write_agents_to_workspace(target_dir, &agents_to_write).await?;

    tracing::info!(
        context = %context_name,
        agents = ?agent_names,
        target = %target_dir.display(),
        "Synced agents to directory"
    );

    Ok(())
}
/// Write tool files to the workspace's `.opencode/tool/` directory.
/// This makes custom tools available to OpenCode when running in this workspace.
/// OpenCode looks for tools in `.opencode/tool/*.ts`
pub async fn write_tools_to_workspace(
    workspace_dir: &Path,
    tools: &[ToolContent],
) -> anyhow::Result<()> {
    if tools.is_empty() {
        return Ok(());
    }

    let tools_dir = workspace_dir.join(".opencode").join("tool");
    tokio::fs::create_dir_all(&tools_dir).await?;

    for tool in tools {
        let tool_path = tools_dir.join(format!("{}.ts", &tool.name));
        tokio::fs::write(&tool_path, &tool.content).await?;

        tracing::debug!(
            tool = %tool.name,
            workspace = %workspace_dir.display(),
            "Wrote tool to workspace"
        );
    }

    tracing::info!(
        count = tools.len(),
        workspace = %workspace_dir.display(),
        "Wrote tools to workspace"
    );

    Ok(())
}

/// Sync tools from library to workspace's `.opencode/tool/` directory.
/// Called when workspace is created, updated, or before mission execution.
/// Default host workspace will include all library tools when none are explicitly configured.
pub async fn sync_workspace_tools(
    workspace: &Workspace,
    library: &LibraryStore,
) -> anyhow::Result<()> {
    let tool_names = resolve_workspace_tool_names(workspace, library).await?;
    sync_tools_to_dir(&workspace.path, &tool_names, &workspace.name, library).await
}

/// Sync tools from library to a specific directory's `.opencode/tool/` folder.
/// Used for syncing tools to mission directories.
/// This performs a full sync: adds new tools and removes tools no longer in the allowlist.
pub async fn sync_tools_to_dir(
    target_dir: &Path,
    tool_names: &[String],
    context_name: &str,
    library: &LibraryStore,
) -> anyhow::Result<()> {
    let tools_dir = target_dir.join(".opencode").join("tool");

    // Clean up tools that are no longer in the allowlist
    if tools_dir.exists() {
        let allowed: std::collections::HashSet<&str> =
            tool_names.iter().map(|s| s.as_str()).collect();

        if let Ok(mut entries) = tokio::fs::read_dir(&tools_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                        if !allowed.contains(name) {
                            tracing::info!(
                                tool = %name,
                                context = %context_name,
                                "Removing tool no longer in allowlist"
                            );
                            let _ = tokio::fs::remove_file(&path).await;
                        }
                    }
                }
            }
        }
    }

    if tool_names.is_empty() {
        tracing::debug!(
            context = %context_name,
            "No tools to sync"
        );
        return Ok(());
    }

    let mut tools_to_write: Vec<ToolContent> = Vec::new();

    for tool_name in tool_names {
        match library.get_library_tool(tool_name).await {
            Ok(tool) => {
                tools_to_write.push(ToolContent {
                    name: tool.name,
                    content: tool.content,
                });
            }
            Err(e) => {
                tracing::warn!(
                    tool = %tool_name,
                    context = %context_name,
                    error = %e,
                    "Failed to load tool from library, skipping"
                );
            }
        }
    }

    write_tools_to_workspace(target_dir, &tools_to_write).await?;

    tracing::info!(
        context = %context_name,
        tools = ?tool_names,
        target = %target_dir.display(),
        "Synced tools to directory"
    );

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
    let workspace_env = HashMap::new();
    write_opencode_config(
        &workspace_dir,
        mcp_configs,
        &workspace_dir,
        WorkspaceType::Host,
        &workspace_env,
        None,
        None, // shared_network: not relevant for host workspaces
    )
    .await?;
    Ok(workspace_dir)
}

/// Prepare a workspace directory for a mission and write `opencode.json`.
pub async fn prepare_mission_workspace(
    config: &Config,
    mcp: &McpRegistry,
    mission_id: Uuid,
) -> anyhow::Result<PathBuf> {
    let default_workspace = Workspace::default_host(config.working_dir.clone());
    prepare_mission_workspace_in(&default_workspace, mcp, mission_id).await
}

/// Prepare a workspace directory for a mission under a specific workspace root.
pub async fn prepare_mission_workspace_in(
    workspace: &Workspace,
    mcp: &McpRegistry,
    mission_id: Uuid,
) -> anyhow::Result<PathBuf> {
    let dir = mission_workspace_dir_for_root(&workspace.path, mission_id);
    prepare_workspace_dir(&dir).await?;
    let mcp_configs = mcp.list_configs().await;
    let skill_allowlist = if workspace.skills.is_empty() {
        None
    } else {
        Some(workspace.skills.as_slice())
    };
    write_opencode_config(
        &dir,
        mcp_configs,
        &workspace.path,
        workspace.workspace_type,
        &workspace.env_vars,
        skill_allowlist,
        workspace.shared_network,
    )
    .await?;
    Ok(dir)
}

/// Prepare a workspace directory for a mission with skill and tool syncing.
/// This version syncs skills and tools from the workspace to the mission directory.
pub async fn prepare_mission_workspace_with_skills(
    workspace: &Workspace,
    mcp: &McpRegistry,
    library: Option<&LibraryStore>,
    mission_id: Uuid,
) -> anyhow::Result<PathBuf> {
    prepare_mission_workspace_with_skills_backend(workspace, mcp, library, mission_id, "opencode")
        .await
}

/// Prepare a workspace directory for a mission with skill and tool syncing for a specific backend.
pub async fn prepare_mission_workspace_with_skills_backend(
    workspace: &Workspace,
    mcp: &McpRegistry,
    library: Option<&LibraryStore>,
    mission_id: Uuid,
    backend_id: &str,
) -> anyhow::Result<PathBuf> {
    let dir = mission_workspace_dir_for_root(&workspace.path, mission_id);
    prepare_workspace_dir(&dir).await?;
    let mcp_configs = mcp.list_configs().await;
    let skill_allowlist = if workspace.skills.is_empty() {
        None
    } else {
        Some(workspace.skills.as_slice())
    };
    let mut skill_contents: Option<Vec<SkillContent>> = None;

    if backend_id == "claudecode" {
        if let Some(lib) = library {
            let context = format!("mission-{}", mission_id);
            let skill_names = resolve_workspace_skill_names(workspace, lib)
                .await
                .unwrap_or_default();
            let skills = collect_skill_contents(&skill_names, &context, lib).await;
            skill_contents = Some(skills);
        }
    }

    write_backend_config(
        &dir,
        backend_id,
        mcp_configs,
        &workspace.path,
        workspace.workspace_type,
        &workspace.env_vars,
        skill_allowlist,
        skill_contents.as_deref(),
        workspace.shared_network,
    )
    .await?;

    // Sync oh-my-opencode settings into the mission directory when using OpenCode.
    if backend_id == "opencode" {
        if let Some(lib) = library {
            match lib.get_opencode_settings().await {
                Ok(settings) => {
                    if !settings.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                        let opencode_dir = dir.join(".opencode");
                        if let Err(e) = tokio::fs::create_dir_all(&opencode_dir).await {
                            tracing::warn!(
                                mission = %mission_id,
                                workspace = %workspace.name,
                                error = %e,
                                "Failed to create .opencode directory for OpenCode settings"
                            );
                        } else {
                            let dest_path = opencode_dir.join("oh-my-opencode.json");
                            match serde_json::to_string_pretty(&settings) {
                                Ok(content) => {
                                    if let Err(e) = tokio::fs::write(&dest_path, content).await {
                                        tracing::warn!(
                                            mission = %mission_id,
                                            workspace = %workspace.name,
                                            error = %e,
                                            "Failed to write oh-my-opencode settings"
                                        );
                                    } else {
                                        tracing::info!(
                                            mission = %mission_id,
                                            path = %dest_path.display(),
                                            "Synced oh-my-opencode settings to mission directory"
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        mission = %mission_id,
                                        workspace = %workspace.name,
                                        error = %e,
                                        "Failed to serialize oh-my-opencode settings"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        mission = %mission_id,
                        workspace = %workspace.name,
                        error = %e,
                        "Failed to load oh-my-opencode settings from library"
                    );
                }
            }
        }
    }

    // Sync skills and tools from workspace to mission directory
    if let Some(lib) = library {
        let context = format!("mission-{}", mission_id);

        // Sync skills
        let skill_names = match resolve_workspace_skill_names(workspace, lib).await {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to resolve skills from library"
                );
                Vec::new()
            }
        };
        if !skill_names.is_empty() {
            if let Err(e) = sync_skills_to_dir(&dir, &skill_names, &context, lib).await {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync skills to mission directory"
                );
            }
        }

        // Sync tools
        let tool_names = match resolve_workspace_tool_names(workspace, lib).await {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to resolve tools from library"
                );
                Vec::new()
            }
        };
        if !tool_names.is_empty() {
            if let Err(e) = sync_tools_to_dir(&dir, &tool_names, &context, lib).await {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync tools to mission directory"
                );
            }
        }

        // Sync library agents (used by mission agent selection)
        let agent_names = match lib.list_library_agents().await {
            Ok(agents) => agents.into_iter().map(|agent| agent.name).collect(),
            Err(e) => {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to list library agents"
                );
                Vec::new()
            }
        };
        if !agent_names.is_empty() {
            if let Err(e) = sync_agents_to_dir(&dir, &agent_names, &context, lib).await {
                tracing::warn!(
                    mission = %mission_id,
                    workspace = %workspace.name,
                    error = %e,
                    "Failed to sync agents to mission directory"
                );
            }
        }
    }

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
    let workspace_env = HashMap::new();
    write_opencode_config(
        &dir,
        mcp_configs,
        &config.working_dir,
        WorkspaceType::Host,
        &workspace_env,
        None,
        None, // shared_network: not relevant for host workspaces
    )
    .await?;
    Ok(dir)
}

/// Translate a host path to a container-relative path by stripping the workspace root.
fn translate_to_container_path(host_path: &Path, workspace_root: &Path) -> PathBuf {
    if let Ok(relative) = host_path.strip_prefix(workspace_root) {
        // Return as absolute path from container root
        PathBuf::from("/").join(relative)
    } else {
        // Fallback: return original path if it doesn't start with workspace root
        host_path.to_path_buf()
    }
}

/// Write the current workspace context to a runtime file for MCP tools.
///
/// For chroot workspaces, paths are translated to container-relative paths so that
/// commands executed inside the container can use them directly.
pub async fn write_runtime_workspace_state(
    working_dir_root: &Path,
    workspace: &Workspace,
    working_dir: &Path,
    mission_id: Option<Uuid>,
    context_dir_name: &str,
) -> anyhow::Result<()> {
    let runtime_dir = working_dir_root.join(".openagent").join("runtime");
    tokio::fs::create_dir_all(&runtime_dir).await?;
    let context_root = working_dir_root.join(context_dir_name);
    let mission_context = mission_id.map(|id| context_root.join(id.to_string()));
    // Create the mission context directory on the host so it exists when bind-mounted
    if let Some(target) = mission_context.as_ref() {
        tokio::fs::create_dir_all(target).await?;
    }
    let context_link = working_dir.join(context_dir_name);
    if let Some(target) = mission_context.as_ref() {
        if !context_link.exists() {
            #[cfg(unix)]
            {
                // For chroot workspaces, the symlink must point to the container path
                // since /root/context is bind-mounted, not the host path
                let symlink_target = if workspace.workspace_type == WorkspaceType::Chroot {
                    PathBuf::from("/root")
                        .join(context_dir_name)
                        .join(mission_id.unwrap().to_string())
                } else {
                    target.clone()
                };
                if let Err(e) = std::os::unix::fs::symlink(&symlink_target, &context_link) {
                    tracing::warn!(
                        workspace = %workspace.name,
                        mission = ?mission_id,
                        error = %e,
                        "Failed to create context symlink; falling back to directory"
                    );
                    let _ = tokio::fs::create_dir_all(&context_link).await;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::fs::create_dir_all(&context_link).await;
            }
        }
    }

    // For chroot workspaces, translate paths to container-relative paths.
    // Inside the container:
    // - working_dir becomes relative to container root (e.g., /workspaces/mission-xxx)
    // - context is bind-mounted at /root/context
    let (effective_working_dir, effective_context_root, effective_mission_context): (
        PathBuf,
        PathBuf,
        Option<PathBuf>,
    ) = if workspace.workspace_type == WorkspaceType::Chroot {
        let container_working_dir = translate_to_container_path(working_dir, &workspace.path);
        // Context is bind-mounted at /root/context inside the container
        let container_context_root = PathBuf::from("/root").join(context_dir_name);
        let container_mission_context =
            mission_id.map(|id| container_context_root.join(id.to_string()));
        (
            container_working_dir,
            container_context_root,
            container_mission_context,
        )
    } else {
        (
            working_dir.to_path_buf(),
            context_root.clone(),
            mission_context.clone(),
        )
    };

    let payload = json!({
        "workspace_id": workspace.id,
        "workspace_name": workspace.name,
        "workspace_type": workspace.workspace_type.as_str(),
        "workspace_root": workspace.path,
        "working_dir": effective_working_dir,
        "mission_id": mission_id,
        "context_root": effective_context_root,
        "mission_context": effective_mission_context,
        "context_dir_name": context_dir_name,
    });

    // Use per-mission workspace file to avoid race conditions with parallel missions
    let filename = match mission_id {
        Some(id) => format!("workspace-{}.json", id),
        None => "current_workspace.json".to_string(),
    };
    let path = runtime_dir.join(&filename);
    let payload_str = serde_json::to_string_pretty(&payload)?;
    tokio::fs::write(&path, &payload_str).await?;

    // Also write to current_workspace.json so OPEN_AGENT_RUNTIME_WORKSPACE_FILE always works
    // (that env var points to current_workspace.json, and oh-my-opencode reads it for container detection)
    if mission_id.is_some() {
        let current_path = runtime_dir.join("current_workspace.json");
        if let Err(e) = tokio::fs::write(&current_path, &payload_str).await {
            tracing::warn!(
                workspace = %workspace.name,
                path = %current_path.display(),
                error = %e,
                "Failed to write current_workspace.json"
            );
        }
    }

    // Also write to the working directory itself so MCPs can find it
    // This allows MCPs to discover workspace context from cwd without racing on a shared file
    let context_file = working_dir.join(".openagent_context.json");
    if let Err(e) = tokio::fs::write(&context_file, &payload_str).await {
        tracing::warn!(
            workspace = %workspace.name,
            path = %context_file.display(),
            error = %e,
            "Failed to write workspace context to working directory"
        );
    }

    Ok(())
}

/// Get the path to the runtime workspace file for a mission.
///
/// Per-mission files are used to avoid race conditions when running parallel missions.
pub fn runtime_workspace_file_path(working_dir_root: &Path, mission_id: Option<Uuid>) -> PathBuf {
    let runtime_dir = working_dir_root.join(".openagent").join("runtime");
    let filename = match mission_id {
        Some(id) => format!("workspace-{}.json", id),
        None => "current_workspace.json".to_string(),
    };
    runtime_dir.join(filename)
}

/// Regenerate `opencode.json` for all workspace directories.
pub async fn sync_all_workspaces(config: &Config, mcp: &McpRegistry) -> anyhow::Result<usize> {
    let root = workspaces_root(&config.working_dir);
    if !root.exists() {
        return Ok(0);
    }

    let mut count = 0;
    let mcp_configs = mcp.list_configs().await;
    let workspace_env = HashMap::new();

    let mut entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if write_opencode_config(
            &path,
            mcp_configs.clone(),
            &config.working_dir,
            WorkspaceType::Host,
            &workspace_env,
            None,
            None, // shared_network: not relevant for host workspaces
        )
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

/// Resolve the workspace for a mission, including skills and plugins.
/// Falls back to a default host workspace if not found.
pub async fn resolve_workspace(
    workspaces: &SharedWorkspaceStore,
    config: &Config,
    workspace_id: Option<Uuid>,
) -> Workspace {
    let id = workspace_id.unwrap_or(DEFAULT_WORKSPACE_ID);
    match workspaces.get(id).await {
        Some(ws) => ws,
        None => {
            warn!("Workspace {} not found; using default host workspace", id);
            Workspace::default_host(config.working_dir.clone())
        }
    }
}

fn find_host_binary(name: &str, working_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        working_dir.join("target").join("release").join(name),
        working_dir.join("target").join("debug").join(name),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

async fn copy_binary_into_container(
    working_dir: &Path,
    container_root: &Path,
    binary: &str,
) -> anyhow::Result<()> {
    let source = find_host_binary(binary, working_dir)
        .ok_or_else(|| anyhow::anyhow!(format!("{} binary not found in target or PATH", binary)))?;

    let dest_dir = container_root.join("usr/local/bin");
    tokio::fs::create_dir_all(&dest_dir).await?;
    let dest = dest_dir.join(binary);
    tokio::fs::copy(&source, &dest).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&dest, perms).await?;
    }

    Ok(())
}

async fn sync_workspace_mcp_binaries(
    _working_dir: &Path,
    _container_root: &Path,
) -> anyhow::Result<()> {
    // Legacy behavior: copy host workspace-mcp/desktop-mcp binaries into container.
    // New behavior: no-op. With per-workspace agent execution, shell/tools execute
    // inside the workspace and we no longer depend on these proxy MCP binaries.
    Ok(())
}

/// Build a container workspace.
pub async fn build_chroot_workspace(
    workspace: &mut Workspace,
    distro: Option<NspawnDistro>,
    force_rebuild: bool,
    working_dir: &Path,
) -> anyhow::Result<()> {
    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err(anyhow::anyhow!("Workspace is not a container type"));
    }

    // Update status to building
    workspace.status = WorkspaceStatus::Building;

    // If a previous build failed, always rebuild to clear partial state.
    let force_rebuild = force_rebuild || workspace.error_message.is_some();

    let distro = distro.unwrap_or_default();

    // Check if already built with the right distro
    if nspawn::is_container_ready(&workspace.path) {
        if !force_rebuild {
            if let Some(existing) = nspawn::detect_container_distro(&workspace.path).await {
                if existing == distro {
                    tracing::info!(
                        "Container already exists at {} with distro {}",
                        workspace.path.display(),
                        distro.as_str()
                    );
                    if let Err(e) = sync_workspace_mcp_binaries(working_dir, &workspace.path).await
                    {
                        workspace.status = WorkspaceStatus::Error;
                        workspace.error_message =
                            Some(format!("Failed to sync MCP binaries: {}", e));
                        return Err(e);
                    }
                    workspace.status = WorkspaceStatus::Ready;
                    workspace.error_message = None;
                    return Ok(());
                }
                tracing::info!(
                    "Container exists at {} with distro {}, rebuilding to {}",
                    workspace.path.display(),
                    existing.as_str(),
                    distro.as_str()
                );
            } else {
                tracing::info!(
                    "Container exists at {} with unknown distro, rebuilding to {}",
                    workspace.path.display(),
                    distro.as_str()
                );
            }
        } else {
            tracing::info!(
                "Forcing rebuild of container at {} to distro {}",
                workspace.path.display(),
                distro.as_str()
            );
        }
        nspawn::destroy_container(&workspace.path).await?;
    }

    tracing::info!(
        "Building container workspace at {} with distro {}",
        workspace.path.display(),
        distro.as_str()
    );

    // Create the container
    match nspawn::create_container(&workspace.path, distro).await {
        Ok(()) => {
            match seed_shard_data(&workspace.path).await {
                Ok(true) => {
                    tracing::info!(workspace = %workspace.name, "Seeded Shard data into container workspace")
                }
                Ok(false) => {
                    tracing::debug!(workspace = %workspace.name, "No Shard seed directory found to copy")
                }
                Err(e) => {
                    tracing::warn!(workspace = %workspace.name, error = %e, "Failed to seed Shard data into container")
                }
            }

            if let Err(e) = sync_workspace_mcp_binaries(working_dir, &workspace.path).await {
                workspace.status = WorkspaceStatus::Error;
                workspace.error_message = Some(format!("Failed to sync MCP binaries: {}", e));
                tracing::error!(workspace = %workspace.name, error = %e, "Failed to sync MCP binaries into container workspace");
                return Err(e);
            }

            if let Err(e) = run_workspace_init_script(workspace).await {
                workspace.status = WorkspaceStatus::Error;
                workspace.error_message = Some(format!("Init script failed: {}", e));
                tracing::error!("Init script failed: {}", e);
                return Err(e);
            }
            workspace.status = WorkspaceStatus::Ready;
            workspace.error_message = None;
            tracing::info!("Container workspace built successfully");
            Ok(())
        }
        Err(e) => {
            workspace.status = WorkspaceStatus::Error;
            workspace.error_message = Some(format!("Container build failed: {}", e));
            tracing::error!("Failed to build container: {}", e);
            Err(anyhow::anyhow!("Container build failed: {}", e))
        }
    }
}

async fn seed_shard_data(container_root: &Path) -> anyhow::Result<bool> {
    let seed_dir = std::env::var("OPEN_AGENT_SHARD_SEED")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".shard"))
        })
        .or_else(|| {
            let fallback = PathBuf::from("/root/.shard");
            if fallback.exists() {
                Some(fallback)
            } else {
                None
            }
        });

    let Some(seed_dir) = seed_dir else {
        return Ok(false);
    };

    if !seed_dir.exists() || !seed_dir.is_dir() {
        return Ok(false);
    }

    let dest_dir = container_root.join("root/.shard");
    let _ = tokio::fs::remove_dir_all(&dest_dir).await;
    copy_dir_recursive(&seed_dir, &dest_dir).await?;

    Ok(true)
}

#[async_recursion]
async fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dst).await?;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let entry_path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dst.join(&file_name);

        let metadata = tokio::fs::metadata(&entry_path).await?;
        if metadata.is_dir() {
            copy_dir_recursive(&entry_path, &dest_path).await?;
        } else {
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&entry_path, &dest_path).await?;
        }
    }

    Ok(())
}

async fn run_workspace_init_script(workspace: &Workspace) -> anyhow::Result<()> {
    let script = workspace
        .init_script
        .as_ref()
        .map(|s| s.trim())
        .unwrap_or("");

    if script.is_empty() {
        return Ok(());
    }

    let script_path = workspace.path.join("openagent-init.sh");
    tokio::fs::write(&script_path, script).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&script_path, perms).await?;
    }

    let shell = if workspace.path.join("bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };

    let mut config = nspawn::NspawnConfig::default();
    config.env = workspace.env_vars.clone();

    let command = vec![shell.to_string(), "/openagent-init.sh".to_string()];
    let output = nspawn::execute_in_container(&workspace.path, &command, &config).await?;

    // Clean up the script file after execution.
    let _ = tokio::fs::remove_file(&script_path).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut message = String::new();
        if !stderr.trim().is_empty() {
            message.push_str(stderr.trim());
        }
        if !stdout.trim().is_empty() {
            if !message.is_empty() {
                message.push_str(" | ");
            }
            message.push_str(stdout.trim());
        }
        if message.is_empty() {
            message = "Init script failed with no output".to_string();
        }
        return Err(anyhow::anyhow!(message));
    }

    Ok(())
}

/// Destroy a container workspace.
pub async fn destroy_chroot_workspace(workspace: &Workspace) -> anyhow::Result<()> {
    if workspace.workspace_type != WorkspaceType::Chroot {
        return Err(anyhow::anyhow!("Workspace is not a container type"));
    }

    tracing::info!(
        "Destroying container workspace at {}",
        workspace.path.display()
    );

    nspawn::destroy_container(&workspace.path).await?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Config Sync (Library → System)
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the path to the OpenCode config directory.
/// Uses OPENCODE_CONFIG_DIR env var or falls back to ~/.config/opencode/
fn resolve_opencode_config_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        return std::path::PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home)
        .join(".config")
        .join("opencode")
}

/// Sync oh-my-opencode.json from Library to ~/.config/opencode/
/// This makes Library-backed settings take effect for OpenCode.
pub async fn sync_opencode_settings(library: &crate::library::LibraryStore) -> anyhow::Result<()> {
    let settings = library.get_opencode_settings().await?;

    // Don't sync empty settings
    if settings.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        tracing::debug!("No opencode settings in Library to sync");
        return Ok(());
    }

    let dest_dir = resolve_opencode_config_dir();
    let dest_path = dest_dir.join("oh-my-opencode.json");

    // Ensure directory exists
    tokio::fs::create_dir_all(&dest_dir).await?;

    let content = serde_json::to_string_pretty(&settings)?;
    tokio::fs::write(&dest_path, content).await?;

    tracing::info!(
        path = %dest_path.display(),
        "Synced oh-my-opencode settings from Library"
    );

    Ok(())
}

/// Sync openagent/config.json from Library to the working directory.
/// This makes Library-backed agent visibility settings available.
pub async fn sync_openagent_config(
    library: &crate::library::LibraryStore,
    working_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let config = library.get_openagent_config().await?;

    let dest_dir = working_dir.join(".openagent");
    let dest_path = dest_dir.join("config.json");

    // Ensure directory exists
    tokio::fs::create_dir_all(&dest_dir).await?;

    let content = serde_json::to_string_pretty(&config)?;
    tokio::fs::write(&dest_path, content).await?;

    tracing::info!(
        path = %dest_path.display(),
        "Synced openagent config from Library"
    );

    Ok(())
}

/// Read the OpenAgent config from the working directory.
/// Returns default config if the file doesn't exist.
pub async fn read_openagent_config(
    working_dir: &std::path::Path,
) -> crate::library::OpenAgentConfig {
    let path = working_dir.join(".openagent/config.json");

    if !path.exists() {
        return crate::library::OpenAgentConfig::default();
    }

    match tokio::fs::read_to_string(&path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => crate::library::OpenAgentConfig::default(),
    }
}
