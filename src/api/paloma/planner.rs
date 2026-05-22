use crate::api::control::MissionStatus;
use crate::api::mission_store::{
    Mission, MissionMode, StoredEvent, TelegramAlertPreference, TelegramMissionInterestLevel,
};
use crate::api::paloma::policy::{
    evaluate_alert_policy, mission_policy_input, PalomaPolicyDecision,
};
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

pub fn alert_kind_for_status(status: MissionStatus) -> Option<&'static str> {
    match status {
        MissionStatus::Active => Some("mission_long_running"),
        MissionStatus::AwaitingUser => Some("mission_awaiting_user"),
        MissionStatus::Completed => Some("mission_completed"),
        MissionStatus::Failed => Some("mission_failed"),
        MissionStatus::Blocked => Some("mission_blocked"),
        MissionStatus::Interrupted => Some("mission_interrupted"),
        MissionStatus::NotFeasible => Some("mission_not_feasible"),
        _ => None,
    }
}

pub fn alert_importance_for_mission(
    mission: &Mission,
    interest: TelegramMissionInterestLevel,
) -> &'static str {
    if interest == TelegramMissionInterestLevel::High {
        return "high";
    }
    match mission.status {
        MissionStatus::AwaitingUser | MissionStatus::Failed | MissionStatus::Blocked => "high",
        MissionStatus::Active => "normal",
        _ => "low",
    }
}

pub fn parse_event_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn latest_user_message_at(events: &[StoredEvent]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == "user_message")
        .and_then(|event| parse_event_time(&event.timestamp))
}

pub fn mission_started_at(mission: &Mission) -> Option<DateTime<Utc>> {
    parse_event_time(&mission.created_at).or_else(|| parse_event_time(&mission.updated_at))
}

pub fn evaluate_mission_alert_policy_at(
    mission: &Mission,
    events: &[StoredEvent],
    interest: TelegramMissionInterestLevel,
    now: DateTime<Utc>,
    long_running_after: Duration,
    quiet_after_user_message: Duration,
) -> PalomaPolicyDecision {
    let input = mission_policy_input(
        mission,
        interest,
        mission_started_at(mission),
        latest_user_message_at(events),
        now,
        long_running_after,
        quiet_after_user_message,
    );
    evaluate_alert_policy(&input)
}

pub fn should_alert_long_running_mission_at(
    mission: &Mission,
    events: &[StoredEvent],
    now: DateTime<Utc>,
    long_running_after: Duration,
    quiet_after_user_message: Duration,
) -> bool {
    if mission.status != MissionStatus::Active || mission.mission_mode == MissionMode::Assistant {
        return false;
    }
    evaluate_mission_alert_policy_at(
        mission,
        events,
        TelegramMissionInterestLevel::Normal,
        now,
        long_running_after,
        quiet_after_user_message,
    )
    .allowed
}

pub fn should_alert_mission_at(
    mission: &Mission,
    events: &[StoredEvent],
    interest: TelegramMissionInterestLevel,
    now: DateTime<Utc>,
    long_running_after: Duration,
    quiet_after_user_message: Duration,
) -> bool {
    evaluate_mission_alert_policy_at(
        mission,
        events,
        interest,
        now,
        long_running_after,
        quiet_after_user_message,
    )
    .allowed
}

pub fn alert_suppression_reason(
    mission: &Mission,
    events: &[StoredEvent],
    interest: TelegramMissionInterestLevel,
    now: DateTime<Utc>,
    long_running_after: Duration,
    quiet_after_user_message: Duration,
) -> &'static str {
    evaluate_mission_alert_policy_at(
        mission,
        events,
        interest,
        now,
        long_running_after,
        quiet_after_user_message,
    )
    .suppression_reason
    .unwrap_or("policy_suppressed")
}

pub fn alert_event_kind_at(
    mission: &Mission,
    base_kind: &str,
    events: &[StoredEvent],
    now: DateTime<Utc>,
    long_running_alert_bucket: Duration,
) -> String {
    if mission.status == MissionStatus::Active && base_kind == "mission_long_running" {
        let bucket_seconds = long_running_alert_bucket.num_seconds().max(1);
        let bucket = now.timestamp().div_euclid(bucket_seconds);
        return format!("{base_kind}:{bucket}");
    }

    let status = mission.status.to_string();
    let updated = events
        .iter()
        .rev()
        .find(|event| {
            event.event_type == "mission_status_changed"
                && event
                    .metadata
                    .get("status")
                    .and_then(|value| value.as_str())
                    == Some(status.as_str())
        })
        .map(|event| event.timestamp.as_str())
        .unwrap_or(mission.updated_at.as_str());
    format!("{base_kind}:{}", updated.replace([':', '.', '+'], "-"))
}

pub fn preference_is_failure_only(preference: &TelegramAlertPreference) -> bool {
    let rule = preference.rule_text.to_ascii_lowercase();
    preference.enabled
        && rule.contains("only")
        && (rule.contains("fail") || rule.contains("failure"))
}

pub fn mission_has_failure_only_preference(
    preferences: &[TelegramAlertPreference],
    mission_id: Uuid,
) -> bool {
    let mission_id = mission_id.to_string();
    preferences.iter().any(|preference| {
        preference.scope == "mission"
            && preference.scope_value.as_deref() == Some(mission_id.as_str())
            && preference_is_failure_only(preference)
    })
}
