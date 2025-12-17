//! Complexity estimation agent.
//!
//! Analyzes a task description and estimates:
//! - Complexity score (0-1)
//! - Whether to split into subtasks
//! - Estimated token count
//!
//! ## Learning Integration
//! When memory is available, the estimator queries similar past tasks
//! and adjusts predictions based on historical actual token usage.

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType, Complexity, LeafAgent, LeafCapability,
};
use crate::llm::{ChatMessage, ChatOptions, Role};
use crate::memory::HistoricalContext;
use crate::task::Task;

/// Agent that estimates task complexity.
/// 
/// # Purpose
/// Given a task description, estimate how complex it is and whether
/// it should be split into subtasks.
/// 
/// # Algorithm
/// 1. Send task description to LLM with complexity evaluation prompt
/// 2. Parse LLM response for complexity score and reasoning
/// 3. Return structured Complexity object
pub struct ComplexityEstimator {
    id: AgentId,
    prompt_variant: ComplexityPromptVariant,
    split_threshold: f64,
    token_multiplier: f64,
}

/// Prompt variants for complexity estimation.
///
/// We keep this as an enum (not free-form strings) so we can:
/// - A/B test variants deterministically
/// - Store tuned choice as a stable symbol in config
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityPromptVariant {
    /// Short rubric-based prompt (fast).
    RubricV1,
    /// More explicit calibration prompt encouraging realistic token estimates.
    CalibratedV2,
}

impl ComplexityEstimator {
    /// Create a new complexity estimator.
    pub fn new() -> Self {
        Self {
            id: AgentId::new(),
            prompt_variant: ComplexityPromptVariant::CalibratedV2,
            split_threshold: 0.6,
            token_multiplier: 1.0,
        }
    }

    /// Create a custom estimator (used by calibration harness).
    pub fn with_params(
        prompt_variant: ComplexityPromptVariant,
        split_threshold: f64,
        token_multiplier: f64,
    ) -> Self {
        Self {
            id: AgentId::new(),
            prompt_variant,
            split_threshold: split_threshold.clamp(0.0, 1.0),
            token_multiplier: token_multiplier.max(0.1),
        }
    }

    /// Prompt template for complexity estimation.
    /// 
    /// # Response Format
    /// LLM should respond with JSON containing:
    /// - score: float 0-1
    /// - reasoning: string explanation
    fn build_prompt(&self, task: &Task) -> String {
        match self.prompt_variant {
            ComplexityPromptVariant::RubricV1 => format!(
                r#"You are a task complexity analyzer.

Task: {task}

Respond with ONLY a JSON object:
{{
  "score": <float 0..1>,
  "reasoning": <string>,
  "estimated_tokens": <int>
}}

Rubric for score:
- 0.0-0.2: Trivial
- 0.2-0.4: Simple
- 0.4-0.6: Moderate
- 0.6-0.8: Complex
- 0.8-1.0: Very Complex"#,
                task = task.description()
            ),
            ComplexityPromptVariant::CalibratedV2 => format!(
                r#"You are a task complexity analyzer. Your goal is to estimate:
1) a complexity score in [0, 1]
2) a realistic token budget estimate for completing the task end-to-end using an LLM with tools.

Task: {task}

Important: \"estimated_tokens\" should reflect TOTAL tokens (input + output) across multiple turns, including:
- planning / reasoning
- tool call arguments and tool outputs
- iterative fixes and retries

Respond with ONLY a JSON object:
{{
  \"score\": <float 0..1>,
  \"reasoning\": <string>,
  \"estimated_tokens\": <int>
}}

Rubric for score:
- 0.0-0.2: Trivial (single tool call)
- 0.2-0.4: Simple (1-3 tool calls)
- 0.4-0.6: Moderate (3-8 tool calls)
- 0.6-0.8: Complex (multi-file, tests, iterations)
- 0.8-1.0: Very Complex (architecture, significant refactor)"#,
                task = task.description()
            ),
        }
    }

    /// Parse LLM response into Complexity struct.
    /// 
    /// # Postconditions
    /// - Returns valid Complexity with score in [0, 1]
    /// - Falls back to moderate complexity on parse error
    fn parse_response(&self, response: &str) -> Complexity {
        // Try to parse as JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            let score = json["score"].as_f64().unwrap_or(0.5);
            let reasoning = json["reasoning"].as_str().unwrap_or("No reasoning provided");
            let estimated_tokens = json["estimated_tokens"].as_u64().unwrap_or(2000);
            
            return Complexity::new(score, reasoning, estimated_tokens);
        }

        // Try to extract score from text
        if let Some(score) = self.extract_score_from_text(response) {
            return Complexity::new(score, response, 2000);
        }

        // Default to moderate complexity
        Complexity::moderate("Could not parse complexity response")
    }

    /// Try to extract a score from free-form text.
    fn extract_score_from_text(&self, text: &str) -> Option<f64> {
        // Look for patterns like "0.5" or "score: 0.5" or "50%"
        let text_lower = text.to_lowercase();
        
        // Check for keywords
        if text_lower.contains("trivial") || text_lower.contains("very simple") {
            return Some(0.1);
        }
        if text_lower.contains("very complex") || text_lower.contains("extremely") {
            return Some(0.9);
        }
        if text_lower.contains("complex") {
            return Some(0.7);
        }
        if text_lower.contains("moderate") || text_lower.contains("medium") {
            return Some(0.5);
        }
        if text_lower.contains("simple") || text_lower.contains("easy") {
            return Some(0.3);
        }

        None
    }

    /// Query historical context for similar tasks.
    /// 
    /// Returns adjustment multipliers based on past actual vs predicted values.
    async fn get_historical_adjustments(
        &self,
        task_description: &str,
        ctx: &AgentContext,
    ) -> Option<HistoricalContext> {
        let memory = ctx.memory.as_ref()?;
        
        match memory.retriever.get_historical_context(task_description, 5).await {
            Ok(context) => {
                if let Some(ref hist) = context {
                    tracing::debug!(
                        "Historical context found: {} similar tasks, avg token ratio: {:.2}, success rate: {:.2}",
                        hist.similar_outcomes.len(),
                        hist.avg_token_multiplier,
                        hist.similar_success_rate
                    );
                }
                context
            }
            Err(e) => {
                tracing::warn!("Failed to fetch historical context: {}", e);
                None
            }
        }
    }

    /// Adjust token estimate based on historical data.
    /// 
    /// If similar past tasks consistently used more/fewer tokens than predicted,
    /// we adjust our estimate accordingly.
    fn apply_historical_adjustment(
        &self,
        base_tokens: u64,
        historical: Option<&HistoricalContext>,
    ) -> u64 {
        match historical {
            Some(hist) if hist.similar_outcomes.len() >= 2 => {
                // Apply the historical token multiplier (clamped to reasonable range)
                let multiplier = hist.avg_token_multiplier.clamp(0.5, 3.0);
                let adjusted = (base_tokens as f64 * multiplier).round() as u64;
                
                tracing::debug!(
                    "Adjusted token estimate: {} -> {} (multiplier: {:.2})",
                    base_tokens, adjusted, multiplier
                );
                
                adjusted
            }
            _ => base_tokens,
        }
    }
}

impl Default for ComplexityEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for ComplexityEstimator {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::ComplexityEstimator
    }

    fn description(&self) -> &str {
        "Estimates task complexity and recommends splitting strategy"
    }

    /// Estimate complexity of a task.
    /// 
    /// # Returns
    /// AgentResult with Complexity data in the `data` field.
    /// 
    /// # Learning Integration
    /// When memory is available, queries similar past tasks to adjust predictions
    /// based on actual historical token usage.
    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        // Query historical context for similar tasks (if memory available)
        let historical = self.get_historical_adjustments(task.description(), ctx).await;
        
        let prompt = self.build_prompt(task);
        
        let messages = vec![
            ChatMessage::new(Role::System, "You are a precise task complexity analyzer. Respond only with JSON."),
            ChatMessage::new(Role::User, prompt),
        ];

        // Use a fast, cheap model for complexity estimation
        let model = "openai/gpt-4.1-mini";
        
        let pricing = ctx.pricing.get_pricing(model).await;
        let options = ChatOptions {
            temperature: Some(0.0),
            top_p: None,
            max_tokens: Some(400),
        };

        match ctx
            .llm
            .chat_completion_with_options(model, &messages, None, options)
            .await
        {
            Ok(response) => {
                let content = response.content.unwrap_or_default();
                let parsed = self.parse_response(&content);

                // Apply calibrated adjustments (pure post-processing).
                let base_tokens = ((parsed.estimated_tokens() as f64) * self.token_multiplier)
                    .round()
                    .max(1.0) as u64;
                
                // Apply historical adjustment if we have relevant data
                let adjusted_tokens = self.apply_historical_adjustment(base_tokens, historical.as_ref());
                
                let should_split = parsed.score() > self.split_threshold;
                let complexity = Complexity::new(parsed.score(), parsed.reasoning(), adjusted_tokens)
                    .with_split(should_split);
                
                // Record analysis on the task
                {
                    let a = task.analysis_mut();
                    a.complexity_score = Some(complexity.score());
                    a.complexity_reasoning = Some(complexity.reasoning().to_string());
                    a.should_split = Some(complexity.should_split());
                    a.estimated_total_tokens = Some(complexity.estimated_tokens());
                }

                // Compute cost (if usage + pricing available)
                let cost_cents = match (&response.usage, &pricing) {
                    (Some(u), Some(p)) => p.calculate_cost_cents(u.prompt_tokens, u.completion_tokens),
                    _ => 1, // fallback tiny cost
                };
                
                // Build historical info for response data
                let historical_info = historical.as_ref().map(|h| json!({
                    "similar_tasks_found": h.similar_outcomes.len(),
                    "avg_token_multiplier": h.avg_token_multiplier,
                    "avg_cost_multiplier": h.avg_cost_multiplier,
                    "similar_success_rate": h.similar_success_rate,
                }));
                
                AgentResult::success(
                    format!(
                        "Complexity: {:.2} - {}{}",
                        complexity.score(),
                        if complexity.should_split() { "Should split" } else { "Execute directly" },
                        if historical.is_some() { " (adjusted from history)" } else { "" }
                    ),
                    cost_cents,
                )
                .with_model(model)
                .with_data(json!({
                    "score": complexity.score(),
                    "reasoning": complexity.reasoning(),
                    "should_split": complexity.should_split(),
                    "estimated_tokens": complexity.estimated_tokens(),
                    "base_tokens_before_history": base_tokens,
                    "historical_adjustment": historical_info,
                    "usage": response.usage.as_ref().map(|u| json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens
                    })),
                }))
            }
            Err(e) => {
                // On error, return moderate complexity as fallback
                let fallback = Complexity::moderate(format!("LLM error, using fallback: {}", e));
                
                AgentResult::success(
                    "Using fallback complexity estimate due to LLM error",
                    0,
                )
                .with_data(json!({
                    "score": fallback.score(),
                    "reasoning": fallback.reasoning(),
                    "should_split": fallback.should_split(),
                    "estimated_tokens": fallback.estimated_tokens(),
                    "fallback": true,
                }))
            }
        }
    }
}

impl LeafAgent for ComplexityEstimator {
    fn capability(&self) -> LeafCapability {
        LeafCapability::ComplexityEstimation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_response() {
        let estimator = ComplexityEstimator::new();
        
        let json_response = r#"{"score": 0.7, "reasoning": "Complex task", "estimated_tokens": 3000, "should_split": true}"#;
        let complexity = estimator.parse_response(json_response);
        
        assert!((complexity.score() - 0.7).abs() < 0.01);
        assert!(complexity.should_split());
    }

    #[test]
    fn test_parse_text_response() {
        let estimator = ComplexityEstimator::new();
        
        let text_response = "This is a very complex task";
        let complexity = estimator.parse_response(text_response);
        
        assert!(complexity.score() > 0.6);
    }
}

