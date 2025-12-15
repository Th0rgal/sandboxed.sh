//! Types for the memory subsystem.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of a run or task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// A run stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbRun {
    pub id: Uuid,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
    pub input_text: String,
    pub final_output: Option<String>,
    pub total_cost_cents: Option<i32>,
    pub summary_text: Option<String>,
    pub archive_path: Option<String>,
}

/// A task stored in the database (hierarchical).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTask {
    pub id: Uuid,
    pub run_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub depth: i32,
    pub seq: i32,
    pub description: String,
    pub status: String,
    pub complexity_score: Option<f64>,
    pub model_used: Option<String>,
    pub budget_cents: Option<i32>,
    pub spent_cents: Option<i32>,
    pub output: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// An event stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbEvent {
    pub id: Option<i64>,
    pub run_id: Uuid,
    pub task_id: Option<Uuid>,
    pub seq: i32,
    pub ts: Option<String>,
    pub agent_type: String,
    pub event_kind: String,
    pub preview_text: Option<String>,
    pub meta: Option<serde_json::Value>,
    pub blob_path: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub cost_cents: Option<i32>,
}

/// A chunk for vector search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbChunk {
    pub id: Option<Uuid>,
    pub run_id: Uuid,
    pub task_id: Option<Uuid>,
    pub source_event_id: Option<i64>,
    pub chunk_text: String,
    pub meta: Option<serde_json::Value>,
}

/// Event kinds for the event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// Task started
    TaskStart,
    /// Task completed
    TaskEnd,
    /// LLM request sent
    LlmRequest,
    /// LLM response received
    LlmResponse,
    /// Tool invoked
    ToolCall,
    /// Tool result received
    ToolResult,
    /// Complexity estimation
    ComplexityEstimate,
    /// Model selection decision
    ModelSelect,
    /// Verification result
    Verification,
    /// Task split into subtasks
    TaskSplit,
    /// Error occurred
    Error,
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::TaskStart => "task_start",
            Self::TaskEnd => "task_end",
            Self::LlmRequest => "llm_request",
            Self::LlmResponse => "llm_response",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::ComplexityEstimate => "complexity_estimate",
            Self::ModelSelect => "model_select",
            Self::Verification => "verification",
            Self::TaskSplit => "task_split",
            Self::Error => "error",
        };
        write!(f, "{}", s)
    }
}

/// Search result from vector similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: Uuid,
    pub run_id: Uuid,
    pub task_id: Option<Uuid>,
    pub chunk_text: String,
    pub meta: Option<serde_json::Value>,
    pub similarity: f64,
}

/// Context pack for injection into prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPack {
    /// Relevant chunks from memory
    pub chunks: Vec<SearchResult>,
    /// Total token estimate for the context
    pub estimated_tokens: usize,
    /// Query that was used
    pub query: String,
}

/// Task outcome record for learning from execution history.
/// 
/// Captures predictions vs actuals to enable data-driven optimization
/// of complexity estimation, model selection, and budget allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbTaskOutcome {
    pub id: Option<Uuid>,
    pub run_id: Uuid,
    pub task_id: Uuid,
    
    // Predictions (what we estimated before execution)
    pub predicted_complexity: Option<f64>,
    pub predicted_tokens: Option<i64>,
    pub predicted_cost_cents: Option<i64>,
    pub selected_model: Option<String>,
    
    // Actuals (what happened during execution)
    pub actual_tokens: Option<i64>,
    pub actual_cost_cents: Option<i64>,
    pub success: bool,
    pub iterations: Option<i32>,
    pub tool_calls_count: Option<i32>,
    
    // Metadata for similarity search
    pub task_description: String,
    /// Category of task (inferred or explicit)
    pub task_type: Option<String>,
    
    // Computed ratios for quick stats
    pub cost_error_ratio: Option<f64>,
    pub token_error_ratio: Option<f64>,
    
    pub created_at: Option<String>,
}

impl DbTaskOutcome {
    /// Create a new outcome from predictions and actuals.
    pub fn new(
        run_id: Uuid,
        task_id: Uuid,
        task_description: String,
        predicted_complexity: Option<f64>,
        predicted_tokens: Option<i64>,
        predicted_cost_cents: Option<i64>,
        selected_model: Option<String>,
        actual_tokens: Option<i64>,
        actual_cost_cents: Option<i64>,
        success: bool,
        iterations: Option<i32>,
        tool_calls_count: Option<i32>,
    ) -> Self {
        // Compute error ratios
        let cost_error_ratio = match (actual_cost_cents, predicted_cost_cents) {
            (Some(actual), Some(predicted)) if predicted > 0 => {
                Some(actual as f64 / predicted as f64)
            }
            _ => None,
        };
        
        let token_error_ratio = match (actual_tokens, predicted_tokens) {
            (Some(actual), Some(predicted)) if predicted > 0 => {
                Some(actual as f64 / predicted as f64)
            }
            _ => None,
        };
        
        Self {
            id: None,
            run_id,
            task_id,
            predicted_complexity,
            predicted_tokens,
            predicted_cost_cents,
            selected_model,
            actual_tokens,
            actual_cost_cents,
            success,
            iterations,
            tool_calls_count,
            task_description,
            task_type: None,
            cost_error_ratio,
            token_error_ratio,
            created_at: None,
        }
    }
}

/// Model performance statistics from historical data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    pub model_id: String,
    /// Success rate (0-1)
    pub success_rate: f64,
    /// Average cost vs predicted (1.0 = accurate, >1 = underestimated)
    pub avg_cost_ratio: f64,
    /// Average tokens vs predicted
    pub avg_token_ratio: f64,
    /// Average iterations needed
    pub avg_iterations: f64,
    /// Number of samples
    pub sample_count: i64,
}

/// Historical context for a task (similar past tasks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalContext {
    /// Similar past task outcomes
    pub similar_outcomes: Vec<DbTaskOutcome>,
    /// Average cost adjustment multiplier
    pub avg_cost_multiplier: f64,
    /// Average token adjustment multiplier
    pub avg_token_multiplier: f64,
    /// Success rate for similar tasks
    pub similar_success_rate: f64,
}

impl ContextPack {
    /// Format as a string for prompt injection.
    pub fn format_for_prompt(&self) -> String {
        if self.chunks.is_empty() {
            return String::new();
        }
        
        let mut out = String::from("## Relevant Context from Memory\n\n");
        for (i, chunk) in self.chunks.iter().enumerate() {
            out.push_str(&format!(
                "### Context {} (similarity: {:.2})\n{}\n\n",
                i + 1,
                chunk.similarity,
                chunk.chunk_text
            ));
        }
        out
    }
}

