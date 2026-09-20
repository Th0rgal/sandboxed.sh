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

Orb sends the selected `backend`, `model_override`, `prompt`, `remote_node_id`,
project and `idempotency_key` to POST `/api/control/missions`. It no longer builds
shell commands or mints/embeds proxy keys. A missing/cordoned/offline selected
node is an explicit error; the selection never falls back to Core. Unsupported
remote harness errors preserve the draft and selection.

The previously deployed raw-command API rejects an omitted remote_command.
Orb translates that response into a clear backend-upgrade message and does not
retry on Core or through Claude. Structured remote harness execution is owned by
backend mission `1e991f8e-d0e5-468e-a51a-5eafaac0b560`; contract confirmation and
backend rollout are separate from browser-mocked acceptance. No live mission or
user goal was retried for this verification.

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
missing nodes, unsupported harnesses, and legacy API rejection. All outbound
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
