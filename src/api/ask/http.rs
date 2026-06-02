//! HTTP handlers for the Ask assistant.
//!
//! All routes are mission-scoped and live under `/api/control/missions/:id/ask`.
//! They run in the Ask lane: never acquiring the harness lock, never enqueuing
//! into the mission's message queue, and never writing to `mission_events`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::{AskMessage, AskThread};
use super::{run_ask_turn, AskClient, AskTurn};
use crate::api::auth::AuthUser;
use crate::api::routes::AppState;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Derive a short thread title from the operator's first message (first line,
/// trimmed to ~48 chars). Cheap and synchronous — no extra LLM call.
fn derive_title(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("").trim();
    let mut title: String = first_line.chars().take(48).collect();
    if first_line.chars().count() > 48 {
        title.push('…');
    }
    if title.is_empty() {
        "Ask".to_string()
    } else {
        title
    }
}

#[derive(Debug, Deserialize)]
pub struct AskSendRequest {
    /// Existing thread to continue. When absent, a new thread is created.
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    pub content: String,
    /// Run the Ask bash tool in an isolated copy of the workspace (git worktree
    /// or temp copy) so writes never touch the live tree. Opt-in.
    #[serde(default)]
    pub sandbox: bool,
}

#[derive(Debug, Serialize)]
pub struct AskSendResponse {
    pub thread_id: Uuid,
    pub answer: String,
    pub messages: Vec<AskMessage>,
}

#[derive(Debug, Serialize)]
pub struct AskThreadDetail {
    #[serde(flatten)]
    pub thread: AskThread,
    pub messages: Vec<AskMessage>,
}

/// POST /api/control/missions/:id/ask — send a question to the Ask assistant.
pub async fn ask_send(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<AskSendRequest>,
) -> Result<Json<AskSendResponse>, (StatusCode, String)> {
    if req.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "content is empty".to_string()));
    }

    let control = crate::api::control::control_for_user(&state, &user).await;
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "Mission not found".to_string()))?;

    // Resolve the assistant model/config (Settings override → env → default).
    let model_override = state.settings.get().await.ask_assistant_model;
    let cfg =
        crate::api::metadata_llm::build_assistant_llm_config(&state.ai_providers, model_override)
            .await
            .ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "No assistant LLM configured (set a Cerebras key or ASK_ASSISTANT_MODEL)"
                    .to_string(),
            ))?;

    let ask_store = super::ask_store(&state.config).await.map_err(internal)?;

    // Resolve or create the thread.
    let is_new_thread = req.thread_id.is_none();
    let thread = match req.thread_id {
        Some(tid) => {
            let t = ask_store
                .get_thread(tid)
                .await
                .map_err(internal)?
                .ok_or((StatusCode::NOT_FOUND, "Thread not found".to_string()))?;
            if t.mission_id != mission_id {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Thread does not belong to this mission".to_string(),
                ));
            }
            t
        }
        None => ask_store
            .create_thread(mission_id, Some(cfg.model.clone()))
            .await
            .map_err(internal)?,
    };

    // Resolve the live workspace + working directory for the bash tool.
    let workspace = crate::workspace::resolve_workspace(
        &state.workspaces,
        &state.config,
        Some(mission.workspace_id),
    )
    .await;
    let base_work_dir = mission
        .working_directory
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| workspace.path.clone());

    // Optional sandbox-copy isolation: writes go to a throwaway worktree/copy.
    let setup_exec = crate::workspace_exec::WorkspaceExec::new(workspace.clone());
    let sandbox_dir = if req.sandbox {
        super::prepare_sandbox(&setup_exec, &base_work_dir).await
    } else {
        None
    };
    if req.sandbox && sandbox_dir.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Sandbox mode requires a git workspace (no isolated worktree could be created)"
                .to_string(),
        ));
    }
    let used_sandbox = sandbox_dir.is_some();
    let work_dir = sandbox_dir.clone().unwrap_or_else(|| base_work_dir.clone());

    let turn = AskTurn {
        ask_store: Arc::clone(&ask_store),
        mission_store: Arc::clone(&control.mission_store),
        workspace_exec: crate::workspace_exec::WorkspaceExec::new(workspace),
        work_dir,
        llm: AskClient::new(state.http_client.clone(), cfg),
        mission_id,
        thread_id: thread.id,
        sandbox: used_sandbox,
    };

    let answer_result = run_ask_turn(&turn, &req.content).await;

    // Tear down the sandbox regardless of how the turn ended.
    if let Some(dir) = &sandbox_dir {
        super::cleanup_sandbox(&setup_exec, &base_work_dir, dir).await;
    }
    let answer = answer_result.map_err(internal)?;

    // Auto-title a freshly created thread from the operator's first message.
    if is_new_thread {
        let _ = ask_store
            .set_thread_title(thread.id, &derive_title(&req.content))
            .await;
    }

    let messages = ask_store.list_messages(thread.id).await.map_err(internal)?;

    Ok(Json(AskSendResponse {
        thread_id: thread.id,
        answer,
        messages,
    }))
}

/// GET /api/control/missions/:id/ask/threads — list Ask threads for a mission.
pub async fn list_ask_threads(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Vec<AskThread>>, (StatusCode, String)> {
    let control = crate::api::control::control_for_user(&state, &user).await;
    control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "Mission not found".to_string()))?;

    let ask_store = super::ask_store(&state.config).await.map_err(internal)?;
    let threads = ask_store.list_threads(mission_id).await.map_err(internal)?;
    Ok(Json(threads))
}

/// GET /api/control/missions/:id/ask/threads/:tid — thread + messages.
pub async fn get_ask_thread(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path((mission_id, thread_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<AskThreadDetail>, (StatusCode, String)> {
    let control = crate::api::control::control_for_user(&state, &user).await;
    control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "Mission not found".to_string()))?;

    let ask_store = super::ask_store(&state.config).await.map_err(internal)?;
    let thread = ask_store
        .get_thread(thread_id)
        .await
        .map_err(internal)?
        .filter(|t| t.mission_id == mission_id)
        .ok_or((StatusCode::NOT_FOUND, "Thread not found".to_string()))?;
    let messages = ask_store.list_messages(thread_id).await.map_err(internal)?;
    Ok(Json(AskThreadDetail { thread, messages }))
}

/// DELETE /api/control/missions/:id/ask/threads/:tid — clear/delete a thread.
pub async fn delete_ask_thread(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path((mission_id, thread_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = crate::api::control::control_for_user(&state, &user).await;
    control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "Mission not found".to_string()))?;

    let ask_store = super::ask_store(&state.config).await.map_err(internal)?;
    // Only delete if the thread belongs to this mission.
    if let Some(thread) = ask_store.get_thread(thread_id).await.map_err(internal)? {
        if thread.mission_id == mission_id {
            ask_store.delete_thread(thread_id).await.map_err(internal)?;
        }
    }
    Ok(Json(
        serde_json::json!({ "ok": true, "deleted": thread_id }),
    ))
}
