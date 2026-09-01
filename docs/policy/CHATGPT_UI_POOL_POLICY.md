# ChatGPT UI pool operational policy

Version: 1.4.0

This document is the human-readable form of the versioned operational policy
for the `chatgpt_ui` backend pool. The machine-readable form lives beside it in
[`chatgpt_ui_pool_policy.json`](chatgpt_ui_pool_policy.json), validated against
[`chatgpt_ui_pool_policy.schema.json`](chatgpt_ui_pool_policy.schema.json) by
`scripts/policy_lint.py` in CI. The linter also cross-checks the numeric limits
below against the runtime source (`src/api/runners/chatgpt_ui/mod.rs`) and
pins every source-of-truth path, so this policy cannot silently drift from live
behavior.

Operator setup and diagnostics live in
[`../CHATGPT_UI_HARNESS.md`](../CHATGPT_UI_HARNESS.md). Hermes-facing operating
guidance lives in `skills/hermes-mission-control/SKILL.md`; its
`policy_version` must match this document's version.

## 1. Capacity: live, never hardcoded

Pool capacity is exactly the number of configured `chatgpt_ui.profile_dirs`
entries (deduplicated, canonicalized). There is no static capacity constant in
policy or code, and none may be introduced in prose: schedulers and assistants
must read the live configuration rather than assuming a fixed slot count.
Each slot is one dedicated browser profile guarded by a cross-process
exclusive file lock. Acquisition prefers the first available slot with no
recorded failure, preserving configured order among equivalent candidates.
A deployment may configure more than four slots; practical concurrency is the
number of usable authenticated profiles and the account's live allowance, not
an implementation limit. Cloned profiles for one account share the same
server-side rate limit. Never point two slots at the same profile directory.
A compatibility-failed slot remains usable, but a clean alternative wins the
next lease. Locked, quarantined, and unavailable slots are never reused; when
none is healthy and available, the runtime waits until a slot recovers or the
mission is cancelled.

New browser launches for one profile pool are spaced by 30 seconds by default.
This permits many multi-hour Pro conversations to overlap while avoiding a
burst of navigation/send requests from every slot at once. The interval is
configurable from 5–300 seconds; lowering it requires observed account evidence,
not merely additional browser slots.

## 2. Read-only Pro lanes: concurrent, disjoint

Concurrent `chatgpt_ui` consultations with `model_override: gpt-5.6-pro` are
allowed **only** as read-only lanes (`writer: false`) and **only** on disjoint
slots (distinct dedicated profiles). The profile lock enforces disjointness at
runtime; policy additionally forbids intentionally queueing two lanes onto the
same profile. A `chatgpt_ui` mission never owns repository writes, PRs, or
coding-worker duties.

Completed missions retain their ChatGPT conversation route. A later turn on
the same mission reopens that exact conversation on its owning profile and
adds the new prompt as a follow-up. If the route or profile can no longer be
proved, the turn fails closed instead of silently starting a new discussion.

## 3. Compatibility failures: retry once, different healthy slot

A failure carrying the `compatibility=chatgpt-ui-v2` diagnostic (the versioned
selector/download contract broke) may be retried **at most once**, and the
retry must run on a **different** slot that is currently healthy (not locked,
not showing auth or rate-limit signals). Never retry on the same slot — if the
UI contract broke for that profile, an immediate identical attempt just burns
allowance. If the retry also fails, escalate to the operator; the driver
contract likely needs updating.

Two distinct profiles reporting compatibility or `transport_unavailable`
failures within 180 seconds are backend-wide evidence, not two independent
unhealthy accounts. The runtime opens a five-minute backend circuit: new turns
remain queued and emit a cooldown activity instead of consuming another
profile. One later successful turn closes the circuit immediately. Capacity
callbacks must treat an open circuit as unavailable capacity and must not
redispatch into it.

One exact ChatGPT “Too many requests” interstitial is conclusive account-wide
evidence and opens a ten-minute circuit immediately. A completion from a turn
that was already running does not close this rate-limit circuit. Cooldown expiry
moves the persistent gate to `probing`, not `available`; a single-flight,
30-second browser probe must verify authenticated Pro composer readiness
without entering or submitting a prompt. New, resumed, and conversation-pinned
turns remain blocked until that probe succeeds, including across a backend
restart. The controller must not redispatch rate-limited work meanwhile.

The runtime preference for a clean alternative supports this policy but does
not authorize a retry by itself. The controller must confirm from live pool
telemetry that a different healthy slot is available before submitting the
single retry.

## 4. Auth failures: never blind-retry

`auth_required` is terminal for the mission and gets zero automatic retries —
a blind retry cannot re-authenticate, wastes a slot, and can trip
anti-automation controls. The affected profile is quarantined for at least
1800 seconds (30 minutes), but cooldown expiry alone is not proof that
authentication was repaired. Its durable state remains `requires_login` until
the health probe follows any saved-account picker and observes authenticated
navigation. A picker by itself is inconclusive. The slot is excluded from new,
resumed, and compatibility-retry work until that positive evidence exists.
Legacy unversioned health verdicts are not positive evidence; only a v2
post-picker probe or a successful runtime turn makes a slot ready.

## 5. Rate limits: wait, do not churn

`rate_limited` gets zero automatic retries. Wait for the account's UI
allowance to recover; sandboxed.sh cannot read or predict subscription quota.
Moving the same request to another slot on the same account does not help and
is not permitted as an automatic response. Hermes must read
`get_chatgpt_ui_pool_status` before any ChatGPT UI dispatch and require
`availability.state == available`.

An undifferentiated `browser_launch` error is a host-level transport failure:
it may mean Playwright or Chromium is unavailable globally, so it must not
quarantine the selected profile. Only a proven profile-local Chromium
singleton conflict is recorded as a slot-local launch failure.

## 6. Writers: never concurrent

At most one writer mission per workspace at any time. Parallelism comes from
read-only lanes and from disjoint workspaces/worktrees, never from two writers
in one workspace. `chatgpt_ui` missions are always non-writers.

## 7. Lean writers: independent validation first

Before a writer mission commits Lean changes (e.g. Verity proofs), the
candidate change must pass validation performed **independently** of the
writer: a distinct validation run (separate mission or reviewer lane) that the
writer does not control. Advice obtained through a read-only Pro lane is input
to validation, never a substitute for it.

## 8. Runtime limits (cross-checked against source)

| Limit | Value |
| --- | --- |
| `timeout_secs` default | 14400 |
| `timeout_secs` accepted range | 30–86400 |
| Artifact files per turn | 8 |
| Artifact bytes per turn | 52428800 (50 MiB) |

## Versioning

The policy is semver-versioned. Any change to the JSON policy, this document,
or the skill's policy section bumps the version in all three places;
`scripts/policy_lint.py` fails CI when they disagree or when the table above
diverges from the runtime constants.
