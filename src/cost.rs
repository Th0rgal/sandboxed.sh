//! Cost calculation from token usage and model pricing.
//!
//! This module provides a single source of truth for computing API costs
//! from token usage across all backends (Claude Code, Amp, OpenCode).

/// Model pricing in nanodollars per token (1 USD = 1_000_000_000 nanodollars).
/// Using nanodollars avoids floating-point rounding issues.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per input token in nanodollars
    pub input_nano_per_token: u64,
    /// Cost per output token in nanodollars
    pub output_nano_per_token: u64,
    /// Cost per cache creation input token (if different)
    pub cache_create_nano_per_token: Option<u64>,
    /// Cost per cache read input token (if different, usually much cheaper)
    pub cache_read_nano_per_token: Option<u64>,
}

/// Token usage from an API call.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

impl TokenUsage {
    /// Check if there's any usage to compute cost from.
    pub fn has_usage(&self) -> bool {
        self.input_tokens > 0 || self.output_tokens > 0
    }
}

/// Normalize model names to canonical form for pricing lookup.
fn normalize_model(model: &str) -> &str {
    let trimmed = model.trim();
    
    // Handle common aliases and versioned names
    match trimmed {
        // Claude models - normalize to base names
        s if s.contains("claude-3-5-sonnet") || s.contains("claude-3.5-sonnet") => "claude-3-5-sonnet",
        s if s.contains("claude-sonnet-4") || s.contains("claude-4-sonnet") => "claude-sonnet-4",
        s if s.contains("claude-3-5-haiku") || s.contains("claude-3.5-haiku") => "claude-3-5-haiku",
        s if s.contains("claude-3-opus") || s.contains("claude-3.0-opus") => "claude-3-opus",
        s if s.contains("claude-opus-4") || s.contains("claude-4-opus") => "claude-opus-4",
        
        // GPT models
        s if s.contains("gpt-4o-mini") => "gpt-4o-mini",
        s if s.contains("gpt-4o") => "gpt-4o",
        s if s.contains("gpt-4-turbo") => "gpt-4-turbo",
        s if s.contains("gpt-4") && !s.contains("gpt-4o") && !s.contains("turbo") => "gpt-4",
        s if s.contains("gpt-5") => "gpt-5",
        s if s.contains("o3") && !s.contains("gpt-4o") => "o3",
        s if s.contains("o4-mini") => "o4-mini",
        
        // Gemini models
        s if s.contains("gemini-2.5-pro") || s.contains("gemini-2-5-pro") => "gemini-2.5-pro",
        s if s.contains("gemini-2.5-flash") || s.contains("gemini-2-5-flash") => "gemini-2.5-flash",
        s if s.contains("gemini-2.0-flash") || s.contains("gemini-2-0-flash") => "gemini-2.0-flash",
        s if s.contains("gemini-1.5-pro") || s.contains("gemini-1-5-pro") => "gemini-1.5-pro",
        s if s.contains("gemini-1.5-flash") || s.contains("gemini-1-5-flash") => "gemini-1.5-flash",
        
        // Return as-is if no alias found
        _ => trimmed,
    }
}

/// Get pricing for a model. Returns None if model is unknown.
///
/// Prices are per 1M tokens converted to nanodollars per token:
/// - $3/1M input = 3_000 nanodollars per token
/// - $15/1M output = 15_000 nanodollars per token
pub fn pricing_for_model(model: &str) -> Option<ModelPricing> {
    let normalized = normalize_model(model);
    
    // Pricing as of January 2026 (in nanodollars per token)
    // Formula: $X per 1M tokens = X * 1000 nanodollars per token
    match normalized {
        // Claude 3.5 Sonnet: $3/1M input, $15/1M output
        "claude-3-5-sonnet" => Some(ModelPricing {
            input_nano_per_token: 3_000,
            output_nano_per_token: 15_000,
            cache_create_nano_per_token: Some(3_750),  // 25% more than input
            cache_read_nano_per_token: Some(300),      // 90% less than input
        }),
        
        // Claude Sonnet 4: $3/1M input, $15/1M output (same as 3.5)
        "claude-sonnet-4" => Some(ModelPricing {
            input_nano_per_token: 3_000,
            output_nano_per_token: 15_000,
            cache_create_nano_per_token: Some(3_750),
            cache_read_nano_per_token: Some(300),
        }),
        
        // Claude 3.5 Haiku: $0.80/1M input, $4/1M output
        "claude-3-5-haiku" => Some(ModelPricing {
            input_nano_per_token: 800,
            output_nano_per_token: 4_000,
            cache_create_nano_per_token: Some(1_000),
            cache_read_nano_per_token: Some(80),
        }),
        
        // Claude 3 Opus: $15/1M input, $75/1M output
        "claude-3-opus" => Some(ModelPricing {
            input_nano_per_token: 15_000,
            output_nano_per_token: 75_000,
            cache_create_nano_per_token: Some(18_750),
            cache_read_nano_per_token: Some(1_500),
        }),
        
        // Claude Opus 4: $15/1M input, $75/1M output
        "claude-opus-4" => Some(ModelPricing {
            input_nano_per_token: 15_000,
            output_nano_per_token: 75_000,
            cache_create_nano_per_token: Some(18_750),
            cache_read_nano_per_token: Some(1_500),
        }),
        
        // GPT-4o: $2.50/1M input, $10/1M output
        "gpt-4o" => Some(ModelPricing {
            input_nano_per_token: 2_500,
            output_nano_per_token: 10_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: Some(1_250), // 50% discount for cached
        }),
        
        // GPT-4o-mini: $0.15/1M input, $0.60/1M output
        "gpt-4o-mini" => Some(ModelPricing {
            input_nano_per_token: 150,
            output_nano_per_token: 600,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: Some(75),
        }),
        
        // GPT-4 Turbo: $10/1M input, $30/1M output
        "gpt-4-turbo" => Some(ModelPricing {
            input_nano_per_token: 10_000,
            output_nano_per_token: 30_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // GPT-4: $30/1M input, $60/1M output
        "gpt-4" => Some(ModelPricing {
            input_nano_per_token: 30_000,
            output_nano_per_token: 60_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // GPT-5 / GPT-5.2: estimated $5/1M input, $15/1M output
        "gpt-5" => Some(ModelPricing {
            input_nano_per_token: 5_000,
            output_nano_per_token: 15_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: Some(2_500),
        }),
        
        // o3: $10/1M input, $40/1M output (reasoning model)
        "o3" => Some(ModelPricing {
            input_nano_per_token: 10_000,
            output_nano_per_token: 40_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: Some(5_000),
        }),
        
        // o4-mini: $1.10/1M input, $4.40/1M output
        "o4-mini" => Some(ModelPricing {
            input_nano_per_token: 1_100,
            output_nano_per_token: 4_400,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: Some(550),
        }),
        
        // Gemini 2.5 Pro: $1.25/1M input, $10/1M output (>200k context)
        "gemini-2.5-pro" => Some(ModelPricing {
            input_nano_per_token: 1_250,
            output_nano_per_token: 10_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // Gemini 2.5 Flash: $0.15/1M input, $0.60/1M output
        "gemini-2.5-flash" => Some(ModelPricing {
            input_nano_per_token: 150,
            output_nano_per_token: 600,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // Gemini 2.0 Flash: $0.10/1M input, $0.40/1M output
        "gemini-2.0-flash" => Some(ModelPricing {
            input_nano_per_token: 100,
            output_nano_per_token: 400,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // Gemini 1.5 Pro: $1.25/1M input, $5/1M output
        "gemini-1.5-pro" => Some(ModelPricing {
            input_nano_per_token: 1_250,
            output_nano_per_token: 5_000,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // Gemini 1.5 Flash: $0.075/1M input, $0.30/1M output
        "gemini-1.5-flash" => Some(ModelPricing {
            input_nano_per_token: 75,
            output_nano_per_token: 300,
            cache_create_nano_per_token: None,
            cache_read_nano_per_token: None,
        }),
        
        // Unknown model
        _ => None,
    }
}

/// Calculate cost in cents from token usage and model.
///
/// Returns 0 if:
/// - Model is unknown (logs a warning once per unknown model)
/// - No token usage provided
pub fn cost_cents_from_usage(model: &str, usage: &TokenUsage) -> u64 {
    if !usage.has_usage() {
        return 0;
    }
    
    let Some(pricing) = pricing_for_model(model) else {
        // Log warning for unknown models (in production, consider rate-limiting this)
        tracing::warn!(model = %model, "Unknown model for cost calculation, using 0 cost");
        return 0;
    };
    
    // Calculate cost in nanodollars
    let mut cost_nano: u64 = 0;
    
    // Regular input tokens
    let regular_input = usage.input_tokens.saturating_sub(
        usage.cache_creation_input_tokens.unwrap_or(0) + 
        usage.cache_read_input_tokens.unwrap_or(0)
    );
    cost_nano += regular_input.saturating_mul(pricing.input_nano_per_token);
    
    // Output tokens
    cost_nano += usage.output_tokens.saturating_mul(pricing.output_nano_per_token);
    
    // Cache creation tokens (usually more expensive)
    if let Some(cache_create) = usage.cache_creation_input_tokens {
        let rate = pricing.cache_create_nano_per_token.unwrap_or(pricing.input_nano_per_token);
        cost_nano += cache_create.saturating_mul(rate);
    }
    
    // Cache read tokens (usually much cheaper)
    if let Some(cache_read) = usage.cache_read_input_tokens {
        let rate = pricing.cache_read_nano_per_token.unwrap_or(pricing.input_nano_per_token);
        cost_nano += cache_read.saturating_mul(rate);
    }
    
    // Convert nanodollars to cents: 1 cent = $0.01 = 10_000_000 nanodollars
    // Round to nearest cent
    (cost_nano + 5_000_000) / 10_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_normalize_model() {
        assert_eq!(normalize_model("claude-3-5-sonnet-20241022"), "claude-3-5-sonnet");
        assert_eq!(normalize_model("claude-3.5-sonnet-latest"), "claude-3-5-sonnet");
        assert_eq!(normalize_model("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(normalize_model("gemini-2.5-pro-preview"), "gemini-2.5-pro");
    }
    
    #[test]
    fn test_pricing_for_known_models() {
        assert!(pricing_for_model("claude-3-5-sonnet").is_some());
        assert!(pricing_for_model("gpt-4o").is_some());
        assert!(pricing_for_model("gemini-2.5-pro").is_some());
    }
    
    #[test]
    fn test_pricing_for_unknown_model() {
        assert!(pricing_for_model("unknown-model-xyz").is_none());
    }
    
    #[test]
    fn test_cost_calculation_basic() {
        // Claude 3.5 Sonnet: $3/1M input, $15/1M output
        // 1000 input + 500 output tokens
        // Cost = (1000 * 3000 + 500 * 15000) / 10_000_000 = (3_000_000 + 7_500_000) / 10_000_000 = 1.05 cents
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        assert_eq!(cost, 1); // Rounds to 1 cent
    }
    
    #[test]
    fn test_cost_calculation_with_cache() {
        // Claude 3.5 Sonnet with cache
        // 5000 cache read tokens at $0.30/1M = 1500 nanodollars
        // 1000 output tokens at $15/1M = 15_000_000 nanodollars
        let usage = TokenUsage {
            input_tokens: 5000, // These will be treated as cache read
            output_tokens: 1000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(5000),
        };
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        // (0 * 3000 + 1000 * 15000 + 5000 * 300) / 10_000_000 = (15_000_000 + 1_500_000) / 10_000_000 = 1.65 cents
        assert_eq!(cost, 2); // Rounds to 2 cents
    }
    
    #[test]
    fn test_cost_calculation_large_usage() {
        // Test with larger token counts (100k input, 10k output)
        // Claude 3.5 Sonnet: $3/1M input, $15/1M output
        // Cost = (100000 * 3000 + 10000 * 15000) / 10_000_000 = (300_000_000 + 150_000_000) / 10_000_000 = 45 cents
        let usage = TokenUsage {
            input_tokens: 100_000,
            output_tokens: 10_000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        assert_eq!(cost, 45);
    }
    
    #[test]
    fn test_cost_zero_for_no_usage() {
        let usage = TokenUsage::default();
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        assert_eq!(cost, 0);
    }
    
    #[test]
    fn test_cost_zero_for_unknown_model() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let cost = cost_cents_from_usage("completely-unknown-model", &usage);
        assert_eq!(cost, 0);
    }
}
