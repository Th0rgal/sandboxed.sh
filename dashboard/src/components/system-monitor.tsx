"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import { cn } from "@/lib/utils";
import { formatBytes, formatBytesPerSec } from "@/lib/format";
import { getValidJwt } from "@/lib/auth";
import { getRuntimeApiBase } from "@/lib/settings";
import { Activity } from "lucide-react";

interface SystemMetrics {
  cpu_percent: number;
  cpu_cores: number[];
  memory_used: number;
  memory_total: number;
  memory_percent: number;
  network_rx_bytes_per_sec: number;
  network_tx_bytes_per_sec: number;
  timestamp_ms: number;
}

interface SystemMonitorProps {
  className?: string;
  intervalMs?: number;
}

type ConnectionState = "connecting" | "connected" | "disconnected" | "error";

// Design system colors - indigo accent with varying opacity
const CHART_COLORS = {
  // Primary accent (indigo) at different opacities for core lines
  cores: [
    "rgba(99, 102, 241, 0.9)",   // indigo-500
    "rgba(99, 102, 241, 0.75)",
    "rgba(99, 102, 241, 0.6)",
    "rgba(99, 102, 241, 0.5)",
    "rgba(129, 140, 248, 0.9)",  // indigo-400
    "rgba(129, 140, 248, 0.75)",
    "rgba(129, 140, 248, 0.6)",
    "rgba(129, 140, 248, 0.5)",
  ],
  // Main line colors
  primary: "rgb(99, 102, 241)",      // indigo-500 for main metrics
  primaryFill: "rgba(99, 102, 241, 0.1)",
  secondary: "rgba(255, 255, 255, 0.4)", // white/40 for secondary lines
  secondaryFill: "rgba(255, 255, 255, 0.05)",
  grid: "rgba(255, 255, 255, 0.04)",
};

// Liquid glass pill overlay component
function GlassPill({
  children,
  className,
  position = "top-left"
}: {
  children: React.ReactNode;
  className?: string;
  position?: "top-left" | "top-right" | "bottom-left" | "bottom-right";
}) {
  const positionClasses = {
    "top-left": "top-2 left-2",
    "top-right": "top-2 right-2",
    "bottom-left": "bottom-2 left-2",
    "bottom-right": "bottom-2 right-2",
  };

  return (
    <div className={cn(
      "absolute z-10",
      positionClasses[position],
      "inline-flex items-center h-6 px-2.5 rounded-full",
      "bg-white/[0.04] backdrop-blur-lg",
      "border border-white/[0.06]",
      "shadow-[0_1px_6px_rgba(0,0,0,0.25)]",
      className
    )}>
      {children}
    </div>
  );
}

// Multi-line CPU chart with per-core lines
function CpuChart({
  coreHistories,
  avgPercent,
  coreCount,
  height = 100,
}: {
  coreHistories: number[][];
  avgPercent: number;
  coreCount: number;
  height?: number;
}) {
  const [selectedCore, setSelectedCore] = useState<number | null>(null);
  const [showCoreMenu, setShowCoreMenu] = useState(false);
  const width = 400;
  const padding = 2;
  const chartHeight = height - padding * 2;
  const maxPoints = 60;
  const snap = (value: number) => Math.round(value * 2) / 2;

  const buildPath = (data: number[]) => {
    const paddedData = data.length < maxPoints
      ? [...Array(maxPoints - data.length).fill(0), ...data]
      : data.slice(-maxPoints);

    const pointSpacing = width / (maxPoints - 1);
    return `M${paddedData
      .map((v, i) => {
        const x = snap(i * pointSpacing);
        const y = snap(padding + chartHeight - (Math.min(v, 100) / 100) * chartHeight);
        return `${x},${y}`;
      })
      .join(" L")}`;
  };

  const gridLines = [0.25, 0.5, 0.75].map((p) => padding + chartHeight * (1 - p));

  return (
    <div className="relative h-full rounded-xl overflow-hidden bg-white/[0.02] border border-white/[0.04]">
      {/* Glass pill overlay - top left */}
      <GlassPill position="top-left">
        <button
          type="button"
          onClick={() => setShowCoreMenu((prev) => !prev)}
          className="flex items-center gap-2"
        >
          <span className="text-[10px] leading-none font-medium uppercase tracking-wide text-white/50">CPU</span>
          <span className="text-[10px] leading-none font-semibold tabular-nums text-white/80">
            {avgPercent.toFixed(0)}%
          </span>
          <span className="text-[10px] leading-none text-white/40">
            {selectedCore === null ? "All" : `Core ${selectedCore + 1}`}
          </span>
          <span className="text-[10px] leading-none text-white/40">▾</span>
        </button>
      </GlassPill>

      {showCoreMenu && (
        <div className="absolute z-20 left-2 top-9 min-w-[140px] rounded-lg border border-white/[0.06] bg-[#111113] shadow-lg overflow-hidden">
          <button
            type="button"
            onClick={() => {
              setSelectedCore(null);
              setShowCoreMenu(false);
            }}
            className={cn(
              "w-full text-left px-2.5 py-1.5 text-[10px] leading-none font-medium transition-colors",
              selectedCore === null ? "text-white" : "text-white/60",
              "hover:bg-white/[0.06]"
            )}
          >
            All cores
          </button>
          {coreHistories.map((_, idx) => (
            <button
              key={idx}
              type="button"
              onClick={() => {
                setSelectedCore(idx);
                setShowCoreMenu(false);
              }}
              className={cn(
                "w-full flex items-center gap-2 px-2.5 py-1.5 text-[10px] leading-none font-medium transition-colors",
                selectedCore === idx ? "text-white" : "text-white/60",
                "hover:bg-white/[0.06]"
              )}
            >
              <span
                className="inline-block h-1.5 w-1.5 rounded-full"
                style={{ backgroundColor: CHART_COLORS.cores[idx % CHART_COLORS.cores.length] }}
              />
              <span className="tabular-nums">Core {idx + 1}</span>
            </button>
          ))}
        </div>
      )}

      {/* Core count - top right */}
      <GlassPill position="top-right">
        <span className="text-[10px] leading-none font-medium tabular-nums text-white/50">
          {coreCount} cores
        </span>
      </GlassPill>

      {/* SVG Chart */}
      <svg
        className="w-full h-full"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
      >
        {/* Grid lines */}
        {gridLines.map((y, i) => (
          <line
            key={i}
            x1={0}
            y1={y}
            x2={width}
            y2={y}
            stroke={CHART_COLORS.grid}
          />
        ))}

        {/* Per-core lines */}
        {coreHistories.map((history, idx) => (
          <path
            key={idx}
            d={buildPath(history)}
            fill="none"
            stroke={CHART_COLORS.cores[idx % CHART_COLORS.cores.length]}
            strokeWidth={selectedCore !== null && selectedCore === idx ? "1.6" : "0.8"}
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeDasharray={selectedCore !== null && selectedCore !== idx ? "2 2" : undefined}
            strokeOpacity={selectedCore !== null && selectedCore !== idx ? 0.2 : 1}
            vectorEffect="non-scaling-stroke"
            shapeRendering="geometricPrecision"
          />
        ))}
      </svg>
    </div>
  );
}

// Simple area chart for Memory
function MemoryChart({
  data,
  percent,
  used,
  total,
  height = 80,
}: {
  data: number[];
  percent: number;
  used: number;
  total: number;
  height?: number;
}) {
  const width = 400;
  const padding = 2;
  const chartHeight = height - padding * 2;
  const maxPoints = 60;
  const snap = (value: number) => Math.round(value * 2) / 2;

  const paddedData = data.length < maxPoints
    ? [...Array(maxPoints - data.length).fill(0), ...data]
    : data.slice(-maxPoints);

  const pointSpacing = width / (maxPoints - 1);

  const areaPoints = paddedData
    .map((v, i) => {
      const x = snap(i * pointSpacing);
      const y = snap(padding + chartHeight - (Math.min(v, 100) / 100) * chartHeight);
      return `${x},${y}`;
    })
    .join(" L");

  const areaPath = `M${snap(0)},${snap(height)} L${snap(0)},${snap(padding + chartHeight - (Math.min(paddedData[0], 100) / 100) * chartHeight)} L${areaPoints} L${snap(width)},${snap(height)} Z`;

  const linePath = `M${paddedData
    .map((v, i) => {
      const x = snap(i * pointSpacing);
      const y = snap(padding + chartHeight - (Math.min(v, 100) / 100) * chartHeight);
      return `${x},${y}`;
    })
    .join(" L")}`;

  const gridLines = [0.5].map((p) => padding + chartHeight * (1 - p));

  return (
    <div className="relative h-full rounded-xl overflow-hidden bg-white/[0.02] border border-white/[0.04]">
      {/* Glass pill overlay */}
      <GlassPill position="top-left">
        <div className="flex items-center gap-2">
          <span className="text-[10px] leading-none font-medium uppercase tracking-wide text-white/50">MEM</span>
          <span className="text-[10px] leading-none font-semibold tabular-nums text-white/80">
            {percent.toFixed(0)}%
          </span>
        </div>
      </GlassPill>

      {/* Usage details - top right */}
      <GlassPill position="top-right">
        <span className="text-[10px] leading-none font-medium tabular-nums text-white/50">
          {formatBytes(used)} / {formatBytes(total)}
        </span>
      </GlassPill>

      {/* SVG Chart */}
      <svg
        className="w-full h-full"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
      >
        {/* Grid line */}
        {gridLines.map((y, i) => (
          <line
            key={i}
            x1={0}
            y1={y}
            x2={width}
            y2={y}
            stroke={CHART_COLORS.grid}
          />
        ))}

        {/* Area fill */}
        <path d={areaPath} fill={CHART_COLORS.primaryFill} />

        {/* Line */}
        <path
          d={linePath}
          fill="none"
          stroke={CHART_COLORS.primary}
          strokeWidth="0.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
          shapeRendering="geometricPrecision"
        />
      </svg>
    </div>
  );
}

// Network chart with dual lines (rx/tx)
function NetworkChart({
  rxData,
  txData,
  max,
  height = 80,
}: {
  rxData: number[];
  txData: number[];
  max: number;
  height?: number;
}) {
  const width = 400;
  const padding = 2;
  const chartHeight = height - padding * 2;
  const maxPoints = 60;
  const snap = (value: number) => Math.round(value * 2) / 2;

  const paddedRx = rxData.length < maxPoints
    ? [...Array(maxPoints - rxData.length).fill(0), ...rxData]
    : rxData.slice(-maxPoints);
  const paddedTx = txData.length < maxPoints
    ? [...Array(maxPoints - txData.length).fill(0), ...txData]
    : txData.slice(-maxPoints);

  const pointSpacing = width / (maxPoints - 1);

  const buildPath = (data: number[]) => {
    return `M${data
      .map((v, i) => {
        const x = snap(i * pointSpacing);
        const y = snap(padding + chartHeight - (Math.min(v, max) / max) * chartHeight);
        return `${x},${y}`;
      })
      .join(" L")}`;
  };

  const buildAreaPath = (data: number[]) => {
    const points = data
      .map((v, i) => {
        const x = snap(i * pointSpacing);
        const y = snap(padding + chartHeight - (Math.min(v, max) / max) * chartHeight);
        return `${x},${y}`;
      })
      .join(" L");
    return `M${snap(0)},${snap(height)} L${snap(0)},${snap(padding + chartHeight - (Math.min(data[0], max) / max) * chartHeight)} L${points} L${snap(width)},${snap(height)} Z`;
  };

  const gridLines = [0.5].map((p) => padding + chartHeight * (1 - p));

  const currentRx = rxData[rxData.length - 1] || 0;
  const currentTx = txData[txData.length - 1] || 0;

  return (
    <div className="relative h-full rounded-xl overflow-hidden bg-white/[0.02] border border-white/[0.04]">
      {/* Glass pill overlay - label */}
      <GlassPill position="top-left">
        <span className="text-[10px] leading-none font-medium uppercase tracking-wide text-white/50">NET</span>
      </GlassPill>

      {/* Network stats - top right */}
      <GlassPill position="top-right">
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1">
            <span className="text-[10px] leading-none text-white/40">↓</span>
            <span className="text-[10px] leading-none font-medium tabular-nums text-white/70">{formatBytesPerSec(currentRx)}</span>
          </div>
          <div className="flex items-center gap-1">
            <span className="text-[10px] leading-none text-white/40">↑</span>
            <span className="text-[10px] leading-none font-medium tabular-nums text-white/40">{formatBytesPerSec(currentTx)}</span>
          </div>
        </div>
      </GlassPill>

      {/* SVG Chart */}
      <svg
        className="w-full h-full"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
      >
        {/* Grid line */}
        {gridLines.map((y, i) => (
          <line
            key={i}
            x1={0}
            y1={y}
            x2={width}
            y2={y}
            stroke={CHART_COLORS.grid}
          />
        ))}

        {/* RX Area + Line (primary - indigo) */}
        <path d={buildAreaPath(paddedRx)} fill={CHART_COLORS.primaryFill} />
        <path
          d={buildPath(paddedRx)}
          fill="none"
          stroke={CHART_COLORS.primary}
          strokeWidth="0.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
          shapeRendering="geometricPrecision"
        />

        {/* TX Area + Line (secondary - white/muted) */}
        <path d={buildAreaPath(paddedTx)} fill={CHART_COLORS.secondaryFill} />
        <path
          d={buildPath(paddedTx)}
          fill="none"
          stroke={CHART_COLORS.secondary}
          strokeWidth="0.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
          shapeRendering="geometricPrecision"
        />
      </svg>
    </div>
  );
}

export function SystemMonitor({ className, intervalMs = 1000 }: SystemMonitorProps) {
  const [connectionState, setConnectionState] = useState<ConnectionState>("connecting");
  const [metrics, setMetrics] = useState<SystemMetrics | null>(null);
  const [cpuHistory, setCpuHistory] = useState<number[]>([]);
  const [coreHistories, setCoreHistories] = useState<number[][]>([]);
  const [memoryHistory, setMemoryHistory] = useState<number[]>([]);
  const [networkRxHistory, setNetworkRxHistory] = useState<number[]>([]);
  const [networkTxHistory, setNetworkTxHistory] = useState<number[]>([]);

  const wsRef = useRef<WebSocket | null>(null);
  const connectionIdRef = useRef(0);
  const maxHistory = 60;

  // Build WebSocket URL
  const buildWsUrl = useCallback(() => {
    const baseUrl = getRuntimeApiBase();
    const wsUrl = baseUrl
      .replace("https://", "wss://")
      .replace("http://", "ws://");

    const params = new URLSearchParams({
      interval_ms: intervalMs.toString(),
    });

    return `${wsUrl}/api/monitoring/ws?${params}`;
  }, [intervalMs]);

  // Connect to WebSocket
  const connect = useCallback(() => {
    if (wsRef.current) {
      wsRef.current.close();
    }

    connectionIdRef.current += 1;
    const thisConnectionId = connectionIdRef.current;

    setConnectionState("connecting");

    const url = buildWsUrl();
    const jwt = getValidJwt();
    const token = jwt?.token ?? null;

    const protocols = token ? ["openagent", `jwt.${token}`] : ["openagent"];
    const ws = new WebSocket(url, protocols);

    ws.onopen = () => {
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("connected");
    };

    ws.onmessage = (event) => {
      if (connectionIdRef.current !== thisConnectionId) return;
      if (typeof event.data === "string") {
        try {
          const parsed = JSON.parse(event.data);

          // Check if this is a history snapshot
          if (parsed.type === "history" && Array.isArray(parsed.history)) {
            const historyData: SystemMetrics[] = parsed.history;
            if (historyData.length > 0) {
              // Set the latest metrics
              setMetrics(historyData[historyData.length - 1]);

              // Populate histories from snapshot
              setCpuHistory(historyData.map((m) => m.cpu_percent));

              // Build per-core histories
              const coreCount = historyData[0]?.cpu_cores?.length || 0;
              const cores: number[][] = Array.from({ length: coreCount }, () => []);
              for (const m of historyData) {
                m.cpu_cores.forEach((v, idx) => {
                  if (cores[idx]) cores[idx].push(v);
                });
              }
              setCoreHistories(cores);

              setMemoryHistory(historyData.map((m) => m.memory_percent));
              setNetworkRxHistory(historyData.map((m) => m.network_rx_bytes_per_sec));
              setNetworkTxHistory(historyData.map((m) => m.network_tx_bytes_per_sec));
            }
            return;
          }

          // Regular metrics update
          const data: SystemMetrics = parsed;
          setMetrics(data);

          // Update CPU history
          setCpuHistory((prev) => {
            const next = [...prev, data.cpu_percent];
            return next.slice(-maxHistory);
          });

          // Update per-core histories
          setCoreHistories((prev) => {
            const newHistories = data.cpu_cores.map((corePercent, idx) => {
              const existing = prev[idx] || [];
              return [...existing, corePercent].slice(-maxHistory);
            });
            return newHistories;
          });

          // Update memory history
          setMemoryHistory((prev) => {
            const next = [...prev, data.memory_percent];
            return next.slice(-maxHistory);
          });

          // Update network histories
          setNetworkRxHistory((prev) => {
            const next = [...prev, data.network_rx_bytes_per_sec];
            return next.slice(-maxHistory);
          });
          setNetworkTxHistory((prev) => {
            const next = [...prev, data.network_tx_bytes_per_sec];
            return next.slice(-maxHistory);
          });
        } catch {
          // Ignore parse errors
        }
      }
    };

    ws.onerror = () => {
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("error");
    };

    ws.onclose = () => {
      if (connectionIdRef.current !== thisConnectionId) return;
      setConnectionState("disconnected");
    };

    wsRef.current = ws;
  }, [buildWsUrl]);

  // Connect on mount
  useEffect(() => {
    connect();

    return () => {
      connectionIdRef.current += 1;
      wsRef.current?.close();
    };
  }, [connect]);

  // Auto-reconnect on disconnect
  useEffect(() => {
    if (connectionState === "disconnected" || connectionState === "error") {
      const timeout = setTimeout(() => {
        connect();
      }, 2000);
      return () => clearTimeout(timeout);
    }
  }, [connectionState, connect]);

  // Calculate max for network chart
  const maxNetworkRate = Math.max(
    ...networkRxHistory,
    ...networkTxHistory,
    1024 * 10
  ) * 1.2;

  // Show connection status if not connected
  if (connectionState !== "connected") {
    return (
      <div className={cn("flex items-center justify-center h-full", className)}>
        <div className="flex items-center gap-2 text-sm text-white/30">
          <Activity className="h-4 w-4 animate-pulse" />
          {connectionState === "connecting"
            ? "Connecting..."
            : connectionState === "error"
            ? "Connection error"
            : "Reconnecting..."}
        </div>
      </div>
    );
  }

  return (
    <div className={cn("flex flex-col gap-3 h-full", className)}>
      {/* CPU - Full width at top */}
      <div className="flex-[1.2]">
        <CpuChart
          coreHistories={coreHistories}
          avgPercent={metrics?.cpu_percent ?? 0}
          coreCount={metrics?.cpu_cores.length ?? 0}
          height={100}
        />
      </div>

      {/* Memory and Network - Split bottom */}
      <div className="flex-1 grid grid-cols-2 gap-3">
        <MemoryChart
          data={memoryHistory}
          percent={metrics?.memory_percent ?? 0}
          used={metrics?.memory_used ?? 0}
          total={metrics?.memory_total ?? 0}
          height={80}
        />
        <NetworkChart
          rxData={networkRxHistory}
          txData={networkTxHistory}
          max={maxNetworkRate}
          height={80}
        />
      </div>
    </div>
  );
}
