# Orb harness lifecycle review — 2026-09-25

OpenAI is a provider, not a separate execution lifecycle. Orb can run its models
through Codex (native app-server locally, CLI events remotely) or OpenCode.
Provider selection must not determine whether an action or mission is finished.

## Boundaries

- Local desktop adapters own process lifetime and protocol requests; native
  activity is normalized in `local_stream.rs` and rendered by AgentActivity.
- Remote adapters normalize CLI output to persisted events. The control plane
  owns mission status; the transcript owns action identity and display state.
- Final assistant messages settle foreground transcript actions. Missing results
  are explicitly unknown, never fabricated success. Claude background tasks keep
  their independent lifecycle and are not settled by this transcript rule.
- RPC responses belong only to the matching request ID. Child-thread events
  cannot finish the parent Codex turn. Native goal continuation is distinct from
  one turn completing.

## Confirmed defects and corrections

Session `177d9885-c21f-44fe-ae5a-6ce65bb12cd9` was completed on the server.
OpenCode's terminal `error` tool state did not match the remote publisher's
`failed` state, so failed fetches lost their result events. Normalize it and
preserve the error payload. Old transcripts close unresolved actions at final
response with “No result recorded”. The two visible webfetch targets were
separate actions, not duplicate call identities.

Codex local output previously handled assistant text only. Native item events
now feed the same activity model as Claude; OpenCode CLI tool parts do too.
Only public tool details are displayed; reasoning activity gets a label without
exposing raw/encrypted reasoning. Item IDs preserve stable rows across updates.

Codex RPC collection previously accepted unrelated result/error responses.
Responses now match the request ID exactly; intervening notifications remain
queued. Long goal preservation and child-thread filtering remain in place.

Action disclosures put their content before their trigger and compensate the
scroll offset after layout. Buttons retain focus and expose aria-expanded.

## Validation and activation

Targeted Rust protocol tests, transcript/activity Vitest tests and Playwright
geometry/reconciliation tests cover these changes. Frontend changes hot-reload.
Native adapter updates require a subsequent Orb restart; do not terminate an
active user goal merely to activate display changes. The remote error mapping
requires a backend deployment; historical transcript repair is already frontend.
