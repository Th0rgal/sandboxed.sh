//! Agent configuration types and storage.
//!
//! An Agent is a named configuration combining:
//! - A model from a provider
//! - Subset of MCP servers from library
//! - Skills from library
//! - Commands from library

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Agent configuration combining model, MCPs, skills, and commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: Uuid,
    pub name: String,
    /// Model ID (e.g., "claude-opus-4-5-20251101")
    pub model_id: String,
    /// MCP server names from library to enable
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// Skill names from library to include
    #[serde(default)]
    pub skills: Vec<String>,
    /// Command names from library to include
    #[serde(default)]
    pub commands: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl AgentConfig {
    pub fn new(name: String, model_id: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            model_id,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// In-memory store for agent configurations.
#[derive(Debug, Clone)]
pub struct AgentStore {
    agents: Arc<RwLock<HashMap<Uuid, AgentConfig>>>,
    storage_path: PathBuf,
}

impl AgentStore {
    pub async fn new(storage_path: PathBuf) -> Self {
        let store = Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            storage_path,
        };

        // Load existing agents
        if let Ok(loaded) = store.load_from_disk() {
            let mut agents = store.agents.write().await;
            *agents = loaded;
        }

        store
    }

    /// Load agents from disk.
    fn load_from_disk(&self) -> Result<HashMap<Uuid, AgentConfig>, std::io::Error> {
        if !self.storage_path.exists() {
            return Ok(HashMap::new());
        }

        let contents = std::fs::read_to_string(&self.storage_path)?;
        let agents: Vec<AgentConfig> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(agents.into_iter().map(|a| (a.id, a)).collect())
    }

    /// Save agents to disk.
    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let agents = self.agents.read().await;
        let agents_vec: Vec<&AgentConfig> = agents.values().collect();

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&agents_vec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }

    pub async fn list(&self) -> Vec<AgentConfig> {
        let agents = self.agents.read().await;
        agents.values().cloned().collect()
    }

    pub async fn get(&self, id: Uuid) -> Option<AgentConfig> {
        let agents = self.agents.read().await;
        agents.get(&id).cloned()
    }

    pub async fn add(&self, agent: AgentConfig) -> Uuid {
        let id = agent.id;
        {
            let mut agents = self.agents.write().await;
            agents.insert(id, agent);
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save agents to disk: {}", e);
        }

        id
    }

    pub async fn update(&self, id: Uuid, mut agent: AgentConfig) -> Option<AgentConfig> {
        agent.updated_at = chrono::Utc::now();

        {
            let mut agents = self.agents.write().await;
            if agents.contains_key(&id) {
                agents.insert(id, agent.clone());
            } else {
                return None;
            }
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save agents to disk: {}", e);
        }

        Some(agent)
    }

    pub async fn delete(&self, id: Uuid) -> bool {
        let existed = {
            let mut agents = self.agents.write().await;
            agents.remove(&id).is_some()
        };

        if existed {
            if let Err(e) = self.save_to_disk().await {
                tracing::error!("Failed to save agents to disk: {}", e);
            }
        }

        existed
    }
}
