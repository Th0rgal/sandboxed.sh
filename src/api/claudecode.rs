use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

fn resolve_claudecode_config_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CLAUDE_CONFIG") {
        if !path.trim().is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir).join("settings.json");
        }
    }

    let opencode_home = std::path::PathBuf::from("/var/lib/opencode")
        .join(".claude")
        .join("settings.json");
    if opencode_home.exists() {
        return opencode_home;
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home)
        .join(".claude")
        .join("settings.json")
}

fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    while let Some(n) = chars.next() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                    continue;
                }
                _ => {}
            }
        }

        out.push(c);
    }

    out
}

fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == ',' {
            let mut lookahead = chars.clone();
            while let Some(next) = lookahead.peek() {
                if next.is_whitespace() {
                    lookahead.next();
                } else {
                    break;
                }
            }
            if matches!(lookahead.peek(), Some('}') | Some(']')) {
                continue;
            }
        }

        out.push(c);
    }

    out
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
