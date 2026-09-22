//! Authenticated mission/project file sources. Never accepts a host root from clients.
use super::{auth::AuthUser, routes::AppState};
use axum::{extract::State, http::StatusCode, Extension, Json};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
#[derive(serde::Deserialize)]
pub struct Request {
    pub mission_id: Option<uuid::Uuid>,
    pub project: Option<String>,
    #[serde(default)]
    pub source: String,
    #[serde(flatten)]
    pub operation: crate::file_browser::Request,
}
type Error = (StatusCode, String);
pub async fn operate(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(mut req): Json<Request>,
) -> Result<Json<Value>, Error> {
    let mission = if let Some(id) = req.mission_id {
        Some(
            super::control::get_mission(
                State(state.clone()),
                Extension(user),
                axum::extract::Path(id),
            )
            .await?
            .0,
        )
    } else {
        None
    };
    let project = mission
        .as_ref()
        .and_then(|m| m["project"].as_str())
        .or(req.project.as_deref());
    let mut roots: Vec<(String, String, PathBuf)> = Vec::new();
    if let Some(slug) = project {
        if !super::projects_overview::is_plain_key(slug) {
            return Err((StatusCode::BAD_REQUEST, "Invalid project".into()));
        }
        roots.push((
            "context".into(),
            "Project context · current".into(),
            state
                .config
                .working_dir
                .join(".sandboxed-sh/project-files")
                .join(slug),
        ));
    }
    let remote = mission
        .as_ref()
        .and_then(|m| m.get("remote_job"))
        .filter(|j| j.is_object());
    if let (Some(m), Some(id)) = (&mission, req.mission_id) {
        if remote.is_none()
            && !m["tags"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t == "placement:client"))
        {
            if let Some(ws_id) = m["workspace_id"].as_str().and_then(|v| v.parse().ok()) {
                if let Some(ws) = state.workspaces.get(ws_id).await {
                    let path = m["working_directory"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            crate::workspace::mission_workspace_dir_for_workspace(&ws, id)
                                .to_string_lossy()
                                .into_owned()
                        });
                    if let Ok(path) =
                        super::fs::resolve_path_for_workspace(&state, ws_id, &path, Some(id)).await
                    {
                        if path.join(".paloma").is_dir() {
                            roots.push((
                                "snapshot".into(),
                                "Attached context · mission copy".into(),
                                path.join(".paloma"),
                            ));
                        }
                        roots.insert(0, ("workspace".into(), "Workspace".into(), path));
                    }
                }
            }
        }
    }
    if req.operation.action == "roots" {
        let mut sources:Vec<Value>=roots.iter().map(|(id,label,path)|json!({"id":id,"label":label,"path":path,"available":path.is_dir()})).collect();
        if let Some(remote) = remote {
            sources.insert(0,json!({"id":"workspace","label":"Workspace","machine":remote["node_id"],"available":true}));
        }
        return Ok(Json(json!({"sources":sources})));
    }
    if req.source == "workspace" {
        if let Some(remote) = remote {
            let node = remote["node_id"]
                .as_str()
                .and_then(|id| state.config.remote_nodes.node(id))
                .ok_or((
                    StatusCode::CONFLICT,
                    "Remote file source is unavailable".into(),
                ))?;
            let job = remote["job_id"]
                .as_str()
                .ok_or((StatusCode::CONFLICT, "Remote job is unavailable".into()))?;
            let token = std::env::var(&node.token_env).map_err(|_| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Remote file source is unavailable".into(),
                )
            })?;
            let response = state
                .http_client
                .post(format!("{}/jobs/{}/files", node.base_url, job))
                .bearer_auth(token)
                .timeout(std::time::Duration::from_secs(15))
                .json(&req.operation)
                .send()
                .await
                .map_err(|_| {
                    (
                        StatusCode::BAD_GATEWAY,
                        "Remote file source is unreachable".into(),
                    )
                })?;
            if !response.status().is_success() {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "Remote file browser unavailable; update the node or retry".into(),
                ));
            }
            return Ok(Json(response.json().await.map_err(|_| {
                (
                    StatusCode::BAD_GATEWAY,
                    "Invalid remote file response".into(),
                )
            })?));
        }
    }
    let root = roots
        .into_iter()
        .find(|(id, _, _)| id == &req.source)
        .ok_or((StatusCode::NOT_FOUND, "File source unavailable".into()))?
        .2;
    let original = req.operation.paths.clone();
    if req.source == "workspace" && req.operation.action == "resolve" {
        if let (Some(m), Some(id)) = (&mission, req.mission_id) {
            if let Some(ws) = m["workspace_id"].as_str().and_then(|s| s.parse().ok()) {
                for path in &mut req.operation.paths {
                    if std::path::Path::new(path).is_absolute() {
                        if let Ok(resolved) =
                            super::fs::resolve_path_for_workspace(&state, ws, path, Some(id)).await
                        {
                            if let Ok(rel) = resolved.strip_prefix(&root) {
                                *path = rel.to_string_lossy().into_owned();
                            }
                        }
                    }
                }
            }
        }
    }
    let mut value =
        tokio::task::spawn_blocking(move || crate::file_browser::execute(&root, &req.operation))
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "File read failed".into()))?
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if let Some(results) = value["results"].as_array_mut() {
        for (row, reference) in results.iter_mut().zip(original) {
            row["reference"] = json!(reference);
        }
    }
    Ok(Json(value))
}
