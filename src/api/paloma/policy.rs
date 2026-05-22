use crate::api::control::MissionStatus;
use crate::api::mission_store::{Mission, MissionMode, TelegramMissionInterestLevel};
use crate::api::paloma::event::PalomaReasonCode;
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct PalomaPolicyInput {
    pub status: MissionStatus,
    pub mission_mode: MissionMode,
    pub interest: TelegramMissionInterestLevel,
    pub started_at: Option<DateTime<Utc>>,
    pub last_user_message_at: Option<DateTime<Utc>>,
    pub now: DateTime<Utc>,
    pub long_running_after: Duration,
    pub quiet_after_user_message: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalomaPolicyDecision {
    pub allowed: bool,
    pub reason_code: PalomaReasonCode,
    pub suppression_reason: Option<&'static str>,
    pub priority: &'static str,
}

pub fn mission_policy_input(
    mission: &Mission,
    interest: TelegramMissionInterestLevel,
    started_at: Option<DateTime<Utc>>,
    last_user_message_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    long_running_after: Duration,
    quiet_after_user_message: Duration,
) -> PalomaPolicyInput {
    PalomaPolicyInput {
        status: mission.status,
        mission_mode: mission.mission_mode.clone(),
        interest,
        started_at,
        last_user_message_at,
        now,
        long_running_after,
        quiet_after_user_message,
    }
}

pub fn evaluate_alert_policy(input: &PalomaPolicyInput) -> PalomaPolicyDecision {
    if input.interest == TelegramMissionInterestLevel::Muted {
        return PalomaPolicyDecision {
            allowed: false,
            reason_code: PalomaReasonCode::Muted,
            suppression_reason: Some("mission_muted"),
            priority: "low",
        };
    }
    if input.mission_mode == MissionMode::Assistant {
        return PalomaPolicyDecision {
            allowed: false,
            reason_code: PalomaReasonCode::AssistantMode,
            suppression_reason: Some("assistant_mode"),
            priority: "low",
        };
    }

    match input.status {
        MissionStatus::AwaitingUser => PalomaPolicyDecision {
            allowed: true,
            reason_code: PalomaReasonCode::AwaitingUser,
            suppression_reason: None,
            priority: "high",
        },
        MissionStatus::Active => {
            let Some(started_at) = input.started_at else {
                return PalomaPolicyDecision {
                    allowed: false,
                    reason_code: PalomaReasonCode::NotActionable,
                    suppression_reason: Some("missing_started_at"),
                    priority: "normal",
                };
            };
            if input.now - started_at < input.long_running_after {
                return PalomaPolicyDecision {
                    allowed: false,
                    reason_code: PalomaReasonCode::NotActionable,
                    suppression_reason: Some("below_long_running_threshold"),
                    priority: "normal",
                };
            }
            if input
                .last_user_message_at
                .map(|last| input.now - last < input.quiet_after_user_message)
                .unwrap_or(false)
            {
                return PalomaPolicyDecision {
                    allowed: false,
                    reason_code: PalomaReasonCode::QuietWindow,
                    suppression_reason: Some("recent_user_message"),
                    priority: "normal",
                };
            }
            PalomaPolicyDecision {
                allowed: true,
                reason_code: PalomaReasonCode::LongRunning,
                suppression_reason: None,
                priority: "normal",
            }
        }
        MissionStatus::Completed
        | MissionStatus::Failed
        | MissionStatus::Blocked
        | MissionStatus::Interrupted
        | MissionStatus::NotFeasible => {
            let high_interest = input.interest == TelegramMissionInterestLevel::High;
            let long_running = input
                .started_at
                .map(|started| input.now - started >= input.long_running_after)
                .unwrap_or(false);
            if high_interest || long_running {
                PalomaPolicyDecision {
                    allowed: true,
                    reason_code: if high_interest {
                        PalomaReasonCode::TerminalHighInterest
                    } else {
                        PalomaReasonCode::TerminalLongRunning
                    },
                    suppression_reason: None,
                    priority: if matches!(
                        input.status,
                        MissionStatus::Failed | MissionStatus::Blocked
                    ) {
                        "high"
                    } else {
                        "low"
                    },
                }
            } else {
                PalomaPolicyDecision {
                    allowed: false,
                    reason_code: PalomaReasonCode::NotActionable,
                    suppression_reason: Some("terminal_not_long_running_or_high_interest"),
                    priority: "low",
                }
            }
        }
        _ => PalomaPolicyDecision {
            allowed: false,
            reason_code: PalomaReasonCode::NotActionable,
            suppression_reason: Some("status_not_actionable"),
            priority: "low",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn input_for(status: MissionStatus) -> PalomaPolicyInput {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 1, 0, 0).unwrap();
        PalomaPolicyInput {
            status,
            mission_mode: MissionMode::Task,
            interest: TelegramMissionInterestLevel::Normal,
            started_at: Some(Utc.with_ymd_and_hms(2026, 5, 20, 0, 0, 0).unwrap()),
            last_user_message_at: None,
            now,
            long_running_after: Duration::minutes(30),
            quiet_after_user_message: Duration::minutes(30),
        }
    }

    #[test]
    fn muted_wins_over_awaiting_user() {
        let input = PalomaPolicyInput {
            status: MissionStatus::AwaitingUser,
            mission_mode: MissionMode::Task,
            interest: TelegramMissionInterestLevel::Muted,
            started_at: None,
            last_user_message_at: None,
            now: Utc.with_ymd_and_hms(2026, 5, 20, 1, 0, 0).unwrap(),
            long_running_after: Duration::minutes(30),
            quiet_after_user_message: Duration::minutes(30),
        };
        let decision = evaluate_alert_policy(&input);
        assert!(!decision.allowed);
        assert_eq!(decision.reason_code, PalomaReasonCode::Muted);
    }

    #[test]
    fn awaiting_user_alerts_by_default() {
        let decision = evaluate_alert_policy(&input_for(MissionStatus::AwaitingUser));

        assert!(decision.allowed);
        assert_eq!(decision.reason_code, PalomaReasonCode::AwaitingUser);
        assert_eq!(decision.priority, "high");
    }

    #[test]
    fn active_requires_long_running_and_quiet_window() {
        let mut missing_start = input_for(MissionStatus::Active);
        missing_start.started_at = None;
        let missing_start_decision = evaluate_alert_policy(&missing_start);
        assert!(!missing_start_decision.allowed);
        assert_eq!(
            missing_start_decision.suppression_reason,
            Some("missing_started_at")
        );

        let mut fresh = input_for(MissionStatus::Active);
        fresh.started_at = Some(Utc.with_ymd_and_hms(2026, 5, 20, 0, 45, 0).unwrap());
        let fresh_decision = evaluate_alert_policy(&fresh);
        assert!(!fresh_decision.allowed);
        assert_eq!(
            fresh_decision.suppression_reason,
            Some("below_long_running_threshold")
        );

        let mut recently_messaged = input_for(MissionStatus::Active);
        recently_messaged.last_user_message_at =
            Some(Utc.with_ymd_and_hms(2026, 5, 20, 0, 45, 0).unwrap());
        let quiet_decision = evaluate_alert_policy(&recently_messaged);
        assert!(!quiet_decision.allowed);
        assert_eq!(quiet_decision.reason_code, PalomaReasonCode::QuietWindow);

        let allowed = evaluate_alert_policy(&input_for(MissionStatus::Active));
        assert!(allowed.allowed);
        assert_eq!(allowed.reason_code, PalomaReasonCode::LongRunning);
    }

    #[test]
    fn terminal_statuses_need_long_running_or_high_interest() {
        let mut short_completed = input_for(MissionStatus::Completed);
        short_completed.started_at = Some(Utc.with_ymd_and_hms(2026, 5, 20, 0, 45, 0).unwrap());
        let suppressed = evaluate_alert_policy(&short_completed);
        assert!(!suppressed.allowed);
        assert_eq!(
            suppressed.suppression_reason,
            Some("terminal_not_long_running_or_high_interest")
        );

        short_completed.interest = TelegramMissionInterestLevel::High;
        let high_interest = evaluate_alert_policy(&short_completed);
        assert!(high_interest.allowed);
        assert_eq!(
            high_interest.reason_code,
            PalomaReasonCode::TerminalHighInterest
        );

        let long_running_failed = evaluate_alert_policy(&input_for(MissionStatus::Failed));
        assert!(long_running_failed.allowed);
        assert_eq!(long_running_failed.priority, "high");

        let mut missing_started_at = input_for(MissionStatus::Failed);
        missing_started_at.started_at = None;
        let missing_started_at_decision = evaluate_alert_policy(&missing_started_at);
        assert!(!missing_started_at_decision.allowed);
        assert_eq!(
            missing_started_at_decision.suppression_reason,
            Some("terminal_not_long_running_or_high_interest")
        );
    }

    #[test]
    fn assistant_mode_is_suppressed_before_status_policy() {
        let mut input = input_for(MissionStatus::AwaitingUser);
        input.mission_mode = MissionMode::Assistant;

        let decision = evaluate_alert_policy(&input);

        assert!(!decision.allowed);
        assert_eq!(decision.reason_code, PalomaReasonCode::AssistantMode);
        assert_eq!(decision.suppression_reason, Some("assistant_mode"));
    }
}
