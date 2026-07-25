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

fn upgrade_legacy_opus_default(mut config: Value) -> Value {
    if let Some(object) = config.as_object_mut() {
        for key in ["default_model", "model"] {
            let configured = object.get(key).and_then(Value::as_str).map(str::to_string);
            if configured.is_some() {
                object.insert(
                    key.to_string(),
                    Value::String(crate::library::normalize_claude_code_default_model(
                        configured,
                    )),
                );
            }
        }
    }
    config
}

/// GET /api/claudecode/config - Read Claude Code host settings.
pub async fn get_claudecode_config() -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_claudecode_config_path();
    read_json_config(&path, "Claude Code config")
        .await
        .map(upgrade_legacy_opus_default)
        .map(Json)
}

/// PUT /api/claudecode/config - Write Claude Code host settings.
pub async fn update_claudecode_config(
    Json(config): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let path = resolve_claudecode_config_path();
    let config = upgrade_legacy_opus_default(config);
    write_json_config(&path, &config, "Claude Code config").await?;
    Ok(Json(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrades_legacy_model_keys_without_overwriting_other_models() {
        let upgraded = upgrade_legacy_opus_default(serde_json::json!({
            "model": "claude-opus-4-8",
            "default_model": "anthropic/claude-opus-4.8"
        }));
        assert_eq!(upgraded["model"], "claude-opus-5");
        assert_eq!(upgraded["default_model"], "claude-opus-5");

        let explicit = upgrade_legacy_opus_default(serde_json::json!({
            "model": "claude-sonnet-5"
        }));
        assert_eq!(explicit["model"], "claude-sonnet-5");
    }
}
