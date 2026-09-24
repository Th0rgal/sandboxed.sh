# Orb side questions

In an existing mission, type `/btw question` or select **btw** beside Send.
The question opens a separate panel above the composer. Follow-ups stay in that
panel; the main composer returns to normal. Closing the panel preserves its
thread during this Orb session. Cancel stops only the side request. **Use in
agent draft** appends an answer to the normal composer for review before sending.

This is Orb's context-only Ask assistant, not a native Claude `/btw` protocol
command. The panel identifies the assistant model. It works independently of the
mission harness (Claude Code, Codex, OpenCode, etc.). It requires Core connectivity,
a synced mission, an OpenAI-compatible Ask assistant model, and the new authenticated
`POST /api/control/missions/:id/btw` route. Older Core versions fail explicitly;
there is no fallback that sends `/btw` to the working harness.

The client snapshots the loaded conversation at submission: delivered user
messages, finished assistant replies, recorded errors and completed tool results.
Thinking, queued messages and in-progress replies are excluded. The snapshot is
bounded to the latest 80 KB, with 12,000 characters per tool result; it can be
partial and cannot establish current external state. Up to 20 previous side
exchanges are included, bounded to 65 KB. No filesystem reads or tools are offered.

The endpoint verifies mission access through the user's control context, calls
AskClient with an empty tool list, and streams start/delta/done/error frames. It
never constructs AskTurn, resolves a workspace, acquires the harness lock, writes
mission events, or enqueues messages. Disconnecting the request drops its provider
future. Client cancellation and mission/connection changes reject late responses.

Side exchanges live only in renderer memory, scoped by connection generation and
mission. Changing accounts clears this cache. Reloading Orb loses these exchanges;
they are deliberately not persisted into the mission transcript or Ask database.

Validation: side-questions.test.tsx covers routing, draft preservation, snapshot
filtering/limits, split UTF-8 SSE frames, interrupted streams, explicit transfer,
and late responses. side-questions.browser.spec.ts exercises the actual composer
and panel in WebKit at desktop/mobile widths. Rust tests verify input bounds and
the provider request's absence of tool definitions against a mock HTTP provider.

Deployment: ship the backend route before the UI. No native Tauri changes or
restart of a local mission are required for this feature.
