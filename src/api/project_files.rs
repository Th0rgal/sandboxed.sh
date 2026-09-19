//! Per-project file storage for the Orb/desktop clients.
//!
//! Projects are control-plane records; their working documents (notes, specs,
//! key files the missions reference) live in a plain directory per project on
//! the sandboxed.sh host so they can be listed, read and edited over the API:
//!
//! - `GET    /api/projects`                      — lightweight project list
//! - `GET    /api/projects/:slug/files?path=`    — list a directory
//! - `GET    /api/projects/:slug/file?path=`     — read a text file
//! - `PUT    /api/projects/:slug/file`           — write a text file
//! - `POST   /api/projects/:slug/file/mkdir`     — create a directory
//! - `DELETE /api/projects/:slug/file?path=`     — remove a file or directory
//!
//! Files live under `<working_dir>/.sandboxed-sh/project-files/<slug>/`. The
//! directory is created on first write; unknown slugs list as empty rather
//! than 404 so freshly-created projects work before any store row exists.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{delete, get, post, put};
use axum::Router;

const MAX_READ_BYTES: u64 = 512 * 1024;
const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 500;

type ApiError = (StatusCode, String);

fn bad_request(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn not_found(msg: impl Into<String>) -> ApiError {
    (StatusCode::NOT_FOUND, msg.into())
}

fn internal(msg: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.to_string())
}

fn files_root(state: &super::routes::AppState, slug: &str) -> Result<PathBuf, ApiError> {
    if !super::projects_overview::is_plain_key(slug) {
        return Err(bad_request("invalid project slug"));
    }
    Ok(state
        .config
        .working_dir
        .join(".sandboxed-sh/project-files")
        .join(slug))
}

/// Reject absolute paths, parent traversal and anything that is not a plain
/// relative path of normal components.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, ApiError> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    let mut out = root.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return Err(bad_request("path must be relative with no '..' components")),
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

#[derive(Debug, serde::Serialize)]
struct FileEntry {
    name: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
}

async fn list_files(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = files_root(&state, &slug)?;
    let dir = safe_join(&root, &q.path)?;
    if !dir.exists() {
        return Ok(Json(serde_json::json!({ "path": q.path, "entries": [] })));
    }
    if !dir.is_dir() {
        return Err(bad_request("path is not a directory"));
    }
    let mut entries = Vec::new();
    let read = std::fs::read_dir(&dir).map_err(internal)?;
    for entry in read.flatten().take(MAX_ENTRIES) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        entries.push(FileEntry {
            name,
            kind: if is_dir { "dir" } else { "file" },
            size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            modified: meta
                .and_then(|m| m.modified().ok())
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != "dir")
            .cmp(&(b.kind != "dir"))
            .then(a.name.cmp(&b.name))
    });
    Ok(Json(
        serde_json::json!({ "path": q.path, "entries": entries }),
    ))
}

async fn read_file(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = files_root(&state, &slug)?;
    let path = safe_join(&root, &q.path)?;
    let meta = std::fs::metadata(&path).map_err(|_| not_found("file not found"))?;
    if !meta.is_file() {
        return Err(bad_request("path is not a file"));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(bad_request(format!(
            "file is {} bytes, over the {} byte read cap",
            meta.len(),
            MAX_READ_BYTES
        )));
    }
    let bytes = std::fs::read(&path).map_err(internal)?;
    let content = String::from_utf8(bytes).map_err(|_| bad_request("file is not valid UTF-8"))?;
    Ok(Json(
        serde_json::json!({ "path": q.path, "content": content }),
    ))
}

#[derive(Debug, serde::Deserialize)]
struct WriteRequest {
    path: String,
    #[serde(default)]
    content: String,
}

async fn write_file(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<WriteRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.path.trim().is_empty() {
        return Err(bad_request("path is required"));
    }
    if req.content.len() > MAX_WRITE_BYTES {
        return Err(bad_request(format!(
            "content is {} bytes, over the {} byte write cap",
            req.content.len(),
            MAX_WRITE_BYTES
        )));
    }
    let root = files_root(&state, &slug)?;
    let path = safe_join(&root, &req.path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(internal)?;
    }
    std::fs::write(&path, &req.content).map_err(internal)?;
    Ok(Json(
        serde_json::json!({ "path": req.path, "bytes": req.content.len() }),
    ))
}

#[derive(Debug, serde::Deserialize)]
struct MkdirRequest {
    path: String,
}

async fn mkdir(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(req): Json<MkdirRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.path.trim().is_empty() {
        return Err(bad_request("path is required"));
    }
    let root = files_root(&state, &slug)?;
    let path = safe_join(&root, &req.path)?;
    std::fs::create_dir_all(&path).map_err(internal)?;
    Ok(Json(serde_json::json!({ "path": req.path })))
}

async fn delete_file(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if q.path.trim().is_empty() {
        return Err(bad_request("refusing to delete the project root"));
    }
    let root = files_root(&state, &slug)?;
    let path = safe_join(&root, &q.path)?;
    let meta = std::fs::metadata(&path).map_err(|_| not_found("path not found"))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&path).map_err(internal)?;
    } else {
        std::fs::remove_file(&path).map_err(internal)?;
    }
    Ok(Json(serde_json::json!({ "deleted": q.path })))
}

/// Lightweight roster for clients that only need slug + title + status (the
/// full `/overview` payload carries counts and attention detail they don't).
async fn list_projects(
    State(state): State<Arc<super::routes::AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let projects = state
        .projects
        .list_projects()
        .map_err(super::projects_overview::store_err)?;
    let entries: Vec<serde_json::Value> = projects
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "slug": p.slug,
                "title": p.title,
                "objective": p.objective,
                "status": p.status,
                "updated_at": p.updated_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "projects": entries })))
}

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_projects))
        .route("/:slug/files", get(list_files))
        .route("/:slug/file", get(read_file))
        .route("/:slug/file", put(write_file))
        .route("/:slug/file", delete(delete_file))
        .route("/:slug/file/mkdir", post(mkdir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_traversal() {
        let root = Path::new("/tmp/root");
        assert!(safe_join(root, "notes/a.md").is_ok());
        assert!(safe_join(root, "").is_ok());
        assert!(safe_join(root, "../escape").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "a/../../b").is_err());
    }

    /// The merged /api/projects router mixes our `/:slug/file*` statics with
    /// the existing `/:slug` wildcard — a conflict panics at build time.
    #[test]
    fn merged_projects_router_builds() {
        let _ = crate::api::projects_overview::routes();
    }
}
