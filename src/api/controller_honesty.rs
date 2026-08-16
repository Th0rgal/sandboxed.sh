//! Gates that keep controller prose from rewriting live store facts.
//!
//! Controllers are LLM cron ticks. They already *may* read `get_project` /
//! `list_missions`; nothing forced them to, so a lease-writer headline could
//! land while two writers were active, and a cancel-timeout auto-resume could
//! land as "CAMPAGNE RELANCÉE". These helpers are the store-side refusal.

use super::control::events::MissionStatus;

/// How long an unanswered owner question stays `pending_user` before the
/// ledger expires it. After that the controller must act on the conservative
/// in-grant default instead of re-asking. Coldcard sat on a duplicate
/// checkpoint prompt from 2026-08-13T20:22Z through the next afternoon.
pub const PENDING_DECISION_TTL: chrono::Duration = chrono::Duration::hours(24);

/// A cancel-timeout must not abort a runner that still has a live tool or
/// that emitted an event in this window. Long Lean/Codex turns go quiet
/// between tool results; 90s is longer than the 30s force-clear grace and
/// shorter than the ~15 min watchdog.
pub const CANCEL_TIMEOUT_FRESH_PROGRESS: std::time::Duration = std::time::Duration::from_secs(90);

/// Statuses that mean a writer is actually executing (not parked).
pub fn is_live_writer_status(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Pending | MissionStatus::Active | MissionStatus::WaitingBackground
    )
}

/// Headline that only restates an auto-resume of the same campaign.
pub fn is_relaunch_headline(headline: &str) -> bool {
    let folded = fold_headline(headline);
    folded.contains("relanc")
        || folded.contains("relaunch")
        || folded.contains("re-pin relanc")
        || folded.contains("repin relanc")
}

/// Headline that claims a writer-lease block (Verity's stale 14:30Z signal).
pub fn is_stale_lease_claim(headline: &str) -> bool {
    let folded = fold_headline(headline);
    (folded.contains("lease") && folded.contains("writer"))
        || folded.contains("bloquee par lease")
        || folded.contains("blocked by writer lease")
}

/// CTRL/roster cause that is a writer-lease story.
pub fn is_lease_blocker(blocker: Option<&str>) -> bool {
    let Some(raw) = blocker.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let folded = fold_headline(raw);
    folded.contains("lease") || (folded.contains("writer") && folded.contains("block"))
}

/// `blocked:lease` / `blocked:lease-writer` on the mode string itself.
pub fn mode_claims_lease_block(mode: Option<&str>) -> bool {
    let Some(raw) = mode.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let lower = raw.to_ascii_lowercase();
    if let Some((_, cause)) = lower.split_once(':') {
        return cause.contains("lease") || cause.contains("writer");
    }
    false
}

/// Live writer + lease-block claim → report `active` and drop the cause.
pub fn coerce_mode_against_live<'a>(
    mode: Option<&'a str>,
    blocker: Option<&'a str>,
    has_live_writer: bool,
) -> (Option<&'a str>, Option<&'a str>) {
    if !has_live_writer {
        return (mode, blocker);
    }
    if mode_claims_lease_block(mode) || is_lease_blocker(blocker) {
        return (Some("active"), None);
    }
    (mode, blocker)
}

/// Whether this delivery's human headline should be folded as `[SILENT]`.
///
/// A relaunch of the same campaign after cancel-timeout is infra, not a
/// chapter. A lease-writer claim while a writer is live is a lie.
pub fn should_silence_headline(headline: &str, has_live_writer: bool) -> bool {
    if headline.trim() == "[SILENT]" || headline.trim().is_empty() {
        return true;
    }
    if is_relaunch_headline(headline) {
        return true;
    }
    has_live_writer && is_stale_lease_claim(headline)
}

/// Mission-complete inspect callbacks are real events for the timeline but
/// they are not autonomous acts — they used to steal the card's latest
/// headline ("Needs You") and they must not flood Recent activity.
pub fn is_inspect_callback_headline(headline: &str) -> bool {
    headline.trim().starts_with("[Mission callback:")
}

/// Headline that should appear on the Recent activity panel: a controller
/// chapter, not silence, not a relaunch, not an inspect callback.
pub fn is_material_activity_headline(headline: &str) -> bool {
    let trimmed = headline.trim();
    !trimmed.is_empty()
        && !should_silence_headline(trimmed, false)
        && !is_inspect_callback_headline(trimmed)
}

/// Collapse a question so "relancer X ?" and "Relancer  X?" are one row.
pub fn normalize_decision_question(question: &str) -> String {
    question
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fold_headline(headline: &str) -> String {
    headline
        .chars()
        .map(|ch| match ch {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'à' | 'á' | 'â' => 'a',
            _ => ch.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_headlines_are_infra_noise() {
        assert!(is_relaunch_headline("Lido audit — RE-PIN RELANCÉ"));
        assert!(is_relaunch_headline(
            "Campagne lido-verity-closure-v2 relancée sur son propriétaire unique"
        ));
        assert!(is_relaunch_headline("campaign relaunched"));
        assert!(!is_relaunch_headline("Lido — CAMPAGNE DE RE-PIN LANCÉE"));
        assert!(!is_relaunch_headline(
            "rebase/repair #2332 onto main after #2333"
        ));
    }

    #[test]
    fn lease_claims_are_detected() {
        assert!(is_stale_lease_claim(
            "Verity #2332 — BLOQUÉE PAR LEASE WRITER"
        ));
        assert!(is_stale_lease_claim("blocked by writer lease"));
        assert!(is_lease_blocker(Some("source #2332 dirty lease writer")));
        assert!(mode_claims_lease_block(Some("blocked:lease-writer")));
        assert!(!is_stale_lease_claim("blocked by dirty source"));
    }

    #[test]
    fn inspect_callbacks_are_not_recent_activity() {
        assert!(is_inspect_callback_headline(
            "[Mission callback: Lido closure v2 — re-pin Verity from minimal-11]"
        ));
        assert!(!is_material_activity_headline(
            "[Mission callback: Repair Lido #76 false receipt provenance]"
        ));
        assert!(!is_material_activity_headline("[SILENT]"));
        assert!(!is_material_activity_headline(
            "Lido audit — RE-PIN RELANCÉ"
        ));
        assert!(is_material_activity_headline(
            "Lido #81 — RÉPARATION/REVIEW EN COURS"
        ));
    }

    #[test]
    fn live_writer_silences_lease_and_relaunch() {
        assert!(should_silence_headline(
            "Verity #2332 — BLOQUÉE PAR LEASE WRITER",
            true
        ));
        assert!(!should_silence_headline(
            "Verity #2332 — BLOQUÉE PAR LEASE WRITER",
            false
        ));
        assert!(should_silence_headline(
            "Lido audit — CAMPAGNE RELANCÉE",
            false
        ));
        assert!(!should_silence_headline("Certify #69 at exact head", false));
    }

    #[test]
    fn live_writer_clears_a_lease_mode() {
        assert_eq!(
            coerce_mode_against_live(Some("blocked:lease-writer"), Some("lease"), true),
            (Some("active"), None)
        );
        assert_eq!(
            coerce_mode_against_live(Some("blocked"), Some("lease writer"), true),
            (Some("active"), None)
        );
        assert_eq!(
            coerce_mode_against_live(Some("blocked:transport-cap"), None, true),
            (Some("blocked:transport-cap"), None)
        );
    }

    #[test]
    fn decision_questions_collapse_whitespace_and_case() {
        assert_eq!(
            normalize_decision_question("Relancer  coldcard_skip depuis le checkpoint ?"),
            normalize_decision_question("relancer coldcard_skip depuis le checkpoint ?")
        );
    }
}
