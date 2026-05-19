'use client';

/**
 * ProviderUsageHud — a video-game-inspired "resource HUD" for AI provider usage.
 *
 * Design vocabulary borrowed from RPG / shooter HUDs:
 *  - "Energy Core" stat tiles (Cost / Requests / Tokens) with a soft glow.
 *  - Segmented horizontal bars (like retro RPG mana bars) for cache-hit ratio
 *    and provider mix.
 *  - "Loadout" list for top models, where each row is an equipment card.
 *  - Period selector tabs ("season" picker): 24h / 7d / 30d / All.
 *
 * Data source: `GET /api/ai/usage/summary?window=<window>` — see
 * `dashboard/src/lib/api/providers.ts::getUsageSummary`.
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
import { Coins, Zap, ArrowDown, ArrowUp, Trophy, Shield } from 'lucide-react';

const WINDOWS: { id: UsageWindow; label: string }[] = [
  { id: '24h', label: '24h' },
  { id: '7d', label: '7d' },
  { id: '30d', label: '30d' },
  { id: 'all', label: 'All' },
];

/** Provider visual identity — colors map to the existing providerConfig palette. */
const PROVIDER_COLOR: Record<string, { fill: string; text: string; icon: string }> = {
  anthropic: { fill: 'bg-orange-400', text: 'text-orange-300', icon: '🧠' },
  openai: { fill: 'bg-emerald-400', text: 'text-emerald-300', icon: '🤖' },
  google: { fill: 'bg-blue-400', text: 'text-blue-300', icon: '🔮' },
  xai: { fill: 'bg-slate-300', text: 'text-slate-200', icon: '𝕏' },
  zai: { fill: 'bg-cyan-400', text: 'text-cyan-300', icon: 'Z' },
  minimax: { fill: 'bg-teal-400', text: 'text-teal-300', icon: 'M' },
  mistral: { fill: 'bg-indigo-400', text: 'text-indigo-300', icon: '🌪️' },
  groq: { fill: 'bg-pink-400', text: 'text-pink-300', icon: '⚡' },
  'open-router': { fill: 'bg-purple-400', text: 'text-purple-300', icon: '🔀' },
  cohere: { fill: 'bg-rose-400', text: 'text-rose-300', icon: '💬' },
  perplexity: { fill: 'bg-cyan-400', text: 'text-cyan-300', icon: '🔍' },
  'github-copilot': { fill: 'bg-gray-400', text: 'text-gray-300', icon: '🐙' },
  unknown: { fill: 'bg-white/30', text: 'text-white/40', icon: '?' },
};

function providerSkin(id: string | null | undefined) {
  return PROVIDER_COLOR[id || 'unknown'] || PROVIDER_COLOR.unknown;
}

/** Compact number formatting: 1.2k, 3.4M, 1.1B */
function fmtCompact(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

/** Segmented bar (RPG mana-bar style). `pct` is 0..100. */
function SegmentBar({
  pct,
  segments = 20,
  className,
  fillClass = 'bg-emerald-400',
  trackClass = 'bg-white/[0.06]',
}: {
  pct: number;
  segments?: number;
  className?: string;
  fillClass?: string;
  trackClass?: string;
}) {
  const clamped = Math.max(0, Math.min(100, pct));
  const filled = Math.round((clamped / 100) * segments);
  return (
    <div
      className={cn('flex items-center gap-[2px]', className)}
      data-testid="hud-segment-bar"
      role="progressbar"
      aria-valuenow={clamped}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      {Array.from({ length: segments }).map((_, i) => (
        <div
          key={i}
          className={cn(
            'h-2 flex-1 rounded-[1px] transition-colors',
            i < filled ? fillClass : trackClass
          )}
        />
      ))}
    </div>
  );
}

/** A single stat tile — the "HUD pod". */
function StatTile({
  icon,
  label,
  value,
  sub,
  glow = 'shadow-[0_0_24px_-12px_rgba(99,102,241,0.4)]',
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  sub?: React.ReactNode;
  glow?: string;
}) {
  return (
    <div
      className={cn(
        'relative rounded-lg border border-white/[0.06] bg-gradient-to-b from-white/[0.04] to-white/[0.01] p-3',
        glow
      )}
      data-testid="hud-stat-tile"
    >
      <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wider text-white/40">
        <span className="text-white/50">{icon}</span>
        <span>{label}</span>
      </div>
      <div className="mt-1 font-mono text-lg font-semibold text-white tabular-nums">
        {value}
      </div>
      {sub && <div className="mt-0.5 text-[10px] text-white/40">{sub}</div>}
    </div>
  );
}

/** Top models loadout list. */
function ModelLoadout({ models, totalRequests }: { models: ModelUsageSummary[]; totalRequests: number }) {
  // Top 5 by requests
  const top = useMemo(
    () =>
      [...models]
        .filter((m) => m.requests > 0)
        .sort((a, b) => b.requests - a.requests)
        .slice(0, 5),
    [models]
  );

  if (top.length === 0) {
    return (
      <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] p-4 text-center text-xs text-white/40">
        No model usage recorded yet — run a mission to populate the leaderboard.
      </div>
    );
  }

  return (
    <div className="space-y-2" data-testid="hud-loadout">
      {top.map((m, idx) => {
        const skin = providerSkin(m.provider);
        const pct = totalRequests > 0 ? (m.requests / totalRequests) * 100 : 0;
        return (
          <div
            key={m.model + idx}
            className="rounded-lg border border-white/[0.05] bg-white/[0.015] px-3 py-2"
            data-testid="hud-loadout-row"
          >
            <div className="flex items-center gap-2">
              <span className="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded text-[10px] text-white/50">
                {idx === 0 ? <Trophy className="h-3.5 w-3.5 text-amber-300" /> : `#${idx + 1}`}
              </span>
              <span className="text-base leading-none">{skin.icon}</span>
              <div className="min-w-0 flex-1">
                <div className="truncate text-xs font-medium text-white/80">
                  {m.model || 'unknown'}
                </div>
                <div className="text-[10px] text-white/35">
                  {fmtCompact(m.requests)} req · in {fmtCompact(m.input_tokens)} · out{' '}
                  {fmtCompact(m.output_tokens)}
                </div>
              </div>
              <div className="text-right">
                <div className="font-mono text-xs text-white/80 tabular-nums">
                  {formatCents(m.cost_cents)}
                </div>
                <div className="text-[10px] text-white/35">{pct.toFixed(0)}% share</div>
              </div>
            </div>
            <SegmentBar
              pct={pct}
              segments={24}
              className="mt-2"
              fillClass={skin.fill}
            />
          </div>
        );
      })}
    </div>
  );
}

/** Stacked provider distribution bar. */
function ProviderDistribution({ models }: { models: ModelUsageSummary[] }) {
  // Aggregate cost by provider
  const byProvider = useMemo(() => {
    const map = new Map<string, number>();
    for (const m of models) {
      const key = m.provider || 'unknown';
      map.set(key, (map.get(key) || 0) + m.cost_cents);
    }
    const total = Array.from(map.values()).reduce((a, b) => a + b, 0);
    return {
      total,
      entries: Array.from(map.entries())
        .map(([provider, cost]) => ({ provider, cost }))
        .sort((a, b) => b.cost - a.cost),
    };
  }, [models]);

  if (byProvider.total === 0) {
    // Fall back to request share so the bar shows something useful
    const map = new Map<string, number>();
    let total = 0;
    for (const m of models) {
      const key = m.provider || 'unknown';
      map.set(key, (map.get(key) || 0) + m.requests);
      total += m.requests;
    }
    if (total === 0) return null;
    const entries = Array.from(map.entries())
      .map(([provider, cost]) => ({ provider, cost }))
      .sort((a, b) => b.cost - a.cost);
    return <ProviderDistributionBar entries={entries} total={total} unit="req" />;
  }

  return (
    <ProviderDistributionBar
      entries={byProvider.entries}
      total={byProvider.total}
      unit="cost"
    />
  );
}

function ProviderDistributionBar({
  entries,
  total,
  unit,
}: {
  entries: { provider: string; cost: number }[];
  total: number;
  unit: 'cost' | 'req';
}) {
  return (
    <div data-testid="hud-distribution">
      <div className="mb-1.5 flex items-center justify-between text-[10px] uppercase tracking-wider text-white/40">
        <span>Provider Mix ({unit === 'cost' ? 'by spend' : 'by requests'})</span>
        <span className="font-mono text-white/50 tabular-nums">
          {unit === 'cost' ? formatCents(total) : `${fmtCompact(total)} req`}
        </span>
      </div>
      <div className="flex h-2 overflow-hidden rounded-full bg-white/[0.04]">
        {entries.map(({ provider, cost }) => {
          const pct = (cost / total) * 100;
          const skin = providerSkin(provider);
          return (
            <div
              key={provider}
              className={cn('h-full transition-all', skin.fill)}
              style={{ width: `${pct}%` }}
              title={`${provider}: ${pct.toFixed(1)}%`}
            />
          );
        })}
      </div>
      <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1">
        {entries.map(({ provider, cost }) => {
          const pct = (cost / total) * 100;
          const skin = providerSkin(provider);
          return (
            <div key={provider} className="flex items-center gap-1.5 text-[10px]">
              <span className={cn('h-1.5 w-1.5 rounded-full', skin.fill)} />
              <span className="text-white/60">{provider}</span>
              <span className="font-mono text-white/35 tabular-nums">{pct.toFixed(0)}%</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export interface ProviderUsageHudProps {
  /** Initial window selection. */
  window: UsageWindow;
  /** Called when the user picks a new window tab. */
  onWindowChange: (w: UsageWindow) => void;
}

export function ProviderUsageHud({ window, onWindowChange }: ProviderUsageHudProps) {
  const { data, isLoading, error } = useSWR<UsageSummary>(
    ['ai-usage-summary', window],
    () => getUsageSummary(window),
    { revalidateOnFocus: false }
  );

  const totals = data?.totals;
  const cacheReadShare = useMemo(() => {
    if (!totals) return 0;
    const total = totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
    if (total === 0) return 0;
    return (totals.cache_read_tokens / total) * 100;
  }, [totals]);

  return (
    <div
      className={cn(
        'rounded-xl border border-white/[0.06] bg-gradient-to-b from-indigo-500/[0.04] via-white/[0.02] to-white/[0.01] p-5',
        // Soft "screen" glow at the edges to feel like a HUD overlay
        'shadow-[inset_0_1px_0_0_rgba(255,255,255,0.04)]'
      )}
      data-testid="provider-usage-hud"
    >
      {/* Header: title + window picker */}
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10 ring-1 ring-inset ring-indigo-400/20">
            {/* hex-style energy core */}
            <Zap className="h-5 w-5 text-indigo-300" />
          </div>
          <div>
            <h2 className="text-sm font-medium text-white">Resource HUD</h2>
            <p className="text-xs text-white/40">
              Token economy across every mission you&apos;ve run
            </p>
          </div>
        </div>
        <div
          className="flex items-center gap-0.5 rounded-lg border border-white/[0.06] bg-white/[0.02] p-0.5"
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
              data-testid={`hud-window-${w.id}`}
              className={cn(
                'rounded-md px-2 py-1 text-[11px] font-medium transition-colors cursor-pointer',
                window === w.id
                  ? 'bg-indigo-500/20 text-indigo-200 ring-1 ring-inset ring-indigo-400/30'
                  : 'text-white/40 hover:text-white/70'
              )}
            >
              {w.label}
            </button>
          ))}
        </div>
      </div>

      {/* Error / empty / loading states */}
      {error ? (
        <div className="rounded-lg border border-red-500/20 bg-red-500/[0.04] p-3 text-xs text-red-300">
          Failed to load usage summary.
        </div>
      ) : isLoading || !data ? (
        <HudSkeleton />
      ) : data.by_model.length === 0 ? (
        <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] p-6 text-center">
          <div className="text-xs text-white/50">
            No AI usage recorded in this window.
          </div>
          <div className="mt-1 text-[10px] text-white/30">
            Start a mission to begin filling your stat sheet.
          </div>
        </div>
      ) : (
        <>
          {/* Stat tiles */}
          <div
            className="grid grid-cols-2 gap-2 sm:grid-cols-4"
            data-testid="hud-stat-tiles"
          >
            <StatTile
              icon={<Coins className="h-3.5 w-3.5 text-amber-300" />}
              label="Gold spent"
              value={formatCents(totals!.cost_cents)}
              sub={
                <span className="font-mono tabular-nums">
                  {fmtCompact(totals!.requests)} battles
                </span>
              }
              glow="shadow-[0_0_24px_-12px_rgba(245,158,11,0.5)]"
            />
            <StatTile
              icon={<ArrowDown className="h-3.5 w-3.5 text-sky-300" />}
              label="Input tokens"
              value={fmtCompact(totals!.input_tokens)}
              sub={
                <span className="text-white/40">
                  + {fmtCompact(totals!.cache_read_tokens)} cached
                </span>
              }
              glow="shadow-[0_0_24px_-12px_rgba(56,189,248,0.45)]"
            />
            <StatTile
              icon={<ArrowUp className="h-3.5 w-3.5 text-emerald-300" />}
              label="Output tokens"
              value={fmtCompact(totals!.output_tokens)}
              sub={
                <span className="text-white/40 font-mono tabular-nums">
                  {totals!.requests > 0
                    ? `${Math.round(totals!.output_tokens / totals!.requests)} avg/req`
                    : '—'}
                </span>
              }
              glow="shadow-[0_0_24px_-12px_rgba(52,211,153,0.45)]"
            />
            <StatTile
              icon={<Shield className="h-3.5 w-3.5 text-cyan-300" />}
              label="Cache shield"
              value={`${cacheReadShare.toFixed(0)}%`}
              sub={
                <span className="text-white/40">
                  saved {fmtCompact(totals!.cache_read_tokens)} tok
                </span>
              }
              glow="shadow-[0_0_24px_-12px_rgba(34,211,238,0.5)]"
            />
          </div>

          {/* Provider distribution */}
          <div className="mt-4">
            <ProviderDistribution models={data.by_model} />
          </div>

          {/* Top models loadout */}
          <div className="mt-4">
            <div className="mb-2 flex items-center justify-between">
              <span className="text-[10px] uppercase tracking-wider text-white/40">
                Top loadout
              </span>
              <span className="text-[10px] text-white/30">{data.by_model.length} models</span>
            </div>
            <ModelLoadout models={data.by_model} totalRequests={totals!.requests} />
          </div>
        </>
      )}
    </div>
  );
}

function HudSkeleton() {
  return (
    <div className="space-y-3" data-testid="hud-skeleton">
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div
            key={i}
            className="h-[68px] rounded-lg border border-white/[0.06] bg-white/[0.02]"
          />
        ))}
      </div>
      <div className="h-2 rounded-full bg-white/[0.04]" />
      <div className="space-y-2">
        {Array.from({ length: 3 }).map((_, i) => (
          <div key={i} className="h-[52px] rounded-lg border border-white/[0.05] bg-white/[0.015]" />
        ))}
      </div>
    </div>
  );
}
