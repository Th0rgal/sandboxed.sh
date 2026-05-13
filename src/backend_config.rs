//! Backend configuration storage and persistence.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

const BACKEND_CONFIG_MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_BACKEND_CONFIG_ENTRIES: usize = 128;

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

fn sanitize_backend_config_entries(entries: &mut Vec<BackendConfigEntry>) {
    entries.retain(|entry| !entry.id.trim().is_empty() && entry.id.len() <= 128);
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    entries.dedup_by(|a, b| a.id == b.id);
    if entries.len() > MAX_BACKEND_CONFIG_ENTRIES {
        entries.truncate(MAX_BACKEND_CONFIG_ENTRIES);
    }
}

fn write_private_json_file(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let write_result = (|| {
        std::fs::write(&tmp_path, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result
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
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > BACKEND_CONFIG_MAX_FILE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "backend config file is too large ({} bytes, max {})",
                    metadata.len(),
                    BACKEND_CONFIG_MAX_FILE_BYTES
                ),
            ));
        }

        let contents = std::fs::read_to_string(path)?;
        let mut entries: Vec<BackendConfigEntry> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        sanitize_backend_config_entries(&mut entries);
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
        write_private_json_file(&self.storage_path, &contents)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_config_store_rejects_oversized_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("backend_config.json");
        std::fs::write(
            &path,
            vec![b'x'; BACKEND_CONFIG_MAX_FILE_BYTES as usize + 1],
        )
        .expect("write oversized config");

        let err =
            BackendConfigStore::load_from_disk(&path).expect_err("oversized file should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn backend_config_store_prunes_loaded_entries_to_capacity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("backend_config.json");
        let entries: Vec<_> = (0..(MAX_BACKEND_CONFIG_ENTRIES + 8))
            .map(|i| BackendConfigEntry {
                id: format!("backend-{i:03}"),
                name: format!("Backend {i:03}"),
                enabled: true,
                settings: serde_json::json!({"api_key": "secret"}),
            })
            .collect();
        std::fs::write(&path, serde_json::to_string(&entries).unwrap()).expect("write config");

        let loaded = BackendConfigStore::load_from_disk(&path).expect("load config");
        assert_eq!(loaded.len(), MAX_BACKEND_CONFIG_ENTRIES);
    }

    #[test]
    #[cfg(unix)]
    fn write_private_json_file_restricts_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("backend_config.json");
        write_private_json_file(&path, "[]").expect("write config");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
