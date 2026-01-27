//! OpenCode connection configuration and storage.
//!
//! Manages multiple OpenCode server connections (e.g., Claude Code, other backends).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::library::Plugin;
use crate::mcp::{McpRegistry, McpScope, McpTransport};

/// OpenCode connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeConnection {
    pub id: Uuid,
    /// Human-readable name (e.g., "Claude Code", "Local OpenCode")
    pub name: String,
    /// Base URL for the OpenCode server
    pub base_url: String,
    /// Default agent name (e.g., "build", "plan")
    #[serde(default)]
    pub agent: Option<String>,
    /// Whether to auto-allow all permissions
    #[serde(default = "default_permissive")]
    pub permissive: bool,
    /// Whether this connection is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Whether this is the default connection
    #[serde(default)]
    pub is_default: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn default_permissive() -> bool {
    true
}

fn default_enabled() -> bool {
    true
}

impl OpenCodeConnection {
    pub fn new(name: String, base_url: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            base_url,
            agent: None,
            permissive: true,
            enabled: true,
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }
}

/// In-memory store for OpenCode connections.
#[derive(Debug, Clone)]
pub struct OpenCodeStore {
    connections: Arc<RwLock<HashMap<Uuid, OpenCodeConnection>>>,
    storage_path: PathBuf,
}

impl OpenCodeStore {
    pub async fn new(storage_path: PathBuf) -> Self {
        let store = Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            storage_path,
        };

        // Load existing connections
        if let Ok(loaded) = store.load_from_disk() {
            let mut connections = store.connections.write().await;
            *connections = loaded;
        }

        store
    }

    /// Load connections from disk.
    fn load_from_disk(&self) -> Result<HashMap<Uuid, OpenCodeConnection>, std::io::Error> {
        if !self.storage_path.exists() {
            return Ok(HashMap::new());
        }

        let contents = std::fs::read_to_string(&self.storage_path)?;
        let connections: Vec<OpenCodeConnection> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(connections.into_iter().map(|c| (c.id, c)).collect())
    }

    /// Save connections to disk.
    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let connections = self.connections.read().await;
        let connections_vec: Vec<&OpenCodeConnection> = connections.values().collect();

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&connections_vec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }

    pub async fn list(&self) -> Vec<OpenCodeConnection> {
        let connections = self.connections.read().await;
        let mut list: Vec<_> = connections.values().cloned().collect();
        // Sort by name
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub async fn get(&self, id: Uuid) -> Option<OpenCodeConnection> {
        let connections = self.connections.read().await;
        connections.get(&id).cloned()
    }

    /// Get the default connection (first enabled, or first overall).
    pub async fn get_default(&self) -> Option<OpenCodeConnection> {
        let connections = self.connections.read().await;
        // Find the one marked as default
        if let Some(conn) = connections.values().find(|c| c.is_default && c.enabled) {
            return Some(conn.clone());
        }
        // Fallback to first enabled
        connections.values().find(|c| c.enabled).cloned()
    }

    pub async fn add(&self, connection: OpenCodeConnection) -> Uuid {
        let id = connection.id;
        {
            let mut connections = self.connections.write().await;

            // If this is the first connection, make it default
            let is_first = connections.is_empty();
            let mut conn = connection;
            if is_first {
                conn.is_default = true;
            }

            connections.insert(id, conn);
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save OpenCode connections to disk: {}", e);
        }

        id
    }

    pub async fn update(
        &self,
        id: Uuid,
        mut connection: OpenCodeConnection,
    ) -> Option<OpenCodeConnection> {
        connection.updated_at = chrono::Utc::now();

        {
            let mut connections = self.connections.write().await;
            if connections.contains_key(&id) {
                // If setting as default, unset others
                if connection.is_default {
                    for c in connections.values_mut() {
                        if c.id != id {
                            c.is_default = false;
                        }
                    }
                }
                connections.insert(id, connection.clone());
            } else {
                return None;
            }
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save OpenCode connections to disk: {}", e);
        }

        Some(connection)
    }

    pub async fn delete(&self, id: Uuid) -> bool {
        let existed = {
            let mut connections = self.connections.write().await;
            connections.remove(&id).is_some()
        };

        if existed {
            if let Err(e) = self.save_to_disk().await {
                tracing::error!("Failed to save OpenCode connections to disk: {}", e);
            }
        }

        existed
    }

    /// Set a connection as the default.
    pub async fn set_default(&self, id: Uuid) -> bool {
        let mut connections = self.connections.write().await;

        if !connections.contains_key(&id) {
            return false;
        }

        for c in connections.values_mut() {
            c.is_default = c.id == id;
        }

        drop(connections);

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save OpenCode connections to disk: {}", e);
        }

        true
    }
}

fn sanitize_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_lowercase()
        .replace('-', "_")
}

fn resolve_command_path(cmd: &str) -> String {
    let cmd_path = Path::new(cmd);
    if cmd_path.is_absolute() || cmd.contains('/') {
        return cmd.to_string();
    }

    // User-local paths are checked first so that non-root installs (bun, npm,
    // the official OpenCode installer) take precedence over system-wide copies.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let candidates = [
        PathBuf::from(&home).join(".opencode/bin").join(cmd),
        PathBuf::from(&home).join(".local/bin").join(cmd),
        PathBuf::from(&home).join(".bun/bin").join(cmd),
        Path::new("/usr/local/bin").join(cmd),
        Path::new("/usr/bin").join(cmd),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    cmd.to_string()
}

fn opencode_entry_from_mcp(config: &crate::mcp::McpServerConfig) -> Value {
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
        McpTransport::Stdio { command, args, .. } => {
            let mut cmd = vec![resolve_command_path(command)];
            cmd.extend(args.clone());
            json!({
                "type": "local",
                "command": cmd,
                "enabled": config.enabled,
            })
        }
    }
}

fn resolve_opencode_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("OPENCODE_CONFIG") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("opencode.json");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    // OpenCode stores its config under ~/.config/opencode by default.
    PathBuf::from(home)
        .join(".config")
        .join("opencode")
        .join("opencode.json")
}

pub async fn ensure_global_config(mcp: &McpRegistry) -> anyhow::Result<()> {
    let config_path = resolve_opencode_config_path();
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut root: Value = if config_path.exists() {
        let contents = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_default();
        serde_json::from_str(&contents).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    if !root.is_object() {
        root = json!({});
    }

    let mcp_configs = mcp.list_configs().await;
    let mut mcp_entries = serde_json::Map::new();
    for config in mcp_configs.iter().filter(|c| c.enabled) {
        if config.scope != McpScope::Global {
            continue;
        }
        if config.name == "desktop" || config.name == "playwright" {
            continue;
        }
        let key = sanitize_key(&config.name);
        mcp_entries.insert(key, opencode_entry_from_mcp(config));
    }

    let root_obj = root.as_object_mut().expect("config object");
    let mcp_obj = root_obj
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("mcp object");
    for (key, value) in mcp_entries {
        mcp_obj.insert(key, value);
    }

    let tools_obj = root_obj
        .entry("tools")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("tools object");
    tools_obj.insert("bash".to_string(), json!(true));
    tools_obj.insert("desktop_*".to_string(), json!(true));
    tools_obj.insert("playwright_*".to_string(), json!(true));
    tools_obj.insert("browser_*".to_string(), json!(true));
    tools_obj.insert("workspace_*".to_string(), json!(false));

    let payload = serde_json::to_string_pretty(&root)?;
    tokio::fs::write(&config_path, payload).await?;
    tracing::info!(path = %config_path.display(), "Ensured OpenCode global config");

    Ok(())
}

fn split_package_spec(spec: &str) -> (String, Option<String>) {
    if let Some(at_pos) = spec.rfind('@') {
        if at_pos > 0 {
            let base = spec[..at_pos].to_string();
            let version = spec[at_pos + 1..].trim().to_string();
            if !version.is_empty() {
                return (base, Some(version));
            }
        }
    }
    (spec.to_string(), None)
}

fn package_base(spec: &str) -> String {
    split_package_spec(spec).0
}

pub async fn sync_global_plugins(plugins: &HashMap<String, Plugin>) -> anyhow::Result<()> {
    let config_path = resolve_opencode_config_path();
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut root: Value = if config_path.exists() {
        let contents = tokio::fs::read_to_string(&config_path)
            .await
            .unwrap_or_default();
        serde_json::from_str(&contents).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    if !root.is_object() {
        root = json!({});
    }

    let root_obj = root.as_object_mut().expect("config object");
    let existing_plugins: Vec<String> = root_obj
        .get("plugin")
        .or_else(|| root_obj.get("plugins"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut merged = existing_plugins;
    let mut seen = HashSet::new();
    merged.retain(|entry| seen.insert(entry.clone()));

    for plugin in plugins.values().filter(|plugin| plugin.enabled) {
        let spec = plugin.package.trim();
        if spec.is_empty() {
            continue;
        }
        let (base, version) = split_package_spec(spec);
        if version.is_some() {
            merged.retain(|entry| package_base(entry) != base);
            if !merged.iter().any(|entry| entry == spec) {
                merged.push(spec.to_string());
            }
        } else if !merged.iter().any(|entry| package_base(entry) == base) {
            merged.push(spec.to_string());
        }
    }

    root_obj.insert("plugin".to_string(), json!(merged));

    let payload = serde_json::to_string_pretty(&root)?;
    tokio::fs::write(&config_path, payload).await?;
    tracing::info!(path = %config_path.display(), "Synced OpenCode global plugins");

    Ok(())
}

/// Get the list of installed plugins from OpenCode's global config.
/// Returns a list of plugin specs (e.g., "opencode-gemini-auth@latest", "oh-my-opencode").
pub async fn get_installed_plugins() -> Vec<String> {
    let config_path = resolve_opencode_config_path();
    if !config_path.exists() {
        return Vec::new();
    }

    let contents = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let root: Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    root.get("plugin")
        .or_else(|| root.get("plugins"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Shared store type.
pub type SharedOpenCodeStore = Arc<OpenCodeStore>;
