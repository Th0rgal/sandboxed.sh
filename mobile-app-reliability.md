# Mobile App Reliability Plan

This plan focuses on the iOS dashboard, with notes from the web and Android
clients where they expose useful contrast. The goal is not only surviving bad
networks; it is also making the app fast and boring when the connection is good.

## Current Findings

- The recent reconnect bug came from a blank saved `api_base_url`. The iOS app
  issued hostless requests such as `/api/control/stream?`, URLSession rejected
  them with `NSURLErrorDomain -1002`, and the SSE loop kept showing
  `Reconnecting...`.
- The backend already sends useful liveness signals:
  - SSE emits an initial `status` event and 15s comment keepalives.
  - WebSocket emits 15s heartbeat messages containing the latest sequence.
  - `/events?since_seq=N` is the canonical catch-up path.
  - `/snapshot` is the first-paint path.
- iOS already has a strong partial recovery design:
  - `NetworkMonitor` combines `NWPathMonitor` and health probes.
  - `missionMaxSeq` tracks per-mission cursors.
  - reconnect and scene-activation paths use `since_seq` before falling back to
    a snapshot.
  - stale cached data is preserved when reloads fail.
- The biggest remaining risk is state ambiguity: auth failure, invalid
  configuration, no network path, stream timeout, slow server, and successful
  quiet streams can still collapse into similar user-facing banners.
- The biggest good-network opportunity is to avoid unnecessary reconnect work,
  snapshot fetches, duplicate stream tasks, and slow-path health probes when
  the stream is demonstrably healthy.

## Principles

1. Separate transport state from reachability state.
   A healthy path does not prove the stream is alive; a quiet stream does not
   prove the path is bad.

2. Prefer explicit protocol signals over timers.
   A server heartbeat, event sequence, HTTP status, or WebSocket close reason is
   stronger than an elapsed-time heuristic.

3. Make the good path cheap.
   On good Wi-Fi, the app should hold one live stream, avoid health polling,
   apply event batches once per frame, and avoid refetching data it already has.

4. Make the bad path bounded.
   Every retry loop needs a cap, jitter, a reason, and a recovery trigger.

5. Keep UI truth precise.
   `Reconnecting...`, `Offline`, `Session expired`, `Invalid server URL`, and
   `Slow connection` should mean different things.

## Recommendations

### P0: Keep The URL Fix And Add Configuration State

The blank-base-URL fix should stay. Add one more layer on top: a first-class
configuration state.

- Treat missing/blank URL as the documented default backend.
- Treat syntactically invalid custom URLs as `invalidConfiguration`, not
  `reconnecting`.
- Normalize custom URLs by trimming whitespace and trailing slashes before
  saving.
- Consider rejecting non-HTTP(S) schemes in Settings before persistence.

Good-network benefit: prevents a local configuration bug from taking the app
through the expensive stream retry path.

Tests:
- blank `api_base_url` falls back to the default backend.
- `" https://example.com/ "` normalizes to `https://example.com`.
- `ftp://example.com` or `example.com` produces an explicit configuration
  error before any stream task starts.

### P0: Count Every Server Heartbeat As Stream Activity

SSE comments and `status` events are liveness signals. The iOS parser should
continue to notify `NetworkMonitor` when a comment keepalive arrives. This
matches the backend streaming contract, where comment keepalives exist to keep
quiet long-running sessions alive.

Good-network benefit: a quiet mission no longer drifts into degraded or
reconnecting just because no content-bearing agent event arrived.

Tests:
- an SSE stream containing only `: keepalive\n\n` every 15s keeps the banner
  hidden.
- a stream with no bytes for longer than the inactivity timeout transitions to
  reconnecting.

### P0: Split Connection States

Replace the single user-facing state pipeline with two internal dimensions:

- `Reachability`: `pathDown`, `pathUpUnverified`, `serverHealthy`,
  `serverUnhealthy`.
- `Stream`: `idle`, `connecting`, `open`, `heartbeatFresh`, `stale`,
  `recovering`, `rejected(status)`, `authExpired`, `invalidConfiguration`.

Then map those to UI labels. Suggested mapping:

- `invalidConfiguration` -> `Check server URL`
- HTTP 401 -> `Session expired`
- path down -> `Offline`
- stream stale but health OK -> `Slow connection - catching up`
- stream closed and retrying -> `Reconnecting...`
- stream open with recent heartbeat -> no banner

Good-network benefit: avoids scary banners when the stream is healthy but quiet,
and avoids retrying stream work when the real issue is auth/config.

Tests:
- HTTP 401 from stream logs out and shows auth UI instead of a reconnect banner.
- invalid URL never starts a reconnect loop.
- healthy heartbeat suppresses the banner even without agent events.

### P1: Prefer WebSocket For iOS Control Streaming

The backend already exposes `/api/control/ws?mission=<uuid>` with two iOS-friendly
features SSE does not have:

- explicit heartbeat payloads containing the latest sequence;
- client-driven resume: `{"type":"resume","since_seq": N}`.

iOS does not have the browser limitation called out in `backend/STREAMING.md`;
`URLSessionWebSocketTask` can be created from a `URLRequest`, so the app can
attach the same bearer token it uses for other requests. Apple documents
`URLSessionWebSocketTask` as the Foundation WebSocket transport, and it supports
async send/receive over `ws:` and `wss:` URLs.

Implementation shape:

- Add `ControlStreamTransport` with two implementations: `SSEControlStream` and
  `WSControlStream`.
- Default iOS to WebSocket when authenticated and backend version supports it.
- On WS open, immediately send `resume` with `missionMaxSeq[missionId]` when
  available.
- Use WS heartbeat `seq` to advance freshness, but only advance `missionMaxSeq`
  after events are applied or after a successful delta fetch confirms no gap.
- Fall back to SSE if WS handshake fails with unsupported status or protocol
  errors.

Good-network benefit: fewer unnecessary HTTP snapshot calls after foregrounding
or reconnecting, because heartbeat `seq` tells the client whether it is already
current.

Bad-network benefit: resume is one round trip on the same socket instead of a
separate `/events` fetch after stream reconnect.

Tests:
- WS sends `resume` after open with the saved cursor.
- WS heartbeat with unchanged `seq` marks stream fresh without fetching.
- WS heartbeat with newer `seq` triggers a bounded delta fetch or resume replay.
- failed WS handshake falls back to SSE once, then applies normal backoff.

### P1: Add Stream Diagnostics

Add a lightweight in-memory diagnostic ring buffer visible from Settings or a
debug overlay. The web client already has a `StreamDiagnosticUpdate` shape; iOS
should mirror that idea.

Capture:

- transport: `sse` or `ws`;
- URL host, never the auth token;
- lifecycle phase: connecting, open, heartbeat, event, closed, error;
- HTTP status or WS close code;
- last event type;
- last applied sequence;
- last heartbeat age;
- reconnect attempt and backoff;
- auth/config/reachability classification.

Good-network benefit: makes accidental duplicate streams, repeated scene-phase
reloads, and needless health probes obvious during normal use.

Tests:
- diagnostics never include Authorization header or JWT.
- reconnect reason is recorded for timeouts, HTTP rejection, cancellation, and
  path-down transitions.

### P1: Guard Against Duplicate Stream Tasks

`ControlView.startStreaming()` cancels the previous task, but scene changes,
mission changes, and cold start all touch streaming. Add a stream generation ID
and log it in diagnostics.

- Increment generation on every intentional stream restart.
- Drop events from stale generations before they hit state.
- Ignore cancellation errors for stale generations.
- Do not call `noteStreamReconnecting` for intentional mission-switch teardown.

Good-network benefit: prevents flicker and avoidable reconnect banners when the
user switches missions quickly or foregrounds the app.

Tests:
- old stream event after mission switch is ignored.
- cancelling a stream during mission switch does not show `Reconnecting...`.

### P1: Tune Health Probes For Good Networks

Health probes should be evidence, not background noise.

- Do not probe while heartbeat freshness is inside the expected window.
- Add jitter to the probe interval so multiple clients do not synchronize.
- Pause probes while app is backgrounded unless a foreground task explicitly
  needs fresh data.
- Reset probe failure count on any authenticated successful API response, not
  only stream activity.

Apple's `waitsForConnectivity` can make URLSession wait instead of failing
immediately when connectivity is unavailable. That is useful for background or
low-urgency operations, but not for foreground chat actions where bounded
feedback is better. Keep short foreground timeouts; consider a separate
connectivity-waiting session only for non-urgent refreshes.

Good-network benefit: fewer extra `/api/health` calls while the stream is
healthy.

Tests:
- no health probes fire while keepalives arrive on schedule.
- any successful JSON request clears degraded health state.
- foreground send still fails within the configured request timeout when the
  host blackholes.

### P2: Make Reconnect Backoff Reason-Aware

The current exponential backoff is a reasonable base. Make it reason-aware:

- path down: wait for `NWPathMonitor` path recovery before retrying;
- HTTP 401: stop retrying and show login;
- invalid URL: stop retrying and show Settings;
- server 5xx or stream close: retry with exponential backoff and jitter;
- timeout after previous heartbeat: retry quickly once, then back off.

Good-network benefit: transient server-side stream closes recover quickly
without waiting through a long backoff accumulated during an unrelated outage.

Tests:
- retry does not run while path is unsatisfied.
- 401 and invalid URL produce no retry loop.
- first post-heartbeat timeout retries quickly once.

### P2: Use Sequence Freshness To Avoid Refetches

Once iOS consumes WS heartbeat `seq`, foreground activation can avoid reloads
when the app already has the latest sequence.

- On active transition, compare `heartbeatSeq` to `missionMaxSeq`.
- If equal and heartbeat is fresh, skip `/events` and `/snapshot`.
- If heartbeat is newer, fetch only `since_seq`.
- If no heartbeat exists, keep the current delta-first behavior.

Good-network benefit: foregrounding the app should often cost zero HTTP
requests.

Tests:
- active transition with fresh equal sequence performs no fetch.
- active transition with newer sequence performs one delta fetch.

### P2: Improve Send Reliability Without Slowing Successful Sends

The send path should be optimistic but idempotent.

- Keep client-generated message IDs.
- Persist pending sends locally before firing the request.
- On app relaunch, show pending sends and let the user retry.
- Deduplicate server echoes by client/server ID.
- Do not delay successful sends behind reachability probes; let the request
  prove the path.

Good-network benefit: no extra preflight request before sending.

Bad-network benefit: user input is not lost if the app is killed mid-request.

Tests:
- duplicate tap/send cannot create duplicate server messages.
- failed send remains visible with retry.
- SSE echo resolves pending state even if POST response was lost.

### P2: Add Mobile Network Simulation Tests

Unit tests catch parser and state bugs, but reliability needs integration tests.

Add a local controllable stream server for XCTest that can:

- send initial status and periodic keepalives;
- stay quiet without closing;
- close immediately after accept;
- return 401, 500, invalid content type, and malformed JSON;
- pause bytes longer than the inactivity timeout;
- emit `stream_lagged`;
- replay duplicate and out-of-order events;
- emit a large event near the buffer cap.

Use simulator tests for:

- app starts with blank defaults and reaches the configured backend path;
- quiet good stream stays connected for at least two heartbeat intervals;
- offline-to-online recovery resumes via `since_seq`;
- mission switch does not show a reconnect banner;
- background/foreground does not refetch when sequence is fresh.

## Suggested Roadmap

1. Land the URL/default-backend fix and heartbeat activity handling.
2. Add explicit connection-state classification and diagnostics.
3. Add the controllable stream test server and cover SSE quiet-stream behavior.
4. Implement WebSocket transport behind a feature flag.
5. Use WS heartbeat sequence to skip good-network foreground refetches.
6. Expand pending-send persistence and retry UX.

## Source Notes

- `backend/STREAMING.md` is the repo's canonical stream contract.
- `src/api/control.rs` confirms SSE keepalives and WS heartbeat/resume support.
- `dashboard/src/lib/api.ts` already has stream diagnostics and opt-in WS logic
  worth mirroring in iOS.
- Apple documents
  [`NWPathMonitor`](https://developer.apple.com/documentation/Network/NWPathMonitor)
  as the Network framework path monitor,
  [`URLSessionConfiguration.waitsForConnectivity`](https://developer.apple.com/documentation/foundation/urlsessionconfiguration/waitsforconnectivity)
  as a way to wait for usable connectivity, and
  [`URLSessionWebSocketTask`](https://developer.apple.com/documentation/Foundation/URLSessionWebSocketTask)
  as Foundation's WebSocket task.
