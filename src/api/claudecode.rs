use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use crate::util::{resolve_config_path, strip_jsonc_comments, strip_trailing_commas};

fn resolve_claudecode_config_path() -> std::path::PathBuf {
    let has_env = std::env::var("CLAUDE_CONFIG").is_ok_and(|v| !v.trim().is_empty())
        || std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|v| !v.trim().is_empty());

    if has_env {
        return resolve_config_path(
            "CLAUDE_CONFIG",
            "CLAUDE_CONFIG_DIR",
            "settings.json",
            ".claude/settings.json",
        );
    }

    let opencode_home = std::path::PathBuf::from("/var/lib/opencode/.claude/settings.json");
    if opencode_home.exists() {
        return opencode_home;
    }

    resolve_config_path(
        "CLAUDE_CONFIG",
        "CLAUDE_CONFIG_DIR",
        "settings.json",
        ".claude/settings.json",
    )
}

/// GET /api/claudecode/config - Read Claude Code host settings.
pub async fn get_claudecode_config() -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_claudecode_config_path();

    if !config_path.exists() {
        return Ok(Json(serde_json::json!({})));
    }

    let contents = tokio::fs::read_to_string(&config_path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read Claude Code config: {}", e),
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
                format!("Invalid JSON in Claude Code config: {}", e),
            )
        })?;

    Ok(Json(config))
}

/// PUT /api/claudecode/config - Write Claude Code host settings.
pub async fn update_claudecode_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let config_path = resolve_claudecode_config_path();

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
                format!("Failed to write Claude Code config: {}", e),
            )
        })?;

    tracing::info!(path = %config_path.display(), "Updated Claude Code config");

    Ok(Json(config))
}
