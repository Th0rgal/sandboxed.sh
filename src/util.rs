//! Shared utility functions used across the codebase.

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
