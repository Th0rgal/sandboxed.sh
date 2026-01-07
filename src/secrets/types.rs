//! Secrets types and data structures.
//!
//! This module defines the types for encrypted secrets storage:
//! - Public keys stored in git-tracked config
//! - Encrypted secrets stored in registries (git-tracked)
//! - Private keys stored locally (never committed)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for the secrets system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsConfig {
    /// Version of the secrets format
    pub version: u32,
    /// Default key ID to use for encryption
    pub default_key: String,
    /// Available public keys (key_id -> key info)
    pub keys: HashMap<String, PublicKeyInfo>,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            version: 1,
            default_key: "default".to_string(),
            keys: HashMap::new(),
        }
    }
}

/// Information about a public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyInfo {
    /// Key algorithm
    pub algorithm: KeyAlgorithm,
    /// Public key in PEM format (filename in keys/ directory)
    pub public_key_file: String,
    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When this key was created
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Supported key algorithms.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KeyAlgorithm {
    /// AES-256-GCM with key derived from passphrase (PBKDF2)
    /// Simple and portable - no RSA key management needed
    Aes256Gcm,
}

impl Default for KeyAlgorithm {
    fn default() -> Self {
        Self::Aes256Gcm
    }
}

/// A registry of encrypted secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRegistry {
    /// Registry name/purpose
    pub name: String,
    /// Description of what this registry contains
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Which key was used to encrypt these secrets
    pub key_id: String,
    /// The encrypted secrets (name -> encrypted value)
    pub secrets: HashMap<String, EncryptedSecret>,
    /// When this registry was last modified
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl SecretRegistry {
    /// Create a new empty registry.
    pub fn new(name: String, key_id: String) -> Self {
        Self {
            name,
            description: None,
            key_id,
            secrets: HashMap::new(),
            updated_at: chrono::Utc::now(),
        }
    }
}

/// An encrypted secret value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSecret {
    /// The encrypted value (base64 encoded)
    pub ciphertext: String,
    /// Nonce/IV used (base64 encoded)
    pub nonce: String,
    /// Salt for key derivation (base64 encoded)
    pub salt: String,
    /// Optional metadata (not encrypted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SecretMetadata>,
}

/// Metadata about a secret (not encrypted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    /// What type of secret this is
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub secret_type: Option<SecretType>,
    /// When this secret expires (for tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Additional non-sensitive info
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

impl Default for SecretMetadata {
    fn default() -> Self {
        Self {
            secret_type: None,
            expires_at: None,
            labels: HashMap::new(),
        }
    }
}

/// Types of secrets that can be stored.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SecretType {
    /// OAuth access token
    #[serde(rename = "oauth_access_token")]
    OAuthAccessToken,
    /// OAuth refresh token
    #[serde(rename = "oauth_refresh_token")]
    OAuthRefreshToken,
    /// API key
    #[serde(rename = "api_key")]
    ApiKey,
    /// Password
    #[serde(rename = "password")]
    Password,
    /// Generic secret
    #[serde(rename = "generic")]
    Generic,
}

/// Summary information about a secret (for listing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretInfo {
    /// Secret key/name
    pub key: String,
    /// Type of secret
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_type: Option<SecretType>,
    /// When this secret expires
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Labels/tags
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    /// Whether the secret has expired
    pub is_expired: bool,
}

/// Summary information about a registry (for listing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryInfo {
    /// Registry name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Number of secrets in this registry
    pub secret_count: usize,
    /// When last modified
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Result of initializing keys.
#[derive(Debug, Clone, Serialize)]
pub struct InitializeKeysResult {
    /// The key ID that was created
    pub key_id: String,
    /// Message about where to store the passphrase
    pub message: String,
}

/// Status of the secrets system.
#[derive(Debug, Clone, Serialize)]
pub struct SecretsStatus {
    /// Whether the secrets system is initialized (has at least one key)
    pub initialized: bool,
    /// Whether we can decrypt (passphrase is available)
    pub can_decrypt: bool,
    /// List of registries
    pub registries: Vec<RegistryInfo>,
    /// Default key ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_key: Option<String>,
}

/// Request to set a secret.
#[derive(Debug, Clone, Deserialize)]
pub struct SetSecretRequest {
    /// The secret value
    pub value: String,
    /// Optional metadata
    #[serde(default)]
    pub metadata: Option<SecretMetadata>,
}

/// Request to unlock secrets with passphrase.
#[derive(Debug, Clone, Deserialize)]
pub struct UnlockRequest {
    /// The passphrase to unlock secrets
    pub passphrase: String,
}

/// Request to initialize the secrets system.
#[derive(Debug, Clone, Deserialize)]
pub struct InitializeRequest {
    /// Key ID to create (defaults to "default")
    #[serde(default = "default_key_id")]
    pub key_id: String,
}

fn default_key_id() -> String {
    "default".to_string()
}
