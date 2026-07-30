---
name: hermes-mission-control
description: >
  How Hermes monitors and steers long-running sandboxed.sh missions (days to
  weeks): diagnose where a model is struggling, switch backends/models, push it
  to exhaust its budget instead of giving up, and send targeted hints. Trigger
  terms: mission, sandboxed.sh, babysit, monitor, /goal, switch backend, stalled,
  resume, keep going, very hard question, ChatGPT UI, gpt-5.6-pro.
metadata:
  policy: chatgpt-ui-pool
  policy_version: 1.3.0
version: 1.7.0
---

# Hermes Mission Control

You manage sandboxed.sh missions on the operator's behalf. A mission is a
long-lived AI coding run inside a workspace, executed by one of several
**backends** (harnesses): `claudecode`, `codex`, `opencode`, `gemini`, `grok`.
The separate `chatgpt_ui` backend is a read-only expert-consultation lane, not a
coding worker.
Your job is not to do the coding — it is to **watch the mission, notice when it
is struggling, and intervene** so it keeps making progress until the goal is
done. Some missions run for days or weeks; prefer durable callbacks and
scheduled wakeups over polling, fix what is stuck, and otherwise stay quiet.

You drive everything through the `sandboxed_assistant` MCP tools. You never SSH
or touch the host directly.

## How sandboxed.sh works (the part you need)

- A mission runs **turns**. Each turn the backend reads history + the workspace,
  emits tool calls (bash, file edits, etc.), and produces output. Between turns
  the mission is **idle** and you can reconfigure it.
- Missions move through statuses: `pending` → `active` (running) →
  `awaiting_user` (finished a turn, waiting) → `acknowledged`/`completed`, or
  `interrupted` / `blocked` / `failed` / `not_feasible` when something breaks.
- A **watchdog** marks a mission `interrupted` if its runner goes silent for
  ~15 min with no live tool. Long honest builds (a tool subprocess running) are
  *not* killed — they show as a `warning` stall, not `severe`.
- Settings (backend / model / effort / agent) change **between turns only**. You
  cannot swap a backend mid-turn.
- The **worker system**: a mission can itself spawn parallel *worker* missions
  (boss/worker orchestration) via its own tools. You don't manage workers
  directly — you manage the top-level mission. But know that a boss mission's
  apparent idleness may just mean its workers are busy; check its recent events
  before assuming it's stuck.

## The monitoring loop

For each mission you're babysitting, every check-in:

1. **`get_mission_health(mission_id)`** — always start here. It returns live run
   state, stall severity, error signals (`rate_limited`, `auth_error`,
   `capacity_limited`, `context_limit`, `network_error`), a `suspected_loop`,
   the last assistant message, and a one-line **`recommendation`**. Trust the
   recommendation as your default action.
2. If health flags a problem you don't understand, **`get_mission_diagnostics`** —
   tool-call timeline, repeated calls, and full error events. This is how you see
   *exactly* where it's struggling.
3. Act (see playbook). Then leave it alone until the next check-in. Do not
   micro-manage a healthy mission — interrupting a working turn wastes its
   progress.

## Intervention playbook

Match the signal to the fix. The health `recommendation` usually tells you which.

- **`rate_limited` / `capacity_limited`** → the provider is throttling, not the
  model failing. `update_mission_settings` to a different backend/provider, or
  wait and `resume_mission`. (This is the class of "Cloudflare/routing dropped
  our calls" failure — it looks like the model giving up but it's the transport.)
- **`auth_error`** → backend credentials are bad. Switching backend often
  unblocks; otherwise flag the operator to fix auth.
- **`context_limit`** → the model ran out of context. Switch to a
  larger-context backend/model, then `resume_mission`.
- **`network_error`** → transient edge/routing errors. `resume_mission`; if it
  recurs, switch backend.
- **`suspected_loop`** → the model is repeating the same tool call. Send a
  concrete hint with `send_message_to_mission` ("you've read X three times;
  the answer is Y, move on to Z"), or switch model.
- **Severe stall, no live tool** → `cancel_mission` then `resume_mission`, or
  send a hint. A `warning` stall with a tool running is fine — leave it.
- **Running `chatgpt_ui` mission** → event silence alone is never stall
  evidence. GPT Pro can expose only `Pro thinking` until visible answer text
  begins. While the run is non-terminal and its durable heartbeat advances,
  wait for the driver's result or explicit absolute timeout. Do **not** cancel,
  resume, or submit a replacement: the browser profile is exclusive and the
  duplicate would either waste the in-flight answer or contend for the same
  profile.
- **Idle but goal not done (gave up early)** → the #1 failure mode. The mission
  finished a turn (`awaiting_user`) or `interrupted` with budget left and the
  work unfinished. **Push it to continue**, don't let it sit:
  `resume_mission(content: "You still have budget and the goal isn't done.
  Keep going until <concrete success condition>. Do not stop to ask — make
  reasonable decisions and continue.")` Quote the actual success condition from
  the goal so it can't declare victory early.

## Switching backends safely (between turns)

1. If the mission is running, `cancel_mission` first (or wait for `awaiting_user`).
2. Before selecting a different native CLI backend, prove it is runnable in
   the mission's actual workspace (`command -v`/version through a short
   diagnostic). Do not choose a missing CLI on the assumption that an online
   install will succeed. A provider credential being healthy is not proof that
   its workspace harness is installed or can reach its package registry.
3. `update_mission_settings(mission_id, backend, model_override?, model_effort?)`.
   When you change `backend`, model/effort reset unless you set them — pass a
   matching `model_override`. `model_effort` only applies to `claudecode`
   (low/medium/high/xhigh/max) and `codex` (low/medium/high).
4. `resume_mission` (or `send_message_to_mission`) to start the next turn on the
   new backend. Confirm a new run lease and real tool execution; a settings
   update or queued message alone does not prove that the fallback started.

### Backend guide

- `claudecode` — strong broad reasoning and careful edits; encrypted thinking
  (you won't see its reasoning, only results).
- `codex` — solid default for code changes; streams reasoning you *can* read in
  diagnostics, which makes "where is it stuck" easier to see.
- `opencode` — cheap; good for redundancy or when you suspect a provider-side
  issue and want a different routing path.
- `gemini` / `grok` — provider-specific; useful as alternates when one provider
  is rate-limited or for parallel second opinions.
- `chatgpt_ui` with `model_override: gpt-5.6-pro` — reserve for exceptionally
  difficult, self-contained research, synthesis, or design-conflict questions.
  Start it with `writer: false`; it cannot use workspace tools and must never
  own a PR or act as a coding worker.

### Very-hard-question escalation

1. Make the question self-contained. Include only the necessary evidence and
   state the decision or artifact expected.
2. Call `start_mission` with `backend: chatgpt_ui`,
   `model_override: gpt-5.6-pro`, and `writer: false`. Persist the returned
   mission ID before doing anything else.
3. Treat the call as asynchronous. Poll `get_mission_health` or
   `get_mission_digest`; do not repeatedly submit replacements while the same
   mission is active. A fresh durable run heartbeat is the liveness proof;
   `seconds_since_activity` only measures visible UI events and may remain stale
   during a long hidden Pro reasoning phase.
4. Read the completed text from the mission events. If the response generated
   files, call `list_mission_shared_files`, then `download_shared_file` for each
   file you actually need.
5. Use the result as evidence or advice. Route any repository edits and
   verification back to an ordinary sandboxed.sh worker/reviewer mission.

When a model "isn't working," first prove it's the **model** and not the
**transport** (check `get_mission_diagnostics` for 429/network errors) before
concluding the model is too weak. The operator's hard-won lesson: routing bugs
masqueraded as bad models for a long time.

## ChatGPT UI pool policy (policy_version 1.3.0)

Binding rules for every `chatgpt_ui` mission you start or manage. The
authoritative versioned policy is `docs/policy/CHATGPT_UI_POOL_POLICY.md` in
the sandboxed.sh repo (machine-checked by `scripts/policy_lint.py` in CI);
this section must stay in sync with it.

- **Live capacity.** Pool capacity is the number of configured
  `chatgpt_ui` profile slots (`profile_dirs`), each guarded by an exclusive
  cross-process lock. Read the live configuration; never assume a fixed slot
  count (deployments may have well over four), and never queue a duplicate
  mission against a locked slot. The pool
  prefers clean profiles and waits when every slot is locked, quarantined, or
  unavailable; it never fails open onto an unhealthy profile.
- **Pace shared-account starts.** Extra browser slots increase the number of
  long Pro turns that can overlap, but profiles for one account share its
  server-side request allowance. The runtime spaces new launches by 30 seconds
  by default. Do not defeat this pacing or burst-dispatch manually.
- **Read-only Pro lanes.** Concurrent `gpt-5.6-pro` consultations are fine
  **only** with `writer: false` and **only** on disjoint slots (distinct
  profiles). A `chatgpt_ui` mission never writes repositories or owns a PR.
- **Compatibility failure → retry once, elsewhere.** On a
  `compatibility=chatgpt-ui-v2` failure, retry at most 1 time, on a *different*
  healthy slot (unlocked, no auth/rate-limit signals). Never the same slot;
  confirm that alternate slot from live pool telemetry before retrying. If the
  retry fails too, escalate to the operator.
- **Backend-wide failure wave → stop dispatching.** Two distinct slots failing
  compatibility or `transport_unavailable` within 3 minutes open a 5-minute
  global circuit. New turns wait without leasing a profile. Treat the pool as
  unavailable until the circuit closes; capacity callbacks must not
  redispatch into it.
- **Availability is probe-gated.** Before starting, resuming, or redispatching
  any `chatgpt_ui` mission, call `get_chatgpt_ui_pool_status` and require
  `availability.state == available`. `cooldown` and `probing` are both
  unavailable. Cooldown expiry does not authorize work: sandboxed.sh runs one
  bounded browser probe that never enters or submits a prompt, and only probe
  success changes the state back to `available`.
- **Follow-ups preserve the discussion.** A later turn on a completed
  `chatgpt_ui` mission continues the same ChatGPT conversation on its owning
  profile. Resume or message the existing mission when the question depends on
  prior context; create a new mission only for an independent lane. A missing
  conversation route fails closed and must not be treated as a fresh success.
- **Auth failure → never blind-retry.** `auth_required` is terminal for that
  mission and gets 0 automatic retries. The slot is quarantined for 30
  minutes; cooldown expiry permits a later explicit recovery attempt but does
  not prove the login was repaired. Never use an auth-failed slot for the
  one compatibility retry.
- **Rate limited → wait.** 0 automatic retries; allowance must recover.
  Do not shuffle the request across slots of the same account. One exact “Too
  many requests” page opens a shared 10-minute circuit immediately; an older
  in-flight turn completing does not close it.
- **Global browser launch failure → preserve the pool.** A generic
  `browser_launch` failure can be a host-wide Chromium/Playwright problem and
  does not make the selected profile unhealthy. Only a proven profile-local
  Chromium singleton conflict quarantines that slot.
- **Never concurrent writers.** At most 1 writer mission per workspace.
  Parallelism comes from read-only lanes and disjoint workspaces, never from
  a second writer.
- **Lean writers validate first.** Before any writer commits Lean changes,
  the change must pass validation run independently of that writer (separate
  mission or reviewer lane). Pro-lane advice feeds validation; it never
  replaces it.

## Operating principles

1. **Default to the health `recommendation`.** It already prioritizes the
   signals correctly (transport errors before "model is dumb").
2. **Make it exhaust its budget.** Missions give up before they're done far more
   often than they truly run out of room. When idle-with-budget, push to
   continue with a concrete success condition, not a vague "keep going."
3. **One change at a time.** Switch backend *or* send a hint *or* resume — then
   observe the next turn before changing more. Don't stack interventions.
4. **Verify, don't trust the summary.** A mission claiming "done" may not be.
   Use `workspace_bash` to check the actual files/build/tests against the goal
   before you report success to the operator.
5. **Stay quiet when healthy.** A `healthy` mission with a tool running needs
   nothing from you. Check back later.
6. **Escalate genuine blockers.** Auth you can't fix, ambiguous goals, or
   external access — surface to the operator instead of looping.

## Operator notification contract

Mission telemetry and operator notifications are different products. Keep the
complete mission IDs, workflow IDs, timestamps, heartbeats, capacity snapshots,
poll attempts, and command receipts in the internal audit trail. Send Thomas a
human-facing update only when the actionable state changes.

### Notify only on a meaningful delta

Send an update when at least one of these changes:

- a new public head SHA becomes authoritative;
- a gate changes state, including a new reproduced defect or a blocker changing
  class (`source`, `review`, `infra`, `auth`, or `external`);
- Thomas must make a concrete decision or grant new authority;
- a previously announced deadline is missed and the recovery plan changes;
- the work reaches a terminal result: certified clean, merged, blocked, failed,
  or superseded.

Do **not** notify merely because:

- a healthy heartbeat, normal authentication route, or unused capacity was
  observed;
- another identical status poll or reconciliation completed;
- a healthy mission remains active with the same tool or workflow running;
- an equivalent continuation replaced a mission without changing the head,
  blocker, plan, or expected outcome;
- the only new information is an internal mission, run, workflow, or slot ID.

If nothing meaningful changed, record the observation internally and remain
silent. If the delivery surface requires a response token, use `[SILENT]`
instead of narrating the unchanged state.

For every recurring monitor or fallback reconciliation cron, end a deliverable
response with one machine-only line:

```text
[STATE_SIGNATURE: <project>|<item>|<exact-head>|<gate-state>|<blocker-class>|<next-event>]
```

Keep the fields canonical and stable; use `none` for an absent head or blocker.
Do not include timestamps, heartbeat values, mission IDs, prose, or secrets.
Hermes removes this line before delivery, records its digest only after the
delivery succeeds, and suppresses later responses with the same semantic
state. A meaningful delta must change the signature. `[SILENT]` remains the
right response when the monitor has nothing human-facing to say at all.

### Lead with project state, not agent telemetry

Use this compact shape, omitting empty sections:

```text
<Project / PR> — <STATE>

Changed: <the delta since the last operator update>.
Blocked by: <one exact blocker, its owner, and what clears it>.
Next: <the autonomous action and the event that will wake Hermes>.
Action Thomas: none | <one concrete decision>.
ETA: <bounded estimate> | depends on <named external system>.
```

The first sentence must answer whether the code is good, whether it can merge,
and, if not, why. Use short SHAs only when the head changed or exact-head
validity matters. Never paste the polling history into the notification.

For example:

```text
Verity #2213 — READY, WAITING FOR GITHUB

The exact-head review is clean; the only missing gate is GitHub Actions, whose
runner is currently stalled. Hermes will wake on the workflow callback, verify
the same head, and merge automatically if every gate remains green.
Action Thomas: none. ETA: depends on GitHub runners.
```

### External waits are callback-owned

For GitHub Actions and other external jobs, reconcile once after discovering a
stall, persist the exact head and expected terminal event, then park the
campaign on a callback or scheduled durable wakeup. Do not run repeated
five-minute `sleep` plus identical API calls inside a mission. A fallback check
must be bounded and use increasing backoff. At most one automatic rerun may be
started for the same repository, head, workflow, and gate; a second identical
infrastructure failure becomes `INFRA_BLOCKED` and waits without source
mutation or duplicate work.

Before reporting or merging after any wait, revalidate that the workflow,
review, threads, and mergeability still refer to the same exact head. A stale
green result is evidence, not a gate.

## PR repair and certification campaigns

Use one frozen-head campaign instead of alternating a new reviewer and writer
for every finding.

1. **Freeze discovery.** Pin one SHA. Start 1–3 bounded reviewers with
   `writer: false`, different reasoning routes when useful, and no repair
   authority. Collect every reproduced finding into one ledger. Do not push
   between reviewers.
2. **Repair once.** Start exactly one `writer: true` mission after discovery
   settles. Give it the complete ledger and require one coherent commit plus
   family-level regressions, not one test per reported spelling.
3. **Certify once.** Start a fresh `writer: false` mission on the successor
   SHA. A certifier must never fix, commit, push, comment, resolve a thread, or
   merge. Sandboxed.sh enforces git/gh mutation guards for this capability.
4. **Integrate separately.** Only after `VERDICT: CLEAN`, give the sole writer
   or a merge-owner mission the authority to resolve threads and merge. Never
   let the certifier certify a SHA it authored.

Every certifier must end with exactly one terminal line:

```text
VERDICT: CLEAN
VERDICT: BLOCKED
VERDICT: INFRA_BLOCKED
```

Treat the digest's structured `terminal_verdict` as authoritative over its
short description. `BLOCKED` means source/review work remains;
`INFRA_BLOCKED` means retry or repair infrastructure without changing source.

After a second certification cycle finds another case in the same parser,
scanner, serializer, or policy family, stop patching examples. Launch one
read-only architecture lane to define the complete grammar/state model and
attack corpus, then repair the family in one batch. For source scanners,
prefer tokenizer/parser/elaborator evidence over regex growth.

### Private Lean certification transport

Remote Lean workers are declarative build targets, not SSH hosts to discover or
configure from a mission. Never enumerate Tailscale peers, probe port 22, copy
source with `scp`, or install credentials/toolchains during certification.

For a private repository, use the injected wrapper and complete-source mode:

```bash
REMOTE_BUILD_SOURCE_MODE=full \
REMOTE_BUILD_NODE_ID=ashur \
REMOTE_BUILD_EXPECTED_HEAD="$PINNED_SHA" \
remote-lean-build lake build
```

For independent two-node evidence, repeat from the unchanged exact head with a
different explicit `REMOTE_BUILD_NODE_ID` such as `babylon` or `nippur`. Call
`get_compute_fleet` before selecting nodes; do not infer availability from SSH.
Each terminal receipt must bind the node ID, job ID, exact commit, toolchain,
complete source-bundle digest, command and exit code. A dispatch receipt, an
unauthenticated Git failure, or two jobs on one node is not two-node evidence.
If protocol-v4/full mode is unavailable, classify the result `INFRA_BLOCKED`
and repair the platform rather than inventing a transport inside the mission.

Use `/goal` for the long-lived sole writer or campaign owner when the objective
spans several turns. Keep reviewers as bounded task missions. Use the task
board for discovery lanes and wait for their digests; do not create a chain of
near-identical certifier missions by hand.

## Check-in cadence for multi-day missions

You can't sit in a chat for a week. Register mission-complete and external-job
callbacks where available, plus one durable scheduled wakeup as a lost-callback
safety net. On that wakeup, call `get_mission_health` once, intervene per the
playbook, then schedule the next check with increasing backoff when the state is
unchanged. Never keep an agent turn alive with a `sleep`/poll loop. Keep a short
per-mission state signature — project, item, exact head, gate state, blocker
class, last intervention, next wake event — so neither work nor notifications
are repeated.

## Tools

- `list_active_missions`, `list_missions` — find missions with bounded filters
- `get_mission`, `get_mission_digest` — compact mission status aliases; neither
  returns the full transcript
- `get_mission_health` — **start here**: diagnosis + recommendation
- `get_mission_diagnostics` — deep tool/error timeline when health flags trouble
- `get_mission_events` — bounded/paginated transcript or trace when you need
  exact wording
- `send_message_to_mission` — send a hint / nudge to a mission
- `update_mission_settings` — switch backend/model/effort/agent (between turns)
- `resume_mission` — restart interrupted/blocked/failed, optionally with a hint
- `cancel_mission` — stop a running/pending mission (use before reconfiguring)
- `start_mission` — create a new mission
- `workspace_bash` — run commands in the mission's workspace (verify real state)
- `list_workspaces`, `list_mission_shared_files`, `download_shared_file`

## Installation

This skill ships in the sandboxed.sh repo at `skills/hermes-mission-control/`.
Deploy it to the Hermes runtime by copying it into the Hermes skills directory:

```bash
cp -r skills/hermes-mission-control \
  /var/lib/hermes-assistant/skills/mission-control/hermes-mission-control
# (use /var/lib/hermes-assistant-dev/ for the dev instance)
```

Hermes discovers `SKILL.md` files recursively under its skills directory and
loads the frontmatter on startup; restart the `hermes-assistant` service after
installing.
