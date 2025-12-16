//! MCP runtime registry - manages connections and tool execution.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::config::McpConfigStore;
use super::types::*;

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

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            config_store,
            states: RwLock::new(states),
            client,
            disabled_tools: RwLock::new(std::collections::HashSet::new()),
        }
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
    pub async fn add(&self, req: AddMcpRequest) -> anyhow::Result<McpServerState> {
        let mut config = McpServerConfig::new(req.name, req.endpoint);
        config.description = req.description;
        
        // Save to persistent store
        let config = self.config_store.add(config).await?;
        
        // Create runtime state
        let state = McpServerState::from_config(config.clone());
        
        // Add to states
        {
            let mut states = self.states.write().await;
            states.insert(config.id, state.clone());
        }
        
        // Try to connect and discover tools
        let _ = self.refresh(config.id).await;
        
        Ok(self.get(config.id).await.unwrap_or(state))
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
        
        // Try to connect
        let _ = self.refresh(id).await;
        
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

    /// Refresh an MCP server - reconnect and discover tools.
    pub async fn refresh(&self, id: Uuid) -> anyhow::Result<McpServerState> {
        let state = self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))?;
        
        if !state.config.enabled {
            return Ok(state);
        }
        
        let endpoint = &state.config.endpoint;
        
        // Try to list tools from the MCP server
        let tools_url = format!("{}/tools/list", endpoint.trim_end_matches('/'));
        
        match self.client.post(&tools_url).json(&serde_json::json!({})).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<McpToolsResponse>().await {
                        Ok(tools_response) => {
                            let tool_names: Vec<String> = tools_response
                                .tools
                                .iter()
                                .map(|t| t.name.clone())
                                .collect();
                            
                            // Update config with discovered tools
                            let _ = self.config_store.update(id, |c| {
                                c.tools = tool_names.clone();
                                c.last_connected_at = Some(chrono::Utc::now());
                            }).await;
                            
                            // Update state
                            let mut states = self.states.write().await;
                            if let Some(state) = states.get_mut(&id) {
                                state.config.tools = tool_names;
                                state.config.last_connected_at = Some(chrono::Utc::now());
                                state.status = McpStatus::Connected;
                                state.error = None;
                            }
                        }
                        Err(e) => {
                            let mut states = self.states.write().await;
                            if let Some(state) = states.get_mut(&id) {
                                state.status = McpStatus::Error;
                                state.error = Some(format!("Failed to parse tools: {}", e));
                            }
                        }
                    }
                } else {
                    let mut states = self.states.write().await;
                    if let Some(state) = states.get_mut(&id) {
                        state.status = McpStatus::Error;
                        state.error = Some(format!("HTTP {}", response.status()));
                    }
                }
            }
            Err(e) => {
                let mut states = self.states.write().await;
                if let Some(state) = states.get_mut(&id) {
                    state.status = McpStatus::Disconnected;
                    state.error = Some(format!("Connection failed: {}", e));
                }
            }
        }
        
        self.get(id).await.ok_or_else(|| anyhow::anyhow!("MCP not found"))
    }

    /// Refresh all MCP servers.
    pub async fn refresh_all(&self) {
        let ids: Vec<Uuid> = self.states.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.refresh(id).await;
        }
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
        
        let endpoint = &state.config.endpoint;
        let call_url = format!("{}/tools/call", endpoint.trim_end_matches('/'));
        
        let request = McpCallToolRequest {
            name: tool_name.to_string(),
            arguments,
        };
        
        let response = self.client
            .post(&call_url)
            .json(&request)
            .send()
            .await?;
        
        if !response.status().is_success() {
            // Increment error counter
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&mcp_id) {
                state.tool_errors += 1;
            }
            anyhow::bail!("Tool call failed: HTTP {}", response.status());
        }
        
        let result: McpCallToolResponse = response.json().await?;
        
        // Increment success counter
        {
            let mut states = self.states.write().await;
            if let Some(state) = states.get_mut(&mcp_id) {
                if result.isError {
                    state.tool_errors += 1;
                } else {
                    state.tool_calls += 1;
                }
            }
        }
        
        if result.isError {
            let error_text = result.content
                .iter()
                .filter_map(|c| c.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("Tool error: {}", error_text);
        }
        
        // Combine text content
        let output = result.content
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
