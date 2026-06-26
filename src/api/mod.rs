//! HTTP API for sandboxed.sh.
//!
//! ## Endpoints
//!
//! - `POST /api/task` - Submit a new task
//! - `GET /api/task/{id}` - Get task status and result
//! - `GET /api/task/{id}/stream` - Stream task progress via SSE
//! - `GET /api/health` - Health check
//! - `GET /api/providers` - List available providers
//! - `GET /api/mcp` - List all MCP servers
//! - `POST /api/mcp` - Add a new MCP server
//! - `DELETE /api/mcp/{id}` - Remove an MCP server
//! - `POST /api/mcp/{id}/enable` - Enable an MCP server
//! - `POST /api/mcp/{id}/disable` - Disable an MCP server
//! - `GET /api/tools` - List all tools (built-in + MCP)
//! - `POST /api/tools/{name}/toggle` - Enable/disable a tool

pub mod ai_providers;
pub mod ask;
mod auth;
pub mod automation_variables;
pub mod backends;
pub mod claudecode;
pub mod codex_usage;
mod console;
pub mod control;
pub mod control_metrics;
pub mod deferred_proxy;
pub mod desktop;
mod desktop_stream;
pub mod durable_jobs;
pub mod fido;
mod fs;
mod github_auth;
pub mod github_integration;
pub(crate) mod grok_goal;
pub mod library;
pub mod mcp;
pub mod metadata_llm;
pub mod mission_runner;
pub mod mission_store;
pub mod mission_workspace_gc;
mod model_routing;
mod monitoring;
mod native_loop_observer;
pub mod oauth_reconcile;
pub mod opencode;
pub mod paloma;
mod provider_usage_cache;
pub(crate) mod providers;
pub(crate) mod proxy;
mod proxy_keys;
pub(crate) mod proxy_liveness;
mod routes;
pub(crate) mod runners;
pub mod secrets;
pub mod settings;
pub(crate) mod spark;
pub(crate) mod supervision;
pub mod system;
pub mod telegram;
pub mod types;
pub mod usage_optimize;
pub mod workspaces;

pub use routes::serve;
pub use types::*;
