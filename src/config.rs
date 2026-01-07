//! Configuration management for Open Agent.
//!
//! Open Agent uses OpenCode as its execution backend. Configuration can be set via environment variables:
//! - `DEFAULT_MODEL` - Optional. Default OpenCode model to request (e.g. `claude-opus-4-5-20251101`).
//! - `WORKING_DIR` - Optional. Default working directory for relative paths. Defaults to `/root` in production, current directory in dev.
//! - `HOST` - Optional. Server host. Defaults to `127.0.0.1`.
//! - `PORT` - Optional. Server port. Defaults to `3000`.
//! - `MAX_ITERATIONS` - Optional. Maximum agent loop iterations. Defaults to `50`.
//! - `OPENCODE_BASE_URL` - Optional. Base URL for OpenCode server. Defaults to `http://127.0.0.1:4096`.
//! - `OPENCODE_AGENT` - Optional. OpenCode agent name (e.g., `build`, `plan`).
//! - `OPENCODE_PERMISSIVE` - Optional. If true, auto-allows all permissions for OpenCode sessions (default: true).
//! - `OPEN_AGENT_USERS` - Optional. JSON array of user accounts for multi-user auth.
//! - `CONSOLE_SSH_HOST` - Optional. Host for dashboard console/file explorer SSH (default: 127.0.0.1).
//! - `CONSOLE_SSH_PORT` - Optional. SSH port (default: 22).
//! - `CONSOLE_SSH_USER` - Optional. SSH user (default: root).
//! - `CONSOLE_SSH_PRIVATE_KEY_PATH` - Optional. Path to an OpenSSH private key file (recommended).
//! - `CONSOLE_SSH_PRIVATE_KEY_B64` - Optional. Base64-encoded OpenSSH private key.
//! - `CONSOLE_SSH_PRIVATE_KEY` - Optional. Raw (multiline) OpenSSH private key (fallback).
//! - `SUPABASE_URL` - Optional. Supabase project URL for memory storage.
//! - `SUPABASE_SERVICE_ROLE_KEY` - Optional. Service role key for Supabase.
//! - `MEMORY_EMBED_MODEL` - Optional. Embedding model. Defaults to `openai/text-embedding-3-small`.
//! - `MEMORY_RERANK_MODEL` - Optional. Reranker model.
//!
//! Note: The agent has **full system access**. It can read/write any file, execute any command,
//! and search anywhere on the machine. The `WORKING_DIR` is just the default for relative paths.

use base64::Engine;
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

/// Memory/storage configuration.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Supabase project URL
    pub supabase_url: Option<String>,

    /// Supabase service role key (for full access)
    pub supabase_service_role_key: Option<String>,

    /// Embedding model for vector storage
    pub embed_model: String,

    /// Reranker model for precision retrieval
    pub rerank_model: Option<String>,

    /// Embedding dimension (must match model output)
    pub embed_dimension: usize,
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

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            supabase_url: None,
            supabase_service_role_key: None,
            embed_model: "openai/text-embedding-3-small".to_string(),
            rerank_model: None,
            embed_dimension: 1536,
        }
    }
}

impl MemoryConfig {
    /// Check if memory is enabled (Supabase configured)
    pub fn is_enabled(&self) -> bool {
        self.supabase_url.is_some() && self.supabase_service_role_key.is_some()
    }
}

/// Agent configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Default OpenCode model identifier (provider/model format).
    pub default_model: String,

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

    /// Remote console/file explorer SSH configuration (optional).
    pub console_ssh: ConsoleSshConfig,

    /// Memory/storage configuration
    pub memory: MemoryConfig,

    /// Context injection configuration
    pub context: ContextConfig,

    /// OpenCode server base URL
    pub opencode_base_url: String,

    /// Default OpenCode agent name (e.g., "build", "plan")
    pub opencode_agent: Option<String>,

    /// Whether to auto-allow all OpenCode permissions for created sessions
    pub opencode_permissive: bool,

    /// Timeout in seconds after which a stuck tool will be auto-aborted.
    /// Set to 0 to disable auto-abort (default: 0 = warn only, don't abort).
    pub tool_stuck_abort_timeout_secs: u64,

    /// Path to the configuration library git repo.
    /// Default: {working_dir}/.openagent/library
    pub library_path: PathBuf,

    /// Git remote URL for the configuration library.
    /// Set via LIBRARY_REMOTE env var. Runtime settings can override this.
    pub library_remote: Option<String>,
}

/// SSH configuration for the dashboard console + file explorer.
#[derive(Debug, Clone)]
pub struct ConsoleSshConfig {
    /// Host to SSH into (default: 127.0.0.1)
    pub host: String,
    /// SSH port (default: 22)
    pub port: u16,
    /// SSH username (default: root)
    pub user: String,
    /// Private key (OpenSSH) used for auth (prefer *_B64 env)
    pub private_key: Option<String>,
}

impl Default for ConsoleSshConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 22,
            user: "root".to_string(),
            private_key: None,
        }
    }
}

impl ConsoleSshConfig {
    pub fn is_configured(&self) -> bool {
        self.private_key
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }
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

        // Tool stuck abort timeout (default: 0 = disabled, warn only)
        let tool_stuck_abort_timeout_secs = std::env::var("TOOL_STUCK_ABORT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let default_model = std::env::var("DEFAULT_MODEL")
            .unwrap_or_else(|_| "claude-opus-4-5-20251101".to_string());

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

        let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

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

        // Hours of inactivity after which an active mission is auto-closed
        // Default: 24 hours. Set to 0 to disable.
        let stale_mission_hours = std::env::var("STALE_MISSION_HOURS")
            .unwrap_or_else(|_| "24".to_string())
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

        let users = std::env::var("OPEN_AGENT_USERS")
            .ok()
            .filter(|raw| !raw.trim().is_empty())
            .map(|raw| {
                serde_json::from_str::<Vec<UserAccount>>(&raw).map_err(|e| {
                    ConfigError::InvalidValue("OPEN_AGENT_USERS".to_string(), e.to_string())
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
                        return Err(ConfigError::MissingEnvVar("OPEN_AGENT_USERS".to_string()));
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
                            "OPEN_AGENT_USERS".to_string(),
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
                        "DASHBOARD_PASSWORD or OPEN_AGENT_USERS".to_string(),
                    ));
                }
            }
        }

        // Memory configuration (optional)
        let embed_model = std::env::var("MEMORY_EMBED_MODEL")
            .unwrap_or_else(|_| "openai/text-embedding-3-small".to_string());

        // Determine embed dimension from env or infer from model
        let embed_dimension = std::env::var("MEMORY_EMBED_DIMENSION")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| infer_embed_dimension(&embed_model));

        let memory = MemoryConfig {
            supabase_url: std::env::var("SUPABASE_URL").ok(),
            supabase_service_role_key: std::env::var("SUPABASE_SERVICE_ROLE_KEY").ok(),
            embed_model,
            rerank_model: std::env::var("MEMORY_RERANK_MODEL").ok(),
            embed_dimension,
        };

        let console_ssh = ConsoleSshConfig {
            host: std::env::var("CONSOLE_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("CONSOLE_SSH_PORT")
                .ok()
                .map(|v| {
                    v.parse::<u16>().map_err(|e| {
                        ConfigError::InvalidValue("CONSOLE_SSH_PORT".to_string(), format!("{}", e))
                    })
                })
                .transpose()?
                .unwrap_or(22),
            user: std::env::var("CONSOLE_SSH_USER").unwrap_or_else(|_| "root".to_string()),
            private_key: read_private_key_from_env()?,
        };

        let context = ContextConfig::from_env();

        // Library configuration
        let library_path = std::env::var("LIBRARY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| working_dir.join(".openagent/library"));

        let library_remote = std::env::var("LIBRARY_REMOTE").ok();

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
            console_ssh,
            memory,
            context,
            opencode_base_url,
            opencode_agent,
            opencode_permissive,
            tool_stuck_abort_timeout_secs,
            library_path,
            library_remote,
        })
    }

    /// Create a config with custom values (useful for testing).
    pub fn new(default_model: String, working_dir: PathBuf) -> Self {
        let library_path = working_dir.join(".openagent/library");
        Self {
            default_model,
            working_dir,
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_iterations: 50,
            stale_mission_hours: 24,
            max_parallel_missions: 1,
            dev_mode: true,
            auth: AuthConfig::default(),
            console_ssh: ConsoleSshConfig::default(),
            memory: MemoryConfig::default(),
            context: ContextConfig::default(),
            opencode_base_url: "http://127.0.0.1:4096".to_string(),
            opencode_agent: None,
            opencode_permissive: true,
            tool_stuck_abort_timeout_secs: 0,
            library_path,
            library_remote: None,
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

/// Infer embedding dimension from model name.
fn infer_embed_dimension(model: &str) -> usize {
    let model_lower = model.to_lowercase();

    // Qwen embedding models output 4096 dimensions
    if model_lower.contains("qwen") && model_lower.contains("embedding") {
        return 4096;
    }

    // OpenAI text-embedding-3 models
    if model_lower.contains("text-embedding-3") {
        if model_lower.contains("large") {
            return 3072;
        }
        return 1536; // small
    }

    // OpenAI ada
    if model_lower.contains("ada") {
        return 1536;
    }

    // Cohere embed models
    if model_lower.contains("embed-english") || model_lower.contains("embed-multilingual") {
        return 1024;
    }

    // Default fallback
    1536
}

fn read_private_key_from_env() -> Result<Option<String>, ConfigError> {
    // Recommended: load from file path to avoid large/multiline env values.
    if let Ok(path) = std::env::var("CONSOLE_SSH_PRIVATE_KEY_PATH") {
        if path.trim().is_empty() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(path.trim()).map_err(|e| {
            ConfigError::InvalidValue("CONSOLE_SSH_PRIVATE_KEY_PATH".to_string(), format!("{}", e))
        })?;
        if s.trim().is_empty() {
            return Ok(None);
        }
        return Ok(Some(s));
    }

    // Prefer base64 to avoid multiline env complications.
    if let Ok(b64) = std::env::var("CONSOLE_SSH_PRIVATE_KEY_B64") {
        if b64.trim().is_empty() {
            return Ok(None);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim().as_bytes())
            .map_err(|e| {
                ConfigError::InvalidValue(
                    "CONSOLE_SSH_PRIVATE_KEY_B64".to_string(),
                    format!("{}", e),
                )
            })?;
        let s = String::from_utf8(bytes).map_err(|e| {
            ConfigError::InvalidValue("CONSOLE_SSH_PRIVATE_KEY_B64".to_string(), format!("{}", e))
        })?;
        return Ok(Some(s));
    }

    // Fallback: raw private key in env (EnvironmentFile can support multiline).
    match std::env::var("CONSOLE_SSH_PRIVATE_KEY") {
        Ok(s) if !s.trim().is_empty() => Ok(Some(s)),
        _ => Ok(None),
    }
}
