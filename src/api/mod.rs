//! HTTP API for the Open Agent.
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
pub mod ampcode;
mod auth;
pub mod automation_variables;
pub mod backends;
pub mod claudecode;
mod console;
pub mod control;
pub mod desktop;
mod desktop_stream;
mod fs;
pub mod library;
pub mod mcp;
pub mod mission_runner;
pub mod mission_store;
mod monitoring;
pub mod opencode;
mod providers;
mod routes;
pub mod secrets;
pub mod settings;
pub mod system;
pub mod types;
pub mod workspaces;

pub use routes::serve;
pub use types::*;
