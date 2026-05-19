# /control performance overhaul — final notes

This is the engineering log for the 27-item perf overhaul tracked in
issue / project plan from May 2026. It captures what shipped, what was
deliberately deferred, and the before/after measurements on the
verity fixture missions.

## Status summary

| Phase | Item | Status |
| --- | --- | --- |
| P0 | #1 `?debug=perf` overlay | ✅ shipped #437 |
| P0 | #2 reducer timers | ✅ shipped #437 |
| P0 | #3 server metrics endpoint | ✅ shipped #440 |
| P1 | #4 server SSE filter | ✅ shipped #438 |
| P1 | #5 SSE-fresh poll guard | ✅ shipped #438 |
| P1 | #6 rAF coalescing | ✅ shipped #438 |
| P1 | #7 NowTickProvider | ✅ shipped #439 |
| P1 | #8 tolerant continuation | ✅ shipped #438 |
| P1 | #9 navigation leak | ✅ shipped #438 |
| P1 | #10 markdown size cap | ✅ shipped #438 |
| P2 | #11 virtualize chat | ✅ implemented in this branch |
| P2 | #12 virtualize thoughts sheet | ✅ implemented in this branch |
| P2 | #13 lazy markdown | ✅ shipped #443 |
| P2 | #14 memoize derived slices | ✅ shipped #442 |
| P2 | #15 split ControlView | ✅ implemented in this branch (slice stores) |
| P2 | #16 worker reducer | ✅ implemented in this branch |
| P3 | #17 delta summarization | ✅ implemented in this branch |
| P3 | #18 since_seq cursors | ⏸ deferred — backend, large |
| P3 | #19 WS migration | ✅ implemented in this branch |
| P3 | #20 per-mission channels | ⏸ deferred — backend, medium |
| P3 | #21 backend text_delta backpressure | ⏸ deferred — backend, medium |
| P4 | #22 negotiated `text_op` protocol | ✅ implemented in this branch |
| P4 | #23 canonical assistant rows | ✅ implemented in this branch |
| P4 | #24 tool-output truncation | ⏸ deferred — backend, medium |
| P5 | #25 health budget telemetry | ⏸ deferred — needs ingestion |
| P5 | #26 Playwright perf CI | ✅ implemented in this branch |
| P5 | #27 STREAMING.md | ✅ shipped (this file's sibling) |

## Before / after (verity mission `3a902278`, 1882 events)

Measured via the `?debug=perf` overlay we landed in P0-#1.

| Metric | Before (master, May 17) | After (P0+P1+P2 partial) | Delta |
| --- | --- | --- | --- |
| 10s longtask total | 23.4 s | 53 ms | -440× |
| 10s longtask max | 5.3 s | 53 ms | -100× |
| DOM nodes after 10s idle | 13–14k (growing) | 967 (stable) | -14× |
| JS heap after 10s | 318 MB (growing) | 141 MB (stable) | -55% |
| SSE drops/sec (cross-mission noise) | 9.9 | 0 (post-server-filter deploy) | -100% |
| Markdown render time, 200 KB bubble | 5.0 s | <1 ms (capped) | bounded |

The original symptom (74-second freezes on opening verity #1884)
disappeared after P1-#4..#10 alone. Subsequent items are
optimisations, not bug fixes.

## New measurements from this branch

| Item | Measurement |
| --- | --- |
| P2-#11/#12 virtualization | `dashboard/tests/control-perf.spec.ts` fixture mission with 500 messages passes DOM `<5k`; local Chromium run completed in 34.0s. |
| P3-#17 summarization | `inactive_stream_summary_reduces_large_payload_by_ten_x` covers the read-side collapse and asserts the synthetic payload reduction is at least 10x. |
| P4-#22 negotiated deltas | `text_op_stream_transform_converts_cumulative_delta_to_insert_then_replace` and `text_op_stream_transform_finalizes_before_assistant_message` cover the `cap=text_op` transport conversion path. |
| P4-#23 canonical rows | `finalized_text_ops_collapse_to_canonical_assistant_row` proves a finalized `text_op` log is replaced by one `assistant_message_canonical` row. |
| P5-#26 perf CI | Playwright `control @perf keeps large mission within browser budgets` asserts heap `<300MB`, max longtask `<500ms`, and DOM `<5k`. |

Validation commands run locally:

```bash
cargo fmt --all --check
cargo check -q
cargo test -q inactive_stream_summary --lib
cargo test -q text_op_stream_transform --lib
cargo test -q finalized_text_ops_collapse_to_canonical_assistant_row --lib
cd dashboard && npx tsc --noEmit
cd dashboard && bun run build
cd dashboard && PLAYWRIGHT_PORT=3001 PLAYWRIGHT_BASE_URL=http://localhost:3001 bunx playwright test tests/control-perf.spec.ts --project=chromium
cd ios_dashboard && xcodebuild -project SandboxedDashboard.xcodeproj -scheme SandboxedDashboard -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=26.4' build
```

iOS simulator smoke evidence:

- `ios-control-direct-after-sequential.png` shows historical replay of the
  goal fixture mission against the dev backend.
- The first dev-backend run surfaced a Swift concurrency abort in
  `loadMission`; the saved-mission metadata/transcript fetch was made
  sequential and the fixture rendered after rebuild/reinstall.

## Deferred items, with reasoning

These are intentional decisions to stop work, not abandoned TODOs.

### P2-#11/12: virtualize chat list + thoughts sheet

The main transcript and thoughts sheet now use `@tanstack/react-virtual`
with estimated row heights, mount-time measurement, bottom anchoring,
and a scroll-to-bottom pill when the user is away from the bottom. The
Playwright perf fixture holds the 500-message mission under the DOM
budget.

### P2-#15: split ControlView into subscribers

The dashboard now mirrors the iOS-style split with explicit stores for
items, queue, thinking, streaming diagnostics, and the viewing mission.
The layout component owns layout state while panels subscribe to the
slices they render.

### P2-#16: Web Worker for `eventsToItems`

`eventsToItemsImpl` and its parsing/continuation helpers live in
`events-reducer.ts`; `events-worker.ts` exposes a Promise RPC worker
using Next's `new Worker(new URL(..., import.meta.url))` bundling path.
Existing synchronous `eventsToItems()` call sites remain available, while
initial transcript and `since_seq=0` replays route through the worker
with a sync fallback if worker startup fails.

### P3-#17..#21: backend streaming changes

P3-#17 adds a pure read-side summarization pass for inactive missions:
`thinking` and `text_delta` runs are collapsed for `/events`, `/trace`,
and transcript wrappers only when `updated_at` is older than five
minutes. Persisted rows are unchanged, and active missions keep the
incremental path.

P3-#19 adds `/api/control/ws` with 15s heartbeats, client resume, and
dashboard WS-first/SSE-fallback behavior. P3-#18, #20, and #21 remain
deferred follow-ups.

### P4-#22..#24: content model changes

P4-#22 is implemented as a negotiated transport capability:
dashboard and iOS append `cap=text_op`, the backend converts cumulative
`text_delta` buffers into `TextOp::Insert`/`Replace` events for that
connection, and older clients continue receiving cumulative
`text_delta`.

P4-#23 persists in-flight `text_op` rows and collapses a finalized
`bubble_id` into one `assistant_message_canonical` row. Historical
fetches return that canonical row instead of the delta log. Existing
missions are unchanged. P4-#24 remains a deferred follow-up.

### P5-#25: health budget telemetry

Needs a telemetry ingestion endpoint that the dashboard can POST to.
We don't currently run one. Cheap to add server-side; the
client-side aggregation is ~20 lines using the same
`PerformanceObserver` we set up in P0-#1. Deferred pending decision
on where the telemetry should land.

### P5-#26: Playwright perf CI

`dashboard/tests/control-perf.spec.ts` is marked `@perf`, loads the
fixture with `?debug=perf`, waits 30s, and asserts heap, longtask, and
DOM budgets.

## Operational

- Dashboard perf overlay: append `?debug=perf` to any /control URL.
- Server metrics: `GET /api/control/metrics` returns
  `{ uptime_secs, sse: { chunks_total, bytes_total, chunk_size_p50,
  chunk_size_p99 }, endpoints: { events_req_per_minute,
  running_req_per_minute }, broadcast: { events_total,
  mission_count_observed, events_avg_per_mission, top_missions } }`.
- Streaming contract: `backend/STREAMING.md`.
