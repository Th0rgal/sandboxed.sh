/// Returns true when `text` is exactly `name` or `name` followed by arguments.
pub fn is_command(text: &str, name: &str) -> bool {
    let Some((command, _tail)) = split_command(text) else {
        return false;
    };
    command_name(command) == name
}

/// Returns true only when `text` invokes `name` without arguments.
pub fn is_exact_command(text: &str, name: &str) -> bool {
    let Some((command, tail)) = split_command(text) else {
        return false;
    };
    command_name(command) == name && tail.is_empty()
}

/// Keep the first Paloma natural-language command mapping deterministic.
pub fn normalize_natural_command(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.starts_with('/') || trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let mentions_missions = lower.contains("mission");
    let asks_for_mission_list = mentions_missions
        && (lower.contains("en cours")
            || lower.contains("en ce moment")
            || lower.contains("actif")
            || lower.contains("active")
            || lower.contains("liste")
            || lower.contains("quelles")
            || lower.contains("lesquelles")
            || lower.contains("quoi")
            || lower.contains("voir")
            || lower.contains("list")
            || lower.contains("show")
            || lower.contains("current")
            || lower.contains("running")
            || lower.contains("montre"));
    if asks_for_mission_list {
        return Some("/missions");
    }

    if lower == "status"
        || lower == "statut"
        || lower.starts_with("status ")
        || lower.starts_with("statut ")
        || lower.contains("statut")
        || lower.contains("update me")
        || lower.contains("update moi")
        || lower.contains("mets moi a jour")
        || lower.contains("mets-moi a jour")
        || lower.contains("mets moi à jour")
        || lower.contains("mets-moi à jour")
        || lower.contains("quoi de neuf")
        || lower.contains("nouveau")
        || lower.contains("what changed")
    {
        return Some("/status");
    }
    None
}

pub fn parse_selector_and_payload<'a>(text: &'a str, command: &str) -> Option<(&'a str, &'a str)> {
    let (actual_command, tail) = split_command(text)?;
    if command_name(actual_command) != command {
        return None;
    }
    let (selector, payload) = tail.split_once(char::is_whitespace)?;
    let payload = payload.trim();
    if selector.trim().is_empty() || payload.is_empty() {
        None
    } else {
        Some((selector.trim(), payload))
    }
}

fn split_command(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    match trimmed.find(char::is_whitespace) {
        Some(idx) => Some((&trimmed[..idx], trimmed[idx..].trim())),
        None => Some((trimmed, "")),
    }
}

fn command_name(command: &str) -> &str {
    command
        .split_once('@')
        .map(|(name, _)| name)
        .unwrap_or(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_matching_requires_boundary() {
        assert!(is_command("/status", "/status"));
        assert!(is_command("/status latest", "/status"));
        assert!(is_command("/status@paloma_test_bot latest", "/status"));
        assert!(!is_command("/statusplease", "/status"));
    }

    #[test]
    fn exact_command_allows_bot_suffix_without_arguments() {
        assert!(is_exact_command("/summary", "/summary"));
        assert!(is_exact_command("/summary@paloma_test_bot", "/summary"));
        assert!(!is_exact_command("/summary latest", "/summary"));
        assert!(!is_exact_command(
            "/summary@paloma_test_bot latest",
            "/summary"
        ));
    }

    #[test]
    fn selector_parser_allows_bot_suffix() {
        assert_eq!(
            parse_selector_and_payload("/send@paloma_test_bot latest go", "/send"),
            Some(("latest", "go"))
        );
    }
}
