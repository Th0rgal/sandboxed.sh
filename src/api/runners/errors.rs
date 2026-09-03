//! Provider/CLI error classification shared by all harness turn runners.
//!
//! Every predicate works on raw harness output (stderr lines, CLI result
//! text, structured provider error payloads) and answers one question:
//! "what kind of failure is this?" — driving retry, account rotation, and
//! TerminalReason classification in the orchestration layer.
//!
//! Moved verbatim from `mission_runner.rs`; behavior changes belong in
//! separate commits guarded by the marker tests below.

// ── ASCII case-insensitive matching helpers ────────────────────────────
//
// Hand-rolled instead of `to_lowercase()` so classification of multi-MB
// outputs doesn't allocate.

#[inline]
pub(crate) fn ascii_lower(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte + 32,
        _ => byte,
    }
}

pub(crate) fn starts_with_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }

    haystack[..needle.len()]
        .iter()
        .zip(needle.iter())
        .all(|(&left, &right)| ascii_lower(left) == ascii_lower(right))
}

pub(crate) fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() || needle.is_empty() {
        return None;
    }

    for idx in 0..=haystack.len() - needle.len() {
        if starts_with_ascii_case_insensitive(&haystack[idx..], needle) {
            return Some(idx);
        }
    }
    None
}

#[inline]
pub(crate) fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    find_ascii_case_insensitive(haystack.as_bytes(), needle.as_bytes()).is_some()
}

// ── Failure-class predicates ───────────────────────────────────────────

pub(crate) fn is_auth_error(message: &str) -> bool {
    const AUTH_MARKERS: [&str; 10] = [
        "invalid authentication credentials",
        "authentication_error",
        "invalid api key",
        "invalid x-api-key",
        "failed to authenticate",
        "error: 401",
        // Codex/ChatGPT OAuth surfaces refresh-token reuse with these
        // phrasings; both should drive account rotation rather than failing
        // the mission outright (the user may have another configured account
        // whose refresh_token is still valid).
        "refresh token was already used",
        "refresh_token was already used",
        "refresh_token_reused",
        "please log out and sign in again",
    ];

    AUTH_MARKERS
        .iter()
        .any(|needle| contains_ascii_case_insensitive(message, needle))
}

pub(crate) fn is_rate_limited_error(message: &str) -> bool {
    const RATE_LIMIT_MARKERS: [&str; 18] = [
        "overloaded_error",
        // Claude Code CLI on a subscription whose credits ran out (seen on
        // prod mission 1971a723, 2026-09-03): the CLI exits 0 with a plain
        // assistant message "You're out of usage credits. Switch to another
        // model to continue." — no provider-error payload, no status code.
        // It is a quota signal: rotate accounts / hand off, never "Model
        // error".
        "out of usage credits",
        "out of credits",
        "switch to another model to continue",
        "rate limit",
        "rate_limit",
        "resource_exhausted",
        "too many requests",
        "error: 429",
        "error: 529",
        "status code: 429",
        "status code: 529",
        "out of extra usage",
        "out of regular usage",
        // Claude Code CLI surfaces subscription quota exhaustion with this
        // phrasing (e.g. "You've hit your limit · resets 9pm"). Treat it
        // as a rate-limit signal so account rotation kicks in.
        "hit your limit",
        // Codex CLI / ChatGPT account quota exhaustion. Codex emits
        // TurnFailed with messages like:
        //   "You've hit your usage limit. Visit
        //    https://chatgpt.com/codex/settings/usage to purchase more
        //    credits or try again at Apr 28th, 2026 10:03 PM."
        // The reset window is days, not minutes — match it as a
        // rate-limit so the harness classifies the turn correctly and
        // surfaces the actionable message instead of the generic
        // "Codex CLI exited before completing the turn" wrapper.
        "hit your usage limit",
        "purchase more credits",
        "settings/usage",
    ];

    RATE_LIMIT_MARKERS
        .iter()
        .any(|needle| contains_ascii_case_insensitive(message, needle))
}

pub(crate) fn is_provider_payload_error(message: &str) -> bool {
    const PROVIDER_PAYLOAD_MARKERS: [&str; 3] = [
        "image.source.base64.data",
        "image dimensions exceed max allowed size",
        "many-image requests: 2000 pixels",
    ];

    PROVIDER_PAYLOAD_MARKERS
        .iter()
        .any(|needle| contains_ascii_case_insensitive(message, needle))
}

pub(crate) fn is_capacity_limited_error(message: &str) -> bool {
    const CAPACITY_LIMIT_MARKERS: [&str; 8] = [
        "already have five missions running",
        "already have 5 missions running",
        "too many concurrent missions",
        "concurrent mission limit",
        "maximum concurrent missions",
        // OpenAI's model-level capacity rejection, emitted by Codex CLI
        // as a TurnFailed error when the selected model (e.g. GPT-5.5
        // during its rollout window) is saturated.
        "selected model is at capacity",
        "model is at capacity",
        "please try a different model",
    ];

    if CAPACITY_LIMIT_MARKERS
        .iter()
        .any(|needle| contains_ascii_case_insensitive(message, needle))
    {
        return true;
    }

    let has_already_have = contains_ascii_case_insensitive(message, "already have");
    let has_missions_running = contains_ascii_case_insensitive(message, "missions running");
    if has_already_have && has_missions_running {
        return true;
    }

    let has_concurrent = contains_ascii_case_insensitive(message, "concurrent");
    let has_mission = contains_ascii_case_insensitive(message, "mission");
    let has_limit = contains_ascii_case_insensitive(message, "limit")
        || contains_ascii_case_insensitive(message, "exceeded");
    has_concurrent && has_mission && has_limit
}

// ── Success-path variants ──────────────────────────────────────────────
//
// A harness can exit 0 while its "assistant text" is actually a provider
// error payload. These detect that case without misclassifying legitimate
// model output that merely mentions an error.

pub(crate) fn looks_like_explicit_provider_error_output(message: &str) -> bool {
    let trimmed = message.trim();
    let lower = trimmed.to_ascii_lowercase();
    let compact_lower = lower
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect::<String>();
    let starts_with_error_payload = compact_lower.starts_with("{\"error\":")
        || compact_lower.starts_with("[{\"error\":")
        || compact_lower.starts_with("{\"type\":\"error\"");
    let structured_provider_error = starts_with_error_payload
        && (compact_lower.contains("\"error\":{")
            || compact_lower.contains("\"message\":")
            || compact_lower.contains("\"code\":")
            || compact_lower.contains("authentication_error")
            || compact_lower.contains("invalid_request_error")
            || compact_lower.contains("permission_error")
            || compact_lower.contains("rate_limit_error")
            || compact_lower.contains("overloaded_error"));

    trimmed.starts_with("API Error:")
        || lower.starts_with("error:")
        || lower.starts_with("anthropic api error:")
        || lower.starts_with("claude code error:")
        || structured_provider_error
        || lower.contains("status code: 401")
        || lower.contains("status code: 429")
        || lower.contains("status code: 529")
}

pub(crate) fn is_standalone_invalid_credentials_message(message: &str) -> bool {
    let normalized = message
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | '!' | '"' | '\''))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    normalized == "invalid authentication credentials"
}

pub(crate) fn is_success_path_rate_limited_error(message: &str) -> bool {
    let lower = message.trim().replace('\u{2019}', "'").to_ascii_lowercase();
    lower.starts_with("you've hit your limit")
        || lower.starts_with("you have hit your limit")
        || lower.starts_with("you're out of usage credits")
        || lower.starts_with("you are out of usage credits")
        || lower.starts_with("you're out of credits")
        || lower.starts_with("you are out of credits")
        || (looks_like_explicit_provider_error_output(message) && is_rate_limited_error(message))
}

pub(crate) fn is_success_path_auth_error(message: &str) -> bool {
    is_standalone_invalid_credentials_message(message)
        || (looks_like_explicit_provider_error_output(message) && is_auth_error(message))
}

pub(crate) fn is_success_path_provider_payload_error(message: &str) -> bool {
    (looks_like_explicit_provider_error_output(message)
        || message.trim_start().starts_with("messages."))
        && is_provider_payload_error(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden marker tests: each entry is a real string observed in prod
    // incidents (journals, mission DBs). If a refactor of the predicates
    // changes any of these classifications, that's a behavior change and
    // must be intentional.

    #[test]
    fn golden_auth_markers() {
        let positives = [
            "API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}",
            "Invalid API key · please run /login",
            "Error: 401 Unauthorized",
            "The refresh token was already used. Please log out and sign in again.",
            "refresh_token_reused",
            "Invalid authentication credentials",
        ];
        for msg in positives {
            assert!(is_auth_error(msg), "should classify as auth error: {msg}");
        }
        let negatives = [
            "The user asked about authentication flows in the codebase.",
            "Updated the API key handling documentation.",
        ];
        for msg in negatives {
            assert!(!is_auth_error(msg), "false positive: {msg}");
        }
    }

    #[test]
    fn golden_rate_limit_markers() {
        let positives = [
            "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}",
            "Error: 429 Too Many Requests",
            "status code: 529",
            "You've hit your limit · resets 9pm",
            "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Apr 28th, 2026 10:03 PM.",
            "RESOURCE_EXHAUSTED: Quota exceeded",
        ];
        for msg in positives {
            assert!(
                is_rate_limited_error(msg),
                "should classify as rate limit: {msg}"
            );
        }
        assert!(!is_rate_limited_error(
            "Increased the rate of the limit checker loop."
        ));
    }

    #[test]
    fn golden_capacity_markers() {
        assert!(is_capacity_limited_error(
            "Error: You already have five missions running for this account."
        ));
        assert!(is_capacity_limited_error(
            "The selected model is at capacity. Please try a different model."
        ));
        assert!(!is_capacity_limited_error(
            "Mission completed; capacity planning doc updated."
        ));
    }

    #[test]
    fn golden_success_path_detection() {
        // Exit-0 turns whose "assistant text" is actually an error payload.
        assert!(is_success_path_auth_error(
            "Invalid authentication credentials."
        ));
        assert!(is_success_path_rate_limited_error(
            "You\u{2019}ve hit your limit \u{b7} resets 9pm"
        ));
        // Plain prose mentioning errors must NOT trip the success-path check.
        assert!(!is_success_path_auth_error(
            "I fixed the bug where invalid api key errors were mishandled."
        ));
        assert!(!is_success_path_rate_limited_error(
            "Added a test for the rate limit handler."
        ));
    }

    #[test]
    fn ascii_helpers_match_case_insensitively() {
        assert!(contains_ascii_case_insensitive(
            "FOO Rate Limit BAR",
            "rate limit"
        ));
        assert!(!contains_ascii_case_insensitive("rat limit", "rate limit"));
        assert!(starts_with_ascii_case_insensitive(b"Error: 401", b"error:"));
        assert_eq!(find_ascii_case_insensitive(b"abCDef", b"cde"), Some(2));
        assert_eq!(find_ascii_case_insensitive(b"abc", b""), None);
    }

    #[test]
    fn out_of_usage_credits_is_a_rate_limit_signal() {
        let msg = "You\u{2019}re out of usage credits. Switch to another model to continue.";
        assert!(is_rate_limited_error(msg));
        assert!(is_success_path_rate_limited_error(msg));
        assert!(is_success_path_rate_limited_error(
            "You're out of usage credits. Switch to another model to continue."
        ));
        assert!(!is_success_path_rate_limited_error(
            "I checked the billing page; we are not out of credits."
        ));
    }
}
