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
