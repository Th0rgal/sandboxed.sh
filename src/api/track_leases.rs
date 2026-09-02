//! Track ownership leases: acquisition helpers for the control plane and the
//! periodic sweep that keeps them honest.
//!
//! A lease says "this mission may mutate this track right now". It is taken
//! when a mission is created or re-tagged, renewed while the mission is live,
//! and released when the mission ends. Because the mission store and
//! `projects.db` are separate authorities, the two crash windows (mission
//! created but lease never taken; lease taken but mission gone) are repaired
//! here rather than pretended away with a single transaction.

use std::sync::Arc;

use serde::Serialize;

use super::control::events::MissionStatus;
use super::projects_store::{LeaseError, LeaseRequest, TrackLease};
use super::routes::AppState;

/// How long a lease lives without renewal. Missions run for days; the sweep
/// renews live ones every pass, so this only has to outlast a restart.
pub const LEASE_TTL_SECS: u64 = 6 * 60 * 60;

/// Intents that never mutate the governed artifact. A mission with one of
/// these takes a reader lease and coexists with the writer. Anything else —
/// including no intent at all — is a writer.
const READONLY_INTENTS: &[&str] = &[
    "review",
    "review_pr",
    "review_merge_pr",
    "audit",
    "inspect",
    "certify",
    "certification",
    "verify",
    "validate",
    "watch",
    "monitor",
    "report",
    "research",
    "read",
    "readonly",
    "triage",
];

/// Server-side decision: reader or writer. The caller's `writer: false` is
/// honoured (it can only make a mission *less* powerful); `writer: true` or
/// a `pr-writer` tag forces writer; otherwise the intent decides.
pub fn lease_mode(
    writer_flag: Option<bool>,
    tags: &[String],
    intent: Option<&str>,
) -> &'static str {
    if writer_flag == Some(false) || tags.iter().any(|tag| tag == "pr-readonly") {
        return "reader";
    }
    if writer_flag == Some(true) || tags.iter().any(|tag| tag == "pr-writer") {
        return "writer";
    }
    let intent = intent
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if READONLY_INTENTS.iter().any(|candidate| {
        intent == *candidate
            || intent.starts_with(&format!("{candidate}_"))
            || intent.starts_with(&format!("{candidate}-"))
    }) {
        "reader"
    } else {
        "writer"
    }
}

/// `SANDBOXED_TRACK_REQUIRED=1`: a project-tagged mission without a track is
/// rejected instead of absorbed under a generated key.
pub fn track_required() -> bool {
    matches!(
        std::env::var("SANDBOXED_TRACK_REQUIRED")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Generated key for a legacy untagged mission during the compatibility
/// window. Stable per mission so retries do not fan out.
pub fn generated_track_key(mission_id: &str) -> String {
    format!("mission-{}", mission_id.chars().take(8).collect::<String>())
}

/// PR number out of a free-form `github_pr` reference (`owner/repo#233`,
/// `#233`, `233`, a PR URL).
pub fn pr_number(github_pr: Option<&str>) -> Option<i64> {
    let text = github_pr?.trim();
    if text.is_empty() {
        return None;
    }
    let tail = text.rsplit(['#', '/']).next().unwrap_or(text).trim();
    tail.parse::<i64>().ok().filter(|n| *n > 0)
}

pub fn lease_request(
    slug: &str,
    track: &str,
    mission_id: &str,
    mode: &str,
    idempotency_key: Option<&str>,
) -> LeaseRequest {
    LeaseRequest {
        slug: slug.to_string(),
        track: track.to_string(),
        mutation_domain: "track".to_string(),
        attempt_id: mission_id.to_string(),
        mode: mode.to_string(),
        idempotency_key: idempotency_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| format!("lease:{key}"))
            .unwrap_or_else(|| format!("lease:mission:{mission_id}:{slug}:{track}")),
        ttl_secs: LEASE_TTL_SECS,
    }
}

/// The 409 body for a held track.
pub fn owned_body(slug: &str, track: &str, error: &LeaseError) -> serde_json::Value {
    match error {
        LeaseError::Owned {
            holder_attempt_id,
            lease_until,
            lease_id,
        } => serde_json::json!({
            "error": "track_owned",
            "project": slug,
            "track": track,
            "holder_mission_id": holder_attempt_id,
            "lease_until": lease_until,
            "lease_id": lease_id,
            "message": format!(
                "track '{track}' of '{slug}' is owned by mission {holder_attempt_id} until {lease_until}; \
                 attach to it (send_message_to_mission), wait for it to end, or dispatch a read-only \
                 intent (writer=false)"
            ),
        }),
        other => serde_json::json!({ "error": "lease_failed", "message": other.to_string() }),
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct LeaseSweepReport {
    pub live: usize,
    pub renewed: usize,
    /// Missions (not leases) released because they ended.
    pub released_terminal: usize,
    /// Missions released because no store knows them.
    pub released_missing: usize,
    pub expired_overdue: usize,
    /// Leases whose attempt id is not a mission id.
    pub expired_invalid: usize,
}

/// One pass: renew leases of live missions, release those whose mission is
/// terminal or gone, expire overdue ones nobody renewed.
pub async fn sweep(state: &Arc<AppState>) -> Result<LeaseSweepReport, String> {
    let mut report = LeaseSweepReport::default();
    let leases: Vec<TrackLease> = state.projects.live_leases(None)?;
    report.live = leases.len();
    // A mission may hold several leases; look it up and count it once.
    let mut released: std::collections::HashSet<String> = std::collections::HashSet::new();
    for lease in &leases {
        if released.contains(&lease.attempt_id) {
            continue;
        }
        let Ok(mission_id) = uuid::Uuid::parse_str(&lease.attempt_id) else {
            state.projects.expire_lease(&lease.id)?;
            report.expired_invalid += 1;
            continue;
        };
        match state.control.find_mission_any_store(mission_id).await {
            Ok(Some(mission)) => {
                if mission.status.is_terminal() || mission.status == MissionStatus::Acknowledged {
                    state
                        .projects
                        .release_leases_for_attempt(&lease.attempt_id)?;
                    released.insert(lease.attempt_id.clone());
                    report.released_terminal += 1;
                } else if state.projects.renew_lease(&lease.id, LEASE_TTL_SECS)? {
                    report.renewed += 1;
                }
            }
            Ok(None) => {
                state
                    .projects
                    .release_leases_for_attempt(&lease.attempt_id)?;
                released.insert(lease.attempt_id.clone());
                report.released_missing += 1;
            }
            Err(error) => {
                tracing::warn!(
                    lease = %lease.id,
                    mission = %lease.attempt_id,
                    %error,
                    "lease sweep: mission store unavailable; leaving lease"
                );
            }
        }
    }
    for lease in state.projects.overdue_leases()? {
        if state.projects.expire_lease(&lease.id)? {
            report.expired_overdue += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_intents_take_reader_leases_unless_forced() {
        assert_eq!(lease_mode(None, &[], Some("review_merge_pr")), "reader");
        assert_eq!(lease_mode(None, &[], Some("Certify")), "reader");
        assert_eq!(lease_mode(None, &[], Some("repair-build")), "writer");
        assert_eq!(lease_mode(None, &[], None), "writer");
        assert_eq!(lease_mode(Some(false), &[], Some("repair")), "reader");
        assert_eq!(lease_mode(Some(true), &[], Some("review")), "writer");
        assert_eq!(
            lease_mode(None, &["pr-writer".into()], Some("review")),
            "writer"
        );
    }

    #[test]
    fn pr_numbers_come_out_of_any_reference_shape() {
        assert_eq!(pr_number(Some("lfglabs-dev/verity#233")), Some(233));
        assert_eq!(pr_number(Some("#233")), Some(233));
        assert_eq!(pr_number(Some("233")), Some(233));
        assert_eq!(
            pr_number(Some("https://github.com/o/r/pull/233")),
            Some(233)
        );
        assert_eq!(pr_number(Some("main")), None);
        assert_eq!(pr_number(None), None);
    }

    #[test]
    fn generated_keys_are_stable_per_mission() {
        assert_eq!(
            generated_track_key("693cc6e8-a6cf-4491-86a7-6585625db99e"),
            "mission-693cc6e8"
        );
    }
}
