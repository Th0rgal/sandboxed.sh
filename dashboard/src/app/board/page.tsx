'use client';

import { useMemo, useCallback } from 'react';
import Link from 'next/link';
import { useRouter } from 'next/navigation';
import useSWR from 'swr';
import { cn } from '@/lib/utils';
import {
  listMissions,
  getRunningMissions,
  cancelMission,
  deleteMission,
  resumeMission,
  type Mission,
  type MissionStatus,
  type RunningMissionInfo,
} from '@/lib/api';
import { toast } from '@/components/toast';
import { RelativeTime } from '@/components/ui/relative-time';
import {
  Loader,
  CheckCircle,
  XCircle,
  Ban,
  Clock,
  ArrowRight,
  RotateCcw,
  Trash2,
  Target,
  Kanban,
} from 'lucide-react';

interface Column {
  id: string;
  label: string;
  statuses: MissionStatus[];
  color: string;
  dotColor: string;
  emptyLabel: string;
}

const columns: Column[] = [
  {
    id: 'queued',
    label: 'Queued',
    statuses: ['interrupted', 'blocked'],
    color: 'border-amber-500/30',
    dotColor: 'bg-amber-400',
    emptyLabel: 'No queued missions',
  },
  {
    id: 'running',
    label: 'Running',
    statuses: ['active'],
    color: 'border-indigo-500/30',
    dotColor: 'bg-indigo-400',
    emptyLabel: 'No active missions',
  },
  {
    id: 'completed',
    label: 'Completed',
    statuses: ['completed'],
    color: 'border-emerald-500/30',
    dotColor: 'bg-emerald-400',
    emptyLabel: 'No completed missions',
  },
  {
    id: 'failed',
    label: 'Failed',
    statuses: ['failed', 'not_feasible'],
    color: 'border-red-500/30',
    dotColor: 'bg-red-400',
    emptyLabel: 'No failed missions',
  },
];

const statusIcons: Record<string, typeof Clock> = {
  active: Loader,
  completed: CheckCircle,
  failed: XCircle,
  interrupted: Ban,
  blocked: Ban,
  not_feasible: XCircle,
};

const statusColors: Record<string, string> = {
  active: 'text-indigo-400',
  completed: 'text-emerald-400',
  failed: 'text-red-400',
  interrupted: 'text-amber-400',
  blocked: 'text-orange-400',
  not_feasible: 'text-rose-400',
};

function getMissionTitle(mission: Mission): string {
  if (mission.title) return mission.title;
  const firstUser = mission.history?.find((h) => h.role === 'user');
  if (firstUser?.content) {
    const content = firstUser.content.trim();
    return content.length > 80 ? content.slice(0, 80) + '...' : content;
  }
  return 'Untitled Mission';
}

function MissionCard({
  mission,
  runningInfo,
  onCancel,
  onResume,
  onDelete,
}: {
  mission: Mission;
  runningInfo?: RunningMissionInfo;
  onCancel: (id: string) => void;
  onResume: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  const Icon = statusIcons[mission.status] || Clock;
  const color = statusColors[mission.status] || 'text-white/40';
  const title = getMissionTitle(mission);
  const isRunning = mission.status === 'active';
  const isResumable = mission.resumable && mission.status === 'interrupted';

  return (
    <div className="group rounded-lg bg-white/[0.02] border border-white/[0.06] hover:border-white/[0.12] p-3 transition-colors">
      <div className="flex items-start gap-2.5 mb-2">
        <Icon
          className={cn(
            'h-4 w-4 mt-0.5 shrink-0',
            color,
            isRunning && 'animate-spin'
          )}
        />
        <Link
          href={`/control?mission=${mission.id}`}
          className="flex-1 min-w-0"
        >
          <p className="text-sm text-white/80 leading-snug line-clamp-2 hover:text-white transition-colors">
            {title}
          </p>
        </Link>
      </div>

      <div className="flex items-center gap-2 flex-wrap mb-2">
        {mission.workspace_name && (
          <span className="inline-flex items-center rounded-md bg-white/[0.04] px-1.5 py-0.5 text-[10px] text-white/50">
            {mission.workspace_name}
          </span>
        )}
        {mission.agent && (
          <span className="inline-flex items-center rounded-md bg-indigo-500/10 px-1.5 py-0.5 text-[10px] text-indigo-400">
            {mission.agent}
          </span>
        )}
        {mission.backend && mission.backend !== 'opencode' && (
          <span className="inline-flex items-center rounded-md bg-white/[0.04] px-1.5 py-0.5 text-[10px] text-white/40">
            {mission.backend}
          </span>
        )}
      </div>

      {runningInfo && (
        <div className="flex items-center gap-2 mb-2 text-[10px] text-white/30">
          <span>{runningInfo.queue_len} queued</span>
          <span>·</span>
          <span>{runningInfo.history_len} msgs</span>
          {runningInfo.seconds_since_activity > 60 && (
            <>
              <span>·</span>
              <span className="text-amber-400">
                {Math.floor(runningInfo.seconds_since_activity)}s idle
              </span>
            </>
          )}
        </div>
      )}

      <div className="flex items-center justify-between">
        <RelativeTime
          date={mission.updated_at}
          className="text-[10px] text-white/30"
        />
        <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          {isResumable && (
            <button
              onClick={() => onResume(mission.id)}
              className="p-1 rounded hover:bg-white/[0.08] text-white/40 hover:text-emerald-400 transition-colors"
              title="Resume mission"
            >
              <RotateCcw className="h-3.5 w-3.5" />
            </button>
          )}
          {isRunning && (
            <button
              onClick={() => onCancel(mission.id)}
              className="p-1 rounded hover:bg-white/[0.08] text-white/40 hover:text-red-400 transition-colors"
              title="Cancel mission"
            >
              <XCircle className="h-3.5 w-3.5" />
            </button>
          )}
          {!isRunning && (
            <button
              onClick={() => onDelete(mission.id)}
              className="p-1 rounded hover:bg-white/[0.08] text-white/40 hover:text-red-400 transition-colors"
              title="Delete mission"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          )}
          <Link
            href={`/control?mission=${mission.id}`}
            className="p-1 rounded hover:bg-white/[0.08] text-white/40 hover:text-indigo-400 transition-colors"
            title="Open mission"
          >
            <ArrowRight className="h-3.5 w-3.5" />
          </Link>
        </div>
      </div>
    </div>
  );
}

export default function BoardPage() {
  const router = useRouter();

  const {
    data: missions = [],
    mutate: mutateMissions,
  } = useSWR('missions', listMissions, {
    refreshInterval: 5000,
    revalidateOnFocus: false,
  });

  const { data: runningMissions = [] } = useSWR(
    'running-missions',
    getRunningMissions,
    {
      refreshInterval: 3000,
      revalidateOnFocus: false,
    }
  );

  const runningMap = useMemo(() => {
    const map = new Map<string, RunningMissionInfo>();
    for (const rm of runningMissions) {
      map.set(rm.mission_id, rm);
    }
    return map;
  }, [runningMissions]);

  const columnData = useMemo(() => {
    return columns.map((col) => {
      const colMissions = missions
        .filter((m) => col.statuses.includes(m.status))
        .sort(
          (a, b) =>
            new Date(b.updated_at).getTime() -
            new Date(a.updated_at).getTime()
        );
      return { ...col, missions: colMissions };
    });
  }, [missions]);

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
          (current) => current ? current.filter((m) => m.id !== id) : current,
          false
        );
        toast.success('Mission deleted');
      } catch {
        toast.error('Failed to delete mission');
      }
    },
    [mutateMissions]
  );

  const totalCount = missions.length;

  return (
    <div className="flex flex-col h-screen p-6">
      <div className="mb-6 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
            <Kanban className="h-5 w-5 text-indigo-400" />
          </div>
          <div>
            <h1 className="text-xl font-semibold text-white">Mission Board</h1>
            <p className="text-sm text-white/50">
              {totalCount} mission{totalCount !== 1 ? 's' : ''}
            </p>
          </div>
        </div>
      </div>

      <div className="flex-1 grid grid-cols-4 gap-4 min-h-0 overflow-hidden">
        {columnData.map((col) => (
          <div
            key={col.id}
            className="flex flex-col min-h-0 rounded-xl bg-white/[0.01] border border-white/[0.04]"
          >
            <div
              className={cn(
                'flex items-center justify-between px-4 py-3 border-b',
                col.color
              )}
            >
              <div className="flex items-center gap-2">
                <span
                  className={cn('h-2 w-2 rounded-full', col.dotColor)}
                />
                <span className="text-sm font-medium text-white">
                  {col.label}
                </span>
              </div>
              <span className="text-xs text-white/40 tabular-nums">
                {col.missions.length}
              </span>
            </div>

            <div className="flex-1 overflow-y-auto p-2 space-y-2">
              {col.missions.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-12 text-center">
                  <Target className="h-6 w-6 text-white/10 mb-2" />
                  <p className="text-xs text-white/30">{col.emptyLabel}</p>
                </div>
              ) : (
                col.missions.map((mission) => (
                  <MissionCard
                    key={mission.id}
                    mission={mission}
                    runningInfo={runningMap.get(mission.id)}
                    onCancel={handleCancel}
                    onResume={handleResume}
                    onDelete={handleDelete}
                  />
                ))
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
