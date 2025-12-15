//! Supabase client for PostgREST and Storage APIs.

use reqwest::Client;
use uuid::Uuid;

use super::types::{DbRun, DbTask, DbEvent, DbChunk, DbTaskOutcome, SearchResult, ModelStats};

/// Supabase client for database and storage operations.
pub struct SupabaseClient {
    client: Client,
    url: String,
    service_role_key: String,
}

impl SupabaseClient {
    /// Create a new Supabase client.
    pub fn new(url: &str, service_role_key: &str) -> Self {
        Self {
            client: Client::new(),
            url: url.trim_end_matches('/').to_string(),
            service_role_key: service_role_key.to_string(),
        }
    }
    
    /// Get the PostgREST URL.
    fn rest_url(&self) -> String {
        format!("{}/rest/v1", self.url)
    }
    
    /// Get the Storage URL.
    fn storage_url(&self) -> String {
        format!("{}/storage/v1", self.url)
    }
    
    // ==================== Runs ====================
    
    /// Create a new run.
    pub async fn create_run(&self, input_text: &str) -> anyhow::Result<DbRun> {
        let body = serde_json::json!({
            "input_text": input_text,
            "status": "pending"
        });
        
        let resp = self.client
            .post(format!("{}/runs", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to create run: {} - {}", status, text);
        }
        
        let runs: Vec<DbRun> = serde_json::from_str(&text)?;
        runs.into_iter().next().ok_or_else(|| anyhow::anyhow!("No run returned"))
    }
    
    /// Update a run.
    pub async fn update_run(&self, id: Uuid, updates: serde_json::Value) -> anyhow::Result<()> {
        let resp = self.client
            .patch(format!("{}/runs?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&updates)
            .send()
            .await?;
        
        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update run: {}", text);
        }
        
        Ok(())
    }
    
    /// Get a run by ID.
    pub async fn get_run(&self, id: Uuid) -> anyhow::Result<Option<DbRun>> {
        let resp = self.client
            .get(format!("{}/runs?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;
        
        let runs: Vec<DbRun> = resp.json().await?;
        Ok(runs.into_iter().next())
    }
    
    /// List runs with pagination.
    pub async fn list_runs(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<DbRun>> {
        let resp = self.client
            .get(format!(
                "{}/runs?order=created_at.desc&limit={}&offset={}",
                self.rest_url(), limit, offset
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;
        
        Ok(resp.json().await?)
    }
    
    // ==================== Tasks ====================
    
    /// Create a task.
    pub async fn create_task(&self, task: &DbTask) -> anyhow::Result<DbTask> {
        let resp = self.client
            .post(format!("{}/tasks", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(task)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to create task: {} - {}", status, text);
        }
        
        let tasks: Vec<DbTask> = serde_json::from_str(&text)?;
        tasks.into_iter().next().ok_or_else(|| anyhow::anyhow!("No task returned"))
    }
    
    /// Update a task.
    pub async fn update_task(&self, id: Uuid, updates: serde_json::Value) -> anyhow::Result<()> {
        let resp = self.client
            .patch(format!("{}/tasks?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&updates)
            .send()
            .await?;
        
        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update task: {}", text);
        }
        
        Ok(())
    }
    
    /// Get tasks for a run.
    pub async fn get_tasks_for_run(&self, run_id: Uuid) -> anyhow::Result<Vec<DbTask>> {
        let resp = self.client
            .get(format!(
                "{}/tasks?run_id=eq.{}&order=depth,seq",
                self.rest_url(), run_id
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;
        
        Ok(resp.json().await?)
    }
    
    // ==================== Events ====================
    
    /// Insert an event.
    pub async fn insert_event(&self, event: &DbEvent) -> anyhow::Result<i64> {
        let resp = self.client
            .post(format!("{}/events", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(event)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to insert event: {} - {}", status, text);
        }
        
        let events: Vec<DbEvent> = serde_json::from_str(&text)?;
        events.into_iter().next()
            .and_then(|e| e.id)
            .ok_or_else(|| anyhow::anyhow!("No event ID returned"))
    }
    
    /// Get events for a run.
    pub async fn get_events_for_run(&self, run_id: Uuid, limit: Option<usize>) -> anyhow::Result<Vec<DbEvent>> {
        let limit_str = limit.map(|l| format!("&limit={}", l)).unwrap_or_default();
        let resp = self.client
            .get(format!(
                "{}/events?run_id=eq.{}&order=seq{}",
                self.rest_url(), run_id, limit_str
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;
        
        Ok(resp.json().await?)
    }
    
    // ==================== Chunks ====================
    
    /// Insert a chunk with embedding.
    pub async fn insert_chunk(&self, chunk: &DbChunk, embedding: &[f32]) -> anyhow::Result<Uuid> {
        // Format embedding as Postgres array literal
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        );
        
        let body = serde_json::json!({
            "run_id": chunk.run_id,
            "task_id": chunk.task_id,
            "source_event_id": chunk.source_event_id,
            "chunk_text": chunk.chunk_text,
            "embedding": embedding_str,
            "meta": chunk.meta
        });
        
        let resp = self.client
            .post(format!("{}/chunks", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to insert chunk: {} - {}", status, text);
        }
        
        let chunks: Vec<DbChunk> = serde_json::from_str(&text)?;
        chunks.into_iter().next()
            .and_then(|c| c.id)
            .ok_or_else(|| anyhow::anyhow!("No chunk ID returned"))
    }
    
    /// Search chunks by embedding similarity.
    pub async fn search_chunks(
        &self,
        embedding: &[f32],
        threshold: f64,
        limit: usize,
        filter_run_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        );
        
        let body = serde_json::json!({
            "query_embedding": embedding_str,
            "match_threshold": threshold,
            "match_count": limit,
            "filter_run_id": filter_run_id
        });
        
        let resp = self.client
            .post(format!("{}/rpc/search_chunks", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to search chunks: {} - {}", status, text);
        }
        
        Ok(serde_json::from_str(&text)?)
    }
    
    // ==================== Storage ====================
    
    /// Upload a file to storage.
    pub async fn upload_file(
        &self,
        bucket: &str,
        path: &str,
        content: &[u8],
        content_type: &str,
    ) -> anyhow::Result<String> {
        let resp = self.client
            .post(format!("{}/object/{}/{}", self.storage_url(), bucket, path))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", content_type)
            .body(content.to_vec())
            .send()
            .await?;
        
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to upload file: {} - {}", status, text);
        }
        
        Ok(format!("{}/{}", bucket, path))
    }
    
    /// Download a file from storage.
    pub async fn download_file(&self, bucket: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.client
            .get(format!("{}/object/{}/{}", self.storage_url(), bucket, path))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;
        
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to download file: {} - {}", status, text);
        }
        
        Ok(resp.bytes().await?.to_vec())
    }
    
    /// Update run with summary embedding.
    pub async fn update_run_summary(
        &self,
        run_id: Uuid,
        summary_text: &str,
        embedding: &[f32],
    ) -> anyhow::Result<()> {
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        );
        
        let body = serde_json::json!({
            "summary_text": summary_text,
            "summary_embedding": embedding_str
        });
        
        self.update_run(run_id, body).await
    }
    
    // ==================== Task Outcomes (Learning) ====================
    
    /// Insert a task outcome for learning.
    pub async fn insert_task_outcome(
        &self,
        outcome: &DbTaskOutcome,
        embedding: Option<&[f32]>,
    ) -> anyhow::Result<Uuid> {
        let embedding_str = embedding.map(|e| format!(
            "[{}]",
            e.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        ));
        
        let body = serde_json::json!({
            "run_id": outcome.run_id,
            "task_id": outcome.task_id,
            "predicted_complexity": outcome.predicted_complexity,
            "predicted_tokens": outcome.predicted_tokens,
            "predicted_cost_cents": outcome.predicted_cost_cents,
            "selected_model": outcome.selected_model,
            "actual_tokens": outcome.actual_tokens,
            "actual_cost_cents": outcome.actual_cost_cents,
            "success": outcome.success,
            "iterations": outcome.iterations,
            "tool_calls_count": outcome.tool_calls_count,
            "task_description": outcome.task_description,
            "task_type": outcome.task_type,
            "cost_error_ratio": outcome.cost_error_ratio,
            "token_error_ratio": outcome.token_error_ratio,
            "task_embedding": embedding_str
        });
        
        let resp = self.client
            .post(format!("{}/task_outcomes", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            anyhow::bail!("Failed to insert task outcome: {} - {}", status, text);
        }
        
        let outcomes: Vec<DbTaskOutcome> = serde_json::from_str(&text)?;
        outcomes.into_iter().next()
            .and_then(|o| o.id)
            .ok_or_else(|| anyhow::anyhow!("No outcome ID returned"))
    }
    
    /// Get model statistics for a complexity range.
    /// 
    /// Returns aggregated stats for each model that has been used
    /// for tasks in the given complexity range.
    pub async fn get_model_stats(
        &self,
        complexity_min: f64,
        complexity_max: f64,
    ) -> anyhow::Result<Vec<ModelStats>> {
        let body = serde_json::json!({
            "complexity_min": complexity_min,
            "complexity_max": complexity_max
        });
        
        let resp = self.client
            .post(format!("{}/rpc/get_model_stats", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            // If RPC doesn't exist yet, return empty
            tracing::debug!("get_model_stats RPC not available: {}", text);
            return Ok(vec![]);
        }
        
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }
    
    /// Search for similar task outcomes by embedding similarity.
    pub async fn search_similar_outcomes(
        &self,
        embedding: &[f32],
        threshold: f64,
        limit: usize,
    ) -> anyhow::Result<Vec<DbTaskOutcome>> {
        let embedding_str = format!(
            "[{}]",
            embedding.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        );
        
        let body = serde_json::json!({
            "query_embedding": embedding_str,
            "match_threshold": threshold,
            "match_count": limit
        });
        
        let resp = self.client
            .post(format!("{}/rpc/search_similar_outcomes", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            // If RPC doesn't exist yet, return empty
            tracing::debug!("search_similar_outcomes RPC not available: {}", text);
            return Ok(vec![]);
        }
        
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }
    
    /// Get global learning statistics (for tuning).
    pub async fn get_global_stats(&self) -> anyhow::Result<serde_json::Value> {
        let resp = self.client
            .post(format!("{}/rpc/get_global_learning_stats", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({}))
            .send()
            .await?;
        
        let status = resp.status();
        let text = resp.text().await?;
        
        if !status.is_success() {
            tracing::debug!("get_global_learning_stats RPC not available: {}", text);
            return Ok(serde_json::json!({}));
        }
        
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }
}

