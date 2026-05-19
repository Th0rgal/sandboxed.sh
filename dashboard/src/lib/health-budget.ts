/**
 * P5-#25: client-side health budget watcher.
 *
 * Sits on the same `PerformanceObserver({type:'longtask'})` the perf
 * overlay (`perf-bus.ts`) installs. Every 5 seconds we compute the
 * total longtask duration in the trailing window; if it exceeds 2s we
 * POST a small JSON report to the server (`/api/control/telemetry/perf`).
 * The server keeps the last 256 in memory and exposes them via
 * `GET /api/control/metrics`.
 *
 * One report per 5s window max, even on sustained breaches, to keep
 * the network and the in-memory ring under control. Reports include
 * mission id (from the URL), heap, DOM size, and stored event count.
 * No PII; no payload content.
 */

import { apiUrl } from "@/lib/api";
import { authHeader } from "@/lib/auth";

const WINDOW_MS = 5_000;
const BUDGET_MS = 2_000;
const REPORT_INTERVAL_MS = 5_000;

type Longtask = { t: number; d: number };

export function startHealthBudgetWatcher(getMissionId: () => string | null, getEventCount: () => number): () => void {
  if (typeof window === "undefined") return () => {};
  if (typeof PerformanceObserver === "undefined") return () => {};

  const longtasks: Longtask[] = [];
  let observer: PerformanceObserver | null = null;
  try {
    observer = new PerformanceObserver((list) => {
      const now = performance.now();
      for (const entry of list.getEntries()) {
        longtasks.push({ t: entry.startTime, d: entry.duration });
      }
      // Prune outside the trailing window.
      while (longtasks.length && longtasks[0].t < now - WINDOW_MS) {
        longtasks.shift();
      }
    });
    observer.observe({ type: "longtask", buffered: true });
  } catch {
    // longtask isn't supported on Safari / Firefox.
    return () => {};
  }

  const interval = window.setInterval(() => {
    const now = performance.now();
    while (longtasks.length && longtasks[0].t < now - WINDOW_MS) {
      longtasks.shift();
    }
    let total = 0;
    let max = 0;
    for (const l of longtasks) {
      total += l.d;
      if (l.d > max) max = l.d;
    }
    if (total < BUDGET_MS) return;

    type MemoryInfo = { usedJSHeapSize: number };
    const memory = (performance as unknown as { memory?: MemoryInfo }).memory;
    const heapUsedMB = memory ? memory.usedJSHeapSize / 1024 / 1024 : 0;
    const domNodes = document.getElementsByTagName("*").length;
    const report = {
      mission_id: getMissionId(),
      longtask_total_ms: Math.round(total),
      longtask_max_ms: Math.round(max),
      event_count: getEventCount(),
      heap_used_mb: Math.round(heapUsedMB * 10) / 10,
      dom_nodes: domNodes,
      at: Date.now(),
    };
    void fetch(apiUrl("/api/control/telemetry/perf"), {
      method: "POST",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        ...authHeader(),
      },
      body: JSON.stringify(report),
      keepalive: true,
    }).catch(() => {
      // Best-effort — never surface failures to the user.
    });
  }, REPORT_INTERVAL_MS);

  return () => {
    window.clearInterval(interval);
    observer?.disconnect();
  };
}
