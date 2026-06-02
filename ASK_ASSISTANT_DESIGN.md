# Ask Assistant — Design

> A fast, read-mostly **sidecar assistant** that rides alongside a running
> mission. It can read the full mission history, run bash in the live workspace,
> and answer questions — **without interrupting the main agent's turn loop or
> queue**. Powered by a configurable fast model (default: Cerebras
> `gpt-oss-120b`).

Status: **M1 backend implemented** (store + client + loop + HTTP routes,
compiles, storage unit-tested). M2–M5 pending.
Owner: Thomas
Related: conversation-anchored snapshot (#477, #482), thinking capture (#481),
Cerebras title fix (#489), `metadata_llm.rs` provider ladder.

---

## 1. Goals & non-goals

### Goals
- Let the operator **chat about what a mission is doing** at any time, in a
  separate lane that never touches the harness lock, the message queue, or the
  mission transcript fed back to the working agent.
- Give the assistant **real capability**: full read+write `bash` in the live
  workspace plus history retrieval.
- **Persistent, browsable Ask threads** per mission, clearable, **never merged
  into mission history** (the working agent cannot see the Ask conversation).
- A **dedicated, configurable Ask model** (separate from the metadata model),
  optimized for *smart + fast + large context*.
- One **quiet "operator-note"** bridge: when Ask **writes** to the workspace, the
  working agent gets a passive heads-up on its next turn.
- Works across web, iOS, and Android (shared API).

### Non-goals (v1)
- No worktree / sandbox-copy mode (usage is read-dominant).
- No destructive-write confirmation gate. Full bash, no friction; a visual
  "agent idle/working" indicator is informational only.
- Ask never **starts** a main-agent turn. Strictly non-interrupting.
- Not every harness *heeds* the operator-note identically — delivery is
  harness-agnostic, faithfulness is best-effort (§9).

---

## 2. Concept & lanes

Two lanes share one workspace and one mission identity, otherwise isolated:

- **Driver lane** — the working agent: `mission_events`, history, harness lock,
  message queue.
- **Co-pilot lane** — the Ask assistant: `ask_threads` / `ask_messages` (separate
  `ask.db`), no lock, no queue, its own response path.

Hard rule: **nothing ever reads `ask_*` rows into the working agent's prompt.**
The only cross-lane bridges are the operator-note (Ask write → working agent,
passive) and "Send to agent" (manual: Ask answer → real composer).

---

## 3. Data model (implemented)

Separate `ask.db` (rusqlite), keyed by `mission_id` (ownership enforced at the
HTTP layer via the user's per-user mission store).

- `ask_threads(id, mission_id, title, model, created_at, updated_at)`
- `ask_messages(id, thread_id, seq, role, content, tool_name, tool_call_id,
  metadata, created_at)` — `role ∈ {user, assistant, tool_call, tool_result}`,
  FK → `ask_threads ON DELETE CASCADE`
- `operator_notes(id, mission_id, body, source_thread_id, created_at,
  flushed_at)` — the M2 bridge buffer

Store: `src/api/ask/store.rs` (`AskStore`). Global singleton via tokio
`OnceCell` at `<working_dir>/.sandboxed-sh/missions/ask.db`.

---

## 4. Backend: the Ask agentic loop (implemented)

`src/api/ask/` — net-new, small, in-process:
- `store.rs` — persistence (above).
- `client.rs` — OpenAI-compatible chat client **with tool-calling** (reuses
  `MetadataLlmConfig`; forwards `reasoning_effort` so reasoning models return
  visible content).
- `mod.rs` — `run_ask_turn`: persist user msg → seed system prompt (recent
  events + summary) → tool loop (≤6 iterations) → persist assistant answer.
- `http.rs` — handlers.

Tools (read-on-demand so the 128K context ceiling never bites):
- `bash(command)` → `WorkspaceExec::output(work_dir, "/bin/bash", ["-lc", cmd])`
  — **full** read+write, live workspace, host or nspawn. No gate.
- `read_history(limit)` → `MissionStore::get_latest_events`.
- `read_file(path)` → bash `cat`.

---

## 5. Model configuration (implemented)

`metadata_llm.rs`:
- Lifted `resolve_provider_api_key` to module scope.
- Added `build_assistant_llm_config()` → prefers Cerebras `gpt-oss-120b`
  (`reasoning_effort=low`), overridable via `ASK_ASSISTANT_MODEL` env; falls back
  to the metadata provider ladder.

Cerebras caps context at ~128K regardless of model — seed recent window +
summary, let the model pull more via grep/`read_history`. (M3 adds a Settings
picker with a `supports_tools` capability flag.)

---

## 6. HTTP API (implemented)

Routes (protected) in `routes.rs`, handlers in `src/api/ask/http.rs`:

```
POST   /api/control/missions/:id/ask                 send (returns final answer + messages)
GET    /api/control/missions/:id/ask/threads         list threads
GET    /api/control/missions/:id/ask/threads/:tid    thread + messages
DELETE /api/control/missions/:id/ask/threads/:tid    clear/delete thread
```

These never call `queue_message` or acquire the harness lock.

> M1 returns the final answer synchronously (JSON). Token-level **SSE streaming**
> (`AgentEvent::Ask*` variants on a separate lane) is folded into M3 when the web
> panel needs it.

---

## 7. Context strategy (the 128K ceiling)

Seed: recent mission events + summary + the thread's prior messages. Then the
model pulls more via `read_history` / `bash grep`. Keeps every request under
128K and scales to arbitrarily long missions.

---

## 8. The quiet operator-note (M2)

Delivery is harness-agnostic; faithfulness is best-effort.

- **Detect** an Ask write by wrapping `bash`: `git status --porcelain` snapshot
  before/after (non-git fallback deferred to M5). On change →
  `enqueue_operator_note`.
- **Deliver** by prepending a `<operator-note>` block into the working agent's
  **next** `user_message` (all backends take the next turn as a single string:
  Claude Code `--` arg, OpenCode prompt file, Codex `send_message_streaming`).
  Centralized helper `prepend_pending_operator_notes(mission_id, user_message)`.
- **Invariant:** notes are passive — flushed only when a turn is already about to
  run. Ask can never wake the working agent.
- **Audit:** a flushed note records an `operator_note` mission event (the *fact*
  of a write touches the mission; the *chat* does not).

Compatibility: all backends *see* the note; Claude Code *heeds* `<…>`-tagged
blocks best.

---

## 9. Frontend — web (M3)

Right-side collapsible **Ask panel**: thread switcher, co-pilot styling (distinct
from the indigo Bot), tool-call rendering, slim composer, informational
"agent idle/working" banner, per-item "ask about this" spark, "Send to agent"
bridge. `Ask` button beside Queue/Stop + a keyboard toggle (not a Cmd+W variant).

## 10. Frontend — iOS / Android (M4)

Same endpoints. iOS: `.sheet` with medium/large detents. Android: bottom sheet.

---

## 11. Milestones

- **M1 — Backend core** ✅ tables + store (unit-tested), Ask loop + tools,
  `build_assistant_llm_config`, routes. Synchronous JSON responses.
- **M2 — Operator-note bridge** — git-porcelain detection +
  `prepend_pending_operator_notes` in all backend turn-prep + audit event.
- **M3 — Web panel** — Ask store + (optional SSE), drawer, picker, spark, bridge.
- **M4 — Mobile** — iOS sheet + Android bottom sheet.
- **M5 — Polish** — per-thread cost, auto-titling, non-git write detection,
  sandbox-copy mode.

---

## 12. Verification

- M1 storage: `cargo test --lib ask::store` (thread/message roundtrip, cascade
  delete, operator-note take-once) — passing.
- M1 end-to-end: deploy to **dev**, then
  `curl -X POST /api/control/missions/<id>/ask -d '{"content":"what is the agent doing?"}'`
  with a Cerebras key configured. (Pending live run.)
