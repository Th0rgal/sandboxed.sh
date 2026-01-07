//! # Open Agent Panel
//!
//! Managed control plane for OpenCode-based agents.
//!
//! This library provides:
//! - HTTP APIs for missions, workspaces, MCP tooling, and library sync
//! - An OpenCode-backed agent wrapper for task delegation
//! - Streaming events for mission telemetry in the dashboards
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
//! 1. Receive mission task via API
//! 2. Delegate to OpenCode server
//! 3. Stream real-time events (thinking, tool calls, results)
//! 4. Store logs and return result
//!
//! ## Modules
//! - `agents`: OpenCodeAgent for task delegation
//! - `task`: Task definitions and lightweight cost tracking
//! - `opencode`: OpenCode API client

pub mod agent_config;
pub mod agents;
pub mod ai_providers;
pub mod api;
pub mod chroot;
pub mod config;
pub mod library;
pub mod mcp;
pub mod opencode;
pub mod opencode_config;
pub mod secrets;
pub mod task;
pub mod tools;
pub mod workspace;

pub use agent_config::{AgentConfig, AgentStore};
pub use ai_providers::{AIProvider, AIProviderStore, ProviderType};
pub use config::Config;
pub use opencode_config::{OpenCodeConnection, OpenCodeStore};
