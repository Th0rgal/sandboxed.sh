# ChatGPT UI pool operational policy

Version: 1.0.0

This document is the human-readable form of the versioned operational policy
for the `chatgpt_ui` backend pool. The machine-readable form lives beside it in
[`chatgpt_ui_pool_policy.json`](chatgpt_ui_pool_policy.json), validated against
[`chatgpt_ui_pool_policy.schema.json`](chatgpt_ui_pool_policy.schema.json) by
`scripts/policy_lint.py` in CI. The linter also cross-checks the numeric limits
below against the runtime source (`src/api/runners/chatgpt_ui.rs`), so this
policy cannot silently drift from live behavior.

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
exclusive file lock; acquisition takes the first free slot in configured order.
If every slot is locked, the correct behavior is to wait or fail fast — never
to reuse a locked profile or submit a duplicate mission against the same slot.

## 2. Read-only Pro lanes: concurrent, disjoint

Concurrent `chatgpt_ui` consultations with `model_override: gpt-5.6-pro` are
allowed **only** as read-only lanes (`writer: false`) and **only** on disjoint
slots (distinct dedicated profiles). The profile lock enforces disjointness at
runtime; policy additionally forbids intentionally queueing two lanes onto the
same profile. A `chatgpt_ui` mission never owns repository writes, PRs, or
coding-worker duties.

## 3. Compatibility failures: retry once, different healthy slot

A failure carrying the `compatibility=chatgpt-ui-v2` diagnostic (the versioned
selector/download contract broke) may be retried **at most once**, and the
retry must run on a **different** slot that is currently healthy (not locked,
not showing auth or rate-limit signals). Never retry on the same slot — if the
UI contract broke for that profile, an immediate identical attempt just burns
allowance. If the retry also fails, escalate to the operator; the driver
contract likely needs updating.

## 4. Auth failures: never blind-retry

`auth_required` is terminal until an operator re-provisions login
interactively in the affected profile. Zero automatic retries — a blind retry
cannot re-authenticate, wastes a slot, and can trip anti-automation controls.
The same applies per-slot: an auth-failed slot is unhealthy and must not be
selected for compatibility retries until re-provisioned.

## 5. Rate limits: wait, do not churn

`rate_limited` gets zero automatic retries. Wait for the account's UI
allowance to recover; sandboxed.sh cannot read or predict subscription quota.
Moving the same request to another slot on the same account does not help and
is not permitted as an automatic response.

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
