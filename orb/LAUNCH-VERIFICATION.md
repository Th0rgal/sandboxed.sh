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

The supported local create path sends the exact `backend`, `model_override`,
`prompt`, project and `idempotency_key`. Remote selection is validated against
the fleet, but Orb rejects remote agent creation before POST: the actual node
contract requires a `remote_command`, and backend/model fields only hold local
resume metadata. There is no verified remote harness/credential provisioning
adapter in this client. Installed Claude/OpenCode executables are not a
substitute for Grok Build. The draft, harness/model and node selection remain
intact; no fallback machine, command or credential request is issued.

Contract checked against `fix/orb-remote-mission-launch` at `1b292c9c`:
`src/api/control/mod.rs` CreateMissionRequest and remote_job_projection. Existing
remote missions use `remote_job.node_id` for placement and the durable job phase
and node state for status. Active/observed alone means accepted, not running;
only node_state=running confirms running. Ambiguous or unobserved jobs explicitly
show that the backend is checking their state. Terminal mission status remains
authoritative. No live mission or user goal was retried.

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
missing nodes, pre-POST unsupported remote harness rejection, and durable remote
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


## Verified remote contract correction

The launch timing and slow-POST success fixtures now use the supported local
create path. Previous remote success mocks assumed command synthesis that the
backend does not implement; they are not evidence of working remote harness
launches. Remote Grok and Codex browser tests instead assert zero POSTs, retained
draft and selection, and an explicit error. API tests also cover Claude,
OpenCode and Gemini rejection without network requests. Read-only mission views
cover observed, queued, running, unobserved and ambiguous remote job states.

This correction passes 50 unit/component tests, 14 targeted launch browser tests,
and the production frontend build. Local launch benchmark: optimistic feedback
0.7 ms median / 1.3 ms p95; accepted-view transition 2.8 / 4.0 ms. Screenshot:
`test-results/orb-remote-unsupported.png`.
