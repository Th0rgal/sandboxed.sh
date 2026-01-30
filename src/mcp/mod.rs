//! MCP (Model Context Protocol) management module.
//!
//! Allows dynamic addition/removal of MCP servers and their tools without restarting.
//! Configurations are persisted to `{working_dir}/.sandboxed-sh/mcp/config.json`.

mod config;
mod registry;
mod types;

pub use config::McpConfigStore;
pub use registry::McpRegistry;
pub use types::*;
