//! Pure rendering of a per-mission Telegram card.
//!
//! The card is the default channel for mission updates: one Telegram message
//! per mission, edited in place as state changes. This module owns the
//! text+keyboard rendering and the content-hash that lets the scheduler skip
//! identical edits. IO (sending/editing the Telegram message, persisting the
//! anchor) lives in the Telegram bridge.

use crate::api::control::MissionStatus;
use crate::api::mission_store::Mission;
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// Maximum characters of the latest activity line shown on the card. Short
/// enough to fit even when prepended with a name and emoji.
const LATEST_LINE_MAX_CHARS: usize = 200;

/// Rendered card content. Pure data; the bridge turns it into the actual
/// `editMessageText` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardContent {
    /// Mission title (already truncated/cleaned for display).
    pub title: String,
    /// Status emoji prefix, e.g. "🟢", "🟡", "✅".
    pub status_emoji: &'static str,
    /// One-line status lead, e.g. "Active for 4h 12m" or "Waiting for your input".
    pub status_line: String,
    /// Optional latest activity line (assistant message, error, status change).
    pub latest_line: Option<String>,
    /// True when the card should stop updating after this render.
    pub archived: bool,
}

/// Inline-button suggestion. The bridge maps these to Telegram inline keyboard
/// buttons with appropriate callback_data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardButton {
    /// "Reply": user taps to write a free-text reply that lands as a
    /// `UserMessage` for this mission.
    Reply,
    /// "Open in dashboard": deep link to the mission view.
    OpenDashboard,
    /// "Mute mission": flips the subscription to `Muted`.
    MuteMission,
    /// "Acknowledge": clears `AwaitingUser` without typing a reply.
    Acknowledge,
}

impl CardButton {
    pub fn label(&self) -> &'static str {
        match self {
            CardButton::Reply => "Reply",
            CardButton::OpenDashboard => "Open in dashboard",
            CardButton::MuteMission => "Mute mission",
            CardButton::Acknowledge => "Acknowledge",
        }
    }

    pub fn callback_kind(&self) -> &'static str {
        match self {
            CardButton::Reply => "reply",
            CardButton::OpenDashboard => "open_dashboard",
            CardButton::MuteMission => "mute_mission",
            CardButton::Acknowledge => "acknowledge",
        }
    }
}

/// Pick the status-emoji prefix for a mission status. Kept deliberately small
/// and recognizable; do not introduce per-status colour variants.
pub fn status_emoji(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::Active => "🟢",
        MissionStatus::AwaitingUser => "🟡",
        MissionStatus::Pending => "⏳",
        MissionStatus::Completed => "✅",
        MissionStatus::Failed => "❌",
        MissionStatus::Blocked => "🚧",
        MissionStatus::Interrupted => "⏸️",
        MissionStatus::NotFeasible => "🚫",
        MissionStatus::Acknowledged => "☑️",
        MissionStatus::Paused => "⏯️",
    }
}

/// Inline keyboard layout for a card, conditional on mission status.
pub fn buttons_for(status: MissionStatus) -> Vec<CardButton> {
    match status {
        MissionStatus::AwaitingUser => vec![
            CardButton::Reply,
            CardButton::Acknowledge,
            CardButton::OpenDashboard,
            CardButton::MuteMission,
        ],
        MissionStatus::Active | MissionStatus::Pending | MissionStatus::Paused => vec![
            CardButton::Reply,
            CardButton::OpenDashboard,
            CardButton::MuteMission,
        ],
        MissionStatus::Completed
        | MissionStatus::Failed
        | MissionStatus::Blocked
        | MissionStatus::Interrupted
        | MissionStatus::NotFeasible
        | MissionStatus::Acknowledged => vec![CardButton::OpenDashboard],
    }
}

/// Render a card for a mission and its recent events.
///
/// `latest_event_line` is the cleanest one-line summary the bridge has for the
/// mission's most recent meaningful event (we let the bridge compute it because
/// `event_summary_line` lives there and reuses Telegram-side helpers). Pass
/// `None` when no displayable event exists yet.
pub fn render_card(
    mission: &Mission,
    title: &str,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    latest_event_line: Option<&str>,
) -> CardContent {
    let status_line = status_line_for(mission.status, started_at, now);
    let latest = latest_event_line.map(|line| truncate_chars(line.trim(), LATEST_LINE_MAX_CHARS));
    CardContent {
        title: title.to_string(),
        status_emoji: status_emoji(mission.status),
        status_line,
        latest_line: latest.filter(|line| !line.is_empty()),
        archived: is_terminal(mission.status),
    }
}

/// Convert a `CardContent` to the literal Telegram message text. Two trailing
/// blank lines are normalised away; everything else is preserved so the hash
/// is stable across renders.
pub fn card_to_telegram_text(content: &CardContent) -> String {
    let mut text = String::with_capacity(content.title.len() + content.status_line.len() + 64);
    text.push_str(content.status_emoji);
    text.push(' ');
    text.push_str(&content.title);
    text.push('\n');
    text.push_str(&content.status_line);
    if let Some(latest) = content.latest_line.as_deref() {
        text.push_str("\n\nLatest: ");
        text.push_str(latest);
    }
    text
}

/// Stable SHA-256 hash of the rendered text. The bridge stores this on the
/// anchor row; if it matches the next render, `editMessageText` is skipped.
pub fn content_hash(content: &CardContent) -> String {
    let mut hasher = Sha256::new();
    hasher.update(card_to_telegram_text(content).as_bytes());
    hex::encode(hasher.finalize())
}

fn is_terminal(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Completed
            | MissionStatus::Failed
            | MissionStatus::Blocked
            | MissionStatus::Interrupted
            | MissionStatus::NotFeasible
            | MissionStatus::Acknowledged
    )
}

fn status_line_for(
    status: MissionStatus,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    let elapsed = started_at.map(|started| now - started);
    match status {
        MissionStatus::Active => match elapsed {
            Some(elapsed) if elapsed >= Duration::minutes(1) => {
                format!("Active — running for {}", format_elapsed(elapsed))
            }
            _ => "Active".to_string(),
        },
        MissionStatus::AwaitingUser => "Waiting for your input".to_string(),
        MissionStatus::Pending => "Pending".to_string(),
        MissionStatus::Completed => match elapsed {
            Some(elapsed) if elapsed >= Duration::minutes(1) => {
                format!("Completed — ran for {}", format_elapsed(elapsed))
            }
            _ => "Completed".to_string(),
        },
        MissionStatus::Failed => "Failed".to_string(),
        MissionStatus::Blocked => "Blocked".to_string(),
        MissionStatus::Interrupted => "Interrupted".to_string(),
        MissionStatus::NotFeasible => "Marked not feasible".to_string(),
        MissionStatus::Acknowledged => "Acknowledged".to_string(),
        MissionStatus::Paused => "Paused".to_string(),
    }
}

/// Format a duration as a coarse human string ("4h 12m", "32m", "2d 3h").
/// Deliberately low-precision: the card refreshes every couple of seconds, so
/// minute granularity is plenty and seconds would churn the hash needlessly.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total_minutes = elapsed.num_minutes().max(0);
    if total_minutes < 60 {
        return format!("{}m", total_minutes);
    }
    let total_hours = elapsed.num_hours();
    if total_hours < 24 {
        let minutes = total_minutes % 60;
        if minutes == 0 {
            return format!("{}h", total_hours);
        }
        return format!("{}h {}m", total_hours, minutes);
    }
    let days = total_hours / 24;
    let hours = total_hours % 24;
    if hours == 0 {
        format!("{}d", days)
    } else {
        format!("{}d {}h", days, hours)
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Helper for callers that need to pull the mission's start timestamp for
/// elapsed-time computation. Uses `mission.created_at` directly — earlier
/// versions sniffed the first event, but the card now loads only the
/// *latest* N events, so the first one in the window is not the mission
/// start. The mission row's `created_at` is always present and stable.
pub fn mission_started_at(mission: &Mission) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&mission.created_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::MissionMode;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn mission_with_status(status: MissionStatus, title: &str) -> Mission {
        Mission {
            id: Uuid::nil(),
            status,
            title: Some(title.to_string()),
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
            backend: "claudecode".to_string(),
            config_profile: None,
            history: vec![],
            created_at: "2026-05-24T00:00:00Z".to_string(),
            updated_at: "2026-05-24T00:00:00Z".to_string(),
            interrupted_at: None,
            paused_at: None,
            resumable: false,
            desktop_sessions: vec![],
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::Task,
            goal_mode: false,
            goal_objective: None,
            first_viewed_at: None,
            scheduling: Default::default(),
            project: Default::default(),
            activity: Default::default(),
            awaiting_kind: None,
        }
    }

    #[test]
    fn format_elapsed_uses_minute_granularity_until_day_boundary() {
        assert_eq!(format_elapsed(Duration::seconds(45)), "0m");
        assert_eq!(format_elapsed(Duration::minutes(32)), "32m");
        assert_eq!(format_elapsed(Duration::minutes(60)), "1h");
        assert_eq!(format_elapsed(Duration::minutes(252)), "4h 12m");
        assert_eq!(format_elapsed(Duration::hours(24)), "1d");
        assert_eq!(format_elapsed(Duration::hours(51)), "2d 3h");
    }

    #[test]
    fn status_emoji_covers_all_statuses() {
        for status in [
            MissionStatus::Active,
            MissionStatus::AwaitingUser,
            MissionStatus::Pending,
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Blocked,
            MissionStatus::Interrupted,
            MissionStatus::NotFeasible,
            MissionStatus::Acknowledged,
        ] {
            assert!(!status_emoji(status).is_empty());
        }
    }

    #[test]
    fn render_active_card_includes_runtime_and_latest_event() {
        let mission = mission_with_status(MissionStatus::Active, "Concrete Audit");
        let started = Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 4, 12, 0).unwrap();
        let card = render_card(
            &mission,
            "Concrete Audit",
            Some(started),
            now,
            Some("Concrete Audit replied: explored auditor traits in src/audit/mod.rs"),
        );

        assert_eq!(card.status_emoji, "🟢");
        assert_eq!(card.status_line, "Active — running for 4h 12m");
        assert_eq!(
            card.latest_line.as_deref(),
            Some("Concrete Audit replied: explored auditor traits in src/audit/mod.rs")
        );
        assert!(!card.archived);
    }

    #[test]
    fn render_awaiting_user_card_drops_runtime_and_shows_prompt() {
        let mission = mission_with_status(MissionStatus::AwaitingUser, "Verity proof");
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 5, 0, 0).unwrap();
        let card = render_card(&mission, "Verity proof", None, now, None);

        assert_eq!(card.status_emoji, "🟡");
        assert_eq!(card.status_line, "Waiting for your input");
        assert!(card.latest_line.is_none());
        assert!(!card.archived);
    }

    #[test]
    fn render_terminal_card_archives() {
        let mission = mission_with_status(MissionStatus::Completed, "Keel OS");
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 6, 0, 0).unwrap();
        let started = Utc.with_ymd_and_hms(2026, 5, 24, 5, 30, 0).unwrap();
        let card = render_card(&mission, "Keel OS", Some(started), now, None);

        assert!(card.archived);
        assert_eq!(card.status_line, "Completed — ran for 30m");
    }

    #[test]
    fn telegram_text_layout_is_stable_across_renders_with_same_inputs() {
        let mission = mission_with_status(MissionStatus::Active, "Audit");
        let started = Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 4, 12, 0).unwrap();
        let card_a = render_card(
            &mission,
            "Audit",
            Some(started),
            now,
            Some("worked on tests"),
        );
        let card_b = render_card(
            &mission,
            "Audit",
            Some(started),
            now,
            Some("worked on tests"),
        );

        let text_a = card_to_telegram_text(&card_a);
        let text_b = card_to_telegram_text(&card_b);
        assert_eq!(text_a, text_b);
        assert_eq!(content_hash(&card_a), content_hash(&card_b));
        assert!(text_a.contains("🟢 Audit"));
        assert!(text_a.contains("Latest: worked on tests"));
    }

    #[test]
    fn content_hash_changes_when_status_line_changes() {
        let mission_active = mission_with_status(MissionStatus::Active, "Audit");
        let mission_awaiting = mission_with_status(MissionStatus::AwaitingUser, "Audit");
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 4, 0, 0).unwrap();

        let card_a = render_card(&mission_active, "Audit", Some(now), now, None);
        let card_b = render_card(&mission_awaiting, "Audit", Some(now), now, None);
        assert_ne!(content_hash(&card_a), content_hash(&card_b));
    }

    #[test]
    fn content_hash_is_stable_within_a_minute_for_active_mission() {
        // We do NOT want every-second elapsed updates to churn the hash. The
        // card refreshes every ~2s; minute-granularity status lines keep the
        // hash quiet when nothing meaningful changed.
        let mission = mission_with_status(MissionStatus::Active, "Audit");
        let started = Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap();
        let now_a = Utc.with_ymd_and_hms(2026, 5, 24, 4, 12, 5).unwrap();
        let now_b = Utc.with_ymd_and_hms(2026, 5, 24, 4, 12, 55).unwrap();
        let card_a = render_card(&mission, "Audit", Some(started), now_a, None);
        let card_b = render_card(&mission, "Audit", Some(started), now_b, None);

        assert_eq!(content_hash(&card_a), content_hash(&card_b));
    }

    #[test]
    fn buttons_depend_on_status() {
        assert!(buttons_for(MissionStatus::AwaitingUser).contains(&CardButton::Acknowledge));
        assert!(!buttons_for(MissionStatus::Active).contains(&CardButton::Acknowledge));
        assert_eq!(
            buttons_for(MissionStatus::Completed),
            vec![CardButton::OpenDashboard]
        );
    }

    #[test]
    fn latest_line_is_truncated_with_ellipsis() {
        let mission = mission_with_status(MissionStatus::Active, "X");
        let now = Utc.with_ymd_and_hms(2026, 5, 24, 4, 0, 0).unwrap();
        let long = "a".repeat(LATEST_LINE_MAX_CHARS + 50);
        let card = render_card(&mission, "X", None, now, Some(&long));
        let latest = card.latest_line.expect("expected latest line");
        assert!(latest.chars().count() <= LATEST_LINE_MAX_CHARS);
        assert!(latest.ends_with('…'));
    }
}
