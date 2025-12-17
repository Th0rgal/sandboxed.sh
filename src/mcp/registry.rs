//! MCP runtime registry - manages connections and tool execution.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::config::McpConfigStore;
use super::types::*;

/// MCP protocol version we support
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Runtime registry for MCP servers.
pub struct McpRegistry {
    /// Persistent configuration store
    config_store: Arc<McpConfigStore>,
    /// Runtime state for each MCP (keyed by ID)
    states: RwLock<HashMap<Uuid, McpServerState>>,
    /// HTTP client for MCP requests
    client: reqwest::Client,
    /// Disabled tools (by name)
    disabled_tools: RwLock<std::collections::HashSet<String>>,
    /// Request ID counter for JSON-RPC
    request_id: AtomicU64,
}

impl McpRegistry {
    /// Create a new MCP registry.
    pub async fn new(working_dir: &Path) -> Self {
        let config_store = Arc::new(McpConfigStore::new(working_dir).await);
        
        // Initialize states from configs
        let configs = config_store.list().await;
        let mut states = HashMap::new();
        for config in configs {
            states.insert(config.id, McpServerState::from_config(config));
        }

        // Use very short timeouts to avoid blocking for too long
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_millis(1000))
            .build()
            .unwrap_or_default();

        Self {
            config_store,
            states: RwLock::new(states),
            client,
            disabled_tools: RwLock::new(std::collections::HashSet::new()),
            request_id: AtomicU64::new(1),
        }
    }
    
    /// Get the next request ID for JSON-RPC
    fn next_request_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }
    
    /// Send a JSON-RPC request to an MCP server
    async fn send_jsonrpc(&self, endpoint: &str, method: &str, params: Option<serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        let request = JsonRpcRequest::new(self.next_request_id(), method, params);
        
        let response = self.client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;
        
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }
        
        let json_response: JsonRpcResponse = response.json().await?;
        
        if let Some(error) = json_response.error {
            anyhow::bail!("JSON-RPC error {}: {}", error.code, error.message);
        }
        
        json_response.result.ok_or_else(|| anyhow::anyhow!("No result in response"))
    }
    
    /// Initialize connection with an MCP server
    async fn initialize_mcp(&self, endpoint: &str) -> anyhow::Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "open-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        
        let result = self.send_jsonrpc(
            endpoint, 
            "initialize", 
            Some(serde_json::to_value(params)?)
        ).await?;
        
        let init_result: InitializeResult = serde_json::from_value(result)?;
        
        // Send initialized notification (no response expected, but some servers require it)
        let _ = self.client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await;
        
        Ok(init_result)
    }

    /// List all MCP servers with their current state.
    pub async fn list(&self) -> Vec<McpServerState> {
        self.states.read().await.values().cloned().collect()
    }

    /// Get a specific MCP server state.
    pub async fn get(&self, id: Uuid) -> Option<McpServerState> {
        self.states.read().await.get(&id).cloned()
    }

    /// Add a new MCP server.
    /// Note: This does NOT automatically attempt to connect. Use refresh() after adding.
    pub async fn add(&self, req: AddMcpRequest) -> anyhow::Result<McpServerState> {
        let mut config = McpServerConfig::new(req.name, req.endpoint);
        config.description = req.description;
        
        // Save to persistent store
        let config = self.config_store.add(config).await?;
        let id = config.id;
        
        // Create runtime state
        let state = McpServerState::from_config(config.clone());
        
        // Add to states
        {
            let mut states = self.states.write().await;
            states.insert(id, state.clone());
        }
        
        // Return immediately - user should call refresh() to connect
        Ok(state)
    }

    /// Remove an MCP server.
    pub async fn remove(&self, id: Uuid) -> anyhow::Result<()> {
        // Remove from persistent store
        self.config_store.remove(id).await?;
        
        // Remove from states
        self.states.write().await.remove(&id);
        
        Ok(())
    }

    /// Enable an MCP server.
    /// Note: This does NOT automatically attempt to connect. Use refresh() after enabling.
    pub async fn enable(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        // Update persistent config
        let config = self.config_store.enable(id).await?;
        
        // Update runtime state
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&id) {
                state.config = config;
                state.status = McpStatus::Disconnected;
                state.error = None;
            }
        }
        
        self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Disable an MCP server.
    pub async fn disable(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        // Update persistent config
        let config = self.config_store.disable(id).await?;
        
        // Update runtime state
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&id) {
                state.config = config;
                state.status = McpStatus::Disabled;
                state.error = None;
            }
        }
        
        self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Helper to update state with error - retries a few times to handle lock contention
    async fn update_state_error(&self, id: Uuid, error_msg: String) {
        // Try up to 5 times with small delays to handle temporary lock contention
        for attempt in 0..5 {
            if let Ok(mut states) = self.states.try_write() {
                if let Some(state) = states.get_mut(&id) {
                    state.status = McpStatus::Error;
                    state.error = Some(error_msg);
                }
                return;
            }
            // Small delay before retry (10ms, 20ms, 40ms, 80ms, 160ms)
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(10 << attempt)).await;
            }
        }
        // If still can't get lock after retries, log warning
        tracing::warn!("Failed to update MCP {} error state after retries", id);
    }
    
    /// Helper to update state with success - retries a few times to handle lock contention
    async fn update_state_success(&self, id: Uuid, tool_names: Vec<String>, server_version: Option<String>) {
        // Try up to 5 times with small delays to handle temporary lock contention
        for attempt in 0..5 {
            if let Ok(mut states) = self.states.try_write() {
                if let Some(state) = states.get_mut(&id) {
                    state.config.tools = tool_names;
                    state.config.version = server_version;
                    state.config.last_connected_at = Some(chrono::Utc::now());
                    state.status = McpStatus::Connected;
                    state.error = None;
                }
                return;
            }
            // Small delay before retry
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(10 << attempt)).await;
            }
        }
        // If still can't get lock after retries, log warning
        tracing::warn!("Failed to update MCP {} success state after retries", id);
    }
    
    /// Refresh an MCP server - reconnect and discover tools.
    pub async fn refresh(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        let state = self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))?;
        
        if !state.config.enabled {
            return Ok(state);
        }
        
        let endpoint = state.config.endpoint.trim_end_matches('/').to_string();
        
        // Step 1: Initialize the MCP connection with JSON-RPC
        let init_result = match self.initialize_mcp(&endpoint).await {
            Ok(result) => result,
            Err(e) => {
                self.update_state_error(id, format!("Initialize failed: {}", e)).await;
                return self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"));
            }
        };
        
        // Extract server version if available
        let server_version = init_result.server_info.as_ref().and_then(|s| s.version.clone());
        
        // Step 2: List tools using JSON-RPC
        match self.send_jsonrpc(&endpoint, "tools/list", None).await {
            Ok(result) => {
                match serde_json::from_value::<McpToolsResponse>(result) {
                    Ok(tools_response) => {
                        let tool_names: Vec<String> = tools_response
                            .tools
                            .iter()
                            .map(|t| t.name.clone())
                            .collect();
                        
                        // Update config with discovered tools
                        let _ = self.config_store.update(id, |c| {
                            c.tools = tool_names.clone();
                            c.version = server_version.clone();
                            c.last_connected_at = Some(chrono::Utc::now());
                        }).await;
                        
                        // Update runtime state
                        self.update_state_success(id, tool_names, server_version).await;
                    }
                    Err(e) => {
                        self.update_state_error(id, format!("Failed to parse tools: {}", e)).await;
                    }
                }
            }
            Err(e) => {
                self.update_state_error(id, format!("tools/list failed: {}", e)).await;
            }
        }
        
        self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Refresh all MCP servers concurrently.
    pub async fn refresh_all(&self) {
        let ids: Vec<Uuid> = self.states.read().await.keys().cloned().collect();
        
        // Refresh all MCPs concurrently using join_all
        let futures: Vec<_> = ids.iter().map(|id| self.refresh(*id)).collect();
        futures::future::join_all(futures).await;
    }

    /// Call a tool on an MCP server.
    pub async fn call_tool(&self, mcp_id: Uuid, tool_name: &str, arguments: serde_json::Value) -> anyhow::Result<String> {
        // Check if tool is disabled
        if self.disabled_tools.read().await.contains(tool_name) {
            anyhow::bail!("Tool {} is disabled", tool_name);
        }
        
        let state = self.get(mcp_id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))?;
        
        if !state.config.enabled {
            anyhow::bail!("MCP {} is disabled", state.config.name);
        }
        
        if state.status != McpStatus::Connected {
            anyhow::bail!("MCP {} is not connected", state.config.name);
        }
        
        let endpoint = state.config.endpoint.trim_end_matches('/');
        
        // Use JSON-RPC tools/call method
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });
        
        let result = match self.send_jsonrpc(endpoint, "tools/call", Some(params)).await {
            Ok(result) => result,
            Err(e) => {
                // Increment error counter
                let mut states = self.states.write().await;
                if let Some(state) = states.get_mut(&mcp_id) {
                    state.tool_errors += 1;
                }
                anyhow::bail!("Tool call failed: {}", e);
            }
        };
        
        let response: McpCallToolResponse = serde_json::from_value(result)?;
        
        // Increment counters
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&mcp_id) {
                if response.is_error {
                    state.tool_errors += 1;
                } else {
                    state.tool_calls += 1;
                }
            }
        }
        
        if response.is_error {
            let error_text = response.content
                .iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("Tool error: {}", error_text);
        }
        
        // Combine text content
        let output = response.content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        
        Ok(output)
    }

    /// List all tools from all connected MCPs.
    pub async fn list_tools(&self) -> Vec<McpTool> {
        let states = self.states.read().await;
        let disabled = self.disabled_tools.read().await;
        
        let mut tools = Vec::new();
        for state in states.values() {
            if state.config.enabled && state.status == McpStatus::Connected {
                for tool_name in &state.config.tools {
                    tools.push(McpTool {
                        name: tool_name.clone(),
                        description: String::new(), // Would need to store this from discovery
                        parameters_schema: serde_json::json!({}),
                        mcp_id: state.config.id,
                        enabled: !disabled.contains(tool_name),
                    });
                }
            }
        }
        tools
    }

    /// Enable a tool.
    pub async fn enable_tool(&self, name: &str) {
        self.disabled_tools.write().await.remove(name);
    }

    /// Disable a tool.
    pub async fn disable_tool(&self, name: &str) {
        self.disabled_tools.write().await.insert(name.to_string());
    }

    /// Check if a tool is enabled.
    pub async fn is_tool_enabled(&self, name: &str) -> bool {
        !self.disabled_tools.read().await.contains(name)
    }
}
