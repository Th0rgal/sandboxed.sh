//! Authenticated operator API; node credentials stay on the control plane.
use super::routes::AppState;
use crate::agent_software::{self as software, Inventory, UpdateJob};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
type Error = (StatusCode, String);
fn bad(e: String) -> Error {
    (StatusCode::BAD_REQUEST, e)
}
#[derive(Default, Deserialize)]
pub struct Params {
    pub node: Option<String>,
    #[serde(default)]
    pub force: bool,
}
#[derive(Deserialize, serde::Serialize)]
pub struct Update {
    pub component: String,
    pub version: String,
    pub path: String,
}
#[derive(Deserialize, serde::Serialize)]
pub struct Cancel {
    pub id: String,
}
async fn remote(
    state: &AppState,
    node: &str,
    method: reqwest::Method,
    suffix: &str,
    body: Option<serde_json::Value>,
) -> Result<reqwest::Response, Error> {
    let config = state
        .config
        .remote_nodes
        .node(node)
        .ok_or((StatusCode::NOT_FOUND, "Machine not found".into()))?;
    let token = std::env::var(&config.token_env).map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Machine credentials unavailable".into(),
        )
    })?;
    let mut request = state
        .http_client
        .request(
            method,
            format!(
                "{}/software{}",
                config.base_url.trim_end_matches('/'),
                suffix
            ),
        )
        .bearer_auth(token)
        .timeout(Duration::from_secs(90));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "Machine unavailable".into()))?;
    if !response.status().is_success() {
        return Err((
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            if response.status().as_u16() == 404 {
                "Runtime update required".into()
            } else {
                "Machine software request failed".into()
            },
        ));
    }
    Ok(response)
}
pub async fn inventory(
    State(state): State<Arc<AppState>>,
    Query(q): Query<Params>,
) -> Result<Json<Inventory>, Error> {
    if let Some(node) = q.node {
        return remote(
            &state,
            &node,
            reqwest::Method::GET,
            if q.force { "?force=true" } else { "" },
            None,
        )
        .await?
        .json()
        .await
        .map(Json)
        .map_err(|_| bad("Invalid machine inventory".into()));
    }
    let overrides: HashMap<String, String> = state
        .backend_configs
        .list()
        .await
        .into_iter()
        .filter_map(|c| {
            c.settings
                .get("cli_path")
                .and_then(|p| p.as_str())
                .filter(|p| !p.trim().is_empty())
                .map(|p| (c.id.clone(), p.to_string()))
        })
        .collect();
    let mut result = tokio::task::spawn_blocking(move || {
        if q.force {
            software::clear_versions();
        }
        software::scan("Execution service", &overrides)
    })
    .await
    .map_err(|e| bad(e.to_string()))?;
    software::releases(&mut result, q.force).await;
    Ok(Json(result))
}
pub async fn update(
    State(state): State<Arc<AppState>>,
    Query(q): Query<Params>,
    Json(body): Json<Update>,
) -> Result<Json<UpdateJob>, Error> {
    if let Some(node) = q.node {
        return remote(
            &state,
            &node,
            reqwest::Method::POST,
            "/updates",
            serde_json::to_value(body).ok(),
        )
        .await?
        .json()
        .await
        .map(Json)
        .map_err(|_| bad("Invalid update receipt".into()));
    }
    tokio::task::spawn_blocking(move || software::queue(&body.component, &body.version, &body.path))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map(Json)
        .map_err(bad)
}
pub async fn cancel(
    State(state): State<Arc<AppState>>,
    Query(q): Query<Params>,
    Json(body): Json<Cancel>,
) -> Result<Json<serde_json::Value>, Error> {
    if let Some(node) = q.node {
        remote(
            &state,
            &node,
            reqwest::Method::POST,
            "/updates/cancel",
            serde_json::to_value(body).ok(),
        )
        .await?;
    } else {
        software::cancel(&body.id).map_err(bad)?;
    }
    Ok(Json(serde_json::json!({"ok":true})))
}
