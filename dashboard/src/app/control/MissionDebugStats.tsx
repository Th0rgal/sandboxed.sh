"use client";

import { useEffect, useRef, useState } from "react";
import type { ChatItem } from "./control-client";

type HeapInfo = {
  usedJSHeapSize?: number;
  totalJSHeapSize?: number;
  jsHeapSizeLimit?: number;
};

type MemoryStatsSnapshot = {
  itemsCount: number;
  itemsContentBytes: number;
  visibleItems: number;
  heap: HeapInfo;
  peakHeap: number;
  timestamp: number;
};

/**
 * Rough estimate of the memory footprint of a ChatItem array.
 *
 * We sum up the `.length` of the obvious string fields (`content`, `result`,
 * error messages) and add a constant per item to approximate object overhead
 * plus ignored metadata. This is not a byte-accurate measurement — strings
 * in V8 are stored as UTF-16 so actual bytes are `length * 2` — but it's
 * a stable enough signal to diff frame-to-frame and to threshold against.
 */
function estimateItemsBytes(items: ChatItem[]): number {
  let total = 0;
  for (const it of items) {
    total += 64; // object + keys overhead, ballpark
    // Every ChatItem has an `id`; add its length without narrowing.
    const id = (it as { id?: string }).id;
    if (typeof id === "string") total += id.length * 2;
    switch (it.kind) {
      case "user":
      case "assistant":
      case "stream": {
        total += ((it as { content?: string }).content?.length ?? 0) * 2;
        break;
      }
      case "thinking": {
        total += ((it as { content?: string }).content?.length ?? 0) * 2;
        break;
      }
      case "tool": {
        const tool = it as {
          name?: string;
          input?: unknown;
          result?: unknown;
        };
        if (typeof tool.name === "string") total += tool.name.length * 2;
        if (tool.input !== undefined) {
          try {
            total += JSON.stringify(tool.input).length * 2;
          } catch {
            /* cyclic — ignore */
          }
        }
        if (tool.result !== undefined) {
          try {
            total += JSON.stringify(tool.result).length * 2;
          } catch {
            /* cyclic — ignore */
          }
        }
        break;
      }
      default:
        break;
    }
  }
  return total;
}

function readHeap(): HeapInfo {
  const perfMem = (
    performance as unknown as { memory?: HeapInfo }
  ).memory;
  if (!perfMem) return {};
  return {
    usedJSHeapSize: perfMem.usedJSHeapSize,
    totalJSHeapSize: perfMem.totalJSHeapSize,
    jsHeapSizeLimit: perfMem.jsHeapSizeLimit,
  };
}

function fmtBytes(n: number | undefined): string {
  if (n === undefined || !Number.isFinite(n)) return "?";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const SESSION_KEY = "control-debug-peak";

/**
 * Floating memory/debug overlay for the control page.
 *
 * Renders a small badge in the bottom-right that shows live item counts
 * and JS heap usage, plus the peak heap seen during this tab's lifetime.
 * Always-on (not gated by a flag) because the renderer crashes we're
 * chasing leave no post-mortem otherwise — when the tab goes OOM, the
 * last snapshot written to `sessionStorage[control-debug-peak]` is all
 * that survives, and the user can copy it out after reload.
 *
 * Also emits `console.warn` / `console.error` when thresholds are hit so
 * anyone with DevTools open sees exactly what blew up, and publishes a
 * `mission-debug-stats` CustomEvent that `control-client.tsx` listens
 * for to decide whether to shed items pre-emptively.
 */
export function MissionDebugStats(props: {
  items: ChatItem[];
  visibleItems: number;
}) {
  const { items, visibleItems } = props;
  const [snapshot, setSnapshot] = useState<MemoryStatsSnapshot | null>(null);
  const [expanded, setExpanded] = useState(false);
  const peakHeapRef = useRef<number>(0);
  const lastLoggedBucketRef = useRef<number>(0);

  useEffect(() => {
    let cancelled = false;

    function tick() {
      if (cancelled) return;
      const itemsContentBytes = estimateItemsBytes(items);
      const heap = readHeap();
      const used = heap.usedJSHeapSize ?? 0;
      if (used > peakHeapRef.current) {
        peakHeapRef.current = used;
      }
      const snap: MemoryStatsSnapshot = {
        itemsCount: items.length,
        itemsContentBytes,
        visibleItems,
        heap,
        peakHeap: peakHeapRef.current,
        timestamp: Date.now(),
      };
      setSnapshot(snap);

      // Persist the latest reading so a subsequent tab-reload after a
      // renderer crash still has something to look at.
      try {
        sessionStorage.setItem(SESSION_KEY, JSON.stringify(snap));
      } catch {
        /* storage full — ignore */
      }

      // Emit a synthetic event so the main component can choose to shed
      // items pre-emptively (kept as an event rather than a prop callback
      // to avoid re-running this tick effect when the handler changes).
      window.dispatchEvent(
        new CustomEvent("mission-debug-stats", { detail: snap })
      );

      // Warn at coarse thresholds (MB buckets) to avoid console spam
      // while still surfacing growth trends.
      const bucket = Math.floor(used / (100 * 1024 * 1024)); // per 100 MB
      if (bucket > lastLoggedBucketRef.current) {
        lastLoggedBucketRef.current = bucket;
        const level =
          used > 1_500_000_000 ? "error" : used > 800_000_000 ? "warn" : "log";
        const logger =
          level === "error"
            ? console.error
            : level === "warn"
              ? console.warn
              : console.log;
        logger(
          `[mission-debug] heap=${fmtBytes(used)} items=${items.length} ` +
            `content≈${fmtBytes(itemsContentBytes)} visible=${visibleItems} ` +
            `peak=${fmtBytes(peakHeapRef.current)}`
        );
      }
    }

    tick();
    const handle = window.setInterval(tick, 2000);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [items, visibleItems]);

  if (!snapshot) return null;

  const used = snapshot.heap.usedJSHeapSize ?? 0;
  const limit = snapshot.heap.jsHeapSizeLimit ?? 0;
  const pct = limit > 0 ? (used / limit) * 100 : 0;
  const riskColor =
    pct > 75
      ? "bg-red-600/80 text-white"
      : pct > 50
        ? "bg-amber-500/80 text-black"
        : "bg-neutral-800/80 text-neutral-200";

  return (
    <div
      className={`fixed bottom-2 right-2 z-[999] rounded-md text-[10px] font-mono shadow-lg cursor-pointer select-none ${riskColor}`}
      onClick={() => setExpanded((v) => !v)}
      title="click to toggle details"
    >
      <div className="px-2 py-1">
        items {snapshot.itemsCount} · heap {fmtBytes(used)}
        {limit > 0 && ` (${pct.toFixed(0)}%)`}
      </div>
      {expanded && (
        <div className="border-t border-white/10 px-2 py-1 space-y-0.5">
          <div>visible: {snapshot.visibleItems}</div>
          <div>content: {fmtBytes(snapshot.itemsContentBytes)}</div>
          <div>peak heap: {fmtBytes(snapshot.peakHeap)}</div>
          <div>limit: {fmtBytes(limit)}</div>
        </div>
      )}
    </div>
  );
}
