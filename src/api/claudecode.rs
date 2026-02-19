use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use crate::util::{home_dir, read_json_config, resolve_config_path, write_json_config};

fn resolve_claudecode_config_path() -> std::path::PathBuf {
    // If an explicit env var is set, honour it (no probing needed).
    if std::env::var("CLAUDE_CONFIG").is_ok_and(|v| !v.trim().is_empty())
        || std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|v| !v.trim().is_empty())
    {
        return resolve_config_path(
            "CLAUDE_CONFIG",
            "CLAUDE_CONFIG_DIR",
            "settings.json",
            ".claude/settings.json",
        );
    }

    // Container path takes precedence over the home-dir default.
    let opencode_home = std::path::PathBuf::from("/var/lib/opencode/.claude/settings.json");
    if opencode_home.exists() {
        return opencode_home;
    }

    std::path::PathBuf::from(home_dir()).join(".claude/settings.json")
}

/// GET /api/claudecode/config - Read Claude Code host settings.
pub async fn get_claudecode_config() -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_claudecode_config_path();
    read_json_config(&path, "Claude Code config")
        .await
        .map(Json)
}

/// PUT /api/claudecode/config - Write Claude Code host settings.
pub async fn update_claudecode_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_claudecode_config_path();
    write_json_config(&path, &config, "Claude Code config").await?;
    Ok(Json(config))
}
