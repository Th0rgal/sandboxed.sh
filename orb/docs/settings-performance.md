# Settings routing benchmark — 2026-09-24

Run from this checkout:

```
npm --prefix orb run dev -- --host 127.0.0.1 --port 1432
node orb/tests/routing-benchmark.mjs
```

Chromium, five fresh browser contexts, 1300 × 900 viewport. The fixture mounts
real RoutingSettings and uses controlled HTTP responses: 150 ms for routing
endpoints, 500 ms for accounts, 1500 ms for the model catalog. No production
configuration changes or inference requests are made. These are **simulated
network timings**, not measured production backend latency.

Cold ready measures from page navigation completion to the Refresh control being
enabled. Warm measures closing/reopening the component to visible chain content.
It includes Playwright interaction overhead. It is not LCP or full-app startup.

| Median | Before | After |
| --- | ---: | ---: |
| Cold ready | 1822 ms | 198 ms |
| Warm chain display | 225 ms | 38 ms |
| Initial routing requests | 5 | 1 |

Before samples: cold 1844,1821,1822,1829,1813; warm 219,225,228,227,216.
After samples: cold 224,211,198,197,195; warm 42,39,35,38,34.

The previous page already rendered chains progressively, but remained in the
refreshing state until the unrelated model catalog and accounts finished.
The change removes those dependencies from initial loading. Suggestions load
when editing; health/accounts and events load only on their respective tabs.
Only the visible diagnostics tab polls. Chain reads are cached for 30 seconds,
model suggestions for five minutes, with concurrent requests deduplicated.
Explicit refresh and mutation refreshes bypass the chain TTL. Cache scope is the
connection generation, so reconnecting never reuses another account/server's
results. Failures are retryable. Client settings also reuse the already-scanned
local agent inventory; Scan remains available for explicit rescanning.

Functional coverage: routing.test.tsx. Visual smoke checks:
`node orb/tests/routing-screenshot.mjs` (full application, desktop and narrow
window, fixture APIs only). Existing dark/light theme tokens are preserved.
