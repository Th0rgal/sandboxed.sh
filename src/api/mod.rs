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
//! - `GET /api/agents` - List all agent configurations
//! - `POST /api/agents` - Create a new agent configuration
//! - `GET /api/agents/{id}` - Get agent configuration details
//! - `PUT /api/agents/{id}` - Update agent configuration
//! - `DELETE /api/agents/{id}` - Delete agent configuration

pub mod agents;
pub mod ai_providers;
mod auth;
mod console;
pub mod control;
mod desktop_stream;
mod fs;
pub mod library;
pub mod mcp;
pub mod mission_runner;
pub mod opencode;
mod providers;
mod routes;
pub mod secrets;
mod ssh_util;
pub mod types;
pub mod workspaces;

pub use routes::serve;
pub use types::*;
