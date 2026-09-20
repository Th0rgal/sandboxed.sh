//! Project-scoped view of Hermes cron jobs.
//!
//! Hermes remains the scheduler and source of job state. This module owns only
//! the durable project <-> Hermes job-id binding and never writes a cron file.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

type AppState = super::routes::AppState;

fn valid_slug(slug: &str) -> Result<(), Response> {
    if super::projects_overview::is_plain_key(slug) {
        Ok(())
    } else {
        Err((StatusCode::BAD_REQUEST, "invalid project slug").into_response())
    }
}

async fn hermes(
    state: &AppState,
    method: reqwest::Method,
    suffix: &str,
    body: Option<Value>,
) -> Result<Value, Response> {
    let key = super::system::hermes_api_server_key(state)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e).into_response())?;
    let url = format!(
        "{}{}",
        super::system::hermes_api_server_url(&state.config),
        suffix
    );
    let mut request = state.http_client.request(method, url).bearer_auth(key);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Hermes scheduler unavailable: {e}"),
        )
            .into_response()
    })?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let mapped = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return Err((mapped, text).into_response());
    }
    serde_json::from_str(&text).map_err(|_| {
        (
            StatusCode::BAD_GATEWAY,
            "Hermes scheduler returned invalid JSON",
        )
            .into_response()
    })
}

fn owned(state: &AppState, slug: &str, id: &str) -> Result<(), Response> {
    state
        .projects
        .owns_project_cron(slug, id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e).into_response())?
        .then_some(())
        .ok_or_else(|| (StatusCode::NOT_FOUND, "cron is not bound to this project").into_response())
}

async fn list(State(state): State<Arc<AppState>>, Path(slug): Path<String>) -> Response {
    if let Err(e) = valid_slug(&slug) {
        return e;
    }
    let ids = match state.projects.project_cron_ids(&slug) {
        Ok(ids) => ids,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let mut jobs = Vec::new();
    for id in ids {
        if let Ok(job) = hermes(
            &state,
            reqwest::Method::GET,
            &format!("/api/jobs/{id}"),
            None,
        )
        .await
        {
            jobs.push(job.get("job").cloned().unwrap_or(job));
        }
    }
    Json(json!({"jobs": jobs})).into_response()
}

async fn create(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(e) = valid_slug(&slug) {
        return e;
    }
    match state.projects.get_project(&slug) {
        Ok(Some(_)) => {}
        Ok(None) => return (StatusCode::NOT_FOUND, "project not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
    let result = match hermes(&state, reqwest::Method::POST, "/api/jobs", Some(body)).await {
        Ok(value) => value,
        Err(e) => return e,
    };
    let id = result
        .pointer("/job/id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if id.is_empty() {
        return (
            StatusCode::BAD_GATEWAY,
            "Hermes created a job without an id",
        )
            .into_response();
    }
    if let Err(e) = state.projects.bind_project_cron(&slug, id) {
        // Avoid a scheduler orphan if our durable binding cannot be committed.
        let _ = hermes(
            &state,
            reqwest::Method::DELETE,
            &format!("/api/jobs/{id}"),
            None,
        )
        .await;
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    Json(result).into_response()
}

async fn get_one(
    State(state): State<Arc<AppState>>,
    Path((slug, id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = valid_slug(&slug) {
        return e;
    }
    if let Err(e) = owned(&state, &slug, &id) {
        return e;
    }
    hermes(
        &state,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await
    .map(Json)
    .map(IntoResponse::into_response)
    .unwrap_or_else(|e| e)
}

async fn update(
    State(state): State<Arc<AppState>>,
    Path((slug, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(e) = valid_slug(&slug) {
        return e;
    }
    if let Err(e) = owned(&state, &slug, &id) {
        return e;
    }
    hermes(
        &state,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{id}"),
        Some(body),
    )
    .await
    .map(Json)
    .map(IntoResponse::into_response)
    .unwrap_or_else(|e| e)
}

async fn action(
    State(state): State<Arc<AppState>>,
    Path((slug, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(e) = valid_slug(&slug) {
        return e;
    }
    if let Err(e) = owned(&state, &slug, &id) {
        return e;
    }
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let suffix = match action {
        "pause" | "resume" | "run" => format!("/api/jobs/{id}/{action}"),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "action must be pause, resume, or run",
            )
                .into_response()
        }
    };
    hermes(&state, reqwest::Method::POST, &suffix, None)
        .await
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|e| e)
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/:slug/crons", get(list).post(create))
        .route("/:slug/crons/:id", get(get_one).patch(update))
        .route("/:slug/crons/:id/action", post(action))
}
