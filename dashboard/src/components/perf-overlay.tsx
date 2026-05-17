"use client";

import { useEffect, useState } from "react";
import { perfBus, type ReducerSample } from "@/lib/perf-bus";

type Snapshot = {
  longtaskTotalMs: number;
  longtaskMaxMs: number;
  longtaskCount: number;
  domNodes: number;
  heapUsedMB: number | null;
  heapLimitMB: number | null;
  sseBytesPerSec: number;
  sseTotalKb: number;
  sseEventsPerSec: number;
  sseFilteredPerSec: number;
  reducers: { name: string; sample: ReducerSample }[];
};

type MemoryInfo = { usedJSHeapSize: number; jsHeapSizeLimit: number };

function readHeap(): MemoryInfo | null {
  const mem = (performance as unknown as { memory?: MemoryInfo }).memory;
  return mem && typeof mem.usedJSHeapSize === "number" ? mem : null;
}

/**
 * Fixed-position diagnostics panel. Only mounts when `?debug=perf` is set on
 * the URL (the bus reads the flag once at module load). Polls the bus every
 * 500 ms; everything is best-effort and silently degrades on Safari/Firefox
 * (no `PerformanceObserver` for longtask, no `performance.memory`).
 */
export function PerfOverlay() {
  const [snap, setSnap] = useState<Snapshot | null>(null);

  useEffect(() => {
    if (!perfBus.enabled) return;

    let prevBytes = perfBus.sseBytes;
    let prevReceived = perfBus.sseEventsReceived;
    let prevFiltered = perfBus.sseEventsFiltered;
    let prevAt = performance.now();

    const tick = () => {
      const now = performance.now();
      perfBus.pruneLongtasks(now);
      const dt = (now - prevAt) / 1000;
      const heap = readHeap();
      const reducers = [...perfBus.reducerTimings.entries()]
        .map(([name, sample]) => ({ name, sample }))
        .sort((a, b) => b.sample.totalMs - a.sample.totalMs)
        .slice(0, 6);

      setSnap({
        longtaskTotalMs: perfBus.longtasks.reduce((a, b) => a + b.d, 0),
        longtaskMaxMs: perfBus.longtasks.reduce(
          (a, b) => (b.d > a ? b.d : a),
          0
        ),
        longtaskCount: perfBus.longtasks.length,
        domNodes: document.getElementsByTagName("*").length,
        heapUsedMB: heap ? heap.usedJSHeapSize / 1024 / 1024 : null,
        heapLimitMB: heap ? heap.jsHeapSizeLimit / 1024 / 1024 : null,
        sseBytesPerSec: dt > 0 ? (perfBus.sseBytes - prevBytes) / dt : 0,
        sseTotalKb: perfBus.sseBytes / 1024,
        sseEventsPerSec:
          dt > 0 ? (perfBus.sseEventsReceived - prevReceived) / dt : 0,
        sseFilteredPerSec:
          dt > 0 ? (perfBus.sseEventsFiltered - prevFiltered) / dt : 0,
        reducers,
      });

      prevBytes = perfBus.sseBytes;
      prevReceived = perfBus.sseEventsReceived;
      prevFiltered = perfBus.sseEventsFiltered;
      prevAt = now;
    };

    tick();
    const id = window.setInterval(tick, 500);
    return () => window.clearInterval(id);
  }, []);

  if (!perfBus.enabled) return null;
  if (!snap) {
    return (
      <div className="fixed right-2 top-2 z-[9999] rounded bg-black/80 px-2 py-1 font-mono text-[10px] text-white">
        perf overlay loading…
      </div>
    );
  }

  const fmtMs = (ms: number) =>
    ms < 1 ? "<1ms" : ms < 1000 ? `${ms.toFixed(0)}ms` : `${(ms / 1000).toFixed(1)}s`;
  const fmtRate = (n: number) => (n < 10 ? n.toFixed(1) : n.toFixed(0));
  const fmtKB = (n: number) =>
    n < 1024 ? `${n.toFixed(0)} KB` : `${(n / 1024).toFixed(1)} MB`;

  const heapWarn =
    snap.heapUsedMB != null && snap.heapLimitMB != null
      ? snap.heapUsedMB / snap.heapLimitMB > 0.5
      : false;
  const ltWarn = snap.longtaskTotalMs > 2000;

  return (
    <div
      className="pointer-events-auto fixed right-2 top-2 z-[9999] w-[260px] rounded-md border border-white/10 bg-black/85 px-3 py-2 font-mono text-[11px] leading-tight text-white shadow-lg backdrop-blur"
      data-testid="perf-overlay"
    >
      <div className="mb-1 flex items-center justify-between text-[10px] uppercase tracking-wider text-white/60">
        <span>perf · ?debug=perf</span>
        <span className={ltWarn ? "text-red-400" : "text-white/40"}>
          {snap.longtaskCount} LT/10s
        </span>
      </div>
      <Row
        label="Longtask 10s"
        value={`${fmtMs(snap.longtaskTotalMs)} (max ${fmtMs(snap.longtaskMaxMs)})`}
        warn={ltWarn}
      />
      <Row label="DOM nodes" value={snap.domNodes.toLocaleString()} warn={snap.domNodes > 5000} />
      {snap.heapUsedMB != null && (
        <Row
          label="JS heap"
          value={`${snap.heapUsedMB.toFixed(0)} / ${snap.heapLimitMB?.toFixed(0) ?? "?"} MB`}
          warn={heapWarn}
        />
      )}
      <Row
        label="SSE in"
        value={`${fmtKB(snap.sseBytesPerSec / 1)}/s · total ${fmtKB(snap.sseTotalKb)}`}
      />
      <Row
        label="SSE evt/s"
        value={`${fmtRate(snap.sseEventsPerSec)} rx · ${fmtRate(snap.sseFilteredPerSec)} drop`}
      />
      {snap.reducers.length > 0 && (
        <div className="mt-2 border-t border-white/10 pt-1">
          <div className="mb-0.5 text-[10px] uppercase tracking-wider text-white/40">
            Reducers (cum)
          </div>
          {snap.reducers.map(({ name, sample }) => (
            <div key={name} className="flex justify-between text-[10px]">
              <span className="truncate text-white/70">{name}</span>
              <span className="ml-2 shrink-0 text-white/80">
                {sample.count}× · {fmtMs(sample.totalMs)} · max {fmtMs(sample.maxMs)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function Row({
  label,
  value,
  warn,
}: {
  label: string;
  value: string;
  warn?: boolean;
}) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <span className="text-white/60">{label}</span>
      <span className={warn ? "text-red-400" : "text-white/90"}>{value}</span>
    </div>
  );
}
