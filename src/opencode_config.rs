//! OpenCode connection configuration and storage.
//!
//! Manages multiple OpenCode server connections (e.g., Claude Code, other backends).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

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
        connections
            .values()
            .find(|c| c.enabled)
            .cloned()
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

    pub async fn update(&self, id: Uuid, mut connection: OpenCodeConnection) -> Option<OpenCodeConnection> {
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

/// Shared store type.
pub type SharedOpenCodeStore = Arc<OpenCodeStore>;
