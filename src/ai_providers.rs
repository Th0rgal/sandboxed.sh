//! AI Provider configuration and storage.
//!
//! Manages inference providers that OpenCode can use (Anthropic, OpenAI, etc.).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Known AI provider types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAI,
    Google,
    AmazonBedrock,
    Azure,
    OpenRouter,
    Mistral,
    Groq,
    Xai,
    DeepInfra,
    Cerebras,
    Cohere,
    #[serde(rename = "together-ai")]
    TogetherAI,
    Perplexity,
    GithubCopilot,
    Custom,
}

impl ProviderType {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAI => "OpenAI",
            Self::Google => "Google AI",
            Self::AmazonBedrock => "Amazon Bedrock",
            Self::Azure => "Azure OpenAI",
            Self::OpenRouter => "OpenRouter",
            Self::Mistral => "Mistral AI",
            Self::Groq => "Groq",
            Self::Xai => "xAI",
            Self::DeepInfra => "DeepInfra",
            Self::Cerebras => "Cerebras",
            Self::Cohere => "Cohere",
            Self::TogetherAI => "Together AI",
            Self::Perplexity => "Perplexity",
            Self::GithubCopilot => "GitHub Copilot",
            Self::Custom => "Custom",
        }
    }

    pub fn env_var_name(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::OpenAI => Some("OPENAI_API_KEY"),
            Self::Google => Some("GOOGLE_API_KEY"),
            Self::AmazonBedrock => None, // Uses AWS credentials
            Self::Azure => Some("AZURE_OPENAI_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::Mistral => Some("MISTRAL_API_KEY"),
            Self::Groq => Some("GROQ_API_KEY"),
            Self::Xai => Some("XAI_API_KEY"),
            Self::DeepInfra => Some("DEEPINFRA_API_KEY"),
            Self::Cerebras => Some("CEREBRAS_API_KEY"),
            Self::Cohere => Some("COHERE_API_KEY"),
            Self::TogetherAI => Some("TOGETHER_API_KEY"),
            Self::Perplexity => Some("PERPLEXITY_API_KEY"),
            Self::GithubCopilot => None, // Uses OAuth
            Self::Custom => None,
        }
    }

    /// Returns whether this provider uses OAuth authentication.
    pub fn uses_oauth(&self) -> bool {
        matches!(self, Self::Anthropic | Self::GithubCopilot)
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// AI provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIProvider {
    pub id: Uuid,
    /// Provider type (anthropic, openai, etc.)
    pub provider_type: ProviderType,
    /// Human-readable name (e.g., "My Claude Account", "Work OpenAI")
    pub name: String,
    /// API key (if using API key auth)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Custom base URL (for self-hosted or proxy endpoints)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Whether this provider is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Whether this is the default provider
    #[serde(default)]
    pub is_default: bool,
    /// Connection status (populated at runtime)
    #[serde(skip)]
    pub status: ProviderStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn default_enabled() -> bool {
    true
}

/// Provider connection status.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    #[default]
    Unknown,
    Connected,
    NeedsAuth,
    Error(String),
}

impl AIProvider {
    pub fn new(provider_type: ProviderType, name: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            provider_type,
            name,
            api_key: None,
            base_url: None,
            enabled: true,
            is_default: false,
            status: ProviderStatus::Unknown,
            created_at: now,
            updated_at: now,
        }
    }

    /// Check if this provider has valid credentials configured.
    pub fn has_credentials(&self) -> bool {
        // OAuth providers are handled separately
        if self.provider_type.uses_oauth() {
            return true; // Will be validated at runtime
        }
        // API key providers need a key
        self.api_key.is_some()
    }
}

/// In-memory store for AI providers.
#[derive(Debug, Clone)]
pub struct AIProviderStore {
    providers: Arc<RwLock<HashMap<Uuid, AIProvider>>>,
    storage_path: PathBuf,
}

impl AIProviderStore {
    pub async fn new(storage_path: PathBuf) -> Self {
        let store = Self {
            providers: Arc::new(RwLock::new(HashMap::new())),
            storage_path,
        };

        // Load existing providers
        if let Ok(loaded) = store.load_from_disk() {
            let mut providers = store.providers.write().await;
            *providers = loaded;
        }

        store
    }

    /// Load providers from disk.
    fn load_from_disk(&self) -> Result<HashMap<Uuid, AIProvider>, std::io::Error> {
        if !self.storage_path.exists() {
            return Ok(HashMap::new());
        }

        let contents = std::fs::read_to_string(&self.storage_path)?;
        let providers: Vec<AIProvider> = serde_json::from_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(providers.into_iter().map(|p| (p.id, p)).collect())
    }

    /// Save providers to disk.
    async fn save_to_disk(&self) -> Result<(), std::io::Error> {
        let providers = self.providers.read().await;
        let providers_vec: Vec<&AIProvider> = providers.values().collect();

        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(&providers_vec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&self.storage_path, contents)?;
        Ok(())
    }

    pub async fn list(&self) -> Vec<AIProvider> {
        let providers = self.providers.read().await;
        let mut list: Vec<_> = providers.values().cloned().collect();
        // Sort by name
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub async fn get(&self, id: Uuid) -> Option<AIProvider> {
        let providers = self.providers.read().await;
        providers.get(&id).cloned()
    }

    /// Get the default provider (first enabled, or first overall).
    pub async fn get_default(&self) -> Option<AIProvider> {
        let providers = self.providers.read().await;
        // Find the one marked as default
        if let Some(provider) = providers.values().find(|p| p.is_default && p.enabled) {
            return Some(provider.clone());
        }
        // Fallback to first enabled
        providers.values().find(|p| p.enabled).cloned()
    }

    /// Get provider by type.
    pub async fn get_by_type(&self, provider_type: ProviderType) -> Option<AIProvider> {
        let providers = self.providers.read().await;
        providers
            .values()
            .find(|p| p.provider_type == provider_type && p.enabled)
            .cloned()
    }

    pub async fn add(&self, provider: AIProvider) -> Uuid {
        let id = provider.id;
        {
            let mut providers = self.providers.write().await;

            // If this is the first provider, make it default
            let is_first = providers.is_empty();
            let mut prov = provider;
            if is_first {
                prov.is_default = true;
            }

            providers.insert(id, prov);
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save AI providers to disk: {}", e);
        }

        id
    }

    pub async fn update(&self, id: Uuid, mut provider: AIProvider) -> Option<AIProvider> {
        provider.updated_at = chrono::Utc::now();

        {
            let mut providers = self.providers.write().await;
            if providers.contains_key(&id) {
                // If setting as default, unset others
                if provider.is_default {
                    for p in providers.values_mut() {
                        if p.id != id {
                            p.is_default = false;
                        }
                    }
                }
                providers.insert(id, provider.clone());
            } else {
                return None;
            }
        }

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save AI providers to disk: {}", e);
        }

        Some(provider)
    }

    pub async fn delete(&self, id: Uuid) -> bool {
        let existed = {
            let mut providers = self.providers.write().await;
            providers.remove(&id).is_some()
        };

        if existed {
            if let Err(e) = self.save_to_disk().await {
                tracing::error!("Failed to save AI providers to disk: {}", e);
            }
        }

        existed
    }

    /// Set a provider as the default.
    pub async fn set_default(&self, id: Uuid) -> bool {
        let mut providers = self.providers.write().await;

        if !providers.contains_key(&id) {
            return false;
        }

        for p in providers.values_mut() {
            p.is_default = p.id == id;
        }

        drop(providers);

        if let Err(e) = self.save_to_disk().await {
            tracing::error!("Failed to save AI providers to disk: {}", e);
        }

        true
    }
}

/// Shared store type.
pub type SharedAIProviderStore = Arc<AIProviderStore>;
