# Paloma Telegram Roadmap

Long-term roadmap for making Paloma a quiet Telegram operations aide for
Sandboxed.sh.

The goal is not to build a second dashboard in chat. Telegram should be the
smallest useful surface for awareness, steering, and timely answers while the
dashboard remains the full control plane.

## Product North Star

Paloma should feel like a trusted operations aide:

- She tells Thomas what changed since he last checked.
- She alerts Thomas only when something matters.
- She lets Thomas steer long-running agents from Telegram.
- She can answer or help Benjamin in shared chats, but never expose secrets or
  grant mission control to anyone except Thomas.
- She learns notification preferences from direct feedback.
- She handles useful media without turning Telegram into a noisy file dump.

## User Model

### Thomas

Owner. Full control from DM only.

Thomas can:

- Check status across missions.
- See what changed since his last Telegram or dashboard session.
- Steer existing missions.
- Start small worker missions.
- Answer agent questions from Telegram.
- Receive proactive alerts.
- Teach Paloma alert preferences.

### Benjamin

Trusted friend/helper. Limited interaction in shared chats.

Benjamin can:

- Ask high-level questions in allowed shared chats.
- Receive safe summaries when Paloma decides it is useful.
- Trigger helpful context from Paloma when the answer saves Thomas time.

Benjamin cannot:

- DM Paloma to control Thomas's missions.
- Start, stop, steer, or inspect private missions.
- Access secrets, raw logs, private prompts, file paths, credentials, or
  sensitive workspace details.

### Everyone Else

Ignored by default unless explicitly allowed.

## Minimal Command Set

Keep the command surface intentionally small:

| Command | Purpose |
| --- | --- |
| `/status` | Delta summary since Thomas last checked. |
| `/missions` | Compact list of active/interested missions. |
| `/summary` | Succinct summary of one mission or the current situation. |
| `/send` | Send a steering message to a mission or selected agent. |
| `/approve` | Answer agent questions/options from Telegram. |

Natural replies to Paloma alerts should work whenever possible, so commands are
fallbacks rather than the main UX.

## Core Concepts

### Last-Seen Cursors

Paloma needs per-user cursors, not just message history.

Track:

- Last Telegram `/status`.
- Last Telegram alert acknowledged.
- Last dashboard session activity.
- Last dashboard mission viewed.
- Last mission event summarized to Thomas.
- Last digest sent.

This lets `/status` answer the real question: "What changed since I last paid
attention?"

### Mission Interest

Not every mission deserves alerts.

Track interest signals:

- Thomas explicitly subscribed.
- Thomas recently viewed or messaged the mission.
- Mission is long-running.
- Mission is a parent goal mission.
- Mission asks for input.
- Mission has failed, completed, or become blocked/unblocked.

Interest should decay over time unless refreshed by user activity.

### Alert Preferences

Paloma should store explicit notification rules learned from feedback.

Examples:

- "Do not alert me about routine worker completions."
- "Tell me when a proof mission becomes unblocked."
- "Send fewer updates about this mission."
- "Tell me more about deployment failures."
- "Never share raw logs in group chats."

Preference updates should be auditable and editable later.

### Friend-Safe Output

Shared-chat answers must pass a safety filter before sending.

Default deny:

- Secrets and tokens.
- Raw logs.
- Private prompts or instructions.
- File paths.
- Credentials.
- Full mission transcripts.
- Sensitive personal data.
- Unreviewed external links or generated files.

Default allow:

- High-level project status.
- Publicly safe summaries.
- Non-sensitive calendar availability if Thomas explicitly enables it.
- Helpful answers that save Thomas time.

## Architecture

Current Telegram architecture:

```text
Telegram webhook
  -> /api/telegram/webhook/:channel_id
  -> TelegramBridge
  -> ControlCommand::UserMessage
  -> mission runner
  -> AgentEvent stream
  -> Telegram send/edit/media delivery
```

Paloma should extend this with a notification and preference layer:

```text
Mission events + dashboard activity + Telegram messages
  -> Paloma classifier
  -> interest and preference store
  -> alert/digest planner
  -> Telegram delivery
  -> feedback parser
  -> preference updates
```

Do not rebuild an OpenClaw-style gateway. Sandboxed.sh is mission-first. Paloma
should make missions usable from Telegram.

## Data Model

Proposed tables:

### `telegram_users`

- `id`
- `telegram_user_id`
- `username`
- `display_name`
- `role`: `owner`, `trusted_friend`, `observer`, `blocked`
- `created_at`
- `updated_at`

### `telegram_user_cursors`

- `id`
- `telegram_user_id`
- `last_status_at`
- `last_dashboard_seen_at`
- `last_alert_ack_at`
- `last_digest_at`
- `last_seen_event_sequence_by_mission_json`
- `created_at`
- `updated_at`

### `telegram_mission_subscriptions`

- `id`
- `telegram_user_id`
- `mission_id`
- `interest_level`: `muted`, `normal`, `high`
- `reason`
- `expires_at`
- `created_at`
- `updated_at`

### `telegram_alert_preferences`

- `id`
- `telegram_user_id`
- `scope`: `global`, `mission`, `chat`, `event_kind`
- `scope_value`
- `rule_text`
- `enabled`
- `created_from_message_id`
- `created_at`
- `updated_at`

### `telegram_alerts`

- `id`
- `telegram_user_id`
- `mission_id`
- `event_kind`
- `importance`
- `title`
- `body`
- `status`: `pending`, `sent`, `muted`, `failed`, `acknowledged`
- `telegram_message_id`
- `last_error`
- `created_at`
- `sent_at`
- `acknowledged_at`

### `telegram_agent_questions`

- `id`
- `mission_id`
- `telegram_user_id`
- `question`
- `options_json`
- `status`: `pending`, `answered`, `expired`
- `telegram_message_id`
- `answer_text`
- `created_at`
- `answered_at`

## Delivery Rules

### Owner DM

Allowed:

- Full mission control.
- Status summaries.
- Steering.
- Agent questions.
- Worker creation.
- Media and files.

Still protected:

- Secrets should be redacted by default.
- Dangerous actions can be gated later, but the first version should focus on
  answering agent questions rather than broad approval workflows.

### Shared Chats

Allowed only when:

- Thomas or Benjamin is in the chat.
- The chat is allowlisted.
- The message is directly relevant.
- The answer is safe after redaction.
- Paloma's intervention is likely to save time.

Paloma should usually stay silent.

## Alert Policy

Start with simple rules:

Alert Thomas when:

- A mission asks for user input.
- A long-running mission completes.
- A long-running mission fails.
- A mission becomes stuck or repeatedly errors.
- A subscribed mission has meaningful progress.
- A deployment completes or fails.
- A generated media/report artifact is ready.

Do not alert Thomas for:

- Routine worker completions unless subscribed.
- Noisy low-level tool events.
- Every text delta.
- Repeated failures with the same cause after the first alert.

Cadence:

- Immediate for input-needed, failed, completed, or explicitly high-interest
  events.
- Digest after a few hours of quiet.
- Exponential backoff until daily summaries.
- Reset cadence when Thomas replies, views the dashboard, or marks a mission as
  high-interest.

## Status UX

`/status` should return a delta summary.

Example:

```text
3 meaningful changes since you last checked.

1. Verity proof mission unblocked after the worker found the missing invariant.
2. PR #1914 is waiting for your answer about scope.
3. Keel UI worker failed screenshot validation twice; likely CSS overflow.
```

If there are many missions, send a compact text summary first and optionally a
status card image second.

## Missions UX

`/missions` should be compact.

Example:

```text
Active missions

High interest
- Verity proof layer: running, last progress 18m ago
- Keel OS MVP: awaiting user

Other
- 4 workers running
- 2 completed since last status
```

## Summary UX

`/summary` should choose a sensible target:

- If replying to an alert, summarize that mission.
- If a mission is selected in the owner DM, summarize that mission.
- Otherwise summarize high-interest active missions.

Default length should be terse.

## Steering UX

`/send` should support:

```text
/send <mission selector> <message>
/send latest focus on tests and report only blockers
/send verity spawn a small worker to inspect docs
```

Natural replies to alerts should be preferred:

```text
Focus it on tests first.
Ask a small worker to check docs.
Stop this mission.
```

Paloma should confirm the target before sending if ambiguous.

## Approval/Input UX

`/approve` is primarily for answering agent questions.

When a mission waits for input, Paloma should DM:

```text
Verity proof mission needs your input:
Should it pin the merged Verity commit or keep tracking main?

Reply:
1. Pin merged commit
2. Track main
3. Ask agent to decide
```

Thomas can reply with a number or plain text. Paloma forwards the answer into
the mission.

## Media Roadmap

### Phase 1

- Forward mission `shared_files` to Telegram.
- Send images as photos when safe.
- Send reports and non-image files as documents.
- Include captions with origin mission and short context.

### Phase 2

- Treat inbound Telegram files as mission attachments, not just temp paths.
- Persist attachment metadata.
- Show attachments in dashboard mission history.
- Add OCR for images and screenshots.
- Add transcription for voice notes.

### Phase 3

- Generate compact status cards as images.
- Send graph/image summaries for complex mission states.
- Support media bundles when a mission produces multiple artifacts.

Voice is lowest priority.

## Preference Learning

Paloma should parse feedback messages such as:

- "Don't tell me about this again."
- "Tell me more about this kind of thing."
- "Only alert me if this fails."
- "Summarize this daily."
- "Benjamin can see this level of detail."

Implementation should create explicit preference records, then confirm tersely:

```text
Noted. I will mute routine updates for this mission unless it fails or asks for input.
```

Avoid opaque memory. Preference changes should be visible in Settings later.

## Safety Rules

Hard requirements:

- Only Thomas can control missions.
- Benjamin can interact only in allowlisted shared chats.
- No one except Thomas can DM-control Paloma.
- Redact secrets before Telegram delivery.
- Do not send raw logs to shared chats.
- Do not expose private prompts, internal instructions, or file paths to
  friends.
- Treat generated files as private unless explicitly shared.

Future approval gates can cover deploy, push, merge, external messages, spending
money, and sharing files. The first version should not overbuild approvals.

## Implementation Phases

### Phase 0: Confirm Current Baseline

- Verify active production bot registration.
- Verify webhook secret validation.
- Verify `scripts/telegram_user_smoke.py` works from Thomas's Telegram account.
- Verify current inbound text, media download, outbound text, and shared-file
  delivery.
- Document the production DB path clearly. Current production has been observed
  using `missions-dev.db`; either rename it or document why this is intentional.

### Phase 1: Owner Identity and `/status`

- Add Telegram user role storage.
- Mark Thomas as owner.
- Track last-seen cursors.
- Add `/status` command.
- Generate delta summaries from mission events since last cursor.
- Update cursor after successful status delivery.
- Test with Telethon from Thomas's account.

Acceptance:

- `/status` returns only changes since the previous `/status`.
- Dashboard activity can advance or influence the cursor.
- Non-owner DMs cannot access mission status.

### Phase 2: Mission Interest and `/missions`

- Add mission subscriptions and interest scoring.
- Add `/missions` command.
- Rank long-running, recently viewed, parent, awaiting-user, failed, and
  subscribed missions first.
- Support muting and high-interest marking from Telegram replies.

Acceptance:

- `/missions` shows a compact list.
- Muted missions disappear from proactive alerts.
- High-interest missions surface first.

### Phase 3: Proactive Alerts

- Add alert classifier for mission events.
- Add `telegram_alerts` store.
- Add alert deduplication and cooldowns.
- Add exponential digest cadence.
- Deliver input-needed, completed, failed, stuck, and important-progress alerts.
- Parse feedback to update alert preferences.

Acceptance:

- A completed interested mission sends one concise DM alert.
- Repeated identical failures do not spam.
- "Don't tell me about this again" creates a persistent mute rule.
- "Tell me more about this" raises interest/preference.

### Phase 4: Agent Questions and `/approve`

- Detect missions awaiting user input.
- Send question alerts to Thomas.
- Support numbered options and free-text replies.
- Route answer back into the correct mission.
- Add `/approve` fallback command.

Acceptance:

- A mission question appears in Telegram.
- Thomas answers in Telegram.
- The answer reaches the mission.
- The question is marked answered and not re-alerted.

### Phase 5: Steering and Small Workers

- Add `/send`.
- Add natural reply routing for alert threads.
- Support selecting latest/high-interest mission.
- Support creating small worker missions from Telegram when Thomas asks.
- Require confirmation only when the target is ambiguous.

Acceptance:

- Thomas can steer an existing long-running mission from Telegram.
- Thomas can start a small worker from Telegram.
- Benjamin cannot start or steer missions.

### Phase 6: Shared Chat Intelligence

- Add trusted friend role for Benjamin.
- Add allowlisted shared chat behavior.
- Add friend-safe redaction and summary policy.
- Let Paloma answer when directly useful, but stay silent by default.

Acceptance:

- Benjamin can ask a safe high-level question in the shared chat.
- Paloma answers without secrets or control access.
- Random users cannot control or query private state.

### Phase 7: Media

- Persist inbound Telegram attachment metadata.
- Show Telegram attachments in mission history.
- Improve outbound shared-file captions.
- Add status card image generation for complex status.
- Add OCR for images and transcription for voice notes if still useful.

Acceptance:

- Thomas can send an image/file and the mission receives a durable attachment.
- Mission-generated images/files can be sent back to Telegram.
- `/status` can optionally include a compact image card.

### Phase 8: Dashboard Settings

- Add UI for:
  - Telegram users and roles.
  - Allowed chats.
  - Alert preferences.
  - Mission subscriptions.
  - Last delivered alerts.
  - Test-send button.

Acceptance:

- Thomas can inspect and edit Paloma's learned preferences.
- Thomas can see why an alert was sent or muted.

## Testing Strategy

Use three test layers.

### Unit Tests

Cover:

- Command parsing.
- Role checks.
- Redaction.
- Alert classification.
- Preference feedback parsing.
- Delta summary cursor logic.
- Mission interest ranking.

### Local/Backend Integration Tests

Cover:

- SQLite migrations.
- Alert store deduplication.
- Cursor updates.
- Mission event to alert flow.
- Webhook update dedup.
- Agent question routing.

### Live Telegram Smoke Tests

Use `scripts/telegram_user_smoke.py` with a real Telegram user session.

Required environment:

```bash
export TELEGRAM_API_ID=...
export TELEGRAM_API_HASH=...
export TELEGRAM_PHONE=...
export TELEGRAM_CHAT=...
export TELEGRAM_FROM_USER=ana_lfgbot
```

Example:

```bash
python3 scripts/telegram_user_smoke.py \
  --chat "$TELEGRAM_CHAT" \
  --send "/status" \
  --watch-seconds 60 \
  --print-history
```

The production bot token is stored server-side and should not be needed for
Telethon client-side smoke tests. If bot-token API checks are needed, retrieve
only masked metadata or use server-side authenticated control APIs; do not print
tokens in logs.

Each feature phase should include a live smoke:

- Send command from Thomas account.
- Verify Paloma response appears.
- Verify DB state changed correctly.
- Verify non-owner behavior is denied.
- Verify no secrets appear in Telegram output.

## Open Questions

- Should dashboard views update the same cursor as Telegram `/status`, or should
  dashboard and Telegram have separate "briefed" cursors?
- Should Paloma DM Thomas before answering Benjamin in a shared chat when the
  answer depends on private context?
- How should "small worker" defaults be chosen: same workspace as parent,
  current dashboard mission, or Paloma's own control workspace?
- Should status card images be generated by backend code or by an agent tool?

## Suggested First Slice

Build Phase 1 and part of Phase 3:

- Owner role.
- `/status` delta summary.
- Last-seen cursor.
- Input-needed proactive alert.
- Telethon smoke test.

This is the smallest version that changes the daily workflow.

## Simple Goal Prompt

Use this with an agent:

```text
/goal Implement the Paloma Telegram roadmap in docs/PALOMA_TELEGRAM_ROADMAP.md, starting with the smallest useful slice and continuing phase by phase. Keep the UX simple: one bot, Thomas-only mission control, concise /status deltas, proactive important alerts, and safe limited shared-chat behavior for Benjamin. For every completed feature, add focused tests and run live Telegram smoke tests with scripts/telegram_user_smoke.py from my Telegram account when credentials are available. Do not print bot tokens or secrets. Before stopping, audit the roadmap checklist against code, tests, production config, and Telegram smoke results; keep going until each implemented phase is genuinely verified or clearly marked blocked with the exact missing credential/config.
```
