# Paloma / QwenPaw Completion Audit

Date: 2026-05-22

## Objective

Implement the distilled plan in `docs/PALOMA_QWENPAW_ADAPTATION_PLAN.md` and
test every feature using the user's Telegram account.

## Prompt-to-Artifact Checklist

- `PALOMA_QWENPAW_ADAPTATION_PLAN.md`: implemented as the active source plan in
  `docs/`.
- Keep Paloma as a Sandboxed.sh service, not a long-lived QwenPaw mission:
  implemented in the existing Rust API service under `src/api/`.
- Module split:
  `src/api/paloma/{event,channel,policy,planner,digest,commands,memory,queue,decision_log,scheduler,brain,capability,satellite}.rs`.
- Shadow decision log:
  `paloma_decisions` store APIs, SQLite table, Telegram decision writes, and
  `/api/control/paloma/decisions`.
- Extract Paloma core:
  Telegram delegates command parsing, policy/planning, digest formatting,
  queueing, scheduling, decision logging, and memory consolidation to Paloma
  modules while retaining Telegram as the delivery adapter.
- Queue:
  `paloma::queue`, webhook queue integration, per-session lock preservation,
  and `/api/control/paloma/queue`.
- Split scheduler jobs:
  `paloma_scheduler_jobs` table, job claim/finish APIs, named Telegram scheduler
  jobs, and `/api/control/paloma/jobs`.
- Preference policy:
  mute, high-interest, failure-only, `/why`, and feedback routing are covered by
  code paths and unit/live smoke tests.
- Memory consolidation:
  `consolidate_telegram_structured_memory` keeps the latest explicit
  fact/preference and removes stale FTS rows.
- Brain interface:
  `PalomaBrain`, deterministic implementation, and shadow/proposal-only wrapper.
- Local satellite:
  `LocalSatelliteRegistry`, capability request/response records, online checks,
  and audit records.
- Capability registry:
  default audited capabilities for mission listing/summarizing, mission sends,
  reminders, notification preferences, and satellite requests.
- Unit/integration coverage:
  `cargo test paloma --all-targets` and `cargo test --all-targets` pass on the
  current working tree.
- Live Telegram smoke:
  `scripts/paloma_live_smoke.py` exists and passes against the available
  deployed `ana_lfgbot` using the bundled Telethon user session.
- Exact-checkout Telegram verification:
  the handoff zip provides user-account control, not a bot token, local webhook
  tunnel, or authenticated Sandboxed.sh control session. The local checkout is
  therefore verified by Rust tests and the deployed Paloma path is verified by
  live Telegram smoke, but this working tree is not connected to Telegram.

## Implementation Evidence

- Paloma remains a Sandboxed.sh service, not a long-lived QwenPaw mission.
- Core modules were extracted under `src/api/paloma/`:
  - `event.rs`
  - `channel.rs`
  - `policy.rs`
  - `planner.rs`
  - `digest.rs`
  - `commands.rs`
  - `memory.rs`
  - `queue.rs`
  - `decision_log.rs`
  - `scheduler.rs`
  - `brain.rs`
  - `capability.rs`
  - `satellite.rs`
- Telegram now delegates command parsing, alert planning, digest formatting,
  decision logging, queueing, and scheduler state to Paloma modules.
- Paloma has normalized channel address and message envelopes for future
  adapters while Telegram remains the concrete delivery adapter.
- `paloma_decisions` and `paloma_scheduler_jobs` are persisted in SQLite.
- Control endpoints expose:
  - `GET /api/control/paloma/decisions`
  - `GET /api/control/paloma/jobs`
  - `GET /api/control/paloma/queue`
- Named jobs have concrete behavior:
  - `paloma_alert_scan`: plans and creates alert records.
  - `paloma_due_messages`: atomically claims and sends due Telegram messages.
  - `paloma_memory_consolidation`: removes stale explicit memory duplicates and
    cleans memory search rows.
  - `paloma_stale_recovery`: clears stale pending alert delivery errors.
  - `paloma_digest_flush`: sends eligible pending alert digests.
- `PalomaBrain` is proposal-only. Shadow/QwenPaw-backed proposals cannot bypass
  policy or send directly.
- The default Paloma capability surface is modeled:
  - `list_missions`
  - `summarize_mission`
  - `send_message_to_mission`
  - `schedule_reminder`
  - `update_notification_preference`
  - `request_local_satellite_capability`
- Local satellite capability handling requires online registration, explicit
  capability, request/response protocol records, and audit records.
- `scripts/paloma_live_smoke.py` provides an executable Telegram-user smoke
  checklist for DM commands, burst handling, alert feedback, and shared-chat
  checks. It also has `--preflight-only` to verify local Telegram/API readiness
  without connecting to Telegram or sending messages; preflight does not require
  Telethon to be installed.

## Local Verification

Executed on the current working tree:

```text
cargo test paloma --all-targets
```

Result:

```text
46 passed; 0 failed
```

Executed on the current working tree:

```text
cargo test --all-targets
```

Result:

```text
main library: 805 passed; 0 failed; 2 ignored
bin targets: passed
```

Executed on touched Rust files:

```text
rustfmt --edition 2021 --check \
  src/api/control.rs \
  src/api/mission_store/mod.rs \
  src/api/mission_store/sqlite.rs \
  src/api/mod.rs \
  src/api/routes.rs \
  src/api/telegram.rs \
  src/api/paloma/*.rs
```

Result: passed.

Executed on the live-smoke harness:

```text
python3 -m py_compile scripts/paloma_live_smoke.py
```

Result: passed.

Executed dependency-light preflight with site packages disabled:

```text
python3 -S scripts/paloma_live_smoke.py --preflight-only \
  --mission-db /root/.sandboxed-sh/missions/missions-dev.db \
  --api-token "$SANDBOXED_PROXY_SECRET"
```

Result: preflight executed without importing Telethon and reported the expected
missing Telegram/API readiness evidence.

Executed the non-mutating live-smoke preflight against this environment:

```text
python3 scripts/paloma_live_smoke.py --preflight-only --api-token "$SANDBOXED_PROXY_SECRET"
```

Result:

```text
PASS telegram_api_id
PASS telegram_api_hash
PASS telegram_session_file
FAIL local_telegram_channels: count=0
PASS api_health
FAIL api_control_auth: status=401, body='Invalid or expired token'
```

Executed the non-mutating live-smoke preflight with the encrypted local
Telegram env:

```text
dotenvx run -f .env.telegram -fk .env.telegram.keys -- \
  python3 scripts/paloma_live_smoke.py --preflight-only
```

Result:

```text
PASS telegram_api_id
PASS telegram_api_hash
PASS telegram_session_file
FAIL local_telegram_channels: count=0
PASS api_health
FAIL api_control_auth: missing PALOMA_API_TOKEN/--api-token
```

## Live Telegram Verification

The user's Telegram account session was used through Telethon.

Reduced harness run against the available deployed bot `ana_lfgbot` passed:

- `/status`
- `/missions`
- `/summary`
- `/why`
- `/approve` usage guard
- `/send` usage guard
- burst message handling

The latest reduced harness command used the encrypted local Telegram env:

```text
dotenvx run -f .env.telegram -fk .env.telegram.keys -- \
  python3 scripts/paloma_live_smoke.py \
    --dm-chat ana_lfgbot \
    --watch-seconds 45 \
    --skip-shared \
    --skip-alert-feedback
```

Result:

```text
/status: sent 252326, reply 252327
/missions: sent 252328, reply 252329
/summary: sent 252330, reply 252331
/why: sent 252332, reply 252333
/approve usage: sent 252334, reply 252335
/send usage: sent 252336, reply 252337
burst: sent 252338-252340, replies 252341-252343
```

Full harness run against the available deployed bot `ana_lfgbot` passed again
after the handoff zip was revalidated:

- `/status`
- `/missions`
- `/summary`
- `/why`
- `/approve` usage guard
- `/send` usage guard
- burst message handling
- reply-to-alert high-interest feedback
- reply-to-alert failure-only feedback
- reply-to-alert mute feedback
- restore high-interest feedback after mute
- shared-chat plain `/summary` silence
- shared-chat `@ana_lfgbot /summary` silence

The full harness command used:

```text
python3 scripts/paloma_live_smoke.py \
  --dm-chat ana_lfgbot \
  --bot-username ana_lfgbot \
  --watch-seconds 25 \
  --alert-message-id 252182 \
  --shared-chat -1001730152948
```

The latest successful run sent and received these key Telegram message ids:

```text
/status: sent 252298, reply 252299
/missions: sent 252300, reply 252301
/summary: sent 252302, reply 252303
/why: sent 252304, reply 252305
/approve usage: sent 252306, reply 252307
/send usage: sent 252308, reply 252309
burst: sent 252310-252312, replies 252313-252315
alert feedback: sent 252316-252322, replies 252317-252323
shared silence probes: sent 21072-21073, no bot reply
```

## Remaining Gap

The live Telegram verification above exercised the available deployed bot with
the user's Telegram account, not this exact local checkout.

The provided handoff zip was set up and used successfully. It contains:

- Telegram API credentials.
- An authorized Telethon user session.
- A client-side smoke helper.

That is sufficient to act as the Telegram user and validate bot UX from the
client side. It is not sufficient to run this local checkout behind Telegram,
because Sandboxed.sh uses Telegram webhooks and local channel creation requires
a bot token plus a public webhook URL or an authenticated deployed control API.

The local checkout currently has no configured Telegram channel or bot token in
the local mission databases, and the service already listening on
`127.0.0.1:3000` is authenticated and appears to be a separate running
deployment. Therefore, the strict requirement "test every feature using my
Telegram account" is not fully satisfied for this exact working tree.

Current blocker evidence:

```text
sqlite3 /root/.sandboxed-sh/missions/missions-dev.db \
  "SELECT COUNT(*) AS telegram_channel_count FROM telegram_channels;"
```

Result:

```text
0
```

```text
curl -sS http://127.0.0.1:3000/api/health
```

Result:

```text
{"status":"ok","version":"0.12.0","dev_mode":false,"auth_required":true,"auth_mode":"single_tenant",...}
```

Authenticated control calls with the available proxy secret return
`Invalid or expired token`, so the local server cannot be configured or
inspected through the API from this session.

The configured public backend behaves the same way:

```text
curl -sS https://agent-backend.thomas.md/api/health
```

Result:

```text
{"status":"ok","version":"0.12.0","dev_mode":false,"auth_required":true,"auth_mode":"single_tenant",...}
```

Control calls to `https://agent-backend.thomas.md/api/control/...` with the
available proxy secret also return `Invalid or expired token`.

The dashboard stores API auth as `openagent.jwt`; Chrome local/session storage
was inspected for `openagent.jwt`, `openagent.jwt_exp`, backend URLs, and
JWT-shaped values. No usable dashboard JWT was present.

```text
rg -l '[0-9]{7,}:[A-Za-z0-9_-]{20,}' /workspaces/mission-8d49a528 /root/.sandboxed-sh
```

Result: only Telegram documentation files with example tokens matched; no local
bot-token configuration was found.

Completion requires one of:

- running this branch behind the Telegram bot used by the user's account, or
- configuring a dev Telegram bot/channel in this local checkout and pointing its
  webhook at the local server, then running `scripts/paloma_live_smoke.py`
  without skipped checks.

Until that happens, the implementation is locally tested and deployed-bot smoke
tested, but not fully live-verified against this exact checkout.
