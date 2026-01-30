//! API endpoints for global settings management.

use std::io::{Read as IoRead, Write as IoWrite};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post, put},
    Router,
};
use serde::{Deserialize, Serialize};

use crate::settings::Settings;
use crate::workspace;

use super::routes::AppState;

/// Create the settings API routes.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_settings).put(update_settings))
        .route("/library-remote", put(update_library_remote))
        .route("/backup", get(download_backup))
        .route("/restore", post(restore_backup))
}

/// Response for settings endpoints.
#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub library_remote: Option<String>,
}

impl From<Settings> for SettingsResponse {
    fn from(settings: Settings) -> Self {
        Self {
            library_remote: settings.library_remote,
        }
    }
}

/// Request to update all settings.
#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub library_remote: Option<String>,
}

/// Request to update library remote specifically.
#[derive(Debug, Deserialize)]
pub struct UpdateLibraryRemoteRequest {
    /// Git remote URL. Set to null or empty string to clear.
    pub library_remote: Option<String>,
}

/// Response after updating library remote.
#[derive(Debug, Serialize)]
pub struct UpdateLibraryRemoteResponse {
    pub library_remote: Option<String>,
    /// Whether the library was reinitialized.
    pub library_reinitialized: bool,
    /// Error message if library initialization failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_error: Option<String>,
}

/// GET /api/settings
/// Get all settings.
async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsResponse> {
    let settings = state.settings.get().await;
    Json(settings.into())
}

/// PUT /api/settings
/// Update all settings.
async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    let new_settings = Settings {
        library_remote: req.library_remote,
    };

    state
        .settings
        .update(new_settings.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(new_settings.into()))
}

/// PUT /api/settings/library-remote
/// Update the library remote URL and optionally reinitialize the library.
async fn update_library_remote(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateLibraryRemoteRequest>,
) -> Result<Json<UpdateLibraryRemoteResponse>, (StatusCode, String)> {
    // Normalize empty string to None
    let new_remote = req.library_remote.filter(|s| !s.trim().is_empty());

    // Update the setting
    let (changed, _previous) = state
        .settings
        .set_library_remote(new_remote.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // If the value actually changed, reinitialize the library
    let (library_reinitialized, library_error) = if changed {
        if let Some(ref remote) = new_remote {
            // Reinitialize with new remote
            match reinitialize_library(&state, remote).await {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e)),
            }
        } else {
            // Clear the library
            *state.library.write().await = None;
            tracing::info!("Library cleared (remote set to None)");
            (true, None)
        }
    } else {
        // No change in value
        (false, None)
    };

    Ok(Json(UpdateLibraryRemoteResponse {
        library_remote: new_remote,
        library_reinitialized,
        library_error,
    }))
}

/// Reinitialize the library with a new remote URL.
async fn reinitialize_library(state: &Arc<AppState>, remote: &str) -> Result<(), String> {
    let library_path = state.config.library_path.clone();

    match crate::library::LibraryStore::new(library_path, remote).await {
        Ok(store) => {
            // Sync OpenCode plugins
            if let Ok(plugins) = store.get_plugins().await {
                if let Err(e) = crate::opencode_config::sync_global_plugins(&plugins).await {
                    tracing::warn!("Failed to sync OpenCode plugins: {}", e);
                }
            }

            tracing::info!("Configuration library reinitialized from {}", remote);
            let library = Arc::new(store);
            *state.library.write().await = Some(Arc::clone(&library));

            // Sync skills/tools to all workspaces
            let workspaces = state.workspaces.list().await;
            for ws in workspaces {
                let is_default_host = ws.id == workspace::DEFAULT_WORKSPACE_ID
                    && ws.workspace_type == workspace::WorkspaceType::Host;

                if is_default_host || !ws.skills.is_empty() {
                    if let Err(e) = workspace::sync_workspace_skills(&ws, &library).await {
                        tracing::warn!(
                            workspace = %ws.name,
                            error = %e,
                            "Failed to sync skills after library reinit"
                        );
                    }
                }
            }

            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to reinitialize library from {}: {}", remote, e);
            Err(e.to_string())
        }
    }
}

// ============================================
// Backup & Restore
// ============================================

/// Files included in the backup (relative to .sandboxed-sh/)
const BACKUP_FILES: &[&str] = &[
    "settings.json",
    "ai_providers.json",
    "backend_config.json",
    "workspaces.json",
    "mcp/config.json",
    "private_key",
];

/// Directories included in the backup (relative to .sandboxed-sh/)
const BACKUP_DIRS: &[&str] = &["secrets"];

/// Find Claude credentials file from various possible locations.
/// Returns the path and archive name if found.
fn find_claude_credentials() -> Option<(std::path::PathBuf, &'static str)> {
    // Check locations in order of preference
    let locations = [
        // OpenCode isolated home (used when OPENCODE_CONFIG_DIR is set)
        (
            "/var/lib/opencode/.claude/.credentials.json",
            ".claude/.credentials.json",
        ),
        // Standard root home
        (
            "/root/.claude/.credentials.json",
            ".claude/.credentials.json",
        ),
    ];

    for (path, archive_name) in locations {
        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Some((path, archive_name));
        }
    }

    // Check HOME environment variable
    if let Ok(home) = std::env::var("HOME") {
        let path = std::path::PathBuf::from(home).join(".claude/.credentials.json");
        if path.exists() {
            return Some((path, ".claude/.credentials.json"));
        }
    }

    None
}

/// GET /api/settings/backup
/// Download a backup archive of all settings files.
async fn download_backup(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let sandboxed_dir = state.config.working_dir.join(".sandboxed-sh");

    // Create a zip archive in memory
    let mut zip_buffer = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buffer));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        // Add individual files from .sandboxed-sh/
        for file in BACKUP_FILES {
            let file_path = sandboxed_dir.join(file);
            if file_path.exists() {
                if let Ok(contents) = std::fs::read_to_string(&file_path) {
                    if let Err(e) = zip.start_file(format!(".sandboxed-sh/{}", file), options) {
                        tracing::warn!("Failed to add {} to backup: {}", file, e);
                        continue;
                    }
                    if let Err(e) = zip.write_all(contents.as_bytes()) {
                        tracing::warn!("Failed to write {} to backup: {}", file, e);
                    }
                }
            }
        }

        // Add directories recursively
        for dir in BACKUP_DIRS {
            let dir_path = sandboxed_dir.join(dir);
            if dir_path.exists() && dir_path.is_dir() {
                add_directory_to_zip(
                    &mut zip,
                    &dir_path,
                    &format!(".sandboxed-sh/{}", dir),
                    options,
                )
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to add directory {} to backup: {}", dir, e),
                    )
                })?;
            }
        }

        // Add Claude credentials file if it exists
        if let Some((creds_path, archive_name)) = find_claude_credentials() {
            if let Ok(contents) = std::fs::read_to_string(&creds_path) {
                if let Err(e) = zip.start_file(archive_name, options) {
                    tracing::warn!("Failed to add Claude credentials to backup: {}", e);
                } else if let Err(e) = zip.write_all(contents.as_bytes()) {
                    tracing::warn!("Failed to write Claude credentials to backup: {}", e);
                } else {
                    tracing::info!(
                        "Added Claude credentials to backup from {}",
                        creds_path.display()
                    );
                }
            }
        }

        zip.finish().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to finalize backup archive: {}", e),
            )
        })?;
    }

    // Generate filename with timestamp
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let filename = format!("sandboxed-backup-{}.zip", timestamp);
    let content_disposition = format!("attachment; filename=\"{}\"", filename);

    let body = Body::from(zip_buffer);
    let headers = [
        (header::CONTENT_TYPE, "application/zip".to_string()),
        (header::CONTENT_DISPOSITION, content_disposition),
    ];

    Ok((headers, body))
}

/// Recursively add a directory to a zip archive.
fn add_directory_to_zip<W: IoWrite + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    dir_path: &std::path::Path,
    archive_prefix: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let archive_path = format!("{}/{}", archive_prefix, name.to_string_lossy());

        if path.is_dir() {
            add_directory_to_zip(zip, &path, &archive_path, options)?;
        } else if path.is_file() {
            let mut file = std::fs::File::open(&path)?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;

            zip.start_file(&archive_path, options)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            zip.write_all(&contents)?;
        }
    }
    Ok(())
}

/// Response after restoring backup.
#[derive(Debug, Serialize)]
pub struct RestoreBackupResponse {
    pub success: bool,
    pub message: String,
    pub restored_files: Vec<String>,
    pub errors: Vec<String>,
}

/// POST /api/settings/restore
/// Restore settings from an uploaded backup archive.
async fn restore_backup(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<RestoreBackupResponse>, (StatusCode, String)> {
    let sandboxed_dir = state.config.working_dir.join(".sandboxed-sh");

    // Extract the uploaded file
    let mut archive_data: Option<Vec<u8>> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to read multipart field: {}", e),
        )
    })? {
        if field.name() == Some("backup") || field.name() == Some("file") {
            let data = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Failed to read file data: {}", e),
                )
            })?;
            archive_data = Some(data.to_vec());
            break;
        }
    }

    let archive_data = archive_data.ok_or((
        StatusCode::BAD_REQUEST,
        "No backup file provided. Expected field 'backup' or 'file'.".to_string(),
    ))?;

    // Open the zip archive
    let cursor = std::io::Cursor::new(archive_data);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid zip archive: {}", e),
        )
    })?;

    let mut restored_files = Vec::new();
    let mut errors = Vec::new();

    // Determine Claude credentials restore path
    // Prefer /var/lib/opencode/.claude if it exists (for isolated OpenCode home), else use /root/.claude
    let claude_creds_dir = if std::path::Path::new("/var/lib/opencode/.claude").exists() {
        std::path::PathBuf::from("/var/lib/opencode/.claude")
    } else {
        std::path::PathBuf::from("/root/.claude")
    };

    // Extract files
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read archive entry: {}", e),
            )
        })?;

        let name = file.name().to_string();

        // Determine target path based on archive name
        let (target_path, display_name) = if name.starts_with(".sandboxed-sh/") {
            // Standard .sandboxed-sh files
            let relative_path = name.strip_prefix(".sandboxed-sh/").unwrap_or(&name);
            if relative_path.is_empty() {
                continue;
            }
            (sandboxed_dir.join(relative_path), relative_path.to_string())
        } else if name == ".claude/.credentials.json" {
            // Claude credentials file - restore to the appropriate .claude directory
            (claude_creds_dir.join(".credentials.json"), name.clone())
        } else {
            // Skip unknown files
            continue;
        };

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                errors.push(format!("Failed to create directory for {}: {}", name, e));
                continue;
            }
        }

        // Skip directories (they're created automatically)
        if file.is_dir() {
            continue;
        }

        // Read and write the file
        let mut contents = Vec::new();
        if let Err(e) = file.read_to_end(&mut contents) {
            errors.push(format!("Failed to read {}: {}", name, e));
            continue;
        }

        match std::fs::write(&target_path, &contents) {
            Ok(()) => {
                restored_files.push(display_name);
                tracing::info!("Restored: {} -> {}", name, target_path.display());
            }
            Err(e) => {
                errors.push(format!("Failed to write {}: {}", name, e));
            }
        }
    }

    // Reload settings stores after restore
    if restored_files.iter().any(|f| f == "settings.json") {
        if let Err(e) = state.settings.reload().await {
            errors.push(format!("Failed to reload settings: {}", e));
        }
    }

    // Load restored encryption key into the process environment
    if restored_files.iter().any(|f| f == "private_key") {
        let key_path = sandboxed_dir.join("private_key");
        match std::fs::read_to_string(&key_path) {
            Ok(key_hex) => {
                let trimmed = key_hex.trim();
                if !trimmed.is_empty() {
                    if let Err(e) = crate::library::env_crypto::set_private_key_hex(trimmed).await {
                        errors.push(format!("Failed to activate restored encryption key: {}", e));
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read restored private_key: {}", e));
            }
        }
    }

    // Note: Other stores (ai_providers, backend_configs, etc.) would need similar reload
    // methods to be implemented for a complete hot-reload. For now, a server restart
    // may be needed to pick up restored credentials.

    let success = !restored_files.is_empty() && errors.is_empty();
    let message = if success {
        format!(
            "Successfully restored {} files. A server restart may be required to apply credential changes.",
            restored_files.len()
        )
    } else if restored_files.is_empty() {
        "No files were restored. The backup may be empty or invalid.".to_string()
    } else {
        format!(
            "Restored {} files with {} errors. A server restart may be required.",
            restored_files.len(),
            errors.len()
        )
    };

    Ok(Json(RestoreBackupResponse {
        success,
        message,
        restored_files,
        errors,
    }))
}
