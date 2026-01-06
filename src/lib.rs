//! # Open Agent
//!
//! A minimal autonomous coding agent with full machine access.
//!
//! This library provides:
//! - An HTTP API for task submission and monitoring
//! - OpenCode-based agent architecture for task execution
//! - Integration with Claude Max subscriptions via OpenCode
//!
//! ## Architecture (OpenCode Backend)
//!
//! ```text
//!        ┌──────────────────────────────────┐
//!        │         OpenCodeAgent            │
//!        │  (delegates to OpenCode server)  │
//!        └────────────────┬─────────────────┘
//!                         │
//!                         ▼
//!                ┌─────────────────┐
//!                │  OpenCode       │
//!                │  Server         │
//!                └─────────────────┘
//! ```
//!
//! ## Task Flow
//! 1. Receive task via API
//! 2. Delegate to OpenCode server
//! 3. Stream real-time events (thinking, tool calls, results)
//! 4. Return result
//!
//! ## Modules
//! - `agents`: OpenCodeAgent for task delegation
//! - `task`: Task and verification types
//! - `budget`: Cost tracking and model pricing

pub mod agent_config;
pub mod agents;
pub mod api;
pub mod budget;
pub mod chroot;
pub mod config;
pub mod library;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod opencode;
pub mod task;
pub mod tools;
pub mod workspace;

pub use agent_config::{AgentConfig, AgentStore};
pub use config::Config;
pub use config::MemoryConfig;
