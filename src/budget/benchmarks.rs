//! Benchmark data integration for model selection.
//!
//! This module loads benchmark scores from `models_with_benchmarks.json`
//! and provides capability lookup for task-aware model selection.
//!
//! # Task Types
//! - `code`: Coding tasks (SWE-bench, HumanEval scores)
//! - `math`: Mathematical reasoning (AIME, MATH-500 scores)
//! - `reasoning`: General reasoning (GPQA, MMLU scores)
//! - `tool_calling`: Tool/function use (BFCL, Tau-Bench scores)
//! - `long_context`: Long context handling (RULER, LongBench scores)
//! - `general`: General assistant tasks (IFEval, Arena-Hard scores)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Task type categories for model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Coding tasks: writing, debugging, refactoring code
    Code,
    /// Mathematical reasoning: calculations, proofs, problem solving
    Math,
    /// General reasoning: logic, analysis, comprehension
    Reasoning,
    /// Tool calling: function calls, API usage
    ToolCalling,
    /// Long context: processing long documents, multi-file analysis
    LongContext,
    /// General assistant: conversation, instructions, misc tasks
    General,
}

impl TaskType {
    /// Get the category key used in the JSON data.
    pub fn category_key(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Math => "math",
            Self::Reasoning => "reasoning",
            Self::ToolCalling => "tool_calling",
            Self::LongContext => "long_context",
            Self::General => "general",
        }
    }

    /// Infer task type from task description.
    pub fn infer_from_description(description: &str) -> Self {
        let desc_lower = description.to_lowercase();
        
        // Helper: check if word exists as a whole word (not substring)
        let has_word = |word: &str| {
            desc_lower.split(|c: char| !c.is_alphanumeric())
                .any(|w| w == word)
        };

        // Code indicators (use word boundaries to avoid false positives like "interesting" matching "test")
        if has_word("code")
            || has_word("implement")
            || has_word("function")
            || has_word("bug")
            || has_word("debug")
            || has_word("refactor")
            || has_word("test")
            || has_word("tests")
            || has_word("compile")
            || has_word("script")
            || has_word("api")
            || desc_lower.contains("programming")
        {
            return Self::Code;
        }

        // Math indicators
        if desc_lower.contains("math")
            || desc_lower.contains("calculate")
            || desc_lower.contains("equation")
            || desc_lower.contains("formula")
            || desc_lower.contains("prove")
            || desc_lower.contains("number")
            || desc_lower.contains("algorithm")
            || desc_lower.contains("sum")
            || desc_lower.contains("prime")
            || desc_lower.contains("fibonacci")
            || desc_lower.contains("factor")
            || desc_lower.contains("integer")
            || desc_lower.contains("solve")
            || desc_lower.contains("multiply")
            || desc_lower.contains("divide")
        {
            return Self::Math;
        }

        // Tool calling indicators
        if desc_lower.contains("tool")
            || desc_lower.contains("fetch")
            || desc_lower.contains("search")
            || desc_lower.contains("file")
            || desc_lower.contains("directory")
            || desc_lower.contains("command")
            || desc_lower.contains("browser")
            || desc_lower.contains("screenshot")
            || desc_lower.contains("navigate")
            || desc_lower.contains("website")
            || desc_lower.contains("webpage")
            || desc_lower.contains("url")
        {
            return Self::ToolCalling;
        }

        // Long context indicators
        if desc_lower.contains("long")
            || desc_lower.contains("document")
            || desc_lower.contains("analyze")
            || desc_lower.contains("summarize")
            || desc_lower.contains("multiple files")
        {
            return Self::LongContext;
        }

        // Default to reasoning for analytical tasks, general otherwise
        if desc_lower.contains("reason")
            || desc_lower.contains("explain")
            || desc_lower.contains("why")
            || desc_lower.contains("how")
            || desc_lower.contains("analyze")
        {
            return Self::Reasoning;
        }

        Self::General
    }
}

/// Category scores for a model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CategoryScores {
    #[serde(default)]
    pub code: Option<f64>,
    #[serde(default)]
    pub math: Option<f64>,
    #[serde(default)]
    pub reasoning: Option<f64>,
    #[serde(default)]
    pub tool_calling: Option<f64>,
    #[serde(default)]
    pub long_context: Option<f64>,
    #[serde(default)]
    pub general: Option<f64>,
}

impl CategoryScores {
    /// Get score for a specific task type.
    pub fn get(&self, task_type: TaskType) -> Option<f64> {
        match task_type {
            TaskType::Code => self.code,
            TaskType::Math => self.math,
            TaskType::Reasoning => self.reasoning,
            TaskType::ToolCalling => self.tool_calling,
            TaskType::LongContext => self.long_context,
            TaskType::General => self.general,
        }
    }

    /// Get the best score across all categories.
    pub fn best_score(&self) -> Option<f64> {
        [
            self.code,
            self.math,
            self.reasoning,
            self.tool_calling,
            self.long_context,
            self.general,
        ]
        .into_iter()
        .flatten()
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Get average score across all available categories.
    pub fn average_score(&self) -> Option<f64> {
        let scores: Vec<f64> = [
            self.code,
            self.math,
            self.reasoning,
            self.tool_calling,
            self.long_context,
            self.general,
        ]
        .into_iter()
        .flatten()
        .collect();

        if scores.is_empty() {
            None
        } else {
            Some(scores.iter().sum::<f64>() / scores.len() as f64)
        }
    }
}

/// Model entry with benchmark data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelBenchmark {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub category_scores: Option<CategoryScores>,
}

impl ModelBenchmark {
    /// Get capability score for a task type (0.0-1.0).
    pub fn capability(&self, task_type: TaskType) -> f64 {
        self.category_scores
            .as_ref()
            .and_then(|cs| cs.get(task_type))
            .unwrap_or(0.5) // Default to moderate capability if unknown
    }

    /// Check if this model has benchmark data.
    pub fn has_benchmarks(&self) -> bool {
        self.category_scores.is_some()
    }
}

/// Benchmark data file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkData {
    pub generated_at: String,
    pub total_models: usize,
    pub models_with_benchmarks: usize,
    pub categories: Vec<String>,
    pub models: Vec<ModelBenchmark>,
}

/// Benchmark registry for model capability lookup.
#[derive(Debug)]
pub struct BenchmarkRegistry {
    /// Model ID -> Benchmark data
    models: HashMap<String, ModelBenchmark>,
    /// Normalized model name -> Original model ID (for fuzzy matching)
    normalized_index: HashMap<String, String>,
}

impl BenchmarkRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            normalized_index: HashMap::new(),
        }
    }

    /// Load benchmark data from file.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| format!("Failed to read benchmark file: {}", e))?;

        let data: BenchmarkData = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse benchmark file: {}", e))?;

        let mut registry = Self::new();

        for model in data.models {
            let id = model.id.clone();
            let normalized = Self::normalize_id(&id);

            registry
                .normalized_index
                .insert(normalized.clone(), id.clone());

            // Also index by the model name part (after the slash)
            if let Some(name_part) = id.split('/').last() {
                let normalized_name = Self::normalize_id(name_part);
                if !registry.normalized_index.contains_key(&normalized_name) {
                    registry
                        .normalized_index
                        .insert(normalized_name, id.clone());
                }
            }

            registry.models.insert(id, model);
        }

        tracing::info!(
            "Loaded {} models with benchmarks ({} total indexed)",
            registry
                .models
                .values()
                .filter(|m| m.has_benchmarks())
                .count(),
            registry.models.len()
        );

        Ok(registry)
    }

    /// Normalize a model ID for matching.
    fn normalize_id(id: &str) -> String {
        id.to_lowercase().replace([':', '-', '_', '.'], "")
    }

    /// Look up a model by ID (with fuzzy matching).
    pub fn get(&self, model_id: &str) -> Option<&ModelBenchmark> {
        // Try exact match first
        if let Some(model) = self.models.get(model_id) {
            return Some(model);
        }

        // Try normalized match
        let normalized = Self::normalize_id(model_id);
        if let Some(original_id) = self.normalized_index.get(&normalized) {
            return self.models.get(original_id);
        }

        // Try partial match on model name part
        if let Some(name_part) = model_id.split('/').last() {
            let normalized_name = Self::normalize_id(name_part);
            if let Some(original_id) = self.normalized_index.get(&normalized_name) {
                return self.models.get(original_id);
            }
        }

        None
    }

    /// Get capability score for a model and task type.
    pub fn capability(&self, model_id: &str, task_type: TaskType) -> f64 {
        self.get(model_id)
            .map(|m| m.capability(task_type))
            .unwrap_or(0.5)
    }

    /// Get top N models for a task type.
    pub fn top_models(&self, task_type: TaskType, n: usize) -> Vec<(&str, f64)> {
        let mut scores: Vec<_> = self
            .models
            .iter()
            .filter_map(|(id, model)| {
                model
                    .category_scores
                    .as_ref()
                    .and_then(|cs| cs.get(task_type))
                    .map(|score| (id.as_str(), score))
            })
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(n);
        scores
    }

    /// Get all models with benchmarks.
    pub fn all_models(&self) -> impl Iterator<Item = &ModelBenchmark> {
        self.models.values()
    }

    /// Get count of models with benchmark data.
    pub fn benchmark_count(&self) -> usize {
        self.models.values().filter(|m| m.has_benchmarks()).count()
    }
}

impl Default for BenchmarkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe benchmark registry wrapper.
pub type SharedBenchmarkRegistry = Arc<RwLock<BenchmarkRegistry>>;

/// Create a shared benchmark registry, loading from default path.
pub fn load_benchmarks(workspace_dir: &str) -> SharedBenchmarkRegistry {
    let path = format!("{}/models_with_benchmarks.json", workspace_dir);

    match BenchmarkRegistry::load_from_file(&path) {
        Ok(registry) => {
            tracing::info!("Loaded benchmark registry from {}", path);
            Arc::new(RwLock::new(registry))
        }
        Err(e) => {
            tracing::warn!("Failed to load benchmarks: {}. Using empty registry.", e);
            Arc::new(RwLock::new(BenchmarkRegistry::new()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_type_inference() {
        assert_eq!(
            TaskType::infer_from_description("Implement a function to sort arrays"),
            TaskType::Code
        );
        assert_eq!(
            TaskType::infer_from_description("Calculate the integral of x^2"),
            TaskType::Math
        );
        assert_eq!(
            TaskType::infer_from_description("Search for files containing 'error'"),
            TaskType::ToolCalling
        );
        assert_eq!(
            TaskType::infer_from_description("Explain why the sky is blue"),
            TaskType::Reasoning
        );
    }

    #[test]
    fn test_category_scores() {
        let scores = CategoryScores {
            code: Some(0.8),
            math: Some(0.9),
            reasoning: Some(0.7),
            ..Default::default()
        };

        assert_eq!(scores.get(TaskType::Code), Some(0.8));
        assert_eq!(scores.get(TaskType::Math), Some(0.9));
        assert_eq!(scores.get(TaskType::ToolCalling), None);
        assert_eq!(scores.best_score(), Some(0.9));
    }

    #[test]
    fn test_normalize_id() {
        assert_eq!(
            BenchmarkRegistry::normalize_id("openai/gpt-4.1-mini"),
            "openaigpt41mini"
        );
        assert_eq!(
            BenchmarkRegistry::normalize_id("deepseek/deepseek-v3.2:exacto"),
            "deepseekdeepseekv32exacto"
        );
    }
}
