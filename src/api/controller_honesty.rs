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

/// Identity used to dedupe owner escalations.
///
/// Controllers rephrase the same ticket every tick (`VL-002 — rétablir…` vs
/// `restaurer l'OAuth Codex…`). Whitespace-only normalize then inserts a new
/// `pending_user` row and the project rail stacks duplicate NEEDS YOU cards.
/// Prefer a leading ticket (`VL-001`, `LSC1-03`); otherwise the normalized
/// full question.
pub fn decision_identity(question: &str) -> String {
    let normalized = normalize_decision_question(question);
    if let Some(ticket) = leading_decision_ticket(&normalized) {
        return format!("ticket:{ticket}");
    }
    normalized
}

fn leading_decision_ticket(normalized: &str) -> Option<&str> {
    let token = normalized
        .split(|c: char| c.is_whitespace() || matches!(c, '—' | '–' | ':'))
        .next()?
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    if token.is_empty() {
        return None;
    }
    // VL-001, LSC1-03. Do not split the hyphen out of the token.
    let (head, tail) = token.split_once('-')?;
    let head_ok = !head.is_empty() && head.chars().all(|c| c.is_ascii_alphanumeric());
    let tail_ok = (2..=4).contains(&tail.len()) && tail.chars().all(|c| c.is_ascii_digit());
    if head_ok && tail_ok {
        Some(token)
    } else {
        None
    }
}

/// `inspect <mission-uuid>` — a controller parking on a dead writer instead
/// of redispaching. Not a project-level no-lane.
pub fn is_inspect_next_action(next: Option<&str>) -> bool {
    parse_inspect_mission_id(next).is_some()
}

/// Parse `inspect 51f37d4b-…` / `inspect 51f37d4b` into a UUID when possible.
pub fn parse_inspect_mission_id(next: Option<&str>) -> Option<uuid::Uuid> {
    let raw = next.map(str::trim).filter(|s| !s.is_empty())?;
    let rest = raw
        .strip_prefix("inspect")
        .or_else(|| raw.strip_prefix("Inspect"))
        .or_else(|| raw.strip_prefix("INSPECT"))?;
    let token = rest.split_whitespace().next()?;
    if let Ok(id) = uuid::Uuid::parse_str(token) {
        return Some(id);
    }
    // Controllers often paste the 8-char prefix. Not enough to look up.
    None
}

/// Terminal reasons that are harness/transport, not a project blocker.
pub fn is_harness_terminal_reason(reason: Option<&str>, evidence: Option<&str>) -> bool {
    let blob = format!("{} {}", reason.unwrap_or(""), evidence.unwrap_or("")).to_ascii_lowercase();
    blob.contains("llm_error")
        || blob.contains("transport")
        || blob.contains("server_shutdown")
        || blob.contains("pending tool")
        || blob.contains("not replayed")
        || blob.contains("stream closed")
}

/// A next-action that means "idle until a PR appears" rather than do the
/// stored roadmap item. Controllers rephrase this every tick, which used
/// to look like progress.
pub fn is_watch_idle_next_action(next: Option<&str>) -> bool {
    let Some(raw) = next.map(fold_headline) else {
        return false;
    };
    if raw.is_empty() {
        return false;
    }
    let watch = raw.contains("watch")
        || raw.contains("surveill")
        || raw.contains("monitor")
        || raw.contains("veille");
    let pr = raw.contains("pr") || raw.contains("pull request");
    watch && pr
}

/// Track `desired_state` that still wants implementation (not merge/done).
pub fn is_implement_ready_desired(desired: Option<&str>) -> bool {
    let Some(raw) = desired.map(fold_headline) else {
        return false;
    };
    raw == "implement"
        || raw == "implementable"
        || raw == "implement-ready"
        || raw == "ready"
        || raw.starts_with("implement")
}

fn track_is_open(status: Option<&str>) -> bool {
    match status.map(fold_headline) {
        None => true,
        Some(s) if s.is_empty() => true,
        Some(s) => !matches!(
            s.as_str(),
            "done" | "merged" | "closed" | "superseded" | "dropped"
        ),
    }
}

fn track_is_open_pr(track: &str, status: Option<&str>) -> bool {
    let name = fold_headline(track);
    track_is_open(status) && (name.starts_with("pr-") || name.starts_with("pr "))
}

/// When the drain is empty and a stored track is still implementable, that
/// item wins over a "watch for a new PR" idle line.
pub fn honest_next_action(
    reported: Option<&str>,
    tracks: &[(String, Option<String>, Option<String>)],
) -> Option<String> {
    let implement = tracks.iter().find_map(|(track, desired, status)| {
        if !track_is_open(status.as_deref()) {
            return None;
        }
        if is_implement_ready_desired(desired.as_deref()) {
            return Some(format!("implement {track}"));
        }
        None
    });
    let has_open_pr = tracks
        .iter()
        .any(|(track, _, status)| track_is_open_pr(track, status.as_deref()));
    if let Some(item) = implement {
        if !has_open_pr && is_watch_idle_next_action(reported) {
            return Some(item);
        }
        if reported.is_none() || reported.is_some_and(|s| s.trim().is_empty()) {
            return Some(item);
        }
    }
    reported
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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
    fn inspect_next_action_parses_uuid_and_rejects_prose() {
        let id = uuid::Uuid::parse_str("51f37d4b-7e27-466a-8c2f-fec0be2bebed").unwrap();
        assert_eq!(
            parse_inspect_mission_id(Some("inspect 51f37d4b-7e27-466a-8c2f-fec0be2bebed")),
            Some(id)
        );
        assert!(is_inspect_next_action(Some(
            "inspect 51f37d4b-7e27-466a-8c2f-fec0be2bebed"
        )));
        assert!(!is_inspect_next_action(Some("watch #2367 CI then merge")));
        assert!(is_harness_terminal_reason(
            Some("llm_error"),
            Some("pending tool call exec-1 remained unresolved after thread/resume")
        ));
        assert!(!is_harness_terminal_reason(Some("rate_limited"), None));
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

    #[test]
    fn decision_identity_collapses_ticket_paraphrases() {
        assert_eq!(
            decision_identity("VL-002 — rétablir l'OAuth Codex, ou la facturation Muse."),
            decision_identity("VL-002 — restaurer Codex OAuth sandboxed.sh ou Muse billing")
        );
        assert_eq!(
            decision_identity("VL-001 — réparer le chemin OpenCode/Spark SIGTERM"),
            "ticket:vl-001"
        );
        assert_ne!(
            decision_identity("VL-001 — spark"),
            decision_identity("VL-002 — oauth")
        );
    }

    #[test]
    fn watch_idle_does_not_win_over_implement_ready_track() {
        let tracks = vec![(
            "p-reserve-relational".to_string(),
            Some("implement".to_string()),
            Some("open".to_string()),
        )];
        assert_eq!(
            honest_next_action(
                Some("watch for a new Lido PR or exact-head finding"),
                &tracks
            )
            .as_deref(),
            Some("implement p-reserve-relational")
        );
        assert_eq!(
            honest_next_action(Some("surveiller toute nouvelle PR Lido vers main"), &tracks)
                .as_deref(),
            Some("implement p-reserve-relational")
        );
        // An open PR drain still reports the controller's line.
        let with_pr = vec![
            tracks[0].clone(),
            (
                "pr-84".to_string(),
                Some("mergeable".to_string()),
                Some("open".to_string()),
            ),
        ];
        assert_eq!(
            honest_next_action(Some("watch for a new Lido PR"), &with_pr).as_deref(),
            Some("watch for a new Lido PR")
        );
    }

    #[test]
    fn watch_phrasing_is_detected() {
        assert!(is_watch_idle_next_action(Some(
            "watch for a new Lido PR or exact-head finding"
        )));
        assert!(is_watch_idle_next_action(Some(
            "monitor-open-prs then merge"
        )));
        assert!(!is_watch_idle_next_action(Some(
            "implement p-reserve-relational"
        )));
    }
}
