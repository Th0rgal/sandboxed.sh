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

fn context_store(
    state: &super::routes::AppState,
    slug: &str,
) -> Result<crate::project_context::Store, ApiError> {
    let root = files_root(state, slug)?;
    Ok(crate::project_context::Store::new(
        root,
        state
            .config
            .working_dir
            .join(".sandboxed-sh/project-context-state")
            .join(slug),
    ))
}

async fn context_manifest(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<crate::project_context::Manifest>, ApiError> {
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || store.manifest())
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(internal)
}
async fn context_history(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<Vec<crate::project_context::Change>>, ApiError> {
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || store.history())
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(internal)
}
async fn context_conflicts(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<std::collections::BTreeMap<String, crate::project_context::Operation>>, ApiError> {
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || store.conflicts())
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(internal)
}
async fn context_apply(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Json(operation): Json<crate::project_context::Operation>,
) -> Result<Json<crate::project_context::Receipt>, ApiError> {
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || store.apply(operation))
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(bad_request)
}
async fn context_resolve(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath((slug, id)): AxumPath<(String, String)>,
    Json(operation): Json<crate::project_context::Operation>,
) -> Result<Json<crate::project_context::Receipt>, ApiError> {
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || store.resolve(&id, operation))
        .await
        .map_err(internal)?
        .map(Json)
        .map_err(bad_request)
}
async fn context_blob(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath((slug, hash)): AxumPath<(String, String)>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let store = context_store(&state, &slug)?;
    let bytes = tokio::task::spawn_blocking(move || store.blob(&hash))
        .await
        .map_err(internal)?
        .map_err(not_found)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    ))
}
async fn context_upload(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    bytes: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = context_store(&state, &slug)?;
    let hash = tokio::task::spawn_blocking(move || store.put_blob(&bytes))
        .await
        .map_err(internal)?
        .map_err(bad_request)?;
    Ok(Json(serde_json::json!({"hash":hash})))
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
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || {
        let manifest = store.manifest().map_err(internal)?;
        let entry = manifest
            .entries
            .get(&q.path)
            .ok_or_else(|| not_found("file not found"))?;
        if entry.directory {
            return Err(bad_request("path is not a file"));
        }
        if entry.size > MAX_READ_BYTES {
            return Err(bad_request("file exceeds text preview limit"));
        }
        let bytes = store
            .blob(
                entry
                    .hash
                    .as_deref()
                    .ok_or_else(|| bad_request("missing content"))?,
            )
            .map_err(internal)?;
        let content =
            String::from_utf8(bytes).map_err(|_| bad_request("file is not valid UTF-8"))?;
        Ok(Json(
            serde_json::json!({"path":q.path,"content":content,"revision":entry.revision}),
        ))
    })
    .await
    .map_err(internal)?
}

#[derive(Debug, serde::Deserialize)]
struct WriteRequest {
    path: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    expected_revision: Option<u64>,
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
    let store = context_store(&state, &slug)?;
    let result = tokio::task::spawn_blocking(move || {
        let manifest = store.manifest()?;
        let base = req
            .expected_revision
            .or_else(|| manifest.entries.get(&req.path).map(|entry| entry.revision));
        let hash = store.put_blob(req.content.as_bytes())?;
        store.apply(crate::project_context::Operation {
            id: uuid::Uuid::new_v4().to_string(),
            path: req.path,
            base,
            hash: Some(hash),
            directory: false,
            delete: false,
            source: "orb".into(),
        })
    })
    .await
    .map_err(internal)?
    .map_err(bad_request)?;
    if result.conflict {
        return Err((
            StatusCode::CONFLICT,
            "The file changed since it was opened. Your edit was preserved as a conflict.".into(),
        ));
    }
    Ok(Json(serde_json::json!({"revision":result.revision})))
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
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || {
        let mut path = String::new();
        crate::project_context::valid_path(&req.path).map_err(bad_request)?;
        for part in req.path.split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            let receipt = store
                .apply(crate::project_context::Operation {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: path.clone(),
                    base: None,
                    hash: None,
                    directory: true,
                    delete: false,
                    source: "orb".into(),
                })
                .map_err(bad_request)?;
            if receipt.conflict {
                return Err((
                    StatusCode::CONFLICT,
                    "A file already exists at this path".into(),
                ));
            }
        }
        Ok(Json(serde_json::json!({"path":req.path})))
    })
    .await
    .map_err(internal)?
}

async fn delete_file(
    State(state): State<Arc<super::routes::AppState>>,
    AxumPath(slug): AxumPath<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if q.path.trim().is_empty() {
        return Err(bad_request("refusing to delete the project root"));
    }
    let store = context_store(&state, &slug)?;
    tokio::task::spawn_blocking(move || {
        crate::project_context::valid_path(&q.path).map_err(bad_request)?;
        let manifest = store.manifest().map_err(internal)?;
        let prefix = format!("{}/", q.path);
        let mut entries: Vec<_> = manifest
            .entries
            .iter()
            .filter(|(path, _)| *path == &q.path || path.starts_with(&prefix))
            .collect();
        entries.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        for (path, entry) in entries {
            let receipt = store
                .apply(crate::project_context::Operation {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: path.clone(),
                    base: Some(entry.revision),
                    hash: None,
                    directory: false,
                    delete: true,
                    source: "orb".into(),
                })
                .map_err(bad_request)?;
            if receipt.conflict {
                return Err((
                    StatusCode::CONFLICT,
                    "A file changed during deletion; the remaining files were kept".into(),
                ));
            }
        }
        Ok(Json(serde_json::json!({"deleted":q.path})))
    })
    .await
    .map_err(internal)?
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
    let hermes_dir = super::projects_overview::hermes_projects_dir();
    let aliases = hermes_dir
        .as_ref()
        .map(|dir| super::projects_overview::read_alias_map(dir))
        .unwrap_or_default();
    let overrides = hermes_dir
        .as_ref()
        .map(|dir| super::projects_overview::read_overrides(dir))
        .unwrap_or_default();
    let rows: Vec<RosterRow> = projects
        .into_iter()
        .map(|p| RosterRow {
            slug: p.slug,
            title: p.title,
            objective: p.objective,
            status: Some(p.status),
            updated_at: Some(p.updated_at),
        })
        .collect();
    let entries: Vec<serde_json::Value> = dedupe_roster(rows, &aliases, &overrides)
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

#[derive(Clone, Debug)]
struct RosterRow {
    slug: String,
    title: Option<String>,
    objective: Option<String>,
    status: Option<String>,
    updated_at: Option<String>,
}

/// Collapse alias rows onto their canonical project, drop archived/deleted
/// projects (board overrides win over the stored status), and keep one entry
/// per project. Desktop clients showed `verity` and `verity-core` as two
/// projects because the roster store keeps a row per alias ever delivered.
fn dedupe_roster(
    rows: Vec<RosterRow>,
    aliases: &std::collections::HashMap<String, String>,
    overrides: &std::collections::HashMap<String, String>,
) -> Vec<RosterRow> {
    let mut by_canonical: std::collections::HashMap<String, RosterRow> =
        std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in rows {
        let canonical =
            super::projects_overview::canonicalize_project_slug_with(aliases, &row.slug);
        let canonical = if canonical.is_empty() {
            row.slug.clone()
        } else {
            canonical
        };
        let is_canonical_row = row.slug == canonical;
        match by_canonical.get_mut(&canonical) {
            None => {
                order.push(canonical.clone());
                by_canonical.insert(
                    canonical.clone(),
                    RosterRow {
                        slug: canonical.clone(),
                        ..row
                    },
                );
            }
            Some(existing) => {
                // Prefer the canonical row's fields; otherwise fill gaps and
                // keep the freshest timestamp.
                let take = |mine: &mut Option<String>, theirs: Option<String>| {
                    if is_canonical_row {
                        if theirs.is_some() {
                            *mine = theirs;
                        }
                    } else if mine.is_none() {
                        *mine = theirs;
                    }
                };
                take(&mut existing.title, row.title);
                take(&mut existing.objective, row.objective);
                take(&mut existing.status, row.status);
                if row.updated_at > existing.updated_at {
                    existing.updated_at = row.updated_at;
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|slug| by_canonical.remove(&slug))
        .map(|mut row| {
            if let Some(forced) = overrides.get(&row.slug) {
                row.status = Some(forced.clone());
            }
            row
        })
        .filter(|row| !matches!(row.status.as_deref(), Some("archived") | Some("deleted")))
        .collect()
}

pub fn routes() -> Router<Arc<super::routes::AppState>> {
    Router::new()
        .route("/", get(list_projects))
        .route("/:slug/context/manifest", get(context_manifest))
        .route("/:slug/context/history", get(context_history))
        .route("/:slug/context/conflicts", get(context_conflicts))
        .route("/:slug/context/conflicts/:id", post(context_resolve))
        .route("/:slug/context/operations", post(context_apply))
        .route("/:slug/context/blobs/:hash", get(context_blob))
        .route(
            "/:slug/context/blobs",
            post(context_upload).layer(axum::extract::DefaultBodyLimit::max(
                crate::project_context::FILE_LIMIT,
            )),
        )
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
    fn roster_folds_aliases_and_hides_archived() {
        let aliases: std::collections::HashMap<String, String> = [
            ("verity".to_string(), "verity-core".to_string()),
            ("Verity-Lido".to_string(), "verity-lido".to_string()),
        ]
        .into_iter()
        .collect();
        let overrides: std::collections::HashMap<String, String> =
            [("old".to_string(), "archived".to_string())]
                .into_iter()
                .collect();
        let row = |slug: &str, title: Option<&str>, status: &str, at: &str| RosterRow {
            slug: slug.into(),
            title: title.map(str::to_string),
            objective: None,
            status: Some(status.into()),
            updated_at: Some(at.into()),
        };
        let out = dedupe_roster(
            vec![
                row(
                    "verity",
                    Some("Verity (alias)"),
                    "active",
                    "2026-09-19T10:00:00Z",
                ),
                row(
                    "verity-core",
                    Some("Verity"),
                    "active",
                    "2026-09-18T10:00:00Z",
                ),
                row("Verity-Lido", None, "archived", "2026-09-01T00:00:00Z"),
                row(
                    "verity-lido",
                    Some("Lido"),
                    "active",
                    "2026-09-17T00:00:00Z",
                ),
                row("old", Some("Old"), "active", "2026-09-17T00:00:00Z"),
            ],
            &aliases,
            &overrides,
        );
        let slugs: Vec<&str> = out.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(slugs, vec!["verity-core", "verity-lido"]);
        // Canonical row's title wins, freshest timestamp is kept.
        assert_eq!(out[0].title.as_deref(), Some("Verity"));
        assert_eq!(out[0].updated_at.as_deref(), Some("2026-09-19T10:00:00Z"));
        // Alias status (archived) must not hide the canonical live project.
        assert_eq!(out[1].status.as_deref(), Some("active"));
    }

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

/// Observe direct harness writes even while no UI is polling.
pub fn start_context_observer(working_dir: PathBuf) {
    tokio::spawn(async move {
        loop {
            let working_dir = working_dir.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let root = working_dir.join(".sandboxed-sh/project-files");
                if let Ok(projects) = std::fs::read_dir(root) {
                    for project in projects.flatten() {
                        let Some(slug) = project.file_name().to_str().map(str::to_owned) else {
                            continue;
                        };
                        if !super::projects_overview::is_plain_key(&slug) {
                            continue;
                        }
                        let metadata = working_dir
                            .join(".sandboxed-sh/project-context-state")
                            .join(&slug);
                        let store = crate::project_context::Store::new(project.path(), metadata);
                        if let Err(error) = store.manifest() {
                            tracing::warn!(project=%slug,%error,"Context reconciliation deferred");
                        }
                    }
                }
            })
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}
