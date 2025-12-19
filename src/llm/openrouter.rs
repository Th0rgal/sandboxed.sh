//! OpenRouter API client implementation with automatic retry for transient errors.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use super::error::{classify_http_status, LlmError, LlmErrorKind, RetryConfig};
use super::{
    ChatMessage, ChatOptions, ChatResponse, LlmClient, ReasoningContent, TokenUsage, ToolCall,
    ToolDefinition,
};

const OPENROUTER_API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// OpenRouter API client with automatic retry for transient errors.
pub struct OpenRouterClient {
    client: Client,
    api_key: String,
    retry_config: RetryConfig,
}

impl OpenRouterClient {
    /// Create a new OpenRouter client with default retry configuration.
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            retry_config: RetryConfig::default(),
        }
    }

    /// Create a new OpenRouter client with custom retry configuration.
    pub fn with_retry_config(api_key: String, retry_config: RetryConfig) -> Self {
        Self {
            client: Client::new(),
            api_key,
            retry_config,
        }
    }

    /// Parse Retry-After header if present.
    fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
        headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                // Try parsing as seconds first
                s.parse::<u64>().ok().map(Duration::from_secs)
            })
    }

    /// Create an LlmError from HTTP response status and body.
    fn create_error(
        status: reqwest::StatusCode,
        body: &str,
        retry_after: Option<Duration>,
    ) -> LlmError {
        let status_code = status.as_u16();
        let kind = classify_http_status(status_code);

        match kind {
            LlmErrorKind::RateLimited => LlmError::rate_limited(body.to_string(), retry_after),
            LlmErrorKind::ServerError => LlmError::server_error(status_code, body.to_string()),
            LlmErrorKind::ClientError => LlmError::client_error(status_code, body.to_string()),
            _ => LlmError::server_error(status_code, body.to_string()),
        }
    }

    /// Execute a single request without retry.
    async fn execute_request(&self, request: &OpenRouterRequest) -> Result<ChatResponse, LlmError> {
        let response = match self
            .client
            .post(OPENROUTER_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/open-agent")
            .header("X-Title", "Open Agent")
            .json(request)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Network or connection error
                if e.is_timeout() {
                    return Err(LlmError::network_error(format!("Request timeout: {}", e)));
                } else if e.is_connect() {
                    return Err(LlmError::network_error(format!("Connection failed: {}", e)));
                } else {
                    return Err(LlmError::network_error(format!("Request failed: {}", e)));
                }
            }
        };

        let status = response.status();
        let retry_after = Self::parse_retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(Self::create_error(status, &body, retry_after));
        }

        // Debug: Log raw response for Gemini models to see thought_signature format
        if request.model.contains("gemini") {
            tracing::info!("Gemini raw response body (first 2000 chars): {}", &body[..body.len().min(2000)]);
        }

        let parsed: OpenRouterResponse = serde_json::from_str(&body).map_err(|e| {
            LlmError::parse_error(format!("Failed to parse response: {}, body: {}", e, body))
        })?;

        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::parse_error("No choices in response".to_string()))?;

        // Get reasoning using the flexible parser that handles both string and array formats
        let reasoning = choice.message.get_reasoning();
        
        // Log if we received reasoning blocks (for debugging thinking models)
        if let Some(ref reasoning_blocks) = reasoning {
            let has_thought_sig = reasoning_blocks.iter().any(|r| r.thought_signature.is_some());
            tracing::debug!(
                "Received {} reasoning blocks from model (has_thought_signature: {})",
                reasoning_blocks.len(),
                has_thought_sig
            );
            // Log thought_signature details for debugging Gemini issues
            for (i, r) in reasoning_blocks.iter().enumerate() {
                if r.thought_signature.is_some() {
                    tracing::debug!("Reasoning block {} has thought_signature", i);
                }
            }
        }
        
        // Log if tool calls have thought_signatures attached (Gemini format)
        if let Some(ref tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                // Check both ToolCall level and FunctionCall level
                let has_tc_sig = tc.thought_signature.is_some();
                let has_fn_sig = tc.function.thought_signature.is_some();
                // Always log tool calls for Gemini debugging
                tracing::info!(
                    "Tool call '{}' thought_signature status: tool_call_level={}, function_level={}",
                    tc.function.name,
                    has_tc_sig,
                    has_fn_sig
                );
            }
        }

        Ok(ChatResponse {
            content: choice.message.content,
            tool_calls: choice.message.tool_calls,
            finish_reason: choice.finish_reason,
            usage: parsed
                .usage
                .map(|u| TokenUsage::new(u.prompt_tokens, u.completion_tokens)),
            model: parsed.model.or_else(|| Some(request.model.clone())),
            reasoning,
        })
    }

    /// Execute a request with automatic retry for transient errors.
    async fn execute_with_retry(
        &self,
        request: &OpenRouterRequest,
    ) -> anyhow::Result<ChatResponse> {
        let start = Instant::now();
        let mut attempt = 0;
        let mut last_error: Option<LlmError> = None;

        loop {
            // Check if we've exceeded max retry duration
            if start.elapsed() > self.retry_config.max_retry_duration {
                let err = last_error.unwrap_or_else(|| {
                    LlmError::network_error("Max retry duration exceeded".to_string())
                });
                return Err(anyhow::anyhow!("{}", err));
            }

            match self.execute_request(request).await {
                Ok(response) => {
                    if attempt > 0 {
                        tracing::info!(
                            "Request succeeded after {} retries (total time: {:?})",
                            attempt,
                            start.elapsed()
                        );
                    }
                    return Ok(response);
                }
                Err(error) => {
                    let should_retry = self.retry_config.should_retry(&error)
                        && attempt < self.retry_config.max_retries;

                    if should_retry {
                        let delay = error.suggested_delay(attempt);

                        // Make sure we won't exceed max retry duration
                        let remaining = self
                            .retry_config
                            .max_retry_duration
                            .saturating_sub(start.elapsed());
                        let actual_delay = delay.min(remaining);

                        if actual_delay.is_zero() {
                            tracing::warn!(
                                "Retry attempt {} failed, no time remaining: {}",
                                attempt + 1,
                                error
                            );
                            return Err(anyhow::anyhow!("{}", error));
                        }

                        tracing::warn!(
                            "Retry attempt {} failed with {}, retrying in {:?}: {}",
                            attempt + 1,
                            error.kind,
                            actual_delay,
                            error.message
                        );

                        tokio::time::sleep(actual_delay).await;
                        attempt += 1;
                        last_error = Some(error);
                    } else {
                        // Non-retryable error or max retries exceeded
                        if attempt > 0 {
                            tracing::error!(
                                "Request failed after {} retries (total time: {:?}): {}",
                                attempt,
                                start.elapsed(),
                                error
                            );
                        } else {
                            tracing::error!("Request failed (non-retryable): {}", error);
                        }
                        return Err(anyhow::anyhow!("{}", error));
                    }
                }
            }
        }
    }
}

#[async_trait]
impl LlmClient for OpenRouterClient {
    async fn chat_completion(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
    ) -> anyhow::Result<ChatResponse> {
        self.chat_completion_with_options(model, messages, tools, ChatOptions::default())
            .await
    }

    async fn chat_completion_with_options(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: Option<&[ToolDefinition]>,
        options: ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let request = OpenRouterRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            tools: tools.map(|t| t.to_vec()),
            tool_choice: tools.map(|_| "auto".to_string()),
            temperature: options.temperature,
            top_p: options.top_p,
            max_tokens: options.max_tokens,
        };

        tracing::debug!("Sending request to OpenRouter: model={}", model);

        self.execute_with_retry(&request).await
    }
}

/// OpenRouter API request format.
#[derive(Debug, Serialize)]
struct OpenRouterRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
}

/// OpenRouter API response format.
#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    choices: Vec<OpenRouterChoice>,
    #[serde(default)]
    usage: Option<OpenRouterUsage>,
    #[serde(default)]
    model: Option<String>,
}

/// A choice in the OpenRouter response.
#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
    message: OpenRouterMessage,
    finish_reason: Option<String>,
}

/// Message in OpenRouter response.
#[derive(Debug, Deserialize)]
struct OpenRouterMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
    /// Reasoning text as a plain string (some models like Kimi return this).
    /// We'll merge this with reasoning_details if both are present.
    #[serde(default)]
    reasoning: Option<serde_json::Value>,
    /// Reasoning blocks from "thinking" models (Gemini 3, Kimi, etc.)
    /// Contains thought_signature that must be preserved for tool call continuations.
    #[serde(default)]
    reasoning_details: Option<Vec<ReasoningContent>>,
}

impl OpenRouterMessage {
    /// Get reasoning content, handling both string and array formats.
    /// Some models (Kimi) return `reasoning` as a string AND `reasoning_details` as an array.
    /// Other models (Gemini) may put thought_signature in the array.
    fn get_reasoning(&self) -> Option<Vec<ReasoningContent>> {
        // Prefer reasoning_details if available (it's the structured format)
        if let Some(ref details) = self.reasoning_details {
            if !details.is_empty() {
                return Some(details.clone());
            }
        }
        
        // Fall back to reasoning field - could be string or array
        if let Some(ref reasoning) = self.reasoning {
            match reasoning {
                serde_json::Value::String(s) => {
                    // Single string reasoning - convert to ReasoningContent
                    return Some(vec![ReasoningContent {
                        content: Some(s.clone()),
                        thought_signature: None,
                        reasoning_type: Some("thinking".to_string()),
                        format: None,
                        index: None,
                    }]);
                }
                serde_json::Value::Array(arr) => {
                    // Array of reasoning blocks - try to parse
                    if let Ok(blocks) = serde_json::from_value::<Vec<ReasoningContent>>(
                        serde_json::Value::Array(arr.clone())
                    ) {
                        return Some(blocks);
                    }
                }
                _ => {}
            }
        }
        
        None
    }
}

/// Usage data (OpenAI-compatible).
#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(rename = "total_tokens")]
    _total_tokens: u64,
}
