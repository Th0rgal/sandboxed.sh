# Telegram Assistant

Connect a Telegram bot to sandboxed.sh so users can chat with an AI assistant
directly from Telegram.

## Overview

A **Telegram Assistant** is a persistent mission (mode: `assistant`) that
receives messages from a Telegram bot via webhooks and streams responses back.
The assistant has full access to the workspace's tools and library — just like
any other mission.

There are two ways to create a Telegram assistant:

1. **Standalone bot** (recommended) — auto-creates a new mission per Telegram
   chat. Configured from **Settings → Telegram** in the dashboard.
2. **Per-mission channel** — attaches a bot to an existing mission via the API.
   All chats share one mission context.

## Dashboard Setup (Standalone Bot)

1. Open **Settings → Telegram** in the dashboard.
2. Click **Create Bot**.
3. Paste your bot token (from [@BotFather](https://t.me/BotFather)).
4. Choose a backend, model, workspace, and trigger mode.
5. Click **Create**.

The bot is now live. Each Telegram chat automatically gets its own mission.

## API Setup (Per-Mission Channel)

### 1. Create a Telegram Bot

1. Open Telegram and message [@BotFather](https://t.me/BotFather)
2. Send `/newbot` and follow the prompts to choose a name and username
3. Copy the **bot token** (e.g. `123456789:ABCdefGHIjklMNOpqrsTUVwxyz`)

### 2. Create a Mission

Create a mission that will serve as the assistant:

```
POST /api/control/missions
Authorization: Bearer <token>

{
  "title": "My Telegram Assistant"
}
```

Note the returned mission `id`.

### 3. Attach a Telegram Channel

```
POST /api/control/missions/:mission_id/telegram-channels
Authorization: Bearer <token>

{
  "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
  "bot_username": "my_bot",
  "allowed_chat_ids": [12345678],
  "trigger_mode": "mention_or_dm",
  "instructions": "Respond in plain text only. Do not use markdown formatting."
}
```

This will:
- Auto-set the mission to `assistant` mode
- Register a Telegram webhook so the bot receives messages
- Start routing messages to the mission

### 4. Create a Standalone Bot (Auto-Create Missions)

```
POST /api/control/telegram/bots
Authorization: Bearer <token>

{
  "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
  "bot_username": "my_bot",
  "trigger_mode": "mention_or_dm",
  "instructions": "You are a helpful assistant.",
  "default_backend": "claudecode",
  "default_model_override": "claude-sonnet-4-20250514",
  "default_workspace_id": "uuid-of-workspace",
  "default_config_profile": "default"
}
```

Each new Telegram chat will automatically create a dedicated mission using the
specified defaults.

## Fields Reference

### Channel Creation Fields

| Field | Required | Description |
|---|---|---|
| `bot_token` | Yes | Bot token from BotFather |
| `bot_username` | No | Bot username (auto-detected if omitted) |
| `allowed_chat_ids` | No | Restrict to specific chat IDs. Empty = allow all. |
| `trigger_mode` | No | See [Trigger Modes](#trigger-modes). Default: `mention_or_dm` |
| `instructions` | No | System instructions prepended to every message |

### Standalone Bot Additional Fields

| Field | Required | Description |
|---|---|---|
| `default_backend` | No | Backend for auto-created missions (e.g. `claudecode`, `opencode`, `amp`) |
| `default_model_override` | No | Model override for auto-created missions |
| `default_model_effort` | No | Model effort level (`low`, `medium`, `high`) |
| `default_workspace_id` | No | Workspace for auto-created missions |
| `default_config_profile` | No | Config profile from the Library |
| `default_agent` | No | Agent name for auto-created missions |

## Configuration

### Trigger Modes

| Value | Description |
|---|---|
| `mention_or_dm` | Respond to @mentions in groups, replies to bot, or DMs (**default**) |
| `bot_mention` | Only respond when @mentioned in groups |
| `reply` | Only respond to replies to bot messages |
| `direct_message` | Only respond in private (1:1) conversations |
| `always` | Process every message in allowed chats |

### Instructions

The `instructions` field lets you customize the assistant's behavior
per-channel. Common uses:

- `"Respond in plain text only. Do not use markdown formatting."` — Telegram
  doesn't render full markdown
- `"You are a helpful coding assistant. Keep answers concise."` — Set
  personality/scope
- `"Always respond in French."` — Language preference

Instructions are prepended to every incoming message as `[Instructions: ...]`.

### Chat ID Restrictions

Set `allowed_chat_ids` to restrict which Telegram chats can interact with the
bot. Leave empty to allow all chats. You can find a chat's ID by forwarding a
message to [@userinfobot](https://t.me/userinfobot).

## API Reference

### Standalone Bots

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/control/telegram/bots` | List all standalone bots |
| `POST` | `/api/control/telegram/bots` | Create a standalone bot |
| `GET` | `/api/control/telegram/bots/:id/chats` | List chats for a bot |

### Per-Mission Channels

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/control/missions/:id/telegram-channels` | List channels for a mission |
| `POST` | `/api/control/missions/:id/telegram-channels` | Create a channel |
| `PATCH` | `/api/control/telegram-channels/:id` | Update channel settings |
| `POST` | `/api/control/telegram-channels/:id/toggle` | Toggle active/inactive |
| `DELETE` | `/api/control/telegram-channels/:id` | Delete a channel |

### Other

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/control/assistants` | List all assistant-mode missions |
| `POST` | `/api/control/missions/:id/mode` | Set mission mode (`task` or `assistant`) |

## Environment Variables

| Variable | Description |
|---|---|
| `SANDBOXED_PUBLIC_URL` | Public URL for webhook registration (e.g. `https://agent.example.com`). Falls back to `http://{HOST}:{PORT}`. |

## Architecture

```
Telegram → webhook → /api/telegram/webhook/:channel_id
         → TelegramBridge routes to ChannelContext
         → ControlCommand::UserMessage { target_mission_id }
         → MissionRunner (parallel execution)
         → AgentEvent stream
         → TelegramBridge sends response via editMessageText
```

Key design decisions:

- **Parallel execution**: Telegram messages always run in parallel runners,
  never hijacking the main session.
- **Webhook-based**: Uses Telegram's `setWebhook` API (not polling) for lower
  latency.
- **Streaming responses**: The bot sends a typing indicator, then the first text
  chunk as a message, followed by progressive `editMessageText` calls as the AI
  generates more text.
- **Eager boot**: On server startup, Telegram webhooks are re-registered
  automatically.
- **Duplicate token rejection**: Creating a second channel with the same bot
  token is rejected (409 Conflict) to prevent webhook conflicts.
- **Webhook rollback**: If webhook registration fails during channel creation,
  the channel is deleted from the database to avoid inconsistent state.
- **Assistant persistence**: Assistant-mode missions are not auto-completed
  after replies — they stay active for future messages.

## Security

- **Bot token**: Stored in the database. Not returned in API responses (masked
  via `#[serde(skip_serializing)]`).
- **Webhook secret**: Each channel gets a unique webhook secret validated via
  `X-Telegram-Bot-Api-Secret-Token` header.
- **Chat ID filtering**: Use `allowed_chat_ids` to restrict which Telegram
  users/groups can interact with the bot.
- **File size limit**: Telegram file downloads are capped at 50 MB.

## Troubleshooting

**Bot not responding?**
1. Check the channel is active: `GET /api/control/telegram/bots`
2. Toggle the channel off and on to re-register the webhook
3. Check server logs for `boot_from_store` or webhook registration errors
4. Ensure `SANDBOXED_PUBLIC_URL` is set and publicly accessible

**Messages appearing in wrong mission?**
Telegram messages use parallel execution, ensuring they stay in their own
mission context.

**Webhook not registered after restart?**
The server eagerly boots all active Telegram channels on startup. If this fails,
any authenticated API call will trigger lazy boot as a fallback.
