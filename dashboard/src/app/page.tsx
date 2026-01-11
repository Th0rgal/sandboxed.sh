'use client';

import { useCallback, useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { toast } from '@/components/toast';
import { StatsCard } from '@/components/stats-card';
import { ConnectionStatus } from '@/components/connection-status';
import { RecentTasks } from '@/components/recent-tasks';
import { ShimmerStat } from '@/components/ui/shimmer';
import { createMission, getStats, isNetworkError, listWorkspaces, type StatsResponse, type Workspace } from '@/lib/api';
import { Activity, CheckCircle, DollarSign, Zap } from 'lucide-react';
import { formatCents } from '@/lib/utils';
import { SystemMonitor } from '@/components/system-monitor';
import { NewMissionDialog } from '@/components/new-mission-dialog';

export default function OverviewPage() {
  const router = useRouter();
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [isActive, setIsActive] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [creatingMission, setCreatingMission] = useState(false);

  useEffect(() => {
    let mounted = true;
    let hasShownError = false;

    const fetchStats = async () => {
      try {
        const data = await getStats();
        if (!mounted) return;
        setStats(data);
        setIsActive(data.active_tasks > 0);
        setError(null);
        setLoading(false);
        hasShownError = false;
      } catch (err) {
        if (!mounted) return;
        const message = err instanceof Error ? err.message : 'Failed to fetch stats';
        setError(message);
        setLoading(false);
        if (!hasShownError) {
          toast.error('Failed to connect to agent server');
          hasShownError = true;
        }
      }
    };

    fetchStats();
    const interval = setInterval(fetchStats, 3000);
    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    listWorkspaces()
      .then((data) => {
        setWorkspaces(data);
      })
      .catch((err) => {
        if (isNetworkError(err)) return;
        console.error('Failed to fetch workspaces:', err);
      });
  }, []);

  const handleNewMission = useCallback(
    async (options?: { workspaceId?: string; agent?: string }) => {
      try {
        setCreatingMission(true);
        const mission = await createMission({
          workspaceId: options?.workspaceId,
          agent: options?.agent,
        });
        toast.success('New mission created');
        router.push(`/control?mission=${mission.id}`);
      } catch (err) {
        console.error('Failed to create mission:', err);
        toast.error('Failed to create new mission');
      } finally {
        setCreatingMission(false);
      }
    },
    [router]
  );

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
          
          {/* Quick Actions */}
          <NewMissionDialog
            workspaces={workspaces}
            disabled={creatingMission}
            onCreate={handleNewMission}
          />
        </div>

        {/* System Metrics Area */}
        <div className="flex-1 flex items-center justify-center rounded-2xl bg-white/[0.01] border border-white/[0.04] mb-6 min-h-[300px] p-6">
          <SystemMonitor className="w-full max-w-4xl" />
        </div>

        {/* Stats grid - at bottom */}
        <div className="grid grid-cols-4 gap-4">
          {loading ? (
            <>
              <ShimmerStat />
              <ShimmerStat />
              <ShimmerStat />
              <ShimmerStat />
            </>
          ) : (
            <>
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
            </>
          )}
        </div>
      </div>

      {/* Right sidebar - no glass panel wrapper, just border */}
      <div className="w-80 h-screen border-l border-white/[0.06] p-4 flex flex-col overflow-hidden">
        <div className="flex-1 min-h-0 overflow-hidden">
          <RecentTasks />
        </div>
        <div className="mt-4 flex-shrink-0">
          <ConnectionStatus />
        </div>
      </div>
    </div>
  );
}
