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
    /// Clock for persisted `awaiting_user`+`decision` (`last_status_change_at`).
    pub updated_at: &'a str,
    pub waiting_for_user_tool: bool,
    /// Live AskUserQuestion clock: the user-wait tool's `started_at`.
    /// Never `missions.updated_at` — that is the Active flip.
    pub wait_started_at: Option<&'a str>,
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
    !held_for_controller_triage(input.has_origin_session, page_age_secs(input, now))
}

/// [`needs_operator`] from a stored mission plus optional live AskUserQuestion.
pub fn mission_needs_operator(
    mission: &Mission,
    waiting_for_user_tool: bool,
    wait_started_at: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    needs_operator(
        &attention_input(mission, waiting_for_user_tool, wait_started_at),
        now,
    )
}

pub fn attention_input<'a>(
    mission: &'a Mission,
    waiting_for_user_tool: bool,
    wait_started_at: Option<&'a str>,
) -> OperatorAttentionInput<'a> {
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
        wait_started_at: wait_started_at.filter(|ts| !ts.is_empty()),
    }
}

/// Whether a status-change alert row is itself an operator page.
///
/// `needs_operator=true` on the alerts feed is **current-state**: the mission
/// must qualify now. The event is kept only when it is the page itself
/// (`awaiting_user`) or a live AskUserQuestion (`active` + WaitingUser), so
/// older failed/completed rows on the same mission do not fill Needs You.
pub fn alert_event_is_operator_page(
    event_status: &str,
    current_needs_operator: bool,
    waiting_for_user_tool: bool,
) -> bool {
    if !current_needs_operator {
        return false;
    }
    event_status == "awaiting_user" || (waiting_for_user_tool && event_status == "active")
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

fn page_age_secs(input: &OperatorAttentionInput<'_>, now: DateTime<Utc>) -> i64 {
    if input.waiting_for_user_tool {
        // Missing tool start: treat as now so a long-lived Active row does
        // not page the instant AskUserQuestion fires. Never use updated_at.
        return input
            .wait_started_at
            .map(|ts| age_secs(ts, now))
            .unwrap_or(0);
    }
    age_secs(input.updated_at, now)
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
            wait_started_at: waiting_for_user_tool.then_some(updated_at),
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
        sample_mission_with_status(
            MissionStatus::AwaitingUser,
            kind,
            origin,
            updated_at,
            updated_at,
        )
    }

    fn sample_mission_with_status(
        status: MissionStatus,
        kind: Option<AwaitingKind>,
        origin: Option<&str>,
        updated_at: &str,
        last_status_change_at: &str,
    ) -> Mission {
        Mission {
            id: Uuid::new_v4(),
            status,
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
                last_status_change_at: Some(last_status_change_at.to_string()),
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
            None,
            now()
        ));
        assert!(!mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Decision), Some("sess-1"), &fresh),
            false,
            None,
            now()
        ));
        assert!(mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Decision), Some("sess-1"), &expired),
            false,
            None,
            now()
        ));
        assert!(!mission_needs_operator(
            &sample_mission(Some(AwaitingKind::Ack), Some("sess-1"), &expired),
            false,
            None,
            now()
        ));
    }

    #[test]
    fn live_ask_user_question_clocks_grace_from_updated_at_not_active_flip() {
        let status_flip = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 600);
        let tool_start = ts_ago(30);
        let mission = sample_mission_with_status(
            MissionStatus::Active,
            None,
            Some("sess-1"),
            &tool_start,
            &status_flip,
        );
        assert!(
            !mission_needs_operator(&mission, true, Some(&tool_start), now()),
            "controller-owned AskUserQuestion inside grace must not page just because Active is old"
        );
    }

    #[test]
    fn live_ask_user_question_clocks_grace_from_tool_started_at_not_updated_at() {
        let too_old = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 600);
        let tool_start = ts_ago(30);
        let mission = sample_mission_with_status(
            MissionStatus::Active,
            None,
            Some("sess-1"),
            &too_old,
            &too_old,
        );
        assert!(
            !mission_needs_operator(&mission, true, Some(&tool_start), now()),
            "grace must start at the user-wait tool started_at, not missions.updated_at"
        );
        assert!(
            !mission_needs_operator(&mission, true, None, now()),
            "missing tool start must not fall back to updated_at and page immediately"
        );
        let expired_tool = ts_ago(CONTROLLER_TRIAGE_GRACE_SECS + 1);
        assert!(mission_needs_operator(
            &mission,
            true,
            Some(&expired_tool),
            now()
        ));
    }

    #[test]
    fn alert_event_filter_keeps_pages_not_historical_noise() {
        // ack / in-grace decision: current needs_operator is false.
        assert!(!alert_event_is_operator_page("awaiting_user", false, false));
        // no-origin decision that still qualifies.
        assert!(alert_event_is_operator_page("awaiting_user", true, false));
        // live AskUserQuestion qualifies only with WaitingUser + current page.
        assert!(alert_event_is_operator_page("active", true, true));
        assert!(!alert_event_is_operator_page("active", true, false));
        assert!(!alert_event_is_operator_page("failed", true, true));
        assert!(!alert_event_is_operator_page("completed", true, false));
    }
}
