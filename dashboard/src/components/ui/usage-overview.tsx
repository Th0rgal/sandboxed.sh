'use client';

/**
 * UsageOverview — compact, analytics-page-styled summary of AI usage.
 *
 * Layout (top to bottom, all in one card stack):
 *   1. Header row: title + window picker (24h / 7d / 30d / All).
 *   2. Four metric tiles (Spend, Input, Output, Cache hit) on one row.
 *   3. Two-column row: cost-over-time bar chart + provider mix legend.
 *   4. Compact model table (rank, swatch + model, calls, tokens, spend, share).
 *
 * The visual tokens match `/analytics`:
 *   - card: `bg-white/[0.02] border border-white/[0.06] rounded-xl`
 *   - metric value: `text-2xl font-semibold font-mono tabular-nums`
 *   - axis bar: indigo-500/50 with weekend opacity dim
 *   - section label: `text-xs uppercase tracking-[0.08em] text-white/40`
 *
 * Data: `GET /api/ai/usage/summary?window=<window>` returns
 * `{ totals, by_model, by_day }`. The sparkline aligns its day series to
 * the selected window so bars always fill the chart even on sparse data.
 */

import { useMemo, useState, useRef, useCallback } from 'react';
import useSWR from 'swr';
import {
  getUsageSummary,
  type DailyUsage,
  type HourlyUsage,
  type ModelUsageSummary,
  type UsageSummary,
  type UsageWindow,
} from '@/lib/api';
import { cn, formatCents } from '@/lib/utils';
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Calendar,
  Database,
  DollarSign,
} from 'lucide-react';

const WINDOWS: { id: UsageWindow; label: string }[] = [
  { id: '24h', label: '24h' },
  { id: '7d', label: '7d' },
  { id: '30d', label: '30d' },
  { id: 'all', label: 'All' },
];

const PROVIDER_COLOR: Record<string, string> = {
  anthropic: '#d97757',
  openai: '#10a37f',
  google: '#4285f4',
  xai: '#e2e8f0',
  zai: '#22d3ee',
  minimax: '#14b8a6',
  mistral: '#6366f1',
  groq: '#ec4899',
  'open-router': '#a855f7',
  cohere: '#f43f5e',
  perplexity: '#06b6d4',
  'github-copilot': '#9ca3af',
  unknown: '#52525b',
};
function providerColor(id?: string | null): string {
  return PROVIDER_COLOR[id || 'unknown'] || PROVIDER_COLOR.unknown;
}

function fmtCompact(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}K`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

/** Number of bars to draw for each window. The data may have fewer days; we
 * fill gaps with zero-height placeholders so the axis width is stable. */
/** A single data point on the cost-over-time chart. `ts` is a UTC timestamp,
 * `label` is what we render on the X axis. */
type ChartPoint = {
  ts: Date;
  cost_cents: number;
  requests: number;
  bucket: string; // raw bucket key (day or hour)
};

/** Build a continuous series at the granularity that best fits the window.
 *  - 24h → 24 hourly buckets ending now
 *  - 7d  → 168 hourly buckets (preferred) or 7 days fallback
 *  - 30d → 30 daily buckets
 *  - all → up to 60 daily buckets from the actual data tail
 */
function buildSeries(
  byDay: DailyUsage[],
  byHour: HourlyUsage[],
  windowKey: UsageWindow
): { points: ChartPoint[]; granularity: 'hour' | 'day' } {
  const dayMap = new Map(byDay.map((d) => [d.day, d]));
  const hourMap = new Map(byHour.map((h) => [h.hour, h]));
  const now = new Date();

  if (windowKey === '24h' && byHour.length > 0) {
    const out: ChartPoint[] = [];
    for (let i = 23; i >= 0; i--) {
      const d = new Date(now);
      d.setUTCMinutes(0, 0, 0);
      d.setUTCHours(d.getUTCHours() - i);
      const key = `${d.toISOString().slice(0, 13)}`;
      const found = hourMap.get(key);
      out.push({
        ts: d,
        bucket: key,
        cost_cents: found?.cost_cents ?? 0,
        requests: found?.requests ?? 0,
      });
    }
    return { points: out, granularity: 'hour' };
  }

  if (windowKey === '7d' && byHour.length > 0) {
    const out: ChartPoint[] = [];
    for (let i = 7 * 24 - 1; i >= 0; i--) {
      const d = new Date(now);
      d.setUTCMinutes(0, 0, 0);
      d.setUTCHours(d.getUTCHours() - i);
      const key = `${d.toISOString().slice(0, 13)}`;
      const found = hourMap.get(key);
      out.push({
        ts: d,
        bucket: key,
        cost_cents: found?.cost_cents ?? 0,
        requests: found?.requests ?? 0,
      });
    }
    return { points: out, granularity: 'hour' };
  }

  // Daily granularity for 24h/7d fallback, 30d, all.
  const count =
    windowKey === '24h' ? 1 : windowKey === '7d' ? 7 : windowKey === '30d' ? 30 : 60;
  if (windowKey === 'all' && byDay.length > 0) {
    const slice = byDay.slice(-count);
    return {
      points: slice.map((d) => ({
        ts: new Date(d.day + 'T00:00:00Z'),
        bucket: d.day,
        cost_cents: d.cost_cents,
        requests: d.requests,
      })),
      granularity: 'day',
    };
  }
  const out: ChartPoint[] = [];
  for (let i = count - 1; i >= 0; i--) {
    const d = new Date(now);
    d.setUTCHours(0, 0, 0, 0);
    d.setUTCDate(d.getUTCDate() - i);
    const key = d.toISOString().slice(0, 10);
    const found = dayMap.get(key);
    out.push({
      ts: d,
      bucket: key,
      cost_cents: found?.cost_cents ?? 0,
      requests: found?.requests ?? 0,
    });
  }
  return { points: out, granularity: 'day' };
}

// ─── Tiles ───────────────────────────────────────────────────────────────────

function MetricTile({
  icon,
  label,
  value,
  sub,
}: {
  icon: React.ReactNode;
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
}) {
  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-3.5"
      data-testid="usage-metric"
    >
      <div className="flex items-center gap-2 mb-1.5">
        {icon}
        <span className="text-[11px] text-white/50">{label}</span>
      </div>
      <div className="text-xl font-semibold text-white font-mono tabular-nums leading-tight">
        {value}
      </div>
      {sub && <div className="text-[11px] text-white/40 mt-0.5 truncate">{sub}</div>}
    </div>
  );
}

// ─── Cost-over-time sparkline ────────────────────────────────────────────────

/** Build a quadratic-bezier-smoothed SVG path string. */
function buildSmoothPath(
  points: { x: number; y: number }[],
  baselineY: number,
  close = false
): string {
  if (points.length === 0) return '';
  if (points.length === 1) {
    const p = points[0];
    return close
      ? `M ${p.x} ${baselineY} L ${p.x} ${p.y} L ${p.x} ${baselineY} Z`
      : `M ${p.x} ${p.y}`;
  }
  let d = `M ${points[0].x} ${points[0].y}`;
  for (let i = 0; i < points.length - 1; i++) {
    const p = points[i];
    const n = points[i + 1];
    const mx = (p.x + n.x) / 2;
    const my = (p.y + n.y) / 2;
    d += ` Q ${p.x} ${p.y} ${mx} ${my}`;
  }
  const last = points[points.length - 1];
  d += ` T ${last.x} ${last.y}`;
  if (close) {
    d += ` L ${last.x} ${baselineY} L ${points[0].x} ${baselineY} Z`;
  }
  return d;
}

function fmtAxisLabel(ts: Date, gran: 'hour' | 'day'): string {
  if (gran === 'hour') {
    return `${ts.getUTCHours().toString().padStart(2, '0')}:00`;
  }
  const m = ts.toLocaleString('en-US', { month: 'short', timeZone: 'UTC' });
  return `${m} ${ts.getUTCDate()}`;
}

function fmtPointLabel(ts: Date, gran: 'hour' | 'day'): string {
  if (gran === 'hour') {
    const day = ts.toLocaleString('en-US', { month: 'short', day: 'numeric', timeZone: 'UTC' });
    return `${day} · ${ts.getUTCHours().toString().padStart(2, '0')}:00 UTC`;
  }
  return ts.toLocaleString('en-US', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    timeZone: 'UTC',
  });
}

function CostAreaChart({
  byDay,
  byHour,
  windowKey,
}: {
  byDay: DailyUsage[];
  byHour: HourlyUsage[];
  windowKey: UsageWindow;
}) {
  const { points, granularity } = useMemo(
    () => buildSeries(byDay, byHour, windowKey),
    [byDay, byHour, windowKey]
  );
  const totalCost = useMemo(
    () => points.reduce((s, p) => s + p.cost_cents, 0),
    [points]
  );
  const maxCost = useMemo(
    () => points.reduce((m, p) => Math.max(m, p.cost_cents), 0),
    [points]
  );
  const rangeLabel = windowKey === 'all' ? 'All' : windowKey;

  // Internal SVG coordinates; preserveAspectRatio="none" stretches them.
  const W = 600;
  const H = 180;
  const padX = 4;
  const padTop = 6;
  const padBottom = 4;
  const chartW = W - padX * 2;
  const chartH = H - padTop - padBottom;
  const baselineY = padTop + chartH;

  const linePoints = useMemo(
    () =>
      points.map((p, i) => {
        const x =
          points.length <= 1
            ? W / 2
            : padX + (i / (points.length - 1)) * chartW;
        const y =
          padTop + chartH - (maxCost > 0 ? p.cost_cents / maxCost : 0) * chartH;
        return { x, y };
      }),
    [points, maxCost, chartW, chartH, padX, padTop, baselineY]
  );
  const linePath = useMemo(
    () => buildSmoothPath(linePoints, baselineY, false),
    [linePoints, baselineY]
  );
  const fillPath = useMemo(
    () => buildSmoothPath(linePoints, baselineY, true),
    [linePoints, baselineY]
  );

  const gridYs = [0.25, 0.5, 0.75, 1].map((f) => padTop + chartH - f * chartH);
  const tickCount = granularity === 'hour' ? (windowKey === '24h' ? 6 : 7) : 6;
  const tickIdxs: number[] = [];
  if (points.length > 0) {
    const visibleTickCount = Math.min(tickCount, points.length);
    for (let i = 0; i < visibleTickCount; i++) {
      const t = Math.max(1, visibleTickCount - 1);
      const idx = Math.round((i / t) * (points.length - 1));
      tickIdxs.push(idx);
    }
  }

  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  const svgRef = useRef<SVGSVGElement | null>(null);

  const onMove = useCallback(
    (e: React.MouseEvent<SVGSVGElement>) => {
      if (!svgRef.current || points.length === 0) return;
      const pt = svgRef.current.createSVGPoint();
      pt.x = e.clientX;
      pt.y = e.clientY;
      const ctm = svgRef.current.getScreenCTM();
      if (!ctm) return;
      const local = pt.matrixTransform(ctm.inverse());
      const x = Math.max(padX, Math.min(W - padX, local.x));
      const ratio = (x - padX) / chartW;
      const idx = Math.round(ratio * (points.length - 1));
      setHoverIdx(idx);
    },
    [points.length, chartW, padX, W]
  );
  const onLeave = useCallback(() => setHoverIdx(null), []);

  const hovered = hoverIdx != null ? points[hoverIdx] : null;
  const hoveredPoint = hoverIdx != null ? linePoints[hoverIdx] : null;

  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4 h-full flex flex-col"
      data-testid="usage-chart"
    >
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2 text-sm font-medium text-white">
          <Calendar className="h-4 w-4 text-white/50" />
          <span>Cost over time</span>
        </div>
        <div className="font-mono text-[11px] text-white/40 tabular-nums">
          {formatCents(totalCost)} · {rangeLabel}
        </div>
      </div>

      <div className="flex-1 min-h-[9rem] relative" data-testid="usage-chart-area">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          className="w-full h-full overflow-visible"
          onMouseMove={onMove}
          onMouseLeave={onLeave}
        >
          <defs>
            <linearGradient id="usage-fill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="rgb(99,102,241)" stopOpacity="0.45" />
              <stop offset="100%" stopColor="rgb(99,102,241)" stopOpacity="0" />
            </linearGradient>
          </defs>

          {gridYs.map((y, i) => (
            <line
              key={i}
              x1={padX}
              x2={W - padX}
              y1={y}
              y2={y}
              stroke="rgba(255,255,255,0.045)"
              strokeWidth={1}
              vectorEffect="non-scaling-stroke"
            />
          ))}

          {maxCost > 0 && (
            <>
              <path d={fillPath} fill="url(#usage-fill)" />
              <path
                d={linePath}
                fill="none"
                stroke="rgb(129,140,248)"
                strokeWidth={1.5}
                strokeLinecap="round"
                strokeLinejoin="round"
                vectorEffect="non-scaling-stroke"
              />
            </>
          )}

          {hovered && hoveredPoint && (
            <g pointerEvents="none">
              <line
                x1={hoveredPoint.x}
                x2={hoveredPoint.x}
                y1={padTop}
                y2={padTop + chartH}
                stroke="rgba(255,255,255,0.2)"
                strokeWidth={1}
                vectorEffect="non-scaling-stroke"
              />
              <circle
                cx={hoveredPoint.x}
                cy={hoveredPoint.y}
                r={3.5}
                fill="rgb(129,140,248)"
                stroke="rgba(15,15,25,0.85)"
                strokeWidth={2}
                vectorEffect="non-scaling-stroke"
              />
            </g>
          )}
        </svg>

        {hovered && hoveredPoint && (
          <div
            className="pointer-events-none absolute z-10 rounded-md border border-white/[0.08] bg-black/85 backdrop-blur px-2 py-1.5 text-[10px] shadow-lg"
            style={{
              left: `${(hoveredPoint.x / W) * 100}%`,
              top: 0,
              transform: 'translate(-50%, -10%)',
            }}
            data-testid="usage-chart-tooltip"
          >
            <div className="font-medium text-white/85 whitespace-nowrap">
              {fmtPointLabel(hovered.ts, granularity)}
            </div>
            <div className="mt-0.5 flex gap-3 font-mono tabular-nums whitespace-nowrap">
              <span className="text-indigo-300">{formatCents(hovered.cost_cents)}</span>
              <span className="text-white/40">{fmtCompact(hovered.requests)} req</span>
            </div>
          </div>
        )}
      </div>

      <div className="relative mt-1 h-3 text-[9px] text-white/30 tabular-nums">
        {tickIdxs.map((idx, i) => {
          const p = points[idx];
          if (!p) return null;
          const ratio = points.length <= 1 ? 0.5 : idx / (points.length - 1);
          const transform =
            i === 0
              ? 'translate(0, 0)'
              : i === tickIdxs.length - 1
              ? 'translate(-100%, 0)'
              : 'translate(-50%, 0)';
          return (
            <span
              key={idx}
              className="absolute top-0 whitespace-nowrap"
              style={{ left: `${ratio * 100}%`, transform }}
            >
              {fmtAxisLabel(p.ts, granularity)}
            </span>
          );
        })}
      </div>
    </div>
  );
}

// ─── Provider distribution card ──────────────────────────────────────────────

function ProviderDistribution({ models }: { models: ModelUsageSummary[] }) {
  const { entries, total, unit } = useMemo(() => {
    const byCost = new Map<string, number>();
    for (const m of models) {
      const k = m.provider || 'unknown';
      byCost.set(k, (byCost.get(k) || 0) + m.cost_cents);
    }
    let total = Array.from(byCost.values()).reduce((a, b) => a + b, 0);
    if (total === 0) {
      const byReq = new Map<string, number>();
      for (const m of models) {
        const k = m.provider || 'unknown';
        byReq.set(k, (byReq.get(k) || 0) + m.requests);
      }
      total = Array.from(byReq.values()).reduce((a, b) => a + b, 0);
      return {
        entries: Array.from(byReq.entries())
          .map(([provider, value]) => ({ provider, value }))
          .sort((a, b) => b.value - a.value),
        total,
        unit: 'req' as const,
      };
    }
    return {
      entries: Array.from(byCost.entries())
        .map(([provider, value]) => ({ provider, value }))
        .sort((a, b) => b.value - a.value),
      total,
      unit: 'cost' as const,
    };
  }, [models]);

  // Collapse the long tail into a single "Other" row so the legend never
  // dominates the layout when there are many tiny providers (<1% each).
  // The stacked bar above still shows the true distribution.
  const TOP_N = 5;
  const display = useMemo(() => {
    if (entries.length <= TOP_N + 1) return entries;
    const top = entries.slice(0, TOP_N);
    const rest = entries.slice(TOP_N);
    const restTotal = rest.reduce((s, e) => s + e.value, 0);
    if (restTotal === 0) return top;
    return [
      ...top,
      { provider: '__other__', value: restTotal, count: rest.length } as
        | { provider: string; value: number }
        | { provider: string; value: number; count: number },
    ];
  }, [entries]);

  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4 h-full flex flex-col"
      data-testid="usage-distribution"
    >
      <div className="flex items-center justify-between mb-3">
        <div className="text-sm font-medium text-white">By provider</div>
        <span className="font-mono text-[10px] text-white/40 tabular-nums">
          {entries.length} · {unit === 'cost' ? 'spend' : 'requests'}
        </span>
      </div>

      {total === 0 ? (
        <div className="flex-1 flex items-center justify-center text-[11px] text-white/30">
          No data
        </div>
      ) : (
        <>
          {/* Stacked bar — keep the long-tail providers visible here */}
          <div className="flex h-1.5 overflow-hidden rounded-full bg-white/[0.04]">
            {entries.map(({ provider, value }) => {
              const pct = (value / total) * 100;
              return (
                <div
                  key={provider}
                  className="h-full"
                  style={{ width: `${pct}%`, backgroundColor: providerColor(provider) }}
                  title={`${provider} — ${pct.toFixed(1)}%`}
                />
              );
            })}
          </div>

          {/* Legend rows */}
          <div className="mt-3 flex-1 space-y-1.5 min-h-0">
            {display.map((row) => {
              const pct = (row.value / total) * 100;
              const isOther = row.provider === '__other__';
              const count = (row as { count?: number }).count;
              return (
                <div
                  key={row.provider}
                  className="grid grid-cols-[minmax(0,1fr)_3.5rem_2.25rem] items-center gap-2 text-[11px]"
                >
                  <span className="flex items-center gap-2 text-white/65 truncate">
                    <span
                      className={cn(
                        'h-1.5 w-1.5 flex-shrink-0 rounded-sm',
                        isOther && 'opacity-30'
                      )}
                      style={
                        isOther
                          ? { backgroundColor: '#a1a1aa' }
                          : { backgroundColor: providerColor(row.provider) }
                      }
                    />
                    <span className="truncate">
                      {isOther
                        ? `Other${count ? ` (${count})` : ''}`
                        : row.provider}
                    </span>
                  </span>
                  <span className="text-right font-mono text-white/55 tabular-nums">
                    {unit === 'cost' ? formatCents(row.value) : fmtCompact(row.value)}
                  </span>
                  <span className="text-right font-mono text-white/30 tabular-nums">
                    {pct.toFixed(pct < 10 ? 1 : 0)}%
                  </span>
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

// ─── Model table ─────────────────────────────────────────────────────────────

function ModelTable({
  models,
  totalRequests,
}: {
  models: ModelUsageSummary[];
  totalRequests: number;
}) {
  const sorted = useMemo(
    () =>
      [...models]
        .filter((m) => m.requests > 0)
        .sort((a, b) => b.cost_cents - a.cost_cents || b.requests - a.requests)
        .slice(0, 10),
    [models]
  );
  const maxRequests = sorted[0]?.requests || 1;

  if (sorted.length === 0) {
    return (
      <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
        <div className="text-sm font-medium text-white mb-3">By model</div>
        <div className="rounded-md border border-white/[0.05] bg-white/[0.01] px-3 py-6 text-center text-xs text-white/40">
          No model usage recorded.
        </div>
      </div>
    );
  }

  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4"
      data-testid="usage-model-table"
    >
      <div className="flex items-center justify-between mb-3">
        <div className="text-sm font-medium text-white">By model</div>
        <span className="font-mono text-[10px] text-white/40 tabular-nums">
          {models.length} {models.length === 1 ? 'model' : 'models'}
        </span>
      </div>
      <div className="overflow-hidden rounded-md border border-white/[0.05]">
        <div className="grid grid-cols-[1.5rem_minmax(0,1fr)_5rem_5.5rem_5rem_3.5rem] gap-x-3 border-b border-white/[0.05] bg-white/[0.02] px-3 py-1.5 text-[10px] uppercase tracking-[0.08em] text-white/40">
          <span>#</span>
          <span>Model</span>
          <span className="text-right">Calls</span>
          <span className="text-right">Tokens</span>
          <span className="text-right">Spend</span>
          <span className="text-right">Share</span>
        </div>
        {sorted.map((m, idx) => {
          const totalTokens =
            m.input_tokens + m.output_tokens + m.cache_read_tokens + m.cache_creation_tokens;
          const pctOfRequests =
            totalRequests > 0 ? (m.requests / totalRequests) * 100 : 0;
          const barWidth = Math.max(2, (m.requests / maxRequests) * 100);
          return (
            <div
              key={m.model + idx}
              className="group relative grid grid-cols-[1.5rem_minmax(0,1fr)_5rem_5.5rem_5rem_3.5rem] gap-x-3 border-b border-white/[0.04] px-3 py-1.5 last:border-b-0 hover:bg-white/[0.015]"
              data-testid="usage-model-row"
            >
              <div
                className="pointer-events-none absolute inset-y-0 left-0 opacity-[0.07] transition-opacity group-hover:opacity-[0.13]"
                style={{ width: `${barWidth}%`, backgroundColor: providerColor(m.provider) }}
              />
              <span className="relative font-mono text-[11px] text-white/35 tabular-nums">
                {idx + 1}
              </span>
              <div className="relative flex items-center gap-2 min-w-0">
                <span
                  className="h-1.5 w-1.5 flex-shrink-0 rounded-sm"
                  style={{ backgroundColor: providerColor(m.provider) }}
                />
                <span className="truncate text-[12px] text-white/85">
                  {m.model || 'unknown'}
                </span>
              </div>
              <span className="relative text-right font-mono text-[11px] text-white/70 tabular-nums">
                {fmtCompact(m.requests)}
              </span>
              <span className="relative text-right font-mono text-[11px] text-white/55 tabular-nums">
                {fmtCompact(totalTokens)}
              </span>
              <span className="relative text-right font-mono text-[11px] text-white/85 tabular-nums">
                {formatCents(m.cost_cents)}
              </span>
              <span className="relative text-right font-mono text-[11px] text-white/40 tabular-nums">
                {pctOfRequests.toFixed(pctOfRequests < 10 ? 1 : 0)}%
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ─── Skeleton ────────────────────────────────────────────────────────────────

function ShimmerBar({ className }: { className?: string }) {
  return (
    <div
      className={cn(
        'relative overflow-hidden rounded bg-white/[0.04]',
        className
      )}
    >
      <div className="absolute inset-0 -translate-x-full animate-[shimmer_1.6s_infinite] bg-gradient-to-r from-transparent via-white/[0.06] to-transparent" />
    </div>
  );
}

function UsageSkeleton() {
  return (
    <div className="space-y-3" data-testid="usage-skeleton">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div
            key={i}
            className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-3.5"
          >
            <ShimmerBar className="h-3 w-20" />
            <ShimmerBar className="mt-2 h-6 w-24" />
            <ShimmerBar className="mt-2 h-3 w-16" />
          </div>
        ))}
      </div>
      <div className="grid gap-3 md:grid-cols-3">
        <div className="md:col-span-2 bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <div className="flex items-center justify-between mb-3">
            <ShimmerBar className="h-4 w-28" />
            <ShimmerBar className="h-3 w-20" />
          </div>
          <div className="h-28 flex items-end gap-[2px]">
            {Array.from({ length: 14 }).map((_, i) => (
              <div
                key={i}
                className="flex-1 rounded-sm bg-white/[0.04]"
                style={{ height: `${20 + ((i * 53) % 75)}%` }}
              />
            ))}
          </div>
        </div>
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
          <ShimmerBar className="h-4 w-24" />
          <ShimmerBar className="mt-3 h-1.5 w-full rounded-full" />
          <div className="mt-3 space-y-2">
            {Array.from({ length: 4 }).map((_, i) => (
              <ShimmerBar key={i} className="h-3 w-full" />
            ))}
          </div>
        </div>
      </div>
      <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4">
        <ShimmerBar className="h-4 w-20 mb-3" />
        <div className="space-y-1.5">
          {Array.from({ length: 6 }).map((_, i) => (
            <ShimmerBar key={i} className="h-5 w-full" />
          ))}
        </div>
      </div>
    </div>
  );
}

// ─── Top-level component ────────────────────────────────────────────────────

export interface UsageOverviewProps {
  window: UsageWindow;
  onWindowChange: (w: UsageWindow) => void;
}

export function UsageOverview({ window, onWindowChange }: UsageOverviewProps) {
  const { data, isLoading, error } = useSWR<UsageSummary>(
    ['ai-usage-summary', window],
    () => getUsageSummary(window),
    { revalidateOnFocus: false }
  );

  const totals = data?.totals;
  const cacheHitRate = useMemo(() => {
    if (!totals) return 0;
    const denom = totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
    if (denom === 0) return 0;
    return (totals.cache_read_tokens / denom) * 100;
  }, [totals]);

  const hasData = !!data && (data.by_model.length > 0 || (totals?.requests ?? 0) > 0);

  return (
    <div className="space-y-3" data-testid="usage-overview">
      {/* Header row */}
      <div className="flex items-center justify-between">
        <div className="flex items-baseline gap-3 min-w-0">
          <h2 className="text-sm font-medium text-white">Usage</h2>
          <p className="text-xs text-white/40 truncate">
            Token consumption and cost across every mission
          </p>
        </div>
        <div
          className="flex items-center gap-0.5 rounded-md border border-white/[0.06] bg-white/[0.02] p-0.5 flex-shrink-0"
          role="tablist"
          aria-label="Usage time window"
        >
          {WINDOWS.map((w) => (
            <button
              key={w.id}
              type="button"
              onClick={() => onWindowChange(w.id)}
              role="tab"
              aria-selected={window === w.id}
              data-testid={`usage-window-${w.id}`}
              className={cn(
                'rounded px-2 py-0.5 text-[11px] font-medium transition-colors cursor-pointer',
                window === w.id
                  ? 'bg-indigo-500/20 text-indigo-300'
                  : 'text-white/50 hover:text-white/80'
              )}
            >
              {w.label}
            </button>
          ))}
        </div>
      </div>

      {error ? (
        <div className="bg-white/[0.02] border border-red-500/20 rounded-xl p-4 text-xs text-red-300/80">
          Failed to load usage data.
        </div>
      ) : isLoading || !data ? (
        <UsageSkeleton />
      ) : !hasData ? (
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl px-5 py-10 text-center">
          <div className="text-sm text-white/55">No usage recorded in this window.</div>
          <div className="mt-1 text-xs text-white/30">
            Run a mission to populate this summary.
          </div>
        </div>
      ) : (
        <>
          {/* Top metric tiles */}
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
            <MetricTile
              icon={<DollarSign className="h-3.5 w-3.5 text-emerald-400" />}
              label="Total spend"
              value={formatCents(totals!.cost_cents)}
              sub={`${fmtCompact(totals!.requests)} calls`}
            />
            <MetricTile
              icon={<ArrowDownToLine className="h-3.5 w-3.5 text-indigo-400" />}
              label="Input tokens"
              value={fmtCompact(totals!.input_tokens)}
              sub={`+${fmtCompact(totals!.cache_read_tokens)} from cache`}
            />
            <MetricTile
              icon={<ArrowUpFromLine className="h-3.5 w-3.5 text-amber-400" />}
              label="Output tokens"
              value={fmtCompact(totals!.output_tokens)}
              sub={
                totals!.requests > 0
                  ? `${fmtCompact(Math.round(totals!.output_tokens / totals!.requests))} avg per call`
                  : '—'
              }
            />
            <MetricTile
              icon={<Database className="h-3.5 w-3.5 text-cyan-400" />}
              label="Cache hit rate"
              value={`${cacheHitRate.toFixed(0)}%`}
              sub={`${fmtCompact(totals!.cache_read_tokens)} tokens reused`}
            />
          </div>

          {/* Chart + provider distribution — equal-height row */}
          <div className="grid gap-3 md:grid-cols-3 md:items-stretch">
            <div className="md:col-span-2">
              <CostAreaChart
                byDay={data.by_day}
                byHour={data.by_hour ?? []}
                windowKey={window}
              />
            </div>
            <div className="md:col-span-1">
              <ProviderDistribution models={data.by_model} />
            </div>
          </div>

          {/* Model table */}
          <ModelTable models={data.by_model} totalRequests={totals!.requests} />
        </>
      )}
    </div>
  );
}
