/**
 * Process-wide diagnostic bus for the `?debug=perf` overlay.
 *
 * The overlay polls this module every 500 ms to render counters; the rest of
 * the dashboard (SSE handler, reducers) call `recordX` to push data in.
 * Everything no-ops when the URL flag isn't set, so production cost is one
 * boolean check per call site.
 */

type SseEventCategory = "received" | "filtered";

export type ReducerSample = {
  count: number;
  totalMs: number;
  maxMs: number;
};

class PerfBus {
  enabled = false;

  /** Cumulative bytes read from the SSE stream since this tab loaded. */
  sseBytes = 0;
  sseEventsReceived = 0;
  sseEventsFiltered = 0;

  /** Reducer name → aggregated timing since tab load. */
  reducerTimings = new Map<string, ReducerSample>();

  /** Sliding window of longtask entries in the last 10s (start-time, duration). */
  longtasks: { t: number; d: number }[] = [];

  recordSseBytes(totalBytes: number) {
    if (!this.enabled) return;
    this.sseBytes = totalBytes;
  }

  recordSseEvent(category: SseEventCategory) {
    if (!this.enabled) return;
    if (category === "received") this.sseEventsReceived++;
    else this.sseEventsFiltered++;
  }

  recordReducer(name: string, durationMs: number) {
    if (!this.enabled) return;
    const entry = this.reducerTimings.get(name) ?? {
      count: 0,
      totalMs: 0,
      maxMs: 0,
    };
    entry.count++;
    entry.totalMs += durationMs;
    if (durationMs > entry.maxMs) entry.maxMs = durationMs;
    this.reducerTimings.set(name, entry);
  }

  pruneLongtasks(now = performance.now()) {
    const cutoff = now - 10_000;
    while (this.longtasks.length && this.longtasks[0].t < cutoff) {
      this.longtasks.shift();
    }
  }

  /** Wraps `fn` with a console.time + reducer timing record. Mirrors Chrome's perf panel. */
  time<T>(name: string, fn: () => T): T {
    if (!this.enabled) return fn();
    const t0 = performance.now();
    // eslint-disable-next-line no-console
    console.time(name);
    try {
      return fn();
    } finally {
      // eslint-disable-next-line no-console
      console.timeEnd(name);
      this.recordReducer(name, performance.now() - t0);
    }
  }
}

export const perfBus = new PerfBus();

/**
 * One-shot install at module load. Behind `typeof window` so the bus stays
 * inert during SSR.
 */
if (typeof window !== "undefined") {
  try {
    const params = new URLSearchParams(window.location.search);
    perfBus.enabled = params.get("debug") === "perf";
  } catch {
    perfBus.enabled = false;
  }

  if (perfBus.enabled && typeof PerformanceObserver !== "undefined") {
    try {
      const observer = new PerformanceObserver((list) => {
        const now = performance.now();
        for (const entry of list.getEntries()) {
          perfBus.longtasks.push({ t: entry.startTime, d: entry.duration });
        }
        perfBus.pruneLongtasks(now);
      });
      observer.observe({ type: "longtask", buffered: true });
    } catch {
      // longtask is not implemented in Safari/Firefox — silently degrade.
    }
  }
}
