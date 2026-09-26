# Mission launch and cron capability verification

## Client behavior

The new-agent form shows the submitted user prompt and a reduced-motion-aware
Starting status on the selected machine before awaiting fleet validation or
POST. A rejected launch keeps the editable draft and restores focus. A successful
POST seeds the mission view immediately; the mission-list refresh runs in the
background. Submit is guarded while pending. Identical retries reuse their
idempotency key; intentional new launches get a new key after acceptance.

The accepted prompt and selected destination have a guarded session receipt.
History's first real user event replaces the optimistic initial turn rather than
appending a duplicate. Existing empty-history goal missions can display
`/goal ` plus `goal_objective`; this recovers the persisted goal, not an invented
message. Pending, resuming, interrupted, failed and empty completed states are
visible independently of transcript events. The accepted remote selection takes
precedence over a bookkeeping workspace named `host`.

## Remote contract and integration

Orb sends the exact `backend`, `model_override`, `prompt`, `remote_node_id`,
project and `idempotency_key` to the server-owned typed create path. It contains
no client-generated shell, proxy-key provisioning, or fallback machine/harness.
Preflight no longer carries a client-side harness list. Every remote launch
re-reads `GET /api/remote-nodes` and follows its `remote_launch` capability
(`{typed, harnesses, raw_command, proxy_url_configured, optional
requires_proxy_harnesses}`; backend `07b57559` on `fix/orb-native-grok-goal`
advertises `claudecode`, `opencode`, and `grok` once that SHA is deployed).
A harness the server has not advertised, a missing or untyped capability
(older backend), an unset proxy URL for Claude Code/OpenCode (or advertised
`requires_proxy_harnesses`), a failed capability read, and a
missing/cordoned/offline node are all refused before POST while retaining the
draft and selection. Native Grok uses managed OAuth, so `proxy_url_configured=false`
does not block it. Grok is sent unchanged as soon as a backend advertises `grok`,
and never before. Server
validation errors keep the draft and selection. A typed-capable backend that still
answers `remote_command is required` is explained the same way, without a raw
command or local-machine retry.

Contract verified against `fix/orb-native-grok-goal` at `07b57559`,
`docs/REMOTE_NODES.md` and the typed remote admission test in
`src/api/control/dispatch_admission_tests.rs`. There is no capability endpoint
in this checkpoint: the typed POST remains the authority for model/provisioning validation after
the client checks the confirmed harness list.
The server supports Claude Code, OpenCode, and native Grok Build when
`remote_launch.harnesses` includes `grok`. OpenCode with a Grok model is never
substituted for a Grok Build selection.

Existing and newly accepted remote missions use `remote_job.node_id` for
placement and job phase/node state for status. Active/observed means accepted,
not running. Node_state=running confirms running. Ambiguous or unobserved jobs
show that the backend is checking status. Terminal mission status remains
authoritative. No live mission, DGX goal, deployment or provisioning ran here.

Voice integration: Composer's existing `onSend` now accepts an async boolean
result. Preserve its `sending` guard and only-clear-on-success behavior when
integrating `feat/orb-macos-voice`. The dictation insertion and microphone/native
voice implementations are unchanged by this branch.

## Cron status

Unsupported 404/405 responses are shown in one compact child row. The details
explain the backend update and provide Check again. Capability is negatively
cached until explicit check or reconnect. Transient server/network errors keep
cached jobs and have a small inline retry icon; non-retryable errors do not.
The canonical controller and project tree remain rendered.

## Reproduction and limits

Run `pnpm test`, `pnpm build` and `pnpm test:browser` in orb. The launch browser
suite mounts the actual App with intercepted APIs; it tests a POST held for over
one second, a three-second mission-list refresh, request rejection, empty failed
and interrupted history, pending/resuming status, late user-event reconciliation,
missing nodes, server-side unsupported remote harness rejection, and durable remote
job status. All outbound local
launches are mocked; a proxy-key request fails the test.

`benchmarks/launch-benchmark.json` records 12 explicit launches. Timing measures
DOM insertion using MutationObserver, from Enter to optimistic feedback and from
POST response to accepted mission view. It does not measure inference or native
DGX runner startup. Host load affects timings. The suite asserts p95 below 500 ms
and distinct request IDs for separate launches. `launch-timings.json` records the
held-POST case separately.

Final client checks: 37 unit/component tests, 22 browser tests and production
frontend build pass. In the final 12-launch sample, optimistic DOM latency was
0.5 ms median / 1.1 ms p95; accepted-view latency was 2.1 / 4.4 ms. The held-POST
case waited about 1.1 seconds while continuing to show the prompt/status.

## Connection isolation follow-up

Cron loading and capability resets now respect disconnected state. Sidebar reads
capture the connection version and discard responses from older connections;
cron defaults and capability checks follow the same rule. A delayed 401 cannot
clear a newer connection, and repeated logout is idempotent.

Verification: 39 unit/component tests, 28 browser tests, and the frontend build
pass. New cases cover concurrent 401s, logout without subsequent polling until
reconnect, and held old-connection 200/401/404/500 cron responses. The 500 polling
case explicitly checks page visibility and waits for the request count with
`expect.poll`. No native or voice code changed.


## Typed contract integration verification

The blanket interim guard from 73181f46 is removed. The supported remote browser
fixtures use the published server contract: explicitly selected Claude Code
(claude-sonnet-4-6) and OpenCode (xai/grok-4.6) receive Active missions with remote_job and
waiting_remote_job execution metadata. The accepted view opens without waiting
for the slow mission-list refresh, and durable user history replaces the
optimistic prompt without duplication. Grok/Codex rejection tests retain their
exact selection and assert zero POSTs. Both supported-harness requests stay pending
for at least one second with optimistic prompt/status visible and duplicate
submission blocked. Neither request contains commands or credentials.
The old-server test asserts an explicit rejection and no retry/fallback.

55 unit/component tests, 17 targeted launch browser tests, and the production
frontend build pass. These are mocked client contract tests, not proof of DGX
execution. Backend fixture admission tests are present in the referenced server
checkpoint. Actual node startup needs the approved backend rollout and canary.
Screenshots: `test-results/orb-remote-claudecode-accepted.png` and
`test-results/orb-remote-opencode-accepted.png`.


## Goal composer and capability preflight verification

`/goal` has no dedicated create field on the backend: `POST /api/control/missions`
(`CreateMissionRequest`) persists `goal_mode` and `goal_objective` from a prompt of
the form `/goal <objective>` (`parse_goal_objective` / `canonical_goal_message` in
`src/api/control/mod.rs`). Orb keeps that contract. The composer recognises the
same grammar (`/goal` plus whitespace; `/goals …` is chat), shows a compact Goal
tag beside the harness picker while the draft is a goal, refuses an empty
`/goal` with the draft kept, and sends the canonical `/goal <objective>` prompt
with the objective (first line, 42 characters) as the title instead of the raw
slash command. The launch preview, transcript user turn, status line and title bar
render goal turns as a Goal tag plus the exact objective. Stored raw `/goal …`
titles from older clients display as their objective. The empty-history fallback
to the persisted `goal_objective` is unchanged (`initialPrompt`).

The harness menu shows the server-advertised remote support for the selected
machine (`not on DGX Spark`, `no typed remote launch`, `remote support unknown`,
`checking …`); the machine menu lists the advertised harnesses per node, marked
`(last known)` after a failed read. The tag is `role="status"` with an
`aria-label`, is not focusable, and Escape still closes the pickers.

Verification in this checkout: 82 unit/component tests, 23 launch browser tests
plus the existing cron, sidebar, project-picker and transcript browser suites, and
the production frontend build. Browser coverage includes the goal indicator and
canonical POST, Grok accepted once advertised (`screenshots/orb-remote-grok-accepted.png`),
Grok refused while unadvertised (`orb-remote-unsupported.png`, `orb-harness-menu-remote.png`),
missing capability, capability read failure, proxy URL unset, and the persisted
goal fallback (`orb-launch-interrupted.png`). Those screenshots and the composer
views (`orb-goal-composer.png`, `orb-goal-accepted.png`, `orb-goal-accepted-light.png`)
are copied into `screenshots/` because `test-results/` is git-ignored. `benchmarks/goal-composer-timings.json`
measures the per-keystroke cost of the indicator (input event through forced
layout, 60 samples each): plain text 0.4 ms median / 0.6 ms p95, goal draft
0.5 / 0.8 ms, editing an existing goal 0.5 / 0.7 ms. The 12-launch startup
benchmark stayed at 0.9 ms median / 2.6 ms p95 optimistic and 2.3 / 4.8 ms
accepted view; the held-POST case showed the goal preview after 2.7 ms and the
accepted view 4.7 ms after the response. These are mocked client checks; no production backend, DGX node or
native Tauri build was exercised.
