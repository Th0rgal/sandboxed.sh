//! Agent configuration management API endpoints.
//!
//! Provides endpoints for managing agent configurations:
//! - List agents
//! - Create agent
//! - Get agent details
//! - Update agent
//! - Delete agent

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::agent_config::AgentConfig;

/// Create agent routes.
pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_agents))
        .route("/", post(create_agent))
        .route("/:id", get(get_agent))
        .route("/:id", put(update_agent))
        .route("/:id", delete(delete_agent))
}

// ─────────────────────────────────────────────────────────────────────────────
// Request/Response Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub model_id: String,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub model_id: Option<String>,
    pub mcp_servers: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct AgentResponse {
    pub id: Uuid,
    pub name: String,
    pub model_id: String,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<AgentConfig> for AgentResponse {
    fn from(a: AgentConfig) -> Self {
        Self {
            id: a.id,
            name: a.name,
            model_id: a.model_id,
            mcp_servers: a.mcp_servers,
            skills: a.skills,
            commands: a.commands,
            created_at: a.created_at,
            updated_at: a.updated_at,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/agents - List all agents.
async fn list_agents(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<Vec<AgentResponse>>, (StatusCode, String)> {
    let agents = state.agents.list().await;
    let responses: Vec<AgentResponse> = agents.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// POST /api/agents - Create a new agent.
async fn create_agent(
    State(state): State<Arc<super::routes::AppState>>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<AgentResponse>, (StatusCode, String)> {
    if req.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
    }

    if req.model_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Model ID cannot be empty".to_string(),
        ));
    }

    let mut agent = AgentConfig::new(req.name, req.model_id);
    agent.mcp_servers = req.mcp_servers;
    agent.skills = req.skills;
    agent.commands = req.commands;

    let id = state.agents.add(agent.clone()).await;

    tracing::info!("Created agent: {} ({})", agent.name, id);

    Ok(Json(agent.into()))
}

/// GET /api/agents/:id - Get agent details.
async fn get_agent(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AgentResponse>, (StatusCode, String)> {
    state
        .agents
        .get(id)
        .await
        .map(|a| Json(a.into()))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Agent {} not found", id)))
}

/// PUT /api/agents/:id - Update an agent.
async fn update_agent(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentResponse>, (StatusCode, String)> {
    let mut agent = state
        .agents
        .get(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Agent {} not found", id)))?;

    if let Some(name) = req.name {
        if name.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "Name cannot be empty".to_string()));
        }
        agent.name = name;
    }

    if let Some(model_id) = req.model_id {
        if model_id.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Model ID cannot be empty".to_string(),
            ));
        }
        agent.model_id = model_id;
    }

    if let Some(mcp_servers) = req.mcp_servers {
        agent.mcp_servers = mcp_servers;
    }

    if let Some(skills) = req.skills {
        agent.skills = skills;
    }

    if let Some(commands) = req.commands {
        agent.commands = commands;
    }

    let updated = state
        .agents
        .update(id, agent)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Agent {} not found", id)))?;

    tracing::info!("Updated agent: {} ({})", updated.name, id);

    Ok(Json(updated.into()))
}

/// DELETE /api/agents/:id - Delete an agent.
async fn delete_agent(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<(StatusCode, String), (StatusCode, String)> {
    if state.agents.delete(id).await {
        Ok((StatusCode::OK, format!("Agent {} deleted successfully", id)))
    } else {
        Err((StatusCode::NOT_FOUND, format!("Agent {} not found", id)))
    }
}
