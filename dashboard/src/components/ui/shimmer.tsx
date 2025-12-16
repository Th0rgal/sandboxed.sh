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

// Shimmer for stats card
export function ShimmerStat({ className }: ShimmerProps) {
  return (
    <div className={cn('animate-pulse p-4 rounded-xl bg-white/[0.02] border border-white/[0.04]', className)}>
      <div className="flex items-center justify-between mb-3">
        <div className="h-3 bg-white/[0.04] rounded w-20" />
        <div className="h-8 w-8 rounded-lg bg-white/[0.06]" />
      </div>
      <div className="h-7 bg-white/[0.06] rounded w-16" />
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
