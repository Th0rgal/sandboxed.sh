//! Supabase client for PostgREST and Storage APIs.

use reqwest::Client;
use uuid::Uuid;

use super::types::{
    DbChunk, DbEvent, DbMission, DbRun, DbTask, DbTaskOutcome, MissionMessage, MissionSummary,
    ModelStats, SearchResult, UserFact,
};

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

        let resp = self
            .client
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
        runs.into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No run returned"))
    }

    /// Update a run.
    pub async fn update_run(&self, id: Uuid, updates: serde_json::Value) -> anyhow::Result<()> {
        let resp = self
            .client
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
        let resp = self
            .client
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
        let resp = self
            .client
            .get(format!(
                "{}/runs?order=created_at.desc&limit={}&offset={}",
                self.rest_url(),
                limit,
                offset
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    /// Get total cost across all runs (in cents).
    pub async fn get_total_cost_cents(&self) -> anyhow::Result<u64> {
        // Fetch only the total_cost_cents column for efficiency
        #[derive(serde::Deserialize)]
        struct CostOnly {
            total_cost_cents: Option<i64>,
        }

        let resp = self
            .client
            .get(format!("{}/runs?select=total_cost_cents", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        let costs: Vec<CostOnly> = resp.json().await?;
        let total: i64 = costs.iter().filter_map(|c| c.total_cost_cents).sum();

        Ok(total.max(0) as u64)
    }

    // ==================== Tasks ====================

    /// Create a task.
    pub async fn create_task(&self, task: &DbTask) -> anyhow::Result<DbTask> {
        let resp = self
            .client
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
        tasks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No task returned"))
    }

    /// Update a task.
    pub async fn update_task(&self, id: Uuid, updates: serde_json::Value) -> anyhow::Result<()> {
        let resp = self
            .client
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
        let resp = self
            .client
            .get(format!(
                "{}/tasks?run_id=eq.{}&order=depth,seq",
                self.rest_url(),
                run_id
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
        let resp = self
            .client
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
        events
            .into_iter()
            .next()
            .and_then(|e| e.id)
            .ok_or_else(|| anyhow::anyhow!("No event ID returned"))
    }

    /// Get events for a run.
    pub async fn get_events_for_run(
        &self,
        run_id: Uuid,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<DbEvent>> {
        let limit_str = limit.map(|l| format!("&limit={}", l)).unwrap_or_default();
        let resp = self
            .client
            .get(format!(
                "{}/events?run_id=eq.{}&order=seq{}",
                self.rest_url(),
                run_id,
                limit_str
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
            embedding
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let body = serde_json::json!({
            "run_id": chunk.run_id,
            "task_id": chunk.task_id,
            "source_event_id": chunk.source_event_id,
            "chunk_text": chunk.chunk_text,
            "embedding": embedding_str,
            "meta": chunk.meta
        });

        let resp = self
            .client
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
        chunks
            .into_iter()
            .next()
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
            embedding
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let body = serde_json::json!({
            "query_embedding": embedding_str,
            "match_threshold": threshold,
            "match_count": limit,
            "filter_run_id": filter_run_id
        });

        let resp = self
            .client
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
        let resp = self
            .client
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
        let resp = self
            .client
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
            embedding
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
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
        let embedding_str = embedding.map(|e| {
            format!(
                "[{}]",
                e.iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });

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

        let resp = self
            .client
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
        outcomes
            .into_iter()
            .next()
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

        let resp = self
            .client
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
            embedding
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let body = serde_json::json!({
            "query_embedding": embedding_str,
            "match_threshold": threshold,
            "match_count": limit
        });

        let resp = self
            .client
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
        let resp = self
            .client
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

    // ==================== Missions ====================

    /// Create a new mission.
    pub async fn create_mission(
        &self,
        title: Option<&str>,
        model_override: Option<&str>,
        workspace_id: Option<Uuid>,
        agent_id: Option<Uuid>,
        hooks: Option<Vec<String>>,
    ) -> anyhow::Result<DbMission> {
        let mut body = serde_json::json!({
            "title": title,
            "status": "active",
            "history": []
        });

        // Add model_override if provided (column may not exist in older schemas)
        if let Some(model) = model_override {
            body["model_override"] = serde_json::Value::String(model.to_string());
        }

        // Add workspace_id if provided
        if let Some(ws_id) = workspace_id {
            body["workspace_id"] = serde_json::Value::String(ws_id.to_string());
        }

        // Add agent_id if provided
        if let Some(a_id) = agent_id {
            body["agent_id"] = serde_json::Value::String(a_id.to_string());
        }

        // Add hooks if provided
        if let Some(h) = hooks {
            body["hooks"] = serde_json::json!(h);
        }

        let resp = self
            .client
            .post(format!("{}/missions", self.rest_url()))
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
            anyhow::bail!("Failed to create mission: {} - {}", status, text);
        }

        let missions: Vec<DbMission> = serde_json::from_str(&text)?;
        missions
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No mission returned"))
    }

    /// Get a mission by ID.
    pub async fn get_mission(&self, id: Uuid) -> anyhow::Result<Option<DbMission>> {
        let resp = self
            .client
            .get(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        let missions: Vec<DbMission> = resp.json().await?;
        Ok(missions.into_iter().next())
    }

    /// List missions with pagination.
    pub async fn list_missions(
        &self,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<DbMission>> {
        let resp = self
            .client
            .get(format!(
                "{}/missions?order=updated_at.desc&limit={}&offset={}",
                self.rest_url(),
                limit,
                offset
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    /// Get active missions that haven't been updated in the specified hours.
    pub async fn get_stale_active_missions(
        &self,
        stale_hours: u64,
    ) -> anyhow::Result<Vec<DbMission>> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(stale_hours as i64);
        let cutoff_str = cutoff.to_rfc3339();

        let resp = self
            .client
            .get(format!(
                "{}/missions?status=eq.active&updated_at=lt.{}",
                self.rest_url(),
                cutoff_str
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    /// Update mission status.
    pub async fn update_mission_status(&self, id: Uuid, status: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "status": status,
            "updated_at": chrono::Utc::now().to_rfc3339()
        });

        let resp = self
            .client
            .patch(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update mission status: {}", text);
        }

        Ok(())
    }

    /// Update mission history.
    pub async fn update_mission_history(
        &self,
        id: Uuid,
        history: &[MissionMessage],
    ) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "history": history,
            "updated_at": chrono::Utc::now().to_rfc3339()
        });

        let resp = self
            .client
            .patch(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update mission history: {}", text);
        }

        Ok(())
    }

    /// Update mission title.
    pub async fn update_mission_title(&self, id: Uuid, title: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "title": title,
            "updated_at": chrono::Utc::now().to_rfc3339()
        });

        let resp = self
            .client
            .patch(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update mission title: {}", text);
        }

        Ok(())
    }

    /// Save the final agent tree for a mission (when it completes).
    pub async fn update_mission_tree(
        &self,
        id: Uuid,
        tree: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "final_tree": tree,
            "updated_at": chrono::Utc::now().to_rfc3339()
        });

        let resp = self
            .client
            .patch(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to update mission tree: {}", text);
        }

        Ok(())
    }

    /// Delete a mission by ID.
    /// Returns true if the mission was deleted, false if it didn't exist.
    pub async fn delete_mission(&self, id: Uuid) -> anyhow::Result<bool> {
        let resp = self
            .client
            .delete(format!("{}/missions?id=eq.{}", self.rest_url(), id))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .header("Prefer", "return=representation")
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to delete mission: {}", text);
        }

        // Check if anything was actually deleted
        let deleted: Vec<DbMission> = resp.json().await?;
        Ok(!deleted.is_empty())
    }

    /// Delete all empty "Untitled" missions (no history, no title set).
    /// Returns the count of deleted missions.
    pub async fn delete_empty_untitled_missions(&self) -> anyhow::Result<usize> {
        self.delete_empty_untitled_missions_excluding(&[]).await
    }

    /// Delete all empty "Untitled" missions (no history, no title set),
    /// excluding the specified mission IDs (e.g., currently running missions).
    /// Returns the count of deleted missions.
    pub async fn delete_empty_untitled_missions_excluding(
        &self,
        exclude_ids: &[Uuid],
    ) -> anyhow::Result<usize> {
        // Minimal struct for partial field selection - avoids deserialization errors
        // when querying only id, title, history fields (DbMission has more required fields)
        #[derive(serde::Deserialize)]
        struct PartialMission {
            id: Uuid,
            #[allow(dead_code)]
            title: Option<String>,
            history: serde_json::Value,
        }

        // First get missions with null or "Untitled Mission" title and empty history
        let resp = self.client
            .get(format!(
                "{}/missions?select=id,title,history&or=(title.is.null,title.eq.Untitled%20Mission)&status=eq.active",
                self.rest_url()
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to query empty missions: {}", text);
        }

        let missions: Vec<PartialMission> = resp.json().await?;

        // Filter to only those with empty history (history is a JSON array)
        // and not in the exclude list (e.g., currently running missions)
        let empty_ids: Vec<Uuid> = missions
            .into_iter()
            .filter(|m| m.history.as_array().map_or(true, |arr| arr.is_empty()))
            .filter(|m| !exclude_ids.contains(&m.id))
            .map(|m| m.id)
            .collect();

        if empty_ids.is_empty() {
            return Ok(0);
        }

        // Delete in batches
        let mut deleted_count = 0;
        for id in &empty_ids {
            if self.delete_mission(*id).await? {
                deleted_count += 1;
            }
        }

        Ok(deleted_count)
    }

    // ==================== User Facts ====================

    /// Insert a user fact into memory.
    pub async fn insert_user_fact(
        &self,
        fact_text: &str,
        category: Option<&str>,
        embedding: Option<&[f32]>,
        source_mission_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<uuid::Uuid> {
        let embedding_str = embedding.map(|e| {
            format!(
                "[{}]",
                e.iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });

        let body = serde_json::json!({
            "fact_text": fact_text,
            "category": category,
            "embedding": embedding_str,
            "source_mission_id": source_mission_id
        });

        let resp = self
            .client
            .post(format!("{}/user_facts", self.rest_url()))
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
            anyhow::bail!("Failed to insert user fact: {} - {}", status, text);
        }

        let facts: Vec<UserFact> = serde_json::from_str(&text)?;
        facts
            .into_iter()
            .next()
            .and_then(|f| f.id)
            .ok_or_else(|| anyhow::anyhow!("No fact ID returned"))
    }

    /// Search user facts by text (simple contains search).
    pub async fn search_user_facts(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserFact>> {
        // Use ilike for case-insensitive search
        let resp = self
            .client
            .get(format!(
                "{}/user_facts?fact_text=ilike.*{}*&order=created_at.desc&limit={}",
                self.rest_url(),
                urlencoding::encode(query),
                limit
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to search user facts: {}", text);
        }

        Ok(resp.json().await?)
    }

    /// Get all user facts (for injection into prompts).
    pub async fn get_all_user_facts(&self, limit: usize) -> anyhow::Result<Vec<UserFact>> {
        let resp = self
            .client
            .get(format!(
                "{}/user_facts?order=created_at.desc&limit={}",
                self.rest_url(),
                limit
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to get user facts: {}", text);
        }

        Ok(resp.json().await?)
    }

    // ==================== Mission Summaries ====================

    /// Insert a mission summary.
    pub async fn insert_mission_summary(
        &self,
        mission_id: uuid::Uuid,
        summary: &str,
        key_files: &[String],
        tools_used: &[String],
        success: bool,
        embedding: Option<&[f32]>,
    ) -> anyhow::Result<uuid::Uuid> {
        let embedding_str = embedding.map(|e| {
            format!(
                "[{}]",
                e.iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });

        let body = serde_json::json!({
            "mission_id": mission_id,
            "summary": summary,
            "key_files": key_files,
            "tools_used": tools_used,
            "success": success,
            "embedding": embedding_str
        });

        let resp = self
            .client
            .post(format!("{}/mission_summaries", self.rest_url()))
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
            anyhow::bail!("Failed to insert mission summary: {} - {}", status, text);
        }

        let summaries: Vec<MissionSummary> = serde_json::from_str(&text)?;
        summaries
            .into_iter()
            .next()
            .and_then(|s| s.id)
            .ok_or_else(|| anyhow::anyhow!("No summary ID returned"))
    }

    /// Search mission summaries by text.
    pub async fn search_mission_summaries(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MissionSummary>> {
        let resp = self
            .client
            .get(format!(
                "{}/mission_summaries?summary=ilike.*{}*&order=created_at.desc&limit={}",
                self.rest_url(),
                urlencoding::encode(query),
                limit
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to search mission summaries: {}", text);
        }

        Ok(resp.json().await?)
    }

    /// Get recent mission summaries (for context injection).
    pub async fn get_recent_mission_summaries(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<MissionSummary>> {
        let resp = self
            .client
            .get(format!(
                "{}/mission_summaries?order=created_at.desc&limit={}",
                self.rest_url(),
                limit
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!("Failed to get recent mission summaries: {}", text);
        }

        Ok(resp.json().await?)
    }

    /// Get mission summaries for a specific mission only.
    pub async fn get_mission_summaries_for_mission(
        &self,
        mission_id: uuid::Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<MissionSummary>> {
        let resp = self
            .client
            .get(format!(
                "{}/mission_summaries?mission_id=eq.{}&order=created_at.desc&limit={}",
                self.rest_url(),
                mission_id,
                limit
            ))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let text = resp.text().await?;
            anyhow::bail!(
                "Failed to get mission summaries for mission {}: {}",
                mission_id,
                text
            );
        }

        Ok(resp.json().await?)
    }

    // ==================== Learned Model Selection ====================

    /// Get learned model performance statistics.
    ///
    /// Fetches from the `model_performance` view which aggregates task_outcomes.
    pub async fn get_learned_model_stats(
        &self,
    ) -> anyhow::Result<Vec<crate::budget::LearnedModelStats>> {
        let resp = self
            .client
            .get(format!("{}/model_performance", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            // View might not exist yet - return empty instead of error
            if status.as_u16() == 404 || text.contains("does not exist") {
                tracing::debug!("model_performance view not found, returning empty stats");
                return Ok(vec![]);
            }
            anyhow::bail!("Failed to get learned model stats: {} - {}", status, text);
        }

        Ok(resp.json().await?)
    }

    /// Get learned budget estimates.
    ///
    /// Fetches from the `budget_estimates` view which aggregates task_outcomes.
    pub async fn get_learned_budget_estimates(
        &self,
    ) -> anyhow::Result<Vec<crate::budget::LearnedBudgetEstimate>> {
        let resp = self
            .client
            .get(format!("{}/budget_estimates", self.rest_url()))
            .header("apikey", &self.service_role_key)
            .header("Authorization", format!("Bearer {}", self.service_role_key))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            // View might not exist yet - return empty instead of error
            if status.as_u16() == 404 || text.contains("does not exist") {
                tracing::debug!("budget_estimates view not found, returning empty estimates");
                return Ok(vec![]);
            }
            anyhow::bail!(
                "Failed to get learned budget estimates: {} - {}",
                status,
                text
            );
        }

        Ok(resp.json().await?)
    }
}
