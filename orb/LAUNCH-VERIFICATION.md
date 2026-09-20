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
no client-generated shell, proxy-key provisioning, supported-harness allowlist,
or fallback machine/harness. A missing/cordoned/offline node is still refused
before POST. Server validation errors keep the draft and selection. Older
raw-only backends reject missing `remote_command`; Orb explains that they do not
support typed launches and does not retry with a raw command or local machine.

Contract verified against `fix/orb-remote-mission-launch` at `8271a0e8` (unchanged
at `488d0a4e`), `docs/REMOTE_NODES.md` and the typed remote admission test in
`src/api/control/dispatch_admission_tests.rs`. There is no capability endpoint
in this checkpoint: the typed POST is the authority for harness/model validation.
The server currently supports Claude Code and OpenCode. Native Grok Build is
explicitly rejected; its node provisioning remains a separate backend task.
OpenCode with a Grok model is never substituted for a Grok Build selection.

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
fixture uses the published server contract: an explicitly selected OpenCode
harness and xai/grok-4.6 model receive an Active mission with remote_job and
waiting_remote_job execution metadata. The accepted view opens without waiting
for the slow mission-list refresh, and durable user history replaces the
optimistic prompt without duplication. Grok/Codex rejection tests retain their
exact selection and assert one POST with no command or credential requests.
The old-server test asserts an explicit rejection and no retry/fallback.

51 unit/component tests, 16 targeted launch browser tests, and the production
frontend build pass. These are mocked client contract tests, not proof of DGX
execution. Backend fixture admission tests are present in the referenced server
checkpoint. Actual node startup needs the approved backend rollout and canary.
Screenshot: `test-results/orb-remote-accepted.png`.
