//! Secrets management module.
//!
//! Provides encrypted storage for sensitive data like OAuth tokens and API keys.
//!
//! ## Architecture
//!
//! ```text
//! .sandboxed-sh/secrets/
//! ├── config.json           # Key configuration (git-tracked)
//! ├── keys/
//! │   └── default.key       # Key marker file (git-tracked)
//! └── registries/
//!     ├── mcp-tokens.json   # Encrypted MCP tokens (git-tracked)
//!     └── api-keys.json     # Encrypted API keys (git-tracked)
//! ```
//!
//! The actual passphrase is provided via:
//! - `SANDBOXED_SECRET_PASSPHRASE` (or legacy `OPENAGENT_SECRET_PASSPHRASE`) environment variable
//! - Or via the unlock API endpoint (session-based)
//!
//! ## Usage
//!
//! ```ignore
//! // Create store
//! let store = SecretsStore::new(&working_dir).await?;
//!
//! // Initialize (first time only)
//! store.initialize("default").await?;
//!
//! // Unlock with passphrase
//! store.unlock("my-secret-passphrase").await?;
//!
//! // Set a secret
//! store.set_secret("mcp-tokens", "my-service/api_key", "sk-...", None).await?;
//!
//! // Get a secret
//! let token = store.get_secret("mcp-tokens", "my-service/api_key").await?;
//!
//! // Export to workspace
//! store.export_to_workspace(&workspace_path, "mcp-tokens", None).await?;
//! ```

mod crypto;
mod store;
pub mod types;

pub use crypto::{CryptoError, SecretsCrypto};
pub use store::SecretsStore;
pub use types::*;
