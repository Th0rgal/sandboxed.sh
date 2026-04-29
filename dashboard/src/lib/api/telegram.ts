/**
 * Telegram Channels API - manage Telegram bot integrations for assistant missions.
 */

import { apiGet, apiPost, apiPatch, apiDel } from "./core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type TelegramTriggerMode = "mention_or_dm" | "bot_mention" | "reply" | "direct_message" | "always";

export interface TelegramChannel {
  id: string;
  mission_id: string;
  bot_username: string | null;
  allowed_chat_ids: number[];
  trigger_mode: TelegramTriggerMode;
  active: boolean;
  instructions: string | null;
  auto_create_missions: boolean;
  default_backend: string | null;
  default_model_override: string | null;
  default_model_effort: string | null;
  default_workspace_id: string | null;
  default_config_profile: string | null;
  default_agent: string | null;
  created_at: string;
  updated_at: string;
}

export interface TelegramChatMission {
  id: string;
  channel_id: string;
  chat_id: number;
  mission_id: string;
  chat_title: string | null;
  created_at: string;
}

export type TelegramScheduledMessageStatus = "pending" | "sent" | "failed";

export interface TelegramScheduledMessage {
  id: string;
  channel_id: string;
  source_mission_id: string | null;
  chat_id: number;
  chat_title: string | null;
  text: string;
  send_at: string;
  sent_at: string | null;
  status: TelegramScheduledMessageStatus;
  last_error: string | null;
  created_at: string;
}

export type TelegramStructuredMemoryKind = "fact" | "note" | "task" | "preference";
export type TelegramStructuredMemoryScope = "chat" | "user" | "channel";

export interface TelegramStructuredMemoryEntry {
  id: string;
  channel_id: string;
  chat_id: number;
  mission_id: string | null;
  scope: TelegramStructuredMemoryScope;
  kind: TelegramStructuredMemoryKind;
  label: string | null;
  value: string;
  subject_user_id: number | null;
  subject_username: string | null;
  subject_display_name: string | null;
  source_message_id: number | null;
  source_role: string;
  created_at: string;
  updated_at: string;
}

export interface TelegramStructuredMemorySearchHit {
  entry: TelegramStructuredMemoryEntry;
  score: number;
  matched_terms: string[];
  reasons: string[];
}

export type TelegramActionExecutionStatus = "pending" | "sent" | "failed";
export type TelegramActionExecutionKind = "send" | "reminder";

export interface TelegramActionExecution {
  id: string;
  channel_id: string;
  source_mission_id: string | null;
  source_chat_id: number | null;
  target_chat_id: number;
  target_chat_title: string | null;
  action_kind: TelegramActionExecutionKind;
  target_kind: string;
  target_value: string;
  text: string;
  delay_seconds: number;
  scheduled_message_id: string | null;
  status: TelegramActionExecutionStatus;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

export type TelegramActionTarget =
  | { kind: "current" }
  | { kind: "chat_id"; value: number }
  | { kind: "chat_title"; value: string };

export interface TelegramActionExecutionResult {
  channel_id: string;
  chat_id: number;
  chat_title: string | null;
  scheduled_message_id?: string | null;
  immediate: boolean;
}

export interface CreateTelegramChannelInput {
  bot_token: string;
  bot_username?: string;
  allowed_chat_ids?: number[];
  trigger_mode?: TelegramTriggerMode;
  instructions?: string;
}

export interface CreateTelegramBotInput {
  bot_token: string;
  bot_username?: string;
  allowed_chat_ids?: number[];
  trigger_mode?: TelegramTriggerMode;
  instructions?: string;
  default_backend?: string;
  default_model_override?: string;
  default_model_effort?: string;
  default_workspace_id?: string;
  default_config_profile?: string;
  default_agent?: string;
}

export interface UpdateTelegramChannelInput {
  active?: boolean;
  trigger_mode?: TelegramTriggerMode;
  allowed_chat_ids?: number[];
  instructions?: string;
  default_backend?: string;
  default_model_override?: string;
  default_model_effort?: string;
  default_workspace_id?: string;
  default_config_profile?: string;
  default_agent?: string;
}

// ---------------------------------------------------------------------------
// Legacy per-mission API Functions
// ---------------------------------------------------------------------------

export async function listTelegramChannels(missionId: string): Promise<TelegramChannel[]> {
  return apiGet<TelegramChannel[]>(
    `/api/control/missions/${missionId}/telegram-channels`,
    "Failed to fetch Telegram channels"
  );
}

export async function createTelegramChannel(
  missionId: string,
  input: CreateTelegramChannelInput
): Promise<TelegramChannel> {
  return apiPost<TelegramChannel>(
    `/api/control/missions/${missionId}/telegram-channels`,
    input,
    "Failed to create Telegram channel"
  );
}

export async function updateTelegramChannel(
  channelId: string,
  updates: UpdateTelegramChannelInput
): Promise<TelegramChannel> {
  return apiPatch<TelegramChannel>(
    `/api/control/telegram-channels/${channelId}`,
    updates,
    "Failed to update Telegram channel"
  );
}

export async function deleteTelegramChannel(channelId: string): Promise<void> {
  await apiDel(`/api/control/telegram-channels/${channelId}`, "Failed to delete Telegram channel");
}

// ---------------------------------------------------------------------------
// Standalone Bot API Functions (auto-create missions per chat)
// ---------------------------------------------------------------------------

export async function listTelegramBots(): Promise<TelegramChannel[]> {
  return apiGet<TelegramChannel[]>(
    `/api/control/telegram/bots`,
    "Failed to fetch Telegram bots"
  );
}

export async function createTelegramBot(
  input: CreateTelegramBotInput
): Promise<TelegramChannel> {
  return apiPost<TelegramChannel>(
    `/api/control/telegram/bots`,
    input,
    "Failed to create Telegram bot"
  );
}

export async function listBotChats(botId: string): Promise<TelegramChatMission[]> {
  return apiGet<TelegramChatMission[]>(
    `/api/control/telegram/bots/${botId}/chats`,
    "Failed to fetch bot chats"
  );
}

export async function listBotScheduledMessages(
  botId: string,
  options?: { chat_id?: number; limit?: number }
): Promise<TelegramScheduledMessage[]> {
  const params = new URLSearchParams();
  if (options?.chat_id !== undefined) params.set("chat_id", String(options.chat_id));
  if (options?.limit !== undefined) params.set("limit", String(options.limit));
  const qs = params.toString();
  return apiGet<TelegramScheduledMessage[]>(
    `/api/control/telegram/bots/${botId}/scheduled${qs ? `?${qs}` : ""}`,
    "Failed to fetch scheduled Telegram messages"
  );
}

export async function listBotStructuredMemory(
  botId: string,
  options?: { chat_id?: number; limit?: number; q?: string; subject_user_id?: number }
): Promise<TelegramStructuredMemoryEntry[]> {
  const params = new URLSearchParams();
  if (options?.chat_id !== undefined) params.set("chat_id", String(options.chat_id));
  if (options?.limit !== undefined) params.set("limit", String(options.limit));
  if (options?.q) params.set("q", options.q);
  if (options?.subject_user_id !== undefined) {
    params.set("subject_user_id", String(options.subject_user_id));
  }
  const qs = params.toString();
  return apiGet<TelegramStructuredMemoryEntry[]>(
    `/api/control/telegram/bots/${botId}/memory${qs ? `?${qs}` : ""}`,
    "Failed to fetch Telegram structured memory"
  );
}

export async function searchBotStructuredMemory(
  botId: string,
  options: { q: string; chat_id?: number; limit?: number; subject_user_id?: number }
): Promise<TelegramStructuredMemorySearchHit[]> {
  const params = new URLSearchParams();
  params.set("q", options.q);
  if (options.chat_id !== undefined) params.set("chat_id", String(options.chat_id));
  if (options.limit !== undefined) params.set("limit", String(options.limit));
  if (options.subject_user_id !== undefined) {
    params.set("subject_user_id", String(options.subject_user_id));
  }
  return apiGet<TelegramStructuredMemorySearchHit[]>(
    `/api/control/telegram/bots/${botId}/memory-search?${params.toString()}`,
    "Failed to search Telegram structured memory"
  );
}

export async function listBotActionExecutions(
  botId: string,
  options?: { chat_id?: number; limit?: number }
): Promise<TelegramActionExecution[]> {
  const params = new URLSearchParams();
  if (options?.chat_id !== undefined) params.set("chat_id", String(options.chat_id));
  if (options?.limit !== undefined) params.set("limit", String(options.limit));
  const qs = params.toString();
  return apiGet<TelegramActionExecution[]>(
    `/api/control/telegram/bots/${botId}/actions${qs ? `?${qs}` : ""}`,
    "Failed to fetch Telegram action executions"
  );
}
