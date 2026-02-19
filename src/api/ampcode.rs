use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use crate::util::{read_json_config, resolve_config_path, write_json_config};

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
    let path = resolve_amp_config_path();
    read_json_config(&path, "Amp config").await.map(Json)
}

/// PUT /api/amp/config - Write Amp host settings.
pub async fn update_amp_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_amp_config_path();
    write_json_config(&path, &config, "Amp config").await?;
    Ok(Json(config))
}
