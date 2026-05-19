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
| P2 | #11 virtualize chat | ⏸ deferred — see below |
| P2 | #12 virtualize thoughts sheet | ⏸ deferred — see below |
| P2 | #13 lazy markdown | ✅ shipped #443 |
| P2 | #14 memoize derived slices | ✅ shipped #442 |
| P2 | #15 split ControlView | ⏸ deferred — see below |
| P2 | #16 worker reducer | ⏸ deferred — see below |
| P3 | #17 delta summarization | ⏸ deferred — backend, large |
| P3 | #18 since_seq cursors | ⏸ deferred — backend, large |
| P3 | #19 WS migration | ⏸ deferred — backend, large |
| P3 | #20 per-mission channels | ⏸ deferred — backend, medium |
| P3 | #21 backend text_delta backpressure | ⏸ deferred — backend, medium |
| P4 | #22-24 content model | ⏸ deferred — cross-stack |
| P5 | #25 health budget telemetry | ⏸ deferred — needs ingestion |
| P5 | #26 Playwright perf CI | ⏸ deferred — flaky-risk in CI |
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

## Deferred items, with reasoning

These are intentional decisions to stop work, not abandoned TODOs.

### P2-#11/12: virtualize chat list + thoughts sheet

The chat list already uses CSS `content-visibility: auto` +
`contain-intrinsic-size: auto 140px` on every row, which gives the
browser permission to skip layout and paint for off-screen rows
without any JS-level virtualizer. Combined with P2-#13 (lazy
markdown) and the P2-#14 memoization, the perf overlay no longer
shows DOM-traversal cost in the longtask profile on a 1.8k-event
mission. A `@tanstack/react-virtual` integration would add 30 KB to
the bundle and a non-trivial scroll-anchor refactor; cost > benefit
at current data sizes. Revisit if a mission with >5k visible items
becomes routine.

### P2-#15: split ControlView into subscribers

The win here is preventing the entire 9k-line component from
re-rendering on every state tick. Half the win has already been
captured: `ChatItemRow` is `memo()`-wrapped, the derived views go
through `useMemo`+`useDeferredValue`, the 1Hz timer is shared
(P1-#7), and SSE bursts coalesce into one commit per frame (P1-#6).
The remaining win comes from migrating to Zustand-or-similar so
unrelated state slices stop triggering global re-renders. That's a
multi-day refactor with high regression risk. Track separately;
don't bundle it into the perf overhaul.

### P2-#16: Web Worker for `eventsToItems`

After P0+P1 landed the `replay:apply` reducer runs at most
**65 ms** for a 5000-event replay on the verity fixture (measured
via the `replay:apply` console.time + the metrics overlay's
"Reducers (cum)" panel). Moving it to a worker requires extracting
~250 lines of helpers, building a worker bundle in Next.js, and
paying ~100ms of structured-clone cost on every call to ship a 5k
ChatItem[] back across the boundary. Net: probably break-even at
current sizes, regression on small-list call sites that fire
hundreds of times per session. Deferred until a mission size emerges
where the reducer alone exceeds 200ms.

### P3-#17..#21: backend streaming changes

All five require coordinated dashboard + iOS + backend changes and
substantial test coverage. The current SSE + `/events` shape is
stable across three clients; changing it has high blast radius. The
per-mission broadcast channels (P3-#20) and the text_delta
backpressure (P3-#21) are the cheapest wins remaining; track them as
follow-up issues.

### P4-#22..#24: content model changes

CRDT-style deltas (P4-#22), canonical-bubble persistence (P4-#23),
and tool-output truncation (P4-#24) all imply data-model migrations.
The P1-#8 tolerant continuation heuristic absorbs most of the
duplicate-token symptom that motivated #22. #23 needs a clear
data-loss story before it's worth the risk. #24 should be done but
also needs backend cooperation (the streaming download endpoint
doesn't exist yet).

### P5-#25: health budget telemetry

Needs a telemetry ingestion endpoint that the dashboard can POST to.
We don't currently run one. Cheap to add server-side; the
client-side aggregation is ~20 lines using the same
`PerformanceObserver` we set up in P0-#1. Deferred pending decision
on where the telemetry should land.

### P5-#26: Playwright perf CI

`@playwright/test --grep perf` runs that load a fixture mission and
assert heap/longtask/DOM budgets are flaky-prone — the Vercel
preview deploy lifecycle alone introduces 30s+ of variance. Better
done as a manual regression script that the perf overlay's
`?debug=perf` already supports.

## Operational

- Dashboard perf overlay: append `?debug=perf` to any /control URL.
- Server metrics: `GET /api/control/metrics` returns
  `{ uptime_secs, sse: { chunks_total, bytes_total, chunk_size_p50,
  chunk_size_p99 }, endpoints: { events_req_per_minute,
  running_req_per_minute }, broadcast: { events_total,
  mission_count_observed, events_avg_per_mission, top_missions } }`.
- Streaming contract: `backend/STREAMING.md`.
