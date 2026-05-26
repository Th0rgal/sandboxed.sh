'use client';

import { useEffect, useState } from 'react';
import useSWR from 'swr';
import { CheckCircle, XCircle, Loader, DollarSign, Activity, Hand } from 'lucide-react';
import { getStats, type Mission, type StatsResponse } from '@/lib/api';
import { formatCents, cn } from '@/lib/utils';

interface LastDaySummaryProps {
  missions: Mission[];
  runningMissionIds: Set<string>;
}

const ONE_DAY_MS = 24 * 60 * 60 * 1000;

/**
 * Compact "Last 24 hours" panel for the Overview right sidebar.
 * Pulls global 24h stats from the API and derives a few extras
 * (active count, needs-attention count) from the missions list
 * already loaded by the page.
 */
export function LastDaySummary({ missions, runningMissionIds }: LastDaySummaryProps) {
  const { sinceIso, cutoff } = useDailyWindow();

  const { data: dayStats, isLoading } = useSWR<StatsResponse>(
    ['stats', sinceIso],
    () => getStats(sinceIso),
    {
      refreshInterval: 30_000,
      revalidateOnFocus: false,
    },
  );

  const activeCount = runningMissionIds.size;
  const needsAttention = missions.filter((m) => m.status === 'awaiting_user').length;

  const updatedLast24h = missions.filter((m) => {
    const ts = new Date(m.updated_at).getTime();
    return Number.isFinite(ts) && ts >= cutoff;
  });
  const finishedLast24h = updatedLast24h.filter((m) =>
    m.status === 'completed' || m.status === 'acknowledged',
  ).length;
  const failedLast24h = updatedLast24h.filter((m) =>
    m.status === 'failed' || m.status === 'not_feasible',
  ).length;

  const completed = dayStats?.completed_tasks ?? finishedLast24h;
  const failed = dayStats?.failed_tasks ?? failedLast24h;
  const spent = dayStats?.total_cost_cents ?? 0;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-baseline justify-between">
        <h2 className="text-sm font-medium text-white/80">Last 24 hours</h2>
        <span className="text-[10px] uppercase tracking-wider text-white/30">
          rolling
        </span>
      </div>

      <div className="grid grid-cols-2 gap-2">
        <SummaryTile
          icon={CheckCircle}
          label="Completed"
          value={completed}
          tone="emerald"
          loading={isLoading}
        />
        <SummaryTile
          icon={XCircle}
          label="Failed"
          value={failed}
          tone={failed > 0 ? 'red' : 'muted'}
          loading={isLoading}
        />
        <SummaryTile
          icon={Loader}
          label="Active"
          value={activeCount}
          tone={activeCount > 0 ? 'indigo' : 'muted'}
          live={activeCount > 0}
        />
        <SummaryTile
          icon={DollarSign}
          label="Spent"
          value={formatCents(spent)}
          tone="muted"
          loading={isLoading}
        />
      </div>

      <div className="rounded-md border border-white/[0.06] bg-white/[0.02] px-3 py-2.5">
        <div className="flex items-center justify-between text-xs">
          <div className="flex items-center gap-2 text-white/60">
            <Hand
              className={cn(
                'h-3.5 w-3.5',
                needsAttention > 0 ? 'text-amber-400' : 'text-white/30',
              )}
            />
            <span>Needs you</span>
          </div>
          <span
            className={cn(
              'tabular-nums text-sm font-medium',
              needsAttention > 0 ? 'text-amber-300' : 'text-white/40',
            )}
          >
            {needsAttention}
          </span>
        </div>
        <div className="mt-2 flex items-center justify-between text-xs">
          <div className="flex items-center gap-2 text-white/60">
            <Activity className="h-3.5 w-3.5 text-white/30" />
            <span>Total updated</span>
          </div>
          <span className="tabular-nums text-sm font-medium text-white/70">
            {updatedLast24h.length}
          </span>
        </div>
      </div>
    </div>
  );
}

function SummaryTile({
  icon: Icon,
  label,
  value,
  tone,
  loading,
  live,
}: {
  icon: typeof CheckCircle;
  label: string;
  value: number | string;
  tone: 'emerald' | 'red' | 'indigo' | 'muted';
  loading?: boolean;
  live?: boolean;
}) {
  const toneIcon = {
    emerald: 'text-emerald-400',
    red: 'text-red-400',
    indigo: 'text-indigo-400',
    muted: 'text-white/40',
  }[tone];

  const toneValue = {
    emerald: 'text-white',
    red: 'text-red-300',
    indigo: 'text-indigo-300',
    muted: 'text-white/80',
  }[tone];

  return (
    <div className="rounded-md border border-white/[0.06] bg-white/[0.02] px-3 py-2.5">
      <div className="flex items-center gap-1.5 text-[10px] uppercase tracking-wider text-white/40">
        <Icon className={cn('h-3 w-3', toneIcon, live && 'animate-spin')} />
        <span>{label}</span>
      </div>
      <div
        className={cn(
          'mt-1.5 text-lg font-semibold tabular-nums leading-none',
          toneValue,
          loading && 'opacity-50',
        )}
      >
        {value}
      </div>
    </div>
  );
}

/**
 * Returns the 24h window cutoff, snapped to the minute and refreshed every
 * 60s. Lazy useState initializer keeps the impure Date.now() call off the
 * render path; the interval bumps the window forward so stats and the
 * mission filter stay roughly aligned without re-rendering on each tick.
 */
function useDailyWindow(): { sinceIso: string; cutoff: number } {
  const [window, setWindow] = useState(computeWindow);
  useEffect(() => {
    const id = setInterval(() => setWindow(computeWindow()), 60_000);
    return () => clearInterval(id);
  }, []);
  return window;
}

function computeWindow(): { sinceIso: string; cutoff: number } {
  const minute = Math.floor(Date.now() / 60_000) * 60_000;
  const cutoff = minute - ONE_DAY_MS;
  return { sinceIso: new Date(cutoff).toISOString(), cutoff };
}
