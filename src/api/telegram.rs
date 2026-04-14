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

use crate::api::control::{AgentEvent, ControlCommand};
use crate::api::mission_store::{
    now_string, MissionMode, MissionStore, TelegramActionExecution, TelegramActionExecutionKind,
    TelegramActionExecutionStatus, TelegramChannel, TelegramChatMission, TelegramConversation,
    TelegramConversationMessage, TelegramConversationMessageDirection, TelegramScheduledMessage,
    TelegramScheduledMessageStatus, TelegramStructuredMemoryEntry, TelegramStructuredMemoryKind,
    TelegramStructuredMemoryScope, TelegramTriggerMode, TelegramWorkflow, TelegramWorkflowEvent,
    TelegramWorkflowKind, TelegramWorkflowStatus,
};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use uuid::Uuid;

/// Shared handle to the Telegram bridge manager.
pub type SharedTelegramBridge = Arc<TelegramBridge>;
type TelegramChatLockMap = HashMap<(Uuid, i64), Arc<Mutex<()>>>;

/// Manages Telegram webhook registrations and channel routing context.
pub struct TelegramBridge {
    /// Routing context for each active channel (needed to forward webhook messages).
    active_channels: RwLock<HashMap<Uuid, ChannelContext>>,
    /// Per-chat locks to serialize auto-create mission resolution.
    chat_locks: RwLock<TelegramChatLockMap>,
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
                let due = match ctx
                    .mission_store
                    .list_due_telegram_scheduled_messages(ctx.channel.id, &now_string(), 32)
                    .await
                {
                    Ok(messages) => messages,
                    Err(err) => {
                        tracing::warn!(
                            channel_id = %ctx.channel.id,
                            "Failed to load due Telegram scheduled messages: {}",
                            err
                        );
                        continue;
                    }
                };

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

                    let base_url = format!("https://api.telegram.org/bot{}", ctx.channel.bot_token);
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
                                .mark_telegram_scheduled_message_sent(message.id, &now_string())
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
                                .mark_telegram_scheduled_message_failed(message.id, &err)
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
    let remember_re = regex::Regex::new(r"(?i)^(?:souviens[- ]toi que|remember that)\s+(.+)$")
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

    if let Some(conversation) = conversation.as_ref() {
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
            &clean_text,
        )
        .await;
    }

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
                    }) if Some(id) == expected_user_message_id && mid == mission_id => {
                        if !queued && !request_started {
                            request_started = true;
                            send_chat_action(http, &base_url, chat_id).await;
                        }
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
                                if let Err(e) = send_file_to_telegram(http, &base_url, chat_id, file).await {
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
        extract_telegram_actions, format_structured_memory_context, markdown_to_telegram_html,
        merge_telegram_chat_metadata, render_telegram_chunk, sanitize_telegram_visible_text,
        scope_for_extracted_memory, telegram_action_target_matches, telegram_chat_display_title,
        truncate_for_telegram, verify_internal_telegram_action_token, workflow_reply_text,
        workflow_request_delivery_text, Chat, ExtractedTelegramMemory, TelegramAction,
        TelegramActionKind, TelegramBridge, TelegramMemorySubject, TelegramStructuredMemoryEntry,
        TelegramStructuredMemoryKind, TelegramStructuredMemoryScope,
    };
    use uuid::Uuid;

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
