use super::{auth::AuthUser, routes::AppState};
use axum::{extract::State, http::StatusCode, Extension, Json};
use std::sync::Arc;

#[derive(serde::Deserialize)]
pub struct Request {
    pub node_id: Option<String>,
    #[serde(flatten)]
    pub file: crate::uploads::Upload,
}
type Error = (StatusCode, String);

pub async fn upload(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(request): Json<Request>,
) -> Result<Json<crate::uploads::Receipt>, Error> {
    let slot = crate::uploads::SLOTS.try_acquire().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "Other uploads are in progress; try again shortly".into(),
        )
    })?;
    if let Some(id) = request.node_id.as_deref().filter(|id| *id != "core") {
        let node = state.config.remote_nodes.node(id).ok_or((
            StatusCode::BAD_REQUEST,
            "Unknown destination machine".into(),
        ))?;
        let token = std::env::var(&node.token_env).map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Machine connection is unavailable".into(),
            )
        })?;
        let response = state
            .http_client
            .post(format!("{}/uploads", node.base_url))
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(120))
            .json(&request.file)
            .send()
            .await
            .map_err(|_| {
                (
                    StatusCode::BAD_GATEWAY,
                    "Could not transfer the file to the selected machine".into(),
                )
            })?;
        if !response.status().is_success() {
            let status = response.status();
            return Err((
                StatusCode::BAD_GATEWAY,
                if status == StatusCode::NOT_FOUND {
                    "This machine needs an update before accepting file uploads".into()
                } else {
                    format!("File transfer refused by the selected machine ({status})")
                },
            ));
        }
        return response.json().await.map(Json).map_err(|_| {
            (
                StatusCode::BAD_GATEWAY,
                "Invalid upload receipt from selected machine".into(),
            )
        });
    }
    let root = super::mission_payload::storage_root(&state.config.working_dir).join("uploads");
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        crate::uploads::store(&root, request.file)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Upload worker failed".into(),
        )
    })?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}
