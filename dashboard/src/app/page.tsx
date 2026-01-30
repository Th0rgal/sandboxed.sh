'use client';

import { useCallback, useMemo, useRef, useState } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import { StatsCard } from '@/components/stats-card';
import { RecentTasks } from '@/components/recent-tasks';
import { ShimmerStat } from '@/components/ui/shimmer';
import { RelativeTime } from '@/components/ui/relative-time';
import {
  createMission,
  getStats,
  listWorkspaces,
  listMissions,
  getRunningMissions,
  cancelMission,
  deleteMission,
  resumeMission,
  type Mission,
} from '@/lib/api';
import {
  Activity,
  CheckCircle,
  DollarSign,
  Zap,
  Loader,
  Clock,
  RotateCcw,
  Trash2,
  Hand,
  XCircle,
} from 'lucide-react';
import { cn, formatCents } from '@/lib/utils';
import { NewMissionDialog } from '@/components/new-mission-dialog';
import {
  categorizeMissions,
  getMissionTextColor,
  getMissionTitle,
  type MissionCategory,
} from '@/lib/mission-status';
import { getStatusIcon } from '@/components/ui/status-icons';

interface Column {
  id: MissionCategory;
  label: string;
  icon: typeof Clock;
}

const columns: Column[] = [
  { id: 'running', label: 'Running', icon: Loader },
  { id: 'needs-you', label: 'Needs You', icon: Hand },
  { id: 'finished', label: 'Finished', icon: CheckCircle },
];

function CompactMissionCard({
  mission,
  isActuallyRunning,
  onCancel,
  onResume,
  onDelete,
}: {
  mission: Mission;
  isActuallyRunning: boolean;
  onCancel: (id: string) => void;
  onResume: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  const Icon = isActuallyRunning ? Loader : getStatusIcon(mission.status);
  const color = getMissionTextColor(mission.status, isActuallyRunning);
  const title = getMissionTitle(mission);
  const isResumable = !isActuallyRunning && mission.resumable &&
    (mission.status === 'interrupted' || mission.status === 'blocked' || mission.status === 'failed');

  return (
    <div className="group rounded-md bg-white/[0.02] border border-white/[0.06] hover:border-white/[0.12] px-2.5 py-2 transition-colors">
      <div className="flex items-center gap-2 mb-1.5">
        <Icon
          className={cn('h-3.5 w-3.5 shrink-0', color, isActuallyRunning && 'animate-spin')}
        />
        <Link href={`/control?mission=${mission.id}`} className="flex-1 min-w-0">
          <p className="text-xs text-white/80 leading-snug truncate hover:text-white transition-colors">
            {title}
          </p>
        </Link>
      </div>
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          {mission.workspace_name && (
            <span className="inline-flex items-center rounded bg-white/[0.04] px-1 py-0.5 text-[9px] text-white/40 truncate max-w-[60px]">
              {mission.workspace_name}
            </span>
          )}
          <RelativeTime date={mission.updated_at} className="text-[9px] text-white/30" />
        </div>
        <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
          {isResumable && (
            <button
              onClick={() => onResume(mission.id)}
              className="p-0.5 rounded hover:bg-white/[0.08] text-white/40 hover:text-emerald-400 transition-colors"
              title="Resume"
            >
              <RotateCcw className="h-3 w-3" />
            </button>
          )}
          {isActuallyRunning && (
            <button
              onClick={() => onCancel(mission.id)}
              className="p-0.5 rounded hover:bg-white/[0.08] text-white/40 hover:text-red-400 transition-colors"
              title="Cancel"
            >
              <XCircle className="h-3 w-3" />
            </button>
          )}
          {!isActuallyRunning && (
            <button
              onClick={() => onDelete(mission.id)}
              className="p-0.5 rounded hover:bg-white/[0.08] text-white/40 hover:text-red-400 transition-colors"
              title="Delete"
            >
              <Trash2 className="h-3 w-3" />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export default function OverviewPage() {
  const router = useRouter();
  const [creatingMission, setCreatingMission] = useState(false);
  const hasShownErrorRef = useRef(false);

  // SWR: poll stats every 3 seconds
  const { data: stats, isLoading: statsLoading, error: statsError } = useSWR(
    'stats',
    getStats,
    {
      refreshInterval: 3000,
      revalidateOnFocus: false,
      onSuccess: () => {
        hasShownErrorRef.current = false;
      },
      onError: () => {
        if (!hasShownErrorRef.current) {
          toast.error('Failed to connect to agent server');
          hasShownErrorRef.current = true;
        }
      },
    }
  );

  // SWR: fetch workspaces (shared key with workspaces page)
  const { data: workspaces = [] } = useSWR('workspaces', listWorkspaces, {
    revalidateOnFocus: false,
  });

  // SWR: fetch missions for kanban
  const { data: missions = [], mutate: mutateMissions } = useSWR(
    'missions',
    listMissions,
    {
      refreshInterval: 5000,
      revalidateOnFocus: false,
    }
  );

  const { data: runningMissions = [] } = useSWR(
    'running-missions',
    getRunningMissions,
    {
      refreshInterval: 3000,
      revalidateOnFocus: false,
    }
  );

  // Build a set of actually running mission IDs from the runtime state
  const runningMissionIds = useMemo(() => {
    return new Set(runningMissions.map((rm) => rm.mission_id));
  }, [runningMissions]);

  // Categorize missions using shared utility
  const categorized = useMemo(
    () => categorizeMissions(missions, runningMissionIds),
    [missions, runningMissionIds]
  );

  // Build column data for display
  const columnData = useMemo(() => {
    return columns.map((col) => {
      const colMissions = categorized[col.id]
        .sort(
          (a, b) =>
            new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime()
        )
        .slice(0, col.id === 'finished' ? 8 : 10);
      return { ...col, missions: colMissions };
    });
  }, [categorized]);

  const isActive = (stats?.active_tasks ?? 0) > 0;

  const handleCancel = useCallback(
    async (id: string) => {
      try {
        await cancelMission(id);
        toast.success('Mission cancelled');
        mutateMissions();
      } catch {
        toast.error('Failed to cancel mission');
      }
    },
    [mutateMissions]
  );

  const handleResume = useCallback(
    async (id: string) => {
      try {
        await resumeMission(id);
        toast.success('Mission resumed');
        router.push(`/control?mission=${id}`);
      } catch {
        toast.error('Failed to resume mission');
      }
    },
    [router]
  );

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await deleteMission(id);
        mutateMissions(
          (current) => (current ? current.filter((m) => m.id !== id) : current),
          false
        );
        toast.success('Mission deleted');
      } catch {
        toast.error('Failed to delete mission');
      }
    },
    [mutateMissions]
  );

  const handleNewMission = useCallback(
    async (options?: { workspaceId?: string; agent?: string; modelOverride?: string; configProfile?: string; backend?: string; openInNewTab?: boolean }) => {
      try {
        setCreatingMission(true);
        const mission = await createMission({
          workspaceId: options?.workspaceId,
          agent: options?.agent,
          modelOverride: options?.modelOverride,
          configProfile: options?.configProfile,
          backend: options?.backend,
        });
        toast.success('New mission created');
        return { id: mission.id };
      } catch (err) {
        console.error('Failed to create mission:', err);
        toast.error('Failed to create new mission');
        throw err; // Re-throw so dialog knows creation failed
      } finally {
        setCreatingMission(false);
      }
    },
    []
  );

  return (
    <div className="flex h-screen overflow-hidden">
      {/* Main content */}
      <div className="flex-1 flex flex-col p-6 min-h-0">
        {/* Header */}
        <div className="flex-shrink-0 mb-4 flex items-start justify-between">
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

        {/* Compact Kanban Board - 3 columns, fills available space */}
        <div className="flex-1 min-h-0 grid grid-cols-3 gap-4 mb-4">
          {columnData.map((col) => {
            const ColIcon = col.icon;
            return (
              <div
                key={col.id}
                className="flex flex-col min-h-0 rounded-xl bg-white/[0.01] border border-white/[0.04] overflow-hidden"
              >
                <div className="flex items-center justify-between px-3 py-2.5 border-b border-white/[0.04]">
                  <div className="flex items-center gap-2">
                    <ColIcon className={cn('h-3.5 w-3.5', col.id === 'running' && 'animate-spin', col.id === 'running' ? 'text-indigo-400' : col.id === 'needs-you' ? 'text-amber-400' : 'text-white/40')} />
                    <span className="text-xs font-medium text-white/70">{col.label}</span>
                  </div>
                  {col.missions.length > 0 && (
                    <span className="text-[10px] text-white/30 tabular-nums">
                      {col.missions.length}
                    </span>
                  )}
                </div>
                <div className="flex-1 overflow-y-auto p-2 space-y-2">
                  {col.missions.length === 0 ? (
                    <div className="flex flex-col items-center justify-center py-10 text-center">
                      <p className="text-[10px] text-white/20">
                        {col.id === 'running' ? 'No active missions' : col.id === 'needs-you' ? 'All good!' : 'No recent missions'}
                      </p>
                    </div>
                  ) : (
                    col.missions.map((mission) => (
                      <CompactMissionCard
                        key={mission.id}
                        mission={mission}
                        isActuallyRunning={runningMissionIds.has(mission.id)}
                        onCancel={handleCancel}
                        onResume={handleResume}
                        onDelete={handleDelete}
                      />
                    ))
                  )}
                </div>
              </div>
            );
          })}
        </div>

        {/* Stats grid - fixed at bottom */}
        <div className="flex-shrink-0 grid grid-cols-4 gap-4">
          {statsLoading ? (
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
        <RecentTasks />
      </div>
    </div>
  );
}
