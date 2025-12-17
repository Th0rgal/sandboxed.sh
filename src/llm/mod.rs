//! LLM client module for interacting with language models.
//!
//! This module provides a trait-based abstraction over LLM providers,
//! with OpenRouter as the primary implementation.
//!
//! Supports multimodal content (text + images) for vision-capable models.

mod error;
mod openrouter;

pub use error::{LlmError, LlmErrorKind, RetryConfig, classify_http_status};
pub use openrouter::OpenRouterClient;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Role in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Content part for multimodal messages (text or image).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Text content
    Text { text: String },
    /// Image URL content (for vision models)
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

/// Image URL wrapper for vision content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    /// Optional detail level: "auto", "low", or "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ContentPart {
    /// Create a text content part.
    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }

    /// Create an image URL content part.
    pub fn image_url(url: impl Into<String>) -> Self {
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: url.into(),
                detail: None,
            },
        }
    }

    /// Create an image URL content part with detail level.
    pub fn image_url_with_detail(url: impl Into<String>, detail: impl Into<String>) -> Self {
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: url.into(),
                detail: Some(detail.into()),
            },
        }
    }
}

/// Message content - either simple text or multimodal (text + images).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Simple text content (most common case)
    Text(String),
    /// Multimodal content array (for vision models)
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Create simple text content.
    pub fn text(text: impl Into<String>) -> Self {
        MessageContent::Text(text.into())
    }

    /// Create multimodal content with text and images.
    pub fn multimodal(parts: Vec<ContentPart>) -> Self {
        MessageContent::Parts(parts)
    }

    /// Create content with text and a single image URL.
    pub fn text_and_image(text: impl Into<String>, image_url: impl Into<String>) -> Self {
        MessageContent::Parts(vec![
            ContentPart::text(text),
            ContentPart::image_url(image_url),
        ])
    }

    /// Get the text content (first text part if multimodal).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(s) => Some(s),
            MessageContent::Parts(parts) => parts.iter().find_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
        }
    }
}

/// A message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// Create a simple text message.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: Some(MessageContent::text(content)),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create a multimodal message with text and image.
    pub fn with_image(role: Role, text: impl Into<String>, image_url: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: Some(MessageContent::text_and_image(text, image_url)),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Get the text content of this message.
    pub fn text_content(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.as_text())
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments as a JSON string. May be empty or missing for no-argument functions.
    #[serde(default)]
    pub arguments: String,
}

/// Tool definition for the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

/// Function definition with schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Response from a chat completion.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
    pub model: Option<String>,
}

/// Token usage information (if provided by the upstream provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Create a usage object ensuring `total_tokens` is consistent.
    pub fn new(prompt_tokens: u64, completion_tokens: u64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

/// Optional parameters for chat completions.
///
/// These are intentionally conservative; the goal is reproducibility.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Sampling temperature (0 = deterministic).
    pub temperature: Option<f64>,
    /// Top-p nucleus sampling.
    pub top_p: Option<f64>,
    /// Maximum output tokens to generate.
    pub max_tokens: Option<u64>,
}

/// Trait for LLM clients.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request.
    async fn chat_completion(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
    ) -> anyhow::Result<ChatResponse>;

    /// Send a chat completion request with optional parameters.
    ///
    /// Default implementation ignores options and calls `chat_completion`.
    async fn chat_completion_with_options(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        _options: ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_completion(model, messages, tools).await
    }
}

