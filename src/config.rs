//! Configuration management for Open Agent.
//!
//! Configuration can be set via environment variables:
//! - `OPENROUTER_API_KEY` - Required. Your OpenRouter API key.
//! - `DEFAULT_MODEL` - Optional. The default LLM model to use. Defaults to `openai/gpt-5-mini`.
//! - `WORKSPACE_PATH` - Optional. The workspace directory. Defaults to current directory.
//! - `HOST` - Optional. Server host. Defaults to `127.0.0.1`.
//! - `PORT` - Optional. Server port. Defaults to `3000`.
//! - `MAX_ITERATIONS` - Optional. Maximum agent loop iterations. Defaults to `50`.
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

use std::path::PathBuf;
use thiserror::Error;
use base64::Engine;

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
    /// OpenRouter API key
    pub api_key: String,
    
    /// Default LLM model identifier (OpenRouter format)
    pub default_model: String,
    
    /// Workspace directory for file operations
    pub workspace_path: PathBuf,
    
    /// Server host
    pub host: String,
    
    /// Server port
    pub port: u16,
    
    /// Maximum iterations for the agent loop
    pub max_iterations: usize,
    
    /// Development mode (disables auth; more permissive defaults)
    pub dev_mode: bool,

    /// API auth configuration (dashboard login)
    pub auth: AuthConfig,

    /// Remote console/file explorer SSH configuration (optional).
    pub console_ssh: ConsoleSshConfig,

    /// Memory/storage configuration
    pub memory: MemoryConfig,
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
        self.private_key.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
    }
}

/// API auth configuration (single-tenant).
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Password required by the dashboard to obtain a JWT.
    pub dashboard_password: Option<String>,

    /// HMAC secret for signing/verifying JWTs.
    pub jwt_secret: Option<String>,

    /// JWT validity in days.
    pub jwt_ttl_days: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            dashboard_password: None,
            jwt_secret: None,
            jwt_ttl_days: 30,
        }
    }
}

impl AuthConfig {
    /// Whether auth is required for API requests.
    pub fn auth_required(&self, dev_mode: bool) -> bool {
        !dev_mode && self.dashboard_password.is_some() && self.jwt_secret.is_some()
    }
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::MissingEnvVar` if `OPENROUTER_API_KEY` is not set.
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| ConfigError::MissingEnvVar("OPENROUTER_API_KEY".to_string()))?;
        
        let default_model = std::env::var("DEFAULT_MODEL")
            .unwrap_or_else(|_| "anthropic/claude-sonnet-4.5".to_string());
        
        let workspace_path = std::env::var("WORKSPACE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        
        let host = std::env::var("HOST")
            .unwrap_or_else(|_| "127.0.0.1".to_string());
        
        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .map_err(|e| ConfigError::InvalidValue("PORT".to_string(), format!("{}", e)))?;
        
        let max_iterations = std::env::var("MAX_ITERATIONS")
            .unwrap_or_else(|_| "50".to_string())
            .parse()
            .map_err(|e| ConfigError::InvalidValue("MAX_ITERATIONS".to_string(), format!("{}", e)))?;

        let dev_mode = std::env::var("DEV_MODE")
            .ok()
            .map(|v| parse_bool(&v).map_err(|e| ConfigError::InvalidValue("DEV_MODE".to_string(), e)))
            .transpose()?
            // In debug builds, default to dev_mode=true; in release, default to false.
            .unwrap_or(cfg!(debug_assertions));

        let auth = AuthConfig {
            dashboard_password: std::env::var("DASHBOARD_PASSWORD").ok(),
            jwt_secret: std::env::var("JWT_SECRET").ok(),
            jwt_ttl_days: std::env::var("JWT_TTL_DAYS")
                .ok()
                .map(|v| v.parse::<i64>().map_err(|e| ConfigError::InvalidValue("JWT_TTL_DAYS".to_string(), format!("{}", e))))
                .transpose()?
                .unwrap_or(30),
        };

        // In non-dev mode, require auth secrets to be set.
        if !dev_mode {
            if auth.dashboard_password.is_none() {
                return Err(ConfigError::MissingEnvVar("DASHBOARD_PASSWORD".to_string()));
            }
            if auth.jwt_secret.is_none() {
                return Err(ConfigError::MissingEnvVar("JWT_SECRET".to_string()));
            }
        }
        
        // Memory configuration (optional)
        let memory = MemoryConfig {
            supabase_url: std::env::var("SUPABASE_URL").ok(),
            supabase_service_role_key: std::env::var("SUPABASE_SERVICE_ROLE_KEY").ok(),
            embed_model: std::env::var("MEMORY_EMBED_MODEL")
                .unwrap_or_else(|_| "openai/text-embedding-3-small".to_string()),
            rerank_model: std::env::var("MEMORY_RERANK_MODEL").ok(),
            embed_dimension: 1536, // OpenAI text-embedding-3-small default
        };

        let console_ssh = ConsoleSshConfig {
            host: std::env::var("CONSOLE_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("CONSOLE_SSH_PORT")
                .ok()
                .map(|v| v.parse::<u16>().map_err(|e| ConfigError::InvalidValue("CONSOLE_SSH_PORT".to_string(), format!("{}", e))))
                .transpose()?
                .unwrap_or(22),
            user: std::env::var("CONSOLE_SSH_USER").unwrap_or_else(|_| "root".to_string()),
            private_key: read_private_key_from_env()?,
        };
        
        Ok(Self {
            api_key,
            default_model,
            workspace_path,
            host,
            port,
            max_iterations,
            dev_mode,
            auth,
            console_ssh,
            memory,
        })
    }
    
    /// Create a config with custom values (useful for testing).
    pub fn new(
        api_key: String,
        default_model: String,
        workspace_path: PathBuf,
    ) -> Self {
        Self {
            api_key,
            default_model,
            workspace_path,
            host: "127.0.0.1".to_string(),
            port: 3000,
            max_iterations: 50,
            dev_mode: true,
            auth: AuthConfig::default(),
            console_ssh: ConsoleSshConfig::default(),
            memory: MemoryConfig::default(),
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

fn read_private_key_from_env() -> Result<Option<String>, ConfigError> {
    // Recommended: load from file path to avoid large/multiline env values.
    if let Ok(path) = std::env::var("CONSOLE_SSH_PRIVATE_KEY_PATH") {
        if path.trim().is_empty() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(path.trim())
            .map_err(|e| ConfigError::InvalidValue("CONSOLE_SSH_PRIVATE_KEY_PATH".to_string(), format!("{}", e)))?;
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
            .map_err(|e| ConfigError::InvalidValue("CONSOLE_SSH_PRIVATE_KEY_B64".to_string(), format!("{}", e)))?;
        let s = String::from_utf8(bytes)
            .map_err(|e| ConfigError::InvalidValue("CONSOLE_SSH_PRIVATE_KEY_B64".to_string(), format!("{}", e)))?;
        return Ok(Some(s));
    }

    // Fallback: raw private key in env (EnvironmentFile can support multiline).
    match std::env::var("CONSOLE_SSH_PRIVATE_KEY") {
        Ok(s) if !s.trim().is_empty() => Ok(Some(s)),
        _ => Ok(None),
    }
}

