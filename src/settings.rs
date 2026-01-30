//! Global settings storage.
//!
//! Persists user-configurable settings to disk at `{working_dir}/.sandboxed-sh/settings.json`.
//! Environment variables are used as initial defaults when no settings file exists.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Global application settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Git remote URL for the configuration library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_remote: Option<String>,
}

/// In-memory store for global settings with disk persistence.
#[derive(Debug)]
pub struct SettingsStore {
    settings: RwLock<Settings>,
    storage_path: PathBuf,
}

impl SettingsStore {
    /// Create a new settings store, loading from disk if available.
    ///
    /// If no settings file exists, uses environment variables as defaults:
    /// - `LIBRARY_REMOTE` - Git remote URL for the configuration library
    pub async fn new(working_dir: &PathBuf) -> Self {
        let storage_path = working_dir.join(".sandboxed-sh/settings.json");

        let settings = if storage_path.exists() {
            match Self::load_from_path(&storage_path) {
                Ok(s) => {
                    tracing::info!("Loaded settings from {}", storage_path.display());
                    s
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load settings from {}: {}, using defaults",
                        storage_path.display(),
                        e
                    );
                    Self::defaults_from_env()
                }
            }
        } else {
            tracing::info!(
                "No settings file found at {}, using environment defaults",
                storage_path.display()
            );
            Self::defaults_from_env()
        };

        Self {
            settings: RwLock::new(settings),
            storage_path,
        }
    }

    /// Load settings from environment variables as initial defaults.
    fn defaults_from_env() -> Settings {
        Settings {
            library_remote: std::env::var("LIBRARY_REMOTE").ok().or_else(|| {
                Some("https://github.com/Th0rgal/sandboxed-library-template.git".to_string())
            }),
        }
    }

    /// Load settings from a file path.
    fn load_from_path(path: &PathBuf) -> Result<Settings, std::io::Error> {
        let contents = std::fs::read_to_string(path)?;
        serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Save current settings to disk.
    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let settings = self.settings.read().await;

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&*settings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.storage_path, contents)?;
        tracing::debug!("Saved settings to {}", self.storage_path.display());
        Ok(())
    }

    /// Get a clone of the current settings.
    pub async fn get(&self) -> Settings {
        self.settings.read().await.clone()
    }

    /// Get the library remote URL.
    pub async fn get_library_remote(&self) -> Option<String> {
        self.settings.read().await.library_remote.clone()
    }

    /// Update the library remote URL.
    ///
    /// Returns `(changed, previous_value)`.
    pub async fn set_library_remote(
        &self,
        remote: Option<String>,
    ) -> Result<(bool, Option<String>), std::io::Error> {
        let mut settings = self.settings.write().await;
        let previous = settings.library_remote.clone();

        if previous != remote {
            settings.library_remote = remote;
            drop(settings); // Release lock before saving
            self.save_to_disk().await?;
            Ok((true, previous))
        } else {
            Ok((false, previous))
        }
    }

    /// Update multiple settings at once.
    pub async fn update(&self, new_settings: Settings) -> Result<(), std::io::Error> {
        let mut settings = self.settings.write().await;
        *settings = new_settings;
        drop(settings);
        self.save_to_disk().await
    }

    /// Reload settings from disk.
    ///
    /// Used after restoring a backup to pick up the restored settings.
    pub async fn reload(&self) -> Result<(), std::io::Error> {
        if self.storage_path.exists() {
            let loaded = Self::load_from_path(&self.storage_path)?;
            let mut settings = self.settings.write().await;
            *settings = loaded;
            tracing::info!("Reloaded settings from {}", self.storage_path.display());
        }
        Ok(())
    }
}

/// Shared settings store wrapped in Arc for concurrent access.
pub type SharedSettingsStore = Arc<SettingsStore>;
