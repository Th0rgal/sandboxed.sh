//! MCP types and data structures.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Transport type for MCP server communication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// HTTP JSON-RPC transport (server must be running and listening)
    Http {
        endpoint: String,
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
    },
    /// Stdio transport (spawn process, communicate via stdin/stdout)
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
    },
}

impl Default for McpTransport {
    fn default() -> Self {
        McpTransport::Http {
            endpoint: "http://127.0.0.1:3000".to_string(),
            headers: std::collections::HashMap::new(),
        }
    }
}

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

/// Scope for MCP servers (global or workspace-scoped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    Global,
    Workspace,
}

impl Default for McpScope {
    fn default() -> Self {
        Self::Global
    }
}

// ==================== JSON-RPC 2.0 Types ====================

/// JSON-RPC 2.0 request
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 response
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// MCP Initialize request params
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

/// Client capabilities for MCP
#[derive(Debug, Clone, Serialize, Default)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<serde_json::Value>,
}

/// Roots capability
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    pub list_changed: bool,
}

/// Client info for MCP
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// MCP Initialize response result
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: ServerCapabilities,
    #[serde(default)]
    pub server_info: Option<ServerInfo>,
}

/// Server capabilities from MCP
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
    #[serde(default)]
    pub resources: Option<serde_json::Value>,
    #[serde(default)]
    pub prompts: Option<serde_json::Value>,
}

/// Server info from MCP
#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique identifier
    pub id: Uuid,
    /// Human-readable name (e.g., "GitHub", "Browser Extension")
    pub name: String,
    /// Transport configuration (HTTP or stdio)
    pub transport: McpTransport,
    /// Scope for this MCP (global or workspace-scoped)
    #[serde(default)]
    pub scope: McpScope,
    /// Optional description
    pub description: Option<String>,
    /// Whether this MCP is enabled
    pub enabled: bool,
    /// Whether this MCP is included by default in new workspaces.
    /// When a workspace has an empty `mcps` list, only MCPs with
    /// `default_enabled = true` are written into its config.
    #[serde(default)]
    pub default_enabled: bool,
    /// Optional version string
    pub version: Option<String>,
    /// Tool names exposed by this MCP (populated after connection)
    #[serde(default)]
    pub tools: Vec<String>,
    /// Tool descriptors with full metadata (name, description, schema)
    #[serde(default)]
    pub tool_descriptors: Vec<McpToolDescriptor>,
    /// When this MCP was added
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last time we successfully connected
    pub last_connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl McpServerConfig {
    /// Create a new MCP server configuration with HTTP transport.
    pub fn new(name: String, endpoint: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            transport: McpTransport::Http {
                endpoint,
                headers: std::collections::HashMap::new(),
            },
            scope: McpScope::Global,
            description: None,
            enabled: true,
            default_enabled: false,
            version: None,
            tools: Vec::new(),
            tool_descriptors: Vec::new(),
            created_at: chrono::Utc::now(),
            last_connected_at: None,
        }
    }

    /// Create a new MCP server configuration with stdio transport.
    pub fn new_stdio(
        name: String,
        command: String,
        args: Vec<String>,
        env: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            transport: McpTransport::Stdio { command, args, env },
            scope: McpScope::Global,
            description: None,
            enabled: true,
            default_enabled: false,
            version: None,
            tools: Vec::new(),
            tool_descriptors: Vec::new(),
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
    /// Transport configuration
    pub transport: McpTransport,
    pub description: Option<String>,
    #[serde(default)]
    pub scope: Option<McpScope>,
    /// Whether this MCP is included by default in new workspaces.
    #[serde(default)]
    pub default_enabled: Option<bool>,
}

/// Request to update an MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMcpRequest {
    pub name: Option<String>,
    pub transport: Option<McpTransport>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub scope: Option<McpScope>,
    /// Whether this MCP is included by default in new workspaces.
    pub default_enabled: Option<bool>,
}

/// MCP tool list response from server.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolsResponse {
    pub tools: Vec<McpToolDescriptor>,
}

/// Tool descriptor from MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[serde(rename_all = "camelCase")]
pub struct McpCallToolResponse {
    pub content: Vec<McpContent>,
    #[serde(default)]
    pub is_error: bool,
}

/// Content item from MCP response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}
