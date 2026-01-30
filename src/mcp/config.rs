//! MCP configuration persistence.

use std::path::{Path, PathBuf};

use tokio::sync::RwLock;
use uuid::Uuid;

use super::types::McpServerConfig;

/// Persistent store for MCP configurations.
pub struct McpConfigStore {
    /// Path to the config file
    config_path: PathBuf,
    /// In-memory cache of configs
    configs: RwLock<Vec<McpServerConfig>>,
}

impl McpConfigStore {
    /// Create a new config store, loading from disk if available.
    pub async fn new(working_dir: &Path) -> Self {
        let config_dir = working_dir.join(".sandboxed-sh").join("mcp");
        let config_path = config_dir.join("config.json");

        let configs = if config_path.exists() {
            tokio::fs::read_to_string(&config_path)
                .await
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            config_path,
            configs: RwLock::new(configs),
        }
    }

    /// Save current configs to disk.
    async fn save(&self) -> anyhow::Result<()> {
        let configs = self.configs.read().await;

        // Ensure directory exists
        if let Some(parent) = self.config_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(&*configs)?;
        tokio::fs::write(&self.config_path, content).await?;
        Ok(())
    }

    /// Get all MCP configurations.
    pub async fn list(&self) -> Vec<McpServerConfig> {
        self.configs.read().await.clone()
    }

    /// Get a specific MCP configuration by ID.
    pub async fn get(&self, id: Uuid) -> Option<McpServerConfig> {
        self.configs
            .read()
            .await
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    /// Add a new MCP configuration.
    pub async fn add(&self, config: McpServerConfig) -> anyhow::Result<McpServerConfig> {
        {
            let mut configs = self.configs.write().await;

            // Check for duplicate name
            if configs.iter().any(|c| c.name == config.name) {
                anyhow::bail!("MCP with name '{}' already exists", config.name);
            }

            configs.push(config.clone());
        }

        self.save().await?;
        Ok(config)
    }

    /// Update an existing MCP configuration.
    pub async fn update(
        &self,
        id: Uuid,
        updates: impl FnOnce(&mut McpServerConfig),
    ) -> anyhow::Result<McpServerConfig> {
        let updated = {
            let mut configs = self.configs.write().await;
            let config = configs
                .iter_mut()
                .find(|c| c.id == id)
                .ok_or_else(|| anyhow::anyhow!("MCP {} not found", id))?;

            updates(config);
            config.clone()
        };

        self.save().await?;
        Ok(updated)
    }

    /// Remove an MCP configuration.
    pub async fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        {
            let mut configs = self.configs.write().await;
            let idx = configs
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| anyhow::anyhow!("MCP {} not found", id))?;
            configs.remove(idx);
        }

        self.save().await?;
        Ok(())
    }

    /// Enable an MCP.
    pub async fn enable(&self, id: Uuid) -> anyhow::Result<McpServerConfig> {
        self.update(id, |c| c.enabled = true).await
    }

    /// Disable an MCP.
    pub async fn disable(&self, id: Uuid) -> anyhow::Result<McpServerConfig> {
        self.update(id, |c| c.enabled = false).await
    }
}
