//! Shared helpers for `/goal <objective>` slash-command routing.
//!
//! Both backend implementations (codex's native `thread/goal/set` driver and
//! Claude Code's continuation loop) detect a `/goal ` prefix the same way, so
//! the parser lives here to keep the two paths in lockstep.

/// Strip a leading `/goal ` prefix from a user message.
///
/// Returns `(is_goal_mission, payload)`. Single-pass via `strip_prefix`
/// rather than greedy `trim_start_matches`, so a literal `/goal ` inside
/// the objective survives. Leading whitespace before `/goal ` is tolerated.
pub fn parse_goal_prefix(message: &str) -> (bool, String) {
    let trimmed = message.trim_start();
    match trimmed.strip_prefix("/goal ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, message.to_string()),
    }
}

/// Return the objective if `message` is a `/goal <objective>` command with a
/// non-empty objective, otherwise `None`.
pub fn parse_goal_objective(message: &str) -> Option<String> {
    let (is_goal, payload) = parse_goal_prefix(message);
    if is_goal && !payload.is_empty() {
        Some(payload)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_goal_prefix_detects_simple_goal() {
        let (is_goal, payload) = parse_goal_prefix("/goal create file foo");
        assert!(is_goal);
        assert_eq!(payload, "create file foo");
    }

    #[test]
    fn parse_goal_prefix_handles_leading_whitespace() {
        let (is_goal, payload) = parse_goal_prefix("   /goal do the thing");
        assert!(is_goal);
        assert_eq!(payload, "do the thing");
    }

    #[test]
    fn parse_goal_prefix_preserves_inner_goal_literal() {
        // `trim_start_matches` would strip both prefixes; `strip_prefix` is
        // single-pass — the inner `/goal ` must survive.
        let (is_goal, payload) = parse_goal_prefix("/goal /goal explain why this is a bad idea");
        assert!(is_goal);
        assert_eq!(payload, "/goal explain why this is a bad idea");
    }

    #[test]
    fn parse_goal_prefix_ignores_unprefixed_messages() {
        let (is_goal, payload) = parse_goal_prefix("hello world");
        assert!(!is_goal);
        assert_eq!(payload, "hello world");
    }

    #[test]
    fn parse_goal_prefix_requires_trailing_space() {
        // Bare "/goal" without a trailing space should NOT be treated as
        // a goal — the user might mean a literal `/goal` token.
        let (is_goal, payload) = parse_goal_prefix("/goal");
        assert!(!is_goal);
        assert_eq!(payload, "/goal");
    }

    #[test]
    fn parse_goal_objective_returns_none_for_non_goal() {
        assert_eq!(parse_goal_objective("just chatting"), None);
    }

    #[test]
    fn parse_goal_objective_returns_none_for_empty_objective() {
        // `/goal ` with only whitespace after — caller still gets None so it
        // can surface a usage error to the user.
        assert_eq!(parse_goal_objective("/goal   "), None);
    }

    #[test]
    fn parse_goal_objective_returns_objective() {
        assert_eq!(
            parse_goal_objective("/goal fix the build"),
            Some("fix the build".to_string())
        );
    }
}
