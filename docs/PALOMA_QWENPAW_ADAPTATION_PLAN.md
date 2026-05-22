# Paloma / QwenPaw Adaptation Plan

This plan is for evolving Paloma toward the useful parts of QwenPaw while
keeping Sandboxed.sh as the control plane.

## Decision

Do not replace Paloma with a long-lived QwenPaw mission.

Build Paloma as a Sandboxed.sh service and borrow QwenPaw's runtime patterns:

- per-channel/session queueing
- channel abstraction
- named cron/heartbeat jobs
- memory consolidation lifecycle
- explicit tool/capability registry

Do not borrow QwenPaw's proactive loop directly. It is memory/idle driven and
too broad for mission operations. Paloma proactivity should be event and policy
driven.

## Current State

`src/api/telegram.rs` already contains a good first Paloma:

- owner and trusted-friend roles
- `/status`, `/missions`, `/summary`, `/send`, `/approve`
- shared-chat summary restrictions
- natural command normalization
- alert planning for long-running, awaiting-user, completed, failed, blocked,
  interrupted, and not-feasible missions
- quiet period after user messages
- alert dedupe by event kind
- digest formatting
- mute/high-interest feedback
- scheduled Telegram messages
- structured Telegram memory
- workflow/request-reply support

The problem is not missing behavior. The problem is that behavior, policy,
delivery, storage calls, and scheduler loops are mixed into one Telegram module.

Latest `master` was pulled into `paloma-long-running-ios-fixes` on
2026-05-22. The Paloma-specific local changes still apply cleanly and the
focused Telegram/Paloma unit tests pass.

## Target Shape

```text
Telegram webhook
  -> channel adapter
  -> Paloma event queue
  -> Paloma planner
  -> policy engine
  -> decision log
  -> delivery adapter
  -> Telegram

Mission store / runner events
  -> Paloma observer
  -> same planner/policy/delivery path

Optional local satellite
  -> capability registry
  -> remote Paloma tool calls
```

Paloma should own decisions. Telegram should only deliver messages.

LLM components may draft text or classify intent, but they should return
proposals. Sandboxed.sh decides whether to send, suppress, batch, or ask for
confirmation.

This boundary is stricter than QwenPaw's current proactive implementation:
QwenPaw can generate proactive work from idle/memory state and then send via its
channel message API. Paloma should never give a long-lived brain that direct
send path in production.

## Module Split

Start with a mechanical extraction from `telegram.rs`:

```text
src/api/paloma/
  mod.rs
  event.rs        normalized events and reason codes
  policy.rs       deterministic allow/suppress rules
  planner.rs      mission state -> alert proposals
  digest.rs       alert body/digest formatting
  commands.rs     /status, /missions, /summary, /send, /approve
  memory.rs       structured preference/memory helpers
  queue.rs        per-channel/session queue
  decision_log.rs auditable decision records
```

Keep existing Telegram tables at first. Rename/generalize only after the module
boundary is stable.

## Policy Rules

Default Paloma policy:

- Always alert when a mission is awaiting user input.
- Alert long-running active task missions only after the threshold.
- Suppress long-running alerts if the user messaged the mission recently.
- Alert terminal states only for long-running or high-interest missions.
- Do not alert for assistant-mode missions.
- Mute beats every other rule.
- High-interest raises priority but does not bypass safety.
- Batch multiple pending alerts into one digest.
- Never let an LLM directly send proactive Telegram messages.

Useful future rules:

- max one proactive digest per user/window
- active hours / sleep windows
- project-level mute/high-interest
- "only failures for this mission"
- "keep me posted until this is done"
- dashboard view updates briefing cursor

## QwenPaw Patterns To Adapt

### Queue

QwenPaw's `UnifiedQueueManager` uses `(channel_id, session_id, priority)` keys,
on-demand consumers, batch merging, queue metrics, and idle cleanup.

Paloma should implement the same idea in Rust:

```text
QueueKey = { channel, chat/user, optional mission, priority }
```

Use it for:

- inbound Telegram messages
- natural feedback
- alert planning
- scheduled reminders
- workflow replies

Important adaptation: keep per-session serialization, bounded queueing,
cleanup, and metrics, but do not allow separate priority queues for the same
mission to reorder control messages unless the priority rule is explicit and
tested.

### Cron

QwenPaw's cron manager separates job specs, runtime state, history, and
execution. Paloma should replace the single Telegram scheduler loop with named
jobs:

- `paloma_alert_scan`
- `paloma_due_messages`
- `paloma_memory_consolidation`
- `paloma_digest_flush`
- `paloma_stale_recovery`

Each job needs state/history and a single-writer lease in production.

### Memory

QwenPaw's useful pattern is the memory lifecycle:

```text
accumulate -> consolidate -> retrieve -> serve
```

For Paloma:

- accumulate explicit facts/preferences from Telegram and mission feedback
- consolidate duplicates/conflicts periodically
- retrieve preferences during planning
- serve only through deterministic policy gates

Do not copy broad idle proactivity.

### Tools / MCP

QwenPaw exposes built-in tools and MCP per agent. Paloma should expose a smaller
capability registry:

- list/summarize missions
- send message to mission
- schedule reminder
- update notification preference
- request local satellite capability

The future laptop satellite should register capabilities with remote Paloma. It
must not become the canonical Paloma.

## QwenPaw As Brain Prototype

Later, QwenPaw can be tested behind a `PalomaBrain` interface:

```text
classify_message(context) -> intent
draft_digest(context) -> text
extract_preference(message) -> preference proposal
summarize_mission(events) -> summary
```

It must not own:

- Telegram sending
- mission lifecycle
- scheduler source of truth
- durable memory
- dedupe
- alert policy
- secrets

Dev-only experiment:

```text
Paloma event/context
  -> QwenPaw-backed brain
  -> PalomaIntent proposals
  -> decision log
  -> policy engine
  -> optional Telegram send
```

Run this in shadow mode first. Compare QwenPaw proposals against the
deterministic planner, but suppress all QwenPaw-triggered sends until the
decision log proves the proposals are useful and quiet.

## Development Plan

### 1. Shadow Decision Log

Add `paloma_decisions` before changing behavior.

Record:

- event source
- mission id
- user id/channel
- reason code
- proposed action
- allowed/suppressed
- suppression reason
- policy snapshot
- generated text hash or preview

Success gate:

- every alert considered by current code is explainable
- no Telegram behavior change yet

### 2. Extract Paloma Core

Move tested pure functions out of `telegram.rs` without changing behavior.

Success gate:

- existing Telegram/Paloma unit tests pass
- code paths still use Telegram delivery exactly as before

### 3. Add Queue

Introduce Paloma queue with the QwenPaw key idea.

Success gate:

- burst Telegram messages serialize per chat/mission
- unrelated chats/missions can proceed independently
- queue metrics visible in logs/debug endpoint
- queue shutdown is graceful during hotswap/deploy

### 4. Split Scheduler Jobs

Replace the single scheduler loop with named Paloma jobs.

Success gate:

- scheduled messages still claim atomically
- alert scan cannot double-send after deploy/restart
- job history explains last run and failures

### 5. Preference Policy

Promote feedback from text rules to structured policy.

Success gate:

- "mute this" suppresses future routine alerts
- "only tell me if this fails" works
- user can inspect why a message was sent or suppressed

### 6. Memory Consolidation

Add a conservative consolidation job for Paloma preferences and facts.

Success gate:

- no opaque prompt-only memory
- each consolidated rule points back to source messages
- conflicting rules prefer latest explicit user instruction

### 7. Brain Interface

Add pluggable `PalomaBrain`.

Success gate:

- default deterministic/current brain works
- QwenPaw-backed brain can run in dev as shadow/proposal-only
- LLM output never bypasses policy

Initial brain methods should stay narrow:

- `classify_user_message`
- `extract_preference`
- `draft_digest`
- `summarize_mission`

Do not expose arbitrary tool execution through this interface.

### 8. Local Satellite

Add outbound laptop satellite protocol.

Success gate:

- remote Paloma answers when satellite is offline
- satellite registers tools/capabilities when online
- local tools require explicit capability and audit logging

## Testing Plan

Unit tests:

- policy matrix for statuses, interest, quiet windows, mute/high-interest
- digest formatting and overflow
- decision logging
- queue serialization and cleanup
- scheduler job claim/retry behavior

Integration tests:

- synthetic mission lifecycle to alert proposals
- duplicate scheduler instance cannot double-send
- Telegram webhook duplicate update ignored
- dashboard cursor vs Telegram cursor behavior

Live dev Telegram smoke:

- `/status`
- `/missions`
- `/summary`
- reply to alert routes to the right mission
- mute/high-interest feedback
- rapid burst messages
- awaiting-user alert
- long-running alert suppressed after recent user message
- completed mission not mislabeled interrupted
- file caption redaction for shared files
- shared-chat `/summary` allowed only for trusted users in allowlisted chats

Telegram access needed for these tests:

- dev bot token or permission to use the existing Paloma bot in a dev channel
- Thomas owner Telegram user id already allowlisted or provided for dev
- one private DM with the bot for owner-control tests
- one allowlisted shared test chat, ideally with a trusted-friend account
- permission to send burst messages, replies to Paloma alerts, and mute/high
  interest feedback during test windows
- a way to inspect dev server logs/database while tests run

Production canary:

- shadow decision logs first
- one trigger at a time
- low frequency
- inspect `paloma_decisions` before enabling sends

## Open Risks

- Mission status classification may still explain "completed shown as
  interrupted"; verify with event logs from affected missions.
- Current in-process scheduler guard prevents duplicate loops per process, but
  production deploys may still need a DB lease for single-writer behavior.
- QwenPaw proactive behavior is beta and idle/memory driven; use only in dev
  shadow mode until policy gates are proven.
- Generalizing Telegram tables too early could create churn. Extract modules
  first, rename persistence later.
