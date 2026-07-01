'use client';

import { memo, useMemo, useState } from 'react';
import {
  Check,
  X,
  AlertTriangle,
  Ban,
  ChevronDown,
  ArrowLeft,
  Crown,
} from 'lucide-react';
import {
  motion,
  AnimatePresence,
  LayoutGroup,
  useReducedMotion,
} from 'framer-motion';
import { cn } from '@/lib/utils';
import { getMissionShortName } from '@/lib/mission-display';
import type { Mission, RunningMissionInfo } from '@/lib/api';

interface WorkersStripProps {
  /** Workers shown as chips. On a boss view these are children; on a
   * worker view they are siblings (so you can hop between workers). */
  childMissions: Mission[];
  runningMissions: RunningMissionInfo[];
  viewingMissionId?: string | null;
  onSelectWorker: (missionId: string) => void;
  /** When set, the strip renders a leading "Back to Boss" pill that
   * navigates to this mission. Use for worker views. */
  parentMission?: Mission | null;
  className?: string;
}

/**
 * Status is reduced to a small, learnable vocabulary so the row scans in a
 * glance. Color and motion are rationed: a *pulsing* dot means "alive", a
 * still dot or quiet glyph means "settled". Done is intentionally gray (not
 * green) so finished workers recede and live ones stand out.
 */
type ChipStatus = {
  /** Small left indicator: a colored dot for ongoing states, a muted glyph
   * for terminal ones. */
  indicator: React.ReactNode;
  /** Tab-underline color when this worker is the one being viewed. */
  seam: string;
  /** Faint progress-fill tint behind the chip. */
  fill: string;
  activity: string | null;
  /** Live = pulsing, sorts first, counts toward "active". */
  isActive: boolean;
  /** Completed successfully — folds into the "N done" cluster. */
  isDone: boolean;
};

function Dot({ color, pulse }: { color: string; pulse?: boolean }) {
  return (
    <span className="relative flex h-2 w-2 shrink-0 items-center justify-center">
      {pulse && (
        <span
          className={cn(
            'absolute inline-flex h-full w-full rounded-full opacity-60 motion-safe:animate-ping',
            color
          )}
        />
      )}
      <span className={cn('relative inline-flex h-2 w-2 rounded-full', color)} />
    </span>
  );
}

const HollowDot = (
  <span className="h-2 w-2 shrink-0 rounded-full border border-white/40" />
);

function chipStatusFor(mission: Mission, info?: RunningMissionInfo): ChipStatus {
  const base = { activity: null, isActive: false, isDone: false };

  if (info) {
    if (info.state === 'running') {
      return {
        ...base,
        indicator: <Dot color="bg-indigo-400" pulse />,
        seam: 'bg-indigo-400',
        fill: 'bg-indigo-500/10',
        activity: info.current_activity || null,
        isActive: true,
      };
    }
    if (info.state === 'waiting_for_tool') {
      return {
        ...base,
        indicator: <Dot color="bg-amber-400" pulse />,
        seam: 'bg-amber-400',
        fill: 'bg-amber-500/10',
        activity: info.current_activity || 'Waiting for tool',
        isActive: true,
      };
    }
    if (info.state === 'queued') {
      return {
        ...base,
        indicator: HollowDot,
        seam: 'bg-white/40',
        fill: '',
        activity: 'Queued',
      };
    }
  }

  switch (mission.status) {
    case 'completed':
      return {
        ...base,
        indicator: <Check className="h-3 w-3 shrink-0 text-white/35" />,
        seam: 'bg-white/30',
        fill: '',
        isDone: true,
      };
    case 'failed':
      return {
        ...base,
        indicator: <X className="h-3 w-3 shrink-0 text-red-400" />,
        seam: 'bg-red-400',
        fill: '',
      };
    case 'not_feasible':
      return {
        ...base,
        indicator: <Ban className="h-3 w-3 shrink-0 text-red-400" />,
        seam: 'bg-red-400',
        fill: '',
      };
    case 'interrupted':
      return {
        ...base,
        indicator: <AlertTriangle className="h-3 w-3 shrink-0 text-amber-400" />,
        seam: 'bg-amber-400',
        fill: '',
      };
    case 'active':
    case 'waiting_background':
      return {
        ...base,
        indicator: <Dot color="bg-indigo-400" pulse />,
        seam: 'bg-indigo-400',
        fill: 'bg-indigo-500/10',
        isActive: true,
      };
    default:
      return {
        ...base,
        indicator: HollowDot,
        seam: 'bg-white/30',
        fill: '',
      };
  }
}

type ChipModel = {
  mission: Mission;
  info?: RunningMissionInfo;
  status: ChipStatus;
};

/**
 * Horizontal, sticky strip of worker chips. Sits at the top of the chat
 * container so the boss can see active workers without opening a side
 * panel. Click-to-switch into a worker. Self-hides when there are no
 * children.
 *
 * Design: a calm, borderless row. Live workers pulse and lead; completed
 * ones fold into a "N done" cluster so success recedes and attention is
 * reserved for what's running or stuck. The viewed worker carries a sliding
 * tab underline. Chips glide (FLIP) when the active-first sort reshuffles.
 *
 * Performance note: memoized; the sort + chip-info derivation is
 * recomputed only when `childMissions` or `runningMissions` change. The
 * chat scroll never re-renders this strip because it lives outside the
 * scrolling region.
 */
export const WorkersStrip = memo(function WorkersStrip({
  childMissions,
  runningMissions,
  viewingMissionId,
  onSelectWorker,
  parentMission,
  className,
}: WorkersStripProps) {
  const reduce = useReducedMotion();
  const [showDone, setShowDone] = useState(false);

  const { attention, done, activeCount } = useMemo(() => {
    if (childMissions.length === 0) {
      return {
        attention: [] as ChipModel[],
        done: [] as ChipModel[],
        activeCount: 0,
      };
    }
    const running = new Map<string, RunningMissionInfo>();
    for (const rm of runningMissions) running.set(rm.mission_id, rm);

    const models: ChipModel[] = [...childMissions]
      .map((m) => {
        const info = running.get(m.id);
        return { mission: m, info, status: chipStatusFor(m, info) };
      })
      .sort((a, b) => {
        // Active first, then by updated_at desc.
        if (a.status.isActive !== b.status.isActive)
          return a.status.isActive ? -1 : 1;
        const at = a.mission.updated_at || a.mission.created_at || '';
        const bt = b.mission.updated_at || b.mission.created_at || '';
        return bt.localeCompare(at);
      });

    return {
      attention: models.filter((m) => !m.status.isDone),
      done: models.filter((m) => m.status.isDone),
      activeCount: models.filter((m) => m.status.isActive).length,
    };
  }, [childMissions, runningMissions]);

  const total = attention.length + done.length;

  // Nothing to show: no parent link AND no worker chips.
  if (!parentMission && total === 0) return null;

  const onWorkerView = Boolean(parentMission);
  const parentTitle = parentMission
    ? parentMission.title?.trim() || getMissionShortName(parentMission.id)
    : null;
  // Divider sits before the first non-live attention chip (live lead the row).
  const firstSettledIndex = attention.findIndex((c) => !c.status.isActive);

  // `presence` enables enter/exit fade for chips mounted inside an
  // <AnimatePresence> (the collapsible "done" group).
  const renderChip = ({ mission, info, status }: ChipModel, presence = false) => {
    const isViewing = mission.id === viewingMissionId;
    const title = mission.title?.trim() || getMissionShortName(mission.id);
    const pct =
      info && info.subtask_total > 0
        ? Math.min(
            100,
            Math.round((info.subtask_completed / info.subtask_total) * 100)
          )
        : null;

    return (
      <motion.button
        key={mission.id}
        layout={reduce ? false : 'position'}
        initial={presence && !reduce ? { opacity: 0 } : false}
        animate={presence ? { opacity: 1 } : undefined}
        exit={presence && !reduce ? { opacity: 0 } : undefined}
        transition={{ type: 'spring', stiffness: 500, damping: 40 }}
        onClick={() => onSelectWorker(mission.id)}
        aria-current={isViewing ? 'true' : undefined}
        className={cn(
          'group relative shrink-0 inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-[11px] max-w-[260px]',
          'transition-colors duration-150 active:translate-y-px',
          'hover:bg-white/[0.05]',
          isViewing && 'bg-white/[0.06]'
        )}
        title={status.activity ? `${title}: ${status.activity}` : title}
      >
        {/* Progress fill — faint, left-anchored, behind the content. */}
        {pct !== null && status.fill && (
          <span
            aria-hidden
            className={cn('absolute inset-y-0 left-0 rounded-l-md', status.fill)}
            style={{ width: `${pct}%` }}
          />
        )}
        <span className="relative z-10 flex min-w-0 items-center gap-1.5">
          {status.indicator}
          <span className="min-w-0 truncate font-medium text-foreground/85">
            {title}
          </span>
          {status.activity && status.isActive && (
            <span className="hidden truncate text-white/40 lg:inline max-w-[120px]">
              {status.activity}
            </span>
          )}
          {pct !== null && (
            <span className="hidden tabular-nums text-white/35 lg:inline">
              {pct}%
            </span>
          )}
        </span>
        {/* Sliding "you are here" tab underline, shared across chips. */}
        {isViewing &&
          (reduce ? (
            <span
              aria-hidden
              className={cn(
                'absolute inset-x-1.5 -bottom-[3px] h-0.5 rounded-full',
                status.seam
              )}
            />
          ) : (
            <motion.span
              aria-hidden
              layoutId="worker-tab-indicator"
              transition={{ type: 'spring', stiffness: 500, damping: 40 }}
              className={cn(
                'absolute inset-x-1.5 -bottom-[3px] h-0.5 rounded-full',
                status.seam
              )}
            />
          ))}
      </motion.button>
    );
  };

  return (
    <div
      className={cn(
        'relative flex items-center gap-1.5 px-3 py-1.5 border-b border-white/[0.06] overflow-x-auto',
        'scrollbar-thin scrollbar-thumb-white/10 scrollbar-track-transparent',
        className
      )}
      aria-label={onWorkerView ? 'Worker navigation' : 'Active workers'}
    >
      {parentMission && (
        <>
          <button
            type="button"
            onClick={() => onSelectWorker(parentMission.id)}
            className={cn(
              'shrink-0 inline-flex h-7 items-center gap-1 rounded-md px-2 text-[11px] font-medium max-w-[280px]',
              'bg-violet-500/10 text-violet-300 hover:bg-violet-500/[0.16]',
              'transition-colors duration-150 active:translate-y-px'
            )}
            title={`Back to boss: ${parentTitle}`}
            aria-label={`Back to boss mission ${parentTitle}`}
          >
            <ArrowLeft className="h-3 w-3 shrink-0" />
            <Crown className="h-3 w-3 shrink-0" />
            <span className="min-w-0 truncate">{parentTitle}</span>
          </button>
          {total > 0 && (
            <span aria-hidden className="shrink-0 h-3.5 w-px bg-white/10" />
          )}
        </>
      )}

      {total > 0 && (
        <span
          className="shrink-0 inline-flex items-center gap-1 text-[10px] font-medium text-white/40 mr-0.5"
          title={`${activeCount} active of ${total} ${onWorkerView ? 'siblings' : 'workers'}`}
        >
          <span>{onWorkerView ? 'siblings' : 'workers'}</span>
          <span className="tabular-nums">
            <span
              className={activeCount > 0 ? 'text-indigo-300' : 'text-white/55'}
            >
              {activeCount}
            </span>
            <span className="text-white/30">/{total}</span>
          </span>
        </span>
      )}

      <LayoutGroup>
        {attention.map((chip, index) => {
          const showDivider =
            index !== 0 && index === firstSettledIndex && activeCount > 0;
          return (
            <span key={chip.mission.id} className="contents">
              {showDivider && (
                <span
                  aria-hidden
                  className="shrink-0 h-3.5 w-px bg-white/10 mx-0.5"
                  title="Idle workers"
                />
              )}
              {renderChip(chip)}
            </span>
          );
        })}

        {/* Completed workers fold away — success recedes, attention stays. */}
        {done.length > 0 && (
          <button
            type="button"
            onClick={() => setShowDone((s) => !s)}
            aria-expanded={showDone}
            className={cn(
              'shrink-0 inline-flex h-7 items-center gap-1 rounded-md px-2 text-[11px]',
              'text-white/45 hover:bg-white/[0.05] hover:text-white/70',
              'transition-colors duration-150 active:translate-y-px'
            )}
            title={showDone ? 'Hide completed workers' : 'Show completed workers'}
          >
            <Check className="h-3 w-3 shrink-0" />
            <span className="tabular-nums">{done.length} done</span>
            <ChevronDown
              className={cn(
                'h-3 w-3 shrink-0 transition-transform duration-150',
                showDone && 'rotate-180'
              )}
            />
          </button>
        )}

        <AnimatePresence initial={false}>
          {showDone && done.map((chip) => renderChip(chip, true))}
        </AnimatePresence>
      </LayoutGroup>
    </div>
  );
});
