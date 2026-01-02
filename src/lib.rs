//! # Open Agent
//!
//! A minimal autonomous coding agent with full machine access.
//!
//! This library provides:
//! - An HTTP API for task submission and monitoring
//! - A simple agent architecture for direct task execution
//! - Tool-based execution for autonomous code editing
//! - Integration with OpenRouter for LLM access
//!
//! ## Architecture (v3: SimpleAgent)
//!
//! ```text
//!        ┌──────────────────────────────────┐
//!        │          SimpleAgent             │
//!        │  (direct execution, no overhead) │
//!        └────────────────┬─────────────────┘
//!                         │
//!                         ▼
//!                ┌─────────────────┐
//!                │  TaskExecutor   │
//!                │ (tool loop)     │
//!                └─────────────────┘
//! ```
//!
//! ## Task Flow
//! 1. Receive task via API
//! 2. Resolve model (user override or config default)
//! 3. Execute via TaskExecutor (tool loop)
//! 4. Return result (mission completion via complete_mission tool)
//!
//! ## Modules
//! - `agents`: SimpleAgent and TaskExecutor
//! - `task`: Task, subtask, and verification types
//! - `budget`: Cost tracking and model pricing

pub mod api;
pub mod agents;
pub mod budget;
pub mod config;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod opencode;
pub mod task;
pub mod tools;

pub use config::Config;
pub use config::MemoryConfig;
