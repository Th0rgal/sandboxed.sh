'use client';

/**
 * UsageOverview — usage summary card matching the design language of the
 * Analytics page (`/analytics`):
 *  - The same four-tile metric grid (icon · label · large mono value · sub),
 *  - A 14-day cost sparkline rendered with hand-rolled flex bars (no chart lib),
 *  - A thin provider-mix strip + per-model table.
 *
 * Data: `GET /api/ai/usage/summary?window=<window>`.
 */

import { useMemo } from 'react';
import useSWR from 'swr';
import {
  getUsageSummary,
  type DailyUsage,
  type ModelUsageSummary,
  type UsageSummary,
  type UsageWindow,
} from '@/lib/api';
import { cn, formatCents } from '@/lib/utils';
import { Activity, ArrowDownToLine, ArrowUpFromLine, Calendar, DollarSign, Database } from 'lucide-react';

const WINDOWS: { id: UsageWindow; label: string }[] = [
  { id: '24h', label: '24h' },
  { id: '7d', label: '7d' },
  { id: '30d', label: '30d' },
  { id: 'all', label: 'All time' },
];

/** Single hex per provider; reused for both the distribution bar and the
 * per-model swatch. Matches the analytics palette (semantic indigo for the
 * sparkline; one accent per provider for the strip). */
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
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4"
      data-testid="usage-metric"
    >
      <div className="flex items-center gap-2 mb-2">
        {icon}
        <span className="text-xs text-white/50">{label}</span>
      </div>
      <div className="text-2xl font-semibold text-white font-mono tabular-nums">
        {value}
      </div>
      {sub && <div className="text-xs text-white/40 mt-1">{sub}</div>}
    </div>
  );
}

// ─── Sparkline / cost-over-time bar chart ────────────────────────────────────

function buildDailySeries(
  byDay: DailyUsage[],
  windowKey: UsageWindow
): { day: string; cost_cents: number; requests: number }[] {
  // Build a contiguous series for visualisation: one entry per day in the
  // selected window. Gaps in the data become 0-height bars so we always show
  // a stable axis width.
  const daysCount = windowKey === '24h' ? 14 : windowKey === '7d' ? 14 : windowKey === '30d' ? 30 : 30;
  const map = new Map(byDay.map((d) => [d.day, d]));
  const out: { day: string; cost_cents: number; requests: number }[] = [];
  const now = new Date();
  for (let i = daysCount - 1; i >= 0; i--) {
    const d = new Date(now);
    d.setUTCDate(now.getUTCDate() - i);
    const key = d.toISOString().slice(0, 10);
    const found = map.get(key);
    out.push({
      day: key,
      cost_cents: found?.cost_cents ?? 0,
      requests: found?.requests ?? 0,
    });
  }
  return out;
}

function CostSparkline({
  byDay,
  windowKey,
}: {
  byDay: DailyUsage[];
  windowKey: UsageWindow;
}) {
  const series = useMemo(() => buildDailySeries(byDay, windowKey), [byDay, windowKey]);
  const maxCost = useMemo(
    () => series.reduce((m, d) => Math.max(m, d.cost_cents), 0),
    [series]
  );
  const totalCost = useMemo(
    () => series.reduce((s, d) => s + d.cost_cents, 0),
    [series]
  );

  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4"
      data-testid="usage-sparkline"
    >
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-sm font-medium text-white flex items-center gap-2">
          <Calendar className="h-4 w-4 text-white/50" />
          Cost over time
        </h2>
        <div className="font-mono text-xs text-white/40 tabular-nums">
          {formatCents(totalCost)} · {series.length} days
        </div>
      </div>

      <div className="h-48 flex items-end gap-1">
        {series.map((d, idx) => {
          const height = maxCost > 0 ? (d.cost_cents / maxCost) * 100 : 0;
          const date = new Date(d.day + 'T00:00:00Z');
          const dow = date.getUTCDay();
          const isWeekend = dow === 0 || dow === 6;
          // Show a date label every ~5 days, plus first and last
          const showLabel =
            idx === 0 ||
            idx === series.length - 1 ||
            idx % 5 === 0;
          return (
            <div key={d.day} className="flex-1 flex flex-col items-center gap-1 min-w-0">
              <div className="relative w-full flex flex-col items-center">
                <div
                  className={cn(
                    'w-full rounded-t transition-all',
                    d.cost_cents === 0
                      ? 'bg-white/[0.04]'
                      : isWeekend
                      ? 'bg-indigo-500/30 hover:bg-indigo-500/60'
                      : 'bg-indigo-500/50 hover:bg-indigo-500/70'
                  )}
                  style={{ height: `${Math.max(height, 2)}%` }}
                  title={`${d.day}: ${formatCents(d.cost_cents)} · ${d.requests} req`}
                />
              </div>
              <span
                className={cn(
                  'text-[9px] tabular-nums',
                  showLabel ? 'text-white/30' : 'text-transparent'
                )}
              >
                {date.getUTCDate()}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ─── Provider distribution strip ─────────────────────────────────────────────

function ProviderStrip({ models }: { models: ModelUsageSummary[] }) {
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

  if (total === 0) return null;

  return (
    <div
      className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4"
      data-testid="usage-distribution"
    >
      <div className="flex items-center justify-between mb-3">
        <h2 className="text-sm font-medium text-white">
          By provider
          <span className="ml-2 text-xs text-white/40">
            {unit === 'cost' ? 'share of spend' : 'share of requests'}
          </span>
        </h2>
        <span className="font-mono text-xs text-white/40 tabular-nums">
          {entries.length} {entries.length === 1 ? 'provider' : 'providers'}
        </span>
      </div>
      <div className="flex h-1.5 overflow-hidden rounded-full bg-white/[0.04] mb-3">
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
      <div className="space-y-1.5">
        {entries.map(({ provider, value }) => {
          const pct = (value / total) * 100;
          return (
            <div key={provider} className="flex items-center justify-between text-xs">
              <span className="flex items-center gap-2 text-white/65">
                <span
                  className="h-1.5 w-1.5 rounded-sm"
                  style={{ backgroundColor: providerColor(provider) }}
                />
                {provider}
              </span>
              <span className="font-mono text-white/45 tabular-nums">
                {unit === 'cost' ? formatCents(value) : `${fmtCompact(value)} req`}
                <span className="ml-2 text-white/30">
                  {pct.toFixed(pct < 10 ? 1 : 0)}%
                </span>
              </span>
            </div>
          );
        })}
      </div>
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
        <h2 className="text-sm font-medium text-white mb-3">By model</h2>
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
        <h2 className="text-sm font-medium text-white">By model</h2>
        <span className="font-mono text-xs text-white/40 tabular-nums">
          {models.length} {models.length === 1 ? 'model' : 'models'}
        </span>
      </div>
      <div className="overflow-hidden rounded-md border border-white/[0.06]">
        <div className="grid grid-cols-[1.25rem_minmax(0,1fr)_4.5rem_5.5rem_4.5rem_4rem] gap-x-3 border-b border-white/[0.06] bg-white/[0.02] px-3 py-2 text-[10px] uppercase tracking-[0.08em] text-white/40">
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
              className="group relative grid grid-cols-[1.25rem_minmax(0,1fr)_4.5rem_5.5rem_4.5rem_4rem] gap-x-3 border-b border-white/[0.04] px-3 py-2 last:border-b-0 hover:bg-white/[0.015]"
              data-testid="usage-model-row"
            >
              <div
                className="pointer-events-none absolute inset-y-0 left-0 opacity-[0.08] transition-opacity group-hover:opacity-[0.14]"
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
                <span className="truncate text-[12px] text-white/80">
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

// ─── Skeleton ─────────────────────────────────────────────────────────────────

function MetricSkeleton() {
  return (
    <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
      {Array.from({ length: 4 }).map((_, i) => (
        <div
          key={i}
          className="bg-white/[0.02] border border-white/[0.06] rounded-xl p-4 h-[100px] animate-pulse"
        />
      ))}
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

  return (
    <div className="space-y-4" data-testid="usage-overview">
      {/* Header: section title + window picker */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-medium text-white">Usage</h2>
          <p className="text-xs text-white/40">
            Token consumption and cost across every mission
          </p>
        </div>
        <div
          className="flex items-center gap-1 rounded-md border border-white/[0.06] bg-white/[0.02] p-0.5"
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
                'rounded px-2.5 py-1 text-[11px] font-medium transition-colors cursor-pointer',
                window === w.id
                  ? 'bg-indigo-500/20 text-indigo-300'
                  : 'text-white/50 hover:text-white/70'
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
        <MetricSkeleton />
      ) : data.by_model.length === 0 && totals?.requests === 0 ? (
        <div className="bg-white/[0.02] border border-white/[0.06] rounded-xl px-5 py-10 text-center">
          <div className="text-sm text-white/55">No usage recorded in this window.</div>
          <div className="mt-1 text-xs text-white/30">
            Run a mission to populate this summary.
          </div>
        </div>
      ) : (
        <>
          {/* Top metric tiles — matches analytics page card style */}
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
            <MetricTile
              icon={<DollarSign className="h-4 w-4 text-emerald-400" />}
              label="Total spend"
              value={formatCents(totals!.cost_cents)}
              sub={`${fmtCompact(totals!.requests)} calls`}
            />
            <MetricTile
              icon={<ArrowDownToLine className="h-4 w-4 text-indigo-400" />}
              label="Input tokens"
              value={fmtCompact(totals!.input_tokens)}
              sub={`+${fmtCompact(totals!.cache_read_tokens)} from cache`}
            />
            <MetricTile
              icon={<ArrowUpFromLine className="h-4 w-4 text-amber-400" />}
              label="Output tokens"
              value={fmtCompact(totals!.output_tokens)}
              sub={
                totals!.requests > 0
                  ? `${fmtCompact(Math.round(totals!.output_tokens / totals!.requests))} avg per call`
                  : '—'
              }
            />
            <MetricTile
              icon={<Database className="h-4 w-4 text-cyan-400" />}
              label="Cache hit rate"
              value={`${cacheHitRate.toFixed(0)}%`}
              sub={`${fmtCompact(totals!.cache_read_tokens)} tokens reused`}
            />
          </div>

          {/* Two-column section: sparkline + provider strip */}
          <div className="grid gap-3 md:grid-cols-3">
            <div className="md:col-span-2">
              <CostSparkline byDay={data.by_day} windowKey={window} />
            </div>
            <div className="md:col-span-1">
              <ProviderStrip models={data.by_model} />
            </div>
          </div>

          {/* Full-width model table */}
          <ModelTable models={data.by_model} totalRequests={totals!.requests} />

          {/* Footer note — power-user hint */}
          <div className="flex items-center gap-1.5 text-[11px] text-white/30">
            <Activity className="h-3 w-3" />
            <span>
              Aggregated from {fmtCompact(totals!.requests)} assistant calls.
              {data.since && (
                <> Since {new Date(data.since).toLocaleDateString()}.</>
              )}
            </span>
          </div>
        </>
      )}
    </div>
  );
}
