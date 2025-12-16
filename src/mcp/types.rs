//! MCP types and data structures.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of an MCP server connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    /// Server is connected and responding
    Connected,
    /// Server is not reachable
    Disconnected,
    /// Connection error occurred
    Error,
    /// Server is disabled by user
    Disabled,
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique identifier
    pub id: Uuid,
    /// Human-readable name (e.g., "Supabase", "Browser Extension")
    pub name: String,
    /// Server endpoint URL (e.g., "http://127.0.0.1:4011")
    pub endpoint: String,
    /// Optional description
    pub description: Option<String>,
    /// Whether this MCP is enabled
    pub enabled: bool,
    /// Optional version string
    pub version: Option<String>,
    /// Tool names exposed by this MCP (populated after connection)
    #[serde(default)]
    pub tools: Vec<String>,
    /// When this MCP was added
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last time we successfully connected
    pub last_connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl McpServerConfig {
    /// Create a new MCP server configuration.
    pub fn new(name: String, endpoint: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            endpoint,
            description: None,
            enabled: true,
            version: None,
            tools: Vec::new(),
            created_at: chrono::Utc::now(),
            last_connected_at: None,
        }
    }
}

/// Runtime state of an MCP server (not persisted).
#[derive(Debug, Clone, Serialize)]
pub struct McpServerState {
    /// The configuration
    #[serde(flatten)]
    pub config: McpServerConfig,
    /// Current connection status
    pub status: McpStatus,
    /// Error message if status is Error
    pub error: Option<String>,
    /// Number of successful tool calls
    pub tool_calls: u64,
    /// Number of failed tool calls
    pub tool_errors: u64,
}

impl McpServerState {
    pub fn from_config(config: McpServerConfig) -> Self {
        let status = if config.enabled {
            McpStatus::Disconnected
        } else {
            McpStatus::Disabled
        };
        Self {
            config,
            status,
            error: None,
            tool_calls: 0,
            tool_errors: 0,
        }
    }
}

/// A tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// JSON schema for parameters
    pub parameters_schema: serde_json::Value,
    /// Which MCP server provides this tool
    pub mcp_id: Uuid,
    /// Whether this tool is enabled
    pub enabled: bool,
}

/// Request to add a new MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct AddMcpRequest {
    pub name: String,
    pub endpoint: String,
    pub description: Option<String>,
}

/// Request to update an MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMcpRequest {
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

/// MCP tool list response from server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolsResponse {
    pub tools: Vec<McpToolDescriptor>,
}

/// Tool descriptor from MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Request to call an MCP tool.
#[derive(Debug, Clone, Serialize)]
pub struct McpCallToolRequest {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Response from calling an MCP tool.
#[derive(Debug, Clone, Deserialize)]
pub struct McpCallToolResponse {
    pub content: Vec<McpContent>,
    #[serde(default)]
    pub isError: bool,
}

/// Content item from MCP response.
#[derive(Debug, Clone, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub mimeType: Option<String>,
}
