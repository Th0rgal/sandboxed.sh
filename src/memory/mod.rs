//! Memory subsystem for persistent storage and retrieval.
//!
//! This module provides:
//! - Event recording during task execution
//! - Semantic search over past runs and events
//! - Archival to Supabase Storage for long-term retention
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌──────────────────┐
//! │  EventRecorder  │────▶│   MemoryWriter   │
//! └─────────────────┘     └────────┬─────────┘
//!                                  │
//!                    ┌─────────────┼─────────────┐
//!                    ▼             ▼             ▼
//!              ┌──────────┐  ┌──────────┐  ┌──────────┐
//!              │ Postgres │  │ Storage  │  │ Embedder │
//!              │ (pgvec)  │  │ (blobs)  │  │(OpenRouter)│
//!              └──────────┘  └──────────┘  └──────────┘
//!                    ▲             ▲             │
//!                    └─────────────┼─────────────┘
//!                                  │
//!                         ┌───────┴───────┐
//!                         │MemoryRetriever│
//!                         └───────────────┘
//! ```

mod types;
mod supabase;
mod embed;
mod writer;
mod retriever;
mod context;

pub use types::*;
pub use supabase::SupabaseClient;
pub use embed::EmbeddingClient;
pub use writer::{MemoryWriter, EventRecorder, RecordedEvent};
pub use retriever::MemoryRetriever;
pub use context::{ContextBuilder, SessionContext, MemoryContext, safe_truncate_index};

use crate::config::MemoryConfig;
use std::sync::Arc;

/// Initialize the memory subsystem.
/// 
/// Returns `None` if memory is not configured (Supabase credentials missing).
pub async fn init_memory(
    config: &MemoryConfig,
    openrouter_key: &str,
) -> Option<MemorySystem> {
    if !config.is_enabled() {
        tracing::info!("Memory subsystem disabled (no Supabase config)");
        return None;
    }

    if openrouter_key.trim().is_empty() {
        tracing::warn!("Memory subsystem disabled (OPENROUTER_API_KEY not set)");
        return None;
    }
    
    let supabase = Arc::new(SupabaseClient::new(
        config.supabase_url.as_ref()?,
        config.supabase_service_role_key.as_ref()?,
    ));
    
    let embedder = Arc::new(EmbeddingClient::new(
        openrouter_key.to_string(),
        config.embed_model.clone(),
        config.embed_dimension,
    ));
    
    let writer = Arc::new(MemoryWriter::new(
        Arc::clone(&supabase),
        Arc::clone(&embedder),
    ));
    
    let retriever = Arc::new(MemoryRetriever::new(
        Arc::clone(&supabase),
        Arc::clone(&embedder),
        config.rerank_model.clone(),
        openrouter_key.to_string(),
    ));
    
    tracing::info!("Memory subsystem initialized (Supabase + {} embeddings)", config.embed_model);
    
    Some(MemorySystem {
        supabase,
        embedder,
        writer,
        retriever,
    })
}

/// The complete memory subsystem.
#[derive(Clone)]
pub struct MemorySystem {
    pub supabase: Arc<SupabaseClient>,
    pub embedder: Arc<EmbeddingClient>,
    pub writer: Arc<MemoryWriter>,
    pub retriever: Arc<MemoryRetriever>,
}

