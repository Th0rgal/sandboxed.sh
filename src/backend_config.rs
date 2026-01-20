//! Backend configuration storage and persistence.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfigEntry {
    pub id: String,
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub settings: serde_json::Value,
}

fn default_enabled() -> bool {
    true
}

impl BackendConfigEntry {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        settings: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            enabled: true,
            settings,
        }
    }
}

#[derive(Debug)]
pub struct BackendConfigStore {
    configs: Arc<RwLock<HashMap<String, BackendConfigEntry>>>,
    storage_path: PathBuf,
}

impl BackendConfigStore {
    pub async fn new(storage_path: PathBuf, defaults: Vec<BackendConfigEntry>) -> Self {
        let mut configs = HashMap::new();
        let mut needs_save = false;

        if storage_path.exists() {
            if let Ok(loaded) = Self::load_from_disk(&storage_path) {
                configs = loaded;
            }
        } else {
            needs_save = true;
        }

        for default in defaults {
            match configs.get_mut(&default.id) {
                Some(existing) => {
                    if existing.name.is_empty() {
                        existing.name = default.name.clone();
                        needs_save = true;
                    }
                    if existing.settings.is_null() {
                        existing.settings = default.settings.clone();
                        needs_save = true;
                    }
                }
                None => {
                    configs.insert(default.id.clone(), default);
                    needs_save = true;
                }
            }
        }

        let store = Self {
            configs: Arc::new(RwLock::new(configs)),
            storage_path,
        };

        if needs_save {
            if let Err(e) = store.save_to_disk().await {
                tracing::warn!("Failed to persist backend config defaults: {}", e);
            }
        }

        store
    }

    fn load_from_disk(path: &Path) -> Result<HashMap<String, BackendConfigEntry>, std::io::Error> {
        let contents = std::fs::read_to_string(path)?;
        let entries: Vec<BackendConfigEntry> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(entries
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect())
    }

    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let configs = self.configs.read().await;
        let mut entries: Vec<BackendConfigEntry> = configs.values().cloned().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }

    pub async fn list(&self) -> Vec<BackendConfigEntry> {
        let configs = self.configs.read().await;
        let mut list: Vec<_> = configs.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub async fn get(&self, id: &str) -> Option<BackendConfigEntry> {
        let configs = self.configs.read().await;
        configs.get(id).cloned()
    }

    pub async fn update_settings(
        &self,
        id: &str,
        settings: serde_json::Value,
        enabled: Option<bool>,
    ) -> Result<Option<BackendConfigEntry>, std::io::Error> {
        let mut configs = self.configs.write().await;
        let entry = configs.get_mut(id);
        let Some(entry) = entry else {
            return Ok(None);
        };

        entry.settings = settings;
        if let Some(enabled) = enabled {
            entry.enabled = enabled;
        }

        let updated = entry.clone();
        drop(configs);
        self.save_to_disk().await?;
        Ok(Some(updated))
    }
}

pub type SharedBackendConfigStore = Arc<BackendConfigStore>;
