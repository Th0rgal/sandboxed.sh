# Transcript rendering benchmark

Run `./benchmarks/compare.sh` from `orb` with pnpm dependencies and Playwright
Chromium installed. It reads the original Transcript from commit `9a7cb32b`,
loads it temporarily alongside the current component, runs both in the same
browser harness, then removes the temporary source. Port 1431 is isolated from
Tauri's port 1430. No live backend is used.

Workload: the real Transcript component, 1,500 events (300 user/snapshot/tool
call/result/final groups), 20 initial renders and 60 cumulative live updates.
Every timed render/update forces layout with `offsetHeight`; updates yield to
`requestAnimationFrame`. MutationObserver counts added/removed child nodes.
An existing completed work group is manually opened before updates.

Measured in this Linux Chromium checkout on 2026-09-20:

| Metric | Before | After |
| --- | ---: | ---: |
| Initial render median / p95 | 42.4 / 62.5 ms | 45.5 / 56.2 ms |
| Live update median / p95 | 20.2 / 27.1 ms | 4.6 / 6.9 ms |
| Replay median / p95 | 0.3 / 0.8 ms | 0.5 / 1.2 ms |
| Added / removed nodes | 18,120 / 18,119 | 60 / 59 |
| Existing open work group retained | No | Yes |

Raw results are committed beside this file. Timing varies with host load;
regression assertions enforce retained DOM identity/open state and fewer than
500 node removals, rather than a machine-dependent millisecond ceiling.
The corrected reducer produces fewer text elements because a cumulative response
spanning tools is now one response. History rows are reconciled by stable keys,
unchanged work groups are reused, and historical Markdown does not remount or
reparse on each token. Group traversal remains linear in displayed history;
this change does not claim transcript virtualization.

`tests/transcript.test.ts` also uses the supplied 328-event redacted production
fixture for mission `4bf947aa-7f72-44c2-8a8f-16816e37d663`. That stored log contains
only one coalesced text snapshot; live cumulative operations across tool
boundaries are tested separately against the producer's contract in
`src/api/control/mod.rs::text_op_events_for_stream`. No global text-prefix
deduplication is used.
