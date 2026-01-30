//! Local file explorer endpoints (list/upload/download) via server filesystem access.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Multipart, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::routes::AppState;

#[derive(Debug, Deserialize)]
struct RuntimeWorkspace {
    working_dir: Option<String>,
    mission_context: Option<String>,
    context_root: Option<String>,
    context_dir_name: Option<String>,
    workspace_root: Option<String>,
    workspace_type: Option<String>,
}

fn runtime_workspace_path() -> PathBuf {
    if let Ok(path) = std::env::var("SANDBOXED_SH_RUNTIME_WORKSPACE_FILE") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".sandboxed-sh")
        .join("runtime")
        .join("current_workspace.json")
}

fn load_runtime_workspace() -> Option<RuntimeWorkspace> {
    let path = runtime_workspace_path();
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<RuntimeWorkspace>(&contents).ok()
}

fn is_container_workspace(state: &RuntimeWorkspace) -> bool {
    matches!(state.workspace_type.as_deref(), Some("container"))
}

fn workspace_root_path(state: &RuntimeWorkspace) -> Option<PathBuf> {
    state
        .workspace_root
        .as_ref()
        .map(|root| root.trim())
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

fn map_container_path_to_host(path: &Path, state: &RuntimeWorkspace) -> Option<PathBuf> {
    let root = workspace_root_path(state)?;
    let rel = path.strip_prefix("/").unwrap_or(path);
    Some(root.join(rel))
}

fn resolve_download_path(
    path: &str,
    fallback_root: Option<&Path>,
) -> Result<PathBuf, (StatusCode, String)> {
    let input = Path::new(path);

    if input.is_absolute() {
        // Remap /root/context to mission-specific context if available
        if path.starts_with("/root/context") {
            if let Some(state) = load_runtime_workspace() {
                if let Some(ctx) = state.mission_context {
                    let suffix = path.trim_start_matches("/root/context");
                    return Ok(PathBuf::from(ctx).join(suffix.trim_start_matches('/')));
                }
                if let Some(root) = state.context_root {
                    let suffix = path.trim_start_matches("/root/context");
                    return Ok(PathBuf::from(root).join(suffix.trim_start_matches('/')));
                }
            }
            if let Ok(context_root) = std::env::var("SANDBOXED_SH_CONTEXT_ROOT") {
                let context_root = context_root.trim();
                if !context_root.is_empty() {
                    let suffix = path.trim_start_matches("/root/context");
                    return Ok(PathBuf::from(context_root).join(suffix.trim_start_matches('/')));
                }
            }
        }

        if let Some(state) = load_runtime_workspace() {
            if is_container_workspace(&state) && !input.exists() {
                if let Some(mapped) = map_container_path_to_host(input, &state) {
                    if mapped.exists() {
                        return Ok(mapped);
                    }
                }
            }
        }

        return Ok(input.to_path_buf());
    }

    if let Some(state) = load_runtime_workspace() {
        if let Some(wd) = state
            .working_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let base = PathBuf::from(wd);
            if is_container_workspace(&state) {
                if let Some(mapped_base) = map_container_path_to_host(&base, &state) {
                    return Ok(mapped_base.join(path));
                }
            }
            return Ok(base.join(path));
        }

        if is_container_workspace(&state) {
            if let Some(root) = workspace_root_path(&state) {
                return Ok(root.join(path));
            }
        }
    }

    if let Some(root) = fallback_root {
        return Ok(root.join(path));
    }

    Err((
        StatusCode::BAD_REQUEST,
        "Relative download path requires an active workspace".to_string(),
    ))
}

fn content_type_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("json") => "application/json",
        Some("csv") => "text/csv; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Resolve a path relative to a specific workspace.
/// If mission_id is provided and path is a context path, resolves to mission-specific context.
async fn resolve_path_for_workspace(
    state: &Arc<AppState>,
    workspace_id: uuid::Uuid,
    path: &str,
    mission_id: Option<uuid::Uuid>,
) -> Result<PathBuf, (StatusCode, String)> {
    let workspace = state.workspaces.get(workspace_id).await.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Workspace {} not found", workspace_id),
        )
    })?;

    let workspace_root = workspace.path.canonicalize().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to canonicalize workspace path: {}", e),
        )
    })?;

    let input = Path::new(path);

    // Resolve the final path based on input type
    let resolved = if input.is_absolute() {
        input.to_path_buf()
    } else if path.starts_with("./context") || path.starts_with("context") {
        // For "context" paths, use the mission-specific context directory if mission_id provided
        let suffix = path
            .trim_start_matches("./")
            .trim_start_matches("context/")
            .trim_start_matches("context");

        // If mission_id is provided, use mission-specific context directory
        // This ensures uploaded files go to the right place for the agent to find them
        let context_path = if let Some(mid) = mission_id {
            // Mission context is at /root/context/{mission_id} (or workspace equivalent)
            // For host workspaces, the global context root is typically at working_dir/context
            let context_root = state.config.working_dir.join("context");
            context_root.join(mid.to_string())
        } else {
            workspace_root.join("context")
        };

        if suffix.is_empty() {
            context_path
        } else {
            context_path.join(suffix)
        }
    } else {
        // Default: resolve relative to workspace path
        workspace_root.join(path)
    };

    // Canonicalize to resolve ".." and symlinks, then validate within workspace
    // For non-existent paths, we validate the parent directory exists and is within workspace
    let canonical = if resolved.exists() {
        resolved.canonicalize().map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to resolve path: {}", e),
            )
        })?
    } else {
        // For new files, check that the parent is within workspace
        let parent = resolved.parent().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Invalid path: no parent directory".to_string(),
            )
        })?;
        if !parent.exists() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Parent directory does not exist: {}", parent.display()),
            ));
        }
        let canonical_parent = parent.canonicalize().map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to resolve parent path: {}", e),
            )
        })?;
        // Reconstruct the path with canonical parent + filename
        if let Some(filename) = resolved.file_name() {
            canonical_parent.join(filename)
        } else {
            return Err((StatusCode::BAD_REQUEST, "Invalid path".to_string()));
        }
    };

    // Validate that the resolved path is within an allowed location
    // This can be either the workspace root or the global context directory for missions
    let context_root = state.config.working_dir.join("context");
    let in_workspace = canonical.starts_with(&workspace_root);
    let in_context = mission_id.is_some() && canonical.starts_with(&context_root);

    if !in_workspace && !in_context {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "Path traversal attempt: {} is outside allowed directories",
                canonical.display(),
            ),
        ));
    }

    Ok(canonical)
}

fn resolve_upload_base(path: &str) -> Result<PathBuf, (StatusCode, String)> {
    // Absolute path
    if Path::new(path).is_absolute() {
        // Remap /root/context to mission-specific context if available
        if path.starts_with("/root/context") {
            if let Some(state) = load_runtime_workspace() {
                if let Some(ctx) = state.mission_context {
                    let suffix = path.trim_start_matches("/root/context");
                    return Ok(PathBuf::from(ctx).join(suffix.trim_start_matches('/')));
                }
                if let Some(root) = state.context_root {
                    let suffix = path.trim_start_matches("/root/context");
                    return Ok(PathBuf::from(root).join(suffix.trim_start_matches('/')));
                }
            }
        }
        return Ok(PathBuf::from(path));
    }

    // Relative path -> resolve against current workspace working dir if known
    if let Some(state) = load_runtime_workspace() {
        if let Some(wd) = state
            .working_dir
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let base = PathBuf::from(wd);
            // For container workspaces, map container path back to host path
            if is_container_workspace(&state) {
                if let Some(mapped_base) = map_container_path_to_host(&base, &state) {
                    return Ok(mapped_base.join(path));
                }
            }
            return Ok(base.join(path));
        }

        // Fallback: use workspace root directly for container workspaces
        if is_container_workspace(&state) {
            if let Some(root) = workspace_root_path(&state) {
                return Ok(root.join(path));
            }
        }
    }

    Err((
        StatusCode::BAD_REQUEST,
        "Relative upload path requires an active workspace".to_string(),
    ))
}

/// Sanitize a path component to prevent path traversal attacks.
/// Removes directory separators and path traversal sequences.
fn sanitize_path_component(s: &str) -> String {
    // Take only the filename portion (after any path separator)
    let filename = s.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(s);

    // Remove any remaining path traversal patterns and null bytes
    filename
        .replace("..", "")
        .replace('\0', "")
        .trim()
        .to_string()
}

/// Validate a URL to prevent SSRF attacks.
/// Blocks requests to:
/// - localhost and loopback addresses (127.0.0.0/8, ::1)
/// - Private network ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
/// - Link-local addresses (169.254.0.0/16, fe80::/10)
/// - Cloud metadata endpoints (169.254.169.254)
fn validate_url_for_ssrf(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Disallowed URL scheme: {}", other)),
    }

    // Get the host
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;

    // Check for localhost variants
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".localhost") || host_lower == "0.0.0.0" {
        return Err("Requests to localhost are not allowed".to_string());
    }

    // Try to parse as IP address
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_internal_ip(&ip) {
            return Err(format!(
                "Requests to internal IP addresses are not allowed: {}",
                ip
            ));
        }
    }

    // Try DNS resolution to catch DNS rebinding attacks
    // (hostname that resolves to internal IP)
    if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 80u16)) {
        for addr in addrs {
            if is_internal_ip(&addr.ip()) {
                return Err(format!(
                    "URL resolves to internal IP address: {}",
                    addr.ip()
                ));
            }
        }
    }

    Ok(())
}

/// Check if an IP address is internal/private
fn is_internal_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            // Loopback (127.0.0.0/8)
            ipv4.is_loopback()
            // Private networks
            || ipv4.is_private()
            // Link-local (169.254.0.0/16)
            || ipv4.is_link_local()
            // Broadcast
            || ipv4.is_broadcast()
            // Documentation ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24)
            || ipv4.is_documentation()
            // Cloud metadata endpoint (169.254.169.254)
            || ipv4.octets() == [169, 254, 169, 254]
            // Unspecified (0.0.0.0)
            || ipv4.is_unspecified()
        }
        IpAddr::V6(ipv6) => {
            // Loopback (::1)
            ipv6.is_loopback()
            // Unspecified (::)
            || ipv6.is_unspecified()
            // IPv4-mapped addresses - check the embedded IPv4
            || {
                if let Some(ipv4) = ipv6.to_ipv4_mapped() {
                    is_internal_ip(&IpAddr::V4(ipv4))
                } else {
                    false
                }
            }
            // Unique local addresses (fc00::/7) - private in IPv6
            || (ipv6.segments()[0] & 0xfe00) == 0xfc00
            // Link-local (fe80::/10)
            || (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    pub path: String,
    /// Optional workspace ID to resolve relative paths against
    pub workspace_id: Option<uuid::Uuid>,
    /// Optional mission ID for mission-specific context directories
    pub mission_id: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct MkdirRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct RmRequest {
    pub path: String,
    pub recursive: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub kind: String, // file/dir/link/other
    pub size: u64,
    pub mtime: i64,
}

pub async fn list(
    State(_state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Vec<FsEntry>>, (StatusCode, String)> {
    let entries = list_directory_local(&q.path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(entries))
}

/// List directory contents locally (for localhost optimization)
async fn list_directory_local(path: &str) -> anyhow::Result<Vec<FsEntry>> {
    use std::os::unix::fs::MetadataExt;

    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(path).await?;

    while let Some(entry) = dir.next_entry().await? {
        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };

        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_symlink() {
            "link"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };

        let mtime = metadata.mtime();

        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            kind: kind.to_string(),
            size: metadata.len(),
            mtime,
        });
    }

    Ok(entries)
}

pub async fn mkdir(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<MkdirRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    tokio::fs::create_dir_all(&req.path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn rm(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<RmRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let recursive = req.recursive.unwrap_or(false);

    if recursive {
        tokio::fs::remove_dir_all(&req.path)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        tokio::fs::remove_file(&req.path)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Response, (StatusCode, String)> {
    let resolved_path = resolve_download_path(&q.path, Some(&state.config.working_dir))?;
    let filename = q
        .path
        .split('/')
        .last()
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", filename)
            .parse()
            .unwrap(),
    );
    headers.insert(
        header::CONTENT_TYPE,
        content_type_for_path(&resolved_path).parse().unwrap(),
    );

    let file = tokio::fs::File::open(&resolved_path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("File not found: {}", e)))?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok((headers, body).into_response())
}

pub async fn upload(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // If workspace_id is provided, resolve path relative to that workspace
    // If mission_id is also provided, context paths resolve to mission-specific directory
    let base = if let Some(workspace_id) = q.workspace_id {
        resolve_path_for_workspace(&state, workspace_id, &q.path, q.mission_id).await?
    } else {
        resolve_upload_base(&q.path)?
    };

    // Expect one file field.
    if let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        let file_name = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "upload.bin".to_string());
        // Stream to temp file first (avoid buffering large uploads in memory).
        let tmp = std::env::temp_dir().join(format!("sandboxed_sh_ul_{}", uuid::Uuid::new_v4()));
        let mut f = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
        {
            f.write_all(&chunk)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        f.flush()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let remote_path = if q.path.ends_with('/') {
            base.join(&file_name)
        } else {
            base.join(&file_name)
        };

        // Ensure the target directory exists
        let target_dir = remote_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| base.clone());

        tokio::fs::create_dir_all(&target_dir).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create directory: {}", e),
            )
        })?;

        // Try rename first (fast), fall back to copy+delete if across filesystems
        if tokio::fs::rename(&tmp, &remote_path).await.is_err() {
            tokio::fs::copy(&tmp, &remote_path).await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to copy file: {}", e),
                )
            })?;
            let _ = tokio::fs::remove_file(&tmp).await;
        }

        return Ok(Json(serde_json::json!({
            "ok": true,
            "path": remote_path,
            "name": file_name
        })));
    }

    Err((StatusCode::BAD_REQUEST, "missing file".to_string()))
}

// Chunked upload query params
#[derive(Debug, Deserialize)]
pub struct ChunkUploadQuery {
    pub path: String,
    pub upload_id: String,
    pub chunk_index: u32,
    pub total_chunks: u32,
    /// Optional workspace ID to resolve relative paths against
    pub workspace_id: Option<uuid::Uuid>,
}

// Handle chunked file upload
pub async fn upload_chunk(
    State(_state): State<Arc<AppState>>,
    Query(q): Query<ChunkUploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if q.path.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Invalid path".to_string()));
    }
    // Sanitize upload_id to prevent path traversal attacks
    let safe_upload_id = sanitize_path_component(&q.upload_id);
    if safe_upload_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Invalid upload_id".to_string()));
    }

    // Store chunks in temp directory organized by upload_id
    let chunk_dir = std::env::temp_dir().join(format!("sandboxed_sh_chunks_{}", safe_upload_id));
    tokio::fs::create_dir_all(&chunk_dir).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create chunk dir: {}", e),
        )
    })?;

    if let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        let chunk_path = chunk_dir.join(format!("chunk_{:06}", q.chunk_index));
        let mut f = tokio::fs::File::create(&chunk_path)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
        {
            f.write_all(&chunk)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        f.flush()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        return Ok(Json(serde_json::json!({
            "ok": true,
            "chunk_index": q.chunk_index,
            "total_chunks": q.total_chunks,
        })));
    }

    Err((StatusCode::BAD_REQUEST, "missing chunk data".to_string()))
}

#[derive(Debug, Deserialize)]
pub struct FinalizeUploadRequest {
    pub path: String,
    pub upload_id: String,
    pub file_name: String,
    pub total_chunks: u32,
    /// Optional workspace ID to resolve relative paths against
    pub workspace_id: Option<uuid::Uuid>,
    /// Optional mission ID for mission-specific context directories
    pub mission_id: Option<uuid::Uuid>,
}

// Finalize chunked upload by assembling chunks
pub async fn upload_finalize(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FinalizeUploadRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // If workspace_id is provided, resolve path relative to that workspace
    // If mission_id is also provided, context paths resolve to mission-specific directory
    let base = if let Some(workspace_id) = req.workspace_id {
        resolve_path_for_workspace(&state, workspace_id, &req.path, req.mission_id).await?
    } else {
        resolve_upload_base(&req.path)?
    };

    // Sanitize upload_id and file_name to prevent path traversal attacks
    let safe_upload_id = sanitize_path_component(&req.upload_id);
    if safe_upload_id.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Invalid upload_id".to_string()));
    }
    let safe_file_name = sanitize_path_component(&req.file_name);
    if safe_file_name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Invalid file_name".to_string()));
    }

    let chunk_dir = std::env::temp_dir().join(format!("sandboxed_sh_chunks_{}", safe_upload_id));
    let assembled_path =
        std::env::temp_dir().join(format!("sandboxed_sh_assembled_{}", safe_upload_id));

    // Assemble chunks into single file
    let mut assembled = tokio::fs::File::create(&assembled_path)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create assembled file: {}", e),
            )
        })?;

    for i in 0..req.total_chunks {
        let chunk_path = chunk_dir.join(format!("chunk_{:06}", i));
        let chunk_data = tokio::fs::read(&chunk_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read chunk {}: {}", i, e),
            )
        })?;
        assembled.write_all(&chunk_data).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write chunk {}: {}", i, e),
            )
        })?;
    }
    assembled
        .flush()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    drop(assembled);

    // Move assembled file to destination (using sanitized file_name)
    let remote_path = base.join(&safe_file_name);
    let target_dir = remote_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| base.clone());

    tokio::fs::create_dir_all(&target_dir).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create directory: {}", e),
        )
    })?;

    if tokio::fs::rename(&assembled_path, &remote_path)
        .await
        .is_err()
    {
        tokio::fs::copy(&assembled_path, &remote_path)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to copy file: {}", e),
                )
            })?;
        let _ = tokio::fs::remove_file(&assembled_path).await;
    }

    // Cleanup chunk directory
    let _ = tokio::fs::remove_dir_all(&chunk_dir).await;

    Ok(Json(
        serde_json::json!({ "ok": true, "path": req.path, "name": safe_file_name }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DownloadUrlRequest {
    pub url: String,
    pub path: String,
    pub file_name: Option<String>,
    /// Optional workspace ID to resolve relative paths against
    pub workspace_id: Option<uuid::Uuid>,
    /// Optional mission ID for mission-specific context directories
    pub mission_id: Option<uuid::Uuid>,
}

// Download file from URL to server filesystem
pub async fn download_from_url(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DownloadUrlRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Validate URL to prevent SSRF attacks
    validate_url_for_ssrf(&req.url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Download to temp file
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300)) // 5 min timeout
        // Don't follow redirects automatically to prevent redirect-based SSRF
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create HTTP client: {}", e),
            )
        })?;

    let response = client.get(&req.url).send().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to fetch URL: {}", e),
        )
    })?;

    // Validate the final URL after redirects to prevent redirect-based SSRF
    let final_url = response.url().to_string();
    validate_url_for_ssrf(&final_url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Redirect target blocked: {}", e),
        )
    })?;

    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("URL returned error: {}", response.status()),
        ));
    }

    // Try to get filename from Content-Disposition header or URL
    let raw_file_name = req.file_name.clone().unwrap_or_else(|| {
        response
            .headers()
            .get("content-disposition")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| {
                // Parse Content-Disposition header properly
                // Format: attachment; filename="report.pdf"; size=1234
                // or: attachment; filename=report.pdf
                s.split("filename=").nth(1).and_then(|after_filename| {
                    let trimmed = after_filename.trim();
                    if trimmed.starts_with('"') {
                        // Quoted filename: find the closing quote
                        trimmed
                            .get(1..)
                            .and_then(|s| s.split('"').next())
                            .map(|s| s.to_string())
                    } else if trimmed.starts_with('\'') {
                        // Single-quoted filename: find the closing quote
                        trimmed
                            .get(1..)
                            .and_then(|s| s.split('\'').next())
                            .map(|s| s.to_string())
                    } else {
                        // Unquoted filename: split on semicolon or whitespace
                        trimmed
                            .split(|c: char| c == ';' || c.is_whitespace())
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                    }
                })
            })
            .unwrap_or_else(|| {
                req.url
                    .split('/')
                    .last()
                    .and_then(|s| s.split('?').next())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("download_{}", uuid::Uuid::new_v4()))
            })
    });

    // Sanitize filename to prevent path traversal attacks
    let file_name = sanitize_path_component(&raw_file_name);
    let file_name = if file_name.is_empty() {
        format!("download_{}", uuid::Uuid::new_v4())
    } else {
        file_name
    };

    let tmp = std::env::temp_dir().join(format!("sandboxed_sh_url_{}", uuid::Uuid::new_v4()));
    let mut f = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let bytes = response.bytes().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read response: {}", e),
        )
    })?;

    f.write_all(&bytes)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    f.flush()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    drop(f);

    // Move to destination
    // If mission_id is provided, context paths resolve to mission-specific directory
    let base = if let Some(workspace_id) = req.workspace_id {
        resolve_path_for_workspace(&state, workspace_id, &req.path, req.mission_id).await?
    } else {
        resolve_upload_base(&req.path)?
    };
    let remote_path = base.join(&file_name);
    let target_dir = remote_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| base.clone());

    tokio::fs::create_dir_all(&target_dir).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create directory: {}", e),
        )
    })?;

    if tokio::fs::rename(&tmp, &remote_path).await.is_err() {
        tokio::fs::copy(&tmp, &remote_path).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to copy file: {}", e),
            )
        })?;
        let _ = tokio::fs::remove_file(&tmp).await;
    }

    Ok(Json(
        serde_json::json!({ "ok": true, "path": remote_path, "name": file_name }),
    ))
}
