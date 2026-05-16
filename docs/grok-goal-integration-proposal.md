# Grok Build — `/goal`-equivalent integration proposal

**Status:** proposal, not implemented yet.
**Scope:** mirror the `/goal <objective>` UX the dashboard and iOS app already
expose for Claude Code and Codex, but for the `grok` CLI.

---

## TL;DR

**Grok Build has no native `/goal` slash command.** Reading the bundled docs
under `~/.grok/docs/user-guide/04-slash-commands.md` and inspecting the
`grok 0.1.210` binary confirms it: there is no model-driven "I'm done with
this goal" signal of the kind codex emits as
`thread/goal/updated { status: "complete" }`, and no client-side `/goal`
slash command of the kind Claude Code 2.1.139+ ships.

Grok's closest primitives:

| Primitive | What it is | Why it's not `/goal` |
|---|---|---|
| `--check` (CLI, headless only) | "Append a self-verification loop to the prompt" — appends a structured verify-after-do step to the user prompt. Internal flag name `self_verify` / `SELF_VERIFY` in the binary. | **Single-pass with verification.** Not iterative; the model runs the task, the verification subagent reviews, and the run ends. There is no `update_goal { complete }` signal. |
| `--max-turns N` | Hard cap on agent message count (a "turn" includes every system/user/assistant/tool message; a trivial single-prompt run already burns ~5). | A budget, not a goal definition. The model has no awareness of how close it is to N. |
| `/loop [interval] <prompt>` (TUI slash command) | Scheduler — runs `<prompt>` every N minutes for up to 7 days. | A time-based scheduler, not an objective loop. Maps onto sandboxed.sh's existing **automations** infrastructure, not to native loops. |
| `/plan` (CLI flag + slash command) | Plan mode: read-only research, user reviews, then executes. | A two-phase workflow gate, not a goal loop. |
| `--check` + `--max-turns N` combined | Single pass with verification, hard-capped. | Closest, but still one-shot. |

So **a native `/goal` for Grok is not currently feasible** the way codex's
is — we'd need xAI to ship a goal-mode RPC, or accept a different
semantics.

The recommendation below uses Grok's existing primitives plus a
sandboxed.sh-side iteration loop (matching the existing `native_loops.rs`
adapter pattern) so the user experience matches `/goal` even though the
underlying mechanism is sandboxed.sh-driven, not Grok-driven.

---

## Background: how Claude Code and Codex `/goal` work today

To keep the integration coherent, here's what the existing two adapters do.

### Codex `/goal` — fully harness-native
- `src/backend/codex/mod.rs:128-139` strips `/goal ` server-side via
  `parse_goal_prefix`.
- `src/backend/codex/mod.rs:296-334` routes goal missions to
  `app_server.goal_set(...)` instead of `turn/start`. Codex's app-server
  auto-starts a turn and keeps iterating until the model invokes
  `update_goal { status: "complete" }` (or a token budget is exhausted).
- `src/backend/codex/mod.rs:763-799` translates codex's
  `thread/goal/{updated,cleared}` notifications into
  `ExecutionEvent::GoalIteration` / `ExecutionEvent::GoalStatus`.
- `src/api/mission_runner.rs:13499-13511` forwards those execution events
  into the unified `AgentEvent::Goal*` stream.
- `src/api/native_loop_observer.rs:36-89` watches that stream and
  materializes an `Automation` row + per-iteration `AutomationExecution`
  rows so the dashboard's Automations panel shows them alongside
  scheduled automations.
- `src/backend/native_loops.rs:74-87` registers the codex adapter so
  `find_adapter("codex", "goal")` returns a matching observer.
- `src/api/library.rs:1115-1127` exposes the `/goal` slash command in
  `/api/library/builtin-commands → codex`.

### Claude Code `/goal` — handled inside the CLI
- Claude Code 2.1.139+ has a native `/goal` slash command. `src/api/
  mission_runner.rs:9368-9377` pins the CLI to ≥2.1.140 specifically for
  this reason.
- Sandboxed.sh **does not** strip the `/goal ` prefix for claudecode —
  the message is sent verbatim to `claude --print --output-format
  stream-json …`. The CLI parses the slash command itself.
- `src/backend/native_loops.rs:59-72` registers the
  ClaudeCodeGoal adapter, but in practice claudecode does not (yet) emit
  `ExecutionEvent::Goal*` — only codex does. The adapter is there for
  forward compatibility (and the dashboard slash-command catalog at
  `src/api/library.rs:1023-1109`).

### Where `/goal` events appear in the UI

The dashboard and iOS already render `goal_iteration` and `goal_status`
SSE events:
- Iteration pill ("iter N") above the chat.
- Status pill ("active", "complete", "paused", "budget_limited",
  "aborted") next to it.
- Automation row in the per-mission Automations sheet, with one
  AutomationExecution row per iteration.

So once an adapter emits these events, the entire UI surface is already
wired up.

---

## What I tested locally

`grok` 0.1.210 is installed at `~/.local/bin/grok`. Authenticated with the
OAuth token already cached in `~/.grok/auth.json`.

```bash
# Plain headless run (no --check)
$ grok -p "Say only the word pong." --output-format streaming-json \
       --max-turns 10 --yolo --cwd /tmp/grok-check-test
{"type":"thought","data":"The"}
{"type":"thought","data":" user"} …
{"type":"text","data":"pong"}
{"type":"end","stopReason":"EndTurn","sessionId":"…","requestId":"…"}
```

16 NDJSON lines, terminal `stopReason: EndTurn`.

```bash
# Same with --check
$ grok -p "Say only the word pong." --check --output-format streaming-json \
       --max-turns 20 --yolo --cwd /tmp/grok-check-test
```

446 NDJSON lines — the prompt visibly expands into a long verification
preamble. The terminal event is the same `{"type":"end",
"stopReason":"EndTurn", …}`. Per the docs, `--check` "appends a
self-verification loop to the prompt"; the binary strings show the slash
command template `# /check -- Self-Verification`.

Also observed:
- `--max-turns 1` errors with
  `"max_turns exceeded: limit is 1, but got 5 messages"` — so `max_turns`
  counts every internal message (system + user + assistant + tool calls
  + tool results), not "agent decisions". Any practical budget is ≥10.
- The streaming-json events Grok emits are `text` / `thought` / `tool` /
  `end` — no `goal_iteration` / `goal_status` analogue.
- `/loop` is in the TUI only, not headless; it returns a `scheduler` job
  id, not an interactive loop.

---

## Proposal: three integration modes, pick one

I'd recommend (a). (b) and (c) are listed for completeness; both
deliver less, or duplicate work that already exists.

### (a) Sandboxed.sh-driven `/goal` loop using `--check` + iteration **(recommended)**

The user types `/goal <objective>` in a Grok mission. Sandboxed.sh
detects the prefix, wraps the objective in a structured "until done"
template, and drives the iteration loop itself by re-invoking
`run_grok_turn` until the model produces an "all done" signal or a max
iteration budget is hit.

**Behavior the user sees** — identical to codex `/goal`:
- "iter 1", "iter 2", … pills above the chat.
- A goal-state pill that transitions `active → complete | budget_limited
  | aborted`.
- One Automation row + one AutomationExecution per iteration in the
  Automations sheet.

**Implementation outline (file:line changes):**

1. **Detect the prefix.** Add `parse_goal_prefix` in
   `src/backend/grok/mod.rs` (lift the helper from
   `src/backend/codex/mod.rs:133-139` and re-use the same `/goal ` rule).

2. **New entry point in mission_runner.** Add `run_grok_goal_turn`
   alongside `run_grok_turn` in
   `src/api/mission_runner.rs:12715-12936`. Reuses the existing CLI
   spawn but:
   - Renders the user objective into a goal template (see below).
   - Adds `--check` to the CLI args.
   - Adds `--max-turns 200` (or a configurable limit; default high
     enough that practical missions never hit it).
   - On stream end, parses the final text for a sentinel
     (`<goal_complete/>` or a JSON tail
     `{"goal_status": "complete"}`) — described below.
   - If the sentinel is absent and iteration budget remains, kicks off
     another `run_grok_turn` with `--continue`, keeping the same Grok
     session id so the model retains context. Iteration index
     increments by 1.

3. **Goal template.** Append a system-prompt-override that establishes
   the protocol:
   ```text
   You are operating in goal mode.
   Objective: <user-supplied objective>

   On every turn:
   1. Take concrete steps toward the objective.
   2. Re-check whether the objective is fully achieved.
   3. End the turn with EXACTLY one of these two markers on its own line:
      <goal_complete/>   — when the objective is satisfied.
      <goal_continue/>   — when more work is required.

   If a hard blocker prevents progress, end with:
      <goal_aborted reason="..."/>
   ```
   The sentinel scheme avoids relying on the model to emit a structured
   tool call. Grok already exposes a `--system-prompt-override` flag and
   `--rules` for inline rules. Use `--rules` to append the protocol so
   the agent's primary system prompt stays intact.

4. **Sentinel parsing.** In `run_grok_goal_turn`'s end-of-stream handler
   (next to `final_result` at `mission_runner.rs:12908`):
   ```rust
   let status = parse_goal_sentinel(&final_result);
   // returns one of GoalContinue / GoalComplete / GoalAborted{reason}
   //                 / GoalUnknown (sentinel missing).
   ```
   `GoalUnknown` is treated as `GoalContinue` for the first 2 turns
   (the model often forgets the protocol on turn 1), then as
   `GoalAborted{reason: "no_goal_sentinel"}` so a runaway model can't
   loop forever.

5. **Emit goal events.** Each loop iteration produces:
   ```rust
   ExecutionEvent::GoalIteration {
       iteration: current_iter,
       objective: stripped_user_msg.clone(),
   }
   ```
   (before the turn starts). The terminal status produces:
   ```rust
   ExecutionEvent::GoalStatus {
       status: "active" | "complete" | "aborted" | "budget_limited",
       objective: stripped_user_msg.clone(),
   }
   ```
   `mission_runner.rs:13499-13511` already forwards these into the
   `AgentEvent::Goal*` stream — no change needed there.

6. **Wire into the dispatcher.** In `mission_runner.rs` around the
   `"grok" => { run_grok_turn(…) }` arm (line ~3256), add the same
   `/goal ` prefix check that codex uses (line 3281) and route to
   `run_grok_goal_turn` when present.

7. **Native-loop adapter.** Add `GrokGoal` to
   `src/backend/native_loops.rs:113-115`:
   ```rust
   pub struct GrokGoal;
   impl NativeLoopAdapter for GrokGoal {
       fn harness(&self) -> &'static str { "grok" }
       fn command(&self) -> &'static str { "goal" }
       fn observe(&self, event: &AgentEvent) -> LoopObservation {
           observe_goal_event(event) // shared with codex/claudecode
       }
   }
   ```
   Add it to `registry()` so the `native_loop_observer` materializes
   `Automation` + `AutomationExecution` rows for grok goal missions too.

8. **Slash-command catalog.** Add a `grok` field to
   `BuiltinCommandsResponse` in `src/api/library.rs:949-961` and append:
   ```rust
   let grok_commands = vec![CommandSummary {
       name: "goal".to_string(),
       description: Some("Loop until the objective is achieved \
                          (sandboxed.sh-driven; uses Grok --check)".into()),
       path: "builtin-grok".to_string(),
       params: vec![CommandParam {
           name: "objective".to_string(),
           required: true,
           description: Some("What the agent should keep iterating on \
                              until done".into()),
       }],
   }];
   ```
   And expose it on the JSON wire — both dashboard and iOS already
   render `BuiltinCommandsResponse.<backend>` fields, so adding `grok`
   here surfaces the command in both clients' slash menus automatically
   once the iOS `BuiltinCommandsResponse` decoder gains a `grok: [SlashCommand]?` field (3-line change in `ios_dashboard/SandboxedDashboard/Models/Backend.swift:140-160`).

9. **Tests.**
   - `parse_goal_prefix`: lift the codex tests verbatim (the rule is the
     same).
   - `parse_goal_sentinel`: round-trip the three forms + the
     "missing sentinel" → `GoalUnknown` case.
   - `run_grok_goal_turn` smoke test against a stub CLI (already done
     for codex via the same pattern).

**Risk / sharp edges:**
- **Token cost.** `--check` roughly tripled the token spend in my local
  test for a trivial prompt. We should expose a `grok_goal_use_check`
  config (default true) so the operator can disable it.
- **Model compliance with the sentinel.** Grok models tend to follow
  protocol rules well, but the failure mode (no sentinel → eventual
  `aborted`) is graceful. Mention in the user-facing description that
  Grok's goal mode is sandboxed.sh-driven and may be less precise than
  codex's harness-native goal mode.
- **Session continuity.** `--continue` resumes the most recent session
  in the cwd. Multiple parallel grok missions in the same workspace
  would collide. Use the explicit `--session-id` we already track
  (`mission_runner.rs:12747-12750`) to scope each goal loop to its
  mission.
- **Cancellation.** The existing `cancel: CancellationToken` plumbing
  in `run_grok_turn` (line 12824-12832) covers this for free — break
  out of the iteration loop on cancellation.

**Lines of code:** ~200–300 in `src/backend/grok/mod.rs` +
`src/api/mission_runner.rs` + `src/backend/native_loops.rs` +
`src/api/library.rs`, plus tests. Plus 3–5 lines in
`ios_dashboard/SandboxedDashboard/Models/Backend.swift` to decode the
new `grok` field in `BuiltinCommandsResponse`.

---

### (b) Single-shot `--check` mode (passthrough, no iteration)

Map `/goal <objective>` → run `grok -p "<objective>" --check
--max-turns N`, **exactly once**, and let the model's internal verifier
decide when to stop.

- **Pro:** trivial integration (~30 lines: strip prefix, append flags).
  No fake iteration events, no sentinel-parsing brittleness.
- **Con:** the UI shows zero iterations (the model stops after its
  internal verifier passes). The user gets "Grok ran with verify"
  semantics, not "Grok looped until done" semantics. The whole point of
  `/goal` in the codex/claudecode UI is the iteration pills; this
  loses that. Doesn't justify a separate command name.

Recommend: do **not** ship this as `/goal`. If we want it at all, ship
it as a separate `/check` slash command and document it as
single-shot-with-verify.

---

### (c) Use sandboxed.sh's existing automation as the loop

We already have a turn-based automation that re-fires the same command
on `agent_finished` (`AutomationTrigger::AgentFinished`). A user could
manually create an automation that runs `/follow-up` every turn. This
is what `/loop` does inside Grok's own TUI — a recurring scheduled
job.

- **Pro:** zero new code. Just write a doc page.
- **Con:** the user has to manually create the automation, and there's
  no "is the goal done?" signal — the automation will keep firing until
  the user disables it. Not equivalent to `/goal`.

Recommend: do **not** ship this as `/goal`. Mention it in the docs as a
pre-existing alternative.

---

## Recommended path

**Ship (a).** It's the only option that gives the user the same UX as
codex `/goal`, and it slots into the existing
`native_loops.rs` / `native_loop_observer.rs` / Automations-panel
pipeline without changing anything UI-side beyond the slash-command
catalog entry.

Suggested rollout:
1. Land the parser + `run_grok_goal_turn` skeleton with the sentinel
   protocol behind a flag (`SANDBOXED_SH_ENABLE_GROK_GOAL=1`).
2. Add the slash command to the catalog and the iOS decoder.
3. Try a handful of real missions; tune `max_iterations` (probably 25)
   and `--max-turns` per iteration (probably 60) based on observed
   behavior.
4. Flip the flag on by default once the sentinel-compliance rate looks
   healthy (≥95% in test missions).
5. Add an open question to the xAI side: ask whether they'd add a
   first-class `update_goal` / `thread/goal/*` analogue. If/when they
   do, switch `run_grok_goal_turn` to the native path and keep the
   sentinel as a fallback.

---

## Appendix: source pointers

- Codex `/goal` server-side prefix detection:
  `src/backend/codex/mod.rs:128-139, 296-334`.
- Codex `/goal` event emission:
  `src/backend/codex/mod.rs:763-799`.
- `ExecutionEvent::Goal*` → `AgentEvent::Goal*` translation:
  `src/api/mission_runner.rs:13499-13511, 14005-14017`.
- Native-loop adapter registry:
  `src/backend/native_loops.rs:113-123`.
- Native-loop observer (Automation + AutomationExecution
  materialization): `src/api/native_loop_observer.rs:36-89`.
- Builtin slash-command catalog: `src/api/library.rs:984-1133`.
- Grok backend driver: `src/api/mission_runner.rs:12715-12936`.
- Grok backend module: `src/backend/grok/mod.rs`.
- Grok user-guide source-of-truth (bundled with CLI):
  `~/.grok/docs/user-guide/04-slash-commands.md`,
  `~/.grok/docs/user-guide/13-headless-mode.md`,
  `~/.grok/docs/user-guide/14-agent-mode.md`.

