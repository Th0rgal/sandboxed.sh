//! HTTP handlers for the Ask assistant.
//!
//! All routes are mission-scoped and live under `/api/control/missions/:id/ask`.
//! They run in the Ask lane: never acquiring the harness lock, never enqueuing
//! into the mission's message queue, and never writing to `mission_events`.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Extension, Json};
use futures::Stream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::{AskMessage, AskThread};
use super::{run_ask_turn, run_ask_turn_streaming, AskClient, AskTurn};
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

/// Resolve the directory the Ask bash tool should run in — the same place the
/// harness actually ran the mission. Precedence mirrors the mission runner:
///   1. an explicit `working_directory` override (e.g. an orchestrated worker's
///      git worktree), but only when it still exists;
///   2. otherwise the per-mission workspace dir
///      (`<workspace>/workspaces/mission-<short>`), which is where the backend
///      ran — `working_directory` is left NULL for ordinary missions, so this
///      is the common path;
///   3. a last-resort fall back to the raw workspace root.
///
/// Without step 2 the bash tool started at the workspace *base*, forcing the
/// model to burn its whole iteration budget just locating the project.
fn resolve_base_work_dir(
    working_directory: Option<&str>,
    workspace: &crate::workspace::Workspace,
    nspawn_container: bool,
    mission_id: Uuid,
) -> Result<std::path::PathBuf, String> {
    let workspace_path = &workspace.path;
    if let Some(dir) = working_directory.map(std::path::PathBuf::from) {
        // Mission metadata records the path as the harness saw it. In an
        // nspawn workspace that is a guest path such as
        // `/workspaces/mission-abcd/repo`, not a host path. Checking it on the
        // host either fails (and silently selects the wrong directory) or,
        // for generic paths such as `/tmp`, accidentally selects a host tree.
        if nspawn_container && dir.is_absolute() && !dir.starts_with(workspace_path) {
            let host_dir = workspace_path.join(dir.strip_prefix("/").unwrap_or(&dir));
            if host_dir.exists() {
                return Ok(host_dir);
            }
        } else if dir.exists() {
            return Ok(dir);
        }
    }
    // This has to use the same fail-closed persisted-placement check as
    // mission preparation.  Ask is a sidecar, but running its shell in a
    // newly selected workspace after a mounted persisted root disappears is
    // still a write/read against the wrong mission tree.
    crate::workspace::ensure_persisted_mission_root_is_available(workspace, mission_id)
        .map_err(|error| format!("persisted mission workspace root is unavailable: {error}"))?;
    let per_mission_dir =
        crate::workspace::mission_workspace_dir_for_workspace(workspace, mission_id);
    if per_mission_dir.exists() {
        Ok(per_mission_dir)
    } else {
        Ok(workspace_path.to_path_buf())
    }
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
    let cfg = crate::api::metadata_llm::build_assistant_llm_config(
        &state.ai_providers,
        &state.chain_store,
        model_override,
    )
    .await
    .ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "No assistant LLM configured (set a Cerebras key or ASK_ASSISTANT_MODEL)".to_string(),
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
    let nspawn_container = workspace.workspace_type == crate::workspace::WorkspaceType::Container
        && crate::workspace::use_nspawn_for_workspace(&workspace);
    let base_work_dir = resolve_base_work_dir(
        mission.working_directory.as_deref(),
        &workspace,
        nspawn_container,
        mission_id,
    )
    .map_err(internal)?;

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
            "sandbox_unavailable: could not create an isolated git worktree or temp-copy sandbox"
                .to_string(),
        ));
    }
    let used_sandbox = sandbox_dir.is_some();
    let work_dir = sandbox_dir.clone().unwrap_or_else(|| base_work_dir.clone());

    let workspace_id = workspace.id;
    let turn = AskTurn {
        ask_store: Arc::clone(&ask_store),
        mission_store: Arc::clone(&control.mission_store),
        workspace_exec: crate::workspace_exec::WorkspaceExec::new(workspace),
        work_dir,
        llm: AskClient::new(state.http_client.clone(), cfg),
        mission_id,
        thread_id: thread.id,
        sandbox: used_sandbox,
        workspaces: Arc::clone(&state.workspaces),
        workspace_id,
        proxy_keys: Arc::clone(&state.proxy_api_keys),
        control_cmd_tx: control.cmd_tx.clone(),
        events_tx: control.events_tx.clone(),
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

/// POST /api/control/missions/:id/ask/stream — same as `ask_send`, but streams
/// the answer token-by-token (and tool steps) as Server-Sent Events.
pub async fn ask_send_stream(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<AskSendRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
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

    let cfg = crate::api::metadata_llm::build_assistant_llm_config(
        &state.ai_providers,
        &state.chain_store,
        state.settings.get().await.ask_assistant_model,
    )
    .await
    .ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "No assistant LLM configured (set a Cerebras key or ASK_ASSISTANT_MODEL)".to_string(),
    ))?;

    let ask_store = super::ask_store(&state.config).await.map_err(internal)?;

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

    let workspace = crate::workspace::resolve_workspace(
        &state.workspaces,
        &state.config,
        Some(mission.workspace_id),
    )
    .await;
    let nspawn_container = workspace.workspace_type == crate::workspace::WorkspaceType::Container
        && crate::workspace::use_nspawn_for_workspace(&workspace);
    let base_work_dir = resolve_base_work_dir(
        mission.working_directory.as_deref(),
        &workspace,
        nspawn_container,
        mission_id,
    )
    .map_err(internal)?;

    // Optional sandbox-copy isolation (same as the synchronous path): set up a
    // throwaway worktree, run the streamed turn in it, tear it down after.
    let setup_exec = crate::workspace_exec::WorkspaceExec::new(workspace.clone());
    let sandbox_dir = if req.sandbox {
        super::prepare_sandbox(&setup_exec, &base_work_dir).await
    } else {
        None
    };
    if req.sandbox && sandbox_dir.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "sandbox_unavailable: could not create an isolated git worktree or temp-copy sandbox"
                .to_string(),
        ));
    }
    let used_sandbox = sandbox_dir.is_some();
    let work_dir = sandbox_dir.clone().unwrap_or_else(|| base_work_dir.clone());

    let workspace_id = workspace.id;
    let turn = AskTurn {
        ask_store: Arc::clone(&ask_store),
        mission_store: Arc::clone(&control.mission_store),
        workspace_exec: crate::workspace_exec::WorkspaceExec::new(workspace),
        work_dir,
        llm: AskClient::new(state.http_client.clone(), cfg),
        mission_id,
        thread_id: thread.id,
        sandbox: used_sandbox,
        workspaces: Arc::clone(&state.workspaces),
        workspace_id,
        proxy_keys: Arc::clone(&state.proxy_api_keys),
        control_cmd_tx: control.cmd_tx.clone(),
        events_tx: control.events_tx.clone(),
    };

    let content = req.content.clone();
    let thread_id = thread.id;
    let title_store = Arc::clone(&ask_store);
    let cleanup_base = base_work_dir.clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<super::AskStreamEvent>();
    tokio::spawn(async move {
        let ok = run_ask_turn_streaming(&turn, &content, tx).await;
        // Only title a new thread if the turn actually produced an answer
        // (matches the synchronous path, which titles after success).
        if ok && is_new_thread {
            let _ = title_store
                .set_thread_title(thread_id, &derive_title(&content))
                .await;
        }
        if let Some(dir) = &sandbox_dir {
            super::cleanup_sandbox(&setup_exec, &cleanup_base, dir).await;
        }
    });

    let sse = async_stream::stream! {
        while let Some(ev) = rx.recv().await {
            // On the (practically impossible) serialize failure, emit a real
            // error event so the client still receives a terminal frame rather
            // than a payload-less comment that leaves it hanging.
            let event = Event::default().event("ask").json_data(&ev).unwrap_or_else(|_| {
                Event::default()
                    .event("ask")
                    .data(r#"{"type":"error","message":"serialize error"}"#)
            });
            yield Ok::<Event, Infallible>(event);
        }
    };

    Ok(Sse::new(sse).keep_alive(KeepAlive::default()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn nspawn_working_directory_is_resolved_inside_container_root() {
        let root = tempdir().unwrap();
        let guest = "/workspaces/mission-abcd/repo";
        let host = root.path().join("workspaces/mission-abcd/repo");
        std::fs::create_dir_all(&host).unwrap();
        let workspace = crate::workspace::Workspace::default_host(root.path().to_path_buf());
        let resolved = resolve_base_work_dir(
            Some(guest),
            &workspace,
            true,
            Uuid::parse_str("abcd0000-0000-4000-8000-000000000000").unwrap(),
        );
        assert_eq!(resolved.unwrap(), host);
    }

    #[test]
    fn host_working_directory_is_left_unchanged() {
        let root = tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let workspace = crate::workspace::Workspace::default_host(root.path().to_path_buf());
        let resolved = resolve_base_work_dir(
            repo.to_str(),
            &workspace,
            false,
            Uuid::parse_str("abcd0000-0000-4000-8000-000000000000").unwrap(),
        );
        assert_eq!(resolved.unwrap(), repo);
    }

    #[test]
    fn unavailable_persisted_root_rejects_ask_fallback() {
        let root = tempdir().unwrap();
        let storage = root.path().join("temporarily-unmounted-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let workspace = crate::workspace::Workspace::default_host(root.path().to_path_buf());
        let mission = Uuid::new_v4();
        let registry_dir = workspace.path.join(".sandboxed-sh");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("mission-workspace-roots.json"),
            serde_json::json!({ mission.to_string(): storage }).to_string(),
        )
        .unwrap();
        std::fs::remove_dir(&storage).unwrap();

        let error = resolve_base_work_dir(None, &workspace, false, mission).unwrap_err();
        assert!(error.contains("persisted mission workspace root is unavailable"));
        assert_ne!(error, workspace.path.display().to_string());
    }
}
