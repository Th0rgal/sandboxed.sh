//! MCP management API endpoints.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mcp::{AddMcpRequest, McpServerState};

use super::routes::AppState;

/// List all MCP servers.
pub async fn list_mcps(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<McpServerState>>, (StatusCode, String)> {
    let mcps = state.mcp.list().await;
    Ok(Json(mcps))
}

/// Get a specific MCP server.
pub async fn get_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    state
        .mcp
        .get(id)
        .await
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("MCP {} not found", id)))
}

/// Add a new MCP server.
pub async fn add_mcp(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddMcpRequest>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    state
        .mcp
        .add(req)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// Remove an MCP server.
pub async fn remove_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .mcp
        .remove(id)
        .await
        .map(|_| Json(serde_json::json!({ "success": true })))
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

/// Enable an MCP server.
pub async fn enable_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    state
        .mcp
        .enable(id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

/// Disable an MCP server.
pub async fn disable_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    state
        .mcp
        .disable(id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

/// Refresh an MCP server (reconnect and discover tools).
pub async fn refresh_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    state
        .mcp
        .refresh(id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Refresh all MCP servers.
pub async fn refresh_all_mcps(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    state.mcp.refresh_all().await;
    Json(serde_json::json!({ "success": true }))
}

// ==================== Tools Management ====================

/// Response for listing all tools.
#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub source: ToolSource,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool
    Builtin,
    /// Tool from an MCP server
    Mcp { id: Uuid, name: String },
}

/// List all available tools (built-in + MCP).
pub async fn list_tools(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<ToolInfo>> {
    let mut tools = Vec::new();
    
    // Add built-in tools from the ToolRegistry
    let builtin_tools = crate::tools::ToolRegistry::new().list_tools();
    for t in builtin_tools {
        tools.push(ToolInfo {
            name: t.name.clone(),
            description: t.description.clone(),
            source: ToolSource::Builtin,
            enabled: state.mcp.is_tool_enabled(&t.name).await,
        });
    }
    
    // Add MCP tools
    let mcp_tools = state.mcp.list_tools().await;
    let mcp_states = state.mcp.list().await;
    
    for t in mcp_tools {
        let mcp_name = mcp_states
            .iter()
            .find(|s| s.config.id == t.mcp_id)
            .map(|s| s.config.name.clone())
            .unwrap_or_default();
        
        tools.push(ToolInfo {
            name: t.name.clone(),
            description: t.description.clone(),
            source: ToolSource::Mcp { id: t.mcp_id, name: mcp_name },
            enabled: t.enabled,
        });
    }
    
    // Sort by name for stable ordering
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    
    Json(tools)
}

/// Request to toggle a tool.
#[derive(Debug, Deserialize)]
pub struct ToggleToolRequest {
    pub enabled: bool,
}

/// Enable or disable a tool.
pub async fn toggle_tool(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<ToggleToolRequest>,
) -> Json<serde_json::Value> {
    if req.enabled {
        state.mcp.enable_tool(&name).await;
    } else {
        state.mcp.disable_tool(&name).await;
    }
    Json(serde_json::json!({ "success": true, "name": name, "enabled": req.enabled }))
}
