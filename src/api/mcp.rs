//! MCP management API endpoints.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::mcp::{AddMcpRequest, McpServerState};
use crate::tools::ToolRegistry;
use crate::workspace;

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
    let added = state
        .mcp
        .add(req)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let _ = workspace::sync_all_workspaces(&state.config, &state.mcp).await;
    Ok(Json(added))
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
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let _ = workspace::sync_all_workspaces(&state.config, &state.mcp).await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// Enable an MCP server.
pub async fn enable_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    let updated = state
        .mcp
        .enable(id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let _ = workspace::sync_all_workspaces(&state.config, &state.mcp).await;
    Ok(Json(updated))
}

/// Disable an MCP server.
pub async fn disable_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    let updated = state
        .mcp
        .disable(id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let _ = workspace::sync_all_workspaces(&state.config, &state.mcp).await;
    Ok(Json(updated))
}

/// Refresh an MCP server (reconnect and discover tools).
/// This spawns the refresh in the background and returns the current state immediately.
pub async fn refresh_mcp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerState>, (StatusCode, String)> {
    // Get current state first
    let current_state = state
        .mcp
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("MCP {} not found", id)))?;

    // Spawn refresh in background (don't wait for completion)
    let mcp = Arc::clone(&state.mcp);
    tokio::spawn(async move {
        let _ = mcp.refresh(id).await;
    });

    // Return current state with a status indicating refresh is in progress
    Ok(Json(current_state))
}

/// Refresh all MCP servers.
/// This spawns refreshes in the background and returns immediately.
pub async fn refresh_all_mcps(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Spawn refresh_all in background
    let mcp = Arc::clone(&state.mcp);
    tokio::spawn(async move {
        mcp.refresh_all().await;
    });

    Json(serde_json::json!({ "success": true, "message": "Refresh started in background" }))
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
pub async fn list_tools(State(state): State<Arc<AppState>>) -> Json<Vec<ToolInfo>> {
    let mut tools = Vec::new();
    let mut seen = HashSet::new();

    // Add built-in tools
    let registry = ToolRegistry::new();
    for tool in registry.list_tools() {
        if seen.insert(tool.name.clone()) {
            tools.push(ToolInfo {
                name: tool.name,
                description: tool.description,
                source: ToolSource::Builtin,
                enabled: true,
            });
        }
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

        if seen.insert(t.name.clone()) {
            tools.push(ToolInfo {
                name: t.name.clone(),
                description: t.description.clone(),
                source: ToolSource::Mcp {
                    id: t.mcp_id,
                    name: mcp_name,
                },
                enabled: t.enabled,
            });
        }
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
