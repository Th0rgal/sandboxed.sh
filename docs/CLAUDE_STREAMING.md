# Claude stream processing and repetition protection

The 22 September 2026 Pareto incident (`2dd43842-4824-49d9-b3a6-05867ed43c49`)
was a backend processing backlog. Claude had saved its complete 6,485-character
answer at 13:48:44 UTC, but the backend was still consuming buffered fragments
many minutes later. The old repetition detector rescanned a window for every
candidate substring on every delta after 90 seconds. One scan of the actual
3,132-character partial answer took 9,552 ms in a debug build.

## Runtime contract

- Keep the 90-second initial grace and existing meaningful, adjacent,
  non-overlapping repeat rules. Three occurrences are required by default.
- Analyze only the latest 4,096 Unicode characters. Larger configured windows
  are capped at 4,096; candidate lengths are capped at 256 characters.
- Rolling fingerprints index candidates. Exact character comparisons establish
  repetitions; fingerprints alone can never cause a mission interruption.
- Eliminate prefixes with fewer than the required number of occurrences before
  exploring longer candidates. Cache rejected single-word/noise candidates.
- At most one analysis per second per turn, one outstanding job per turn, and
  two workers for the backend. No pending snapshot queue. Analyses run outside
  the async stream reader.
- Each analysis has cooperative limits of 100 ms and two million character
  comparisons. Cancellation, hash collisions and budget exhaustion produce an
  inconclusive result, not a repetition verdict. Continue the stream and warn.
- Before stopping a CLI, require its process to still be alive and the exact
  evidence to remain in the current text window. Discard old jobs on assistant
  block reset, terminal completion and cancellation.
- The PTY reader uses a 256-message bounded channel with backpressure. Close
  the receiver before waiting for the reader during teardown.
- Emit cumulative text at most every 50 ms, with an unconditional final flush.
  Tool and terminal events are never coalesced. Keep block order deterministic.
- Recognize unreaped/zombie child exits and drain queued output before timeout.
  The process monitor is asynchronous and cancelled when its turn goes away.

No public API or Orb status schema changes are required. Debug logs report
analysis duration and inconclusive results, plus queue depth and processing
latency once per second while events are being processed.

## Verification

Linux debug-build measurements on Core, 50 runs each:

| Input | p50 | p95 | Maximum | Non-clear results |
| --- | ---: | ---: | ---: | ---: |
| Actual partial answer, 3,132 characters | 1.568 ms | 2.216 ms | 2.420 ms | 0 |
| Complete answer, 6,485 characters | 2.038 ms | 2.167 ms | 2.941 ms | 0 |

Unit coverage includes an exhaustive small-input oracle, Unicode, structured
reports, cooperative budgets, slow workers, cancellation, bounded queues,
worker concurrency/cadence, and zombie-process recognition. Retain the existing
repetition and Claude runner tests. Benchmark using debug builds; a release build
must not hide a blocking algorithm.

## Host cleanup

Eight abandoned diagnostic scans were identified through orphaned parent shells,
recorded launch scripts using recursive `glob`, SSH-session cgroups, process start
times and recursive traversal of documentation symlink cycles. All eight exited
on SIGTERM; no SIGKILL was needed. No mission or workspace was deleted.

Receipts on Core: `/tmp/stream-guard-abandoned-scans.json` and
`/tmp/stream-guard-cleanup-receipt.json`. Processes without sufficient attribution
were left running. Do not add a global age-based Python process killer.

For future filesystem diagnostics, use known data paths and bounded traversal:
`timeout 30s find -P <known-root> -maxdepth 6 ...`. Do not use recursive Python
`glob` across `/srv`, `/root`, or container root filesystems: it follows directory
symlinks and can spend weeks traversing cycles. A slice such as `glob(... )[:8]`
does not bound the traversal because the full list is generated first.

## Production verification (22 September 2026)

Deployed `e660fc0a3704` through the guarded endpoint. The backend's previous
SHA-256 was checked before maintenance to avoid overwriting a concurrent deploy.
The Pareto mission was paused and resumed with its original native session;
its already-written report was preserved before maintenance. A recovery prompt
requested redisplay without new research or external actions.

The resumed response completed successfully at 14:48:03.735 UTC (27,950
characters). The mission reached `awaiting_user` at 14:48:04.277 UTC: 542 ms
later. The native transcript's final text timestamp was 14:48:03.009 UTC and
the latest streamed-text timestamp 14:48:03.010 UTC. No new research tools were
called by the resumed turn. The response exceeded the detector's 90-second
grace, so this was also a real production test of the enabled guard.

Runtime samples during the verification:

- 20 analyses: p95 5.388 ms, maximum 8.610 ms, zero inconclusive results.
- 158 sampled dequeues: processing delay p95 149 microseconds, maximum 2.082 ms.
- Sampled queue depth: at most one message.
- A separate 50-row Markdown-table canary completed successfully (5,249
  characters) and was deleted afterward. Its initial artificial sleep was
  rejected by the harness, so that phase is not counted as a successful test;
  the real Pareto turn supplied the long-stream verification instead.
- Seven guard tests, eight legacy repetition tests, 45 Claude tests and the
  additional zombie-exit test passed. Linux binaries were built in debug mode.
- Post-deploy monitoring covered 600.6 seconds (21 samples): every fleet health
  check passed, API latency peaked at 54 ms, and memory PSI `some avg10` remained
  zero. This observation window does not establish performance under saturation.

The runtime is deployed from the isolated fix commit above. The feature branch
also retains concurrent Orb/files/resource changes; merging those sources does
not imply they were included in this deployment.
