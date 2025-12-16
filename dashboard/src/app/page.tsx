'use client';

import { useEffect, useState } from 'react';
import { StatsCard } from '@/components/stats-card';
import { ConnectionStatus } from '@/components/connection-status';
import { RecentTasks } from '@/components/recent-tasks';
import { getStats, StatsResponse } from '@/lib/api';
import { Activity, CheckCircle, DollarSign, Zap } from 'lucide-react';
import { formatCents } from '@/lib/utils';

export default function OverviewPage() {
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [isActive, setIsActive] = useState(false);

  useEffect(() => {
    const fetchStats = async () => {
      try {
        const data = await getStats();
        setStats(data);
        setIsActive(data.active_tasks > 0);
      } catch (error) {
        console.error('Failed to fetch stats:', error);
      }
    };

    fetchStats();
    const interval = setInterval(fetchStats, 3000);
    return () => clearInterval(interval);
  }, []);

  return (
    <div className="flex min-h-screen">
      {/* Main content */}
      <div className="flex-1 flex flex-col p-6">
        {/* Header */}
        <div className="mb-6 flex items-start justify-between">
          <div>
            <div className="flex items-center gap-3">
              <h1 className="text-xl font-semibold text-white">
                Global Monitor
              </h1>
              {isActive && (
                <span className="flex items-center gap-1.5 rounded-md bg-emerald-500/10 border border-emerald-500/20 px-2 py-1 text-[10px] font-medium text-emerald-400">
                  <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
                  LIVE
                </span>
              )}
            </div>
            <p className="mt-1 text-sm text-white/50">
              Real-time agent activity
            </p>
          </div>
        </div>

        {/* Visualization Area (placeholder for radar/globe) - takes remaining space */}
        <div className="flex-1 flex items-center justify-center rounded-2xl bg-white/[0.01] border border-white/[0.04] mb-6 min-h-[300px]">
          {/* Circular radar visualization */}
          <div className="relative">
            {/* Outer rings */}
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-64 w-64 rounded-full border border-white/[0.06]" />
            </div>
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-48 w-48 rounded-full border border-white/[0.05]" />
            </div>
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-32 w-32 rounded-full border border-white/[0.04]" />
            </div>
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-16 w-16 rounded-full border border-white/[0.03]" />
            </div>
            
            {/* Cross lines */}
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-64 w-[1px] bg-white/[0.04]" />
            </div>
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-[1px] w-64 bg-white/[0.04]" />
            </div>
            
            {/* Center dot */}
            <div className="relative h-64 w-64 flex items-center justify-center">
              <div className={`h-3 w-3 rounded-full ${isActive ? 'bg-emerald-400 animate-pulse' : 'bg-white/20'}`} />
              
              {/* Activity dots (mock) */}
              {isActive && (
                <>
                  <div className="absolute top-1/4 left-1/3 h-2 w-2 rounded-full bg-indigo-400/60 animate-pulse-subtle" />
                  <div className="absolute bottom-1/3 right-1/4 h-2 w-2 rounded-full bg-emerald-400/60 animate-pulse-subtle" style={{ animationDelay: '0.5s' }} />
                </>
              )}
            </div>
          </div>
        </div>

        {/* Stats grid - at bottom */}
        <div className="grid grid-cols-4 gap-4">
          <StatsCard
            title="Total Tasks"
            value={stats?.total_tasks ?? 0}
            icon={Activity}
          />
          <StatsCard
            title="Active"
            value={stats?.active_tasks ?? 0}
            subtitle="running"
            icon={Zap}
            color={stats?.active_tasks ? 'accent' : 'default'}
          />
          <StatsCard
            title="Success Rate"
            value={`${((stats?.success_rate ?? 1) * 100).toFixed(0)}%`}
            icon={CheckCircle}
            color="success"
          />
          <StatsCard
            title="Total Cost"
            value={formatCents(stats?.total_cost_cents ?? 0)}
            icon={DollarSign}
          />
        </div>
      </div>

      {/* Right sidebar - no glass panel wrapper, just border */}
      <div className="w-80 border-l border-white/[0.06] p-4 flex flex-col">
        <div className="flex-1">
          <RecentTasks />
        </div>
        <div className="mt-4">
          <ConnectionStatus />
        </div>
      </div>
    </div>
  );
}
