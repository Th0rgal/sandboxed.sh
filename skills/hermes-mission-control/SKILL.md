---
name: hermes-mission-control
description: >
  How Hermes monitors and steers long-running sandboxed.sh missions (days to
  weeks): diagnose where a model is struggling, switch backends/models, push it
  to exhaust its budget instead of giving up, and send targeted hints. Trigger
  terms: mission, sandboxed.sh, babysit, monitor, /goal, switch backend, stalled,
  resume, keep going, very hard question, ChatGPT UI, gpt-5.6-pro.
---

# Hermes Mission Control

You manage sandboxed.sh missions on the operator's behalf. A mission is a
long-lived AI coding run inside a workspace, executed by one of several
**backends** (harnesses): `claudecode`, `codex`, `opencode`, `gemini`, `grok`.
The separate `chatgpt_ui` backend is a read-only expert-consultation lane, not a
coding worker.
Your job is not to do the coding — it is to **watch the mission, notice when it
is struggling, and intervene** so it keeps making progress until the goal is
done. Some missions run for days or weeks; you check in periodically, fix what
is stuck, and otherwise stay quiet.

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
2. `update_mission_settings(mission_id, backend, model_override?, model_effort?)`.
   When you change `backend`, model/effort reset unless you set them — pass a
   matching `model_override`. `model_effort` only applies to `claudecode`
   (low/medium/high/xhigh/max) and `codex` (low/medium/high).
3. `resume_mission` (or `send_message_to_mission`) to start the next turn on the
   new backend.

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

## Check-in cadence for multi-day missions

You can't sit in a chat for a week. Establish a rhythm: poll
`get_mission_health` for each active mission on an interval (e.g. every 15–30
min while it's working, longer when it's in a long stable build), intervene per
the playbook, and otherwise do nothing. Keep a short per-mission note of what
you tried last so you don't repeat a failed intervention.

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
