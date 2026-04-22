//! Shared utility functions used across the codebase.

/// Relative path from a working directory to the AI providers config file.
pub const AI_PROVIDERS_PATH: &str = ".sandboxed-sh/ai_providers.json";

/// Parse an environment variable as a boolean, returning `default` if unset.
///
/// Recognises `1`, `true`, `yes`, `y`, `on` (case-insensitive) as `true`;
/// everything else (including unset) maps to `default`.
pub fn env_var_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "y" | "on"
        ),
        Err(_) => default,
    }
}

/// Return the value of `$HOME`, falling back to `/root`.
pub fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
}

/// Read an environment variable, returning `Some(trimmed)` only when the
/// variable is set *and* non-empty after trimming whitespace. Callers that
/// chain several aliases via `or_else` need this to skip templated blank
/// values — otherwise the first alias wins with an empty string and later
/// aliases never get a chance.
pub fn env_var_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// CLI-proxy base URL aliases, in priority order.
///
/// Shared across `ai_providers.rs`, `proxy.rs`, and `mission_runner.rs` so
/// that every lookup path agrees on the same precedence. Callers should
/// walk these with `env_var_nonempty` so blank values fall through.
pub const CLI_PROXY_BASE_URL_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE_PROXY_BASE_URL",
    "CLI_PROXY_API_BASE_URL",
    "CLIPROXY_API_BASE_URL",
    "CLIPROXY_BASE_URL",
];

/// CLI-proxy API key aliases, in priority order.
pub const CLI_PROXY_API_KEY_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE_PROXY_API_KEY",
    "CLI_PROXY_API_KEY",
    "CLIPROXY_API_KEY",
];

/// First non-empty CLI-proxy base URL found among the alias env vars.
pub fn cli_proxy_base_url_from_env() -> Option<String> {
    CLI_PROXY_BASE_URL_ENV_VARS
        .iter()
        .find_map(|name| env_var_nonempty(name))
}

/// First non-empty CLI-proxy API key found among the alias env vars.
pub fn cli_proxy_api_key_from_env() -> Option<String> {
    CLI_PROXY_API_KEY_ENV_VARS
        .iter()
        .find_map(|name| env_var_nonempty(name))
}

/// True when any CLI-proxy base URL or API key env var is configured
/// (non-empty). Used by the availability gates to decide whether the
/// synthetic `*-cli-proxy` accounts are worth adding.
pub fn any_cli_proxy_env_configured() -> bool {
    CLI_PROXY_BASE_URL_ENV_VARS
        .iter()
        .chain(CLI_PROXY_API_KEY_ENV_VARS.iter())
        .any(|name| env_var_nonempty(name).is_some())
}

/// Build a truncated context string from conversation history.
///
/// Walks `history` from most-recent to oldest, accumulating entries until
/// `max_chars` is reached. The most-recent entry is always included.
pub fn build_history_context(history: &[(String, String)], max_chars: usize) -> String {
    let mut result = String::new();
    let mut total_chars = 0;
    for (role, content) in history.iter().rev() {
        let entry = format!("{}: {}\n\n", role.to_uppercase(), content);
        if total_chars + entry.len() > max_chars && !result.is_empty() {
            break;
        }
        result = format!("{}{}", entry, result);
        total_chars += entry.len();
    }
    result
}

/// Deduplicate and trim a list of skill names, preserving order.
pub fn sanitize_skill_list(skills: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for skill in skills {
        let trimmed = skill.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Strip `//` and `/* */` comments from a JSONC string, preserving string literals.
pub fn strip_jsonc_comments(input: &str) -> String {
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
                    for n in chars.by_ref() {
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
                    for n in chars.by_ref() {
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

/// Strip trailing commas before `}` and `]` in a JSON-like string, preserving string literals.
pub fn strip_trailing_commas(input: &str) -> String {
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

/// Resolve a config file path using standard precedence:
/// 1. `full_path_var` env var (if set, use as full path)
/// 2. `dir_var` env var + `filename` (if set, use as directory)
/// 3. `$HOME` / `default_rel_path`
pub fn resolve_config_path(
    full_path_var: &str,
    dir_var: &str,
    filename: &str,
    default_rel_path: &str,
) -> std::path::PathBuf {
    if let Ok(path) = std::env::var(full_path_var) {
        if !path.trim().is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    if let Ok(dir) = std::env::var(dir_var) {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir).join(filename);
        }
    }
    std::path::PathBuf::from(home_dir()).join(default_rel_path)
}

/// Read a JSON/JSONC config file, returning `{}` if it doesn't exist.
///
/// On parse failure, retries after stripping JSONC comments and trailing commas.
/// `label` is used in error messages (e.g. "Amp config").
pub async fn read_json_config(
    path: &std::path::Path,
    label: &str,
) -> Result<serde_json::Value, (axum::http::StatusCode, String)> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }

    let contents = tokio::fs::read_to_string(path).await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read {label}: {e}"),
        )
    })?;

    serde_json::from_str(&contents)
        .or_else(|_| {
            let cleaned = strip_trailing_commas(&strip_jsonc_comments(&contents));
            serde_json::from_str(&cleaned)
        })
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid JSON in {label}: {e}"),
            )
        })
}

/// Write a JSON config file, creating parent directories as needed.
///
/// `label` is used in the log message (e.g. "Amp config").
pub async fn write_json_config(
    path: &std::path::Path,
    config: &serde_json::Value,
    label: &str,
) -> Result<(), (axum::http::StatusCode, String)> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create config directory: {e}"),
            )
        })?;
    }

    let contents = serde_json::to_string_pretty(config).map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("Invalid JSON: {e}"),
        )
    })?;

    tokio::fs::write(path, contents).await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write {label}: {e}"),
        )
    })?;

    tracing::info!(path = %path.display(), "Updated {label}");
    Ok(())
}

/// Check whether a JSON auth entry contains any recognised credential field.
///
/// Looks for API keys (`key`, `api_key`, `apiKey`) and OAuth tokens
/// (`refresh`, `refresh_token`, `access`, `access_token`).
pub fn auth_entry_has_credentials(value: &serde_json::Value) -> bool {
    value.get("key").is_some()
        || value.get("api_key").is_some()
        || value.get("apiKey").is_some()
        || value.get("refresh").is_some()
        || value.get("refresh_token").is_some()
        || value.get("access").is_some()
        || value.get("access_token").is_some()
}

/// Map any error into an HTTP 500 response.
pub fn internal_error(e: impl std::fmt::Display) -> (axum::http::StatusCode, String) {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Map an error to 404 if the message contains "not found", otherwise 500.
pub fn not_found_or_internal(e: impl std::fmt::Display) -> (axum::http::StatusCode, String) {
    let msg = e.to_string();
    if msg.contains("not found") {
        (axum::http::StatusCode::NOT_FOUND, msg)
    } else {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}

/// Maximum recursion depth for `copy_dir_recursive`.
const MAX_COPY_DEPTH: u32 = 32;

/// Recursively copy a directory, preserving symlinks as-is (symlink-safe with depth limit).
///
/// Uses `symlink_metadata` to avoid following symlinks into loops and caps
/// recursion at [`MAX_COPY_DEPTH`] to prevent runaway traversal.
pub async fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> anyhow::Result<()> {
    copy_dir_recursive_inner(src, dst, 0, &[]).await
}

/// Like [`copy_dir_recursive`] but skips directories whose name matches any entry in `skip`.
pub async fn copy_dir_recursive_skip(
    src: &std::path::Path,
    dst: &std::path::Path,
    skip: &[&str],
) -> anyhow::Result<()> {
    copy_dir_recursive_inner(src, dst, 0, skip).await
}

#[async_recursion::async_recursion]
async fn copy_dir_recursive_inner(
    src: &std::path::Path,
    dst: &std::path::Path,
    depth: u32,
    skip: &'async_recursion [&'async_recursion str],
) -> anyhow::Result<()> {
    if depth > MAX_COPY_DEPTH {
        anyhow::bail!(
            "copy_dir_recursive: exceeded max depth ({}) at {:?} — possible symlink loop",
            MAX_COPY_DEPTH,
            src
        );
    }

    tokio::fs::create_dir_all(dst).await?;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let entry_path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dst.join(&file_name);

        // Skip directories by name (e.g. ".git")
        if !skip.is_empty() {
            if let Some(name) = file_name.to_str() {
                if skip.contains(&name) {
                    continue;
                }
            }
        }

        // Use symlink_metadata to avoid following symlinks into loops
        let metadata = tokio::fs::symlink_metadata(&entry_path).await?;
        if metadata.is_symlink() {
            // Copy symlink as-is rather than following it
            let target = tokio::fs::read_link(&entry_path).await?;
            let _ = tokio::fs::remove_file(&dest_path).await;
            tokio::fs::symlink(&target, &dest_path).await?;
        } else if metadata.is_dir() {
            copy_dir_recursive_inner(&entry_path, &dest_path, depth + 1, skip).await?;
        } else {
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&entry_path, &dest_path).await?;
        }
    }

    Ok(())
}

/// Returns `true` if the given IO error is an ELOOP (too many levels of symbolic links).
pub fn is_eloop(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_history_context_formats_entries() {
        let history = vec![
            ("user".to_string(), "hello".to_string()),
            ("assistant".to_string(), "world".to_string()),
        ];
        let result = build_history_context(&history, 10000);
        assert!(result.contains("USER: hello"));
        assert!(result.contains("ASSISTANT: world"));
    }

    #[test]
    fn build_history_context_respects_max_chars() {
        let history = vec![
            ("user".to_string(), "first message".to_string()),
            ("assistant".to_string(), "second message".to_string()),
            ("user".to_string(), "third message".to_string()),
        ];
        let result = build_history_context(&history, 30);
        assert!(result.contains("USER: third message"));
    }

    #[test]
    fn build_history_context_empty_history() {
        let history: Vec<(String, String)> = vec![];
        let result = build_history_context(&history, 10000);
        assert_eq!(result, "");
    }

    #[test]
    fn build_history_context_always_includes_most_recent() {
        let history = vec![(
            "user".to_string(),
            "a very long message that exceeds the max".to_string(),
        )];
        let result = build_history_context(&history, 5);
        assert!(result.contains("USER: a very long message"));
    }

    #[test]
    fn strip_jsonc_comments_removes_line_comments() {
        let input = r#"{"key": "value" // comment
}"#;
        let result = strip_jsonc_comments(input);
        assert_eq!(result, "{\"key\": \"value\" \n}");
    }

    #[test]
    fn strip_jsonc_comments_removes_block_comments() {
        let input = r#"{"key": /* block */ "value"}"#;
        let result = strip_jsonc_comments(input);
        assert_eq!(result, r#"{"key":  "value"}"#);
    }

    #[test]
    fn strip_jsonc_comments_preserves_strings() {
        let input = r#"{"url": "https://example.com"}"#;
        let result = strip_jsonc_comments(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strip_trailing_commas_removes_commas_before_braces() {
        assert_eq!(strip_trailing_commas(r#"{"a": 1,}"#), r#"{"a": 1}"#);
        assert_eq!(strip_trailing_commas(r#"[1, 2,]"#), r#"[1, 2]"#);
    }

    #[test]
    fn strip_trailing_commas_preserves_commas_in_strings() {
        assert_eq!(strip_trailing_commas(r#"{"a": "b,}"}"#), r#"{"a": "b,}"}"#);
    }

    #[test]
    fn sanitize_skill_list_deduplicates_and_trims() {
        let skills = vec![
            " foo ".to_string(),
            "bar".to_string(),
            "foo".to_string(),
            "".to_string(),
            "  ".to_string(),
            "bar".to_string(),
        ];
        assert_eq!(sanitize_skill_list(skills), vec!["foo", "bar"]);
    }
}
