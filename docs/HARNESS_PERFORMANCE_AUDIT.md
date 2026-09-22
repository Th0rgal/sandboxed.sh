# Harness performance investigation — 22 September 2026

This is a measured follow-up to `CLAUDE_STREAMING.md`, not a deployment of
additional harness changes. Source inspected at `6ee592e3`; Core runs the
stream-guard fix `e660fc0a3704`. All diagnostic missions requested a 30-row table,
no tools and no file changes. A single run is a diagnostic, not a provider speed
ranking. Production credentials were neither copied nor included in receipts.

## OpenCode observations

| Placement | Model | Result |
| --- | --- | --- |
| Core | Kimi k3 through the configured routing | One visible text event, 2,062 characters; final assistant message 395 ms later; awaiting-user another 542 ms later |
| Ashur | Kimi k3 | Failed in about 4.5 seconds: `opencode is not installed on this node` |
| old-agent | Kimi k3 | Success; native CLI emitted one text event at 15:02:33.854 UTC; Core emitted it at 15:02:34.378 UTC, 524 ms later |

On old-agent the native job ran from 15:01:09.588 to 15:02:33.914 UTC. The
native output contained `step_start`, one `text`, then `step_finish`. Therefore
the long wait in this run occurred before CLI text emission. It cannot be fixed
by making Orb render that event faster. Core also showed one final text event;
its native CLI timing was not separately captured. The runs used different
OpenCode versions and were sequential, so their total times are not a fair
machine comparison.

The local runner deliberately uses `opencode run --format json`, without its
former parallel SSE side channel (`src/api/runners/opencode.rs`). Verify whether
the supported native integration can emit incremental text before restoring a
second transport. Any replacement must retain native session identity, tool
events, cancellation, usage accounting and final-output recovery.

## Actual machine capabilities

SSH executable/version probes, plus a real failed Ashur launch:

| Machine | OpenCode | Gemini CLI | Grok |
| --- | --- | --- | --- |
| Core | 1.18.18 | 0.33.1 | 1.0.34 |
| Ashur | absent from login PATH; launch confirms absent | absent from login PATH | absent from login PATH |
| Babylon | absent from login PATH | absent from login PATH | absent from login PATH |
| Nippur | absent from login PATH | absent from login PATH | absent from login PATH |
| old-agent | 1.17.18 | 0.33.1 | 0.2.93 |
| DGX Spark | 1.18.31 | absent from login PATH | 1.0.34 |

Login PATH discovery is not proof of service-user readiness. All six nodes
reported online with node version 1.3.0, but the global remote-launch list
advertises Claude/OpenCode/Grok/Codex without per-node installed/readiness data.
Gemini is not supported by the typed remote launcher even where its CLI exists.
No new software was installed during this investigation.

The Machines page should show per-harness installed version, service-user probe,
authentication readiness, streaming/resume/goal support, last successful canary,
and desired version. Admission must check those same server-owned capabilities.
Provide install/update actions with a pinned fleet version, canary validation and
rollback; do not silently install or update in a mission's critical launch path.

## Shared text fusion: reproduced CPU pathology

`suffix_prefix_overlap_len` in `src/api/mission_runner.rs` rescans the accumulated
Unicode string for each candidate overlap. Gemini text/reasoning and Grok
text/reasoning use it; there are also Claude and Codex call sites.

Synthetic local Mac debug-build measurements of the current algorithm:

| Accumulated UTF-8 bytes | Incoming bytes | Existing merge time |
| --- | --- | --- |
| 10,994 | 20 | 2.75 ms |
| 109,526 | 20 | 27.65 ms |
| 109,526 | 2,000 | 2,557 ms |
| 1,095,260 | 2,000 | 28,035 ms |

The fixture repeats a French sentence in the accumulated buffer and appends
`z` characters with no overlap. These are adversarial synthetic cases, not
measured production pauses. The original timing includes a buffer clone and
append; the prototype timing below isolates overlap search, so do not present
the ratio as an end-to-end harness speedup.

A standalone byte-prefix-function prototype scans only the last
`incoming.len()` bytes of the old buffer. For 2,000-byte fragments its overlap
search averaged 32–34 microseconds across 100 calls regardless of whether the
old buffer was 110 KB or 1.1 MB. It agreed with the original overlap result on
116,281 pairs of strings of length 0–4 over `a`, `b`, `é`, `🙂`.

This is the best first implementation candidate: replace only overlap search,
preserving the current snapshot/replay/append semantics, with regression tests
for long Unicode text, repeated prefixes, exact snapshots and cancellation
responsiveness. Exhaustive short-string agreement is evidence, not a substitute
for review and integration tests. The prototype is not deployed.

Reproduce with `rustc scripts/perf/stream_merge_baseline.rs -o /tmp/merge-old`
and `rustc scripts/perf/stream_merge_prototype.rs -o /tmp/merge-new`, then run
the binaries. Both compile in debug mode; the baseline intentionally contains
the slow algorithm and takes about 30 seconds on the measurement machine.

## Gemini and Grok readiness on Core

Additional bounded diagnostic launches did not reach inference:

- Gemini CLI 0.33.1 with the catalog's `gemini-3-flash-preview` failed after
  about 12 seconds. Its cached-credential authentication returned
  `IneligibleTierError`, stating this client was no longer supported for the
  account's Code Assist for individuals path. This is the observed server error,
  not a verified claim about all Gemini accounts or a demonstrated update fix.
- Grok 1.0.34 with `grok-4.6` became blocked: ACP returned `Authentication
  required` / `no auth method id provided`, and the runner found no usable
  cached login in the workspace HOME. The error explicitly says an API key
  cannot authenticate this native `grok agent stdio` path.

Neither run is a performance benchmark. Resolve account/CLI compatibility and
managed login before timing these harnesses. Do not hide these failures behind
an installed/connected badge, or launch repeated retries. No login flow was
started and no provider/account settings were changed.

## Node transport

`poll_remote_job` sleeps three seconds before each iteration. It fetches full
job status, then incremental logs. Thus detection latency includes up to roughly
three seconds plus request/processing time; the old-agent sample happened near
the next poll and incurred only 524 ms.

Status responses include up to 64 KiB of log tail even when the observer then
fetches the cursor-based log separately. Increasing the entire polling loop to
20 Hz would multiply redundant I/O and lease persistence. Prefer an incremental
log stream or long poll, retain the durable byte cursor and bounded chunks,
and keep state/lease heartbeats on their slower cadence. Reconnect, restart,
cancellation and final-log draining must retain their existing guarantees.

## Recommended sequence

1. Replace shared overlap search with the bounded linear algorithm.
2. Publish and enforce per-node harness readiness; reconcile managed versions.
3. Establish an incremental OpenCode source, validated against its native CLI
   or supported session API. UI coalescing alone cannot invent missing deltas.
4. Separate node log delivery from state/lease polling, avoiding duplicate tails.
5. Coalesce Gemini cumulative text/thinking broadcasts (currently every input
   event) only after measuring event rate and bytes. Grok already has coalescers.

Keep source emission time, Core ingestion time, final-message time and terminal
status time distinct in instrumentation. Provider thinking, cold startup,
transport delay and expensive parsing otherwise look like the same UI stall.

Receipts on Core: `/tmp/harness-perf-probes.json`,
`/tmp/harness-perf-old-agent.json`, `/tmp/harness-perf-other.json`.
Reproducible overlap benchmarks are in `scripts/perf/stream_merge_*.rs`.

## Implemented follow-up: Orb UI probes and production fixes

The investigation above is the historical baseline. By 22 September 17:03 UTC,
Core was running `1b704fea5388` with the following fixes, verified with targeted
Rust tests on macOS and Linux:

- Linear suffix/prefix overlap search, preserving snapshot/replay/append results.
- Incremental remote logs polled every 500 ms; full status/lease checks stay at
  three seconds. Missed ticks are delayed, not replayed in bursts.
- Codex recognizes a labeled “Do not use tools…” clause, so a text-only prompt
  no longer triggers a corrective retry and a false stalled failure.
- Bare Claude model IDs are accepted by the native Messages proxy.
- Exclusive attachment publication uses the appropriate macOS filesystem API;
  a concurrent-write test verifies one winner and no leaked temporary files.

All probes below were launched through Orb's local browser UI, except L1/L2,
which used the actual bundled Tauri desktop application. The common prompt
requests a 30-row French table without tools. These are single diagnostic runs,
not a ranking of models or machines. L1 used a shorter prompt with the same task.

| Probe | Placement / harness / model | Result | Create → final |
| --- | --- | --- | --- |
| A | Core / Claude / Opus 5 | success | 13.84 s |
| B | Core / Codex / GPT-6 Astra, before fix | false stalled failure; repeated table | 47.26 s |
| C | Babylon / OpenCode / K3 | success | 28.89 s |
| D | Ashur / Claude / Sonnet 4.6 | rejected bare model ID | 3.30 s |
| E | Core / Claude / Opus 5 | success | 15.64 s |
| F | Core / OpenCode / K3 | success, one complete text part | 45.67 s |
| G | Core / Codex / GPT-6 Astra, after fix | success, one table | 24.80 s |
| H | Ashur / Claude / Sonnet 4.6, after ID fix | repeated 502s; stopped | not a speed result |
| L1 | This computer / Claude / Opus 5 | success in native Orb | 13.30 s |
| L2 | This computer / Codex / GPT-6 Astra | success in native Orb | 18.33 s |

An independent Core SSE observer recorded metadata only. These timings end at
SSE receipt on Core, **not** DOM paint in Orb. Persisted `text_delta` rows are
coalesced and cannot be used as a count of live streaming updates.

| Probe | SSE text operations | First text after creation | p95 inter-update gap | Maximum gap |
| --- | --- | --- | --- | --- |
| B | 375 | 13.952 s | 105 ms | 4.968 s |
| E | 168 | 4.027 s | 91 ms | 722 ms |
| G | 184 | 10.529 s | 113 ms | 383 ms |

G removes the reproduced retry/failure, but its lower total time is not an
isolated measurement of model speed. OpenCode C/F emitted only final text
parts; more frequent node polling cannot create token events the CLI does not
emit. A future incremental OpenCode integration should use one supported event
transport and retain tools, usage, cancellation and native session identity.

Probe H exposed a second independent bug: native Messages unconditionally sent
Claude OAuth accounts through CLIProxyAPI. Its local `/v1/models` had no Claude
models and `/v1/messages` returned “unknown provider for model claude-sonnet-4-6”.
Cloudflare replaced that origin 502 body with a generic gateway error; Claude
then retried approximately once per minute without log output. Core-owned
Anthropic credentials must use the resolver's direct OAuth token and preserve
the OAuth beta header. CLIProxyAPI is used only for OAuth-only entries owned by
that proxy. Never copy refresh tokens onto nodes to work around this routing.

### Reproduction and receipts

`rustc scripts/perf/stream_merge_compare.rs -o /tmp/merge-compare` compares both
algorithms with identical buffer-clone and append work and asserts equal output.
On the diagnostic Mac debug build, the 1,095,260-byte buffer / 2,000-byte fragment
case fell from 26,220,717 µs to 193 µs. This is an adversarial CPU fixture, not an
end-to-end inference claim. The exhaustive Unicode oracle covers 116,281 pairs.

Probe mission IDs:

- A: `0f037ad2-d025-4cd3-9266-4adff681af2e`
- B: `c8929410-414b-4c4d-808e-028171970fb1`
- C: `37010f5b-7418-4805-b429-2fa845cbe48f`
- D: `774e48e9-2ab7-46d9-b2c1-528deffa859a`
- E: `924197ff-383e-40d4-8323-e2840d96f635`
- F: `d387053b-52ad-4bc4-bba4-507e83d1f569`
- G: `9915fd0d-a6bf-4693-a78e-77978055e8b6`
- H: `aabfbd20-1d95-4c8c-84cb-c0091646f712`
- L1: `b1689576-301b-4dee-9075-fdb4a29b64f3`
- L2: `c9846ff7-61f3-4136-8be5-45d1299ddfdd`

### Babylon recovery

Babylon's node upload binary was installed and validated with a byte-exact
Core→Babylon file upload, then the diagnostic file was removed. The machine was
uncordoned and probe C completed. No reboot was necessary. The resolver fix
persists in `/etc/systemd/resolved.conf.d/70-sandboxed-reliable-dns.conf` using
IPv4 resolver `213.186.33.99`; the previously selected IPv6 resolver timed out.

A large transfer initially stalled again but completed using rsync resume.
Gateway pings and NIC counters were healthy; occasional path loss/retransmission
had been observed. Intermediate traceroute loss alone does not prove provider
packet loss. The exact cause of the intermittent bulk-transfer degradation
remains unproven; do not describe the DNS change as a proven cure for all TCP
stalls. Keep resumable transfers and verify node readiness after updates.
