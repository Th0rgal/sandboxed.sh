use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use crate::util::{resolve_config_path, strip_jsonc_comments, strip_trailing_commas};

fn resolve_amp_config_path() -> std::path::PathBuf {
    resolve_config_path(
        "AMP_CONFIG",
        "AMP_CONFIG_DIR",
        "settings.json",
        ".config/amp/settings.json",
    )
}

/// GET /api/amp/config - Read Amp host settings.
pub async fn get_amp_config() -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_amp_config_path();

    if !config_path.exists() {
        return Ok(Json(serde_json::json!({})));
    }

    let contents = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read Amp config: {}", e),
        )
    })?;

    let config: Value = serde_json::from_str(&contents)
        .or_else(|_| {
            let stripped = strip_jsonc_comments(&contents);
            let cleaned = strip_trailing_commas(&stripped);
            serde_json::from_str(&cleaned)
        })
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid JSON in Amp config: {}", e),
            )
        })?;

    Ok(Json(config))
}

/// PUT /api/amp/config - Write Amp host settings.
pub async fn update_amp_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_amp_config_path();

    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create config directory: {}", e),
            )
        })?;
    }

    let contents = serde_json::to_string_pretty(&config)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)))?;

    tokio::fs::write(&config_path, contents)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write Amp config: {}", e),
            )
        })?;

    tracing::info!(path = %config_path.display(), "Updated Amp config");

    Ok(Json(config))
}
