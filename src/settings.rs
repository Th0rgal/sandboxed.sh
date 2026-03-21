//! Global settings storage.
//!
//! Persists user-configurable settings to disk at `{working_dir}/.sandboxed-sh/settings.json`.
//! Environment variables are used as initial defaults when no settings file exists.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Global cached RTK enabled state, updated when settings change.
/// This allows synchronous checks from non-async contexts.
static RTK_ENABLED_CACHED: AtomicBool = AtomicBool::new(false);
/// Global cached max parallel missions value.
/// A value of 0 means "unset" and callers should fall back to their default.
static MAX_PARALLEL_MISSIONS_CACHED: AtomicUsize = AtomicUsize::new(0);
/// Global cached max concurrent command tasks value.
/// A value of 0 means "unset" and callers should fall back to their default.
static MAX_CONCURRENT_TASKS_CACHED: AtomicUsize = AtomicUsize::new(0);

/// Default repo path for sandboxed.sh source (used for self-updates).
pub const DEFAULT_SANDBOXED_REPO_PATH: &str = "/opt/sandboxed-sh/vaduz-v1";

/// Authentication settings managed via the dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthSettings {
    /// PBKDF2 password hash (format: `pbkdf2:iterations:hex_salt:hex_hash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    /// ISO 8601 timestamp of last password change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_changed_at: Option<String>,
}

/// Global application settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Git remote URL for the configuration library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_remote: Option<String>,
    /// Path to the sandboxed.sh source repo (used for self-updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandboxed_repo_path: Option<String>,
    /// Dashboard-managed auth settings (password hash, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthSettings>,
    /// Whether RTK (Rich Terminal Kit) wrapping is enabled for terminal commands.
    /// When None, falls back to the SANDBOXED_SH_RTK_ENABLED env var (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtk_enabled: Option<bool>,
    /// Maximum number of missions that can run in parallel.
    /// When None, falls back to the MAX_PARALLEL_MISSIONS env var (default: 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel_missions: Option<usize>,
    /// Maximum number of command-mode tasks that can run concurrently.
    /// When None, falls back to the MAX_CONCURRENT_TASKS env var (default: 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<usize>,
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
    pub async fn new(working_dir: &Path) -> Self {
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
        let rtk_enabled = std::env::var("SANDBOXED_SH_RTK_ENABLED")
            .ok()
            .and_then(|v| {
                matches!(
                    v.trim().to_lowercase().as_str(),
                    "1" | "true" | "yes" | "y" | "on"
                )
                .then_some(true)
            });
        let max_parallel_missions = std::env::var("MAX_PARALLEL_MISSIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1);
        let max_concurrent_tasks = std::env::var("MAX_CONCURRENT_TASKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1);

        Settings {
            library_remote: std::env::var("LIBRARY_REMOTE").ok().or_else(|| {
                Some("https://github.com/Th0rgal/sandboxed-library-template.git".to_string())
            }),
            sandboxed_repo_path: std::env::var("SANDBOXED_SH_REPO_PATH")
                .or_else(|_| std::env::var("SANDBOXED_REPO_PATH"))
                .ok()
                .or_else(|| Some(DEFAULT_SANDBOXED_REPO_PATH.to_string())),
            auth: None,
            rtk_enabled,
            max_parallel_missions,
            max_concurrent_tasks,
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

    /// Get the configured sandboxed.sh repo path.
    pub async fn get_sandboxed_repo_path(&self) -> Option<String> {
        self.settings.read().await.sandboxed_repo_path.clone()
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

    /// Get the auth settings.
    pub async fn get_auth_settings(&self) -> Option<AuthSettings> {
        self.settings.read().await.auth.clone()
    }

    /// Update auth settings and persist to disk.
    pub async fn set_auth_settings(&self, auth: AuthSettings) -> Result<(), std::io::Error> {
        let mut settings = self.settings.write().await;
        settings.auth = Some(auth);
        drop(settings);
        self.save_to_disk().await
    }

    /// Get the RTK enabled setting.
    /// Returns None if not explicitly set (caller should check env var as fallback).
    pub async fn get_rtk_enabled(&self) -> Option<bool> {
        self.settings.read().await.rtk_enabled
    }

    /// Update the RTK enabled setting.
    ///
    /// Returns `(changed, previous_value)`.
    pub async fn set_rtk_enabled(
        &self,
        enabled: Option<bool>,
    ) -> Result<(bool, Option<bool>), std::io::Error> {
        let mut settings = self.settings.write().await;
        let previous = settings.rtk_enabled;

        if previous != enabled {
            settings.rtk_enabled = enabled;
            // Update the cached value for synchronous access
            if let Some(e) = enabled {
                set_rtk_enabled_cached(e);
            }
            drop(settings); // Release lock before saving
            self.save_to_disk().await?;
            Ok((true, previous))
        } else {
            Ok((false, previous))
        }
    }

    /// Get the max parallel missions setting.
    /// Returns None if not explicitly set (caller should check env var as fallback).
    pub async fn get_max_parallel_missions(&self) -> Option<usize> {
        self.settings.read().await.max_parallel_missions
    }

    /// Update the max parallel missions setting.
    ///
    /// Returns `(changed, previous_value)`.
    pub async fn set_max_parallel_missions(
        &self,
        max_parallel_missions: Option<usize>,
    ) -> Result<(bool, Option<usize>), std::io::Error> {
        let mut settings = self.settings.write().await;
        let previous = settings.max_parallel_missions;

        if previous != max_parallel_missions {
            settings.max_parallel_missions = max_parallel_missions;
            if let Some(limit) = max_parallel_missions {
                set_max_parallel_missions_cached(limit);
            }
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
    /// Also refreshes all atomic caches so the new values take effect immediately.
    pub async fn reload(&self) -> Result<(), std::io::Error> {
        if self.storage_path.exists() {
            let loaded = Self::load_from_path(&self.storage_path)?;
            let mut settings = self.settings.write().await;
            *settings = loaded;
            // Refresh atomic caches from the reloaded settings.
            if let Some(enabled) = settings.rtk_enabled {
                set_rtk_enabled_cached(enabled);
            }
            if let Some(limit) = settings.max_parallel_missions {
                set_max_parallel_missions_cached(limit);
            }
            if let Some(limit) = settings.max_concurrent_tasks {
                set_max_concurrent_tasks_cached(limit);
            }
            tracing::info!("Reloaded settings from {}", self.storage_path.display());
        }
        Ok(())
    }

    /// Update the max concurrent tasks setting.
    ///
    /// Returns `(changed, previous_value)`.
    pub async fn set_max_concurrent_tasks(
        &self,
        max_concurrent_tasks: Option<usize>,
    ) -> Result<(bool, Option<usize>), std::io::Error> {
        let mut settings = self.settings.write().await;
        let previous = settings.max_concurrent_tasks;

        if previous != max_concurrent_tasks {
            settings.max_concurrent_tasks = max_concurrent_tasks;
            if let Some(limit) = max_concurrent_tasks {
                set_max_concurrent_tasks_cached(limit);
            }
            drop(settings);
            self.save_to_disk().await?;
            Ok((true, previous))
        } else {
            Ok((false, previous))
        }
    }

    /// Initialize cached values from loaded settings.
    /// Must be called after creating the settings store, before any workspace operations.
    pub fn init_cached_values(&self) {
        // Try to get the current value using block_in_place for sync access
        // Since we're in the constructor/startup context, use try_read
        if let Ok(settings) = self.settings.try_read() {
            if let Some(enabled) = settings.rtk_enabled {
                set_rtk_enabled_cached(enabled);
            }
            if let Some(limit) = settings.max_parallel_missions {
                set_max_parallel_missions_cached(limit);
            }
            if let Some(limit) = settings.max_concurrent_tasks {
                set_max_concurrent_tasks_cached(limit);
            }
        }
    }
}

/// Shared settings store wrapped in Arc for concurrent access.
pub type SharedSettingsStore = Arc<SettingsStore>;

/// Get the cached RTK enabled state.
/// This is a synchronous check that uses a cached value updated when settings change.
pub fn rtk_enabled_cached() -> bool {
    RTK_ENABLED_CACHED.load(Ordering::Relaxed)
}

/// Update the cached RTK enabled state.
/// Called during startup and when the setting is changed via the API.
pub fn set_rtk_enabled_cached(enabled: bool) {
    RTK_ENABLED_CACHED.store(enabled, Ordering::Relaxed);
}

/// Get the effective max parallel missions limit from cache, with a fallback default.
pub fn max_parallel_missions_cached_or(default: usize) -> usize {
    let cached = MAX_PARALLEL_MISSIONS_CACHED.load(Ordering::Relaxed);
    if cached >= 1 {
        cached
    } else if default >= 1 {
        default
    } else {
        1
    }
}

/// Update the cached max parallel missions value.
/// Values less than 1 are normalized to 1.
pub fn set_max_parallel_missions_cached(max_parallel_missions: usize) {
    MAX_PARALLEL_MISSIONS_CACHED.store(max_parallel_missions.max(1), Ordering::Relaxed);
}

/// Get the effective max concurrent command tasks limit from cache, with a fallback default.
pub fn max_concurrent_tasks_cached_or(default: usize) -> usize {
    let cached = MAX_CONCURRENT_TASKS_CACHED.load(Ordering::Relaxed);
    if cached >= 1 {
        cached
    } else if default >= 1 {
        default
    } else {
        5
    }
}

/// Update the cached max concurrent tasks value.
/// Values less than 1 are normalized to 1.
pub fn set_max_concurrent_tasks_cached(max_concurrent_tasks: usize) {
    MAX_CONCURRENT_TASKS_CACHED.store(max_concurrent_tasks.max(1), Ordering::Relaxed);
}
