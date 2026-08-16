//! Qualified "Needs You" attention: a real operator page, not worker idle.
//!
//! Shared by mission JSON, the projects board, Telegram, and Paloma so the
//! 20-minute controller-triage grace cannot drift across surfaces.

use chrono::{DateTime, Utc};

use super::control::events::MissionStatus;
use super::mission_store::Mission;

/// How long a controller-owned unanswered question stays off the operator
/// inbox. After this the owning controller failed to resolve it and the
/// human is paged. Matches the historical Telegram constant.
pub const CONTROLLER_TRIAGE_GRACE_SECS: i64 = 20 * 60;

/// Inputs the operator-page predicate needs. Callers that already have a
/// [`Mission`] should prefer [`mission_needs_operator`].
#[derive(Debug, Clone, Copy)]
pub struct OperatorAttentionInput<'a> {
    pub status: MissionStatus,
    pub awaiting_kind: Option<&'a str>,
    pub has_origin_session: bool,
    pub updated_at: &'a str,
    pub waiting_for_user_tool: bool,
}

/// Whether this mission is a qualified operator page right now.
///
/// True only for an unanswered decision / AskUserQuestion that has no
/// owning controller session, or whose controller-triage grace has expired.
/// `awaiting_kind=ack`, blocked/inspect states, and in-grace controller-owned
/// waits are not operator pages.
pub fn needs_operator(input: &OperatorAttentionInput<'_>, now: DateTime<Utc>) -> bool {
    if !is_qualified_operator_page(input) {
        return false;
    }
    !held_for_controller_triage(input.has_origin_session, age_secs(input.updated_at, now))
}

/// [`needs_operator`] from a stored mission plus optional live AskUserQuestion.
pub fn mission_needs_operator(
    mission: &Mission,
    waiting_for_user_tool: bool,
    now: DateTime<Utc>,
) -> bool {
    needs_operator(&attention_input(mission, waiting_for_user_tool), now)
}

pub fn attention_input(
    mission: &Mission,
    waiting_for_user_tool: bool,
) -> OperatorAttentionInput<'_> {
    OperatorAttentionInput {
        status: mission.status,
        awaiting_kind: mission.awaiting_kind.map(|kind| kind.as_str()),
        has_origin_session: mission
            .origin_session_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()),
        updated_at: mission
            .activity
            .last_status_change_at
            .as_deref()
            .unwrap_or(mission.updated_at.as_str()),
        waiting_for_user_tool,
    }
}

fn is_qualified_operator_page(input: &OperatorAttentionInput<'_>) -> bool {
    if input.waiting_for_user_tool {
        return true;
    }
    input.status == MissionStatus::AwaitingUser && input.awaiting_kind == Some("decision")
}

fn held_for_controller_triage(has_origin_session: bool, awaiting_secs: i64) -> bool {
    has_origin_session && awaiting_secs < CONTROLLER_TRIAGE_GRACE_SECS
}

fn age_secs(timestamp: &str, now: DateTime<Utc>) -> i64 {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|parsed| (now - parsed.with_timezone(&Utc)).num_seconds())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::{AwaitingKind, MissionActivity, MissionMode};
    use chrono::TimeZone;
    use uuid::Uuid;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap()
    }

    fn ts_ago(secs: i64) -> String {
        (now() - chrono::Duration::seconds(secs)).to_rfc3339()
    }

    fn input<'a>(
        status: MissionStatus,
        kind: Option<&'a str>,
        has_origin: bool,
        updated_at: &'a str,
        waiting_for_user_tool: bool,
    ) -> OperatorAttentionInput<'a> {
        OperatorAttentionInput {
            status,
            awaiting_kind: kind,
            has_origin_session: has_origin,
            updated_at,
            waiting_for_user_tool,
        }
    }

    #[test]
    fn ack_with_controller_origin_is_not_needs_operator() {
        let updated = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 60);
        assert!(!needs_operator(
            &input(
                MissionStatus::AwaitingUser,
                Some("ack"),
                true,
                &updated,
                false
            ),
            now()
        ));
    }

    #[test]
    fn decision_without_origin_needs_operator_immediately() {
        let updated = ts_ago(5);
        assert!(needs_operator(
            &input(
                MissionStatus::AwaitingUser,
                Some("decision"),
                false,
                &updated,
                false
            ),
            now()
        ));
    }

    #[test]
    fn decision_with_origin_inside_grace_is_not_needs_operator() {
        let updated = ts_ago(60);
        assert!(!needs_operator(
            &input(
                MissionStatus::AwaitingUser,
                Some("decision"),
                true,
                &updated,
                false
            ),
            now()
        ));
    }

    #[test]
    fn decision_with_origin_past_grace_needs_operator() {
        let updated = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 1);
        assert!(needs_operator(
            &input(
                MissionStatus::AwaitingUser,
                Some("decision"),
                true,
                &updated,
                false
            ),
            now()
        ));
    }

    #[test]
    fn waiting_for_tool_on_controller_owned_mission_inside_grace_is_not_needs_operator() {
        let updated = ts_ago(30);
        assert!(!needs_operator(
            &input(MissionStatus::Active, None, true, &updated, true),
            now()
        ));
    }

    #[test]
    fn waiting_for_tool_without_origin_needs_operator() {
        let updated = ts_ago(5);
        assert!(needs_operator(
            &input(MissionStatus::Active, None, false, &updated, true),
            now()
        ));
    }

    #[test]
    fn blocked_and_missing_kind_are_not_operator_pages() {
        let updated = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 1);
        assert!(!needs_operator(
            &input(MissionStatus::Blocked, None, false, &updated, false),
            now()
        ));
        assert!(!needs_operator(
            &input(MissionStatus::AwaitingUser, None, false, &updated, false),
            now()
        ));
    }

    fn sample_mission(
        kind: Option<AwaitingKind>,
        origin: Option<&str>,
        updated_at: &str,
    ) -> Mission {
        Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::AwaitingUser,
            title: Some("q".into()),
            short_description: None,
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: Uuid::nil(),
            workspace_name: None,
            agent: None,
            model_override: None,
            model_effort: None,
            fast_mode: false,
            backend: "opencode".into(),
            config_profile: None,
            history: vec![],
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            interrupted_at: None,
            paused_at: None,
            resumable: false,
            desktop_sessions: vec![],
            session_id: None,
            terminal_reason: None,
            terminal_evidence: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::Task,
            goal_mode: false,
            goal_objective: None,
            first_viewed_at: None,
            scheduling: Default::default(),
            project: Default::default(),
            activity: MissionActivity {
                last_status_change_at: Some(updated_at.to_string()),
                ..Default::default()
            },
            awaiting_kind: kind,
            origin: origin.map(|_| "hermes".into()),
            origin_session_id: origin.map(str::to_string),
        }
    }

    #[test]
    fn mission_helper_uses_stored_kind_and_origin() {
        let fresh = ts_ago(10);
        let expired = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 5);
        assert!(mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Decision), None, &fresh),
            false,
            now()
        ));
        assert!(!mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Decision), Some("sess-1"), &fresh),
            false,
            now()
        ));
        assert!(mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Decision), Some("sess-1"), &expired),
            false,
            now()
        ));
        assert!(!mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Ack), Some("sess-1"), &expired),
            false,
            now()
        ));
    }
}
