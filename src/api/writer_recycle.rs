//! Refuse silent reuse of a PR writer as unrelated work.
//!
//! Lido 2026-08-17: mission `ebfb1fd1` stayed tagged `github_pr=…#88` /
//! `track=pr-88-repair` while being retasked as P-RESERVE-RELATIONAL. The
//! inventory lied. A writer may be reused only when the caller updates the
//! identity fields that named the previous work, or explicitly asserts continuation.
//! Continuation trusts the authenticated caller about prompt semantics; matching
//! tags is not proof that the objective is unchanged.

use super::control::canonical_github_pr;
use serde::{Deserialize, Serialize};

/// A controller's explicit assertion that this turn continues the stored work.
/// All fields are required, including an explicit null for an unrecorded PR.
/// This is a trusted semantic assertion and an identity precondition, not proof
/// of unchanged work, a PR ownership grant, or an identity edit.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterContinuation {
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    pub project: Option<String>,
    pub track: String,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    pub github_pr: Option<String>,
}

/// Identity a controller/writer is currently tagged with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriterIdentity {
    pub project: Option<String>,
    pub title: Option<String>,
    pub github_pr: Option<String>,
    pub track: Option<String>,
}

/// What the new dispatch wants. `None` on a field means "leave unspecified"
/// (silent). `Some(None)` means "clear". `Some(Some(v))` means "set".
#[derive(Debug, Clone, Default)]
pub struct WriterIdentityPatch {
    pub continue_identity: Option<WriterContinuation>,
    pub title: Option<Option<String>>,
    pub github_pr: Option<Option<String>>,
    pub track: Option<Option<String>>,
    /// Prompt / objective of the new turn. Used to detect that the work
    /// changed when the caller did not retag.
    pub work_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleRefusal {
    pub error: &'static str,
    pub message: String,
}

/// Merge `patch` onto `stored`. Refuses when the new work is different and
/// `github_pr` / `track` were not explicitly updated.
pub fn apply_writer_reuse(
    stored: &WriterIdentity,
    patch: &WriterIdentityPatch,
) -> Result<WriterIdentity, RecycleRefusal> {
    if let Some(expected) = &patch.continue_identity {
        if expected.track.trim().is_empty()
            || stored.track.as_deref() != Some(expected.track.as_str())
            || stored.project != expected.project
            || stored.github_pr != expected.github_pr
            || patch.title.is_some()
            || patch.github_pr.is_some()
            || patch.track.is_some()
        {
            return Err(RecycleRefusal {
                error: "writer_identity_stale",
                message: "continuation must match the stored project, nonempty track and github_pr exactly and cannot include identity updates; reread the mission, or explicitly retag different work".into(),
            });
        }
        // The caller has named the work structurally. PRs/campaigns in prose
        // can now be exclusions or collaborators; never infer ownership from
        // those references. The normal writer capability/lease checks remain.
        return Ok(stored.clone());
    }
    let work_changed = dispatch_is_different_work(stored, patch);
    let identity_updated = patch.github_pr.is_some() || patch.track.is_some();
    if work_changed && !identity_updated {
        return Err(RecycleRefusal {
            error: "writer_identity_stale",
            message: format!(
                "refusing silent recycle of writer tagged github_pr={} track={}; \
                 retag github_pr/track (or clear them) before dispatching different work",
                stored.github_pr.as_deref().unwrap_or("-"),
                stored.track.as_deref().unwrap_or("-"),
            ),
        });
    }

    Ok(WriterIdentity {
        project: stored.project.clone(),
        title: merge_field(&stored.title, &patch.title),
        github_pr: merge_field(&stored.github_pr, &patch.github_pr),
        track: merge_field(&stored.track, &patch.track),
    })
}

fn merge_field(stored: &Option<String>, patch: &Option<Option<String>>) -> Option<String> {
    match patch {
        None => stored.clone(),
        Some(None) => None,
        Some(Some(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}

fn dispatch_is_different_work(stored: &WriterIdentity, patch: &WriterIdentityPatch) -> bool {
    let hint = patch
        .work_hint
        .as_deref()
        .or(patch.title.as_ref().and_then(|inner| inner.as_deref()))
        .unwrap_or("")
        .to_ascii_lowercase();
    if hint.is_empty() {
        return false;
    }

    // An untagged mission has no previous identity to protect. The first
    // prompt on a new writer routinely names a PR (`#105`) or campaign
    // (`P-TOPUP-2`); treating that as a silent recycle 409s the operator's
    // opening message (mission 374fac82, 2026-08-19).
    if !stored_has_writer_identity(stored) {
        return false;
    }

    let stored_pr = stored.github_pr.as_deref().map(canonical_github_pr);
    let stored_pr_number = stored_pr
        .as_ref()
        .and_then(|canonical| canonical.rsplit('#').next())
        .map(str::to_string);
    // A follow-up that never names work ("fix the failing test") is the same
    // writer continuing. Only a *named* different PR / campaign is a retask.
    for number in pr_numbers_in(&hint) {
        if stored_pr_number.as_deref() != Some(number.as_str()) {
            return true;
        }
    }

    let stored_track = stored.track.as_deref().unwrap_or("").to_ascii_lowercase();
    for token in campaign_tokens_in(&hint) {
        if !stored_identity_covers(&stored_track, stored_pr.as_deref(), &token) {
            return true;
        }
    }

    false
}

fn pr_numbers_in(hint: &str) -> Vec<String> {
    let bytes = hint.as_bytes();
    let mut numbers = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let (number, next) = take_digits(hint, i + 1);
            numbers.push(number);
            i = next;
            continue;
        }
        if word_at(bytes, i, b"pr") {
            let after = i + 2;
            if after < bytes.len() && matches!(bytes[after], b'-' | b' ' | b'#') {
                let digits_at = after + 1;
                if digits_at < bytes.len() && bytes[digits_at].is_ascii_digit() {
                    let (number, next) = take_digits(hint, digits_at);
                    numbers.push(number);
                    i = next;
                    continue;
                }
            }
        }
        if word_at(bytes, i, b"pull") {
            let after = i + 4;
            if after < bytes.len()
                && bytes[after] == b'/'
                && after + 1 < bytes.len()
                && bytes[after + 1].is_ascii_digit()
            {
                let (number, next) = take_digits(hint, after + 1);
                numbers.push(number);
                i = next;
                continue;
            }
        }
        i += 1;
    }
    numbers
}

fn campaign_tokens_in(hint: &str) -> Vec<String> {
    let bytes = hint.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || !bytes[i.saturating_sub(1)].is_ascii_alphanumeric();
        if at_boundary
            && bytes[i] == b'p'
            && i + 2 < bytes.len()
            && bytes[i + 1] == b'-'
            && bytes[i + 2].is_ascii_alphabetic()
        {
            let mut end = i + 2;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-') {
                end += 1;
            }
            let mut trimmed = end;
            while trimmed > i + 2 && bytes[trimmed - 1] == b'-' {
                trimmed -= 1;
            }
            tokens.push(hint[i..trimmed].to_string());
            i = end;
            continue;
        }
        i += 1;
    }
    tokens
}

fn stored_has_writer_identity(stored: &WriterIdentity) -> bool {
    field_nonempty(&stored.github_pr) || field_nonempty(&stored.track)
}

fn field_nonempty(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|s| !s.trim().is_empty())
}

fn stored_identity_covers(track: &str, github_pr: Option<&str>, token: &str) -> bool {
    if !track.is_empty() {
        let track_norm = track.replace('_', "-");
        if track == token
            || track_norm == token
            || track.contains(token)
            || token.contains(track)
            || track_norm.starts_with(token)
            || token.starts_with(track_norm.as_str())
        {
            return true;
        }
    }
    github_pr.is_some_and(|pr| pr.to_ascii_lowercase().contains(token))
}

fn word_at(bytes: &[u8], i: usize, word: &[u8]) -> bool {
    if i + word.len() > bytes.len() {
        return false;
    }
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes[i..i + word.len()].eq_ignore_ascii_case(word)
}

fn take_digits(hint: &str, start: usize) -> (String, usize) {
    let bytes = hint.as_bytes();
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    (hint[start..end].to_string(), end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_pr88() -> WriterIdentity {
        WriterIdentity {
            project: Some("lido".into()),
            title: Some("Repair Lido PR #88 second current-head findings".into()),
            github_pr: Some("lfglabs-dev/lido-srv3-proof-closure#88".into()),
            track: Some("pr-88-repair".into()),
        }
    }

    fn continuation(stored: &WriterIdentity, hint: &str) -> WriterIdentityPatch {
        WriterIdentityPatch {
            continue_identity: Some(WriterContinuation {
                project: stored.project.clone(),
                track: stored.track.clone().unwrap(),
                github_pr: stored.github_pr.clone(),
            }),
            work_hint: Some(hint.into()),
            ..Default::default()
        }
    }

    #[test]
    fn explicit_same_track_allows_exclusions_and_collaborator_references() {
        let stored = WriterIdentity {
            project: Some("lido".into()),
            title: Some("ALLOC-1".into()),
            track: Some("trio-alloc1".into()),
            github_pr: Some("lfglabs-dev/lido-srv3-proof-closure#242".into()),
        };
        for hint in [
            "Resume ALLOC-1. No-go: do not touch PR #230 or PR #231.",
            "Finish ALLOC-1; coordinate interface assumptions with PR 230 and P-RESERVE-1.",
            "Continue ALLOC-1 after reading https://github.com/other/repo/pull/231 for context.",
        ] {
            let patch = continuation(&stored, hint);
            assert_eq!(apply_writer_reuse(&stored, &patch).unwrap(), stored);
            // No phrase whitelist: legacy callers must still supply identity
            // updates or the explicit continuation when they name other work.
            let legacy = WriterIdentityPatch {
                continue_identity: None,
                ..patch
            };
            assert!(apply_writer_reuse(&stored, &legacy).is_err());
        }
    }

    #[test]
    fn reserve_continuation_does_not_invent_missing_pr_metadata() {
        let stored = WriterIdentity {
            project: Some("lido".into()),
            title: Some("RESERVE-1".into()),
            track: Some("trio-reserve1".into()),
            github_pr: None,
        };
        let patch = continuation(&stored, "Continue RESERVE-1 on our existing PR 244.");
        assert_eq!(apply_writer_reuse(&stored, &patch).unwrap(), stored);
        let legacy = WriterIdentityPatch {
            continue_identity: None,
            ..patch
        };
        assert!(apply_writer_reuse(&stored, &legacy).is_err());
    }

    #[test]
    fn continuation_rejects_wrong_track_pr_and_missing_identity() {
        let stored = stored_pr88();
        for (track, pr) in [
            ("p-reserve-relational", stored.github_pr.clone()),
            ("pr-88-repair", Some("other/repository#88".into())),
            ("pr-88-repair", None),
            ("", stored.github_pr.clone()),
            ("PR-88-REPAIR", stored.github_pr.clone()),
        ] {
            let patch = WriterIdentityPatch {
                continue_identity: Some(WriterContinuation {
                    project: stored.project.clone(),
                    track: track.into(),
                    github_pr: pr,
                }),
                work_hint: Some("Implement P-RESERVE-RELATIONAL".into()),
                ..Default::default()
            };
            assert_eq!(
                apply_writer_reuse(&stored, &patch).unwrap_err().error,
                "writer_identity_stale"
            );
            assert!(apply_writer_reuse(&WriterIdentity::default(), &patch).is_err());
        }
    }

    #[test]
    fn continuation_is_invalid_after_project_or_pr_metadata_changes() {
        let original = stored_pr88();
        let patch = continuation(&original, "Continue the assigned work; consult PR #90.");
        for updated in [
            WriterIdentity {
                project: Some("another-project".into()),
                ..original.clone()
            },
            WriterIdentity {
                project: None,
                ..original.clone()
            },
            WriterIdentity {
                github_pr: None,
                ..original.clone()
            },
        ] {
            assert!(apply_writer_reuse(&updated, &patch).is_err());
        }
        let missing_pr = WriterIdentity {
            github_pr: None,
            ..original.clone()
        };
        let old_assertion = continuation(&missing_pr, "Continue PR #88");
        assert!(apply_writer_reuse(&original, &old_assertion).is_err());
    }

    #[test]
    fn continuation_cannot_be_combined_with_identity_edits_even_noops() {
        let stored = stored_pr88();
        for edit in [
            WriterIdentityPatch {
                github_pr: Some(None),
                ..Default::default()
            },
            WriterIdentityPatch {
                track: Some(stored.track.clone()),
                ..Default::default()
            },
            WriterIdentityPatch {
                title: Some(Some("different work".into())),
                ..Default::default()
            },
        ] {
            let patch = WriterIdentityPatch {
                continue_identity: continuation(&stored, "").continue_identity,
                ..edit
            };
            assert!(apply_writer_reuse(&stored, &patch).is_err());
        }
    }

    #[test]
    fn continuation_requires_explicit_nullable_pr_and_rejects_typos() {
        for raw in [
            serde_json::json!({"project": "lido", "track": "trio-reserve1"}),
            serde_json::json!({"project": "lido", "github_pr": null}),
            serde_json::json!({"track": "trio-reserve1", "github_pr": null}),
            serde_json::json!({"project": "lido", "track": "trio-reserve1", "github_pr": null, "pr": "244"}),
        ] {
            assert!(serde_json::from_value::<WriterContinuation>(raw).is_err());
        }
        let parsed: WriterContinuation = serde_json::from_value(serde_json::json!({
            "project": "lido", "track": "trio-reserve1", "github_pr": null,
        }))
        .unwrap();
        assert!(parsed.github_pr.is_none());
    }

    #[test]
    fn silent_preserve_as_p_reserve_is_rejected() {
        let err = apply_writer_reuse(
            &stored_pr88(),
            &WriterIdentityPatch {
                work_hint: Some("Implement P-RESERVE-RELATIONAL first slice on main".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect_err("must refuse");
        assert_eq!(err.error, "writer_identity_stale");
        assert!(err.message.contains("#88"));
    }

    #[test]
    fn explicit_retag_allows_reuse() {
        let next = apply_writer_reuse(
            &stored_pr88(),
            &WriterIdentityPatch {
                title: Some(Some("P-RESERVE-RELATIONAL first slice".into())),
                github_pr: Some(None),
                track: Some(Some("p-reserve-relational".into())),
                work_hint: Some("Implement P-RESERVE-RELATIONAL".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect("retag ok");
        assert_eq!(next.github_pr.as_deref(), None);
        assert_eq!(next.track.as_deref(), Some("p-reserve-relational"));
        assert_eq!(
            next.title.as_deref(),
            Some("P-RESERVE-RELATIONAL first slice")
        );
    }

    #[test]
    fn explicit_pr_metadata_update_preserves_the_existing_track() {
        let stored = WriterIdentity {
            project: Some("lido".into()),
            title: Some("RESERVE-1".into()),
            track: Some("trio-reserve1".into()),
            github_pr: None,
        };
        let next = apply_writer_reuse(
            &stored,
            &WriterIdentityPatch {
                github_pr: Some(Some("lfglabs-dev/lido-srv3-proof-closure#244".into())),
                work_hint: Some("Continue RESERVE-1 on PR 244".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(next.track, stored.track);
        assert_eq!(next.project, stored.project);
        assert_eq!(next.title, stored.title);
        assert_eq!(
            next.github_pr.as_deref(),
            Some("lfglabs-dev/lido-srv3-proof-closure#244")
        );
        let title_only = WriterIdentityPatch {
            title: Some(Some("Switch this writer to PR #90".into())),
            ..Default::default()
        };
        assert!(apply_writer_reuse(&stored, &title_only).is_err());
    }

    #[test]
    fn same_pr_continue_needs_no_retag() {
        let next = apply_writer_reuse(
            &stored_pr88(),
            &WriterIdentityPatch {
                work_hint: Some("Continue repair of Lido PR #88 exact-head findings".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect("same work");
        assert_eq!(next.github_pr, stored_pr88().github_pr);
        assert_eq!(next.track, stored_pr88().track);
    }

    #[test]
    fn followup_that_does_not_mention_pr_is_same_work() {
        let next = apply_writer_reuse(
            &stored_pr88(),
            &WriterIdentityPatch {
                work_hint: Some("fix the failing test".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect("generic same-work chat must not 409");
        assert_eq!(next.github_pr, stored_pr88().github_pr);
        assert_eq!(next.track, stored_pr88().track);
        assert_eq!(next.title, stored_pr88().title);
    }

    #[test]
    fn naming_a_different_pr_is_a_retask() {
        let err = apply_writer_reuse(
            &stored_pr88(),
            &WriterIdentityPatch {
                work_hint: Some("switch this writer to PR #90".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect_err("different PR is different work");
        assert_eq!(err.error, "writer_identity_stale");
    }

    #[test]
    fn untagged_new_mission_may_name_a_pr_and_campaign() {
        let next = apply_writer_reuse(
            &WriterIdentity::default(),
            &WriterIdentityPatch {
                work_hint: Some(
                    "Base: continue from PR #105. Launch P-TOPUP-2 and P-ALLOC-1.".into(),
                ),
                ..WriterIdentityPatch::default()
            },
        )
        .expect("first message on a blank writer is not a recycle");
        assert_eq!(next.github_pr, None);
        assert_eq!(next.track, None);
    }

    #[test]
    fn whitespace_only_tags_are_untagged() {
        let stored = WriterIdentity {
            project: Some("lido".into()),
            github_pr: Some("  ".into()),
            track: Some("".into()),
            title: None,
        };
        apply_writer_reuse(
            &stored,
            &WriterIdentityPatch {
                work_hint: Some("Implement P-SSZ-1 on PR #105".into()),
                ..WriterIdentityPatch::default()
            },
        )
        .expect("blank tags are not a stale identity");
    }
}
