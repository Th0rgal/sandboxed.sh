//! Model selection agent with U-curve cost optimization.
//!
//! # U-Curve Optimization
//! The total expected cost follows a U-shaped curve:
//! - Cheap models: Low per-token cost, but may fail/retry, use more tokens
//! - Expensive models: High per-token cost, but succeed more often
//! - Optimal: Somewhere in the middle, minimizing total expected cost
//!
//! # Cost Model
//! Expected Cost = base_cost * (1 + failure_rate * retry_multiplier) * token_efficiency

use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    Agent, AgentContext, AgentId, AgentResult, AgentType, LeafAgent, LeafCapability,
};
use crate::budget::PricingInfo;
use crate::task::Task;

/// Agent that selects the optimal model for a task.
/// 
/// # Algorithm
/// 1. Get task complexity and budget constraints
/// 2. Fetch available models and pricing
/// 3. For each model, calculate expected total cost
/// 4. Return model with minimum expected cost within budget
pub struct ModelSelector {
    id: AgentId,
    retry_multiplier: f64,
    inefficiency_scale: f64,
    max_failure_probability: f64,
}

/// Model recommendation from the selector.
#[derive(Debug, Clone)]
pub struct ModelRecommendation {
    /// Recommended model ID
    pub model_id: String,
    
    /// Expected cost in cents
    pub expected_cost_cents: u64,
    
    /// Confidence in this recommendation (0-1)
    pub confidence: f64,
    
    /// Reasoning for the selection
    pub reasoning: String,
    
    /// Alternative models if primary fails
    pub fallbacks: Vec<String>,
}

impl ModelSelector {
    /// Create a new model selector.
    pub fn new() -> Self {
        Self {
            id: AgentId::new(),
            retry_multiplier: 1.5,
            inefficiency_scale: 0.5,
            max_failure_probability: 0.9,
        }
    }

    /// Create a selector with calibrated parameters.
    pub fn with_params(retry_multiplier: f64, inefficiency_scale: f64, max_failure_probability: f64) -> Self {
        Self {
            id: AgentId::new(),
            retry_multiplier: retry_multiplier.max(1.0),
            inefficiency_scale: inefficiency_scale.max(0.0),
            max_failure_probability: max_failure_probability.clamp(0.0, 0.99),
        }
    }

    /// Calculate expected cost for a model given task complexity.
    /// 
    /// # Formula
    /// ```text
    /// expected_cost = base_cost * (1 + failure_prob * retry_cost) * inefficiency_factor
    /// ```
    /// 
    /// # Parameters
    /// - `pricing`: Model pricing info
    /// - `complexity`: Task complexity (0-1)
    /// - `estimated_tokens`: Estimated tokens needed
    /// 
    /// # Returns
    /// Expected cost in cents
    /// 
    /// # Pure Function
    /// No side effects, deterministic output.
    fn calculate_expected_cost(
        &self,
        pricing: &PricingInfo,
        complexity: f64,
        estimated_tokens: u64,
    ) -> ExpectedCost {
        // Model capability estimate based on pricing tier
        // Higher price generally means more capable
        let avg_cost = pricing.average_cost_per_token();
        let capability = self.estimate_capability(avg_cost);
        
        // Failure probability: higher complexity + lower capability = more failures
        // Formula: P(fail) = complexity * (1 - capability)
        let failure_prob = (complexity * (1.0 - capability)).clamp(0.0, self.max_failure_probability);
        
        // Token inefficiency: weaker models need more tokens
        // Formula: inefficiency = 1 + (1 - capability) * 0.5
        let inefficiency = 1.0 + (1.0 - capability) * self.inefficiency_scale;
        
        // Retry cost: if it fails, we pay again (possibly with a better model)
        let retry_multiplier = self.retry_multiplier;
        
        // Base cost for estimated tokens
        let input_tokens = estimated_tokens / 2;
        let output_tokens = estimated_tokens / 2;
        let base_cost = pricing.calculate_cost_cents(input_tokens, output_tokens);
        
        // Adjusted for inefficiency (weak models use more tokens)
        let adjusted_tokens = ((estimated_tokens as f64) * inefficiency) as u64;
        let adjusted_cost = pricing.calculate_cost_cents(adjusted_tokens / 2, adjusted_tokens / 2);
        
        // Expected cost including retry probability
        let expected_cost = (adjusted_cost as f64) * (1.0 + failure_prob * retry_multiplier);
        
        ExpectedCost {
            model_id: pricing.model_id.clone(),
            base_cost_cents: base_cost,
            expected_cost_cents: expected_cost.ceil() as u64,
            failure_probability: failure_prob,
            capability,
            inefficiency,
        }
    }

    /// Estimate model capability from its cost.
    /// 
    /// # Heuristic
    /// More expensive models are generally more capable.
    /// Uses log scale to normalize across price ranges.
    /// 
    /// # Returns
    /// Capability score 0-1
    fn estimate_capability(&self, avg_cost_per_token: f64) -> f64 {
        // Cost tiers (per token):
        // < 0.0001: weak (capability ~0.3)
        // 0.0001-0.001: moderate (capability ~0.6)
        // > 0.001: strong (capability ~0.9)
        
        if avg_cost_per_token < 0.0000001 {
            return 0.3; // Free/very cheap
        }
        
        // Log scale normalization
        let log_cost = avg_cost_per_token.log10();
        // Map from ~-7 (cheap) to ~-3 (expensive) => 0.3 to 0.95
        let normalized = ((log_cost + 7.0) / 4.0).clamp(0.0, 1.0);
        
        0.3 + normalized * 0.65
    }

    /// Select optimal model from available options.
    /// 
    /// # Algorithm
    /// 1. Calculate expected cost for each model
    /// 2. Filter models exceeding budget
    /// 3. Select model with minimum expected cost
    /// 4. Include fallbacks in case of failure
    /// 
    /// # Preconditions
    /// - `models` is non-empty
    /// - `budget_cents > 0`
    fn select_optimal(
        &self,
        models: &[PricingInfo],
        complexity: f64,
        estimated_tokens: u64,
        budget_cents: u64,
    ) -> Option<ModelRecommendation> {
        if models.is_empty() {
            return None;
        }

        // Calculate expected cost for all models
        let mut costs: Vec<ExpectedCost> = models
            .iter()
            .map(|m| self.calculate_expected_cost(m, complexity, estimated_tokens))
            .collect();

        // Sort by expected cost (ascending)
        costs.sort_by(|a, b| {
            a.expected_cost_cents
                .cmp(&b.expected_cost_cents)
        });

        // Find cheapest model within budget
        let within_budget: Vec<_> = costs
            .iter()
            .filter(|c| c.expected_cost_cents <= budget_cents)
            .collect();

        let selected = within_budget.first().copied().or(costs.first())?;
        
        // Get fallback models (next best options)
        let fallbacks: Vec<String> = costs
            .iter()
            .filter(|c| c.model_id != selected.model_id)
            .take(2)
            .map(|c| c.model_id.clone())
            .collect();

        Some(ModelRecommendation {
            model_id: selected.model_id.clone(),
            expected_cost_cents: selected.expected_cost_cents,
            confidence: 1.0 - selected.failure_probability,
            reasoning: format!(
                "Selected {} with expected cost {} cents (capability: {:.2}, failure prob: {:.2})",
                selected.model_id,
                selected.expected_cost_cents,
                selected.capability,
                selected.failure_probability
            ),
            fallbacks,
        })
    }
}

/// Intermediate calculation result for a model.
#[derive(Debug)]
struct ExpectedCost {
    model_id: String,
    #[allow(dead_code)]
    base_cost_cents: u64,
    expected_cost_cents: u64,
    failure_probability: f64,
    capability: f64,
    #[allow(dead_code)]
    inefficiency: f64,
}

impl Default for ModelSelector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Agent for ModelSelector {
    fn id(&self) -> &AgentId {
        &self.id
    }

    fn agent_type(&self) -> AgentType {
        AgentType::ModelSelector
    }

    fn description(&self) -> &str {
        "Selects optimal model for task based on complexity and budget (U-curve optimization)"
    }

    /// Select the optimal model for a task.
    /// 
    /// # Expected Input
    /// Task should have complexity data in its context (from ComplexityEstimator).
    /// 
    /// # Returns
    /// AgentResult with ModelRecommendation in the `data` field.
    async fn execute(&self, task: &mut Task, ctx: &AgentContext) -> AgentResult {
        // Get complexity + estimated tokens from task analysis (populated by ComplexityEstimator).
        let complexity = task
            .analysis()
            .complexity_score
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        let estimated_tokens = task.analysis().estimated_total_tokens.unwrap_or(2000_u64);
        
        // Get available budget
        let budget_cents = task.budget().remaining_cents();
        
        // Fetch pricing for all models
        let models = ctx.pricing.models_by_cost().await;
        
        if models.is_empty() {
            // Use hardcoded defaults if no pricing available
            return AgentResult::success(
                "Using default model (no pricing data available)",
                0,
            )
            .with_data(json!({
                "model_id": "openai/gpt-4.1-mini",
                "expected_cost_cents": 10,
                "confidence": 0.5,
                "reasoning": "Fallback to default model",
                "fallbacks": ["openai/gpt-4o-mini", "anthropic/claude-3-haiku"],
            }));
        }

        match self.select_optimal(&models, complexity, estimated_tokens, budget_cents) {
            Some(rec) => {
                // Record selection in analysis
                {
                    let a = task.analysis_mut();
                    a.selected_model = Some(rec.model_id.clone());
                    a.estimated_cost_cents = Some(rec.expected_cost_cents);
                }

                AgentResult::success(
                    &rec.reasoning,
                    1, // Minimal cost for selection itself
                )
                .with_data(json!({
                    "model_id": rec.model_id,
                    "expected_cost_cents": rec.expected_cost_cents,
                    "confidence": rec.confidence,
                    "reasoning": rec.reasoning,
                    "fallbacks": rec.fallbacks,
                    "inputs": {
                        "complexity": complexity,
                        "estimated_tokens": estimated_tokens,
                        "budget_cents": budget_cents
                    }
                }))
            }
            None => {
                AgentResult::failure(
                    "No suitable model found within budget",
                    0,
                )
            }
        }
    }
}

impl LeafAgent for ModelSelector {
    fn capability(&self) -> LeafCapability {
        LeafCapability::ModelSelection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pricing(id: &str, prompt: f64, completion: f64) -> PricingInfo {
        PricingInfo {
            model_id: id.to_string(),
            prompt_cost_per_million: prompt,
            completion_cost_per_million: completion,
            context_length: 100000,
            max_output_tokens: None,
        }
    }

    #[test]
    fn test_expected_cost_u_curve() {
        let selector = ModelSelector::new();
        
        let cheap = make_pricing("cheap", 0.1, 0.2);
        let medium = make_pricing("medium", 1.0, 2.0);
        let expensive = make_pricing("expensive", 10.0, 20.0);
        
        let complexity = 0.7;
        let tokens = 2000;
        
        let cheap_cost = selector.calculate_expected_cost(&cheap, complexity, tokens);
        let medium_cost = selector.calculate_expected_cost(&medium, complexity, tokens);
        let expensive_cost = selector.calculate_expected_cost(&expensive, complexity, tokens);
        
        // For complex tasks, medium should be optimal (U-curve)
        // Cheap model has high failure rate
        // Expensive model has high base cost
        println!("Cheap: {} (fail: {})", cheap_cost.expected_cost_cents, cheap_cost.failure_probability);
        println!("Medium: {} (fail: {})", medium_cost.expected_cost_cents, medium_cost.failure_probability);
        println!("Expensive: {} (fail: {})", expensive_cost.expected_cost_cents, expensive_cost.failure_probability);
        
        // Basic sanity check: cheap model should have higher failure rate
        assert!(cheap_cost.failure_probability > medium_cost.failure_probability);
    }

    #[test]
    fn test_select_optimal() {
        let selector = ModelSelector::new();
        
        let models = vec![
            make_pricing("cheap", 0.1, 0.2),
            make_pricing("medium", 1.0, 2.0),
            make_pricing("expensive", 10.0, 20.0),
        ];
        
        // For moderate complexity, should pick cost-effective option
        let rec = selector.select_optimal(&models, 0.5, 1000, 1000);
        assert!(rec.is_some());
        
        // For very low budget, might be forced to pick cheap
        let rec_low = selector.select_optimal(&models, 0.5, 1000, 1);
        assert!(rec_low.is_some());
    }
}

