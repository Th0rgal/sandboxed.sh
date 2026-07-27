//! Global settings storage.
//!
//! Persists user-configurable settings to disk at `{working_dir}/.sandboxed-sh/settings.json`.
//! Environment variables are used as initial defaults when no settings file exists.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Global cached max parallel missions value.
/// A value of 0 means "unset" and callers should fall back to their default.
static MAX_PARALLEL_MISSIONS_CACHED: AtomicUsize = AtomicUsize::new(0);
/// Global cached max concurrent command tasks value.
/// A value of 0 means "unset" and callers should fall back to their default.
static MAX_CONCURRENT_TASKS_CACHED: AtomicUsize = AtomicUsize::new(0);

/// Default repo path for sandboxed.sh source (used for self-updates).
pub const DEFAULT_SANDBOXED_REPO_PATH: &str = "/opt/sandboxed-sh/vaduz-v1";

/// Fallback Claude Code version installed into container workspaces when no
/// pin is configured (settings `harness_versions.claude_code` or the
/// `SANDBOXED_SH_CLAUDECODE_VERSION` env var).
pub const DEFAULT_CLAUDE_CODE_VERSION: &str = "2.1.139";

/// Per-harness version pins for container workspace bootstrap.
///
/// `None` for a harness means "no pin": Claude Code falls back to
/// [`DEFAULT_CLAUDE_CODE_VERSION`], the others install latest-if-missing.
/// A pinned harness is reinstalled whenever `--version` stops matching the
/// pin, so editing these (Settings UI / `PUT /api/settings`) takes effect on
/// the next workspace build or rebuild without a redeploy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessVersionPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok: Option<String>,
}

impl HarnessVersionPolicy {
    /// Effective Claude Code version: env override, then settings pin, then
    /// the built-in default.
    pub fn effective_claude_code(&self) -> String {
        std::env::var("SANDBOXED_SH_CLAUDECODE_VERSION")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| self.claude_code.clone())
            .unwrap_or_else(|| DEFAULT_CLAUDE_CODE_VERSION.to_string())
    }
}

/// Globally cached harness version policy, refreshed whenever settings are
/// loaded, updated, or reloaded. Lets code without a `SettingsStore` handle
/// (e.g. the system component update checks) read the current pins.
static HARNESS_VERSIONS_CACHED: std::sync::OnceLock<std::sync::RwLock<HarnessVersionPolicy>> =
    std::sync::OnceLock::new();

fn harness_versions_cell() -> &'static std::sync::RwLock<HarnessVersionPolicy> {
    HARNESS_VERSIONS_CACHED.get_or_init(|| std::sync::RwLock::new(HarnessVersionPolicy::default()))
}

/// Current harness version policy from the global cache.
pub fn harness_versions_cached() -> HarnessVersionPolicy {
    harness_versions_cell()
        .read()
        .map(|p| p.clone())
        .unwrap_or_default()
}

/// Refresh the global harness version cache.
pub fn set_harness_versions_cached(policy: Option<&HarnessVersionPolicy>) {
    if let Ok(mut cell) = harness_versions_cell().write() {
        *cell = policy.cloned().unwrap_or_default();
    }
}

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
    /// Maximum number of missions that can run in parallel.
    /// When None, falls back to the MAX_PARALLEL_MISSIONS env var (default: 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel_missions: Option<usize>,
    /// Maximum number of command-mode tasks that can run concurrently.
    /// When None, falls back to the MAX_CONCURRENT_TASKS env var (default: 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<usize>,
    /// Whether the background GC task should delete on-disk workspace dirs of
    /// missions that have been in a terminal state longer than
    /// `auto_cleanup_days`. When None, treat as disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cleanup_enabled: Option<bool>,
    /// Retention window in days for terminal-mission workspace dirs. Anything
    /// older than this becomes eligible for GC. When None, defaults to 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cleanup_days: Option<u32>,
    /// Long-stop retention (days) for AwaitingUser/Paused mission workspace
    /// dirs. These are exempt from the normal window (the user may come
    /// back) but not forever. When None, defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cleanup_stopped_days: Option<u32>,
    /// Whether the GC also removes `mission-*` dirs that match no mission in
    /// any store (hard-deleted missions, legacy DBs). When None, follows
    /// `auto_cleanup_enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cleanup_orphans_enabled: Option<bool>,
    /// Model for the Ask assistant (sidecar co-pilot). When None, falls back to
    /// the `ASK_ASSISTANT_MODEL` env var, then the built-in default
    /// (`gpt-oss-120b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_assistant_model: Option<String>,
    /// Model for mission titles & status lines. Only routable values (a
    /// Routing chain id or a provider/model passthrough) are honored; when
    /// None or non-routable the auto provider ladder picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_model: Option<String>,
    /// Version pins for harness CLIs installed into container workspaces.
    /// When None, every harness uses its built-in default behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_versions: Option<HarnessVersionPolicy>,
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

        set_harness_versions_cached(settings.harness_versions.as_ref());

        Self {
            settings: RwLock::new(settings),
            storage_path,
        }
    }

    /// Load settings from environment variables as initial defaults.
    fn defaults_from_env() -> Settings {
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
            max_parallel_missions,
            max_concurrent_tasks,
            auto_cleanup_enabled: None,
            auto_cleanup_days: None,
            auto_cleanup_stopped_days: None,
            auto_cleanup_orphans_enabled: None,
            ask_assistant_model: std::env::var("ASK_ASSISTANT_MODEL").ok(),
            metadata_model: None,
            harness_versions: None,
        }
    }

    /// Effective harness version policy (empty policy when unset).
    pub async fn get_harness_versions(&self) -> HarnessVersionPolicy {
        self.settings
            .read()
            .await
            .harness_versions
            .clone()
            .unwrap_or_default()
    }

    /// Get the auto-cleanup enabled state.
    pub async fn get_auto_cleanup_enabled(&self) -> Option<bool> {
        self.settings.read().await.auto_cleanup_enabled
    }

    /// Get the auto-cleanup retention window in days.
    pub async fn get_auto_cleanup_days(&self) -> Option<u32> {
        self.settings.read().await.auto_cleanup_days
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
        set_harness_versions_cached(new_settings.harness_versions.as_ref());
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
            if let Some(limit) = settings.max_parallel_missions {
                set_max_parallel_missions_cached(limit);
            }
            if let Some(limit) = settings.max_concurrent_tasks {
                set_max_concurrent_tasks_cached(limit);
            }
            set_harness_versions_cached(settings.harness_versions.as_ref());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_version_policy_roundtrips_and_defaults() {
        // Old settings files without the field still deserialize.
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.harness_versions.is_none());

        let policy = HarnessVersionPolicy {
            claude_code: Some("2.2.0".to_string()),
            codex: Some("0.48.0".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: HarnessVersionPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, policy);
        // Unpinned harnesses are omitted from the serialized form.
        assert!(!json.contains("gemini"));
    }

    #[test]
    fn effective_claude_code_prefers_pin_over_default() {
        let unpinned = HarnessVersionPolicy::default();
        // Env var may leak from the host environment; only assert the
        // settings-pin path when it is unset.
        if std::env::var("SANDBOXED_SH_CLAUDECODE_VERSION").is_err() {
            assert_eq!(
                unpinned.effective_claude_code(),
                DEFAULT_CLAUDE_CODE_VERSION
            );
            let pinned = HarnessVersionPolicy {
                claude_code: Some("9.9.9".to_string()),
                ..Default::default()
            };
            assert_eq!(pinned.effective_claude_code(), "9.9.9");
        }
    }
}
