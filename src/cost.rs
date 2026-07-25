//! Cost calculation from token usage and model pricing.
//!
//! This module provides a single source of truth for computing API costs
//! from token usage across all backends (Claude Code, OpenCode).

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

struct PricingEntry {
    canonical: &'static str,
    aliases: &'static [&'static str],
    pricing: ModelPricing,
}

const fn pricing(
    input_nano_per_token: u64,
    output_nano_per_token: u64,
    cache_create_nano_per_token: Option<u64>,
    cache_read_nano_per_token: Option<u64>,
) -> ModelPricing {
    ModelPricing {
        input_nano_per_token,
        output_nano_per_token,
        cache_create_nano_per_token,
        cache_read_nano_per_token,
    }
}

// Prices are nanodollars per token. Provider rates are published per 1M tokens,
// so "$1.25 / 1M tokens" becomes 1_250 nanodollars per token.
//
// Sources checked July 9, 2026:
// - OpenAI API pricing and model pages: https://developers.openai.com/api/docs/pricing
// - xAI model pricing: https://docs.x.ai/developers/pricing
// - Z.AI pricing: https://docs.z.ai/guides/overview/pricing
// - MiniMax pay-as-you-go pricing: https://platform.minimax.io/docs/guides/pricing-paygo
const PRICING_ENTRIES: &[PricingEntry] = &[
    PricingEntry {
        canonical: "claude-3-5-sonnet",
        aliases: &["claude-3-5-sonnet", "claude-3.5-sonnet"],
        pricing: pricing(3_000, 15_000, Some(3_750), Some(300)),
    },
    PricingEntry {
        canonical: "claude-haiku-4-5",
        aliases: &["claude-haiku-4-5", "claude-4-5-haiku"],
        pricing: pricing(1_000, 5_000, Some(1_250), Some(100)),
    },
    PricingEntry {
        canonical: "claude-sonnet-4-6",
        aliases: &["claude-sonnet-4-6", "claude-4-6-sonnet"],
        pricing: pricing(3_000, 15_000, Some(3_750), Some(300)),
    },
    PricingEntry {
        canonical: "claude-sonnet-4-5",
        aliases: &["claude-sonnet-4-5", "claude-4-5-sonnet"],
        pricing: pricing(3_000, 15_000, Some(3_750), Some(300)),
    },
    PricingEntry {
        canonical: "claude-sonnet-5",
        aliases: &["claude-sonnet-5", "claude-5-sonnet"],
        pricing: pricing(3_000, 15_000, Some(3_750), Some(300)),
    },
    PricingEntry {
        canonical: "claude-sonnet-4",
        aliases: &["claude-sonnet-4", "claude-4-sonnet"],
        pricing: pricing(3_000, 15_000, Some(3_750), Some(300)),
    },
    PricingEntry {
        canonical: "claude-3-5-haiku",
        aliases: &["claude-3-5-haiku", "claude-3.5-haiku"],
        pricing: pricing(800, 4_000, Some(1_000), Some(80)),
    },
    PricingEntry {
        canonical: "claude-3-opus",
        aliases: &["claude-3-opus", "claude-3.0-opus"],
        pricing: pricing(15_000, 75_000, Some(18_750), Some(1_500)),
    },
    PricingEntry {
        canonical: "claude-fable-5",
        aliases: &["claude-fable-5"],
        pricing: pricing(10_000, 50_000, Some(12_500), Some(1_000)),
    },
    PricingEntry {
        canonical: "claude-opus-5",
        aliases: &["claude-opus-5", "claude-5-opus"],
        pricing: pricing(5_000, 25_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "claude-opus-4-8",
        aliases: &["claude-opus-4-8", "claude-4-8-opus"],
        pricing: pricing(5_000, 25_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "claude-opus-4-7",
        aliases: &["claude-opus-4-7", "claude-4-7-opus"],
        pricing: pricing(5_000, 25_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "claude-opus-4-6",
        aliases: &["claude-opus-4-6", "claude-4-6-opus"],
        pricing: pricing(5_000, 25_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "claude-opus-4-5",
        aliases: &["claude-opus-4-5", "claude-4-5-opus"],
        pricing: pricing(5_000, 25_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "claude-opus-4",
        aliases: &["claude-opus-4", "claude-4-opus"],
        pricing: pricing(15_000, 75_000, Some(18_750), Some(1_500)),
    },
    PricingEntry {
        canonical: "gpt-4o-mini",
        aliases: &["gpt-4o-mini"],
        pricing: pricing(150, 600, None, Some(75)),
    },
    PricingEntry {
        canonical: "gpt-4o",
        aliases: &["gpt-4o"],
        pricing: pricing(2_500, 10_000, None, Some(1_250)),
    },
    PricingEntry {
        canonical: "gpt-4-turbo",
        aliases: &["gpt-4-turbo"],
        pricing: pricing(10_000, 30_000, None, None),
    },
    PricingEntry {
        canonical: "gpt-4",
        aliases: &["gpt-4"],
        pricing: pricing(30_000, 60_000, None, None),
    },
    PricingEntry {
        canonical: "gpt-5.4-nano",
        aliases: &["gpt-5.4-nano", "gpt-5-4-nano"],
        pricing: pricing(200, 1_250, None, Some(20)),
    },
    PricingEntry {
        canonical: "gpt-5-nano",
        aliases: &["gpt-5-nano"],
        pricing: pricing(50, 400, None, Some(5)),
    },
    PricingEntry {
        canonical: "gpt-5.4-mini",
        aliases: &["gpt-5.4-mini", "gpt-5-4-mini"],
        pricing: pricing(750, 4_500, None, Some(75)),
    },
    PricingEntry {
        canonical: "gpt-5-mini",
        aliases: &["gpt-5-mini"],
        pricing: pricing(250, 2_000, None, Some(25)),
    },
    PricingEntry {
        canonical: "gpt-5.6-sol",
        aliases: &["gpt-5.6", "gpt-5-6", "gpt-5.6-sol", "gpt-5-6-sol"],
        pricing: pricing(5_000, 30_000, Some(6_250), Some(500)),
    },
    PricingEntry {
        canonical: "gpt-5.6-terra",
        aliases: &["gpt-5.6-terra", "gpt-5-6-terra"],
        pricing: pricing(2_500, 15_000, Some(3_125), Some(250)),
    },
    PricingEntry {
        canonical: "gpt-5.6-luna",
        aliases: &["gpt-5.6-luna", "gpt-5-6-luna"],
        pricing: pricing(1_000, 6_000, Some(1_250), Some(100)),
    },
    PricingEntry {
        canonical: "gpt-5.5-pro",
        aliases: &["gpt-5.5-pro", "gpt-5-5-pro"],
        pricing: pricing(30_000, 180_000, None, None),
    },
    PricingEntry {
        canonical: "gpt-5.5",
        aliases: &["gpt-5.5", "gpt-5-5"],
        pricing: pricing(5_000, 30_000, None, Some(500)),
    },
    PricingEntry {
        canonical: "gpt-5.4-pro",
        aliases: &["gpt-5.4-pro", "gpt-5-4-pro"],
        pricing: pricing(30_000, 180_000, None, None),
    },
    PricingEntry {
        canonical: "gpt-5.4",
        aliases: &["gpt-5.4", "gpt-5-4"],
        pricing: pricing(2_500, 15_000, None, Some(250)),
    },
    PricingEntry {
        canonical: "gpt-5.3",
        aliases: &["gpt-5.3", "gpt-5-3"],
        pricing: pricing(1_750, 14_000, None, Some(175)),
    },
    PricingEntry {
        canonical: "gpt-5.2",
        aliases: &["gpt-5.2", "gpt-5-2"],
        pricing: pricing(1_750, 14_000, None, Some(175)),
    },
    PricingEntry {
        canonical: "gpt-5",
        aliases: &["gpt-5.1", "gpt-5-1", "gpt-5"],
        pricing: pricing(1_250, 10_000, None, Some(125)),
    },
    PricingEntry {
        canonical: "o3",
        aliases: &["o3"],
        pricing: pricing(2_000, 8_000, None, Some(500)),
    },
    PricingEntry {
        canonical: "o4-mini",
        aliases: &["o4-mini"],
        pricing: pricing(1_100, 4_400, None, Some(550)),
    },
    PricingEntry {
        canonical: "gemini-3.1-pro",
        aliases: &["gemini-3.1-pro", "gemini-3-1-pro"],
        pricing: pricing(2_000, 12_000, None, None),
    },
    PricingEntry {
        canonical: "gemini-3-pro",
        aliases: &["gemini-3-pro"],
        pricing: pricing(2_000, 12_000, None, None),
    },
    PricingEntry {
        canonical: "gemini-3-flash",
        aliases: &["gemini-3-flash"],
        pricing: pricing(150, 600, None, None),
    },
    PricingEntry {
        canonical: "gemini-2.5-pro",
        aliases: &["gemini-2.5-pro", "gemini-2-5-pro"],
        pricing: pricing(1_250, 10_000, None, None),
    },
    PricingEntry {
        canonical: "gemini-2.5-flash",
        aliases: &["gemini-2.5-flash", "gemini-2-5-flash"],
        pricing: pricing(150, 600, None, None),
    },
    PricingEntry {
        canonical: "gemini-2.0-flash",
        aliases: &["gemini-2.0-flash", "gemini-2-0-flash"],
        pricing: pricing(100, 400, None, None),
    },
    PricingEntry {
        canonical: "gemini-1.5-pro",
        aliases: &["gemini-1.5-pro", "gemini-1-5-pro"],
        pricing: pricing(1_250, 5_000, None, None),
    },
    PricingEntry {
        canonical: "gemini-1.5-flash",
        aliases: &["gemini-1.5-flash", "gemini-1-5-flash"],
        pricing: pricing(75, 300, None, None),
    },
    PricingEntry {
        canonical: "grok-4.5",
        aliases: &["grok-4.5", "grok-4.5-latest", "grok-build-latest"],
        pricing: pricing(2_000, 6_000, None, Some(500)),
    },
    PricingEntry {
        canonical: "grok-4-fast",
        aliases: &["grok-4-fast", "grok-inference"],
        pricing: pricing(1_250, 2_500, None, Some(200)),
    },
    PricingEntry {
        canonical: "grok-build",
        aliases: &["grok-build-0.1", "grok-build"],
        pricing: pricing(1_000, 2_000, None, Some(200)),
    },
    PricingEntry {
        canonical: "grok-4",
        aliases: &["grok-4"],
        pricing: pricing(1_250, 2_500, None, Some(200)),
    },
    PricingEntry {
        canonical: "grok-3-mini",
        aliases: &["grok-3-mini", "grok-2"],
        pricing: pricing(1_250, 2_500, None, Some(200)),
    },
    PricingEntry {
        canonical: "grok-3",
        aliases: &["grok-3"],
        pricing: pricing(1_250, 2_500, None, Some(200)),
    },
    PricingEntry {
        // glm-5.2 list price not yet published separately; mirrors glm-5.1
        // ($1.4/$4.4 per Mtok, $0.26 cached) until Z.AI publishes it.
        canonical: "glm-5.2",
        aliases: &["glm-5.2", "glm-5-2"],
        pricing: pricing(1_400, 4_400, None, Some(260)),
    },
    PricingEntry {
        canonical: "glm-5.1",
        aliases: &["glm-5.1", "glm-5-1"],
        pricing: pricing(1_400, 4_400, None, Some(260)),
    },
    PricingEntry {
        canonical: "glm-5-turbo",
        aliases: &["glm-5-turbo"],
        pricing: pricing(1_200, 4_000, None, Some(240)),
    },
    PricingEntry {
        canonical: "glm-5",
        aliases: &["glm-5"],
        pricing: pricing(1_000, 3_200, None, Some(200)),
    },
    PricingEntry {
        canonical: "glm-4.7-flashx",
        aliases: &["glm-4.7-flashx", "glm-4-7-flashx"],
        pricing: pricing(70, 400, None, Some(10)),
    },
    PricingEntry {
        canonical: "glm-4.7",
        aliases: &["glm-4.7", "glm-4-7"],
        pricing: pricing(600, 2_200, None, Some(110)),
    },
    PricingEntry {
        canonical: "glm-4.6",
        aliases: &["glm-4.6", "glm-4-6"],
        pricing: pricing(600, 2_200, None, Some(110)),
    },
    PricingEntry {
        canonical: "glm-4.5-airx",
        aliases: &["glm-4.5-airx", "glm-4-5-airx"],
        pricing: pricing(1_100, 4_500, None, Some(220)),
    },
    PricingEntry {
        canonical: "glm-4.5-air",
        aliases: &["glm-4.5-air", "glm-4-5-air"],
        pricing: pricing(200, 1_100, None, Some(30)),
    },
    PricingEntry {
        canonical: "glm-4.5-x",
        aliases: &["glm-4.5-x", "glm-4-5-x"],
        pricing: pricing(2_200, 8_900, None, Some(450)),
    },
    PricingEntry {
        canonical: "glm-4.5",
        aliases: &["glm-4.5", "glm-4-5"],
        pricing: pricing(600, 2_200, None, Some(110)),
    },
    PricingEntry {
        canonical: "minimax-m3",
        aliases: &["minimax-m3"],
        pricing: pricing(600, 2_400, Some(375), Some(60)),
    },
    PricingEntry {
        canonical: "minimax-m2.7-highspeed",
        aliases: &["minimax-m2.7-highspeed", "minimax-m2-7-highspeed"],
        pricing: pricing(600, 2_400, Some(375), Some(60)),
    },
    PricingEntry {
        canonical: "minimax-m2.7",
        aliases: &["minimax-m2.7", "minimax-m2-7"],
        pricing: pricing(300, 1_200, Some(375), Some(60)),
    },
    PricingEntry {
        canonical: "minimax-m2.5-highspeed",
        aliases: &["minimax-m2.5-highspeed", "minimax-m2-5-highspeed"],
        pricing: pricing(600, 2_400, Some(375), Some(30)),
    },
    PricingEntry {
        canonical: "minimax-m2.5",
        aliases: &["minimax-m2.5", "minimax-m2-5"],
        pricing: pricing(300, 1_200, Some(375), Some(30)),
    },
    PricingEntry {
        canonical: "minimax-m2.1-highspeed",
        aliases: &["minimax-m2.1-highspeed", "minimax-m2-1-highspeed"],
        pricing: pricing(600, 2_400, Some(375), Some(30)),
    },
    PricingEntry {
        canonical: "minimax-m2.1",
        aliases: &["minimax-m2.1", "minimax-m2-1"],
        pricing: pricing(300, 1_200, Some(375), Some(30)),
    },
    PricingEntry {
        canonical: "minimax-m2",
        aliases: &["minimax-m2"],
        pricing: pricing(300, 1_200, Some(375), Some(30)),
    },
];

/// Token usage from an API call.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

impl TokenUsage {
    /// Check if there's any usage to compute cost from.
    pub fn has_usage(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_creation_input_tokens.unwrap_or(0) > 0
            || self.cache_read_input_tokens.unwrap_or(0) > 0
    }
}

/// Normalize model names to canonical form for pricing lookup.
fn normalize_model(model: &str) -> &str {
    let trimmed = model.trim();
    let normalized_for_match = trimmed.to_ascii_lowercase().replace(['_', ' '], "-");

    for entry in PRICING_ENTRIES {
        if entry
            .aliases
            .iter()
            .any(|alias| model_alias_matches(&normalized_for_match, alias))
        {
            return entry.canonical;
        }
    }

    trimmed
}

fn model_alias_matches(model: &str, alias: &str) -> bool {
    for (idx, _) in model.match_indices(alias) {
        let after = &model[idx + alias.len()..];
        if after.is_empty() {
            return true;
        }
        if alias.as_bytes().last().is_some_and(|b| b.is_ascii_digit()) {
            // Numeric family aliases may have date/version suffixes, but should
            // not swallow named variants such as gpt-5.6-terra.
            if let Some(suffix) = after.strip_prefix('-') {
                if suffix
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_digit())
                    || suffix.starts_with("codex")
                {
                    return true;
                }
            }
            continue;
        }
        return true;
    }
    false
}

/// Normalize model names to the canonical pricing key.
pub fn normalized_model(model: &str) -> String {
    normalize_model(model).to_string()
}

/// Get pricing for a model. Returns None if model is unknown.
///
/// Prices are per 1M tokens converted to nanodollars per token:
/// - $3/1M input = 3_000 nanodollars per token
/// - $15/1M output = 15_000 nanodollars per token
pub fn pricing_for_model(model: &str) -> Option<ModelPricing> {
    let normalized = normalize_model(model);
    PRICING_ENTRIES
        .iter()
        .find(|entry| entry.canonical == normalized)
        .map(|entry| entry.pricing)
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
    let regular_input = usage
        .input_tokens
        .saturating_sub(usage.cache_creation_input_tokens.unwrap_or(0));
    cost_nano += regular_input.saturating_mul(pricing.input_nano_per_token);

    // Output tokens
    cost_nano += usage
        .output_tokens
        .saturating_mul(pricing.output_nano_per_token);

    // Cache creation tokens (usually more expensive)
    if let Some(cache_create) = usage.cache_creation_input_tokens {
        let rate = pricing
            .cache_create_nano_per_token
            .unwrap_or(pricing.input_nano_per_token);
        cost_nano += cache_create.saturating_mul(rate);
    }

    // Cache read tokens (usually much cheaper)
    if let Some(cache_read) = usage.cache_read_input_tokens {
        let rate = pricing
            .cache_read_nano_per_token
            .unwrap_or(pricing.input_nano_per_token);
        cost_nano += cache_read.saturating_mul(rate);
    }

    // Convert nanodollars to cents: 1 cent = $0.01 = 10_000_000 nanodollars
    // Round to nearest cent
    (cost_nano + 5_000_000) / 10_000_000
}

/// Resolve cost and provenance from optional actual billing, model name, and
/// token usage.  This is the canonical function used by all agent backends
/// (Claude Code, OpenCode, Codex) to produce the `(cost_cents, CostSource)`
/// pair stored in mission event metadata.
///
/// Priority:
///   1. `actual_cost_cents` present → `(actual, CostSource::Actual)`
///   2. Token usage + known model pricing → `(estimated, CostSource::Estimated)`
///   3. Otherwise → `(0, CostSource::Unknown)`
pub fn resolve_cost_cents_and_source(
    actual_cost_cents: Option<u64>,
    model: Option<&str>,
    usage: &TokenUsage,
) -> (u64, crate::agents::CostSource) {
    use crate::agents::CostSource;

    if let Some(actual) = actual_cost_cents {
        return (actual, CostSource::Actual);
    }

    if usage.has_usage() {
        if let Some(model_name) = model {
            if pricing_for_model(model_name).is_some() {
                return (
                    cost_cents_from_usage(model_name, usage),
                    CostSource::Estimated,
                );
            }
            return (0, CostSource::Unknown);
        }
    }

    (0, CostSource::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model() {
        assert_eq!(
            normalize_model("claude-3-5-sonnet-20241022"),
            "claude-3-5-sonnet"
        );
        assert_eq!(
            normalize_model("claude-3.5-sonnet-latest"),
            "claude-3-5-sonnet"
        );
        assert_eq!(normalize_model("claude-opus-4-7"), "claude-opus-4-7");
        assert_eq!(normalize_model("claude-5-opus"), "claude-opus-5");
        assert_eq!(
            normalize_model("claude-opus-4-5-20251101"),
            "claude-opus-4-5"
        );
        assert_eq!(
            normalize_model("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            normalize_model("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5"
        );
        assert_eq!(normalize_model("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(normalize_model("gpt-5.3-codex"), "gpt-5.3");
        assert_eq!(normalize_model("openai/gpt-5.6"), "gpt-5.6-sol");
        assert_eq!(normalize_model("gpt-5.6-2026-07-09"), "gpt-5.6-sol");
        assert_eq!(normalize_model("openai/gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(normalize_model("openai/gpt-5.6-terra"), "gpt-5.6-terra");
        assert_eq!(normalize_model("openai/gpt-5.6-luna"), "gpt-5.6-luna");
        assert_eq!(normalize_model("gpt-5.5-pro"), "gpt-5.5-pro");
        assert_eq!(normalize_model("gpt-5.4-pro"), "gpt-5.4-pro");
        assert_eq!(normalize_model("OpenAI/GPT-5"), "gpt-5");
        assert_eq!(normalize_model("openai/gpt-5.1-codex"), "gpt-5");
        assert_eq!(normalize_model("gpt-5-mini"), "gpt-5-mini");
        assert_eq!(normalize_model("gemini-2.5-pro-preview"), "gemini-2.5-pro");
        assert_eq!(normalize_model("gemini-3.1-pro-preview"), "gemini-3.1-pro");
        assert_eq!(normalize_model("gemini-3-1-pro-preview"), "gemini-3.1-pro");
        assert_eq!(normalize_model("gemini-3-pro-preview"), "gemini-3-pro");
        assert_eq!(normalize_model("grok-4-fast-reasoning"), "grok-4-fast");
        assert_eq!(normalize_model("xAI/Grok Inference"), "grok-4-fast");
        assert_eq!(normalize_model("xai/grok-4.5"), "grok-4.5");
        assert_eq!(normalize_model("xai/grok-4.5-latest"), "grok-4.5");
        assert_eq!(normalize_model("xai/grok-build-latest"), "grok-4.5");
        assert_eq!(normalize_model("grok-build"), "grok-build");
        assert_eq!(normalize_model("zai/glm-5"), "glm-5");
        assert_eq!(normalize_model("zai/glm-5.2"), "glm-5.2");
        assert_eq!(normalize_model("zai/glm-5.1"), "glm-5.1");
        assert_eq!(normalize_model("zai/glm-5-turbo"), "glm-5-turbo");
        assert_eq!(normalize_model("zai/glm-4.7"), "glm-4.7");
        assert_eq!(normalize_model("zai/glm-4.5-air"), "glm-4.5-air");
        assert_eq!(
            normalize_model("minimax/MiniMax-M2.5-highspeed"),
            "minimax-m2.5-highspeed"
        );
        assert_eq!(normalize_model("minimax/MiniMax-M2.5"), "minimax-m2.5");
    }

    #[test]
    fn test_pricing_for_known_models() {
        assert!(pricing_for_model("claude-3-5-sonnet").is_some());
        assert!(pricing_for_model("claude-opus-4-7").is_some());
        assert!(pricing_for_model("claude-opus-5").is_some());
        assert!(pricing_for_model("claude-opus-4-5").is_some());
        assert!(pricing_for_model("claude-sonnet-4-5").is_some());
        assert!(pricing_for_model("claude-haiku-4-5").is_some());
        assert!(pricing_for_model("gpt-4o").is_some());
        assert!(pricing_for_model("gpt-5.3-codex").is_some());
        assert!(pricing_for_model("openai/gpt-5.6").is_some());
        assert!(pricing_for_model("openai/gpt-5.6-sol").is_some());
        assert!(pricing_for_model("openai/gpt-5.6-terra").is_some());
        assert!(pricing_for_model("openai/gpt-5.6-luna").is_some());
        assert!(pricing_for_model("gpt-5.5-pro").is_some());
        assert!(pricing_for_model("gpt-5.4-pro").is_some());
        assert!(pricing_for_model("OpenAI/GPT-5").is_some());
        assert!(pricing_for_model("openai/gpt-5.1-codex").is_some());
        assert!(pricing_for_model("gpt-5-mini").is_some());
        assert!(pricing_for_model("gemini-2.5-pro").is_some());
        assert!(pricing_for_model("gemini-3.1-pro-preview").is_some());
        assert!(pricing_for_model("gemini-3-pro-preview").is_some());
        assert!(pricing_for_model("gemini-3-flash-preview").is_some());
        assert!(pricing_for_model("grok-4-fast").is_some());
        assert!(pricing_for_model("xAI/Grok Inference").is_some());
        assert!(pricing_for_model("xai/grok-4.5").is_some());
        assert!(pricing_for_model("grok-build").is_some());
        assert!(pricing_for_model("glm-5").is_some());
        assert!(pricing_for_model("zai/glm-5.2").is_some());
        assert!(pricing_for_model("glm-5.1").is_some());
        assert!(pricing_for_model("zai/glm-5-turbo").is_some());
        assert!(pricing_for_model("zai/glm-4.7").is_some());
        assert!(pricing_for_model("zai/glm-4.5-air").is_some());
        assert!(pricing_for_model("minimax/MiniMax-M3").is_some());
        assert!(pricing_for_model("minimax/MiniMax-M2.5-highspeed").is_some());
        assert!(pricing_for_model("minimax/MiniMax-M2.5").is_some());
    }

    #[test]
    fn test_official_api_price_points() {
        let gpt_53 = pricing_for_model("gpt-5.3-codex").expect("gpt-5.3 pricing");
        assert_eq!(gpt_53.input_nano_per_token, 1_750);
        assert_eq!(gpt_53.output_nano_per_token, 14_000);

        let grok = pricing_for_model("grok-build").expect("grok pricing");
        assert_eq!(grok.input_nano_per_token, 1_000);
        assert_eq!(grok.output_nano_per_token, 2_000);
        assert_eq!(grok.cache_read_nano_per_token, Some(200));

        let grok_fast = pricing_for_model("grok-4-fast").expect("grok fast pricing");
        assert_eq!(grok_fast.input_nano_per_token, 1_250);
        assert_eq!(grok_fast.output_nano_per_token, 2_500);
        assert_eq!(grok_fast.cache_read_nano_per_token, Some(200));

        let grok_45 = pricing_for_model("xai/grok-4.5").expect("grok 4.5 pricing");
        assert_eq!(grok_45.input_nano_per_token, 2_000);
        assert_eq!(grok_45.output_nano_per_token, 6_000);
        assert_eq!(grok_45.cache_read_nano_per_token, Some(500));

        let glm = pricing_for_model("zai/glm-5.1").expect("glm-5.1 pricing");
        assert_eq!(glm.input_nano_per_token, 1_400);
        assert_eq!(glm.output_nano_per_token, 4_400);

        let minimax = pricing_for_model("minimax/MiniMax-M3").expect("minimax pricing");
        assert_eq!(minimax.input_nano_per_token, 600);
        assert_eq!(minimax.output_nano_per_token, 2_400);
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
            input_tokens: 0,
            output_tokens: 1000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(5000),
        };
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        // (0 * 3000 + 1000 * 15000 + 5000 * 300) / 10_000_000 = (15_000_000 + 1_500_000) / 10_000_000 = 1.65 cents
        assert_eq!(cost, 2); // Rounds to 2 cents
    }

    #[test]
    fn test_cache_read_tokens_do_not_reduce_regular_input_tokens() {
        let usage = TokenUsage {
            input_tokens: 10_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(20_000),
        };
        let cost = cost_cents_from_usage("claude-3-5-sonnet", &usage);
        assert_eq!(cost, 4);
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

    #[test]
    fn test_has_usage_true_for_cache_only() {
        let usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: Some(1_000),
            cache_read_input_tokens: Some(2_000),
        };
        assert!(usage.has_usage());
    }

    #[test]
    fn resolve_cost_prefers_actual_then_estimated_then_unknown() {
        use crate::agents::CostSource;

        let usage = TokenUsage {
            input_tokens: 10_000,
            output_tokens: 1_000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };

        // Actual takes priority
        let (cost, source) = resolve_cost_cents_and_source(Some(42), Some("gpt-4o"), &usage);
        assert_eq!(cost, 42);
        assert_eq!(source, CostSource::Actual);

        // Falls back to estimated when model is known
        let (cost, source) = resolve_cost_cents_and_source(None, Some("gpt-4o"), &usage);
        assert!(cost > 0);
        assert_eq!(source, CostSource::Estimated);

        // Unknown when no usage
        let empty = TokenUsage::default();
        let (cost, source) = resolve_cost_cents_and_source(None, Some("gpt-4o"), &empty);
        assert_eq!(cost, 0);
        assert_eq!(source, CostSource::Unknown);
    }
}
