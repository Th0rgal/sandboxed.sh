'use client';

/**
 * UsageOverview — compact, professional summary of AI usage across missions.
 *
 * The visual language is deliberately quiet: small letterspaced labels,
 * tabular monospace numerals for every quantity, a single thin accent line
 * for distribution, and a dense per-model table. The information density is
 * inspired by operational dashboards (Palantir Foundry, Linear, Datadog) —
 * the goal is "one card you can glance at, then drill into" without any
 * decorative chrome.
 *
 * Data source: `GET /api/ai/usage/summary?window=<window>`
 * (see `dashboard/src/lib/api/providers.ts::getUsageSummary`).
 */

import { useMemo } from 'react';
import useSWR from 'swr';
import {
  getUsageSummary,
  type ModelUsageSummary,
  type UsageSummary,
  type UsageWindow,
} from '@/lib/api';
import { cn, formatCents } from '@/lib/utils';

const WINDOWS: { id: UsageWindow; label: string }[] = [
  { id: '24h', label: '24h' },
  { id: '7d', label: '7d' },
  { id: '30d', label: '30d' },
  { id: 'all', label: 'All time' },
];

/** Provider swatch color — a single hex per provider, used in dots and the bar. */
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

/** Compact number formatting: 1.2K, 3.4M, 1.1B */
function fmtCompact(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}K`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

/** A small letterspaced uppercase label. */
function Label({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        'text-[10px] uppercase tracking-[0.08em] text-white/35',
        className
      )}
    >
      {children}
    </span>
  );
}

/** Metric — a single label/value pair in the top strip. */
function Metric({
  label,
  value,
  sub,
}: {
  label: string;
  value: string;
  sub?: React.ReactNode;
}) {
  return (
    <div
      className="px-4 py-3 first:pl-0 border-l border-white/[0.05] first:border-l-0"
      data-testid="usage-metric"
    >
      <Label>{label}</Label>
      <div className="mt-1 font-mono text-base font-medium text-white tabular-nums">
        {value}
      </div>
      {sub && (
        <div className="mt-0.5 font-mono text-[10px] text-white/35 tabular-nums">
          {sub}
        </div>
      )}
    </div>
  );
}

/** Stacked thin bar showing provider distribution. */
function DistributionBar({
  models,
}: {
  models: ModelUsageSummary[];
}) {
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
    <div data-testid="usage-distribution">
      <div className="flex items-baseline justify-between">
        <Label>By provider {unit === 'cost' ? '· spend share' : '· request share'}</Label>
        <span className="font-mono text-[10px] text-white/35 tabular-nums">
          {entries.length} {entries.length === 1 ? 'provider' : 'providers'}
        </span>
      </div>
      <div className="mt-2 flex h-1 overflow-hidden rounded-full bg-white/[0.04]">
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
      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1">
        {entries.map(({ provider, value }) => {
          const pct = (value / total) * 100;
          return (
            <div key={provider} className="flex items-center gap-1.5">
              <span
                className="h-1.5 w-1.5 rounded-sm"
                style={{ backgroundColor: providerColor(provider) }}
              />
              <span className="text-[11px] text-white/55">{provider}</span>
              <span className="font-mono text-[11px] text-white/35 tabular-nums">
                {pct.toFixed(pct < 10 ? 1 : 0)}%
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** Compact model table — rank, swatch + model, requests, tokens, cost, share bar. */
function ModelTable({
  models,
  totalRequests,
}: {
  models: ModelUsageSummary[];
  totalRequests: number;
}) {
  const top = useMemo(
    () =>
      [...models]
        .filter((m) => m.requests > 0)
        .sort((a, b) => b.requests - a.requests)
        .slice(0, 8),
    [models]
  );
  const maxReq = top[0]?.requests || 1;

  if (top.length === 0) {
    return (
      <div className="rounded-md border border-white/[0.05] bg-white/[0.01] px-3 py-4 text-center text-[11px] text-white/35">
        No model usage recorded.
      </div>
    );
  }

  return (
    <div className="overflow-hidden rounded-md border border-white/[0.05]" data-testid="usage-model-table">
      <div className="grid grid-cols-[1.25rem_minmax(0,1fr)_4.5rem_5.5rem_4.5rem_4rem] gap-x-3 border-b border-white/[0.05] bg-white/[0.015] px-3 py-1.5">
        <Label>#</Label>
        <Label>Model</Label>
        <Label className="text-right">Calls</Label>
        <Label className="text-right">Tokens</Label>
        <Label className="text-right">Spend</Label>
        <Label className="text-right">Share</Label>
      </div>
      {top.map((m, idx) => {
        const totalTokens =
          m.input_tokens + m.output_tokens + m.cache_read_tokens + m.cache_creation_tokens;
        const pctOfRequests = totalRequests > 0 ? (m.requests / totalRequests) * 100 : 0;
        const barWidth = Math.max(2, (m.requests / maxReq) * 100);
        return (
          <div
            key={m.model + idx}
            className="group relative grid grid-cols-[1.25rem_minmax(0,1fr)_4.5rem_5.5rem_4.5rem_4rem] gap-x-3 border-b border-white/[0.04] px-3 py-1.5 last:border-b-0 hover:bg-white/[0.015]"
            data-testid="usage-model-row"
          >
            {/* Background micro-bar: request share of leader, in provider color */}
            <div
              className="pointer-events-none absolute inset-y-0 left-0 opacity-[0.07] transition-opacity group-hover:opacity-[0.12]"
              style={{ width: `${barWidth}%`, backgroundColor: providerColor(m.provider) }}
            />
            <span className="relative font-mono text-[11px] text-white/30 tabular-nums">
              {idx + 1}
            </span>
            <div className="relative flex items-center gap-2 min-w-0">
              <span
                className="h-1.5 w-1.5 flex-shrink-0 rounded-sm"
                style={{ backgroundColor: providerColor(m.provider) }}
              />
              <span className="truncate text-[12px] text-white/80">{m.model || 'unknown'}</span>
            </div>
            <span className="relative text-right font-mono text-[11px] text-white/70 tabular-nums">
              {fmtCompact(m.requests)}
            </span>
            <span className="relative text-right font-mono text-[11px] text-white/55 tabular-nums">
              {fmtCompact(totalTokens)}
            </span>
            <span className="relative text-right font-mono text-[11px] text-white/80 tabular-nums">
              {formatCents(m.cost_cents)}
            </span>
            <span className="relative text-right font-mono text-[11px] text-white/40 tabular-nums">
              {pctOfRequests.toFixed(pctOfRequests < 10 ? 1 : 0)}%
            </span>
          </div>
        );
      })}
    </div>
  );
}

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
    <div
      className="rounded-xl border border-white/[0.06] bg-white/[0.02]"
      data-testid="usage-overview"
    >
      {/* Header */}
      <div className="flex items-center justify-between border-b border-white/[0.05] px-5 py-3">
        <div>
          <h2 className="text-sm font-medium text-white">Usage</h2>
          <p className="text-[11px] text-white/40">
            Token consumption and cost across every mission
          </p>
        </div>
        <div
          className="flex items-center gap-0.5 rounded-md border border-white/[0.06] p-0.5"
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
                'rounded px-2 py-1 text-[11px] font-medium transition-colors cursor-pointer',
                window === w.id
                  ? 'bg-white/[0.08] text-white'
                  : 'text-white/40 hover:text-white/70'
              )}
            >
              {w.label}
            </button>
          ))}
        </div>
      </div>

      {/* Body */}
      {error ? (
        <div className="px-5 py-4 text-[12px] text-red-300/80">
          Failed to load usage data.
        </div>
      ) : isLoading || !data ? (
        <Skeleton />
      ) : data.by_model.length === 0 ? (
        <div className="px-5 py-8 text-center">
          <div className="text-[12px] text-white/45">
            No usage recorded in this window.
          </div>
          <div className="mt-1 text-[11px] text-white/30">
            Run a mission to populate this summary.
          </div>
        </div>
      ) : (
        <>
          {/* Metric strip */}
          <div className="flex flex-wrap divide-y divide-white/[0.05] px-5 py-2 sm:divide-y-0">
            <Metric
              label="Spend"
              value={formatCents(totals!.cost_cents)}
              sub={`${fmtCompact(totals!.requests)} calls`}
            />
            <Metric
              label="Input"
              value={fmtCompact(totals!.input_tokens)}
              sub={`+${fmtCompact(totals!.cache_read_tokens)} cached`}
            />
            <Metric
              label="Output"
              value={fmtCompact(totals!.output_tokens)}
              sub={
                totals!.requests > 0
                  ? `${fmtCompact(Math.round(totals!.output_tokens / totals!.requests))} per call`
                  : '—'
              }
            />
            <Metric
              label="Cache hit"
              value={`${cacheHitRate.toFixed(0)}%`}
              sub={`${fmtCompact(totals!.cache_read_tokens)} reused`}
            />
          </div>

          {/* Distribution */}
          <div className="border-t border-white/[0.05] px-5 py-3">
            <DistributionBar models={data.by_model} />
          </div>

          {/* Model table */}
          <div className="border-t border-white/[0.05] px-5 py-3">
            <div className="mb-2 flex items-baseline justify-between">
              <Label>By model</Label>
              <span className="font-mono text-[10px] text-white/35 tabular-nums">
                {data.by_model.length} {data.by_model.length === 1 ? 'model' : 'models'}
              </span>
            </div>
            <ModelTable models={data.by_model} totalRequests={totals!.requests} />
          </div>
        </>
      )}
    </div>
  );
}

function Skeleton() {
  return (
    <div className="space-y-3 px-5 py-4" data-testid="usage-skeleton">
      <div className="flex gap-6">
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="h-10 flex-1 rounded bg-white/[0.03]" />
        ))}
      </div>
      <div className="h-1 rounded-full bg-white/[0.04]" />
      <div className="h-32 rounded-md border border-white/[0.05] bg-white/[0.015]" />
    </div>
  );
}
