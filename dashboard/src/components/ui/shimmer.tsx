'use client';

import { cn } from '@/lib/utils';

interface ShimmerProps {
  className?: string;
}

// Basic shimmer line
export function Shimmer({ className }: ShimmerProps) {
  return (
    <div className={cn('animate-pulse', className)}>
      <div className="h-4 bg-white/[0.06] rounded w-full" />
    </div>
  );
}

// Shimmer for card content
export function ShimmerCard({ className }: ShimmerProps) {
  return (
    <div className={cn('animate-pulse space-y-3 p-4 rounded-xl bg-white/[0.02] border border-white/[0.04]', className)}>
      <div className="flex items-center gap-3">
        <div className="h-10 w-10 rounded-xl bg-white/[0.06]" />
        <div className="flex-1 space-y-2">
          <div className="h-4 bg-white/[0.06] rounded w-1/2" />
          <div className="h-3 bg-white/[0.04] rounded w-1/3" />
        </div>
      </div>
      <div className="space-y-2">
        <div className="h-3 bg-white/[0.04] rounded w-full" />
        <div className="h-3 bg-white/[0.04] rounded w-3/4" />
      </div>
    </div>
  );
}

// Shimmer matching McpCard in modules/page.tsx — colored icon box, name+badge,
// endpoint line, tag pill row, footer with toggle silhouette.
export function ShimmerMcpCard({ className }: ShimmerProps) {
  return (
    <div
      className={cn(
        'animate-pulse w-full rounded-xl p-4 bg-white/[0.02] border border-white/[0.04]',
        className
      )}
    >
      <div className="flex items-start gap-3 mb-3">
        <div className="h-10 w-10 rounded-xl bg-indigo-500/10" />
        <div className="flex-1 min-w-0 space-y-2">
          <div className="flex items-center gap-2">
            <div className="h-4 w-32 rounded bg-white/[0.08]" />
            <div className="h-4 w-16 rounded-md bg-white/[0.06]" />
          </div>
          <div className="h-3 w-2/3 rounded bg-white/[0.04]" />
        </div>
      </div>
      <div className="flex flex-wrap gap-1 mb-3">
        <div className="h-5 w-12 rounded bg-white/[0.04]" />
        <div className="h-5 w-16 rounded bg-white/[0.04]" />
        <div className="h-5 w-10 rounded bg-white/[0.04]" />
      </div>
      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <div className="h-3 w-14 rounded bg-white/[0.04]" />
        <div className="h-5 w-9 rounded-full bg-white/[0.06]" />
      </div>
    </div>
  );
}

// Shimmer matching automation rows in mission-automations-dialog — no avatar,
// left text column (label + description + meta) + right action pill cluster.
export function ShimmerAutomationRow({ className }: ShimmerProps) {
  return (
    <div
      className={cn(
        'animate-pulse rounded-xl border border-white/[0.08] bg-white/[0.02]',
        className
      )}
    >
      <div className="flex flex-col gap-3 p-4 md:flex-row md:items-center md:justify-between">
        <div className="space-y-2 min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <div className="h-4 w-40 rounded bg-white/[0.08]" />
            <div className="h-4 w-14 rounded bg-white/[0.06]" />
          </div>
          <div className="h-3 w-3/4 rounded bg-white/[0.04]" />
          <div className="h-3 w-1/2 rounded bg-white/[0.04]" />
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <div className="h-7 w-16 rounded-lg bg-white/[0.04]" />
          <div className="h-7 w-20 rounded-lg bg-white/[0.04]" />
          <div className="h-7 w-12 rounded-lg bg-white/[0.04]" />
        </div>
      </div>
    </div>
  );
}

// Shimmer for table rows
export function ShimmerTableRow({ columns = 5, className }: ShimmerProps & { columns?: number }) {
  return (
    <tr className={cn('animate-pulse', className)}>
      {Array.from({ length: columns }).map((_, i) => (
        <td key={i} className="px-4 py-3">
          <div className="h-4 bg-white/[0.06] rounded w-full" />
        </td>
      ))}
    </tr>
  );
}

// Shimmer for stats card — mirrors StatsCard layout: left column with label,
// large value, optional trend line; right icon box (h-10 w-10 rounded-xl).
export function ShimmerStat({ className }: ShimmerProps) {
  return (
    <div className={cn('animate-pulse stat-panel', className)}>
      <div className="flex items-start justify-between">
        <div className="flex-1 space-y-2">
          <div className="h-3 w-20 rounded bg-white/[0.04]" />
          <div className="h-7 w-24 rounded bg-white/[0.08]" />
          <div className="h-3 w-12 rounded bg-white/[0.04]" />
        </div>
        <div className="h-10 w-10 rounded-xl bg-white/[0.04]" />
      </div>
    </div>
  );
}

// Shimmer for sidebar items
export function ShimmerSidebarItem({ className }: ShimmerProps) {
  return (
    <div className={cn('animate-pulse flex items-center gap-2 p-3 rounded-xl', className)}>
      <div className="h-3 w-3 rounded-full bg-white/[0.06]" />
      <div className="h-4 bg-white/[0.06] rounded flex-1" />
    </div>
  );
}

// Shimmer for text block
export function ShimmerText({ lines = 3, className }: ShimmerProps & { lines?: number }) {
  return (
    <div className={cn('animate-pulse space-y-2', className)}>
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="h-4 bg-white/[0.06] rounded"
          style={{ width: `${100 - (i * 15)}%` }}
        />
      ))}
    </div>
  );
}
