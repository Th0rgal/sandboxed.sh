//! This deployment's policy for which Claude models it still runs.
//!
//! An operator decision, not a statement about Anthropic's release process:
//! this backend runs the current Opus and Fable, and declines to start new work
//! on the older generations it has moved off. Four independent paths could
//! otherwise select a model — the provider catalog, the `claude-*` escape hatch
//! in `validate_model_override`, the `DEFAULT_MODEL` environment variable, and a
//! library config profile's `default_model` — and each previously carried its
//! own partial list. This module is the single place that decides, so they
//! agree.
//!
//! Direction matters: only *older* generations are retired. A model newer than
//! the current one (`claude-opus-6`) and a dated or suffixed alias of a current
//! one (`claude-opus-5-20260101`) are passed through untouched, to be accepted
//! or rejected by ordinary catalog validation. Nothing here downgrades a model
//! for being unfamiliar.
//!
//! Scope: the Opus and Fable lines only. Sonnet, Haiku and every non-Claude
//! family are left exactly as they are.
//!
//! Recorded history is never rewritten. A mission keeps the model it actually
//! ran; the policy governs what runs next.

use std::borrow::Cow;

/// The Opus this deployment runs.
pub const CURRENT_CLAUDE_OPUS: &str = "claude-opus-5";
/// The Fable this deployment runs, offered alongside Opus rather than replacing it.
pub const CURRENT_CLAUDE_FABLE: &str = "claude-fable-5-1";

/// Oldest Opus major version still run. Anything below is retired.
const MIN_OPUS_MAJOR: u32 = 5;
/// Oldest Fable version still run, as `(major, minor)`.
const MIN_FABLE_VERSION: (u32, u32) = (5, 1);

/// Split an optional provider prefix (`anthropic/claude-opus-5`) from the id.
fn split_provider(model: &str) -> (&str, &str) {
    match model.rsplit_once('/') {
        Some((prefix, id)) => (&model[..prefix.len() + 1], id),
        None => ("", model),
    }
}

/// `claude-opus-4.8` and `claude-opus-4-8` are the same model spelled two ways.
fn canonical(id: &str) -> String {
    id.trim().to_ascii_lowercase().replace('.', "-")
}

/// The leading numeric version components after `claude-<line>-`, e.g.
/// `claude-opus-4-5-20251101` → `[4, 5]`. A trailing date (8 digits) is not a
/// version component and stops the scan, so a dated alias compares on the
/// version it actually names.
fn version_parts(rest: &str) -> Vec<u32> {
    let mut parts = Vec::new();
    for segment in rest.split('-') {
        match segment.parse::<u32>() {
            // An 8-digit run is a release date (20251101), not a version.
            Ok(_) if segment.len() >= 8 => break,
            Ok(value) => parts.push(value),
            Err(_) => break,
        }
    }
    parts
}

/// The current model of the same line, when `model` names a *retired* Opus or
/// Fable alias. `None` for the current models, for anything newer, and for
/// every other family.
pub fn retired_claude_model(model: &str) -> Option<&'static str> {
    let (_, id) = split_provider(model);
    let id = canonical(id);

    if let Some(rest) = id.strip_prefix("claude-opus-") {
        let parts = version_parts(rest);
        // Unparseable or unversioned: leave it to catalog validation.
        let major = *parts.first()?;
        return (major < MIN_OPUS_MAJOR).then_some(CURRENT_CLAUDE_OPUS);
    }

    if let Some(rest) = id.strip_prefix("claude-fable-") {
        let parts = version_parts(rest);
        let major = *parts.first()?;
        let minor = parts.get(1).copied().unwrap_or(0);
        return ((major, minor) < MIN_FABLE_VERSION).then_some(CURRENT_CLAUDE_FABLE);
    }

    None
}

/// Models that reject both disabled thinking and manual token budgets.
/// Keep explicit IDs so unknown future models retain their native behavior.
pub fn requires_adaptive_thinking(model: &str) -> bool {
    let (_, id) = split_provider(model);
    let id = canonical(id);
    ["claude-opus-5-5", "claude-fable-5-1"].iter().any(|known| {
        id == *known
            || id
                .strip_prefix(known)
                .is_some_and(|tail| tail.starts_with("-20"))
    })
}

/// Whether this model may no longer be chosen for a new turn.
pub fn is_retired_claude_model(model: &str) -> bool {
    retired_claude_model(model).is_some()
}

/// The model to actually run, keeping any provider prefix the caller used
/// (`anthropic/claude-opus-4-8` → `anthropic/claude-opus-5`). Anything that is
/// not a retired alias is returned untouched.
pub fn current_claude_model(model: &str) -> Cow<'_, str> {
    match retired_claude_model(model) {
        Some(replacement) => {
            let (prefix, _) = split_provider(model.trim());
            Cow::Owned(format!("{prefix}{replacement}"))
        }
        None => Cow::Borrowed(model),
    }
}

/// Rejection text for an explicitly requested retired model. Names the
/// replacement so the caller can retry without guessing.
pub fn retired_model_message(model: &str, replacement: &str) -> String {
    format!(
        "Model '{model}' is no longer run by this backend. \
         Use '{replacement}' instead (or '{CURRENT_CLAUDE_FABLE}' for the Fable line)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retires_older_opus_and_fable_generations() {
        for old in [
            "claude-opus-4-1",
            "claude-opus-4-8",
            "claude-opus-4.8",
            "claude-opus-4-7",
            "claude-opus-4-5-20251101",
            "claude-opus-4",
            "claude-opus-3-5",
        ] {
            assert_eq!(
                retired_claude_model(old),
                Some(CURRENT_CLAUDE_OPUS),
                "{old}"
            );
        }
        for old in [
            "claude-fable-5",
            "claude-fable-5-0",
            "claude-fable-4-5",
            "claude-fable-4",
        ] {
            assert_eq!(
                retired_claude_model(old),
                Some(CURRENT_CLAUDE_FABLE),
                "{old}"
            );
        }
    }

    #[test]
    fn never_downgrades_a_newer_model() {
        // The policy prevents old models; it must not stand in the way of new
        // ones, which ordinary catalog validation handles.
        for newer in [
            "claude-opus-6",
            "claude-opus-6-1",
            "claude-opus-7-20270301",
            "claude-fable-5-2",
            "claude-fable-6",
            "claude-fable-6-1-20270301",
        ] {
            assert_eq!(retired_claude_model(newer), None, "{newer}");
            assert_eq!(current_claude_model(newer), newer);
        }
    }

    #[test]
    fn keeps_current_models_and_their_dated_aliases() {
        for current in [
            CURRENT_CLAUDE_OPUS,
            CURRENT_CLAUDE_FABLE,
            "claude-opus-5-20260101",
            "claude-opus-5-1",
            "claude-fable-5-1-20260101",
        ] {
            assert_eq!(retired_claude_model(current), None, "{current}");
            assert_eq!(current_claude_model(current), current);
        }
    }

    #[test]
    fn leaves_other_families_and_unversioned_ids_alone() {
        for keep in [
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-3-5-haiku-20241022",
            "claude-opus-latest",
            "claude-opus-",
            "gpt-6-astra",
            "xai/grok-4.6",
            "",
        ] {
            assert_eq!(retired_claude_model(keep), None, "{keep}");
            assert_eq!(current_claude_model(keep), keep);
        }
    }

    #[test]
    fn preserves_the_provider_prefix_when_upgrading() {
        assert_eq!(
            current_claude_model("anthropic/claude-opus-4-8"),
            "anthropic/claude-opus-5"
        );
        assert_eq!(
            current_claude_model("anthropic/claude-fable-5"),
            "anthropic/claude-fable-5-1"
        );
        assert_eq!(current_claude_model("claude-opus-4-1"), "claude-opus-5");
    }

    #[test]
    fn message_names_the_replacement() {
        let message = retired_model_message("claude-opus-4-1", CURRENT_CLAUDE_OPUS);
        assert!(message.contains("claude-opus-4-1"));
        assert!(message.contains(CURRENT_CLAUDE_OPUS));
        assert!(message.contains(CURRENT_CLAUDE_FABLE));
    }
}
