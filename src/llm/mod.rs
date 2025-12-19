//! LLM client module for interacting with language models.
//!
//! This module provides a trait-based abstraction over LLM providers,
//! with OpenRouter as the primary implementation.
//!
//! Supports multimodal content (text + images) for vision-capable models.

mod error;
mod openrouter;

pub use error::{classify_http_status, LlmError, LlmErrorKind, RetryConfig};
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

/// Reasoning content block from "thinking" models (e.g., Gemini 3, Claude with extended thinking).
/// 
/// These blocks contain the model's internal reasoning and must be preserved in subsequent
/// requests when using tool calls. The `thought_signature` is an encrypted hash that allows
/// the model to resume its chain of thought.
/// 
/// For Gemini 3 via OpenRouter, the signature is in the `data` field of `reasoning_details`,
/// which corresponds to a tool call `id`. We copy this to `thought_signature` on the matching
/// tool call during post-processing.
/// 
/// Reference: https://openrouter.ai/docs/use-cases/reasoning-tokens
/// Reference: https://ai.google.dev/gemini-api/docs/thought-signatures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningContent {
    /// The reasoning/thinking content (may be redacted or empty for some models).
    /// OpenRouter uses both `content` and `text` fields depending on the model.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "text"
    )]
    pub content: Option<String>,
    
    /// Encrypted thought signature for resuming reasoning (required for Gemini 3).
    /// This MUST be preserved and sent back in subsequent requests for tool call continuations.
    /// Supports both snake_case and camelCase for compatibility with different API formats.
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
    
    /// Type of reasoning block (e.g., "thinking", "reasoning.text", "reasoning.encrypted")
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub reasoning_type: Option<String>,
    
    /// Format of the reasoning content (e.g., "unknown", "google-gemini-v1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    
    /// Index of the reasoning block (for ordered reasoning)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    
    /// ID matching a tool call (Gemini 3 format via OpenRouter)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    
    /// Encrypted reasoning data (Gemini 3 format) - this is the actual thought_signature
    /// that must be sent back with the matching tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl ReasoningContent {
    /// Check if this reasoning block has a thought signature that needs preservation.
    /// For Gemini 3, this is in the `data` field.
    pub fn needs_preservation(&self) -> bool {
        self.thought_signature.is_some() || self.data.is_some()
    }
    
    /// Get the effective thought signature (from thought_signature or data field).
    pub fn get_thought_signature(&self) -> Option<&str> {
        self.thought_signature.as_deref().or(self.data.as_deref())
    }
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
    /// Reasoning/thinking content for "thinking" models (Gemini 3, etc.).
    /// Must be preserved and sent back in subsequent requests when using tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Vec<ReasoningContent>>,
}

impl ChatMessage {
    /// Create a simple text message.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: Some(MessageContent::text(content)),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// Create a multimodal message with text and image.
    pub fn with_image(role: Role, text: impl Into<String>, image_url: impl Into<String>) -> Self {
        ChatMessage {
            role,
            content: Some(MessageContent::text_and_image(text, image_url)),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// Get the text content of this message.
    pub fn text_content(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.as_text())
    }

    /// Attach reasoning content to this message (for thinking models).
    /// 
    /// This should be called when preserving an assistant message that included
    /// reasoning blocks, so they can be sent back in subsequent requests.
    pub fn with_reasoning(mut self, reasoning: Vec<ReasoningContent>) -> Self {
        if !reasoning.is_empty() {
            self.reasoning = Some(reasoning);
        }
        self
    }

    /// Check if this message has reasoning content that needs preservation.
    pub fn has_reasoning(&self) -> bool {
        self.reasoning.as_ref().map_or(false, |r| {
            r.iter().any(|rc| rc.needs_preservation())
        })
    }
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
    /// Thought signature for Gemini 3 models (may be at this level instead of function level).
    /// Supports both snake_case and camelCase for compatibility with different API formats.
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
}

/// Function call details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments as a JSON string. May be empty or missing for no-argument functions.
    #[serde(default)]
    pub arguments: String,
    /// Thought signature for Gemini 3 models. Must be preserved and sent back with tool results.
    /// When present, this allows Gemini to resume its chain of thought after a tool call.
    /// Supports both snake_case and camelCase for compatibility with different API formats.
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
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
    /// Reasoning/thinking content from "thinking" models.
    /// Must be preserved and included in subsequent requests for tool call continuations.
    pub reasoning: Option<Vec<ReasoningContent>>,
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
