//! Memory retriever for semantic search and context packing.

use std::sync::Arc;
use uuid::Uuid;
use serde::Deserialize;

use super::supabase::SupabaseClient;
use super::embed::EmbeddingClient;
use super::types::{SearchResult, ContextPack, DbTaskOutcome, ModelStats, HistoricalContext};

/// Default similarity threshold for vector search.
const DEFAULT_THRESHOLD: f64 = 0.5;

/// Default number of results to retrieve.
const DEFAULT_LIMIT: usize = 10;

/// Maximum tokens for context pack.
const MAX_CONTEXT_TOKENS: usize = 4000;

/// Retriever for searching and fetching context from memory.
pub struct MemoryRetriever {
    supabase: Arc<SupabaseClient>,
    embedder: Arc<EmbeddingClient>,
    rerank_model: Option<String>,
    openrouter_key: String,
}

impl MemoryRetriever {
    /// Create a new memory retriever.
    pub fn new(
        supabase: Arc<SupabaseClient>,
        embedder: Arc<EmbeddingClient>,
        rerank_model: Option<String>,
        openrouter_key: String,
    ) -> Self {
        Self {
            supabase,
            embedder,
            rerank_model,
            openrouter_key,
        }
    }
    
    /// Search for relevant chunks.
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
        threshold: Option<f64>,
        filter_run_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        let threshold = threshold.unwrap_or(DEFAULT_THRESHOLD);
        
        // Generate query embedding
        let embedding = self.embedder.embed(query).await?;
        
        // Vector search
        let results = self.supabase.search_chunks(
            &embedding,
            threshold,
            limit * 2, // Fetch more for reranking
            filter_run_id,
        ).await?;
        
        // Rerank if configured
        let results = if self.rerank_model.is_some() && results.len() > 1 {
            self.rerank(query, results, limit).await?
        } else {
            results.into_iter().take(limit).collect()
        };
        
        Ok(results)
    }
    
    /// Retrieve a context pack for prompt injection.
    pub async fn retrieve_context(
        &self,
        query: &str,
        filter_run_id: Option<Uuid>,
        max_tokens: Option<usize>,
    ) -> anyhow::Result<ContextPack> {
        let max_tokens = max_tokens.unwrap_or(MAX_CONTEXT_TOKENS);
        
        // Search for relevant chunks
        let results = self.search(query, Some(20), None, filter_run_id).await?;
        
        // Build context pack within token budget
        let mut chunks = Vec::new();
        let mut total_tokens = 0;
        
        for result in results {
            let chunk_tokens = EmbeddingClient::estimate_tokens(&result.chunk_text);
            
            if total_tokens + chunk_tokens > max_tokens {
                break;
            }
            
            total_tokens += chunk_tokens;
            chunks.push(result);
        }
        
        Ok(ContextPack {
            chunks,
            estimated_tokens: total_tokens,
            query: query.to_string(),
        })
    }
    
    /// Rerank results using LLM.
    async fn rerank(
        &self,
        query: &str,
        results: Vec<SearchResult>,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let model = match &self.rerank_model {
            Some(m) => m,
            None => return Ok(results.into_iter().take(limit).collect()),
        };
        
        // Build reranking prompt
        let passages: Vec<String> = results.iter()
            .enumerate()
            .map(|(i, r)| format!("[{}] {}", i, truncate(&r.chunk_text, 500)))
            .collect();
        
        let prompt = format!(
            r#"You are a relevance ranking assistant. Given a query and passages, rank the passages by relevance to the query.

Query: {}

Passages:
{}

Return a JSON array of passage indices ordered by relevance (most relevant first). Example: [2, 0, 5, 1, 3, 4]

Only return the JSON array, nothing else."#,
            query,
            passages.join("\n\n")
        );
        
        // Call LLM for reranking
        let client = reqwest::Client::new();
        let resp = client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.openrouter_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ],
                "temperature": 0.0
            }))
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            tracing::warn!("Rerank failed, using original order: {}", text);
            return Ok(results.into_iter().take(limit).collect());
        }
        
        // Parse response
        let response: RerankResponse = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to parse rerank response: {}", e);
                return Ok(results.into_iter().take(limit).collect());
            }
        };
        
        let content = response.choices.first()
            .and_then(|c| c.message.content.as_ref())
            .map(|s| s.trim())
            .unwrap_or("[]");
        
        // Parse ranking
        let ranking: Vec<usize> = match serde_json::from_str(content) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to parse ranking: {} from {}", e, content);
                return Ok(results.into_iter().take(limit).collect());
            }
        };
        
        // Reorder results
        let mut reranked = Vec::new();
        for idx in ranking.into_iter().take(limit) {
            if idx < results.len() {
                reranked.push(results[idx].clone());
            }
        }
        
        // Fill remaining slots if ranking was incomplete
        if reranked.len() < limit {
            for result in results {
                if !reranked.iter().any(|r| r.id == result.id) && reranked.len() < limit {
                    reranked.push(result);
                }
            }
        }
        
        Ok(reranked)
    }
    
    /// Get events for a run.
    pub async fn get_run_events(
        &self,
        run_id: Uuid,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<super::types::DbEvent>> {
        self.supabase.get_events_for_run(run_id, limit).await
    }
    
    /// Get tasks for a run.
    pub async fn get_run_tasks(&self, run_id: Uuid) -> anyhow::Result<Vec<super::types::DbTask>> {
        self.supabase.get_tasks_for_run(run_id).await
    }
    
    /// Get a run by ID.
    pub async fn get_run(&self, run_id: Uuid) -> anyhow::Result<Option<super::types::DbRun>> {
        self.supabase.get_run(run_id).await
    }
    
    /// List runs.
    pub async fn list_runs(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<super::types::DbRun>> {
        self.supabase.list_runs(limit, offset).await
    }
    
    // ==================== Learning Methods ====================
    
    /// Get model performance statistics for a given complexity range.
    /// 
    /// Returns historical success rates, cost ratios, etc. for each model
    /// that has been used at the given complexity level.
    pub async fn get_model_stats(
        &self,
        complexity: f64,
        range: f64,
    ) -> anyhow::Result<Vec<ModelStats>> {
        let min = (complexity - range).max(0.0);
        let max = (complexity + range).min(1.0);
        self.supabase.get_model_stats(min, max).await
    }
    
    /// Find similar past tasks and their outcomes.
    /// 
    /// Uses embedding similarity to find tasks that are semantically similar
    /// to the given task description, then returns their execution outcomes.
    pub async fn find_similar_tasks(
        &self,
        task_description: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<DbTaskOutcome>> {
        // Generate embedding for the task description
        let embedding = self.embedder.embed(task_description).await?;
        
        // Search for similar outcomes
        self.supabase.search_similar_outcomes(&embedding, 0.6, limit).await
    }
    
    /// Get historical context for a task.
    /// 
    /// Returns aggregated learning data from similar past tasks including:
    /// - Average cost adjustment multiplier
    /// - Average token adjustment multiplier
    /// - Success rate for similar tasks
    pub async fn get_historical_context(
        &self,
        task_description: &str,
        limit: usize,
    ) -> anyhow::Result<Option<HistoricalContext>> {
        let similar = self.find_similar_tasks(task_description, limit).await?;
        
        if similar.is_empty() {
            return Ok(None);
        }
        
        // Calculate aggregated stats
        let total = similar.len() as f64;
        
        let avg_cost_multiplier = similar.iter()
            .filter_map(|o| o.cost_error_ratio)
            .sum::<f64>() / similar.iter().filter(|o| o.cost_error_ratio.is_some()).count().max(1) as f64;
        
        let avg_token_multiplier = similar.iter()
            .filter_map(|o| o.token_error_ratio)
            .sum::<f64>() / similar.iter().filter(|o| o.token_error_ratio.is_some()).count().max(1) as f64;
        
        let success_count = similar.iter().filter(|o| o.success).count() as f64;
        let similar_success_rate = success_count / total;
        
        Ok(Some(HistoricalContext {
            similar_outcomes: similar,
            avg_cost_multiplier: if avg_cost_multiplier.is_nan() { 1.0 } else { avg_cost_multiplier },
            avg_token_multiplier: if avg_token_multiplier.is_nan() { 1.0 } else { avg_token_multiplier },
            similar_success_rate,
        }))
    }
    
    /// Get global learning statistics.
    pub async fn get_learning_stats(&self) -> anyhow::Result<serde_json::Value> {
        self.supabase.get_global_stats().await
    }
}

/// Truncate a string to max length.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

#[derive(Debug, Deserialize)]
struct RerankResponse {
    choices: Vec<RerankChoice>,
}

#[derive(Debug, Deserialize)]
struct RerankChoice {
    message: RerankMessage,
}

#[derive(Debug, Deserialize)]
struct RerankMessage {
    content: Option<String>,
}

