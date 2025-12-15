//! Memory writer for persisting events and chunks.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use uuid::Uuid;
use serde_json::json;

use super::supabase::SupabaseClient;
use super::embed::EmbeddingClient;
use super::types::{DbTask, DbEvent, DbChunk, DbTaskOutcome, EventKind, MemoryStatus};

/// Maximum chunk size in characters.
const MAX_CHUNK_SIZE: usize = 2000;

/// Minimum chunk size to avoid tiny chunks.
const MIN_CHUNK_SIZE: usize = 100;

/// Writer for persisting events and creating retrieval chunks.
pub struct MemoryWriter {
    supabase: Arc<SupabaseClient>,
    embedder: Arc<EmbeddingClient>,
}

impl MemoryWriter {
    /// Create a new memory writer.
    pub fn new(supabase: Arc<SupabaseClient>, embedder: Arc<EmbeddingClient>) -> Self {
        Self { supabase, embedder }
    }
    
    /// Create a new run and return its ID.
    pub async fn create_run(&self, input_text: &str) -> anyhow::Result<Uuid> {
        let run = self.supabase.create_run(input_text).await?;
        Ok(run.id)
    }
    
    /// Update run status.
    pub async fn update_run_status(&self, run_id: Uuid, status: MemoryStatus) -> anyhow::Result<()> {
        self.supabase.update_run(run_id, json!({ "status": status.to_string() })).await
    }
    
    /// Complete a run with final output.
    pub async fn complete_run(
        &self,
        run_id: Uuid,
        final_output: &str,
        total_cost_cents: i32,
        success: bool,
    ) -> anyhow::Result<()> {
        let status = if success { MemoryStatus::Completed } else { MemoryStatus::Failed };
        
        self.supabase.update_run(run_id, json!({
            "status": status.to_string(),
            "final_output": final_output,
            "total_cost_cents": total_cost_cents,
            "updated_at": chrono::Utc::now().to_rfc3339()
        })).await
    }
    
    /// Create a task.
    pub async fn create_task(
        &self,
        run_id: Uuid,
        parent_id: Option<Uuid>,
        depth: i32,
        seq: i32,
        description: &str,
    ) -> anyhow::Result<Uuid> {
        let task = DbTask {
            id: Uuid::new_v4(),
            run_id,
            parent_id,
            depth,
            seq,
            description: description.to_string(),
            status: "pending".to_string(),
            complexity_score: None,
            model_used: None,
            budget_cents: None,
            spent_cents: None,
            output: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
        };
        
        let created = self.supabase.create_task(&task).await?;
        Ok(created.id)
    }
    
    /// Update task with completion info.
    pub async fn complete_task(
        &self,
        task_id: Uuid,
        output: &str,
        spent_cents: i32,
        success: bool,
    ) -> anyhow::Result<()> {
        let status = if success { "completed" } else { "failed" };
        
        self.supabase.update_task(task_id, json!({
            "status": status,
            "output": output,
            "spent_cents": spent_cents,
            "completed_at": chrono::Utc::now().to_rfc3339()
        })).await
    }
    
    /// Update task metadata.
    pub async fn update_task_metadata(
        &self,
        task_id: Uuid,
        complexity_score: Option<f64>,
        model_used: Option<&str>,
        budget_cents: Option<i32>,
    ) -> anyhow::Result<()> {
        let mut updates = json!({});
        
        if let Some(score) = complexity_score {
            updates["complexity_score"] = json!(score);
        }
        if let Some(model) = model_used {
            updates["model_used"] = json!(model);
        }
        if let Some(budget) = budget_cents {
            updates["budget_cents"] = json!(budget);
        }
        
        self.supabase.update_task(task_id, updates).await
    }
    
    /// Record an event.
    pub async fn record_event(&self, recorder: &EventRecorder, event: RecordedEvent) -> anyhow::Result<i64> {
        let seq = recorder.next_seq();
        
        let db_event = DbEvent {
            id: None,
            run_id: recorder.run_id,
            task_id: event.task_id,
            seq,
            ts: None,
            agent_type: event.agent_type,
            event_kind: event.kind.to_string(),
            preview_text: event.preview_text.clone(),
            meta: event.meta,
            blob_path: None,
            prompt_tokens: event.prompt_tokens,
            completion_tokens: event.completion_tokens,
            cost_cents: event.cost_cents,
        };
        
        let event_id = self.supabase.insert_event(&db_event).await?;
        
        // Create chunk if preview text is substantial
        if let Some(ref text) = event.preview_text {
            if text.len() >= MIN_CHUNK_SIZE {
                self.create_chunks_for_text(
                    recorder.run_id,
                    event.task_id,
                    Some(event_id),
                    text,
                    event.chunk_meta,
                ).await?;
            }
        }
        
        Ok(event_id)
    }
    
    /// Create chunks for a text and store with embeddings.
    pub async fn create_chunks_for_text(
        &self,
        run_id: Uuid,
        task_id: Option<Uuid>,
        source_event_id: Option<i64>,
        text: &str,
        meta: Option<serde_json::Value>,
    ) -> anyhow::Result<Vec<Uuid>> {
        let chunks = self.chunk_text(text);
        let mut chunk_ids = Vec::new();
        
        for chunk_text in chunks {
            // Generate embedding
            let embedding = self.embedder.embed(&chunk_text).await?;
            
            let chunk = DbChunk {
                id: None,
                run_id,
                task_id,
                source_event_id,
                chunk_text,
                meta: meta.clone(),
            };
            
            let id = self.supabase.insert_chunk(&chunk, &embedding).await?;
            chunk_ids.push(id);
        }
        
        Ok(chunk_ids)
    }
    
    /// Upload blob to storage and return path.
    pub async fn upload_blob(
        &self,
        run_id: Uuid,
        filename: &str,
        content: &[u8],
        content_type: &str,
    ) -> anyhow::Result<String> {
        let path = format!("{}/{}", run_id, filename);
        self.supabase.upload_file("runs-archive", &path, content, content_type).await
    }
    
    /// Archive a completed run to storage.
    pub async fn archive_run(&self, run_id: Uuid) -> anyhow::Result<String> {
        // Fetch all events
        let events = self.supabase.get_events_for_run(run_id, None).await?;
        
        // Serialize to JSONL
        let mut jsonl = String::new();
        for event in &events {
            jsonl.push_str(&serde_json::to_string(event)?);
            jsonl.push('\n');
        }
        
        // Upload
        let path = self.upload_blob(
            run_id,
            "events.jsonl",
            jsonl.as_bytes(),
            "application/x-ndjson",
        ).await?;
        
        // Update run with archive path
        self.supabase.update_run(run_id, json!({ "archive_path": path.clone() })).await?;
        
        Ok(path)
    }
    
    /// Generate and store a summary for a run.
    pub async fn store_run_summary(&self, run_id: Uuid, summary: &str) -> anyhow::Result<()> {
        let embedding = self.embedder.embed(summary).await?;
        self.supabase.update_run_summary(run_id, summary, &embedding).await
    }
    
    /// Record a task outcome for learning.
    /// 
    /// This captures predictions vs actuals to enable data-driven optimization
    /// of complexity estimation, model selection, and budget allocation.
    pub async fn record_task_outcome(
        &self,
        run_id: Uuid,
        task_id: Uuid,
        task_description: &str,
        predicted_complexity: Option<f64>,
        predicted_tokens: Option<i64>,
        predicted_cost_cents: Option<i64>,
        selected_model: Option<String>,
        actual_tokens: Option<i64>,
        actual_cost_cents: Option<i64>,
        success: bool,
        iterations: Option<i32>,
        tool_calls_count: Option<i32>,
    ) -> anyhow::Result<Uuid> {
        // Create the outcome record
        let outcome = DbTaskOutcome::new(
            run_id,
            task_id,
            task_description.to_string(),
            predicted_complexity,
            predicted_tokens,
            predicted_cost_cents,
            selected_model,
            actual_tokens,
            actual_cost_cents,
            success,
            iterations,
            tool_calls_count,
        );
        
        // Generate embedding for similarity search
        let embedding = self.embedder.embed(task_description).await.ok();
        
        self.supabase.insert_task_outcome(&outcome, embedding.as_deref()).await
    }
    
    /// Split text into chunks.
    fn chunk_text(&self, text: &str) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        
        for line in text.lines() {
            if current.len() + line.len() + 1 > MAX_CHUNK_SIZE {
                if !current.is_empty() {
                    chunks.push(current.trim().to_string());
                    current = String::new();
                }
                
                // Handle very long lines
                if line.len() > MAX_CHUNK_SIZE {
                    for chunk in line.as_bytes().chunks(MAX_CHUNK_SIZE) {
                        if let Ok(s) = std::str::from_utf8(chunk) {
                            chunks.push(s.to_string());
                        }
                    }
                } else {
                    current = line.to_string();
                }
            } else {
                if !current.is_empty() {
                    current.push('\n');
                }
                current.push_str(line);
            }
        }
        
        if !current.is_empty() && current.len() >= MIN_CHUNK_SIZE {
            chunks.push(current.trim().to_string());
        }
        
        chunks
    }
}

/// Recorder for tracking events during a run.
pub struct EventRecorder {
    pub run_id: Uuid,
    seq_counter: AtomicI32,
}

impl EventRecorder {
    /// Create a new event recorder for a run.
    pub fn new(run_id: Uuid) -> Self {
        Self {
            run_id,
            seq_counter: AtomicI32::new(0),
        }
    }
    
    /// Get the next sequence number.
    pub fn next_seq(&self) -> i32 {
        self.seq_counter.fetch_add(1, Ordering::SeqCst)
    }
    
    /// Get current sequence number without incrementing.
    pub fn current_seq(&self) -> i32 {
        self.seq_counter.load(Ordering::SeqCst)
    }
}

/// An event to be recorded.
pub struct RecordedEvent {
    pub task_id: Option<Uuid>,
    pub agent_type: String,
    pub kind: EventKind,
    pub preview_text: Option<String>,
    pub meta: Option<serde_json::Value>,
    pub chunk_meta: Option<serde_json::Value>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub cost_cents: Option<i32>,
}

impl RecordedEvent {
    /// Create a simple event.
    pub fn new(agent_type: &str, kind: EventKind) -> Self {
        Self {
            task_id: None,
            agent_type: agent_type.to_string(),
            kind,
            preview_text: None,
            meta: None,
            chunk_meta: None,
            prompt_tokens: None,
            completion_tokens: None,
            cost_cents: None,
        }
    }
    
    /// Set task ID.
    pub fn with_task(mut self, task_id: Uuid) -> Self {
        self.task_id = Some(task_id);
        self
    }
    
    /// Set preview text.
    pub fn with_preview(mut self, text: impl Into<String>) -> Self {
        self.preview_text = Some(text.into());
        self
    }
    
    /// Set metadata.
    pub fn with_meta(mut self, meta: serde_json::Value) -> Self {
        self.meta = Some(meta);
        self
    }
    
    /// Set token usage.
    pub fn with_tokens(mut self, prompt: i32, completion: i32, cost_cents: i32) -> Self {
        self.prompt_tokens = Some(prompt);
        self.completion_tokens = Some(completion);
        self.cost_cents = Some(cost_cents);
        self
    }
}

