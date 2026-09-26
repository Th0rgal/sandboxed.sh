# Orb native Plan mode

The composer offers `/plan` only for a harness/destination whose adapter can
round-trip native user input. Plan and Goal are mutually exclusive chips; a
multiline draft keeps the chip in the bottom toolbar. The mode survives draft
restoration and appears on the sent user message. Changing to an unsupported
harness keeps the draft and blocks sending instead of silently downgrading it.

`/plan` is the durable wire marker, matching the existing `/goal` contract.
Adapters remove the marker and activate the harness's native mode. There is no
prompt-only planning fallback.

## Native adapters

- **Codex:** app-server `turn/start` with `collaborationMode.mode=plan`, using
  the model resolved by the native thread. `item/tool/requestUserInput` returns
  answers keyed by question ID. Completing a planning turn offers **Request
  changes** or **Implement plan**. Revision starts another Plan turn; approval
  starts a Default turn on the same thread. An active Goal must finish before
  switching that thread to Plan.
- **Claude Code:** SDK stream-json with `--permission-mode plan` and
  `--permission-prompt-tool stdio`. `AskUserQuestion` and `ExitPlanMode` are
  answered through the original `control_request` identity. Other permission
  requests require an explicit decision. Versions that stop after accepting a
  plan receive a continuation on the same session; versions that already
  execute do not receive a duplicate implementation turn.
- **Core Claude transport:** stdout remains on a PTY, while stdin is piped so
  Claude accepts SDK input. Native planning never falls back to argv mode or a
  replacement session when initialization fails.

Local pending requests live in the native process, keyed by run generation;
webview reload recovers them. Stopping or process exit cancels the wait. Cleanup
from an older run cannot cancel the next run. Core uses the existing persisted
interactive tool events and frontend tool hub. Replaying an expired answer
returns `delivered: false`, rather than reporting success or caching it for a
future run. Backend/process restarts do not recreate an expired native request.

## Validation (2026-09-23)

| Harness | Local | Core |
| --- | --- | --- |
| Codex 0.155.1 / Core CLI 0.156.0 | Real native question → revise → approve → file creation passed | Core app-server driver tested against the real local CLI; dev API launch blocked by missing native account identity |
| Claude Code 2.1.278 / Core 2.1.280 | Real native question → approve → file creation passed | Real dev mission question → approve → file creation passed |
| OpenCode 1.15.13 | ACP initialization and Plan selection passed using Orb's `OPENCODE_DB=orb-local.db`; tested session did not expose a native question tool | Plan not advertised by this adapter |
| Grok 1.0.40 | ACP initialization and Plan selection passed; prompt rejected with HTTP 402, usage balance exhausted | Plan not advertised by this adapter |
| Gemini / ChatGPT UI | Not selectable as Orb local harnesses | Plan not advertised |

OpenCode and Grok remain hidden in the Plan menu until their entire native
interaction path is implemented and validated. Their CLI accepting a mode is
not sufficient to advertise the feature.

Automated coverage includes native request identity, one-shot responses,
cancellation, stale-generation cleanup, retained drafts, native capability
visibility, remote undelivered replies, revision without approval, and a WebKit
reload/approval test at desktop and narrow widths. The authenticated smoke tests
are ignored by default and create only temporary test files:

```sh
ORB_PLAN_HARNESS=codex ORB_PLAN_BIN=/path/to/codex ORB_PLAN_REVISE=1 \
  cargo test --manifest-path orb/src-tauri/Cargo.toml native_plan_roundtrip -- --ignored
ORB_PLAN_HARNESS=claudecode ORB_PLAN_BIN=/path/to/claude \
  cargo test --manifest-path orb/src-tauri/Cargo.toml native_plan_roundtrip -- --ignored
CODEX_CLI_PATH=/path/to/codex cargo test --lib core_native_plan_roundtrip -- --ignored
cd orb
pnpm exec vitest run tests/native-interaction.test.tsx tests/goal.test.tsx
pnpm exec playwright test native-interaction.browser.spec.ts
```

Protocol references: [Codex app-server](https://developers.openai.com/codex/app-server/),
[Claude user input](https://platform.claude.com/docs/en/agent-sdk/user-input),
[OpenCode ACP](https://opencode.ai/v2/docs/cli/acp/).

## Local run recovery

Orb reconciles a selected local conversation on reload and before starting a new
turn. The native launch holds a per-mission OS file lock across inspection,
recovery, admission and the running process. Other Orb instances cannot recover
or launch that same mission concurrently. Waiting for a Plan approval keeps the
native run and lock alive; no age-based expiration is used.

Recovery reads the current receipt from the server using the stable machine ID,
then closes only that run ID and generation through the existing client-status
API. It requires the native run to have stopped and checks for surviving
processes in the workspace, including descendants left behind by an Orb crash.
Unknown process state, network failure, authorization failure, transfer conflicts
and stale-generation responses keep the server fence intact. A surviving process
must be stopped before recovery can proceed. No prompt is automatically replayed.

### Background-agent completion (2026-09-24)

A Claude SDK `result` finishes a turn, not necessarily the mission. Orb keeps
stdin and stdout open while non-ambient background tasks remain, using
`background_tasks_changed` snapshots (with task-start/notification fallback for
older CLIs). Otherwise later AskUserQuestion and ExitPlanMode requests fail with
`AbortError: Stream closed`, and the UI loses the eventual plan. Background task
activity now has its own description and lifetime, separate from the Agent tool
call that merely started it.

Validation: 60 native tests, including an intermediate-result → background task
completion → question → plan approval → implementation fixture. A real local
Claude Code 2.1.281 question/approval/implementation roundtrip also passed.

Orb's activity view separates live background work from earlier tool calls.
Task IDs deduplicate snapshots/start/progress events; the spawning tool call is
hidden when linked by `tool_use_id`. A snapshot removal means “Finished”; only a
terminal notification supplies success, failure or cancellation. Details show
reported summaries/commands, and elapsed time is omitted when the start event
was missed. Failed background tasks stay visible; completed work is expandable.
Old native builds remain readable without invented durations or results.

Regression coverage: `agent-activity.test.tsx`, `agent-activity.browser.spec.ts`
(WebKit desktop/mobile), and native `local_stream` lifecycle tests.

## Approved-plan tracking in Orb

After successful delivery of **Implement plan**, Orb records the request ID,
plan text, approval time and transcript boundary in IndexedDB, scoped to the
server, account and mission. A Plan disclosure below the composer restores this
receipt after reload and exposes the approved text. This is local UI history,
not a new mission execution authority or a cross-device approval record.

The latest `TodoWrite`, `todowrite` or `update_plan` snapshot after that boundary
provides steps and their reported status. Missing progress is explicitly shown;
idle execution never implies completion. “Steps reported complete” reflects the
agent's checklist, not independent validation. Old approvals without a receipt
are not inferred from assistant prose. Revisions do not record an approval.
