use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalomaEventSource {
    TelegramWebhook,
    MissionObserver,
    Scheduler,
    LocalSatellite,
}

impl PalomaEventSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TelegramWebhook => "telegram_webhook",
            Self::MissionObserver => "mission_observer",
            Self::Scheduler => "scheduler",
            Self::LocalSatellite => "local_satellite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalomaReasonCode {
    AwaitingUser,
    LongRunning,
    TerminalHighInterest,
    TerminalLongRunning,
    Muted,
    QuietWindow,
    AssistantMode,
    NotActionable,
    DigestSend,
    DigestSuppressed,
}

impl PalomaReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingUser => "awaiting_user",
            Self::LongRunning => "long_running",
            Self::TerminalHighInterest => "terminal_high_interest",
            Self::TerminalLongRunning => "terminal_long_running",
            Self::Muted => "muted",
            Self::QuietWindow => "quiet_window",
            Self::AssistantMode => "assistant_mode",
            Self::NotActionable => "not_actionable",
            Self::DigestSend => "digest_send",
            Self::DigestSuppressed => "digest_suppressed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalomaEvent {
    pub source: PalomaEventSource,
    pub channel: String,
    pub user_id: Option<i64>,
    pub mission_id: Option<Uuid>,
    pub reason_code: PalomaReasonCode,
}
