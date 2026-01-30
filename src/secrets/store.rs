//! Secrets store - manages encrypted secrets storage.
//!
//! Provides:
//! - Initialization of the secrets system
//! - CRUD operations on secrets within registries
//! - Export of decrypted secrets to workspaces

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::RwLock;

use super::crypto::SecretsCrypto;
use super::types::*;

/// Store for managing encrypted secrets.
pub struct SecretsStore {
    /// Base directory (.sandboxed-sh/secrets)
    base_dir: PathBuf,
    /// Configuration
    config: RwLock<SecretsConfig>,
    /// Crypto engine
    crypto: RwLock<SecretsCrypto>,
    /// Cached registries
    registries: RwLock<HashMap<String, SecretRegistry>>,
}

impl SecretsStore {
    /// Create a new secrets store.
    pub async fn new(working_dir: &Path) -> Result<Self> {
        let base_dir = working_dir.join(".sandboxed-sh").join("secrets");

        // Load or create config
        let config_path = base_dir.join("config.json");
        let config = if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .await
                .context("Failed to read secrets config")?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            SecretsConfig::default()
        };

        // Try to load passphrase from environment (support both new and legacy names)
        let mut crypto = SecretsCrypto::new();
        if let Ok(passphrase) = std::env::var("SANDBOXED_SECRET_PASSPHRASE")
            .or_else(|_| std::env::var("OPENAGENT_SECRET_PASSPHRASE"))
        {
            if !passphrase.is_empty() {
                crypto.set_passphrase(passphrase);
            }
        }

        let store = Self {
            base_dir,
            config: RwLock::new(config),
            crypto: RwLock::new(crypto),
            registries: RwLock::new(HashMap::new()),
        };

        // Load existing registries
        store.load_registries().await?;

        Ok(store)
    }

    /// Load all registries from disk.
    async fn load_registries(&self) -> Result<()> {
        let registries_dir = self.base_dir.join("registries");

        if !registries_dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(&registries_dir).await?;
        let mut registries = self.registries.write().await;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(registry) = serde_json::from_str::<SecretRegistry>(&content) {
                        registries.insert(registry.name.clone(), registry);
                    }
                }
            }
        }

        Ok(())
    }

    /// Save the config to disk.
    async fn save_config(&self) -> Result<()> {
        let config = self.config.read().await;
        let config_path = self.base_dir.join("config.json");

        // Ensure directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(&*config)?;
        fs::write(&config_path, content).await?;

        Ok(())
    }

    /// Save a registry to disk.
    async fn save_registry(&self, registry: &SecretRegistry) -> Result<()> {
        let registries_dir = self.base_dir.join("registries");
        fs::create_dir_all(&registries_dir).await?;

        let path = registries_dir.join(format!("{}.json", registry.name));
        let content = serde_json::to_string_pretty(registry)?;
        fs::write(&path, content).await?;

        Ok(())
    }

    /// Check if the secrets system is initialized.
    pub async fn is_initialized(&self) -> bool {
        let config = self.config.read().await;
        !config.keys.is_empty()
    }

    /// Check if we can decrypt (passphrase is available).
    pub async fn can_decrypt(&self) -> bool {
        self.crypto.read().await.has_passphrase()
    }

    /// Get the status of the secrets system.
    pub async fn status(&self) -> SecretsStatus {
        let config = self.config.read().await;
        let registries = self.registries.read().await;
        let crypto = self.crypto.read().await;

        let registry_infos: Vec<RegistryInfo> = registries
            .values()
            .map(|r| RegistryInfo {
                name: r.name.clone(),
                description: r.description.clone(),
                secret_count: r.secrets.len(),
                updated_at: r.updated_at,
            })
            .collect();

        SecretsStatus {
            initialized: !config.keys.is_empty(),
            can_decrypt: crypto.has_passphrase(),
            registries: registry_infos,
            default_key: if config.keys.is_empty() {
                None
            } else {
                Some(config.default_key.clone())
            },
        }
    }

    /// Initialize the secrets system with a new key.
    ///
    /// This creates the key entry in config but doesn't store any passphrase.
    /// The user must provide the passphrase via environment variable or unlock endpoint.
    pub async fn initialize(&self, key_id: &str) -> Result<InitializeKeysResult> {
        // Ensure directories exist
        let keys_dir = self.base_dir.join("keys");
        fs::create_dir_all(&keys_dir).await?;

        // Create a marker file for the key (just to track that it exists)
        let key_marker = keys_dir.join(format!("{}.key", key_id));
        fs::write(&key_marker, format!("# Key: {}\n# Created: {}\n# This file marks that this key exists.\n# The actual passphrase must be provided via SANDBOXED_SECRET_PASSPHRASE (or legacy OPENAGENT_SECRET_PASSPHRASE) env var.\n", key_id, chrono::Utc::now())).await?;

        // Update config
        {
            let mut config = self.config.write().await;
            config.keys.insert(
                key_id.to_string(),
                PublicKeyInfo {
                    algorithm: KeyAlgorithm::Aes256Gcm,
                    public_key_file: format!("{}.key", key_id),
                    description: Some("Default encryption key".to_string()),
                    created_at: chrono::Utc::now(),
                },
            );
            config.default_key = key_id.to_string();
        }

        self.save_config().await?;

        Ok(InitializeKeysResult {
            key_id: key_id.to_string(),
            message: "Secrets system initialized. Set SANDBOXED_SECRET_PASSPHRASE environment variable with your passphrase to enable encryption/decryption.".to_string(),
        })
    }

    /// Unlock the secrets system with a passphrase.
    pub async fn unlock(&self, passphrase: &str) -> Result<()> {
        // If we have existing secrets, verify the passphrase works
        let registries = self.registries.read().await;

        for registry in registries.values() {
            if let Some((_, secret)) = registry.secrets.iter().next() {
                // Try to decrypt one secret to verify passphrase
                let mut crypto = self.crypto.write().await;
                crypto.set_passphrase(passphrase.to_string());

                if let Err(_) = crypto.decrypt(secret) {
                    crypto.clear_passphrase();
                    anyhow::bail!("Invalid passphrase");
                }

                return Ok(());
            }
        }

        // No existing secrets to verify against, just set the passphrase
        let mut crypto = self.crypto.write().await;
        crypto.set_passphrase(passphrase.to_string());

        Ok(())
    }

    /// Lock the secrets system (clear passphrase).
    pub async fn lock(&self) {
        let mut crypto = self.crypto.write().await;
        crypto.clear_passphrase();
    }

    /// List all registries.
    pub async fn list_registries(&self) -> Vec<RegistryInfo> {
        let registries = self.registries.read().await;
        registries
            .values()
            .map(|r| RegistryInfo {
                name: r.name.clone(),
                description: r.description.clone(),
                secret_count: r.secrets.len(),
                updated_at: r.updated_at,
            })
            .collect()
    }

    /// Get or create a registry.
    pub async fn get_or_create_registry(&self, name: &str) -> Result<()> {
        let mut registries = self.registries.write().await;

        if registries.contains_key(name) {
            return Ok(());
        }

        let config = self.config.read().await;
        let registry = SecretRegistry::new(name.to_string(), config.default_key.clone());

        // Save to disk
        self.save_registry(&registry).await?;

        registries.insert(name.to_string(), registry);

        Ok(())
    }

    /// List secrets in a registry.
    pub async fn list_secrets(&self, registry_name: &str) -> Result<Vec<SecretInfo>> {
        let registries = self.registries.read().await;
        let registry = registries
            .get(registry_name)
            .ok_or_else(|| anyhow::anyhow!("Registry not found: {}", registry_name))?;

        let now = chrono::Utc::now().timestamp();

        Ok(registry
            .secrets
            .iter()
            .map(|(key, secret)| {
                let expires_at = secret.metadata.as_ref().and_then(|m| m.expires_at);
                let is_expired = expires_at.map(|exp| exp < now).unwrap_or(false);

                SecretInfo {
                    key: key.clone(),
                    secret_type: secret.metadata.as_ref().and_then(|m| m.secret_type),
                    expires_at,
                    labels: secret
                        .metadata
                        .as_ref()
                        .map(|m| m.labels.clone())
                        .unwrap_or_default(),
                    is_expired,
                }
            })
            .collect())
    }

    /// Get a decrypted secret value.
    pub async fn get_secret(&self, registry_name: &str, key: &str) -> Result<String> {
        let crypto = self.crypto.read().await;
        if !crypto.has_passphrase() {
            anyhow::bail!("Secrets are locked. Provide passphrase to unlock.");
        }

        let registries = self.registries.read().await;
        let registry = registries
            .get(registry_name)
            .ok_or_else(|| anyhow::anyhow!("Registry not found: {}", registry_name))?;

        let secret = registry
            .secrets
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("Secret not found: {}", key))?;

        crypto
            .decrypt(secret)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt: {}", e))
    }

    /// Set a secret value.
    pub async fn set_secret(
        &self,
        registry_name: &str,
        key: &str,
        value: &str,
        metadata: Option<SecretMetadata>,
    ) -> Result<()> {
        // Ensure registry exists
        self.get_or_create_registry(registry_name).await?;

        let crypto = self.crypto.read().await;
        if !crypto.has_passphrase() {
            anyhow::bail!("Secrets are locked. Provide passphrase to unlock.");
        }

        // Encrypt the value
        let mut encrypted = crypto
            .encrypt(value)
            .map_err(|e| anyhow::anyhow!("Failed to encrypt: {}", e))?;
        encrypted.metadata = metadata;

        drop(crypto);

        // Update registry
        let mut registries = self.registries.write().await;
        let registry = registries
            .get_mut(registry_name)
            .ok_or_else(|| anyhow::anyhow!("Registry not found: {}", registry_name))?;

        registry.secrets.insert(key.to_string(), encrypted);
        registry.updated_at = chrono::Utc::now();

        // Save to disk
        self.save_registry(registry).await?;

        Ok(())
    }

    /// Delete a secret.
    pub async fn delete_secret(&self, registry_name: &str, key: &str) -> Result<()> {
        let mut registries = self.registries.write().await;
        let registry = registries
            .get_mut(registry_name)
            .ok_or_else(|| anyhow::anyhow!("Registry not found: {}", registry_name))?;

        if registry.secrets.remove(key).is_none() {
            anyhow::bail!("Secret not found: {}", key);
        }

        registry.updated_at = chrono::Utc::now();

        // Save to disk
        self.save_registry(registry).await?;

        Ok(())
    }

    /// Delete a registry and all its secrets.
    pub async fn delete_registry(&self, registry_name: &str) -> Result<()> {
        let mut registries = self.registries.write().await;

        if registries.remove(registry_name).is_none() {
            anyhow::bail!("Registry not found: {}", registry_name);
        }

        // Delete file
        let path = self
            .base_dir
            .join("registries")
            .join(format!("{}.json", registry_name));
        if path.exists() {
            fs::remove_file(&path).await?;
        }

        Ok(())
    }

    /// Export secrets to a workspace file (decrypted).
    ///
    /// This creates a .mcp-secrets.json file in the workspace with decrypted secrets.
    /// The file should be gitignored.
    pub async fn export_to_workspace(
        &self,
        workspace_path: &Path,
        registry_name: &str,
        keys: Option<&[&str]>,
    ) -> Result<PathBuf> {
        let crypto = self.crypto.read().await;
        if !crypto.has_passphrase() {
            anyhow::bail!("Secrets are locked. Provide passphrase to unlock.");
        }

        let registries = self.registries.read().await;
        let registry = registries
            .get(registry_name)
            .ok_or_else(|| anyhow::anyhow!("Registry not found: {}", registry_name))?;

        let mut exported: HashMap<String, serde_json::Value> = HashMap::new();

        let keys_to_export: Vec<&str> = if let Some(keys) = keys {
            keys.to_vec()
        } else {
            registry.secrets.keys().map(|s| s.as_str()).collect()
        };

        for key in keys_to_export {
            if let Some(secret) = registry.secrets.get(key) {
                let value = crypto
                    .decrypt(secret)
                    .map_err(|e| anyhow::anyhow!("Failed to decrypt {}: {}", key, e))?;

                // Include metadata in export
                let entry = if let Some(meta) = &secret.metadata {
                    serde_json::json!({
                        "value": value,
                        "type": meta.secret_type,
                        "expires_at": meta.expires_at,
                        "labels": meta.labels,
                    })
                } else {
                    serde_json::json!({
                        "value": value,
                    })
                };

                exported.insert(key.to_string(), entry);
            }
        }

        let export_path = workspace_path.join(".mcp-secrets.json");
        let content = serde_json::to_string_pretty(&exported)?;
        fs::write(&export_path, content).await?;

        Ok(export_path)
    }

    /// Import secrets from a JSON file.
    pub async fn import_from_json(&self, registry_name: &str, json_content: &str) -> Result<usize> {
        let secrets: HashMap<String, serde_json::Value> = serde_json::from_str(json_content)?;

        let mut count = 0;
        for (key, value) in secrets {
            let secret_value = if let Some(obj) = value.as_object() {
                obj.get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Invalid secret format for key: {}", key))?
                    .to_string()
            } else if let Some(s) = value.as_str() {
                s.to_string()
            } else {
                anyhow::bail!("Invalid secret format for key: {}", key);
            };

            self.set_secret(registry_name, &key, &secret_value, None)
                .await?;
            count += 1;
        }

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_secrets_store_lifecycle() {
        let temp = tempdir().unwrap();
        let store = SecretsStore::new(temp.path()).await.unwrap();

        // Initially not initialized
        assert!(!store.is_initialized().await);
        assert!(!store.can_decrypt().await);

        // Initialize
        let result = store.initialize("default").await.unwrap();
        assert_eq!(result.key_id, "default");

        // Now initialized but still can't decrypt (no passphrase)
        assert!(store.is_initialized().await);
        assert!(!store.can_decrypt().await);

        // Unlock with passphrase
        store.unlock("my-secret-passphrase").await.unwrap();
        assert!(store.can_decrypt().await);

        // Set a secret
        store
            .set_secret("test-registry", "api-key", "sk-12345", None)
            .await
            .unwrap();

        // Get the secret back
        let value = store.get_secret("test-registry", "api-key").await.unwrap();
        assert_eq!(value, "sk-12345");

        // List secrets
        let secrets = store.list_secrets("test-registry").await.unwrap();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].key, "api-key");

        // Delete secret
        store
            .delete_secret("test-registry", "api-key")
            .await
            .unwrap();
        let secrets = store.list_secrets("test-registry").await.unwrap();
        assert_eq!(secrets.len(), 0);
    }

    #[tokio::test]
    async fn test_wrong_passphrase_verification() {
        let temp = tempdir().unwrap();
        let store = SecretsStore::new(temp.path()).await.unwrap();

        // Initialize and set a secret with one passphrase
        store.initialize("default").await.unwrap();
        store.unlock("correct-passphrase").await.unwrap();
        store
            .set_secret("test", "key", "value", None)
            .await
            .unwrap();

        // Lock and try to unlock with wrong passphrase
        store.lock().await;

        // Create a new store instance to simulate fresh start
        let store2 = SecretsStore::new(temp.path()).await.unwrap();
        let result = store2.unlock("wrong-passphrase").await;
        assert!(result.is_err());
    }
}
