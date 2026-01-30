//! Configuration management for Open Agent.
//!
//! Open Agent uses per-mission CLI execution for both OpenCode and Claude Code.
//! Configuration can be set via environment variables:
//! - `DEFAULT_MODEL` - Optional. Override default model (provider/model format). If unset, uses backend default.
//! - `WORKING_DIR` - Optional. Default working directory for relative paths. Defaults to `/root` in production, current directory in dev.
//! - `HOST` - Optional. Server host. Defaults to `127.0.0.1`.
//! - `PORT` - Optional. Server port. Defaults to `3000`.
//! - `MAX_ITERATIONS` - Optional. Maximum agent loop iterations. Defaults to `50`.
//! - `OPENCODE_BASE_URL` - DEPRECATED. No longer used for mission execution (per-mission CLI mode).
//! - `OPENCODE_AGENT` - Optional. Default OpenCode agent name (e.g., `Sisyphus`, `oracle`).
//! - `OPENCODE_PERMISSIVE` - Optional. If true, auto-allows all permissions for OpenCode sessions (default: true).
//! - `SANDBOXED_USERS` or `SANDBOXED_SH_USERS` (legacy) - Optional. JSON array of user accounts for multi-user auth.
//! - `LIBRARY_GIT_SSH_KEY` - Optional. SSH key path for library git operations. If set to a path, uses that key.
//!   If set to empty string, ignores ~/.ssh/config (useful when the config specifies a non-existent key).
//!   If unset, uses default SSH behavior.
//! - `LIBRARY_REMOTE` - Optional. Initial library remote URL (can be changed via Settings in the dashboard).
//!   This environment variable is used as the initial default when no settings file exists.
//!   If not set, defaults to: https://github.com/Th0rgal/sandboxed-library-template.git
//!
//! Note: The agent has **full system access**. It can read/write any file, execute any command,
//! and search anywhere on the machine. The `WORKING_DIR` is just the default for relative paths.

use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Missing required environment variable: {0}")]
    MissingEnvVar(String),

    #[error("Invalid value for {0}: {1}")]
    InvalidValue(String, String),
}

/// Context injection configuration.
///
/// Controls how much context is injected into agent prompts
/// to prevent token overflow while maintaining relevance.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    // === Conversation History ===
    /// Maximum messages to include from conversation history
    pub max_history_messages: usize,
    /// Maximum characters per individual message in history
    pub max_message_chars: usize,
    /// Maximum total characters for conversation context
    pub max_history_total_chars: usize,

    // === Memory Retrieval ===
    /// Number of relevant past task chunks to retrieve
    pub memory_chunk_limit: usize,
    /// Similarity threshold for chunk retrieval (0.0-1.0)
    pub memory_chunk_threshold: f64,
    /// Maximum user facts to inject
    pub user_facts_limit: usize,
    /// Maximum mission summaries to inject
    pub mission_summaries_limit: usize,

    // === Tool Results ===
    /// Maximum characters for tool result before truncation
    pub max_tool_result_chars: usize,

    // === Context Files ===
    /// Maximum context files to list in session metadata
    pub max_context_files: usize,

    // === Directory Structure ===
    /// Context directory name (user uploads)
    pub context_dir_name: String,
    /// Work directory name (agent workspace)
    pub work_dir_name: String,
    /// Tools directory name (reusable scripts)
    pub tools_dir_name: String,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            // Conversation history
            max_history_messages: 10,
            max_message_chars: 5000,
            max_history_total_chars: 30000,

            // Memory retrieval
            memory_chunk_limit: 3,
            memory_chunk_threshold: 0.6,
            user_facts_limit: 10,
            mission_summaries_limit: 5,

            // Tool results
            max_tool_result_chars: 15000,

            // Context files
            max_context_files: 10,

            // Directory structure
            context_dir_name: "context".to_string(),
            work_dir_name: "work".to_string(),
            tools_dir_name: "tools".to_string(),
        }
    }
}

impl ContextConfig {
    /// Load from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(v) = std::env::var("CONTEXT_MAX_HISTORY_MESSAGES") {
            if let Ok(n) = v.parse() {
                config.max_history_messages = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MAX_MESSAGE_CHARS") {
            if let Ok(n) = v.parse() {
                config.max_message_chars = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MAX_HISTORY_CHARS") {
            if let Ok(n) = v.parse() {
                config.max_history_total_chars = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MEMORY_CHUNK_LIMIT") {
            if let Ok(n) = v.parse() {
                config.memory_chunk_limit = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MEMORY_THRESHOLD") {
            if let Ok(n) = v.parse() {
                config.memory_chunk_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_USER_FACTS_LIMIT") {
            if let Ok(n) = v.parse() {
                config.user_facts_limit = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MISSION_SUMMARIES_LIMIT") {
            if let Ok(n) = v.parse() {
                config.mission_summaries_limit = n;
            }
        }
        if let Ok(v) = std::env::var("CONTEXT_MAX_TOOL_RESULT_CHARS") {
            if let Ok(n) = v.parse() {
                config.max_tool_result_chars = n;
            }
        }

        config
    }

    /// Get the context directory path for a given working directory.
    pub fn context_dir(&self, working_dir: &str) -> String {
        self.resolve_subdir(working_dir, &self.context_dir_name)
    }

    /// Get the tools directory path for a given working directory.
    pub fn tools_dir(&self, working_dir: &str) -> String {
        self.resolve_subdir(working_dir, &self.tools_dir_name)
    }

    /// Get the work directory path for a given working directory.
    pub fn work_dir(&self, working_dir: &str) -> String {
        self.resolve_subdir(working_dir, &self.work_dir_name)
    }

    /// Resolve a subdirectory path relative to working directory.
    fn resolve_subdir(&self, working_dir: &str, subdir: &str) -> String {
        if working_dir.contains("/root") {
            format!("/root/{}", subdir)
        } else if working_dir.starts_with('/') {
            format!("{}/{}", working_dir, subdir)
        } else {
            format!("./{}", subdir)
        }
    }
}

/// Agent configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Optional model override (provider/model format). If None, OpenCode uses its own default.
    pub default_model: Option<String>,

    /// Default working directory for relative paths (agent has full system access regardless).
    /// In production, this is typically `/root`. The agent can still access any path on the system.
    pub working_dir: PathBuf,

    /// Server host
    pub host: String,

    /// Server port
    pub port: u16,

    /// Maximum iterations for the agent loop
    pub max_iterations: usize,

    /// Hours of inactivity after which an active mission is auto-closed (0 = disabled)
    pub stale_mission_hours: u64,

    /// Maximum number of missions that can run in parallel (1 = sequential only)
    pub max_parallel_missions: usize,

    /// Development mode (disables auth; more permissive defaults)
    pub dev_mode: bool,

    /// API auth configuration (dashboard login)
    pub auth: AuthConfig,

    /// Context injection configuration
    pub context: ContextConfig,

    /// DEPRECATED: OpenCode server base URL (no longer used for mission execution)
    pub opencode_base_url: String,

    /// Default OpenCode agent name (e.g., "Sisyphus", "oracle")
    pub opencode_agent: Option<String>,

    /// Whether to auto-allow all OpenCode permissions for created sessions
    pub opencode_permissive: bool,

    /// Path to the configuration library git repo.
    /// Default: {working_dir}/.sandboxed-sh/library
    pub library_path: PathBuf,
}

/// API auth configuration.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Password required by the dashboard to obtain a JWT.
    pub dashboard_password: Option<String>,

    /// HMAC secret for signing/verifying JWTs.
    pub jwt_secret: Option<String>,

    /// JWT validity in days.
    pub jwt_ttl_days: i64,

    /// Multi-user accounts (if set, overrides dashboard_password auth).
    pub users: Vec<UserAccount>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            dashboard_password: None,
            jwt_secret: None,
            jwt_ttl_days: 30,
            users: Vec::new(),
        }
    }
}

/// Authentication mode for the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Disabled,
    SingleTenant,
    MultiUser,
}

/// User account for multi-user auth.
#[derive(Debug, Clone, Deserialize)]
pub struct UserAccount {
    /// Stable identifier for the user (defaults to username).
    #[serde(default)]
    pub id: String,
    pub username: String,
    pub password: String,
}

impl AuthConfig {
    /// Whether auth is required for API requests.
    pub fn auth_required(&self, dev_mode: bool) -> bool {
        matches!(
            self.auth_mode(dev_mode),
            AuthMode::SingleTenant | AuthMode::MultiUser
        )
    }

    /// Determine the current auth mode.
    pub fn auth_mode(&self, dev_mode: bool) -> AuthMode {
        if dev_mode {
            return AuthMode::Disabled;
        }
        if !self.users.is_empty() {
            return AuthMode::MultiUser;
        }
        if self.dashboard_password.is_some() && self.jwt_secret.is_some() {
            return AuthMode::SingleTenant;
        }
        AuthMode::Disabled
    }
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// # Errors
    pub fn from_env() -> Result<Self, ConfigError> {
        // OpenCode configuration (always used)
        let opencode_base_url = std::env::var("OPENCODE_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4096".to_string());
        let opencode_agent = std::env::var("OPENCODE_AGENT").ok();
        let opencode_permissive = std::env::var("OPENCODE_PERMISSIVE")
            .ok()
            .map(|v| {
                parse_bool(&v)
                    .map_err(|e| ConfigError::InvalidValue("OPENCODE_PERMISSIVE".to_string(), e))
            })
            .transpose()?
            .unwrap_or(true);

        let default_model = std::env::var("DEFAULT_MODEL").ok();

        // WORKING_DIR: default working directory for relative paths.
        // In production (release build), default to /root. In dev, default to current directory.
        let working_dir = std::env::var("WORKING_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                if cfg!(debug_assertions) {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                } else {
                    PathBuf::from("/root")
                }
            });

        let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .map_err(|e| ConfigError::InvalidValue("PORT".to_string(), format!("{}", e)))?;

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .unwrap_or_else(|_| "50".to_string())
            .parse()
            .map_err(|e| {
                ConfigError::InvalidValue("MAX_ITERATIONS".to_string(), format!("{}", e))
            })?;

        // Hours of inactivity after which an active mission is auto-closed.
        // Default: 2 hours. Set to 0 to disable.
        // Note: orphaned missions (process died) are detected every 5 minutes
        // regardless of this setting. This is only a safety-net timeout.
        let stale_mission_hours = std::env::var("STALE_MISSION_HOURS")
            .unwrap_or_else(|_| "2".to_string())
            .parse()
            .map_err(|e| {
                ConfigError::InvalidValue("STALE_MISSION_HOURS".to_string(), format!("{}", e))
            })?;

        // Maximum parallel missions (default: 1 = sequential)
        let max_parallel_missions = std::env::var("MAX_PARALLEL_MISSIONS")
            .unwrap_or_else(|_| "1".to_string())
            .parse()
            .map_err(|e| {
                ConfigError::InvalidValue("MAX_PARALLEL_MISSIONS".to_string(), format!("{}", e))
            })?;

        let dev_mode = std::env::var("DEV_MODE")
            .ok()
            .map(|v| {
                parse_bool(&v).map_err(|e| ConfigError::InvalidValue("DEV_MODE".to_string(), e))
            })
            .transpose()?
            // In debug builds, default to dev_mode=true; in release, default to false.
            .unwrap_or(cfg!(debug_assertions));

        // Support both new (SANDBOXED_USERS) and legacy (SANDBOXED_SH_USERS) env vars
        let users = std::env::var("SANDBOXED_USERS")
            .or_else(|_| std::env::var("SANDBOXED_SH_USERS"))
            .ok()
            .filter(|raw| !raw.trim().is_empty())
            .map(|raw| {
                serde_json::from_str::<Vec<UserAccount>>(&raw).map_err(|e| {
                    ConfigError::InvalidValue(
                        "SANDBOXED_USERS/SANDBOXED_SH_USERS".to_string(),
                        e.to_string(),
                    )
                })
            })
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|mut user| {
                if user.id.trim().is_empty() {
                    user.id = user.username.clone();
                }
                user
            })
            .collect::<Vec<_>>();

        let auth = AuthConfig {
            dashboard_password: std::env::var("DASHBOARD_PASSWORD").ok(),
            jwt_secret: std::env::var("JWT_SECRET").ok(),
            jwt_ttl_days: std::env::var("JWT_TTL_DAYS")
                .ok()
                .map(|v| {
                    v.parse::<i64>().map_err(|e| {
                        ConfigError::InvalidValue("JWT_TTL_DAYS".to_string(), format!("{}", e))
                    })
                })
                .transpose()?
                .unwrap_or(30),
            users,
        };

        // In non-dev mode, require auth secrets to be set.
        if !dev_mode {
            match auth.auth_mode(dev_mode) {
                AuthMode::MultiUser => {
                    if auth.users.is_empty() {
                        return Err(ConfigError::MissingEnvVar(
                            "SANDBOXED_USERS or SANDBOXED_SH_USERS".to_string(),
                        ));
                    }
                    if auth.jwt_secret.is_none() {
                        return Err(ConfigError::MissingEnvVar("JWT_SECRET".to_string()));
                    }
                    if auth
                        .users
                        .iter()
                        .any(|u| u.username.trim().is_empty() || u.password.trim().is_empty())
                    {
                        return Err(ConfigError::InvalidValue(
                            "SANDBOXED_USERS/SANDBOXED_SH_USERS".to_string(),
                            "username/password must be non-empty".to_string(),
                        ));
                    }
                }
                AuthMode::SingleTenant => {
                    if auth.dashboard_password.is_none() {
                        return Err(ConfigError::MissingEnvVar("DASHBOARD_PASSWORD".to_string()));
                    }
                    if auth.jwt_secret.is_none() {
                        return Err(ConfigError::MissingEnvVar("JWT_SECRET".to_string()));
                    }
                }
                AuthMode::Disabled => {
                    // Provide a more specific error message when partial config exists
                    if auth.dashboard_password.is_some() && auth.jwt_secret.is_none() {
                        return Err(ConfigError::MissingEnvVar("JWT_SECRET".to_string()));
                    }
                    return Err(ConfigError::MissingEnvVar(
                        "DASHBOARD_PASSWORD or SANDBOXED_SH_USERS".to_string(),
                    ));
                }
            }
        }

        let context = ContextConfig::from_env();

        // Library configuration
        // Note: library_remote is now managed via the settings module (persisted to disk)
        let library_path = std::env::var("LIBRARY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| working_dir.join(".sandboxed-sh/library"));

        Ok(Self {
            default_model,
            working_dir,
            host,
            port,
            max_iterations,
            stale_mission_hours,
            max_parallel_missions,
            dev_mode,
            auth,
            context,
            opencode_base_url,
            opencode_agent,
            opencode_permissive,
            library_path,
        })
    }

    /// Create a config with custom values (useful for testing).
    pub fn new(working_dir: PathBuf) -> Self {
        let library_path = working_dir.join(".sandboxed-sh/library");
        Self {
            default_model: None,
            working_dir,
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_iterations: 50,
            stale_mission_hours: 2,
            max_parallel_missions: 1,
            dev_mode: true,
            auth: AuthConfig::default(),
            context: ContextConfig::default(),
            opencode_base_url: "http://127.0.0.1:4096".to_string(),
            opencode_agent: None,
            opencode_permissive: true,
            library_path,
        }
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "f" | "no" | "n" | "off" => Ok(false),
        other => Err(format!("expected boolean-like value, got: {}", other)),
    }
}
