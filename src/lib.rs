//! # Open Agent
//!
//! A minimal autonomous coding agent with full machine access.
//!
//! This library provides:
//! - An HTTP API for task submission and monitoring
//! - A hierarchical agent tree for complex task handling
//! - Tool-based execution for autonomous code editing
//! - Integration with OpenRouter for LLM access
//!
//! ## Architecture (v2: Hierarchical Agent Tree)
//!
//! ```text
//!                    ┌─────────────┐
//!                    │  RootAgent  │
//!                    └──────┬──────┘
//!         ┌─────────────────┼─────────────────┐
//!         ▼                 ▼                 ▼
//! ┌───────────────┐ ┌─────────────┐ ┌─────────────┐
//! │ Complexity    │ │   Model     │ │    Task     │
//! │ Estimator     │ │  Selector   │ │  Executor   │
//! └───────────────┘ └─────────────┘ └─────────────┘
//! ```
//!
//! ## Task Flow
//! 1. Receive task via API
//! 2. Estimate complexity (should we split?)
//! 3. Select optimal model (U-curve cost optimization)
//! 4. Execute (directly or via subtasks)
//! 5. Verify completion (programmatic + LLM hybrid)
//!
//! ## Modules
//! - `agents`: Hierarchical agent tree (Root, Node, Leaf agents)
//! - `task`: Task, subtask, and verification types
//! - `budget`: Cost tracking and model pricing

pub mod api;
pub mod agents;
pub mod budget;
pub mod config;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod task;
pub mod tools;

pub use config::Config;
pub use config::MemoryConfig;

