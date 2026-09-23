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
