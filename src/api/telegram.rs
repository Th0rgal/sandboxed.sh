//! Telegram bridge service.
//!
//! Connects Telegram bots to assistant missions using webhooks (instant delivery)
//! and streaming responses via `sendChatAction` + `editMessageText`.
//!
//! Flow:
//! 1. On channel creation, registers a Telegram webhook pointing at our public endpoint
//! 2. Telegram POSTs updates instantly to `/api/telegram/webhook/:channel_id`
//! 3. The webhook handler routes the message as `ControlCommand::UserMessage`
//! 4. A response task streams `TextDelta` events back via `editMessageText`

use crate::api::control::{AgentEvent, ControlCommand, MissionStatus};
use crate::api::mission_store::{
    now_string, Mission, MissionMode, MissionStore, StoredEvent, TelegramActionExecution,
    TelegramActionExecutionKind, TelegramActionExecutionStatus, TelegramAlert,
    TelegramAlertPreference, TelegramChannel, TelegramChatMission, TelegramConversation,
    TelegramConversationMessage, TelegramConversationMessageDirection,
    TelegramMissionInterestLevel, TelegramMissionSubscription, TelegramScheduledMessage,
    TelegramScheduledMessageStatus, TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind,
    TelegramStructuredMemoryScope, TelegramTriggerMode, TelegramUser, TelegramUserRole,
    TelegramWorkflow, TelegramWorkflowEvent, TelegramWorkflowKind, TelegramWorkflowStatus,
};
use crate::api::paloma::queue::{PalomaQueue, QueueKey, QueueMetrics};
use crate::api::paloma::scheduler::{PalomaJobRegistry, PalomaJobState};
use crate::api::paloma::{commands as paloma_commands, decision_log, digest, planner};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use uuid::Uuid;

/// Shared handle to the Telegram bridge manager.
pub type SharedTelegramBridge = Arc<TelegramBridge>;
type TelegramChatLockMap = HashMap<(Uuid, i64), Arc<Mutex<()>>>;
type PalomaSessionLockMap = HashMap<(Uuid, i64), Arc<Mutex<()>>>;

fn paloma_channel_job_name(base_name: &str, channel_id: Uuid) -> String {
    format!("{base_name}:{channel_id}")
}

fn paloma_scheduler_lease_expires_at() -> String {
    (Utc::now() + ChronoDuration::seconds(PALOMA_SCHEDULER_JOB_LEASE_SECONDS)).to_rfc3339()
}

/// Manages Telegram webhook registrations and channel routing context.
pub struct TelegramBridge {
    /// Routing context for each active channel (needed to forward webhook messages).
    active_channels: RwLock<HashMap<Uuid, ChannelContext>>,
    /// Per-chat locks to serialize auto-create mission resolution.
    chat_locks: RwLock<TelegramChatLockMap>,
    /// Per-channel/chat Paloma session locks to serialize inbound control messages.
    paloma_session_locks: RwLock<PalomaSessionLockMap>,
    /// Bounded per-session queue for inbound Paloma work.
    paloma_session_queue: Mutex<PalomaQueue<()>>,
    /// Runtime state for named Paloma scheduler jobs.
    paloma_jobs: Mutex<PalomaJobRegistry>,
    /// Recently seen Telegram update IDs for webhook idempotence.
    recent_updates: RwLock<HashMap<(Uuid, i64), Instant>>,
    /// Sent outbound reply messages keyed by the inbound Telegram message they reply to.
    recent_replies: RwLock<HashMap<TelegramReplyKey, TelegramReplyRecord>>,
    scheduler_started: AtomicBool,
    http: Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TelegramReplyKey {
    channel_id: Uuid,
    chat_id: i64,
    reply_to_message_id: i64,
}

#[derive(Debug, Clone, Copy)]
struct TelegramReplyRecord {
    message_id: i64,
    created_at: Instant,
}

const TELEGRAM_UPDATE_DEDUP_TTL: Duration = Duration::from_secs(15 * 60);
const TELEGRAM_REPLY_DEDUP_TTL: Duration = Duration::from_secs(15 * 60);
const TELEGRAM_SCHEDULE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PALOMA_SCHEDULER_JOB_LEASE_SECONDS: i64 = 300;
const PALOMA_LONG_RUNNING_MINUTES: i64 = 30;
const PALOMA_USER_MESSAGE_QUIET_MINUTES: i64 = 30;
const PALOMA_LONG_RUNNING_ALERT_BUCKET_MINUTES: i64 = 30;
const PALOMA_ALERT_DIGEST_LIMIT: usize = 12;
const PALOMA_SESSION_QUEUE_MAX_PER_KEY: usize = 256;
const DEFAULT_PALOMA_OWNER_TELEGRAM_ID: i64 = 1_139_694_048;
const DEFAULT_PALOMA_TRUSTED_FRIEND_TELEGRAM_ID: i64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TelegramActionKind {
    Send,
    Reminder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramAction {
    kind: TelegramActionKind,
    target: String,
    delay_seconds: u64,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedTelegramMemory {
    kind: TelegramStructuredMemoryKind,
    label: Option<String>,
    value: String,
}

#[derive(Debug, Clone, Default)]
struct TelegramMemorySubject {
    user_id: Option<i64>,
    username: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum TelegramActionTarget {
    Current,
    ChatId(i64),
    ChatTitle(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramActionExecutionResult {
    pub channel_id: Uuid,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_message_id: Option<Uuid>,
    pub immediate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramWorkflowRequestResult {
    pub workflow_id: Uuid,
    pub channel_id: Uuid,
    pub origin_chat_id: i64,
    pub target_chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chat_title: Option<String>,
}

fn workflow_request_delivery_text(request_text: &str, target_chat_type: Option<&str>) -> String {
    let requires_reply = matches!(target_chat_type, Some("group") | Some("supergroup"));
    if !requires_reply {
        return request_text.to_string();
    }

    format!(
        "{}\n\nReply directly to this message so I can route your answer back to the originating chat.",
        request_text.trim()
    )
}

fn workflow_reply_text(clean_text: &str, file_annotation: Option<&str>) -> String {
    match (clean_text.trim(), file_annotation.map(str::trim)) {
        ("", Some(file_info)) if !file_info.is_empty() => file_info.to_string(),
        (text, Some(file_info)) if !text.is_empty() && !file_info.is_empty() => {
            format!("{}\n{}", text, file_info)
        }
        (text, _) => text.to_string(),
    }
}

fn workflow_requires_direct_reply(workflow: &TelegramWorkflow) -> bool {
    matches!(
        workflow.target_chat_type.as_deref(),
        Some("group") | Some("supergroup")
    )
}

fn configured_telegram_id(env_name: &str, default_value: i64) -> Option<i64> {
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .or_else(|| (default_value != 0).then_some(default_value))
}

fn paloma_role_for_user(user_id: i64) -> TelegramUserRole {
    if configured_telegram_id("PALOMA_TELEGRAM_OWNER_ID", DEFAULT_PALOMA_OWNER_TELEGRAM_ID)
        == Some(user_id)
    {
        TelegramUserRole::Owner
    } else if configured_telegram_id(
        "PALOMA_TELEGRAM_TRUSTED_FRIEND_ID",
        DEFAULT_PALOMA_TRUSTED_FRIEND_TELEGRAM_ID,
    ) == Some(user_id)
    {
        TelegramUserRole::TrustedFriend
    } else {
        TelegramUserRole::Observer
    }
}

fn telegram_display_name(user: &User) -> String {
    match user.last_name.as_deref() {
        Some(last) if !last.is_empty() => format!("{} {}", user.first_name, last),
        _ => user.first_name.clone(),
    }
}

async fn remember_paloma_user(ctx: &ChannelContext, msg: &Message) -> Option<TelegramUser> {
    let from = msg.from.as_ref()?;
    let now = now_string();
    let user = TelegramUser {
        id: Uuid::new_v4(),
        telegram_user_id: from.id,
        username: from.username.clone(),
        display_name: Some(telegram_display_name(from)),
        role: paloma_role_for_user(from.id),
        created_at: now.clone(),
        updated_at: now,
    };
    match ctx.mission_store.upsert_telegram_user(user.clone()).await {
        Ok(user) => Some(user),
        Err(err) => {
            tracing::warn!("Failed to upsert Telegram user role: {}", err);
            Some(user)
        }
    }
}

fn is_owner_dm(user: Option<&TelegramUser>, msg: &Message) -> bool {
    msg.chat.chat_type == "private"
        && user
            .map(|u| u.role == TelegramUserRole::Owner)
            .unwrap_or(false)
}

fn paloma_chat_is_allowed(channel: &TelegramChannel, msg: &Message) -> bool {
    msg.chat.chat_type == "private"
        || channel.allowed_chat_ids.is_empty()
        || channel.allowed_chat_ids.contains(&msg.chat.id)
}

fn is_paloma_shared_summary_allowed(
    channel: &TelegramChannel,
    user: Option<&TelegramUser>,
    msg: &Message,
    command_text: &str,
) -> bool {
    if !matches!(msg.chat.chat_type.as_str(), "group" | "supergroup") {
        return false;
    }
    if channel.allowed_chat_ids.is_empty() || !paloma_chat_is_allowed(channel, msg) {
        return false;
    }
    if !paloma_commands::is_exact_command(command_text, "/summary") {
        return false;
    }
    matches!(
        user.map(|u| u.role),
        Some(TelegramUserRole::Owner | TelegramUserRole::TrustedFriend)
    )
}

fn should_silence_paloma_shared_command(
    channel: &TelegramChannel,
    user: Option<&TelegramUser>,
    msg: &Message,
    command_text: &str,
) -> bool {
    matches!(msg.chat.chat_type.as_str(), "group" | "supergroup")
        && command_text.trim().starts_with('/')
        && !is_paloma_shared_summary_allowed(channel, user, msg, command_text)
}

fn is_paloma_command(text: &str, name: &str) -> bool {
    paloma_commands::is_command(text, name)
}

fn normalize_paloma_natural_command(text: &str) -> Option<&'static str> {
    paloma_commands::normalize_natural_command(text)
}

fn redact_for_telegram(text: &str) -> String {
    let token_re = regex::Regex::new(
        r#"(?i)(bot[0-9]{6,}:[A-Za-z0-9_-]{20,}|[A-Za-z0-9_]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}|api[_-]?hash\s*[:=]\s*[A-Za-z0-9]{16,}|token\s*[:=]\s*\S+|token[-_][A-Za-z0-9_-]{16,}|secret\s*[:=]\s*\S+)"#,
    )
    .expect("telegram redaction regex must compile");
    let path_re = regex::Regex::new(r#"(/(?:root|home|workspaces|tmp)/[^\s,)]+)"#)
        .expect("telegram path redaction regex must compile");
    let redacted = token_re.replace_all(text, "[redacted]");
    path_re
        .replace_all(&redacted, "[path redacted]")
        .to_string()
}

fn mission_label(mission: &Mission) -> String {
    mission
        .title
        .as_deref()
        .or(mission.short_description.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled mission")
        .chars()
        .take(80)
        .collect()
}

fn event_summary_line(mission: &Mission, event: &StoredEvent) -> Option<String> {
    let title = mission_label(mission);
    match event.event_type.as_str() {
        "mission_status_changed" => {
            let status = event
                .metadata
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("updated");
            Some(format!("{} is now {}.", title, status))
        }
        "assistant_message" => {
            if event.content.trim().is_empty() {
                None
            } else {
                let text = redact_for_telegram(event.content.trim());
                let first = text.lines().next().unwrap_or("").trim();
                if first.is_empty() {
                    None
                } else {
                    Some(format!(
                        "{} replied: {}",
                        title,
                        first.chars().take(140).collect::<String>()
                    ))
                }
            }
        }
        "error" => Some(format!("{} reported an error.", title)),
        "tool_call" => event
            .tool_name
            .as_deref()
            .map(|tool| format!("{} used {}.", title, tool)),
        _ => None,
    }
}

fn mission_rank(mission: &Mission, interest: TelegramMissionInterestLevel) -> i32 {
    let mut score = match interest {
        TelegramMissionInterestLevel::High => 100,
        TelegramMissionInterestLevel::Normal => 20,
        TelegramMissionInterestLevel::Muted => -100,
    };
    score += match mission.status {
        MissionStatus::Blocked | MissionStatus::Failed => 60,
        MissionStatus::AwaitingUser => 50,
        MissionStatus::Active => 40,
        MissionStatus::Pending => 25,
        MissionStatus::Interrupted => 20,
        MissionStatus::Acknowledged | MissionStatus::Completed | MissionStatus::NotFeasible => 0,
    };
    if mission.parent_mission_id.is_none() {
        score += 5;
    }
    score
}

fn paloma_status_seen_sequences(cursor_json: &str) -> HashMap<String, i64> {
    serde_json::from_str::<serde_json::Value>(cursor_json)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(mission_id, sequence)| sequence.as_i64().map(|seq| (mission_id, seq)))
        .collect()
}

fn paloma_dashboard_cursor_is_newer(
    status_cursor: Option<&str>,
    dashboard_cursor: Option<&str>,
) -> bool {
    match (status_cursor, dashboard_cursor) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(status), Some(dashboard)) => dashboard > status,
    }
}

fn paloma_status_event_is_new(
    mission_id: Uuid,
    event: &StoredEvent,
    seen_sequences: &HashMap<String, i64>,
    status_since: Option<&str>,
    dashboard_since: Option<&str>,
) -> bool {
    if paloma_dashboard_cursor_is_newer(status_since, dashboard_since)
        && dashboard_since
            .map(|since| event.timestamp.as_str() <= since)
            .unwrap_or(false)
    {
        return false;
    }

    if let Some(seen_sequence) = seen_sequences.get(&mission_id.to_string()) {
        return event.sequence > *seen_sequence;
    }

    status_since
        .map(|since| event.timestamp.as_str() > since)
        .unwrap_or(true)
}

async fn build_paloma_status(
    ctx: &ChannelContext,
    telegram_user_id: i64,
) -> Result<(String, String), String> {
    let cursor = ctx
        .mission_store
        .get_or_create_telegram_user_cursor(telegram_user_id)
        .await?;
    let status_since = cursor.last_status_at.as_deref();
    let dashboard_since = cursor.last_dashboard_seen_at.as_deref();
    let seen_sequences =
        paloma_status_seen_sequences(&cursor.last_seen_event_sequence_by_mission_json);
    let missions = ctx.mission_store.list_missions(80, 0).await?;
    let mut lines = Vec::new();
    let mut max_sequences = serde_json::Map::new();

    for mission in &missions {
        let events = ctx
            .mission_store
            .get_events(mission.id, None, Some(200), None)
            .await
            .unwrap_or_default();
        if let Some(max) = events.iter().map(|event| event.sequence).max() {
            max_sequences.insert(mission.id.to_string(), serde_json::json!(max));
        }
        for event in events {
            if !paloma_status_event_is_new(
                mission.id,
                &event,
                &seen_sequences,
                status_since,
                dashboard_since,
            ) {
                continue;
            }
            if let Some(line) = event_summary_line(mission, &event) {
                lines.push(line);
            }
            if lines.len() >= 8 {
                break;
            }
        }
        if lines.len() >= 8 {
            break;
        }
    }

    let body = if lines.is_empty() {
        match status_since.or(dashboard_since) {
            Some(_) => "No meaningful changes since your last status check.".to_string(),
            None => "No meaningful mission changes found yet.".to_string(),
        }
    } else {
        let mut body = format!(
            "{} meaningful change{} since you last checked.",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        );
        for (idx, line) in lines.into_iter().enumerate() {
            body.push_str(&format!("\n\n{}. {}", idx + 1, line));
        }
        redact_for_telegram(&body)
    };

    Ok((body, serde_json::Value::Object(max_sequences).to_string()))
}

async fn build_paloma_missions(
    ctx: &ChannelContext,
    telegram_user_id: i64,
) -> Result<String, String> {
    let subscriptions = ctx
        .mission_store
        .list_telegram_mission_subscriptions(telegram_user_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|sub| (sub.mission_id, sub.interest_level))
        .collect::<HashMap<_, _>>();
    let mut missions = ctx.mission_store.list_missions(50, 0).await?;
    missions.sort_by_key(|mission| {
        let interest = subscriptions
            .get(&mission.id)
            .copied()
            .unwrap_or(TelegramMissionInterestLevel::Normal);
        -mission_rank(mission, interest)
    });

    let mut high = Vec::new();
    let mut other_count = 0usize;
    for mission in missions {
        let interest = subscriptions
            .get(&mission.id)
            .copied()
            .unwrap_or(TelegramMissionInterestLevel::Normal);
        if interest == TelegramMissionInterestLevel::Muted {
            continue;
        }
        let line = format!("- {}: {}", mission_label(&mission), mission.status);
        if high.len() < 8 && mission_rank(&mission, interest) >= 40 {
            high.push(line);
        } else {
            other_count += 1;
        }
    }

    let mut body = "Active missions".to_string();
    if high.is_empty() {
        body.push_str("\n\nNo high-interest active missions.");
    } else {
        body.push_str("\n\nHigh interest");
        for line in high {
            body.push('\n');
            body.push_str(&line);
        }
    }
    if other_count > 0 {
        body.push_str(&format!(
            "\n\nOther\n- {} lower-priority mission{}",
            other_count,
            if other_count == 1 { "" } else { "s" }
        ));
    }
    Ok(redact_for_telegram(&body))
}

fn parse_paloma_selector_and_payload<'a>(
    text: &'a str,
    command: &str,
) -> Option<(&'a str, &'a str)> {
    paloma_commands::parse_selector_and_payload(text, command)
}

fn feedback_mutes_alerts(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    normalized.contains("don't tell me about this again")
        || normalized.contains("dont tell me about this again")
        || normalized.contains("i am not interested in these updates")
        || normalized.contains("i'm not interested in these updates")
        || normalized.contains("not interested in these updates")
        || normalized.contains("not interested in this")
        || normalized.contains("mute this")
        || normalized.contains("too many updates")
        || normalized.contains("stop these updates")
        || normalized.contains("stop alerting me about this")
        || normalized.contains("stop sending me updates about this")
        || normalized.contains("stop updating me about this")
}

fn feedback_raises_interest(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    normalized.contains("tell me more about this")
        || normalized.contains("keep me posted")
        || normalized.contains("mark this high")
}

fn feedback_only_failures(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    normalized.contains("only tell me if this fails")
        || normalized.contains("only tell me when this fails")
        || normalized.contains("only failures")
        || normalized.contains("only failure updates")
        || normalized.contains("just failures")
}

fn paloma_command_error_response(err: &str) -> &str {
    if err.starts_with("Usage:") {
        err
    } else if err == "ambiguous_digest_reply" {
        "That digest covers multiple missions. Use /summary <mission> or /send <mission> <message>."
    } else {
        "I couldn't read mission status right now."
    }
}

async fn select_paloma_mission(ctx: &ChannelContext, selector: &str) -> Result<Mission, String> {
    let selector = selector.trim();
    let missions = ctx.mission_store.list_missions(80, 0).await?;
    if selector.eq_ignore_ascii_case("latest") || selector.eq_ignore_ascii_case("current") {
        return missions
            .into_iter()
            .next()
            .ok_or_else(|| "No missions are available.".to_string());
    }
    if let Ok(id) = Uuid::parse_str(selector) {
        return ctx
            .mission_store
            .get_mission(id)
            .await?
            .ok_or_else(|| format!("Mission {} was not found.", id));
    }

    let needle = selector.to_ascii_lowercase();
    missions
        .into_iter()
        .find(|mission| {
            mission_label(mission)
                .to_ascii_lowercase()
                .contains(&needle)
        })
        .ok_or_else(|| format!("No mission matched '{}'.", selector))
}

async fn latest_interesting_mission(ctx: &ChannelContext) -> Result<Mission, String> {
    let mut missions = ctx.mission_store.list_missions(80, 0).await?;
    missions.sort_by_key(|mission| -mission_rank(mission, TelegramMissionInterestLevel::Normal));
    missions
        .into_iter()
        .next()
        .ok_or_else(|| "No missions are available.".to_string())
}

fn select_latest_awaiting_user_mission(mut missions: Vec<Mission>) -> Option<Mission> {
    missions.retain(|mission| mission.status == MissionStatus::AwaitingUser);
    missions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    missions.into_iter().next()
}

async fn latest_awaiting_user_mission(ctx: &ChannelContext) -> Result<Mission, String> {
    select_latest_awaiting_user_mission(ctx.mission_store.list_missions(80, 0).await?)
        .ok_or_else(|| "No mission is waiting for approval right now.".to_string())
}

async fn build_paloma_summary(
    ctx: &ChannelContext,
    selector: Option<&str>,
) -> Result<String, String> {
    let mission = match selector.map(str::trim).filter(|value| !value.is_empty()) {
        Some(selector) => select_paloma_mission(ctx, selector).await?,
        None => latest_interesting_mission(ctx).await?,
    };
    build_paloma_summary_for_mission(ctx, &mission).await
}

async fn build_paloma_summary_for_mission(
    ctx: &ChannelContext,
    mission: &Mission,
) -> Result<String, String> {
    let events = ctx
        .mission_store
        .get_events(mission.id, None, Some(80), None)
        .await
        .unwrap_or_default();
    let mut lines = Vec::new();
    for event in events.into_iter().rev() {
        if let Some(line) = event_summary_line(mission, &event) {
            lines.push(line);
        }
        if lines.len() >= 4 {
            break;
        }
    }

    let mut body = format!("{}: {}", mission_label(mission), mission.status);
    if lines.is_empty() {
        body.push_str("\n\nNo concise recent summary is available yet.");
    } else {
        for line in lines {
            body.push_str("\n- ");
            body.push_str(&line);
        }
    }
    Ok(redact_for_telegram(&body))
}

fn paloma_shared_summary_body(mission: &Mission) -> String {
    redact_for_telegram(&format!(
        "{}: {}\n\nShared-chat summary is limited to high-level status.",
        mission_label(mission),
        mission.status
    ))
}

async fn build_paloma_shared_summary(ctx: &ChannelContext) -> Result<String, String> {
    latest_interesting_mission(ctx)
        .await
        .map(|mission| paloma_shared_summary_body(&mission))
}

async fn build_paloma_why(ctx: &ChannelContext) -> Result<String, String> {
    let decisions = ctx.mission_store.list_paloma_decisions(8).await?;
    if decisions.is_empty() {
        return Ok("No Paloma decisions have been recorded yet.".to_string());
    }
    let mut body = "Recent Paloma decisions".to_string();
    for decision in decisions {
        let mission = decision
            .mission_id
            .map(|id| id.simple().to_string().chars().take(8).collect::<String>())
            .unwrap_or_else(|| "no mission".to_string());
        let outcome = if decision.allowed {
            "allowed"
        } else {
            decision
                .suppression_reason
                .as_deref()
                .unwrap_or("suppressed")
        };
        body.push_str(&format!(
            "\n- {} {} {} ({})",
            decision.reason_code, decision.proposed_action, outcome, mission
        ));
    }
    Ok(redact_for_telegram(&body))
}

async fn send_paloma_mission_message(
    ctx: &ChannelContext,
    selector: &str,
    payload: &str,
) -> Result<String, String> {
    let mission = select_paloma_mission(ctx, selector).await?;
    send_user_message_to_mission(ctx, &mission, payload).await
}

async fn send_user_message_to_mission(
    ctx: &ChannelContext,
    mission: &Mission,
    payload: &str,
) -> Result<String, String> {
    let (queued_tx, _queued_rx) = tokio::sync::oneshot::channel();
    ctx.cmd_tx
        .send(ControlCommand::UserMessage {
            id: Uuid::new_v4(),
            content: payload.to_string(),
            agent: None,
            target_mission_id: Some(mission.id),
            respond: queued_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("Sent to {}.", mission_label(mission)))
}

async fn send_paloma_approval(ctx: &ChannelContext, payload: &str) -> Result<String, String> {
    let mission = latest_awaiting_user_mission(ctx).await?;
    send_user_message_to_mission(ctx, &mission, payload).await
}

async fn paloma_replied_alert_mission(
    ctx: &ChannelContext,
    user: &TelegramUser,
    msg: &Message,
) -> Result<Option<Mission>, String> {
    if let Some(reply_message_id) = msg.reply_to_message.as_ref().map(|reply| reply.message_id) {
        if let Some(alert) = ctx
            .mission_store
            .get_telegram_alert_by_message_id(user.telegram_user_id, reply_message_id)
            .await?
        {
            if let Some(mission_id) = alert.mission_id {
                if let Some(mission) = ctx.mission_store.get_mission(mission_id).await? {
                    return Ok(Some(mission));
                }
            }
        }
    }
    Ok(None)
}

async fn paloma_feedback_target_mission(
    ctx: &ChannelContext,
    user: &TelegramUser,
    msg: &Message,
) -> Result<Mission, String> {
    if let Some(mission) = paloma_replied_alert_mission(ctx, user, msg).await? {
        return Ok(mission);
    }
    latest_interesting_mission(ctx).await
}

async fn apply_paloma_interest_feedback(
    ctx: &ChannelContext,
    user: &TelegramUser,
    msg: &Message,
    level: TelegramMissionInterestLevel,
    rule_text: &str,
) -> Result<String, String> {
    let mission = paloma_feedback_target_mission(ctx, user, msg).await?;
    let now = now_string();
    let subscription = TelegramMissionSubscription {
        id: Uuid::new_v4(),
        telegram_user_id: user.telegram_user_id,
        mission_id: mission.id,
        interest_level: level,
        reason: Some(rule_text.to_string()),
        expires_at: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    ctx.mission_store
        .upsert_telegram_mission_subscription(subscription)
        .await?;
    if level == TelegramMissionInterestLevel::Muted {
        let _ = ctx
            .mission_store
            .acknowledge_pending_telegram_alerts_for_mission(
                user.telegram_user_id,
                mission.id,
                &now,
            )
            .await;
        let _ = ctx
            .mission_store
            .update_telegram_user_last_alert_ack_at(user.telegram_user_id, &now)
            .await;
    }
    let _ = ctx
        .mission_store
        .create_telegram_alert_preference(TelegramAlertPreference {
            id: Uuid::new_v4(),
            telegram_user_id: user.telegram_user_id,
            scope: "mission".to_string(),
            scope_value: Some(mission.id.to_string()),
            rule_text: rule_text.to_string(),
            enabled: true,
            created_from_message_id: Some(msg.message_id),
            created_at: now.clone(),
            updated_at: now,
        })
        .await;
    let response = match level {
        TelegramMissionInterestLevel::Muted => {
            format!(
                "Noted. I muted routine alerts for {}.",
                mission_label(&mission)
            )
        }
        TelegramMissionInterestLevel::High => {
            format!(
                "Noted. I will prioritize updates for {}.",
                mission_label(&mission)
            )
        }
        TelegramMissionInterestLevel::Normal => {
            format!(
                "Noted. I reset {} to normal interest.",
                mission_label(&mission)
            )
        }
    };
    Ok(redact_for_telegram(&response))
}

async fn apply_paloma_failure_only_feedback(
    ctx: &ChannelContext,
    user: &TelegramUser,
    msg: &Message,
) -> Result<String, String> {
    let mission = paloma_feedback_target_mission(ctx, user, msg).await?;
    let now = now_string();
    ctx.mission_store
        .upsert_telegram_mission_subscription(TelegramMissionSubscription {
            id: Uuid::new_v4(),
            telegram_user_id: user.telegram_user_id,
            mission_id: mission.id,
            interest_level: TelegramMissionInterestLevel::Normal,
            reason: Some("only failures from Telegram feedback".to_string()),
            expires_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await?;
    let _ = ctx
        .mission_store
        .create_telegram_alert_preference(TelegramAlertPreference {
            id: Uuid::new_v4(),
            telegram_user_id: user.telegram_user_id,
            scope: "mission".to_string(),
            scope_value: Some(mission.id.to_string()),
            rule_text: "only failures from Telegram feedback".to_string(),
            enabled: true,
            created_from_message_id: Some(msg.message_id),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await;
    let _ = ctx
        .mission_store
        .acknowledge_pending_telegram_alerts_for_mission(user.telegram_user_id, mission.id, &now)
        .await;
    Ok(redact_for_telegram(&format!(
        "Noted. I will only alert you if {} fails.",
        mission_label(&mission)
    )))
}

fn paloma_alert_kind_for_status(status: MissionStatus) -> Option<&'static str> {
    planner::alert_kind_for_status(status)
}

fn paloma_alert_importance_for_mission(
    mission: &Mission,
    interest: TelegramMissionInterestLevel,
) -> &'static str {
    planner::alert_importance_for_mission(mission, interest)
}

fn paloma_alert_event_kind_at(
    mission: &Mission,
    base_kind: &str,
    events: &[StoredEvent],
    now: chrono::DateTime<Utc>,
) -> String {
    planner::alert_event_kind_at(
        mission,
        base_kind,
        events,
        now,
        ChronoDuration::minutes(PALOMA_LONG_RUNNING_ALERT_BUCKET_MINUTES),
    )
}

#[cfg(test)]
fn paloma_should_alert_long_running_mission_at(
    mission: &Mission,
    events: &[StoredEvent],
    now: chrono::DateTime<Utc>,
) -> bool {
    planner::should_alert_long_running_mission_at(
        mission,
        events,
        now,
        ChronoDuration::minutes(PALOMA_LONG_RUNNING_MINUTES),
        ChronoDuration::minutes(PALOMA_USER_MESSAGE_QUIET_MINUTES),
    )
}

fn paloma_should_alert_mission_at(
    mission: &Mission,
    events: &[StoredEvent],
    interest: TelegramMissionInterestLevel,
    now: chrono::DateTime<Utc>,
) -> bool {
    planner::should_alert_mission_at(
        mission,
        events,
        interest,
        now,
        ChronoDuration::minutes(PALOMA_LONG_RUNNING_MINUTES),
        ChronoDuration::minutes(PALOMA_USER_MESSAGE_QUIET_MINUTES),
    )
}

fn paloma_alert_suppression_reason(
    mission: &Mission,
    events: &[StoredEvent],
    interest: TelegramMissionInterestLevel,
    now: chrono::DateTime<Utc>,
) -> &'static str {
    planner::alert_suppression_reason(
        mission,
        events,
        interest,
        now,
        ChronoDuration::minutes(PALOMA_LONG_RUNNING_MINUTES),
        ChronoDuration::minutes(PALOMA_USER_MESSAGE_QUIET_MINUTES),
    )
}

fn paloma_mission_has_failure_only_preference(
    preferences: &[TelegramAlertPreference],
    mission_id: Uuid,
) -> bool {
    planner::mission_has_failure_only_preference(preferences, mission_id)
}

fn paloma_policy_snapshot_json(
    mission: &Mission,
    interest: TelegramMissionInterestLevel,
    alert_now: chrono::DateTime<Utc>,
) -> String {
    serde_json::json!({
        "status": mission.status.to_string(),
        "mission_mode": format!("{:?}", mission.mission_mode),
        "interest": interest,
        "long_running_minutes": PALOMA_LONG_RUNNING_MINUTES,
        "quiet_after_user_message_minutes": PALOMA_USER_MESSAGE_QUIET_MINUTES,
        "evaluated_at": alert_now.to_rfc3339(),
    })
    .to_string()
}

#[allow(clippy::too_many_arguments)]
async fn log_paloma_alert_decision(
    ctx: &ChannelContext,
    owner_id: i64,
    mission: &Mission,
    reason_code: &str,
    proposed_action: &str,
    allowed: bool,
    suppression_reason: Option<&str>,
    policy_snapshot_json: String,
    generated_text: Option<&str>,
) {
    let decision = decision_log::new_decision(
        "scheduler",
        Some(mission.id),
        Some(owner_id),
        "telegram",
        reason_code,
        proposed_action,
        allowed,
        suppression_reason,
        &policy_snapshot_json,
        generated_text,
        now_string(),
    );
    if let Err(err) = ctx.mission_store.create_paloma_decision(decision).await {
        tracing::debug!(
            mission_id = %mission.id,
            "Failed to record Paloma decision: {}",
            err
        );
    }
}

fn paloma_latest_attention_line(mission: &Mission, events: &[StoredEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match event.event_type.as_str() {
            "assistant_message" | "error" | "mission_status_changed" => {
                event_summary_line(mission, event)
            }
            _ => None,
        })
}

fn paloma_alert_body(mission: &Mission, events: &[StoredEvent]) -> String {
    let title = mission_label(mission);
    let lead = match mission.status {
        MissionStatus::Active => format!("{title} is still running."),
        MissionStatus::AwaitingUser => format!("{title} is waiting for your input."),
        MissionStatus::Completed => format!("{title} completed."),
        MissionStatus::Failed => format!("{title} failed."),
        MissionStatus::Blocked => format!("{title} is blocked."),
        MissionStatus::Interrupted => format!("{title} was interrupted."),
        MissionStatus::NotFeasible => format!("{title} was marked not feasible."),
        _ => format!("{title} is now {}.", mission.status),
    };

    match paloma_latest_attention_line(mission, events) {
        Some(line) => redact_for_telegram(&format!("{lead}\n\nLatest: {line}")),
        None => redact_for_telegram(&lead),
    }
}

fn paloma_alert_digest_text(alerts: &[TelegramAlert]) -> String {
    digest::alert_digest_text(alerts, redact_for_telegram)
}

fn telegram_shared_file_caption(
    mission_id: Uuid,
    file: &crate::api::control::SharedFile,
    context: &str,
) -> String {
    let mut caption = format!(
        "Mission {} shared {}",
        mission_id
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>(),
        file.name
    );
    let context = sanitize_telegram_visible_text(context);
    let first_context_line = context
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if !first_context_line.is_empty() {
        caption.push('\n');
        caption.push_str(&first_context_line.chars().take(180).collect::<String>());
    }
    redact_for_telegram(&caption)
        .chars()
        .take(900)
        .collect::<String>()
}

async fn paloma_pending_alert_still_eligible(
    ctx: &ChannelContext,
    alert: &TelegramAlert,
    interest: TelegramMissionInterestLevel,
    now: chrono::DateTime<Utc>,
) -> bool {
    let Some(mission_id) = alert.mission_id else {
        return true;
    };
    let Ok(Some(mission)) = ctx.mission_store.get_mission(mission_id).await else {
        return false;
    };
    let Some(current_kind) = paloma_alert_kind_for_status(mission.status) else {
        return false;
    };
    if !alert.event_kind.starts_with(current_kind) {
        return false;
    }
    let events = ctx
        .mission_store
        .get_events(mission.id, None, Some(40), None)
        .await
        .unwrap_or_default();
    paloma_should_alert_mission_at(&mission, &events, interest, now)
}

async fn plan_and_deliver_paloma_alerts(ctx: &ChannelContext, http: &Client) {
    let Some(owner_id) =
        configured_telegram_id("PALOMA_TELEGRAM_OWNER_ID", DEFAULT_PALOMA_OWNER_TELEGRAM_ID)
    else {
        return;
    };
    let subscriptions = ctx
        .mission_store
        .list_telegram_mission_subscriptions(owner_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|sub| (sub.mission_id, sub.interest_level))
        .collect::<HashMap<_, _>>();
    let alert_preferences = ctx
        .mission_store
        .list_telegram_alert_preferences(owner_id)
        .await
        .unwrap_or_default();
    let missions = match ctx.mission_store.list_missions(80, 0).await {
        Ok(missions) => missions,
        Err(err) => {
            tracing::warn!("Failed to load missions for Telegram alerts: {}", err);
            return;
        }
    };

    for mission in missions {
        let Some(base_kind) = paloma_alert_kind_for_status(mission.status) else {
            continue;
        };
        let interest = subscriptions
            .get(&mission.id)
            .copied()
            .unwrap_or(TelegramMissionInterestLevel::Normal);
        let title = mission_label(&mission);
        let events = ctx
            .mission_store
            .get_events(mission.id, None, Some(40), None)
            .await
            .unwrap_or_default();
        let alert_now = Utc::now();
        let policy_snapshot = paloma_policy_snapshot_json(&mission, interest, alert_now);
        if interest == TelegramMissionInterestLevel::Muted {
            log_paloma_alert_decision(
                ctx,
                owner_id,
                &mission,
                base_kind,
                "create_alert",
                false,
                Some("mission_muted"),
                policy_snapshot,
                None,
            )
            .await;
            continue;
        }
        if paloma_mission_has_failure_only_preference(&alert_preferences, mission.id)
            && mission.status != MissionStatus::Failed
        {
            log_paloma_alert_decision(
                ctx,
                owner_id,
                &mission,
                base_kind,
                "create_alert",
                false,
                Some("only_failures_preference"),
                policy_snapshot,
                None,
            )
            .await;
            continue;
        }
        if !paloma_should_alert_mission_at(&mission, &events, interest, alert_now) {
            log_paloma_alert_decision(
                ctx,
                owner_id,
                &mission,
                base_kind,
                "create_alert",
                false,
                Some(paloma_alert_suppression_reason(
                    &mission, &events, interest, alert_now,
                )),
                policy_snapshot,
                None,
            )
            .await;
            continue;
        }
        let body = paloma_alert_body(&mission, &events);
        log_paloma_alert_decision(
            ctx,
            owner_id,
            &mission,
            base_kind,
            "create_alert",
            true,
            None,
            policy_snapshot,
            Some(&body),
        )
        .await;
        let now = now_string();
        let _ = ctx
            .mission_store
            .create_telegram_alert_if_absent(TelegramAlert {
                id: Uuid::new_v4(),
                telegram_user_id: owner_id,
                mission_id: Some(mission.id),
                event_kind: paloma_alert_event_kind_at(&mission, base_kind, &events, alert_now),
                importance: paloma_alert_importance_for_mission(&mission, interest).to_string(),
                title,
                body,
                status: "pending".to_string(),
                telegram_message_id: None,
                last_error: None,
                created_at: now,
                sent_at: None,
                acknowledged_at: None,
            })
            .await;
    }

    let mut pending = ctx
        .mission_store
        .list_pending_telegram_alerts(owner_id, PALOMA_ALERT_DIGEST_LIMIT)
        .await
        .unwrap_or_default();
    let muted_pending = pending
        .iter()
        .filter_map(|alert| alert.mission_id.map(|mission_id| (alert.id, mission_id)))
        .filter(|(_, mission_id)| {
            subscriptions
                .get(mission_id)
                .copied()
                .unwrap_or(TelegramMissionInterestLevel::Normal)
                == TelegramMissionInterestLevel::Muted
        })
        .collect::<Vec<_>>();
    let failure_only_pending = pending
        .iter()
        .filter_map(|alert| alert.mission_id.map(|mission_id| (alert.id, mission_id)))
        .filter(|(_, mission_id)| {
            paloma_mission_has_failure_only_preference(&alert_preferences, *mission_id)
        })
        .filter(|(alert_id, _)| {
            pending
                .iter()
                .find(|alert| alert.id == *alert_id)
                .map(|alert| !alert.event_kind.starts_with("mission_failed"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if !muted_pending.is_empty() || !failure_only_pending.is_empty() {
        let acknowledged_at = now_string();
        for (_, mission_id) in &muted_pending {
            let _ = ctx
                .mission_store
                .acknowledge_pending_telegram_alerts_for_mission(
                    owner_id,
                    *mission_id,
                    &acknowledged_at,
                )
                .await;
        }
        for (alert_id, _) in &failure_only_pending {
            let _ = ctx
                .mission_store
                .acknowledge_pending_telegram_alert(owner_id, *alert_id, &acknowledged_at)
                .await;
        }
        pending.retain(|alert| {
            alert.mission_id.is_none_or(|mission_id| {
                let muted = subscriptions
                    .get(&mission_id)
                    .copied()
                    .unwrap_or(TelegramMissionInterestLevel::Normal)
                    == TelegramMissionInterestLevel::Muted;
                let failure_only_suppressed = failure_only_pending
                    .iter()
                    .any(|(pending_alert_id, _)| *pending_alert_id == alert.id);
                !muted && !failure_only_suppressed
            })
        });
    }
    if !pending.is_empty() {
        let checked_at = Utc::now();
        let mut eligible_pending = Vec::with_capacity(pending.len());
        let acknowledged_at = now_string();
        for alert in pending {
            let interest = alert
                .mission_id
                .and_then(|mission_id| subscriptions.get(&mission_id).copied())
                .unwrap_or(TelegramMissionInterestLevel::Normal);
            if paloma_pending_alert_still_eligible(ctx, &alert, interest, checked_at).await {
                eligible_pending.push(alert);
            } else {
                let _ = ctx
                    .mission_store
                    .acknowledge_pending_telegram_alert(owner_id, alert.id, &acknowledged_at)
                    .await;
            }
        }
        pending = eligible_pending;
    }
    if pending.is_empty() {
        return;
    }
    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    let text = paloma_alert_digest_text(&pending);
    let digest_snapshot = serde_json::json!({
        "pending_alerts": pending.len(),
        "digest_limit": PALOMA_ALERT_DIGEST_LIMIT,
    })
    .to_string();
    match send_message(http, &base_url, owner_id, &text, None).await {
        Ok(message_id) => {
            let sent_at = now_string();
            let _ = ctx
                .mission_store
                .update_telegram_user_last_digest_at(owner_id, &sent_at)
                .await;
            if let Some(first_alert) = pending.first() {
                let decision = decision_log::new_decision(
                    "scheduler",
                    first_alert.mission_id,
                    Some(owner_id),
                    "telegram",
                    "digest_send",
                    "send_digest",
                    true,
                    None,
                    &digest_snapshot,
                    Some(&text),
                    sent_at.clone(),
                );
                let _ = ctx.mission_store.create_paloma_decision(decision).await;
            }
            for alert in pending {
                let _ = ctx
                    .mission_store
                    .mark_telegram_alert_sent(alert.id, Some(message_id), &sent_at)
                    .await;
            }
        }
        Err(err) => {
            for alert in pending {
                tracing::warn!("Failed to send Telegram alert {}: {}", alert.id, err);
                let _ = ctx
                    .mission_store
                    .mark_telegram_alert_failed(alert.id, &err)
                    .await;
            }
        }
    }
}

async fn flush_pending_paloma_digest(
    ctx: &ChannelContext,
    http: &Client,
    owner_id: i64,
) -> Result<usize, String> {
    let subscriptions = ctx
        .mission_store
        .list_telegram_mission_subscriptions(owner_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|sub| (sub.mission_id, sub.interest_level))
        .collect::<HashMap<_, _>>();
    let alert_preferences = ctx
        .mission_store
        .list_telegram_alert_preferences(owner_id)
        .await
        .unwrap_or_default();
    let mut pending = ctx
        .mission_store
        .list_pending_telegram_alerts(owner_id, PALOMA_ALERT_DIGEST_LIMIT)
        .await
        .map_err(|err| format!("failed_to_load_pending_alerts: {err}"))?;

    let muted_pending = pending
        .iter()
        .filter_map(|alert| alert.mission_id.map(|mission_id| (alert.id, mission_id)))
        .filter(|(_, mission_id)| {
            subscriptions
                .get(mission_id)
                .copied()
                .unwrap_or(TelegramMissionInterestLevel::Normal)
                == TelegramMissionInterestLevel::Muted
        })
        .collect::<Vec<_>>();
    let failure_only_pending = pending
        .iter()
        .filter_map(|alert| alert.mission_id.map(|mission_id| (alert.id, mission_id)))
        .filter(|(_, mission_id)| {
            paloma_mission_has_failure_only_preference(&alert_preferences, *mission_id)
        })
        .filter(|(alert_id, _)| {
            pending
                .iter()
                .find(|alert| alert.id == *alert_id)
                .map(|alert| !alert.event_kind.starts_with("mission_failed"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    if !muted_pending.is_empty() || !failure_only_pending.is_empty() {
        let acknowledged_at = now_string();
        for (_, mission_id) in &muted_pending {
            let _ = ctx
                .mission_store
                .acknowledge_pending_telegram_alerts_for_mission(
                    owner_id,
                    *mission_id,
                    &acknowledged_at,
                )
                .await;
        }
        for (alert_id, _) in &failure_only_pending {
            let _ = ctx
                .mission_store
                .acknowledge_pending_telegram_alert(owner_id, *alert_id, &acknowledged_at)
                .await;
        }
        pending.retain(|alert| {
            alert.mission_id.is_none_or(|mission_id| {
                let muted = subscriptions
                    .get(&mission_id)
                    .copied()
                    .unwrap_or(TelegramMissionInterestLevel::Normal)
                    == TelegramMissionInterestLevel::Muted;
                let failure_only_suppressed = failure_only_pending
                    .iter()
                    .any(|(pending_alert_id, _)| *pending_alert_id == alert.id);
                !muted && !failure_only_suppressed
            })
        });
    }

    if !pending.is_empty() {
        let checked_at = Utc::now();
        let mut eligible_pending = Vec::with_capacity(pending.len());
        let acknowledged_at = now_string();
        for alert in pending {
            let interest = alert
                .mission_id
                .and_then(|mission_id| subscriptions.get(&mission_id).copied())
                .unwrap_or(TelegramMissionInterestLevel::Normal);
            if paloma_pending_alert_still_eligible(ctx, &alert, interest, checked_at).await {
                eligible_pending.push(alert);
            } else {
                let _ = ctx
                    .mission_store
                    .acknowledge_pending_telegram_alert(owner_id, alert.id, &acknowledged_at)
                    .await;
            }
        }
        pending = eligible_pending;
    }
    if pending.is_empty() {
        return Ok(0);
    }

    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    let text = paloma_alert_digest_text(&pending);
    let digest_snapshot = serde_json::json!({
        "pending_alerts": pending.len(),
        "digest_limit": PALOMA_ALERT_DIGEST_LIMIT,
        "source": "paloma_digest_flush",
    })
    .to_string();
    match send_message(http, &base_url, owner_id, &text, None).await {
        Ok(message_id) => {
            let sent_at = now_string();
            let _ = ctx
                .mission_store
                .update_telegram_user_last_digest_at(owner_id, &sent_at)
                .await;
            if let Some(first_alert) = pending.first() {
                let decision = decision_log::new_decision(
                    "scheduler",
                    first_alert.mission_id,
                    Some(owner_id),
                    "telegram",
                    "digest_flush",
                    "send_digest",
                    true,
                    None,
                    &digest_snapshot,
                    Some(&text),
                    sent_at.clone(),
                );
                let _ = ctx.mission_store.create_paloma_decision(decision).await;
            }
            let sent_count = pending.len();
            for alert in pending {
                let _ = ctx
                    .mission_store
                    .mark_telegram_alert_sent(alert.id, Some(message_id), &sent_at)
                    .await;
            }
            Ok(sent_count)
        }
        Err(err) => {
            for alert in pending {
                tracing::warn!("Failed to flush Telegram alert {}: {}", alert.id, err);
                let _ = ctx
                    .mission_store
                    .mark_telegram_alert_failed(alert.id, &err)
                    .await;
            }
            Err(err)
        }
    }
}

async fn handle_paloma_command(
    ctx: &ChannelContext,
    msg: &Message,
    http: &Client,
    user: Option<&TelegramUser>,
    clean_text: &str,
) -> bool {
    let normalized_command = normalize_paloma_natural_command(clean_text);
    let command_text = normalized_command.unwrap_or(clean_text);
    let is_known_command = is_paloma_command(command_text, "/status")
        || is_paloma_command(command_text, "/missions")
        || is_paloma_command(command_text, "/summary")
        || is_paloma_command(command_text, "/send")
        || is_paloma_command(command_text, "/approve")
        || is_paloma_command(command_text, "/why");
    if is_known_command && !paloma_chat_is_allowed(&ctx.channel, msg) {
        return false;
    }
    let owner_failure_only_feedback = user
        .filter(|_| msg.chat.chat_type == "private")
        .filter(|u| u.role == TelegramUserRole::Owner)
        .filter(|_| feedback_only_failures(clean_text));
    let owner_feedback = user
        .filter(|_| msg.chat.chat_type == "private")
        .filter(|u| u.role == TelegramUserRole::Owner)
        .and_then(|u| {
            if feedback_mutes_alerts(clean_text) {
                Some((
                    u,
                    TelegramMissionInterestLevel::Muted,
                    "mute from Telegram feedback",
                ))
            } else if feedback_raises_interest(clean_text) {
                Some((
                    u,
                    TelegramMissionInterestLevel::High,
                    "high interest from Telegram feedback",
                ))
            } else {
                None
            }
        });
    let owner_alert_reply =
        if !is_known_command && owner_feedback.is_none() && owner_failure_only_feedback.is_none() {
            match user.filter(|_| msg.chat.chat_type == "private") {
                Some(u) if u.role == TelegramUserRole::Owner => {
                    match paloma_replied_alert_mission(ctx, u, msg).await {
                        Ok(Some(mission)) => Some((u, mission)),
                        Ok(None) => None,
                        Err(err) => {
                            tracing::warn!("Failed to resolve Paloma alert reply target: {}", err);
                            None
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };

    if !is_known_command
        && owner_feedback.is_none()
        && owner_failure_only_feedback.is_none()
        && owner_alert_reply.is_none()
    {
        return false;
    }

    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    if is_paloma_shared_summary_allowed(&ctx.channel, user, msg, command_text) {
        match build_paloma_shared_summary(ctx).await {
            Ok(body) => {
                let _ =
                    send_message(http, &base_url, msg.chat.id, &body, Some(msg.message_id)).await;
            }
            Err(err) => {
                tracing::warn!("Paloma shared summary failed: {}", err);
                let _ = send_message(
                    http,
                    &base_url,
                    msg.chat.id,
                    "I couldn't read mission status right now.",
                    Some(msg.message_id),
                )
                .await;
            }
        }
        return true;
    }
    if should_silence_paloma_shared_command(&ctx.channel, user, msg, command_text) {
        return true;
    }
    if !is_owner_dm(user, msg) {
        let _ = send_message(
            http,
            &base_url,
            msg.chat.id,
            "I can only show mission state to Thomas in DM.",
            Some(msg.message_id),
        )
        .await;
        return true;
    }

    let result = if let Some(user) = owner_failure_only_feedback {
        apply_paloma_failure_only_feedback(ctx, user, msg)
            .await
            .map(|body| (body, None))
    } else if let Some((user, level, rule_text)) = owner_feedback {
        apply_paloma_interest_feedback(ctx, user, msg, level, rule_text)
            .await
            .map(|body| (body, None))
    } else if let Some((_user, mission)) = owner_alert_reply {
        send_user_message_to_mission(ctx, &mission, clean_text)
            .await
            .map(|body| (body, None))
    } else if is_paloma_command(command_text, "/status") {
        match user {
            Some(user) => build_paloma_status(ctx, user.telegram_user_id)
                .await
                .map(|(body, sequences)| (body, Some(sequences))),
            None => Err("Missing Telegram user identity.".to_string()),
        }
    } else if is_paloma_command(command_text, "/missions") {
        match user {
            Some(user) => build_paloma_missions(ctx, user.telegram_user_id)
                .await
                .map(|body| (body, None)),
            None => Err("Missing Telegram user identity.".to_string()),
        }
    } else if is_paloma_command(command_text, "/summary") {
        let selector = command_text.trim().strip_prefix("/summary").map(str::trim);
        match selector.filter(|value| !value.is_empty()) {
            Some(selector) => build_paloma_summary(ctx, Some(selector))
                .await
                .map(|body| (body, None)),
            None => match user {
                Some(user) => match paloma_replied_alert_mission(ctx, user, msg).await {
                    Ok(Some(mission)) => build_paloma_summary_for_mission(ctx, &mission)
                        .await
                        .map(|body| (body, None)),
                    Ok(None) => build_paloma_summary(ctx, None)
                        .await
                        .map(|body| (body, None)),
                    Err(err) => Err(err),
                },
                None => build_paloma_summary(ctx, None)
                    .await
                    .map(|body| (body, None)),
            },
        }
    } else if is_paloma_command(command_text, "/send") {
        match parse_paloma_selector_and_payload(command_text, "/send") {
            Some((selector, payload)) => send_paloma_mission_message(ctx, selector, payload)
                .await
                .map(|body| (body, None)),
            None => Err("Usage: /send <mission selector> <message>".to_string()),
        }
    } else if is_paloma_command(command_text, "/why") {
        build_paloma_why(ctx).await.map(|body| (body, None))
    } else if is_paloma_command(command_text, "/approve") {
        let answer = command_text
            .trim()
            .strip_prefix("/approve")
            .unwrap_or("")
            .trim();
        if answer.is_empty() {
            Err("Usage: /approve <answer>".to_string())
        } else {
            send_paloma_approval(ctx, answer)
                .await
                .map(|body| (body, None))
        }
    } else {
        Err("Unknown Paloma command.".to_string())
    };

    match result {
        Ok((body, sequence_json)) => {
            if send_message(http, &base_url, msg.chat.id, &body, Some(msg.message_id))
                .await
                .is_ok()
            {
                if let (Some(user), Some(sequence_json)) = (user, sequence_json) {
                    let _ = ctx
                        .mission_store
                        .update_telegram_user_last_status_at(
                            user.telegram_user_id,
                            &now_string(),
                            &sequence_json,
                        )
                        .await;
                }
            }
        }
        Err(err) => {
            tracing::warn!("Paloma command failed: {}", err);
            let body = paloma_command_error_response(&err);
            let _ = send_message(http, &base_url, msg.chat.id, body, Some(msg.message_id)).await;
        }
    }
    true
}

fn telegram_internal_action_secret() -> Option<String> {
    std::env::var("SANDBOXED_INTERNAL_ACTION_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("JWT_SECRET")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

pub fn build_internal_telegram_action_token(mission_id: Uuid) -> Option<String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let secret = telegram_internal_action_secret()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(b"telegram-action:");
    mac.update(mission_id.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

pub fn verify_internal_telegram_action_token(mission_id: Uuid, token: &str) -> bool {
    let Some(expected) = build_internal_telegram_action_token(mission_id) else {
        return false;
    };
    super::auth::constant_time_eq(&expected, token.trim())
}

/// Context needed to route incoming webhook messages to a mission.
#[derive(Clone)]
pub struct ChannelContext {
    pub channel: TelegramChannel,
    pub bot_username: String,
    pub cmd_tx: mpsc::Sender<ControlCommand>,
    pub events_tx: broadcast::Sender<AgentEvent>,
    pub mission_store: Arc<dyn MissionStore>,
}

impl Default for TelegramBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramBridge {
    pub fn new() -> Self {
        Self {
            active_channels: RwLock::new(HashMap::new()),
            chat_locks: RwLock::new(HashMap::new()),
            paloma_session_locks: RwLock::new(HashMap::new()),
            paloma_session_queue: Mutex::new(PalomaQueue::new(PALOMA_SESSION_QUEUE_MAX_PER_KEY)),
            paloma_jobs: Mutex::new(PalomaJobRegistry::with_default_jobs()),
            recent_updates: RwLock::new(HashMap::new()),
            recent_replies: RwLock::new(HashMap::new()),
            scheduler_started: AtomicBool::new(false),
            http: Client::new(),
        }
    }

    fn purge_recent_updates(map: &mut HashMap<(Uuid, i64), Instant>, now: Instant) {
        map.retain(|_, seen_at| now.duration_since(*seen_at) <= TELEGRAM_UPDATE_DEDUP_TTL);
    }

    fn purge_recent_replies(
        map: &mut HashMap<TelegramReplyKey, TelegramReplyRecord>,
        now: Instant,
    ) {
        map.retain(|_, record| now.duration_since(record.created_at) <= TELEGRAM_REPLY_DEDUP_TTL);
    }

    /// Returns true if this update has not been seen recently for the channel.
    pub async fn register_update_once(&self, channel_id: Uuid, update_id: i64) -> bool {
        let now = Instant::now();
        let mut updates = self.recent_updates.write().await;
        Self::purge_recent_updates(&mut updates, now);
        updates.insert((channel_id, update_id), now).is_none()
    }

    /// Return a previously sent Telegram message for this inbound reply target, if any.
    pub async fn get_sent_reply_message(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        reply_to_message_id: i64,
    ) -> Option<i64> {
        if reply_to_message_id <= 0 {
            return None;
        }
        let now = Instant::now();
        let mut replies = self.recent_replies.write().await;
        Self::purge_recent_replies(&mut replies, now);
        replies
            .get(&TelegramReplyKey {
                channel_id,
                chat_id,
                reply_to_message_id,
            })
            .map(|record| record.message_id)
    }

    /// Remember which outbound bot message corresponds to an inbound Telegram message.
    pub async fn remember_sent_reply_message(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        reply_to_message_id: i64,
        message_id: i64,
    ) {
        if reply_to_message_id <= 0 {
            return;
        }
        let now = Instant::now();
        let mut replies = self.recent_replies.write().await;
        Self::purge_recent_replies(&mut replies, now);
        replies.insert(
            TelegramReplyKey {
                channel_id,
                chat_id,
                reply_to_message_id,
            },
            TelegramReplyRecord {
                message_id,
                created_at: now,
            },
        );
    }

    /// Get or create the per-chat mutex used to serialize mission auto-creation.
    pub async fn chat_lock(&self, channel_id: Uuid, chat_id: i64) -> Arc<Mutex<()>> {
        {
            let locks = self.chat_locks.read().await;
            if let Some(lock) = locks.get(&(channel_id, chat_id)) {
                return Arc::clone(lock);
            }
        }

        let mut locks = self.chat_locks.write().await;
        Arc::clone(
            locks
                .entry((channel_id, chat_id))
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    /// Get or create the Paloma session mutex used to serialize inbound work per chat.
    pub async fn paloma_session_lock(&self, channel_id: Uuid, chat_id: i64) -> Arc<Mutex<()>> {
        {
            let locks = self.paloma_session_locks.read().await;
            if let Some(lock) = locks.get(&(channel_id, chat_id)) {
                return Arc::clone(lock);
            }
        }

        let mut locks = self.paloma_session_locks.write().await;
        Arc::clone(
            locks
                .entry((channel_id, chat_id))
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    pub async fn enqueue_paloma_session(
        &self,
        channel_id: Uuid,
        chat_id: i64,
        mission_id: Option<Uuid>,
    ) -> Option<QueueKey> {
        let key = QueueKey {
            channel: channel_id.to_string(),
            session_id: chat_id.to_string(),
            mission_id,
            priority: 0,
        };
        let mut queue = self.paloma_session_queue.lock().await;
        match queue.push(key.clone(), ()) {
            Ok(()) => {
                let metrics = queue.metrics();
                tracing::debug!(
                    pending_items = metrics.pending_items,
                    active_sessions = metrics.active_sessions,
                    "Paloma session queue accepted inbound work"
                );
                Some(key)
            }
            Err(()) => {
                let metrics = queue.metrics();
                tracing::warn!(
                    pending_items = metrics.pending_items,
                    active_sessions = metrics.active_sessions,
                    channel_id = %channel_id,
                    chat_id,
                    "Paloma session queue rejected inbound work because the session queue is full"
                );
                None
            }
        }
    }

    pub async fn dequeue_paloma_session(&self, key: &QueueKey) {
        let mut queue = self.paloma_session_queue.lock().await;
        let _ = queue.pop(key);
        queue.cleanup_idle();
        let metrics = queue.metrics();
        tracing::debug!(
            pending_items = metrics.pending_items,
            active_sessions = metrics.active_sessions,
            "Paloma session queue started inbound work"
        );
    }

    pub async fn paloma_queue_metrics(&self) -> QueueMetrics {
        self.paloma_session_queue.lock().await.metrics()
    }

    pub async fn paloma_queue_metrics_for_channels(
        &self,
        channel_ids: &HashSet<String>,
    ) -> QueueMetrics {
        self.paloma_session_queue
            .lock()
            .await
            .metrics_for_channels(channel_ids)
    }

    pub async fn paloma_job_states(&self) -> Vec<PalomaJobState> {
        self.paloma_jobs.lock().await.states()
    }

    async fn record_paloma_job_success(&self, name: &'static str) {
        self.paloma_jobs
            .lock()
            .await
            .record_success(name, now_string());
    }

    async fn record_paloma_job_failure(&self, name: &'static str, error: String) {
        self.paloma_jobs.lock().await.record_failure(name, error);
    }

    /// Register a webhook for a Telegram channel and store routing context.
    pub async fn start_channel(
        self: &Arc<Self>,
        channel: TelegramChannel,
        cmd_tx: mpsc::Sender<ControlCommand>,
        events_tx: broadcast::Sender<AgentEvent>,
        mission_store: Arc<dyn MissionStore>,
        public_base_url: &str,
    ) -> Result<(), String> {
        self.stop_channel(channel.id).await;

        let base_url = format!("https://api.telegram.org/bot{}", channel.bot_token);

        // Resolve bot username
        let bot_username = if let Some(ref u) = channel.bot_username {
            u.clone()
        } else {
            get_bot_username(&self.http, &base_url)
                .await
                .unwrap_or_default()
        };

        // Register the webhook with Telegram
        let webhook_url = format!(
            "{}/api/telegram/webhook/{}",
            public_base_url.trim_end_matches('/'),
            channel.id
        );

        set_webhook(
            &self.http,
            &base_url,
            &webhook_url,
            channel.webhook_secret.as_deref(),
        )
        .await
        .map_err(|e| {
            let msg = format!(
                "Failed to set Telegram webhook for channel {}: {}",
                channel.id, e
            );
            tracing::error!("{}", msg);
            msg
        })?;

        let mode_label = if channel.auto_create_missions {
            "auto-create".to_string()
        } else {
            format!("mission: {}", channel.mission_id)
        };
        tracing::info!(
            "Registered Telegram webhook for channel {} (bot: @{}, {}, url: {})",
            channel.id,
            bot_username,
            mode_label,
            webhook_url,
        );

        let ctx = ChannelContext {
            channel,
            bot_username,
            cmd_tx,
            events_tx,
            mission_store: Arc::clone(&mission_store),
        };

        self.active_channels
            .write()
            .await
            .insert(ctx.channel.id, ctx);

        self.ensure_scheduler_started(mission_store).await;

        Ok(())
    }

    async fn ensure_scheduler_started(self: &Arc<Self>, mission_store: Arc<dyn MissionStore>) {
        if self
            .scheduler_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let bridge = Arc::clone(self);
        tokio::spawn(async move {
            bridge.run_scheduler_loop(mission_store).await;
        });
    }

    async fn run_scheduler_loop(self: Arc<Self>, _mission_store: Arc<dyn MissionStore>) {
        let mut interval = tokio::time::interval(TELEGRAM_SCHEDULE_POLL_INTERVAL);
        let lease_owner = format!("telegram-bridge-{}", std::process::id());
        interval.tick().await;

        loop {
            interval.tick().await;
            let channels: Vec<_> = self
                .active_channels
                .read()
                .await
                .values()
                .cloned()
                .collect();
            for ctx in channels {
                let alert_job_name = paloma_channel_job_name("paloma_alert_scan", ctx.channel.id);
                let alert_job_started = now_string();
                let alert_job_lease_expires = paloma_scheduler_lease_expires_at();
                if ctx
                    .mission_store
                    .claim_paloma_scheduler_job(
                        &alert_job_name,
                        &lease_owner,
                        &alert_job_started,
                        &alert_job_lease_expires,
                    )
                    .await
                    .unwrap_or(false)
                {
                    plan_and_deliver_paloma_alerts(&ctx, &self.http).await;
                    let _ = ctx
                        .mission_store
                        .finish_paloma_scheduler_job(
                            &alert_job_name,
                            &lease_owner,
                            &now_string(),
                            None,
                        )
                        .await;
                    self.record_paloma_job_success("paloma_alert_scan").await;
                }

                let due_job_name = paloma_channel_job_name("paloma_due_messages", ctx.channel.id);
                let due_job_started = now_string();
                let due_job_lease_expires = paloma_scheduler_lease_expires_at();
                if ctx
                    .mission_store
                    .claim_paloma_scheduler_job(
                        &due_job_name,
                        &lease_owner,
                        &due_job_started,
                        &due_job_lease_expires,
                    )
                    .await
                    .unwrap_or(false)
                {
                    match ctx
                        .mission_store
                        .list_due_telegram_scheduled_messages(ctx.channel.id, &now_string(), 32)
                        .await
                    {
                        Ok(due) => {
                            for message in due {
                                // Claim this message atomically: UPDATE … WHERE status='pending'
                                // so concurrent ticks cannot pick up the same message.
                                let claimed = ctx
                                    .mission_store
                                    .claim_telegram_scheduled_message(message.id)
                                    .await;
                                match claimed {
                                    Ok(false) => continue, // Another tick already claimed it
                                    Err(err) => {
                                        tracing::warn!(
                                            scheduled_message_id = %message.id,
                                            "Failed to claim Telegram scheduled message: {}",
                                            err
                                        );
                                        continue;
                                    }
                                    Ok(true) => {} // Claimed successfully, proceed
                                }

                                let base_url = format!(
                                    "https://api.telegram.org/bot{}",
                                    ctx.channel.bot_token
                                );
                                match send_chunked_message(
                                    &self.http,
                                    &base_url,
                                    message.chat_id,
                                    &message.text,
                                    None,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        // Mark as sent AFTER successful delivery.
                                        let _ = ctx
                                            .mission_store
                                            .mark_telegram_scheduled_message_sent(
                                                message.id,
                                                &now_string(),
                                            )
                                            .await;
                                        let _ = ctx
                                            .mission_store
                                            .mark_telegram_action_execution_by_scheduled_message(
                                                message.id,
                                                TelegramActionExecutionStatus::Sent,
                                                None,
                                                &now_string(),
                                            )
                                            .await;
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            scheduled_message_id = %message.id,
                                            chat_id = message.chat_id,
                                            "Failed to deliver scheduled Telegram message: {}",
                                            err
                                        );
                                        let _ = ctx
                                            .mission_store
                                            .mark_telegram_scheduled_message_failed(
                                                message.id, &err,
                                            )
                                            .await;
                                        let _ = ctx
                                            .mission_store
                                            .mark_telegram_action_execution_by_scheduled_message(
                                                message.id,
                                                TelegramActionExecutionStatus::Failed,
                                                Some(&err),
                                                &now_string(),
                                            )
                                            .await;
                                    }
                                }
                            }
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &due_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    None,
                                )
                                .await;
                            self.record_paloma_job_success("paloma_due_messages").await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                channel_id = %ctx.channel.id,
                                "Failed to load due Telegram scheduled messages: {}",
                                err
                            );
                            self.record_paloma_job_failure("paloma_due_messages", err)
                                .await;
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &due_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    Some("failed_to_load_due_messages"),
                                )
                                .await;
                        }
                    }
                }
                let memory_job_name =
                    paloma_channel_job_name("paloma_memory_consolidation", ctx.channel.id);
                let memory_job_started = now_string();
                let memory_job_lease_expires = paloma_scheduler_lease_expires_at();
                if ctx
                    .mission_store
                    .claim_paloma_scheduler_job(
                        &memory_job_name,
                        &lease_owner,
                        &memory_job_started,
                        &memory_job_lease_expires,
                    )
                    .await
                    .unwrap_or(false)
                {
                    match ctx
                        .mission_store
                        .consolidate_telegram_structured_memory(ctx.channel.id, 2048)
                        .await
                    {
                        Ok(deleted) => {
                            tracing::debug!(
                                channel_id = %ctx.channel.id,
                                deleted,
                                "Paloma memory consolidation finished"
                            );
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &memory_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    None,
                                )
                                .await;
                            self.record_paloma_job_success("paloma_memory_consolidation")
                                .await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                channel_id = %ctx.channel.id,
                                "Paloma memory consolidation failed: {}",
                                err
                            );
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &memory_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    Some("memory_consolidation_failed"),
                                )
                                .await;
                            self.record_paloma_job_failure("paloma_memory_consolidation", err)
                                .await;
                        }
                    }
                }

                let stale_job_name =
                    paloma_channel_job_name("paloma_stale_recovery", ctx.channel.id);
                let stale_job_started = now_string();
                let stale_job_lease_expires = paloma_scheduler_lease_expires_at();
                if ctx
                    .mission_store
                    .claim_paloma_scheduler_job(
                        &stale_job_name,
                        &lease_owner,
                        &stale_job_started,
                        &stale_job_lease_expires,
                    )
                    .await
                    .unwrap_or(false)
                {
                    let before = (Utc::now() - ChronoDuration::minutes(10)).to_rfc3339();
                    match ctx
                        .mission_store
                        .recover_stale_telegram_alerts(&before, 128)
                        .await
                    {
                        Ok(recovered) => {
                            tracing::debug!(
                                channel_id = %ctx.channel.id,
                                recovered,
                                "Paloma stale recovery finished"
                            );
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &stale_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    None,
                                )
                                .await;
                            self.record_paloma_job_success("paloma_stale_recovery")
                                .await;
                        }
                        Err(err) => {
                            tracing::warn!(
                                channel_id = %ctx.channel.id,
                                "Paloma stale recovery failed: {}",
                                err
                            );
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &stale_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    Some("stale_recovery_failed"),
                                )
                                .await;
                            self.record_paloma_job_failure("paloma_stale_recovery", err)
                                .await;
                        }
                    }
                }

                let digest_job_name =
                    paloma_channel_job_name("paloma_digest_flush", ctx.channel.id);
                let digest_job_started = now_string();
                let digest_job_lease_expires = paloma_scheduler_lease_expires_at();
                if ctx
                    .mission_store
                    .claim_paloma_scheduler_job(
                        &digest_job_name,
                        &lease_owner,
                        &digest_job_started,
                        &digest_job_lease_expires,
                    )
                    .await
                    .unwrap_or(false)
                {
                    match configured_telegram_id(
                        "PALOMA_TELEGRAM_OWNER_ID",
                        DEFAULT_PALOMA_OWNER_TELEGRAM_ID,
                    ) {
                        Some(owner_id) => {
                            match flush_pending_paloma_digest(&ctx, &self.http, owner_id).await {
                                Ok(sent_count) => {
                                    tracing::debug!(
                                        channel_id = %ctx.channel.id,
                                        sent_count,
                                        "Paloma digest flush finished"
                                    );
                                    let _ = ctx
                                        .mission_store
                                        .finish_paloma_scheduler_job(
                                            &digest_job_name,
                                            &lease_owner,
                                            &now_string(),
                                            None,
                                        )
                                        .await;
                                    self.record_paloma_job_success("paloma_digest_flush").await;
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        channel_id = %ctx.channel.id,
                                        "Paloma digest flush failed: {}",
                                        err
                                    );
                                    let _ = ctx
                                        .mission_store
                                        .finish_paloma_scheduler_job(
                                            &digest_job_name,
                                            &lease_owner,
                                            &now_string(),
                                            Some("digest_flush_failed"),
                                        )
                                        .await;
                                    self.record_paloma_job_failure("paloma_digest_flush", err)
                                        .await;
                                }
                            }
                        }
                        None => {
                            let _ = ctx
                                .mission_store
                                .finish_paloma_scheduler_job(
                                    &digest_job_name,
                                    &lease_owner,
                                    &now_string(),
                                    Some("missing_owner_id"),
                                )
                                .await;
                            self.record_paloma_job_failure(
                                "paloma_digest_flush",
                                "missing_owner_id".to_string(),
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    /// Remove webhook and routing context for a channel.
    pub async fn stop_channel(&self, channel_id: Uuid) {
        if let Some(ctx) = self.active_channels.write().await.remove(&channel_id) {
            let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
            if let Err(e) = delete_webhook(&self.http, &base_url).await {
                tracing::warn!(
                    "Failed to delete Telegram webhook for channel {}: {}",
                    channel_id,
                    e
                );
            }
            tracing::info!("Stopped Telegram channel {}", channel_id);
        }
    }

    /// Stop all channels.
    pub async fn stop_all(&self) {
        let channels: Vec<_> = self.active_channels.write().await.drain().collect();
        for (id, ctx) in channels {
            let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
            let _ = delete_webhook(&self.http, &base_url).await;
            tracing::info!("Stopped Telegram channel {}", id);
        }
    }

    /// Check if a channel is active.
    pub async fn is_running(&self, channel_id: Uuid) -> bool {
        self.active_channels.read().await.contains_key(&channel_id)
    }

    /// Get the routing context for a channel (used by webhook handler).
    pub async fn get_channel_context(&self, channel_id: Uuid) -> Option<ChannelContext> {
        self.active_channels.read().await.get(&channel_id).cloned()
    }

    /// Boot all active channels from the store.
    pub async fn boot_from_store(
        self: &Arc<Self>,
        store: &Arc<dyn MissionStore>,
        cmd_tx: mpsc::Sender<ControlCommand>,
        events_tx: broadcast::Sender<AgentEvent>,
        public_base_url: &str,
    ) {
        match store.list_all_active_telegram_channels().await {
            Ok(channels) => {
                if !channels.is_empty() {
                    tracing::info!(
                        "Booting {} active Telegram channel(s) from store",
                        channels.len()
                    );
                }
                for channel in channels {
                    let ch_id = channel.id;
                    if let Err(e) = self
                        .start_channel(
                            channel,
                            cmd_tx.clone(),
                            events_tx.clone(),
                            store.clone(),
                            public_base_url,
                        )
                        .await
                    {
                        tracing::warn!("Failed to boot Telegram channel {}: {}", ch_id, e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to load Telegram channels from store: {}", e);
            }
        }
    }

    /// Get a reference to the HTTP client (for use in webhook handler).
    pub fn http(&self) -> &Client {
        &self.http
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Telegram Bot API types (minimal subset)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    #[allow(dead_code)]
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    #[allow(dead_code)]
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
    /// Caption for media messages (photos, documents, etc.)
    pub caption: Option<String>,
    pub reply_to_message: Option<Box<Message>>,
    pub entities: Option<Vec<MessageEntity>>,
    /// Entities in the caption (for media messages with @mentions in captions)
    pub caption_entities: Option<Vec<MessageEntity>>,
    /// Document attachment (PDF, ZIP, etc.)
    pub document: Option<TelegramDocument>,
    /// Photo attachment (array of sizes, last is largest)
    pub photo: Option<Vec<PhotoSize>>,
    /// Voice message
    pub voice: Option<TelegramFile>,
    /// Audio file
    pub audio: Option<TelegramFile>,
    /// Video file
    pub video: Option<TelegramFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramDocument {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub offset: i64,
    pub length: i64,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    message_id: i64,
}

#[derive(Debug, Serialize)]
struct EditMessageRequest<'a> {
    chat_id: i64,
    message_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'a str>,
}

/// Response from the Telegram `getFile` API.
#[derive(Debug, Deserialize)]
struct GetFileResponse {
    file_path: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// File download helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Download a file from Telegram by file_id and save it to a local directory.
/// Returns the local file path on success.
async fn download_telegram_file(
    http: &Client,
    bot_token: &str,
    file_id: &str,
    filename: &str,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let base_url = format!("https://api.telegram.org/bot{}", bot_token);

    // Step 1: Get file path from Telegram
    let url = format!("{}/getFile", base_url);
    let response = http
        .post(&url)
        .json(&serde_json::json!({ "file_id": file_id }))
        .send()
        .await
        .map_err(|e| format!("getFile request failed: {}", e))?;

    let body: TelegramResponse<GetFileResponse> = response
        .json()
        .await
        .map_err(|e| format!("getFile parse failed: {}", e))?;

    let tg_file_path = body
        .result
        .and_then(|r| r.file_path)
        .ok_or_else(|| "getFile returned no file_path".to_string())?;

    // Step 2: Download the file
    let download_url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot_token, tg_file_path
    );
    let download_response = http
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("File download failed: {}", e))?;
    if !download_response.status().is_success() {
        return Err(format!(
            "File download HTTP error: {}",
            download_response.status()
        ));
    }
    // Enforce a 50 MB size limit to prevent OOM from large files
    const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
    if let Some(content_length) = download_response.content_length() {
        if content_length > MAX_FILE_SIZE {
            return Err(format!(
                "File too large: {} bytes (limit: {} bytes)",
                content_length, MAX_FILE_SIZE
            ));
        }
    }
    let file_bytes = download_response
        .bytes()
        .await
        .map_err(|e| format!("File read failed: {}", e))?;
    if file_bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(format!(
            "File too large: {} bytes (limit: {} bytes)",
            file_bytes.len(),
            MAX_FILE_SIZE
        ));
    }

    // Step 3: Save to destination
    tokio::fs::create_dir_all(dest_dir)
        .await
        .map_err(|e| format!("Failed to create upload dir: {}", e))?;

    let safe_name = filename.replace(['/', '\\', '\0'], "_");
    let dest_path = dest_dir.join(&safe_name);
    tokio::fs::write(&dest_path, &file_bytes)
        .await
        .map_err(|e| format!("Failed to write file: {}", e))?;

    tracing::info!(
        "Downloaded Telegram file {} ({} bytes) to {}",
        safe_name,
        file_bytes.len(),
        dest_path.display()
    );

    Ok(dest_path)
}

/// Extract file info from a Telegram message. Returns (file_id, filename, mime_type).
fn extract_file_info(msg: &Message) -> Option<(String, String, String)> {
    if let Some(ref doc) = msg.document {
        let name = doc.file_name.clone().unwrap_or_else(|| {
            format!(
                "document_{}",
                doc.file_id.chars().take(8).collect::<String>()
            )
        });
        let mime = doc
            .mime_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        return Some((doc.file_id.clone(), name, mime));
    }
    if let Some(ref photos) = msg.photo {
        if let Some(largest) = photos.last() {
            let name = format!(
                "photo_{}.jpg",
                largest.file_id.chars().take(8).collect::<String>()
            );
            return Some((largest.file_id.clone(), name, "image/jpeg".to_string()));
        }
    }
    if let Some(ref voice) = msg.voice {
        let name = voice
            .file_name
            .clone()
            .unwrap_or_else(|| "voice_message.ogg".to_string());
        let mime = voice
            .mime_type
            .clone()
            .unwrap_or_else(|| "audio/ogg".to_string());
        return Some((voice.file_id.clone(), name, mime));
    }
    if let Some(ref audio) = msg.audio {
        let name = audio
            .file_name
            .clone()
            .unwrap_or_else(|| "audio.mp3".to_string());
        let mime = audio
            .mime_type
            .clone()
            .unwrap_or_else(|| "audio/mpeg".to_string());
        return Some((audio.file_id.clone(), name, mime));
    }
    if let Some(ref video) = msg.video {
        let name = video
            .file_name
            .clone()
            .unwrap_or_else(|| "video.mp4".to_string());
        let mime = video
            .mime_type
            .clone()
            .unwrap_or_else(|| "video/mp4".to_string());
        return Some((video.file_id.clone(), name, mime));
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Webhook management
// ─────────────────────────────────────────────────────────────────────────────

/// Register a webhook URL with Telegram.
async fn set_webhook(
    http: &Client,
    base_url: &str,
    webhook_url: &str,
    secret_token: Option<&str>,
) -> Result<(), String> {
    let url = format!("{}/setWebhook", base_url);
    let mut params = serde_json::json!({
        "url": webhook_url,
        "allowed_updates": ["message"],
        // Keep pending updates when re-registering webhooks so we don't drop real user messages.
        "drop_pending_updates": false,
    });
    if let Some(secret) = secret_token {
        params["secret_token"] = serde_json::Value::String(secret.to_string());
    }
    let response = http
        .post(&url)
        .json(&params)
        .send()
        .await
        .map_err(|e| format!("setWebhook request failed: {}", e))?;

    let body: TelegramResponse<bool> = response
        .json()
        .await
        .map_err(|e| format!("setWebhook parse failed: {}", e))?;

    if body.ok {
        Ok(())
    } else {
        Err(format!(
            "setWebhook API error: {}",
            body.description.unwrap_or_default()
        ))
    }
}

/// Remove the webhook for a bot.
async fn delete_webhook(http: &Client, base_url: &str) -> Result<(), String> {
    let url = format!("{}/deleteWebhook", base_url);
    let response = http
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("deleteWebhook request failed: {}", e))?;

    let body: TelegramResponse<bool> = response
        .json()
        .await
        .map_err(|e| format!("deleteWebhook parse failed: {}", e))?;

    if body.ok {
        Ok(())
    } else {
        Err(format!(
            "deleteWebhook API error: {}",
            body.description.unwrap_or_default()
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Message processing (used by webhook handler)
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve or auto-create a mission for a Telegram chat.
async fn resolve_or_create_mission(
    ctx: &ChannelContext,
    chat_id: i64,
    sender_name: &str,
    chat_title: Option<&str>,
    bridge: &SharedTelegramBridge,
) -> Option<Uuid> {
    let chat_lock = bridge.chat_lock(ctx.channel.id, chat_id).await;
    let _guard = chat_lock.lock().await;

    // 1. Look up existing mapping
    if let Ok(Some(mapping)) = ctx
        .mission_store
        .get_telegram_chat_mission(ctx.channel.id, chat_id)
        .await
    {
        return Some(mapping.mission_id);
    }

    // 2. Create a new mission via ControlCommand
    let (tx, rx) = tokio::sync::oneshot::channel();
    let title = Some(format!("Telegram: {}", sender_name));

    // Normalize agent name: strip parenthetical suffixes like "(Ultraworker)"
    // and lowercase to get the config key (e.g. "Sisyphus (Ultraworker)" → "sisyphus")
    let agent = ctx.channel.default_agent.as_ref().map(|a| {
        let name = if let Some(idx) = a.find('(') {
            a[..idx].trim()
        } else {
            a.trim()
        };
        name.to_lowercase()
    });

    let _ = ctx
        .cmd_tx
        .send(ControlCommand::CreateMission {
            title,
            workspace_id: ctx.channel.default_workspace_id,
            agent,
            model_override: ctx.channel.default_model_override.clone(),
            model_effort: ctx.channel.default_model_effort.clone(),
            backend: ctx.channel.default_backend.clone(),
            config_profile: ctx.channel.default_config_profile.clone(),
            parent_mission_id: None,
            working_directory: None,
            respond: tx,
        })
        .await;

    match rx.await {
        Ok(Ok(mission)) => {
            let mission_id = mission.id;

            // Set to assistant mode
            let _ = ctx
                .mission_store
                .update_mission_mode(mission_id, MissionMode::Assistant)
                .await;

            // Store the mapping
            let mapping = TelegramChatMission {
                id: Uuid::new_v4(),
                channel_id: ctx.channel.id,
                chat_id,
                mission_id,
                chat_title: chat_title.map(|title| title.to_string()),
                created_at: crate::api::mission_store::now_string(),
            };
            // Handle race condition: if another message already created the mapping, look it up
            match ctx
                .mission_store
                .create_telegram_chat_mission(mapping)
                .await
            {
                Ok(_) => {}
                Err(e) if e.contains("UNIQUE constraint") => {
                    // Another concurrent message already created the mapping
                    if let Ok(Some(existing)) = ctx
                        .mission_store
                        .get_telegram_chat_mission(ctx.channel.id, chat_id)
                        .await
                    {
                        return Some(existing.mission_id);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to store chat-mission mapping: {}", e);
                    return None;
                }
            }

            tracing::info!(
                "Auto-created mission {} for Telegram chat {} on channel {}",
                mission_id,
                chat_id,
                ctx.channel.id
            );

            Some(mission_id)
        }
        _ => {
            tracing::error!(
                "Failed to create mission for chat {} on channel {}",
                chat_id,
                ctx.channel.id
            );
            None
        }
    }
}

fn normalize_optional_telegram_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn telegram_chat_display_title(chat: &Chat) -> Option<String> {
    if let Some(title) = normalize_optional_telegram_value(chat.title.as_deref()) {
        return Some(title);
    }

    normalize_optional_telegram_value(chat.username.as_deref()).map(|username| {
        if username.starts_with('@') {
            username
        } else {
            format!("@{}", username)
        }
    })
}

async fn remember_telegram_chat_title(ctx: &ChannelContext, chat: &Chat) {
    let Some(chat_title) = telegram_chat_display_title(chat) else {
        return;
    };

    if let Err(error) = ctx
        .mission_store
        .update_telegram_chat_mission_title(ctx.channel.id, chat.id, Some(chat_title.clone()))
        .await
    {
        tracing::debug!(
            "Failed to update Telegram chat title cache for channel {} chat {}: {}",
            ctx.channel.id,
            chat.id,
            error
        );
    }
}

async fn upsert_telegram_conversation(
    ctx: &ChannelContext,
    chat: &Chat,
    mission_id: Uuid,
    last_message_at: Option<String>,
) -> Option<TelegramConversation> {
    let now = now_string();
    let existing = ctx
        .mission_store
        .get_telegram_conversation_by_chat(ctx.channel.id, chat.id)
        .await
        .ok()
        .flatten();
    let conversation = TelegramConversation {
        id: existing
            .as_ref()
            .map(|item| item.id)
            .unwrap_or_else(Uuid::new_v4),
        channel_id: ctx.channel.id,
        chat_id: chat.id,
        mission_id: Some(mission_id),
        chat_title: telegram_chat_display_title(chat),
        chat_type: Some(chat.chat_type.clone()),
        last_message_at,
        created_at: existing
            .as_ref()
            .map(|item| item.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    ctx.mission_store
        .upsert_telegram_conversation(conversation)
        .await
        .ok()
}

#[allow(clippy::too_many_arguments)]
async fn log_telegram_conversation_message(
    mission_store: &Arc<dyn MissionStore>,
    conversation_id: Uuid,
    channel_id: Uuid,
    chat_id: i64,
    mission_id: Option<Uuid>,
    workflow_id: Option<Uuid>,
    telegram_message_id: Option<i64>,
    direction: TelegramConversationMessageDirection,
    role: &str,
    sender_user_id: Option<i64>,
    sender_username: Option<String>,
    sender_display_name: Option<String>,
    reply_to_message_id: Option<i64>,
    text: &str,
) {
    let message = TelegramConversationMessage {
        id: Uuid::new_v4(),
        conversation_id,
        channel_id,
        chat_id,
        mission_id,
        workflow_id,
        telegram_message_id,
        direction,
        role: role.to_string(),
        sender_user_id,
        sender_username,
        sender_display_name,
        reply_to_message_id,
        text: text.to_string(),
        created_at: now_string(),
    };
    if let Err(error) = mission_store
        .create_telegram_conversation_message(message)
        .await
    {
        tracing::debug!(
            channel_id = %channel_id,
            chat_id,
            "Failed to append Telegram conversation message: {}",
            error
        );
    }
}

fn telegram_memory_subject(msg: &Message, sender_name: &str) -> TelegramMemorySubject {
    let Some(from) = msg.from.as_ref() else {
        return TelegramMemorySubject::default();
    };

    TelegramMemorySubject {
        user_id: Some(from.id),
        username: normalize_optional_telegram_value(from.username.as_deref()),
        display_name: Some(sender_name.to_string()).filter(|value| !value.trim().is_empty()),
    }
}

fn scope_for_extracted_memory(
    entry: &ExtractedTelegramMemory,
    subject: &TelegramMemorySubject,
) -> TelegramStructuredMemoryScope {
    if subject.user_id.is_some()
        && matches!(
            entry.kind,
            TelegramStructuredMemoryKind::Fact | TelegramStructuredMemoryKind::Preference
        )
    {
        TelegramStructuredMemoryScope::User
    } else {
        TelegramStructuredMemoryScope::Chat
    }
}

fn normalize_memory_text(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '.' | '!' | '?' | '"' | '\'' | ' '))
        .to_string()
}

fn strip_memory_follow_up_directives(value: &str) -> String {
    let re = regex::Regex::new(
        r"(?is)^(?P<body>.*?)(?:[.!?\n]+\s*(?:réponds?|reponds?|reply|respond|answer|say)\b.*)?$",
    )
    .expect("telegram memory directive stripping regex must compile");

    re.captures(value.trim())
        .and_then(|captures| captures.name("body").map(|m| m.as_str().trim().to_string()))
        .unwrap_or_else(|| value.trim().to_string())
}

fn extract_fact_memory(clause: &str) -> Option<ExtractedTelegramMemory> {
    let clause = strip_memory_follow_up_directives(clause);
    let re = regex::Regex::new(
        r"(?i)^(?:(?:mon|ma|mes|my)\s+)?(?P<label>.+?)\s+(?:est|is)\s+(?P<value>.+)$",
    )
    .expect("telegram fact extraction regex must compile");
    let captures = re.captures(clause.trim())?;
    let label = normalize_memory_text(captures.name("label")?.as_str());
    let value = normalize_memory_text(captures.name("value")?.as_str());
    if label.is_empty() || value.is_empty() {
        return None;
    }
    Some(ExtractedTelegramMemory {
        kind: TelegramStructuredMemoryKind::Fact,
        label: Some(label),
        value,
    })
}

fn extract_structured_memory_from_text(text: &str) -> Vec<ExtractedTelegramMemory> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let remember_re = regex::Regex::new(
        r"(?i)^(?:souviens[- ]toi que|remember that|please remember that|note that)\s+(.+)$",
    )
    .expect("telegram remember extraction regex must compile");

    if let Some(captures) = remember_re.captures(trimmed) {
        let body = normalize_memory_text(&strip_memory_follow_up_directives(
            captures.get(1).map(|m| m.as_str()).unwrap_or(""),
        ));
        if let Some(fact) = extract_fact_memory(&body) {
            entries.push(fact);
        } else if !body.is_empty() {
            entries.push(ExtractedTelegramMemory {
                kind: TelegramStructuredMemoryKind::Note,
                label: None,
                value: body,
            });
        }
        return entries;
    }

    if trimmed.ends_with('?') {
        return entries;
    }

    if let Some(fact) = extract_fact_memory(trimmed) {
        entries.push(fact);
    }

    let preference_re = regex::Regex::new(r"(?i)^(?:j'aime|i like|i prefer)\s+(.+)$")
        .expect("telegram preference extraction regex must compile");
    if let Some(captures) = preference_re.captures(trimmed) {
        let value = normalize_memory_text(captures.get(1).map(|m| m.as_str()).unwrap_or(""));
        if !value.is_empty() {
            entries.push(ExtractedTelegramMemory {
                kind: TelegramStructuredMemoryKind::Preference,
                label: None,
                value,
            });
        }
    }

    let task_re = regex::Regex::new(r"(?i)^(?:remind me to|rappelle[- ]moi de)\s+(.+)$")
        .expect("telegram task extraction regex must compile");
    if let Some(captures) = task_re.captures(trimmed) {
        let value = normalize_memory_text(&strip_memory_follow_up_directives(
            captures.get(1).map(|m| m.as_str()).unwrap_or(""),
        ));
        if !value.is_empty() {
            entries.push(ExtractedTelegramMemory {
                kind: TelegramStructuredMemoryKind::Task,
                label: None,
                value,
            });
        }
    }

    entries
}

async fn persist_structured_memory_for_message(
    ctx: &ChannelContext,
    chat_id: i64,
    mission_id: Uuid,
    source_message_id: i64,
    subject: &TelegramMemorySubject,
    clean_text: &str,
) {
    let entries = extract_structured_memory_from_text(clean_text);
    for entry in entries {
        let now = now_string();
        let scope = scope_for_extracted_memory(&entry, subject);
        let stored = TelegramStructuredMemoryEntry {
            id: Uuid::new_v4(),
            channel_id: ctx.channel.id,
            chat_id,
            mission_id: Some(mission_id),
            scope: scope.clone(),
            kind: entry.kind,
            label: entry.label,
            value: entry.value,
            subject_user_id: match scope {
                TelegramStructuredMemoryScope::User => subject.user_id,
                _ => None,
            },
            subject_username: match scope {
                TelegramStructuredMemoryScope::User => subject.username.clone(),
                _ => None,
            },
            subject_display_name: match scope {
                TelegramStructuredMemoryScope::User => subject.display_name.clone(),
                _ => None,
            },
            source_message_id: Some(source_message_id),
            source_role: "user".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        if let Err(error) = ctx
            .mission_store
            .upsert_telegram_structured_memory(stored)
            .await
        {
            tracing::warn!(
                "Failed to persist structured Telegram memory for chat {}: {}",
                chat_id,
                error
            );
        }
    }
}

fn format_memory_entry(entry: &TelegramStructuredMemoryEntry) -> String {
    match entry.kind {
        TelegramStructuredMemoryKind::Fact => {
            if let Some(label) = entry.label.as_deref() {
                format!("- Fact: {} = {}", label, entry.value)
            } else {
                format!("- Fact: {}", entry.value)
            }
        }
        TelegramStructuredMemoryKind::Preference => {
            format!("- Preference: {}", entry.value)
        }
        TelegramStructuredMemoryKind::Task => {
            format!("- Task: {}", entry.value)
        }
        TelegramStructuredMemoryKind::Note => {
            format!("- Note: {}", entry.value)
        }
    }
}

fn format_structured_memory_context(entries: &[TelegramStructuredMemoryEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut chat_lines = Vec::new();
    let mut user_lines = Vec::new();
    let mut channel_lines = Vec::new();

    for entry in entries {
        let formatted = format_memory_entry(entry);
        match entry.scope {
            TelegramStructuredMemoryScope::Chat => chat_lines.push(formatted),
            TelegramStructuredMemoryScope::User => user_lines.push(formatted),
            TelegramStructuredMemoryScope::Channel => channel_lines.push(formatted),
        }
    }

    let mut sections = Vec::new();
    if !user_lines.is_empty() {
        sections.push(format!("User memory:\n{}", user_lines.join("\n")));
    }
    if !chat_lines.is_empty() {
        sections.push(format!("Chat memory:\n{}", chat_lines.join("\n")));
    }
    if !channel_lines.is_empty() {
        sections.push(format!("Channel memory:\n{}", channel_lines.join("\n")));
    }

    Some(format!("[Structured memory]\n{}", sections.join("\n")))
}

async fn load_structured_memory_context(
    ctx: &ChannelContext,
    chat_id: i64,
    subject_user_id: Option<i64>,
    query: &str,
) -> Option<String> {
    let trimmed_query = query.trim();
    let mut entries = if trimmed_query.len() >= 4 {
        ctx.mission_store
            .search_telegram_memory_context_hybrid(
                ctx.channel.id,
                chat_id,
                subject_user_id,
                trimmed_query,
                5,
            )
            .await
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|hit| hit.entry)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if entries.len() < 3 {
        let recent = ctx
            .mission_store
            .list_telegram_memory_context(ctx.channel.id, chat_id, subject_user_id, 6)
            .await
            .ok()
            .unwrap_or_default();
        for entry in recent {
            if !entries.iter().any(|existing| existing.id == entry.id) {
                entries.push(entry);
            }
        }
    }

    format_structured_memory_context(&entries)
}

/// Process an incoming Telegram message from a webhook.
/// Called by the axum route handler.
pub async fn process_webhook_message(
    ctx: &ChannelContext,
    msg: &Message,
    http: &Client,
    bridge: &SharedTelegramBridge,
) {
    // Accept text, caption (on media), or file-only messages
    let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");

    let has_file = extract_file_info(msg).is_some();
    if text.is_empty() && !has_file {
        return;
    }

    let Some(paloma_queue_key) = bridge
        .enqueue_paloma_session(ctx.channel.id, msg.chat.id, None)
        .await
    else {
        let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
        let _ = send_message(
            http,
            &base_url,
            msg.chat.id,
            "I'm still handling earlier messages in this chat. Please retry in a moment.",
            Some(msg.message_id),
        )
        .await;
        return;
    };

    let paloma_session_lock = bridge
        .paloma_session_lock(ctx.channel.id, msg.chat.id)
        .await;
    let _paloma_session_guard = paloma_session_lock.lock().await;
    bridge.dequeue_paloma_session(&paloma_queue_key).await;

    let should_respond = should_process_message(&ctx.channel, msg, &ctx.bot_username);

    let sender_name = msg
        .from
        .as_ref()
        .map(|u| {
            if let Some(ref un) = u.username {
                format!("@{}", un)
            } else if let Some(ref last) = u.last_name {
                format!("{} {}", u.first_name, last)
            } else {
                u.first_name.clone()
            }
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let chat_title = telegram_chat_display_title(&msg.chat);

    remember_telegram_chat_title(ctx, &msg.chat).await;

    let clean_text = strip_bot_mention(text, &ctx.bot_username);
    let paloma_user = remember_paloma_user(ctx, msg).await;
    if handle_paloma_command(ctx, msg, http, paloma_user.as_ref(), &clean_text).await {
        return;
    }
    let memory_subject = telegram_memory_subject(msg, &sender_name);

    // Resolve target mission: auto-create per chat or legacy single-mission
    // For context-only messages (should_respond=false), only look up existing
    // missions — don't create new ones just to store context.
    let target_mission_id = if ctx.channel.auto_create_missions {
        if should_respond {
            match resolve_or_create_mission(
                ctx,
                msg.chat.id,
                &sender_name,
                chat_title.as_deref(),
                bridge,
            )
            .await
            {
                Some(id) => id,
                None => {
                    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
                    let _ = send_message(
                        http,
                        &base_url,
                        msg.chat.id,
                        "Sorry, I couldn't start a new conversation. Please try again.",
                        Some(msg.message_id),
                    )
                    .await;
                    return;
                }
            }
        } else {
            // Context-only: look up existing mission for this chat, skip if none
            match ctx
                .mission_store
                .get_telegram_chat_mission(ctx.channel.id, msg.chat.id)
                .await
            {
                Ok(Some(mapping)) => mapping.mission_id,
                _ => return, // No existing mission for this chat — nothing to store context in
            }
        }
    } else {
        ctx.channel.mission_id
    };

    let conversation =
        upsert_telegram_conversation(ctx, &msg.chat, target_mission_id, Some(now_string())).await;

    persist_structured_memory_for_message(
        ctx,
        msg.chat.id,
        target_mission_id,
        msg.message_id,
        &memory_subject,
        &clean_text,
    )
    .await;

    // Download attached file if present
    let file_annotation = if let Some((file_id, filename, mime)) = extract_file_info(msg) {
        let upload_dir =
            std::path::PathBuf::from("/tmp/telegram-uploads").join(target_mission_id.to_string());
        match download_telegram_file(
            http,
            &ctx.channel.bot_token,
            &file_id,
            &filename,
            &upload_dir,
        )
        .await
        {
            Ok(local_path) => Some(format!(
                "[Attached file: {} ({}), saved to: {}]",
                filename,
                mime,
                local_path.display()
            )),
            Err(e) => {
                tracing::warn!("Failed to download Telegram file: {}", e);
                Some(format!(
                    "[Attached file: {} ({}) — download failed: {}]",
                    filename, mime, e
                ))
            }
        }
    } else {
        None
    };

    if let Some(conversation) = conversation.as_ref() {
        let conversation_text = workflow_reply_text(&clean_text, file_annotation.as_deref());
        log_telegram_conversation_message(
            &ctx.mission_store,
            conversation.id,
            ctx.channel.id,
            msg.chat.id,
            Some(target_mission_id),
            None,
            Some(msg.message_id),
            TelegramConversationMessageDirection::Inbound,
            "user",
            memory_subject.user_id,
            memory_subject.username.clone(),
            memory_subject.display_name.clone(),
            msg.reply_to_message.as_ref().map(|reply| reply.message_id),
            &conversation_text,
        )
        .await;
    }

    // Build message content with optional system instructions and file info
    let mut parts = Vec::new();
    parts.push(format!(
        "[Telegram from {} in chat {}]",
        sender_name, msg.chat.id
    ));
    if let Some(ref instructions) = ctx.channel.instructions {
        parts.push(format!("[Instructions: {}]", instructions));
    }
    if let Some(memory_context) =
        load_structured_memory_context(ctx, msg.chat.id, memory_subject.user_id, &clean_text).await
    {
        parts.push(memory_context);
    }
    if let Some(ref file_info) = file_annotation {
        parts.push(file_info.clone());
    }
    if !clean_text.is_empty() {
        parts.push(clean_text.clone());
    }
    let content = parts.join(" ");

    let reply_to_message_id = msg.reply_to_message.as_ref().map(|reply| reply.message_id);
    let mut matched_workflow = match reply_to_message_id {
        Some(reply_to_message_id) => ctx
            .mission_store
            .get_pending_telegram_workflow_for_target_message(
                ctx.channel.id,
                msg.chat.id,
                reply_to_message_id,
            )
            .await
            .ok()
            .flatten(),
        None => None,
    };
    if matched_workflow.is_none() {
        matched_workflow = ctx
            .mission_store
            .get_pending_telegram_workflow_for_target_chat(ctx.channel.id, msg.chat.id)
            .await
            .ok()
            .flatten()
            .filter(|workflow| !workflow_requires_direct_reply(workflow));
    }

    if let Some(mut workflow) = matched_workflow {
        let workflow_reply = workflow_reply_text(&clean_text, file_annotation.as_deref());
        let conversation_id = conversation.as_ref().map(|item| item.id);
        let _ = ctx
            .mission_store
            .create_telegram_workflow_event(TelegramWorkflowEvent {
                id: Uuid::new_v4(),
                workflow_id: workflow.id,
                conversation_id,
                event_type: "external_reply_received".to_string(),
                payload_json: serde_json::json!({
                    "sender_name": sender_name,
                    "chat_id": msg.chat.id,
                    "message_id": msg.message_id,
                    "reply_to_message_id": reply_to_message_id,
                    "text": workflow_reply,
                })
                .to_string(),
                created_at: now_string(),
            })
            .await;

        workflow.target_conversation_id = conversation_id;
        workflow.latest_reply_text = Some(workflow_reply.clone());
        // Only mark Completed if there is no origin relay needed;
        // otherwise the spawned relay task will set RelayedToOrigin or Failed.
        if workflow.origin_mission_id.is_none() {
            workflow.status = TelegramWorkflowStatus::Completed;
        }
        workflow.updated_at = now_string();
        workflow.completed_at = Some(workflow.updated_at.clone());
        let _ = ctx
            .mission_store
            .update_telegram_workflow(workflow.clone())
            .await;

        relay_workflow_reply_to_origin(ctx, bridge, &workflow, &sender_name, &workflow_reply).await;
        return;
    }

    if !should_respond {
        // Context-only: store the message in mission history without triggering
        // the agent. This lets the agent see full chat context when it IS triggered.
        tracing::debug!(
            "Storing Telegram context message for mission {} from {}: {}",
            target_mission_id,
            sender_name,
            &clean_text[..clean_text.floor_char_boundary(100)]
        );
        let _ = ctx
            .mission_store
            .log_event(
                target_mission_id,
                &AgentEvent::UserMessage {
                    id: Uuid::new_v4(),
                    content: content.clone(),
                    queued: false,
                    mission_id: Some(target_mission_id),
                },
            )
            .await;
        return;
    }

    tracing::info!(
        "Telegram webhook message for mission {} from {}: {}",
        target_mission_id,
        sender_name,
        &clean_text[..clean_text.floor_char_boundary(100)]
    );

    // Subscribe to events BEFORE sending the command to avoid race conditions
    // where the response arrives before the subscription is active.
    let events_rx = ctx.events_tx.subscribe();

    // Send to mission
    let msg_id = Uuid::new_v4();
    let (queued_tx, _queued_rx) = tokio::sync::oneshot::channel();
    let _ = ctx
        .cmd_tx
        .send(ControlCommand::UserMessage {
            id: msg_id,
            content,
            agent: None,
            target_mission_id: Some(target_mission_id),
            respond: queued_tx,
        })
        .await;

    // Spawn a task to stream the response back to Telegram
    let http_clone = http.clone();
    let bot_token = ctx.channel.bot_token.clone();
    let chat_id = msg.chat.id;
    let reply_to = msg.message_id;
    let mission_id = target_mission_id;
    let bridge_clone = Arc::clone(bridge);
    let channel_id = ctx.channel.id;
    let mission_store = Arc::clone(&ctx.mission_store);

    tokio::spawn(async move {
        if let Err(e) = stream_response(
            events_rx,
            &http_clone,
            &bot_token,
            chat_id,
            reply_to,
            Some(msg_id),
            mission_id,
            Some(bridge_clone),
            Some(channel_id),
            Some(mission_store),
        )
        .await
        {
            tracing::warn!(
                "Failed to stream Telegram reply for mission {}: {}",
                mission_id,
                e
            );
        }
    });
}

/// Check if a message should be processed based on trigger mode and allowed chats.
fn should_process_message(channel: &TelegramChannel, msg: &Message, bot_username: &str) -> bool {
    // Check allowed chat IDs
    if !channel.allowed_chat_ids.is_empty() && !channel.allowed_chat_ids.contains(&msg.chat.id) {
        return false;
    }

    let is_private = msg.chat.chat_type == "private";

    // Check for @bot_username mentions in both text entities and caption entities
    let mention_target = format!("@{}", bot_username);
    let has_mention_in = |entities: &Option<Vec<MessageEntity>>, text: &Option<String>| -> bool {
        entities
            .as_ref()
            .map(|ents| {
                ents.iter().any(|e| {
                    if e.entity_type == "mention" {
                        if let Some(ref t) = text {
                            let utf16_units: Vec<u16> = t.encode_utf16().collect();
                            let start = e.offset as usize;
                            let end = (e.offset + e.length) as usize;
                            if end <= utf16_units.len() {
                                if let Ok(mention) = String::from_utf16(&utf16_units[start..end]) {
                                    return mention.eq_ignore_ascii_case(&mention_target);
                                }
                            }
                            false
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                })
            })
            .unwrap_or(false)
    };
    let is_mention = has_mention_in(&msg.entities, &msg.text)
        || has_mention_in(&msg.caption_entities, &msg.caption);
    let is_reply = msg
        .reply_to_message
        .as_ref()
        .and_then(|r| r.from.as_ref())
        .map(|u| {
            u.username
                .as_ref()
                .map(|un| un.eq_ignore_ascii_case(bot_username))
                .unwrap_or(false)
        })
        .unwrap_or(false);

    match channel.trigger_mode {
        TelegramTriggerMode::MentionOrDm => is_private || is_mention || is_reply,
        TelegramTriggerMode::BotMention => is_mention,
        TelegramTriggerMode::Reply => is_reply,
        TelegramTriggerMode::DirectMessage => is_private,
        TelegramTriggerMode::Always => true,
    }
}

/// Strip @bot_username from the beginning of a message (case-insensitive).
fn strip_bot_mention(text: &str, bot_username: &str) -> String {
    let mention = format!("@{}", bot_username);
    let trimmed = text.trim();
    // Use char-aware comparison to avoid panics on non-ASCII usernames
    if let Some(rest) = trimmed.get(..mention.len()) {
        if rest.eq_ignore_ascii_case(&mention) {
            return trimmed[mention.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn parse_telegram_action_attrs(raw: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let attr_re = regex::Regex::new(r#"([a-z_]+)\s*=\s*"([^"]*)""#)
        .expect("telegram action attr regex must compile");
    for caps in attr_re.captures_iter(raw) {
        if let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) {
            attrs.insert(key.as_str().to_string(), value.as_str().to_string());
        }
    }
    attrs
}

fn extract_telegram_actions(content: &str) -> (Vec<TelegramAction>, String) {
    let mut actions = Vec::new();
    let send_patterns = [
        regex::Regex::new(r#"(?s)<telegram-send(?P<attrs>[^>]*)>(?P<text>.*?)</telegram-send>"#)
            .expect("telegram send regex must compile"),
        regex::Regex::new(
            r#"(?s)\[telegram-send(?P<attrs>[^\]]*)\](?P<text>.*?)\[/telegram-send\]"#,
        )
        .expect("telegram send bracket regex must compile"),
    ];
    let reminder_patterns = [
        regex::Regex::new(
            r#"(?s)<telegram-reminder(?P<attrs>[^>]*)>(?P<text>.*?)</telegram-reminder>"#,
        )
        .expect("telegram reminder regex must compile"),
        regex::Regex::new(
            r#"(?s)\[telegram-reminder(?P<attrs>[^\]]*)\](?P<text>.*?)\[/telegram-reminder\]"#,
        )
        .expect("telegram reminder bracket regex must compile"),
    ];

    for send_re in &send_patterns {
        for caps in send_re.captures_iter(content) {
            let attrs =
                parse_telegram_action_attrs(caps.name("attrs").map(|m| m.as_str()).unwrap_or(""));
            let text = caps
                .name("text")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            if !text.is_empty() {
                actions.push(TelegramAction {
                    kind: TelegramActionKind::Send,
                    target: attrs
                        .get("target")
                        .cloned()
                        .unwrap_or_else(|| "current".to_string()),
                    delay_seconds: attrs
                        .get("delay_seconds")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0),
                    text,
                });
            }
        }
    }

    for reminder_re in &reminder_patterns {
        for caps in reminder_re.captures_iter(content) {
            let attrs =
                parse_telegram_action_attrs(caps.name("attrs").map(|m| m.as_str()).unwrap_or(""));
            let text = caps
                .name("text")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            if !text.is_empty() {
                actions.push(TelegramAction {
                    kind: TelegramActionKind::Reminder,
                    target: attrs
                        .get("target")
                        .cloned()
                        .unwrap_or_else(|| "current".to_string()),
                    delay_seconds: attrs
                        .get("delay_seconds")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(60),
                    text,
                });
            }
        }
    }

    let without_send = send_patterns.iter().fold(content.to_string(), |acc, re| {
        re.replace_all(&acc, "").into_owned()
    });
    let visible = reminder_patterns
        .iter()
        .fold(without_send, |acc, re| {
            re.replace_all(&acc, "").into_owned()
        })
        .trim()
        .to_string();
    (actions, visible)
}

fn strip_leading_telegram_meta_block<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix(prefix)?;
    let closing = rest.find(']')?;
    Some(rest[closing + 1..].trim_start())
}

fn sanitize_telegram_visible_text(text: &str) -> String {
    let mut current = text.trim().to_string();

    loop {
        if let Some(rest) = strip_leading_telegram_meta_block(&current, "[Telegram from ") {
            current = rest.to_string();
            continue;
        }
        if let Some(rest) = strip_leading_telegram_meta_block(&current, "[Instructions:") {
            current = rest.to_string();
            continue;
        }
        break;
    }

    current
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramActionChatLookup {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
}

fn telegram_action_target_matches(
    target: &str,
    title: Option<&str>,
    username: Option<&str>,
) -> bool {
    let target = target.trim();
    let wanted = target
        .strip_prefix("title:")
        .or_else(|| target.strip_prefix("username:"))
        .unwrap_or(target)
        .trim()
        .trim_start_matches('@')
        .to_lowercase();

    if wanted.is_empty() {
        return false;
    }

    let title_match = normalize_optional_telegram_value(title)
        .map(|value| value.trim_start_matches('@').to_lowercase() == wanted)
        .unwrap_or(false);
    let username_match = normalize_optional_telegram_value(username)
        .map(|value| value.trim_start_matches('@').to_lowercase() == wanted)
        .unwrap_or(false);

    if target.starts_with("title:") {
        return title_match;
    }
    if target.starts_with("username:") || target.starts_with('@') {
        return username_match;
    }

    title_match || username_match
}

async fn fetch_telegram_chat_lookup(
    http: &Client,
    base_url: &str,
    chat_id: i64,
) -> Result<TelegramActionChatLookup, String> {
    let url = format!("{}/getChat", base_url);
    let response = http
        .post(&url)
        .json(&serde_json::json!({ "chat_id": chat_id }))
        .send()
        .await
        .map_err(|e| format!("getChat failed for {}: {}", chat_id, e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("getChat API error {}: {}", status, body_text));
    }

    let parsed: TelegramResponse<TelegramActionChatLookup> = response
        .json()
        .await
        .map_err(|e| format!("getChat parse failed for {}: {}", chat_id, e))?;

    parsed
        .result
        .ok_or_else(|| format!("getChat returned no result for {}", chat_id))
}

fn telegram_action_lookup_title(lookup: &TelegramActionChatLookup) -> Option<String> {
    normalize_optional_telegram_value(lookup.title.as_deref())
        .or_else(|| {
            normalize_optional_telegram_value(lookup.username.as_deref())
                .map(|u| format!("@{}", u.trim_start_matches('@')))
        })
        .or_else(|| {
            let first = normalize_optional_telegram_value(lookup.first_name.as_deref());
            let last = normalize_optional_telegram_value(lookup.last_name.as_deref());
            match (first, last) {
                (Some(first), Some(last)) => Some(format!("{} {}", first, last)),
                (Some(first), None) => Some(first),
                _ => None,
            }
        })
}

fn merge_telegram_chat_metadata(
    cached_title: Option<String>,
    stored_title: Option<String>,
    stored_type: Option<String>,
    fetched_title: Option<String>,
    fetched_type: Option<String>,
) -> (Option<String>, Option<String>) {
    (
        fetched_title.or(stored_title).or(cached_title),
        stored_type.or(fetched_type),
    )
}

async fn resolve_telegram_chat_metadata(
    ctx: &ChannelContext,
    http: &Client,
    base_url: &str,
    chat_id: i64,
    cached_title: Option<String>,
) -> Result<(Option<String>, Option<String>), String> {
    let conversation = ctx
        .mission_store
        .get_telegram_conversation_by_chat(ctx.channel.id, chat_id)
        .await
        .ok()
        .flatten();
    let stored_title = conversation
        .as_ref()
        .and_then(|item| item.chat_title.clone());
    let stored_type = conversation
        .as_ref()
        .and_then(|item| item.chat_type.clone());

    if stored_type.is_some() {
        return Ok(merge_telegram_chat_metadata(
            cached_title,
            stored_title,
            stored_type,
            None,
            None,
        ));
    }

    match fetch_telegram_chat_lookup(http, base_url, chat_id).await {
        Ok(lookup) => Ok(merge_telegram_chat_metadata(
            cached_title,
            stored_title,
            None,
            telegram_action_lookup_title(&lookup),
            Some(lookup.chat_type),
        )),
        Err(error) => {
            let fallback =
                merge_telegram_chat_metadata(cached_title, stored_title, None, None, None);
            if fallback.0.is_some() {
                tracing::debug!(
                    "Failed to backfill Telegram chat {} metadata on channel {}: {}",
                    chat_id,
                    ctx.channel.id,
                    error
                );
                Ok(fallback)
            } else {
                Err(error)
            }
        }
    }
}

/// Result of resolving a Telegram action target.
/// Fields: (chat_id, chat_title, chat_type, mention_username).
/// `mention_username` is `Some("@username")` when the target was resolved via
/// username-to-group fallback, meaning the caller should prepend the @mention
/// to the message text so the target bot/user actually sees the mention.
type ResolvedChatTarget = (i64, Option<String>, Option<String>, Option<String>);

async fn resolve_telegram_action_chat_id(
    ctx: &ChannelContext,
    http: &Client,
    current_chat_id: i64,
    target: &str,
) -> Result<ResolvedChatTarget, String> {
    let target = target.trim();
    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    if target.is_empty() || target.eq_ignore_ascii_case("current") {
        return Ok((current_chat_id, None, None, None));
    }
    if let Some(chat_id) = target.strip_prefix("chat:") {
        let parsed = chat_id
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("Invalid Telegram chat target '{}'", target))?;
        let (chat_title, chat_type) =
            resolve_telegram_chat_metadata(ctx, http, &base_url, parsed, None).await?;
        return Ok((parsed, chat_title, chat_type, None));
    }

    let mappings = ctx
        .mission_store
        .list_telegram_chat_missions(ctx.channel.id)
        .await?;

    for mapping in mappings {
        if telegram_action_target_matches(target, mapping.chat_title.as_deref(), None) {
            let (resolved_title, chat_type) = resolve_telegram_chat_metadata(
                ctx,
                http,
                &base_url,
                mapping.chat_id,
                mapping.chat_title.clone(),
            )
            .await?;
            if resolved_title != mapping.chat_title {
                let _ = ctx
                    .mission_store
                    .update_telegram_chat_mission_title(
                        ctx.channel.id,
                        mapping.chat_id,
                        resolved_title.clone(),
                    )
                    .await;
            }
            return Ok((mapping.chat_id, resolved_title, chat_type, None));
        }

        let lookup = match fetch_telegram_chat_lookup(http, &base_url, mapping.chat_id).await {
            Ok(lookup) => lookup,
            Err(error) => {
                tracing::debug!(
                    "Failed to backfill Telegram chat {} on channel {}: {}",
                    mapping.chat_id,
                    ctx.channel.id,
                    error
                );
                continue;
            }
        };

        let resolved_title = telegram_action_lookup_title(&lookup);

        if resolved_title != mapping.chat_title {
            let _ = ctx
                .mission_store
                .update_telegram_chat_mission_title(
                    ctx.channel.id,
                    mapping.chat_id,
                    resolved_title.clone(),
                )
                .await;
        }

        if telegram_action_target_matches(
            target,
            resolved_title.as_deref(),
            lookup.username.as_deref(),
        ) {
            return Ok((lookup.id, resolved_title, Some(lookup.chat_type), None));
        }
    }

    // Fallback: also search telegram_conversations table (not just chat_missions).
    // This covers chats the bot has interacted with that may not have an active mission.
    if let Ok(conversations) = ctx
        .mission_store
        .list_telegram_conversations(ctx.channel.id, 100)
        .await
    {
        for conv in &conversations {
            if telegram_action_target_matches(target, conv.chat_title.as_deref(), None) {
                let (resolved_title, chat_type) = resolve_telegram_chat_metadata(
                    ctx,
                    http,
                    &base_url,
                    conv.chat_id,
                    conv.chat_title.clone(),
                )
                .await?;
                return Ok((conv.chat_id, resolved_title, chat_type, None));
            }
        }

        // For each known group/supergroup conversation, use getChatMember to
        // check whether the requested @username is a member. This lets the bot
        // target another bot or user by @username when they share a group.
        let clean_target = target
            .strip_prefix("title:")
            .or_else(|| target.strip_prefix("username:"))
            .unwrap_or(target)
            .trim()
            .trim_start_matches('@');
        let looks_like_username = !clean_target.is_empty()
            && !clean_target.contains(' ')
            && clean_target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');

        if looks_like_username {
            for conv in &conversations {
                let is_group = matches!(
                    conv.chat_type.as_deref(),
                    Some("group") | Some("supergroup")
                );
                if !is_group {
                    continue;
                }
                // Try getChat(@username) first — this resolves public groups/channels
                // and bot chats when the bot has previously interacted.
                let username_target = format!("@{}", clean_target);
                let get_chat_url = format!("{}/getChat", base_url);
                if let Ok(response) = http
                    .post(&get_chat_url)
                    .json(&serde_json::json!({ "chat_id": username_target }))
                    .send()
                    .await
                {
                    if response.status().is_success() {
                        if let Ok(parsed) = response
                            .json::<TelegramResponse<TelegramActionChatLookup>>()
                            .await
                        {
                            if let Some(lookup) = parsed.result {
                                let resolved_title = telegram_action_lookup_title(&lookup);
                                return Ok((
                                    lookup.id,
                                    resolved_title,
                                    Some(lookup.chat_type),
                                    None,
                                ));
                            }
                        }
                    }
                }

                // Try getChatMember in known group chats to find the @username.
                // getChatMember doesn't accept usernames, but we can try using the
                // numeric chat_id from the conversation to search.  Unfortunately,
                // getChatMember requires a numeric user_id — we don't have one for
                // the target yet.  Instead, we send the message to the group and
                // mention the target username inline, which lets group bots pick up
                // the mention via their own webhook.
                //
                // For now, if the target looks like a username and we have a group
                // conversation, we resolve to that group's chat_id so the message
                // goes there (the caller prepends @mention in the message text).
                let mention = format!("@{}", clean_target);
                tracing::info!(
                    "Resolving {} to group chat {} ({}) for cross-chat mention",
                    mention,
                    conv.chat_id,
                    conv.chat_title.as_deref().unwrap_or("unknown"),
                );
                let (resolved_title, chat_type) = resolve_telegram_chat_metadata(
                    ctx,
                    http,
                    &base_url,
                    conv.chat_id,
                    conv.chat_title.clone(),
                )
                .await?;
                return Ok((conv.chat_id, resolved_title, chat_type, Some(mention)));
            }
        }
    }

    // Last resort: try getChat(@username) directly via Telegram API.
    // This works for public groups/channels where the bot is a member.
    {
        let clean = target
            .strip_prefix("title:")
            .or_else(|| target.strip_prefix("username:"))
            .unwrap_or(target)
            .trim()
            .trim_start_matches('@');
        if !clean.is_empty()
            && !clean.contains(' ')
            && clean.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            let username_target = format!("@{}", clean);
            match fetch_telegram_chat_by_username(http, &base_url, &username_target).await {
                Ok(lookup) => {
                    let resolved_title = telegram_action_lookup_title(&lookup);
                    return Ok((lookup.id, resolved_title, Some(lookup.chat_type), None));
                }
                Err(error) => {
                    tracing::debug!("getChat fallback for {} failed: {}", username_target, error);
                }
            }
        }
    }

    Err(format!("Unknown Telegram chat target '{}'", target))
}

/// Try to resolve a chat by @username via the Telegram Bot API getChat endpoint.
async fn fetch_telegram_chat_by_username(
    http: &Client,
    base_url: &str,
    username: &str,
) -> Result<TelegramActionChatLookup, String> {
    let url = format!("{}/getChat", base_url);
    let response = http
        .post(&url)
        .json(&serde_json::json!({ "chat_id": username }))
        .send()
        .await
        .map_err(|e| format!("getChat failed for {}: {}", username, e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("getChat API error {}: {}", status, body_text));
    }

    let parsed: TelegramResponse<TelegramActionChatLookup> = response
        .json()
        .await
        .map_err(|e| format!("getChat parse failed for {}: {}", username, e))?;

    parsed
        .result
        .ok_or_else(|| format!("getChat returned no result for {}", username))
}

fn telegram_action_target_parts(target: &str) -> (String, String) {
    let trimmed = target.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("current") {
        return ("current".to_string(), "current".to_string());
    }
    if let Some(value) = trimmed.strip_prefix("chat:") {
        return ("chat_id".to_string(), value.trim().to_string());
    }
    if let Some(value) = trimmed.strip_prefix("title:") {
        return ("chat_title".to_string(), value.trim().to_string());
    }
    if let Some(value) = trimmed.strip_prefix("username:") {
        return (
            "username".to_string(),
            value.trim().trim_start_matches('@').to_string(),
        );
    }
    if let Some(value) = trimmed.strip_prefix('@') {
        return ("username".to_string(), value.trim().to_string());
    }
    ("chat_title".to_string(), trimmed.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn log_telegram_action_execution(
    mission_store: &Arc<dyn MissionStore>,
    channel_id: Uuid,
    source_mission_id: Uuid,
    source_chat_id: Option<i64>,
    target_chat_id: i64,
    target_chat_title: Option<String>,
    action_kind: TelegramActionExecutionKind,
    target_kind: String,
    target_value: String,
    text: &str,
    delay_seconds: u64,
    scheduled_message_id: Option<Uuid>,
    status: TelegramActionExecutionStatus,
    last_error: Option<String>,
) {
    let now = now_string();
    let execution = TelegramActionExecution {
        id: Uuid::new_v4(),
        channel_id,
        source_mission_id: Some(source_mission_id),
        source_chat_id,
        target_chat_id,
        target_chat_title,
        action_kind,
        target_kind,
        target_value,
        text: text.to_string(),
        delay_seconds,
        scheduled_message_id,
        status,
        last_error,
        created_at: now.clone(),
        updated_at: now,
    };

    if let Err(error) = mission_store
        .create_telegram_action_execution(execution)
        .await
    {
        tracing::warn!(
            channel_id = %channel_id,
            mission_id = %source_mission_id,
            "Failed to record Telegram action execution: {}",
            error
        );
    }
}

async fn execute_telegram_actions(
    bridge: &SharedTelegramBridge,
    channel_id: Uuid,
    current_chat_id: i64,
    source_mission_id: Uuid,
    actions: &[TelegramAction],
) -> Result<(), String> {
    if actions.is_empty() {
        return Ok(());
    }

    let ctx = bridge
        .get_channel_context(channel_id)
        .await
        .ok_or_else(|| format!("Telegram channel {} is not active", channel_id))?;
    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);

    for action in actions {
        let (chat_id, chat_title, _, mention_username) =
            resolve_telegram_action_chat_id(&ctx, &bridge.http, current_chat_id, &action.target)
                .await?;
        let (target_kind, target_value) = telegram_action_target_parts(&action.target);
        let execution_kind = match action.kind {
            TelegramActionKind::Send => TelegramActionExecutionKind::Send,
            TelegramActionKind::Reminder => TelegramActionExecutionKind::Reminder,
        };
        // When the target was resolved via username-to-group fallback, prepend
        // the @mention so the target bot/user actually sees the mention.
        let action_text = if let Some(ref mention) = mention_username {
            format!("{} {}", mention, action.text)
        } else {
            action.text.clone()
        };

        let delay_seconds = match action.kind {
            TelegramActionKind::Send => action.delay_seconds,
            TelegramActionKind::Reminder => action.delay_seconds.max(1),
        };

        if delay_seconds == 0 {
            match send_chunked_message(&bridge.http, &base_url, chat_id, &action_text, None).await {
                Ok(()) => {
                    log_telegram_action_execution(
                        &ctx.mission_store,
                        channel_id,
                        source_mission_id,
                        Some(current_chat_id),
                        chat_id,
                        chat_title.clone(),
                        execution_kind,
                        target_kind,
                        target_value,
                        &action_text,
                        0,
                        None,
                        TelegramActionExecutionStatus::Sent,
                        None,
                    )
                    .await;
                }
                Err(error) => {
                    log_telegram_action_execution(
                        &ctx.mission_store,
                        channel_id,
                        source_mission_id,
                        Some(current_chat_id),
                        chat_id,
                        chat_title.clone(),
                        execution_kind,
                        target_kind,
                        target_value,
                        &action_text,
                        0,
                        None,
                        TelegramActionExecutionStatus::Failed,
                        Some(error.clone()),
                    )
                    .await;
                    return Err(error);
                }
            }
            continue;
        }

        let scheduled = TelegramScheduledMessage {
            id: Uuid::new_v4(),
            channel_id,
            source_mission_id: Some(source_mission_id),
            chat_id,
            chat_title: chat_title.clone(),
            text: action_text.clone(),
            send_at: (Utc::now() + ChronoDuration::seconds(delay_seconds as i64)).to_rfc3339(),
            sent_at: None,
            status: TelegramScheduledMessageStatus::Pending,
            last_error: None,
            created_at: now_string(),
        };
        ctx.mission_store
            .create_telegram_scheduled_message(scheduled.clone())
            .await?;
        log_telegram_action_execution(
            &ctx.mission_store,
            channel_id,
            source_mission_id,
            Some(current_chat_id),
            chat_id,
            chat_title,
            execution_kind,
            target_kind,
            target_value,
            &action_text,
            delay_seconds,
            Some(scheduled.id),
            TelegramActionExecutionStatus::Pending,
            None,
        )
        .await;
    }

    Ok(())
}

pub async fn execute_native_telegram_action(
    bridge: &SharedTelegramBridge,
    mission_store: &Arc<dyn MissionStore>,
    source_mission_id: Uuid,
    target: TelegramActionTarget,
    text: &str,
    delay_seconds: u64,
) -> Result<TelegramActionExecutionResult, String> {
    let source = resolve_native_telegram_source(bridge, mission_store, source_mission_id).await?;
    let ctx = source.ctx;
    let (target_spec, target_kind, target_value) = match target {
        TelegramActionTarget::Current => (
            "current".to_string(),
            "current".to_string(),
            "current".to_string(),
        ),
        TelegramActionTarget::ChatId(chat_id) => (
            format!("chat:{}", chat_id),
            "chat_id".to_string(),
            chat_id.to_string(),
        ),
        TelegramActionTarget::ChatTitle(title) => {
            (format!("title:{}", title), "chat_title".to_string(), title)
        }
    };
    let current_chat_id = source.source_chat_id.ok_or_else(|| {
        format!(
            "Mission {} has no active Telegram conversation context",
            source_mission_id
        )
    })?;
    let (chat_id, chat_title, _, mention_username) =
        resolve_telegram_action_chat_id(&ctx, bridge.http(), current_chat_id, &target_spec).await?;

    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    // When the target was resolved via username-to-group fallback, prepend
    // the @mention so the target bot/user actually sees the mention.
    let final_text = if let Some(ref mention) = mention_username {
        format!("{} {}", mention, text)
    } else {
        text.to_string()
    };
    let execution_kind = if delay_seconds > 0 && target_spec == "current" {
        TelegramActionExecutionKind::Reminder
    } else {
        TelegramActionExecutionKind::Send
    };
    if delay_seconds == 0 {
        match send_chunked_message(bridge.http(), &base_url, chat_id, &final_text, None).await {
            Ok(()) => {
                log_telegram_action_execution(
                    mission_store,
                    ctx.channel.id,
                    source_mission_id,
                    source.source_chat_id,
                    chat_id,
                    chat_title.clone(),
                    execution_kind.clone(),
                    target_kind,
                    target_value,
                    &final_text,
                    0,
                    None,
                    TelegramActionExecutionStatus::Sent,
                    None,
                )
                .await;
            }
            Err(error) => {
                log_telegram_action_execution(
                    mission_store,
                    ctx.channel.id,
                    source_mission_id,
                    source.source_chat_id,
                    chat_id,
                    chat_title.clone(),
                    execution_kind,
                    target_kind,
                    target_value,
                    &final_text,
                    0,
                    None,
                    TelegramActionExecutionStatus::Failed,
                    Some(error.clone()),
                )
                .await;
                return Err(error);
            }
        }
        return Ok(TelegramActionExecutionResult {
            channel_id: ctx.channel.id,
            chat_id,
            chat_title,
            scheduled_message_id: None,
            immediate: true,
        });
    }

    let scheduled = TelegramScheduledMessage {
        id: Uuid::new_v4(),
        channel_id: ctx.channel.id,
        source_mission_id: Some(source_mission_id),
        chat_id,
        chat_title: chat_title.clone(),
        text: final_text.clone(),
        send_at: (Utc::now() + ChronoDuration::seconds(delay_seconds as i64)).to_rfc3339(),
        sent_at: None,
        status: TelegramScheduledMessageStatus::Pending,
        last_error: None,
        created_at: now_string(),
    };
    ctx.mission_store
        .create_telegram_scheduled_message(scheduled.clone())
        .await?;
    log_telegram_action_execution(
        mission_store,
        ctx.channel.id,
        source_mission_id,
        source.source_chat_id,
        chat_id,
        chat_title.clone(),
        execution_kind,
        target_kind,
        target_value,
        &final_text,
        delay_seconds,
        Some(scheduled.id),
        TelegramActionExecutionStatus::Pending,
        None,
    )
    .await;

    Ok(TelegramActionExecutionResult {
        channel_id: ctx.channel.id,
        chat_id,
        chat_title,
        scheduled_message_id: Some(scheduled.id),
        immediate: false,
    })
}

pub async fn execute_native_telegram_request_workflow(
    bridge: &SharedTelegramBridge,
    mission_store: &Arc<dyn MissionStore>,
    source_mission_id: Uuid,
    target: TelegramActionTarget,
    text: &str,
) -> Result<TelegramWorkflowRequestResult, String> {
    let source = resolve_native_telegram_source(bridge, mission_store, source_mission_id).await?;
    let ctx = source.ctx;
    let source_chat_id = source.source_chat_id.ok_or_else(|| {
        format!(
            "Mission {} has no active Telegram conversation context",
            source_mission_id
        )
    })?;
    let (target_spec, target_title_hint) = match target {
        TelegramActionTarget::Current => ("current".to_string(), None),
        TelegramActionTarget::ChatId(chat_id) => (format!("chat:{}", chat_id), None),
        TelegramActionTarget::ChatTitle(title) => {
            let hint = title.clone();
            (format!("title:{}", title), Some(hint))
        }
    };
    let mut origin_conversation = ctx
        .mission_store
        .get_telegram_conversation_by_chat(ctx.channel.id, source_chat_id)
        .await?
        .unwrap_or_else(|| TelegramConversation {
            id: Uuid::new_v4(),
            channel_id: ctx.channel.id,
            chat_id: source_chat_id,
            mission_id: Some(source_mission_id),
            chat_title: source.source_chat_title.clone(),
            chat_type: None,
            last_message_at: Some(now_string()),
            created_at: now_string(),
            updated_at: now_string(),
        });
    // Backfill chat_type when missing so workflow routing decisions have the
    // correct chat type (group vs private vs supergroup).
    if origin_conversation.chat_type.is_none() {
        let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
        if let Ok(lookup) =
            fetch_telegram_chat_lookup(bridge.http(), &base_url, source_chat_id).await
        {
            if origin_conversation.chat_title.is_none() {
                origin_conversation.chat_title = telegram_action_lookup_title(&lookup);
            }
            origin_conversation.chat_type = Some(lookup.chat_type);
        }
    }
    origin_conversation = ctx
        .mission_store
        .upsert_telegram_conversation(origin_conversation)
        .await?;

    let (target_chat_id, target_chat_title, resolved_target_chat_type, mention_username) =
        resolve_telegram_action_chat_id(&ctx, bridge.http(), source_chat_id, &target_spec).await?;
    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
    let target_conversation = ctx
        .mission_store
        .get_telegram_conversation_by_chat(ctx.channel.id, target_chat_id)
        .await
        .ok()
        .flatten();
    let target_chat_type = if target_chat_id == source_chat_id {
        origin_conversation
            .chat_type
            .clone()
            .or(resolved_target_chat_type.clone())
    } else {
        target_conversation
            .as_ref()
            .and_then(|item| item.chat_type.clone())
            .or(resolved_target_chat_type)
    };
    if matches!(target_chat_type.as_deref(), Some("channel")) {
        return Err("Telegram request workflows are not supported for channel chats".to_string());
    }
    // When the target was resolved via username-to-group fallback, prepend
    // the @mention so the target bot/user actually sees the mention.
    let mention_text = if let Some(ref mention) = mention_username {
        format!("{} {}", mention, text)
    } else {
        text.to_string()
    };
    let delivery_text = workflow_request_delivery_text(&mention_text, target_chat_type.as_deref());

    let now = now_string();
    let mut workflow = TelegramWorkflow {
        id: Uuid::new_v4(),
        channel_id: ctx.channel.id,
        origin_conversation_id: origin_conversation.id,
        origin_chat_id: source_chat_id,
        origin_mission_id: Some(source_mission_id),
        target_conversation_id: target_conversation.as_ref().map(|item| item.id),
        target_chat_id: Some(target_chat_id),
        target_chat_title: target_chat_title.clone().or(target_title_hint),
        target_chat_type: target_chat_type.clone(),
        target_request_message_id: None,
        initiated_by_user_id: None,
        initiated_by_username: None,
        kind: TelegramWorkflowKind::RequestReply,
        status: TelegramWorkflowStatus::WaitingExternal,
        request_text: text.to_string(),
        latest_reply_text: None,
        summary: None,
        last_error: None,
        created_at: now.clone(),
        updated_at: now,
        completed_at: None,
    };
    ctx.mission_store
        .create_telegram_workflow(workflow.clone())
        .await?;

    let _ = ctx
        .mission_store
        .create_telegram_workflow_event(TelegramWorkflowEvent {
            id: Uuid::new_v4(),
            workflow_id: workflow.id,
            conversation_id: Some(origin_conversation.id),
            event_type: "workflow_created".to_string(),
            payload_json: serde_json::json!({
                "origin_chat_id": source_chat_id,
                "target_chat_id": target_chat_id,
                "text": text,
            })
            .to_string(),
            created_at: now_string(),
        })
        .await;

    let message_id = match send_telegram_text(
        bridge.http(),
        &base_url,
        target_chat_id,
        &delivery_text,
        None,
    )
    .await
    {
        Ok(message_id) => message_id,
        Err(err) => {
            workflow.status = TelegramWorkflowStatus::Failed;
            workflow.last_error = Some(err.clone());
            workflow.updated_at = now_string();
            workflow.completed_at = Some(workflow.updated_at.clone());
            let _ = ctx
                .mission_store
                .update_telegram_workflow(workflow.clone())
                .await;
            let _ = ctx
                .mission_store
                .create_telegram_workflow_event(TelegramWorkflowEvent {
                    id: Uuid::new_v4(),
                    workflow_id: workflow.id,
                    conversation_id: Some(origin_conversation.id),
                    event_type: "delivery_failed".to_string(),
                    payload_json: serde_json::json!({
                        "target_chat_id": target_chat_id,
                        "error": err,
                    })
                    .to_string(),
                    created_at: now_string(),
                })
                .await;
            return Err(err);
        }
    };
    workflow.target_request_message_id = Some(message_id);
    let target_conversation_id = if let Some(existing) = target_conversation {
        existing.id
    } else {
        ctx.mission_store
            .upsert_telegram_conversation(TelegramConversation {
                id: Uuid::new_v4(),
                channel_id: ctx.channel.id,
                chat_id: target_chat_id,
                mission_id: None,
                chat_title: target_chat_title.clone(),
                chat_type: target_chat_type.clone(),
                last_message_at: Some(now_string()),
                created_at: now_string(),
                updated_at: now_string(),
            })
            .await?
            .id
    };
    workflow.target_conversation_id = Some(target_conversation_id);
    workflow.updated_at = now_string();
    ctx.mission_store
        .update_telegram_workflow(workflow.clone())
        .await?;

    log_telegram_conversation_message(
        &ctx.mission_store,
        target_conversation_id,
        ctx.channel.id,
        target_chat_id,
        None,
        Some(workflow.id),
        Some(message_id),
        TelegramConversationMessageDirection::Outbound,
        "assistant",
        None,
        None,
        ctx.channel
            .bot_username
            .clone()
            .map(|value| format!("@{}", value)),
        None,
        &delivery_text,
    )
    .await;

    Ok(TelegramWorkflowRequestResult {
        workflow_id: workflow.id,
        channel_id: ctx.channel.id,
        origin_chat_id: source_chat_id,
        target_chat_id,
        target_chat_title,
    })
}

#[derive(Clone)]
struct NativeTelegramSource {
    ctx: ChannelContext,
    source_chat_id: Option<i64>,
    source_chat_title: Option<String>,
}

async fn resolve_native_telegram_source(
    bridge: &SharedTelegramBridge,
    mission_store: &Arc<dyn MissionStore>,
    source_mission_id: Uuid,
) -> Result<NativeTelegramSource, String> {
    if let Some(mapping) = mission_store
        .get_telegram_chat_mission_by_mission_id(source_mission_id)
        .await?
    {
        let ctx = bridge
            .get_channel_context(mapping.channel_id)
            .await
            .ok_or_else(|| format!("Telegram channel {} is not active", mapping.channel_id))?;
        return Ok(NativeTelegramSource {
            ctx,
            source_chat_id: Some(mapping.chat_id),
            source_chat_title: mapping.chat_title,
        });
    }

    let channel = mission_store
        .list_telegram_channels(source_mission_id)
        .await?
        .into_iter()
        .find(|channel| channel.active)
        .ok_or_else(|| {
            format!(
                "Mission {} is not linked to an active Telegram chat",
                source_mission_id
            )
        })?;
    let ctx = bridge
        .get_channel_context(channel.id)
        .await
        .ok_or_else(|| format!("Telegram channel {} is not active", channel.id))?;
    // Find the most recently updated conversation for this mission on this
    // channel. Using updated_at ordering ensures we pick the chat that
    // triggered the current turn (already sorted DESC by the store query).
    let conversation = mission_store
        .list_telegram_conversations(channel.id, 64)
        .await?
        .into_iter()
        .find(|c| c.mission_id == Some(source_mission_id));

    Ok(NativeTelegramSource {
        ctx,
        source_chat_id: conversation.as_ref().map(|c| c.chat_id),
        source_chat_title: conversation.and_then(|c| c.chat_title),
    })
}

async fn relay_workflow_reply_to_origin(
    ctx: &ChannelContext,
    bridge: &SharedTelegramBridge,
    workflow: &TelegramWorkflow,
    sender_name: &str,
    reply_text: &str,
) {
    let Some(origin_mission_id) = workflow.origin_mission_id else {
        return;
    };

    let events_rx = ctx.events_tx.subscribe();
    let relay_message_id = Uuid::new_v4();
    let content = format!(
        "[Telegram workflow reply from {} in chat {}]\n[Original request: {}]\n{}\nReply in the origin Telegram chat with a concise summary and next step if useful.",
        sender_name,
        workflow.target_chat_id.unwrap_or_default(),
        workflow.request_text,
        reply_text
    );
    let (queued_tx, _queued_rx) = tokio::sync::oneshot::channel();
    let _ = ctx
        .cmd_tx
        .send(ControlCommand::UserMessage {
            id: relay_message_id,
            content,
            agent: None,
            target_mission_id: Some(origin_mission_id),
            respond: queued_tx,
        })
        .await;

    let http = bridge.http().clone();
    let bot_token = ctx.channel.bot_token.clone();
    let mission_store = Arc::clone(&ctx.mission_store);
    let channel_id = ctx.channel.id;
    let origin_chat_id = workflow.origin_chat_id;
    let workflow_id = workflow.id;
    let bridge = Arc::clone(bridge);
    let mut updated = workflow.clone();
    tokio::spawn(async move {
        let result = stream_response(
            events_rx,
            &http,
            &bot_token,
            origin_chat_id,
            0,
            Some(relay_message_id),
            origin_mission_id,
            Some(bridge),
            Some(channel_id),
            Some(Arc::clone(&mission_store)),
        )
        .await;

        updated.updated_at = now_string();
        match result {
            Ok(()) => {
                updated.status = TelegramWorkflowStatus::RelayedToOrigin;
                updated.summary = Some("Relayed origin summary".to_string());
                let _ = mission_store
                    .create_telegram_workflow_event(TelegramWorkflowEvent {
                        id: Uuid::new_v4(),
                        workflow_id,
                        conversation_id: Some(updated.origin_conversation_id),
                        event_type: "relayed_to_origin".to_string(),
                        payload_json: serde_json::json!({
                            "origin_chat_id": origin_chat_id,
                        })
                        .to_string(),
                        created_at: now_string(),
                    })
                    .await;
            }
            Err(error) => {
                updated.status = TelegramWorkflowStatus::Failed;
                updated.last_error = Some(error.clone());
                let _ = mission_store
                    .create_telegram_workflow_event(TelegramWorkflowEvent {
                        id: Uuid::new_v4(),
                        workflow_id,
                        conversation_id: Some(updated.origin_conversation_id),
                        event_type: "relay_failed".to_string(),
                        payload_json: serde_json::json!({
                            "error": error,
                        })
                        .to_string(),
                        created_at: now_string(),
                    })
                    .await;
            }
        }
        let _ = mission_store.update_telegram_workflow(updated).await;
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming response (typing indicator + progressive edits)
// ─────────────────────────────────────────────────────────────────────────────

/// Stream the agent response back to Telegram with typing indicator and progressive edits.
///
/// 1. Sends `sendChatAction(typing)` immediately
/// 2. On first `TextDelta`, sends an initial message and captures `message_id`
/// 3. Accumulates subsequent deltas and calls `editMessageText` every ~1s
/// 4. On `AssistantMessage`, sends final edit with full content
#[allow(clippy::too_many_arguments)]
pub async fn stream_response(
    mut events_rx: broadcast::Receiver<AgentEvent>,
    http: &Client,
    bot_token: &str,
    chat_id: i64,
    reply_to: i64,
    expected_user_message_id: Option<Uuid>,
    mission_id: Uuid,
    bridge: Option<SharedTelegramBridge>,
    channel_id: Option<Uuid>,
    mission_store: Option<Arc<dyn MissionStore>>,
) -> Result<(), String> {
    let base_url = format!("https://api.telegram.org/bot{}", bot_token);
    let timeout = tokio::time::Duration::from_secs(300);
    let deadline = tokio::time::Instant::now() + timeout;

    let mut sent_message_id: Option<i64> = None;
    let mut accumulated_text = String::new();
    let mut last_edit = tokio::time::Instant::now();
    let edit_interval = tokio::time::Duration::from_millis(1500);
    let mut typing_interval = tokio::time::interval(tokio::time::Duration::from_secs(4));
    typing_interval.tick().await; // consume the first immediate tick
    let mut request_started = expected_user_message_id.is_none();
    if request_started {
        send_chat_action(http, &base_url, chat_id).await;
    }

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            // If we sent a partial message, finalize it
            if let Some(msg_id) = sent_message_id {
                if !accumulated_text.is_empty() {
                    let final_text = format!(
                        "{}...\n\n_(timed out)_",
                        sanitize_telegram_visible_text(&accumulated_text)
                    );
                    let display = truncate_for_telegram(&final_text);
                    let _ = edit_message(http, &base_url, chat_id, msg_id, &display.html).await;
                }
            }
            return Err("Timeout waiting for agent response".to_string());
        }

        tokio::select! {
            event = events_rx.recv() => {
                match event {
                    Ok(AgentEvent::UserMessage {
                        id,
                        queued,
                        mission_id: Some(mid),
                        ..
                    }) if Some(id) == expected_user_message_id
                        && mid == mission_id
                        && !queued
                        && !request_started =>
                    {
                        request_started = true;
                        send_chat_action(http, &base_url, chat_id).await;
                    }
                    Ok(AgentEvent::TextDelta {
                        content,
                        mission_id: Some(mid),
                        ..
                    }) if request_started && mid == mission_id => {
                        accumulated_text = content;

                        if accumulated_text.contains("<telegram-send")
                            || accumulated_text.contains("<telegram-reminder")
                            || accumulated_text.contains("[telegram-send")
                            || accumulated_text.contains("[telegram-reminder")
                        {
                            continue;
                        }

                        let display_text = sanitize_telegram_visible_text(&accumulated_text);
                        if display_text.is_empty() {
                            continue;
                        }

                        if let Some(msg_id) = sent_message_id {
                            if last_edit.elapsed() >= edit_interval {
                                // Throttled edit
                                let display = truncate_for_telegram(&display_text);
                                if let Err(e) = edit_message(http, &base_url, chat_id, msg_id, &display.html).await {
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        "Failed to edit Telegram message during streaming: {}",
                                        e
                                    );
                                }
                                last_edit = tokio::time::Instant::now();
                            }
                        } else {
                            // Send initial message
                            let reply = if reply_to > 0 { Some(reply_to) } else { None };
                            let existing_message_id = if let (Some(bridge), Some(channel_id)) =
                                (bridge.as_ref(), channel_id)
                            {
                                bridge
                                    .get_sent_reply_message(channel_id, chat_id, reply_to)
                                    .await
                            } else {
                                None
                            };

                            match existing_message_id {
                                Some(msg_id) => {
                                    sent_message_id = Some(msg_id);
                                    let display = truncate_for_telegram(&display_text);
                                    if let Err(e) =
                                        edit_message(http, &base_url, chat_id, msg_id, &display.html).await
                                    {
                                        tracing::warn!(
                                            mission_id = %mission_id,
                                            message_id = msg_id,
                                            "Failed to edit deduplicated Telegram message during streaming: {}",
                                            e
                                        );
                                    }
                                    last_edit = tokio::time::Instant::now();
                                }
                                None => match send_message(
                                    http,
                                    &base_url,
                                    chat_id,
                                    &display_text,
                                    reply,
                                )
                                .await
                                {
                                    Ok(msg_id) => {
                                        sent_message_id = Some(msg_id);
                                        if let (Some(bridge), Some(channel_id)) =
                                            (bridge.as_ref(), channel_id)
                                        {
                                            bridge
                                                .remember_sent_reply_message(
                                                    channel_id,
                                                    chat_id,
                                                    reply_to,
                                                    msg_id,
                                                )
                                                .await;
                                        }
                                        last_edit = tokio::time::Instant::now();
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to send initial Telegram message: {}",
                                            e
                                        );
                                    }
                                },
                            }
                        }
                    }
                    Ok(AgentEvent::AssistantMessage {
                        content,
                        mission_id: Some(mid),
                        shared_files,
                        ..
                    }) if request_started && mid == mission_id => {
                        let (actions, visible_content) = extract_telegram_actions(&content);
                        let mut delivery_text = if actions.is_empty() {
                            content.clone()
                        } else {
                            visible_content
                        };
                        delivery_text = sanitize_telegram_visible_text(&delivery_text);

                        if !actions.is_empty() {
                            if let (Some(bridge), Some(channel_id)) = (bridge.as_ref(), channel_id) {
                                if let Err(err) = execute_telegram_actions(
                                    bridge,
                                    channel_id,
                                    chat_id,
                                    mission_id,
                                    &actions,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        "Failed to execute Telegram actions: {}",
                                        err
                                    );
                                    if delivery_text.trim().is_empty() {
                                        delivery_text = format!(
                                            "I couldn't complete the Telegram action: {}",
                                            err
                                        );
                                    }
                                }
                            } else if delivery_text.trim().is_empty() {
                                delivery_text =
                                    "Telegram action requested, but no active bridge was available."
                                        .to_string();
                            }
                        }

                        // Final response — send or edit with complete text
                        if !delivery_text.trim().is_empty() {
                            if let Some(msg_id) = sent_message_id {
                            // Edit existing message with final content
                                let display = truncate_for_telegram(&delivery_text);
                                if let Err(e) =
                                    edit_message(http, &base_url, chat_id, msg_id, &display.html)
                                        .await
                                {
                                    tracing::warn!(
                                        mission_id = %mission_id,
                                        "Failed to edit Telegram message with final response, sending as new message: {}",
                                        e
                                    );
                                    // Fallback: send entire content as new chunked messages.
                                    // Skip overflow below since chunked send handles the full content.
                                    let _ = send_chunked_message(
                                        http,
                                        &base_url,
                                        chat_id,
                                        &delivery_text,
                                        None,
                                    )
                                    .await;
                                } else {
                                    // Edit succeeded — send overflow chunks for content beyond the first 4096 chars
                                    send_overflow_chunks(
                                        http,
                                        &base_url,
                                        chat_id,
                                        &delivery_text,
                                        display.source_boundary,
                                    )
                                    .await;
                                }
                            } else {
                                // No streaming happened, send the full response directly
                                send_chunked_message(
                                    http,
                                    &base_url,
                                    chat_id,
                                    &delivery_text,
                                    Some(reply_to),
                                )
                                .await?;
                            }
                        }

                        if !delivery_text.trim().is_empty() {
                            if let (Some(mission_store), Some(channel_id)) =
                                (mission_store.as_ref(), channel_id)
                            {
                                let conversation_id = match mission_store
                                    .get_telegram_conversation_by_chat(channel_id, chat_id)
                                    .await
                                {
                                    Ok(Some(conversation)) => conversation.id,
                                    _ => match mission_store
                                        .upsert_telegram_conversation(TelegramConversation {
                                            id: Uuid::new_v4(),
                                            channel_id,
                                            chat_id,
                                            mission_id: Some(mission_id),
                                            chat_title: None,
                                            chat_type: None,
                                            last_message_at: Some(now_string()),
                                            created_at: now_string(),
                                            updated_at: now_string(),
                                        })
                                        .await
                                    {
                                        Ok(conversation) => conversation.id,
                                        Err(_) => Uuid::nil(),
                                    },
                                };
                                if !conversation_id.is_nil() {
                                    log_telegram_conversation_message(
                                        mission_store,
                                        conversation_id,
                                        channel_id,
                                        chat_id,
                                        Some(mission_id),
                                        None,
                                        sent_message_id,
                                        TelegramConversationMessageDirection::Outbound,
                                        "assistant",
                                        None,
                                        None,
                                        None,
                                        if reply_to > 0 { Some(reply_to) } else { None },
                                        &delivery_text,
                                    )
                                    .await;
                                }
                            }
                        }

                        // Send shared files as Telegram documents/photos
                        if let Some(files) = shared_files {
                            for file in &files {
                                let caption =
                                    telegram_shared_file_caption(mission_id, file, &delivery_text);
                                if let Err(e) =
                                    send_file_to_telegram(http, &base_url, chat_id, file, &caption)
                                        .await
                                {
                                    tracing::warn!("Failed to send file {} to Telegram: {}", file.name, e);
                                }
                            }
                        }

                        return Ok(());
                    }
                    Ok(AgentEvent::Error {
                        message,
                        mission_id: Some(mid),
                        ..
                    }) if request_started && mid == mission_id => {
                        let error_msg = format!("Error: {}", message);
                        if let Some(msg_id) = sent_message_id {
                            let final_text = if accumulated_text.is_empty() {
                                error_msg
                            } else {
                                format!("{}\n\n_{}_", accumulated_text, error_msg)
                            };
                            let display = truncate_for_telegram(&final_text);
                            let _ = edit_message(http, &base_url, chat_id, msg_id, &display.html).await;
                        } else {
                            let _ = send_message(http, &base_url, chat_id, &error_msg, Some(reply_to)).await;
                        }
                        return Ok(());
                    }
                    Ok(AgentEvent::Thinking {
                        mission_id: Some(mid),
                        ..
                    }) if request_started && mid == mission_id => {
                        // Keep sending typing indicator while agent is thinking
                        send_chat_action(http, &base_url, chat_id).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Telegram response listener lagged by {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("Event channel closed".to_string());
                    }
                    _ => {
                        // Not our event, keep listening
                    }
                }
            }
            _ = typing_interval.tick() => {
                // Keep typing indicator alive every 4s while waiting
                if request_started && sent_message_id.is_none() {
                    send_chat_action(http, &base_url, chat_id).await;
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Telegram API helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Send `sendChatAction(typing)` to show typing indicator.
async fn send_chat_action(http: &Client, base_url: &str, chat_id: i64) {
    let url = format!("{}/sendChatAction", base_url);
    let _ = http
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "action": "typing"
        }))
        .send()
        .await;
}

/// Send a file to a Telegram chat via sendDocument or sendPhoto.
/// The file is read from the URL in SharedFile (which is a local file:// or http:// path).
async fn send_file_to_telegram(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    file: &crate::api::control::SharedFile,
    caption: &str,
) -> Result<(), String> {
    use reqwest::multipart;

    // Read the file from the URL (which could be a relative workspace path or absolute)
    let file_path = if file.url.starts_with("http://") || file.url.starts_with("https://") {
        // Download from URL first (cap at 50MB to prevent OOM)
        const MAX_DOWNLOAD: usize = 50 * 1024 * 1024;
        let resp = http
            .get(&file.url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch file from URL: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("File fetch HTTP error {}", resp.status(),));
        }
        if let Some(len) = resp.content_length() {
            if len as usize > MAX_DOWNLOAD {
                return Err(format!(
                    "File too large: {} bytes (max {})",
                    len, MAX_DOWNLOAD
                ));
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read file bytes: {}", e))?;
        if bytes.len() > MAX_DOWNLOAD {
            return Err(format!(
                "File too large: {} bytes (max {})",
                bytes.len(),
                MAX_DOWNLOAD
            ));
        }
        // Sanitize filename to prevent path traversal
        let safe_name = file
            .name
            .replace(['/', '\\', '\0'], "_")
            .trim_start_matches('.')
            .to_string();
        let safe_name = if safe_name.is_empty() {
            "file".to_string()
        } else {
            safe_name
        };
        let tmp_path = std::path::PathBuf::from("/tmp/telegram-outbound").join(&safe_name);
        if let Some(parent) = tmp_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(&tmp_path, &bytes)
            .await
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        tmp_path
    } else {
        // Local file path — must be under a workspace directory
        let path = std::path::PathBuf::from(&file.url);
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve file path: {}", e))?;
        let allowed_roots = ["/root/workspaces/", "/tmp/"];
        if !allowed_roots.iter().any(|r| canonical.starts_with(r)) {
            return Err(format!(
                "File path outside allowed directories: {}",
                canonical.display()
            ));
        }
        canonical
    };

    if !file_path.exists() {
        return Err(format!("File not found: {}", file_path.display()));
    }

    let file_bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let is_image = file.content_type.starts_with("image/") && !file.content_type.contains("svg");

    let (endpoint, field_name) = if is_image {
        ("sendPhoto", "photo")
    } else {
        ("sendDocument", "document")
    };

    let url = format!("{}/{}", base_url, endpoint);
    let file_part = multipart::Part::bytes(file_bytes)
        .file_name(file.name.clone())
        .mime_str(&file.content_type)
        .map_err(|e| format!("Invalid MIME type: {}", e))?;

    let form = multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .text("caption", caption.to_string())
        .part(field_name, file_part);

    let response = http
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("{} request failed: {}", endpoint, e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("{} API error {}: {}", endpoint, status, body));
    }

    tracing::info!("Sent file {} to Telegram chat {}", file.name, chat_id);
    Ok(())
}

/// Public API for sending a text message to a Telegram chat.
/// Handles markdown-to-HTML conversion and chunking for long messages.
/// Public API for sending a text message to a Telegram chat.
/// Handles markdown-to-HTML conversion and chunking for long messages.
pub async fn send_telegram_text(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
) -> Result<i64, String> {
    let display = truncate_for_telegram(text);
    let msg_id = send_message_html(http, base_url, chat_id, &display.html, reply_to).await?;
    if display.source_boundary < text.len() {
        send_overflow_chunks(http, base_url, chat_id, text, display.source_boundary).await;
    }
    Ok(msg_id)
}

/// Send a message and return the message_id. Truncates to first 4096 HTML chars.
async fn send_message(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
) -> Result<i64, String> {
    let display = truncate_for_telegram(text);
    send_message_html(http, base_url, chat_id, &display.html, reply_to).await
}

/// Send pre-rendered HTML text.
async fn send_message_html(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    html: &str,
    reply_to: Option<i64>,
) -> Result<i64, String> {
    let body = SendMessageRequest {
        chat_id,
        text: html,
        reply_to_message_id: reply_to,
        parse_mode: Some("HTML"),
    };

    let url = format!("{}/sendMessage", base_url);
    let response = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("sendMessage failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        return Err(format!("sendMessage API error {}: {}", status, body_text));
    }

    let parsed: TelegramResponse<SendMessageResponse> = response
        .json()
        .await
        .map_err(|e| format!("sendMessage parse failed: {}", e))?;

    parsed
        .result
        .map(|r| r.message_id)
        .ok_or_else(|| "sendMessage returned no result".to_string())
}

/// Edit an existing message's text.
async fn edit_message(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    message_id: i64,
    html: &str,
) -> Result<(), String> {
    if html.is_empty() {
        return Ok(());
    }
    let body = EditMessageRequest {
        chat_id,
        message_id,
        text: html,
        parse_mode: Some("HTML"),
    };

    let url = format!("{}/editMessageText", base_url);
    let response = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("editMessageText failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        // "message is not modified" is not a real error (same content)
        if body_text.contains("message is not modified") {
            return Ok(());
        }
        return Err(format!(
            "editMessageText API error {}: {}",
            status, body_text
        ));
    }

    Ok(())
}

/// Find the byte index that includes at most `max_chars` characters.
fn char_boundary_at(text: &str, max_chars: usize) -> usize {
    text.char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Convert markdown to Telegram HTML for rich rendering.
/// Handles **bold**, *italic*, `code`, ```blocks```, # headers, [links](url).
#[allow(clippy::while_let_on_iterator)]
pub fn markdown_to_telegram_html(text: &str) -> String {
    // Escape HTML special chars first
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    let mut result = String::with_capacity(escaped.len());
    let mut chars = escaped.chars().peekable();
    let mut at_line_start = true;

    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut content = String::new();
                while let Some(c) = chars.next() {
                    if c == '*' && chars.peek() == Some(&'*') {
                        chars.next();
                        break;
                    }
                    content.push(c);
                }
                result.push_str("<b>");
                result.push_str(&content);
                result.push_str("</b>");
                at_line_start = false;
            }
            '*' => {
                let mut content = String::new();
                while let Some(c) = chars.next() {
                    if c == '*' {
                        break;
                    }
                    content.push(c);
                }
                if content.is_empty() {
                    result.push('*');
                } else {
                    result.push_str("<i>");
                    result.push_str(&content);
                    result.push_str("</i>");
                }
                at_line_start = false;
            }
            '`' if chars.peek() == Some(&'`') => {
                chars.next();
                if chars.peek() == Some(&'`') {
                    chars.next();
                    // Skip language tag
                    while chars.peek().map(|c| *c != '\n').unwrap_or(false) {
                        chars.next();
                    }
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    let mut code = String::new();
                    while let Some(c) = chars.next() {
                        if c == '`' && chars.peek() == Some(&'`') {
                            chars.next();
                            if chars.peek() == Some(&'`') {
                                chars.next();
                            }
                            break;
                        }
                        code.push(c);
                    }
                    result.push_str("<pre>");
                    result.push_str(code.trim_end());
                    result.push_str("</pre>");
                } else {
                    let mut code = String::new();
                    while let Some(c) = chars.next() {
                        if c == '`' && chars.peek() == Some(&'`') {
                            chars.next();
                            break;
                        }
                        code.push(c);
                    }
                    result.push_str("<code>");
                    result.push_str(&code);
                    result.push_str("</code>");
                }
                at_line_start = false;
            }
            '`' => {
                let mut code = String::new();
                while let Some(c) = chars.next() {
                    if c == '`' {
                        break;
                    }
                    code.push(c);
                }
                result.push_str("<code>");
                result.push_str(&code);
                result.push_str("</code>");
                at_line_start = false;
            }
            '#' if at_line_start => {
                while chars.peek() == Some(&'#') {
                    chars.next();
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                let mut header = String::new();
                while chars.peek().map(|c| *c != '\n').unwrap_or(false) {
                    header.push(chars.next().unwrap());
                }
                result.push_str("<b>");
                result.push_str(&header);
                result.push_str("</b>");
                at_line_start = false;
            }
            '[' => {
                let mut link_text = String::new();
                let mut found_link = false;
                while let Some(c) = chars.next() {
                    if c == ']' {
                        if chars.peek() == Some(&'(') {
                            chars.next();
                            let mut url = String::new();
                            let mut paren_depth = 1u32;
                            while let Some(c) = chars.next() {
                                if c == '(' {
                                    paren_depth += 1;
                                    url.push(c);
                                } else if c == ')' {
                                    paren_depth -= 1;
                                    if paren_depth == 0 {
                                        break;
                                    }
                                    url.push(c);
                                } else {
                                    url.push(c);
                                }
                            }
                            result.push_str("<a href=\"");
                            result.push_str(&url.replace('"', "&quot;"));
                            result.push_str("\">");
                            result.push_str(&link_text);
                            result.push_str("</a>");
                            found_link = true;
                        }
                        break;
                    }
                    link_text.push(c);
                }
                if !found_link {
                    result.push('[');
                    result.push_str(&link_text);
                    result.push(']');
                }
                at_line_start = false;
            }
            '\n' => {
                result.push('\n');
                at_line_start = true;
            }
            _ => {
                result.push(ch);
                at_line_start = false;
            }
        }
    }
    result
}

struct TelegramRenderChunk {
    html: String,
    source_boundary: usize,
}

fn render_telegram_chunk(
    text: &str,
    max_chars: usize,
    truncated_suffix: Option<&str>,
) -> TelegramRenderChunk {
    let html = markdown_to_telegram_html(text);
    if html.chars().count() <= max_chars {
        return TelegramRenderChunk {
            html,
            source_boundary: text.len(),
        };
    }

    let suffix = truncated_suffix.unwrap_or("");
    let suffix_chars = suffix.chars().count();
    let available_chars = max_chars.saturating_sub(suffix_chars);
    let total_chars = text.chars().count();
    let mut low = 0usize;
    let mut high = total_chars;
    let mut best_chars = 0usize;

    while low <= high {
        let mid = (low + high) / 2;
        let boundary = char_boundary_at(text, mid);
        let candidate = markdown_to_telegram_html(&text[..boundary]);

        if candidate.chars().count() <= available_chars {
            best_chars = mid;
            low = mid.saturating_add(1);
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    let source_boundary = char_boundary_at(text, best_chars);
    let mut html = markdown_to_telegram_html(&text[..source_boundary]);
    html.push_str(suffix);

    TelegramRenderChunk {
        html,
        source_boundary,
    }
}

fn truncate_for_telegram(text: &str) -> TelegramRenderChunk {
    render_telegram_chunk(text, 4096, Some("..."))
}

/// Send overflow chunks (content beyond 4096 chars) as separate messages.
async fn send_overflow_chunks(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    text: &str,
    source_boundary: usize,
) {
    if source_boundary >= text.len() {
        return;
    }
    let rest = &text[source_boundary..];
    if rest.is_empty() {
        return;
    }
    let _ = send_chunked_message(http, base_url, chat_id, rest, None).await;
}

/// Send a long message split into multiple chunks.
async fn send_chunked_message(
    http: &Client,
    base_url: &str,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
) -> Result<(), String> {
    let mut remaining = text;
    let mut first = true;
    while !remaining.is_empty() {
        let rendered = render_telegram_chunk(remaining, 4096, None);
        let reply = if first { reply_to } else { None };
        first = false;
        send_message(
            http,
            base_url,
            chat_id,
            &remaining[..rendered.source_boundary],
            reply,
        )
        .await?;
        remaining = &remaining[rendered.source_boundary..];
    }
    Ok(())
}

/// Fetch the bot's username via getMe.
pub async fn get_bot_username(http: &Client, base_url: &str) -> Result<String, String> {
    let url = format!("{}/getMe", base_url);
    let response = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("getMe failed: {}", e))?;

    #[derive(Deserialize)]
    struct GetMeResult {
        username: Option<String>,
    }

    let body: TelegramResponse<GetMeResult> = response
        .json()
        .await
        .map_err(|e| format!("getMe parse error: {}", e))?;

    body.result
        .and_then(|r| r.username)
        .ok_or_else(|| "Bot has no username".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_internal_telegram_action_token, extract_structured_memory_from_text,
        extract_telegram_actions, feedback_mutes_alerts, feedback_only_failures,
        feedback_raises_interest, format_structured_memory_context, is_paloma_command,
        is_paloma_shared_summary_allowed, markdown_to_telegram_html, merge_telegram_chat_metadata,
        mission_label, normalize_paloma_natural_command, paloma_alert_body,
        paloma_alert_digest_text, paloma_alert_event_kind_at, paloma_alert_importance_for_mission,
        paloma_alert_kind_for_status, paloma_channel_job_name, paloma_chat_is_allowed,
        paloma_command_error_response, paloma_mission_has_failure_only_preference,
        paloma_role_for_user, paloma_shared_summary_body,
        paloma_should_alert_long_running_mission_at, paloma_should_alert_mission_at,
        paloma_status_event_is_new, paloma_status_seen_sequences,
        parse_paloma_selector_and_payload, redact_for_telegram, render_telegram_chunk,
        sanitize_telegram_visible_text, scope_for_extracted_memory,
        select_latest_awaiting_user_mission, should_silence_paloma_shared_command,
        telegram_action_target_matches, telegram_chat_display_title, telegram_shared_file_caption,
        truncate_for_telegram, verify_internal_telegram_action_token, workflow_reply_text,
        workflow_request_delivery_text, Chat, ExtractedTelegramMemory, Message, Mission,
        MissionMode, MissionStatus, TelegramAction, TelegramActionKind, TelegramAlert,
        TelegramAlertPreference, TelegramBridge, TelegramChannel, TelegramMemorySubject,
        TelegramMissionInterestLevel, TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind,
        TelegramStructuredMemoryScope, TelegramTriggerMode, TelegramUser, TelegramUserRole,
        PALOMA_SCHEDULER_JOB_LEASE_SECONDS,
    };
    use crate::api::control::SharedFile;
    use crate::api::mission_store::StoredEvent;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use uuid::Uuid;

    fn test_mission(title: &str, status: MissionStatus) -> Mission {
        Mission {
            id: Uuid::new_v4(),
            status,
            title: Some(title.to_string()),
            short_description: None,
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: Uuid::new_v4(),
            workspace_name: None,
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "opencode".to_string(),
            config_profile: None,
            history: vec![],
            created_at: "2026-05-20T00:00:00Z".to_string(),
            updated_at: "2026-05-20T00:00:00Z".to_string(),
            interrupted_at: None,
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
        }
    }

    fn test_alert(title: &str, body: &str, importance: &str, created_at: &str) -> TelegramAlert {
        TelegramAlert {
            id: Uuid::new_v4(),
            telegram_user_id: 1_139_694_048,
            mission_id: Some(Uuid::new_v4()),
            event_kind: format!("mission_update:{title}"),
            importance: importance.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            status: "pending".to_string(),
            telegram_message_id: None,
            last_error: None,
            created_at: created_at.to_string(),
            sent_at: None,
            acknowledged_at: None,
        }
    }

    fn test_paloma_channel(allowed_chat_ids: Vec<i64>) -> TelegramChannel {
        TelegramChannel {
            id: Uuid::new_v4(),
            mission_id: Uuid::new_v4(),
            bot_token: "test-token".to_string(),
            bot_username: Some("paloma_test_bot".to_string()),
            allowed_chat_ids,
            trigger_mode: TelegramTriggerMode::MentionOrDm,
            active: true,
            webhook_secret: None,
            instructions: None,
            auto_create_missions: true,
            default_backend: None,
            default_model_override: None,
            default_model_effort: None,
            default_workspace_id: None,
            default_config_profile: None,
            default_agent: None,
            created_at: "2026-05-20T00:00:00Z".to_string(),
            updated_at: "2026-05-20T00:00:00Z".to_string(),
        }
    }

    fn test_paloma_message(chat_id: i64, chat_type: &str, text: &str) -> Message {
        Message {
            message_id: 77,
            chat: Chat {
                id: chat_id,
                chat_type: chat_type.to_string(),
                title: Some("Shared chat".to_string()),
                username: None,
                first_name: None,
                last_name: None,
            },
            from: None,
            text: Some(text.to_string()),
            caption: None,
            reply_to_message: None,
            entities: None,
            caption_entities: None,
            document: None,
            photo: None,
            voice: None,
            audio: None,
            video: None,
        }
    }

    fn test_paloma_user(role: TelegramUserRole) -> TelegramUser {
        TelegramUser {
            id: Uuid::new_v4(),
            telegram_user_id: 42,
            username: Some("tester".to_string()),
            display_name: Some("Tester".to_string()),
            role,
            created_at: "2026-05-20T00:00:00Z".to_string(),
            updated_at: "2026-05-20T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn truncate_for_telegram_preserves_valid_html_boundaries() {
        let text = "**bold** ".repeat(700);
        let rendered = truncate_for_telegram(&text);

        assert!(rendered.html.chars().count() <= 4096);
        assert!(rendered.source_boundary < text.len());
        assert!(
            rendered.html.ends_with("...</b>...")
                || rendered.html.ends_with("</b>...")
                || rendered.html.ends_with("...")
        );
        assert!(!rendered.html.contains("&lt;b&gt;"));
    }

    #[test]
    fn render_chunk_tracks_consumed_source_before_html_limit() {
        let text = "[label](https://example.com) ".repeat(300);
        let rendered = render_telegram_chunk(&text, 4096, None);
        let full_html = markdown_to_telegram_html(&text);

        assert!(rendered.html.chars().count() <= 4096);
        assert!(rendered.source_boundary < text.len());
        assert!(full_html.chars().count() > 4096);
        assert_eq!(
            rendered.html,
            markdown_to_telegram_html(&text[..rendered.source_boundary])
        );
    }

    #[tokio::test]
    async fn telegram_bridge_deduplicates_updates_per_channel() {
        let bridge = TelegramBridge::new();
        let channel_id = Uuid::new_v4();

        assert!(bridge.register_update_once(channel_id, 42).await);
        assert!(!bridge.register_update_once(channel_id, 42).await);
        assert!(bridge.register_update_once(channel_id, 43).await);
        assert!(bridge.register_update_once(Uuid::new_v4(), 42).await);
    }

    #[tokio::test]
    async fn telegram_bridge_tracks_paloma_session_locks_and_jobs() {
        let bridge = TelegramBridge::new();
        let channel_id = Uuid::new_v4();

        let first = bridge.paloma_session_lock(channel_id, 123).await;
        let second = bridge.paloma_session_lock(channel_id, 123).await;
        let other = bridge.paloma_session_lock(channel_id, 456).await;
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));

        let queue_key = bridge
            .enqueue_paloma_session(channel_id, 123, Some(Uuid::new_v4()))
            .await
            .expect("queue accepts inbound work");
        assert_eq!(bridge.paloma_queue_metrics().await.pending_items, 1);
        bridge.dequeue_paloma_session(&queue_key).await;
        assert_eq!(bridge.paloma_queue_metrics().await.pending_items, 0);

        bridge.record_paloma_job_success("paloma_alert_scan").await;
        let states = bridge.paloma_job_states().await;
        let alert_scan = states
            .iter()
            .find(|state| state.name == "paloma_alert_scan")
            .expect("alert scan job");
        assert_eq!(alert_scan.runs, 1);
        assert!(alert_scan.last_success_at.is_some());
    }

    #[test]
    fn paloma_scheduler_job_names_are_scoped_per_channel() {
        let channel_a = Uuid::new_v4();
        let channel_b = Uuid::new_v4();

        assert_ne!(
            paloma_channel_job_name("paloma_alert_scan", channel_a),
            paloma_channel_job_name("paloma_alert_scan", channel_b)
        );
        assert!(paloma_channel_job_name("paloma_alert_scan", channel_a)
            .starts_with("paloma_alert_scan:"));
    }

    #[test]
    fn paloma_scheduler_lease_covers_slow_alert_work() {
        let lease_seconds = std::hint::black_box(PALOMA_SCHEDULER_JOB_LEASE_SECONDS);
        assert!(lease_seconds >= 300);
    }

    #[tokio::test]
    async fn telegram_bridge_reuses_sent_reply_message_for_same_inbound_message() {
        let bridge = TelegramBridge::new();
        let channel_id = Uuid::new_v4();

        assert_eq!(
            bridge.get_sent_reply_message(channel_id, 123, 456).await,
            None
        );

        bridge
            .remember_sent_reply_message(channel_id, 123, 456, 789)
            .await;

        assert_eq!(
            bridge.get_sent_reply_message(channel_id, 123, 456).await,
            Some(789)
        );
        assert_eq!(
            bridge.get_sent_reply_message(channel_id, 123, 999).await,
            None
        );
    }

    #[test]
    fn paloma_command_matching_is_exact_command_or_argument_tail() {
        assert!(is_paloma_command("/status", "/status"));
        assert!(is_paloma_command("/status latest", "/status"));
        assert!(!is_paloma_command("/statusplease", "/status"));
        assert!(!is_paloma_command("status", "/status"));
    }

    #[test]
    fn paloma_natural_owner_commands_cover_status_and_missions() {
        assert_eq!(
            normalize_paloma_natural_command("update moi sur les missions en cours"),
            Some("/missions")
        );
        assert_eq!(
            normalize_paloma_natural_command("quelles missions actives en ce moment ?"),
            Some("/missions")
        );
        assert_eq!(
            normalize_paloma_natural_command("show current running missions"),
            Some("/missions")
        );
        assert_eq!(
            normalize_paloma_natural_command("mets-moi à jour sur le statut"),
            Some("/status")
        );
        assert_eq!(
            normalize_paloma_natural_command("what changed since yesterday?"),
            Some("/status")
        );
        assert_eq!(normalize_paloma_natural_command("/status"), None);
        assert_eq!(normalize_paloma_natural_command("how are you?"), None);
    }

    #[test]
    fn paloma_owner_role_defaults_to_thomas_and_others_observer() {
        assert_eq!(paloma_role_for_user(1_139_694_048), TelegramUserRole::Owner);
        assert_eq!(paloma_role_for_user(42), TelegramUserRole::Observer);
    }

    #[test]
    fn paloma_shared_summary_is_limited_to_allowlisted_owner_or_friend() {
        let channel = test_paloma_channel(vec![-100]);
        let msg = test_paloma_message(-100, "supergroup", "/summary");
        let friend = test_paloma_user(TelegramUserRole::TrustedFriend);
        let owner = test_paloma_user(TelegramUserRole::Owner);
        let observer = test_paloma_user(TelegramUserRole::Observer);

        assert!(is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &msg,
            "/summary"
        ));
        assert!(is_paloma_shared_summary_allowed(
            &channel,
            Some(&owner),
            &msg,
            "/summary"
        ));
        assert!(!is_paloma_shared_summary_allowed(
            &channel,
            Some(&observer),
            &msg,
            "/summary"
        ));
        assert!(!is_paloma_shared_summary_allowed(
            &test_paloma_channel(vec![]),
            Some(&friend),
            &msg,
            "/summary"
        ));
        assert!(!is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &test_paloma_message(-101, "supergroup", "/summary"),
            "/summary"
        ));
    }

    #[test]
    fn paloma_group_commands_obey_channel_allowlist() {
        let channel = test_paloma_channel(vec![-100]);
        let allowed_group = test_paloma_message(-100, "supergroup", "/status");
        let blocked_group = test_paloma_message(-101, "supergroup", "/status");
        let dm = test_paloma_message(42, "private", "/status");

        assert!(paloma_chat_is_allowed(&channel, &allowed_group));
        assert!(!paloma_chat_is_allowed(&channel, &blocked_group));
        assert!(paloma_chat_is_allowed(&channel, &dm));
    }

    #[test]
    fn paloma_shared_summary_does_not_allow_control_commands_or_private_dm() {
        let channel = test_paloma_channel(vec![-100]);
        let friend = test_paloma_user(TelegramUserRole::TrustedFriend);

        assert!(!is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &test_paloma_message(-100, "supergroup", "/send latest go"),
            "/send latest go"
        ));
        assert!(!is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &test_paloma_message(-100, "private", "/summary"),
            "/summary"
        ));
        assert!(!is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &test_paloma_message(-100, "supergroup", "/summary latest"),
            "/summary latest"
        ));
        assert!(is_paloma_shared_summary_allowed(
            &channel,
            Some(&friend),
            &test_paloma_message(-100, "supergroup", "/summary@paloma_test_bot"),
            "/summary@paloma_test_bot"
        ));
    }

    #[test]
    fn paloma_shared_commands_stay_silent_except_safe_summary() {
        let channel = test_paloma_channel(vec![-100]);
        let friend = test_paloma_user(TelegramUserRole::TrustedFriend);
        let owner = test_paloma_user(TelegramUserRole::Owner);
        let observer = test_paloma_user(TelegramUserRole::Observer);
        let group = test_paloma_message(-100, "supergroup", "/status");

        assert!(should_silence_paloma_shared_command(
            &channel,
            Some(&observer),
            &group,
            "/status"
        ));
        assert!(should_silence_paloma_shared_command(
            &channel,
            Some(&owner),
            &test_paloma_message(-100, "supergroup", "/send latest go"),
            "/send latest go"
        ));
        assert!(!should_silence_paloma_shared_command(
            &channel,
            Some(&friend),
            &test_paloma_message(-100, "supergroup", "/summary"),
            "/summary"
        ));
        assert!(!should_silence_paloma_shared_command(
            &channel,
            Some(&observer),
            &test_paloma_message(42, "private", "/status"),
            "/status"
        ));
        assert!(!should_silence_paloma_shared_command(
            &channel,
            Some(&observer),
            &test_paloma_message(-100, "supergroup", "status please"),
            "status please"
        ));
    }

    #[test]
    fn paloma_shared_summary_excludes_recent_event_details() {
        let mission = test_mission("Proof deployment", MissionStatus::Active);
        let body = paloma_shared_summary_body(&mission);

        assert!(body.contains("Proof deployment: active"));
        assert!(body.contains("high-level status"));
        assert!(!body.contains("Latest:"));
        assert!(!body.contains("replied:"));
    }

    #[test]
    fn telegram_shared_file_caption_includes_origin_and_short_context() {
        let mission_id = Uuid::parse_str("aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb").unwrap();
        let file = SharedFile::new(
            "report.pdf",
            "/tmp/report.pdf",
            "application/pdf",
            Some(128),
        );

        let caption = telegram_shared_file_caption(
            mission_id,
            &file,
            "Finished the deployment report.\nSecond line should be ignored.",
        );

        assert!(caption.contains("Mission aaaaaaaa shared report.pdf"));
        assert!(caption.contains("Finished the deployment report."));
        assert!(!caption.contains("Second line"));
    }

    #[test]
    fn telegram_shared_file_caption_redacts_sensitive_details() {
        let mission_id = Uuid::parse_str("aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb").unwrap();
        let file = SharedFile::new(
            "token-abc12345678901234567890.txt",
            "/tmp/report.txt",
            "text/plain",
            Some(128),
        );

        let caption = telegram_shared_file_caption(
            mission_id,
            &file,
            "Saved under /root/.sandboxed-sh/missions/missions-dev.db",
        );

        assert!(caption.contains("[redacted]"));
        assert!(caption.contains("[path redacted]"));
        assert!(!caption.contains("abc12345678901234567890"));
        assert!(!caption.contains("/root/.sandboxed-sh"));
    }

    #[test]
    fn paloma_redaction_removes_secrets_and_private_paths() {
        let text =
            "token=abc12345678901234567890 path /root/.sandboxed-sh/missions/missions-dev.db";
        let redacted = redact_for_telegram(text);
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("[path redacted]"));
        assert!(!redacted.contains("abc12345678901234567890"));
        assert!(!redacted.contains("/root/.sandboxed-sh"));
    }

    #[test]
    fn paloma_selector_parser_requires_selector_and_payload() {
        assert_eq!(
            parse_paloma_selector_and_payload("/send latest focus on tests", "/send"),
            Some(("latest", "focus on tests"))
        );
        assert_eq!(
            parse_paloma_selector_and_payload("/send latest", "/send"),
            None
        );
    }

    #[test]
    fn paloma_status_cursor_uses_per_mission_sequences() {
        let mission = test_mission("Cursor test", MissionStatus::Active);
        let seen_sequences = paloma_status_seen_sequences(&format!(r#"{{"{}":2}}"#, mission.id));
        let new_sequence_old_timestamp = StoredEvent {
            id: 1,
            mission_id: mission.id,
            sequence: 3,
            event_type: "assistant_message".to_string(),
            timestamp: "2026-05-20T00:00:01Z".to_string(),
            event_id: None,
            tool_call_id: None,
            tool_name: None,
            content: "New by sequence despite old clock.".to_string(),
            metadata: serde_json::json!({}),
        };
        let old_sequence = StoredEvent {
            sequence: 2,
            content: "Already seen.".to_string(),
            ..new_sequence_old_timestamp.clone()
        };

        assert!(paloma_status_event_is_new(
            mission.id,
            &new_sequence_old_timestamp,
            &seen_sequences,
            Some("2026-05-20T01:00:00Z"),
            None,
        ));
        assert!(!paloma_status_event_is_new(
            mission.id,
            &old_sequence,
            &seen_sequences,
            Some("2026-05-20T00:00:00Z"),
            None,
        ));
    }

    #[test]
    fn paloma_status_cursor_respects_newer_dashboard_view() {
        let mission = test_mission("Dashboard cursor", MissionStatus::Active);
        let seen_sequences = paloma_status_seen_sequences(&format!(r#"{{"{}":2}}"#, mission.id));
        let event = StoredEvent {
            id: 1,
            mission_id: mission.id,
            sequence: 3,
            event_type: "assistant_message".to_string(),
            timestamp: "2026-05-20T00:30:00Z".to_string(),
            event_id: None,
            tool_call_id: None,
            tool_name: None,
            content: "Viewed on dashboard.".to_string(),
            metadata: serde_json::json!({}),
        };

        assert!(!paloma_status_event_is_new(
            mission.id,
            &event,
            &seen_sequences,
            Some("2026-05-20T00:10:00Z"),
            Some("2026-05-20T00:40:00Z"),
        ));
    }

    #[test]
    fn paloma_approval_targets_newest_awaiting_user_mission() {
        let mut failed = test_mission("Failed", MissionStatus::Failed);
        failed.updated_at = "2026-05-20T00:30:00Z".to_string();
        let mut older_waiting = test_mission("Older question", MissionStatus::AwaitingUser);
        older_waiting.updated_at = "2026-05-20T00:10:00Z".to_string();
        let mut newer_waiting = test_mission("Newer question", MissionStatus::AwaitingUser);
        newer_waiting.updated_at = "2026-05-20T00:20:00Z".to_string();

        let selected =
            select_latest_awaiting_user_mission(vec![failed, older_waiting, newer_waiting])
                .expect("awaiting mission");

        assert_eq!(mission_label(&selected), "Newer question");
    }

    #[test]
    fn paloma_feedback_parser_recognizes_interest_changes() {
        assert!(feedback_mutes_alerts("Don't tell me about this again."));
        assert!(feedback_mutes_alerts(
            "I'm not interested in these updates."
        ));
        assert!(feedback_mutes_alerts("This is too many updates."));
        assert!(feedback_mutes_alerts("Stop these updates please."));
        assert!(feedback_raises_interest("Keep me posted on this."));
        assert!(feedback_only_failures("Only tell me if this fails."));
        assert!(feedback_only_failures("Only failures please."));
        assert!(!feedback_mutes_alerts("what changed?"));
    }

    #[test]
    fn paloma_failure_only_preference_matches_mission_scope() {
        let mission_id = Uuid::new_v4();
        let preferences = vec![TelegramAlertPreference {
            id: Uuid::new_v4(),
            telegram_user_id: 42,
            scope: "mission".to_string(),
            scope_value: Some(mission_id.to_string()),
            rule_text: "only failures from Telegram feedback".to_string(),
            enabled: true,
            created_from_message_id: Some(10),
            created_at: "2026-05-20T00:00:00Z".to_string(),
            updated_at: "2026-05-20T00:00:00Z".to_string(),
        }];

        assert!(paloma_mission_has_failure_only_preference(
            &preferences,
            mission_id
        ));
        assert!(!paloma_mission_has_failure_only_preference(
            &preferences,
            Uuid::new_v4()
        ));
    }

    #[test]
    fn paloma_command_error_response_preserves_usage_errors() {
        assert_eq!(
            paloma_command_error_response("Usage: /approve <answer>"),
            "Usage: /approve <answer>"
        );
        assert_eq!(
            paloma_command_error_response("ambiguous_digest_reply"),
            "That digest covers multiple missions. Use /summary <mission> or /send <mission> <message>."
        );
        assert_eq!(
            paloma_command_error_response("database unavailable"),
            "I couldn't read mission status right now."
        );
    }

    #[test]
    fn paloma_alert_kind_covers_attention_states() {
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::Active),
            Some("mission_long_running")
        );
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::Completed),
            Some("mission_completed")
        );
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::AwaitingUser),
            Some("mission_awaiting_user")
        );
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::Failed),
            Some("mission_failed")
        );
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::Blocked),
            Some("mission_blocked")
        );
        assert_eq!(
            paloma_alert_kind_for_status(MissionStatus::NotFeasible),
            Some("mission_not_feasible")
        );
    }

    #[test]
    fn paloma_alert_body_includes_latest_attention_context() {
        let mission = test_mission("Checkout fix", MissionStatus::Active);
        let events = vec![StoredEvent {
            id: 1,
            mission_id: mission.id,
            sequence: 1,
            event_type: "assistant_message".to_string(),
            timestamp: "2026-05-20T00:01:00Z".to_string(),
            event_id: None,
            tool_call_id: None,
            tool_name: None,
            content: "Please confirm the deploy window.".to_string(),
            metadata: serde_json::json!({}),
        }];

        let body = paloma_alert_body(&mission, &events);

        assert!(body.contains("still running"));
        assert!(body.contains("Please confirm the deploy window"));
    }

    #[test]
    fn paloma_alert_digest_coalesces_bursts_without_repeating_titles() {
        let alerts = vec![
            test_alert(
                "Inventory",
                "Inventory completed.\n\nLatest: Wrote the report.",
                "low",
                "2026-05-20T00:02:00Z",
            ),
            test_alert(
                "Checkout fix",
                "Checkout fix is waiting for your input.\n\nLatest: Please confirm the deploy window.",
                "high",
                "2026-05-20T00:01:00Z",
            ),
        ];

        let body = paloma_alert_digest_text(&alerts);

        assert!(body.starts_with("1 mission update needs attention:"));
        assert!(body.contains("- Checkout fix is waiting for your input. Latest: Please confirm"));
        assert!(body.contains("- Inventory completed. Latest: Wrote the report."));
        assert!(!body.contains("Checkout fix\n\nCheckout fix"));
    }

    #[test]
    fn paloma_alert_digest_overflow_does_not_downgrade_hidden_alerts() {
        let alerts = (0..9)
            .map(|idx| {
                test_alert(
                    &format!("Mission {idx}"),
                    &format!("Mission {idx} failed."),
                    "high",
                    &format!("2026-05-20T00:{idx:02}:00Z"),
                )
            })
            .collect::<Vec<_>>();

        let body = paloma_alert_digest_text(&alerts);

        assert!(body.contains("- 1 more update"));
        assert!(!body.contains("lower-priority"));
    }

    #[test]
    fn paloma_single_alert_uses_body_only() {
        let alerts = vec![test_alert(
            "Checkout fix",
            "Checkout fix failed.\n\nLatest: Tests failed.",
            "high",
            "2026-05-20T00:01:00Z",
        )];

        assert_eq!(
            paloma_alert_digest_text(&alerts),
            "Checkout fix failed.\n\nLatest: Tests failed."
        );
    }

    #[test]
    fn paloma_alert_only_surfaces_long_running_quiet_active_missions() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 1, 0, 0).unwrap();

        let mut active = test_mission("Long build", MissionStatus::Active);
        active.created_at = "2026-05-20T00:00:00Z".to_string();
        assert!(paloma_should_alert_long_running_mission_at(
            &active,
            &[],
            now
        ));

        let mut fresh = active.clone();
        fresh.created_at = "2026-05-20T00:45:00Z".to_string();
        assert!(!paloma_should_alert_long_running_mission_at(
            &fresh,
            &[],
            now
        ));

        let recent_user_message = vec![StoredEvent {
            id: 1,
            mission_id: active.id,
            sequence: 1,
            event_type: "user_message".to_string(),
            timestamp: "2026-05-20T00:45:00Z".to_string(),
            event_id: None,
            tool_call_id: None,
            tool_name: None,
            content: "any update?".to_string(),
            metadata: serde_json::json!({}),
        }];
        assert!(!paloma_should_alert_long_running_mission_at(
            &active,
            &recent_user_message,
            now
        ));

        let completed = test_mission("Done", MissionStatus::Completed);
        assert!(!paloma_should_alert_long_running_mission_at(
            &completed,
            &[],
            now
        ));

        let mut assistant = active.clone();
        assistant.mission_mode = MissionMode::Assistant;
        assert!(!paloma_should_alert_long_running_mission_at(
            &assistant,
            &[],
            now
        ));
    }

    #[test]
    fn paloma_alert_status_rules_cover_questions_and_long_running_terminal_states() {
        let now = Utc.with_ymd_and_hms(2026, 5, 20, 1, 0, 0).unwrap();

        let awaiting = test_mission("Needs answer", MissionStatus::AwaitingUser);
        assert!(paloma_should_alert_mission_at(
            &awaiting,
            &[],
            TelegramMissionInterestLevel::Normal,
            now
        ));

        let mut completed = test_mission("Done", MissionStatus::Completed);
        completed.created_at = "2026-05-20T00:00:00Z".to_string();
        assert!(paloma_should_alert_mission_at(
            &completed,
            &[],
            TelegramMissionInterestLevel::Normal,
            now
        ));

        let mut short_completed = completed.clone();
        short_completed.created_at = "2026-05-20T00:45:00Z".to_string();
        assert!(!paloma_should_alert_mission_at(
            &short_completed,
            &[],
            TelegramMissionInterestLevel::Normal,
            now
        ));
        assert!(paloma_should_alert_mission_at(
            &short_completed,
            &[],
            TelegramMissionInterestLevel::High,
            now
        ));

        let mut failed = test_mission("Failed deploy", MissionStatus::Failed);
        failed.created_at = "2026-05-20T00:00:00Z".to_string();
        assert!(paloma_should_alert_mission_at(
            &failed,
            &[],
            TelegramMissionInterestLevel::Normal,
            now
        ));
    }

    #[test]
    fn paloma_alert_event_kind_is_bucketed_and_subscription_can_raise_importance() {
        let mission = test_mission("Checkout fix", MissionStatus::Active);
        let events = Vec::new();
        let first_bucket = Utc.with_ymd_and_hms(2026, 5, 20, 1, 5, 0).unwrap();
        let same_bucket = Utc.with_ymd_and_hms(2026, 5, 20, 1, 29, 0).unwrap();
        let next_bucket = Utc.with_ymd_and_hms(2026, 5, 20, 1, 31, 0).unwrap();

        assert_eq!(
            paloma_alert_event_kind_at(&mission, "mission_long_running", &events, first_bucket),
            paloma_alert_event_kind_at(&mission, "mission_long_running", &events, same_bucket)
        );
        assert_ne!(
            paloma_alert_event_kind_at(&mission, "mission_long_running", &events, first_bucket),
            paloma_alert_event_kind_at(&mission, "mission_long_running", &events, next_bucket)
        );
        assert_eq!(
            paloma_alert_importance_for_mission(&mission, TelegramMissionInterestLevel::Normal),
            "normal"
        );
        assert_eq!(
            paloma_alert_importance_for_mission(&mission, TelegramMissionInterestLevel::High),
            "high"
        );
    }

    #[test]
    fn paloma_alert_event_kind_ignores_non_status_mission_updates() {
        let mut mission = test_mission("Checkout fix", MissionStatus::AwaitingUser);
        mission.updated_at = "2026-05-20T00:20:00Z".to_string();
        let events = vec![
            StoredEvent {
                id: 1,
                mission_id: mission.id,
                sequence: 1,
                event_type: "mission_status_changed".to_string(),
                timestamp: "2026-05-20T00:05:00Z".to_string(),
                event_id: None,
                tool_call_id: None,
                tool_name: None,
                content: String::new(),
                metadata: serde_json::json!({ "status": "awaiting_user" }),
            },
            StoredEvent {
                id: 2,
                mission_id: mission.id,
                sequence: 2,
                event_type: "assistant_message".to_string(),
                timestamp: "2026-05-20T00:15:00Z".to_string(),
                event_id: None,
                tool_call_id: None,
                tool_name: None,
                content: "Still waiting for approval.".to_string(),
                metadata: serde_json::json!({}),
            },
        ];

        assert_eq!(
            paloma_alert_event_kind_at(
                &mission,
                "mission_awaiting_user",
                &events,
                Utc.with_ymd_and_hms(2026, 5, 20, 1, 0, 0).unwrap()
            ),
            "mission_awaiting_user:2026-05-20T00-05-00Z"
        );
    }

    #[test]
    fn mission_label_uses_public_metadata_without_ids() {
        let mission = test_mission("Proof deployment", MissionStatus::Active);
        assert_eq!(mission_label(&mission), "Proof deployment");
    }

    #[test]
    fn extract_telegram_actions_strips_tags_from_visible_response() {
        let content = concat!(
            "<telegram-reminder delay_seconds=\"60\">RAPPEL_TEST_1_OK</telegram-reminder>\n",
            "<telegram-send target=\"title:LFG Labs\">CROSS_CHANNEL_OK</telegram-send>\n",
            "DONE"
        );

        let (actions, visible) = extract_telegram_actions(content);

        assert_eq!(visible, "DONE");
        assert_eq!(
            actions,
            vec![
                TelegramAction {
                    kind: TelegramActionKind::Send,
                    target: "title:LFG Labs".to_string(),
                    delay_seconds: 0,
                    text: "CROSS_CHANNEL_OK".to_string(),
                },
                TelegramAction {
                    kind: TelegramActionKind::Reminder,
                    target: "current".to_string(),
                    delay_seconds: 60,
                    text: "RAPPEL_TEST_1_OK".to_string(),
                },
            ]
        );
    }

    #[test]
    fn extract_telegram_actions_accepts_bracket_syntax() {
        let content = concat!(
            "[telegram-reminder delay_seconds=\"60\"]RAPPEL_TEST_2_OK[/telegram-reminder]\n",
            "DONE"
        );

        let (actions, visible) = extract_telegram_actions(content);

        assert_eq!(visible, "DONE");
        assert_eq!(
            actions,
            vec![TelegramAction {
                kind: TelegramActionKind::Reminder,
                target: "current".to_string(),
                delay_seconds: 60,
                text: "RAPPEL_TEST_2_OK".to_string(),
            }]
        );
    }

    #[test]
    fn telegram_chat_display_title_prefers_title_then_username() {
        let group_chat = Chat {
            id: -100,
            chat_type: "group".to_string(),
            title: Some("LFG Labs".to_string()),
            username: Some("lfg_labs".to_string()),
            first_name: None,
            last_name: None,
        };
        let private_chat = Chat {
            id: 42,
            chat_type: "private".to_string(),
            title: None,
            username: Some("th0rgal".to_string()),
            first_name: Some("Thomas".to_string()),
            last_name: None,
        };

        assert_eq!(
            telegram_chat_display_title(&group_chat).as_deref(),
            Some("LFG Labs")
        );
        assert_eq!(
            telegram_chat_display_title(&private_chat).as_deref(),
            Some("@th0rgal")
        );
    }

    #[test]
    fn workflow_request_delivery_text_adds_reply_instruction_for_groups() {
        let text = workflow_request_delivery_text(
            "Liste des leads et leur contexte ?",
            Some("supergroup"),
        );

        assert!(text.contains("Liste des leads et leur contexte ?"));
        assert!(text.contains("Reply directly to this message"));
    }

    #[test]
    fn workflow_request_delivery_text_keeps_private_dm_requests_clean() {
        let text =
            workflow_request_delivery_text("Can you send me the latest leads?", Some("private"));

        assert_eq!(text, "Can you send me the latest leads?");
    }

    #[test]
    fn workflow_reply_text_preserves_file_only_replies() {
        let text = workflow_reply_text("", Some("[Attached file: report.pdf (application/pdf)]"));

        assert_eq!(text, "[Attached file: report.pdf (application/pdf)]");
    }

    #[test]
    fn workflow_reply_text_appends_file_annotation_to_caption() {
        let text = workflow_reply_text("Here is the report", Some("[Attached file: report.pdf]"));

        assert_eq!(text, "Here is the report\n[Attached file: report.pdf]");
    }

    #[test]
    fn telegram_action_target_matches_title_and_username_variants() {
        assert!(telegram_action_target_matches(
            "title:LFG Labs",
            Some("LFG Labs"),
            Some("lfg_labs")
        ));
        assert!(telegram_action_target_matches(
            "username:lfg_labs",
            Some("LFG Labs"),
            Some("lfg_labs")
        ));
        assert!(telegram_action_target_matches(
            "@lfg_labs",
            Some("LFG Labs"),
            Some("lfg_labs")
        ));
        assert!(telegram_action_target_matches(
            "LFG Labs",
            Some("LFG Labs"),
            Some("lfg_labs")
        ));
        assert!(!telegram_action_target_matches(
            "title:Other",
            Some("LFG Labs"),
            Some("lfg_labs")
        ));
    }

    #[test]
    fn merge_telegram_chat_metadata_backfills_type_from_lookup() {
        let (title, chat_type) = merge_telegram_chat_metadata(
            Some("LFG Labs".to_string()),
            None,
            None,
            Some("@lfg_labs".to_string()),
            Some("supergroup".to_string()),
        );

        assert_eq!(title.as_deref(), Some("@lfg_labs"));
        assert_eq!(chat_type.as_deref(), Some("supergroup"));
    }

    #[test]
    fn merge_telegram_chat_metadata_keeps_stored_type_without_lookup() {
        let (title, chat_type) = merge_telegram_chat_metadata(
            Some("Cached".to_string()),
            Some("Stored".to_string()),
            Some("private".to_string()),
            None,
            None,
        );

        assert_eq!(title.as_deref(), Some("Stored"));
        assert_eq!(chat_type.as_deref(), Some("private"));
    }

    #[test]
    fn sanitize_telegram_visible_text_strips_internal_prefixes() {
        let raw =
            "[Telegram from @th0rgal in chat 1139694048] [Instructions: Do the thing] BETA-99";
        assert_eq!(sanitize_telegram_visible_text(raw), "BETA-99");
    }

    #[test]
    fn extract_structured_memory_captures_fact_from_remember_clause() {
        let entries =
            extract_structured_memory_from_text("Souviens-toi que mon surnom est ORION-5.");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, TelegramStructuredMemoryKind::Fact);
        assert_eq!(entries[0].label.as_deref(), Some("surnom"));
        assert_eq!(entries[0].value, "ORION-5");
    }

    #[test]
    fn extract_structured_memory_falls_back_to_note() {
        let entries = extract_structured_memory_from_text(
            "Remember that I prefer sharp, direct answers in French.",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, TelegramStructuredMemoryKind::Note);
        assert_eq!(entries[0].label, None);
        assert_eq!(entries[0].value, "I prefer sharp, direct answers in French");
    }

    #[test]
    fn extract_structured_memory_captures_preference() {
        let entries = extract_structured_memory_from_text("J'aime les réponses courtes.");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, TelegramStructuredMemoryKind::Preference);
        assert_eq!(entries[0].label, None);
        assert_eq!(entries[0].value, "les réponses courtes");
    }

    #[test]
    fn extract_structured_memory_captures_explicit_notes_and_tasks() {
        let note_entries =
            extract_structured_memory_from_text("Please remember that staging deploys need Alice.");
        assert_eq!(note_entries.len(), 1);
        assert_eq!(note_entries[0].kind, TelegramStructuredMemoryKind::Note);
        assert_eq!(note_entries[0].value, "staging deploys need Alice");

        let task_entries = extract_structured_memory_from_text("Remind me to check CI tomorrow.");
        assert_eq!(task_entries.len(), 1);
        assert_eq!(task_entries[0].kind, TelegramStructuredMemoryKind::Task);
        assert_eq!(task_entries[0].value, "check CI tomorrow");
    }

    #[test]
    fn extract_structured_memory_ignores_follow_up_reply_instruction() {
        let entries = extract_structured_memory_from_text(
            "Souviens-toi que mon surnom est ORION-9. Réponds DONE.",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, TelegramStructuredMemoryKind::Fact);
        assert_eq!(entries[0].label.as_deref(), Some("surnom"));
        assert_eq!(entries[0].value, "ORION-9");
    }

    #[test]
    fn extract_structured_memory_does_not_store_questions() {
        let entries = extract_structured_memory_from_text("Quel est mon surnom ?");
        assert!(entries.is_empty());
    }

    #[test]
    fn user_facts_are_scoped_to_the_sender_when_available() {
        let scope = scope_for_extracted_memory(
            &ExtractedTelegramMemory {
                kind: TelegramStructuredMemoryKind::Fact,
                label: Some("surnom".to_string()),
                value: "ORION-10".to_string(),
            },
            &TelegramMemorySubject {
                user_id: Some(42),
                username: Some("th0rgal".to_string()),
                display_name: Some("@th0rgal".to_string()),
            },
        );

        assert_eq!(scope, TelegramStructuredMemoryScope::User);
    }

    #[test]
    fn structured_memory_context_groups_user_and_chat_scopes() {
        let entries = vec![
            TelegramStructuredMemoryEntry {
                id: Uuid::new_v4(),
                channel_id: Uuid::new_v4(),
                chat_id: 1,
                mission_id: None,
                scope: TelegramStructuredMemoryScope::User,
                kind: TelegramStructuredMemoryKind::Fact,
                label: Some("surnom".to_string()),
                value: "ORION-10".to_string(),
                subject_user_id: Some(42),
                subject_username: Some("th0rgal".to_string()),
                subject_display_name: Some("@th0rgal".to_string()),
                source_message_id: Some(10),
                source_role: "user".to_string(),
                created_at: "2026-04-07T00:00:00Z".to_string(),
                updated_at: "2026-04-07T00:00:00Z".to_string(),
            },
            TelegramStructuredMemoryEntry {
                id: Uuid::new_v4(),
                channel_id: Uuid::new_v4(),
                chat_id: 1,
                mission_id: None,
                scope: TelegramStructuredMemoryScope::Chat,
                kind: TelegramStructuredMemoryKind::Note,
                label: None,
                value: "Projet lié à LFG".to_string(),
                subject_user_id: None,
                subject_username: None,
                subject_display_name: None,
                source_message_id: Some(11),
                source_role: "user".to_string(),
                created_at: "2026-04-07T00:00:00Z".to_string(),
                updated_at: "2026-04-07T00:00:00Z".to_string(),
            },
        ];

        let rendered = format_structured_memory_context(&entries).expect("memory context");
        assert!(rendered.contains("User memory:"));
        assert!(rendered.contains("Chat memory:"));
        assert!(rendered.contains("surnom = ORION-10"));
        assert!(rendered.contains("Projet lié à LFG"));
    }

    #[test]
    fn internal_telegram_action_token_round_trip() {
        let mission_id = Uuid::new_v4();
        std::env::set_var("SANDBOXED_INTERNAL_ACTION_SECRET", "telegram-test-secret");
        let token = build_internal_telegram_action_token(mission_id)
            .expect("token should be derived when internal secret is configured");

        assert!(verify_internal_telegram_action_token(mission_id, &token));
        assert!(!verify_internal_telegram_action_token(
            Uuid::new_v4(),
            &token
        ));

        std::env::remove_var("SANDBOXED_INTERNAL_ACTION_SECRET");
    }
}
