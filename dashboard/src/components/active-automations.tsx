'use client';

import { useMemo, useState } from 'react';
import Link from 'next/link';
import useSWR from 'swr';
import { Workflow, Trash2, PauseCircle, Loader, Check, X } from 'lucide-react';
import {
  listActiveAutomations,
  getMission,
  updateAutomation,
  deleteAutomation,
  type Automation,
  type Mission,
  type MissionStatus,
} from '@/lib/api';
import { cn } from '@/lib/utils';
import { stableJsonCompare } from '@/lib/swr-config';
import { useToast } from '@/components/toast';

interface ActiveAutomationsProps {
  missions: Mission[];
  runningMissionIds: Set<string>;
}

// Statuses where an agent_finished / goal-loop automation can still legitimately
// fire. Anything else means the linked mission has ended, so the automation is
// orphaned and lingering only because its stop_policy never triggered.
const LIVE_STATUSES: ReadonlySet<MissionStatus> = new Set<MissionStatus>([
  'pending',
  'active',
  'awaiting_user',
  'blocked',
  'waiting_background',
]);

/**
 * "Active automations" panel for the Overview right sidebar. Lists every
 * active automation across all missions, surfaces stale/orphaned ones, and
 * offers inline disable/remove plus a selection mode for bulk cleanup.
 * Fills the sidebar's remaining vertical space (the parent provides the
 * flex context). Shares the `active-automations` SWR key with the page so
 * the kanban badges stay in sync.
 */
export function ActiveAutomations({ missions, runningMissionIds }: ActiveAutomationsProps) {
  const { data: automations = [], isLoading, mutate } = useSWR(
    'active-automations',
    listActiveAutomations,
    { refreshInterval: 5000, revalidateOnFocus: false, compare: stableJsonCompare },
  );

  const missionMap = useMemo(
    () => new Map(missions.map((m) => [m.id, m])),
    [missions],
  );

  // The page only loads the most recent missions, so some automations point at
  // older missions absent from the list. Fetch just those for accurate titles.
  const missingIds = useMemo(() => {
    const ids = automations
      .map((a) => a.mission_id)
      .filter((id) => !missionMap.has(id));
    return Array.from(new Set(ids)).sort();
  }, [automations, missionMap]);

  const { data: extraMissions = [] } = useSWR(
    missingIds.length ? ['automation-missions', missingIds.join(',')] : null,
    () =>
      Promise.all(missingIds.map((id) => getMission(id).catch(() => null))).then(
        (rows) => rows.filter((m): m is Mission => m !== null),
      ),
    { revalidateOnFocus: false, compare: stableJsonCompare },
  );

  const lookup = useMemo(() => {
    const map = new Map(missionMap);
    for (const m of extraMissions) map.set(m.id, m);
    return map;
  }, [missionMap, extraMissions]);

  const rows = useMemo(
    () =>
      automations
        .map((a) => {
          const mission = lookup.get(a.mission_id) ?? null;
          const stale = isStale(a, mission, runningMissionIds);
          return { automation: a, mission, stale };
        })
        // Stale first, then newest by created_at.
        .sort((x, y) => {
          if (x.stale !== y.stale) return x.stale ? -1 : 1;
          return (y.automation.created_at ?? '').localeCompare(x.automation.created_at ?? '');
        }),
    [automations, lookup, runningMissionIds],
  );

  const staleCount = rows.filter((r) => r.stale).length;

  // Selection mode for bulk removal.
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  const [confirmBulk, setConfirmBulk] = useState(false);
  const { showError } = useToast();

  function exitSelection() {
    setSelecting(false);
    setSelected(new Set());
    setConfirmBulk(false);
  }

  function toggle(id: string) {
    setConfirmBulk(false);
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function selectAll() {
    setConfirmBulk(false);
    setSelected(new Set(rows.map((r) => r.automation.id)));
  }

  function selectStale() {
    setConfirmBulk(false);
    setSelected(new Set(rows.filter((r) => r.stale).map((r) => r.automation.id)));
  }

  async function removeSelected() {
    if (selected.size === 0) return;
    if (!confirmBulk) {
      setConfirmBulk(true);
      return;
    }
    setBulkBusy(true);
    try {
      const ids = Array.from(selected);
      const results = await Promise.allSettled(
        ids.map((id) => deleteAutomation(id)),
      );
      const failed = ids.filter((_, i) => results[i].status === 'rejected');
      mutate();
      if (failed.length > 0) {
        // Keep the failed ids selected so the user can retry just those.
        setSelected(new Set(failed));
        setConfirmBulk(false);
        showError(
          `Failed to remove ${failed.length} of ${ids.length} automation${ids.length === 1 ? '' : 's'}`,
        );
      } else {
        exitSelection();
      }
    } finally {
      setBulkBusy(false);
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col rounded-xl border border-border bg-[rgb(var(--foreground)/0.025)] p-3">
      <div className="mb-2 flex h-6 flex-shrink-0 items-center justify-between">
        <div className="flex items-center gap-2 text-xs font-medium text-[rgb(var(--foreground)/0.75)]">
          <Workflow className="h-3.5 w-3.5 text-primary" />
          <span>Automations</span>
        </div>
        {selecting ? (
          <button
            onClick={exitSelection}
            disabled={bulkBusy}
            className="inline-flex h-5 items-center gap-1 rounded-md px-1.5 text-[10px] font-medium text-[rgb(var(--foreground)/0.45)] hover:bg-[rgb(var(--foreground)/0.06)] hover:text-[rgb(var(--foreground)/0.75)] disabled:opacity-50"
          >
            <X className="h-3 w-3" />
            Done
          </button>
        ) : (
          <div className="flex items-center gap-2">
            <span className="text-[10px] tabular-nums text-[rgb(var(--foreground)/0.4)]">
              {rows.length}
              {staleCount > 0 && ` · ${staleCount} stale`}
            </span>
            {rows.length > 1 && (
              <button
                onClick={() => setSelecting(true)}
                className="inline-flex h-5 items-center rounded-md px-1.5 text-[10px] font-medium text-[rgb(var(--foreground)/0.45)] hover:bg-[rgb(var(--foreground)/0.06)] hover:text-[rgb(var(--foreground)/0.75)]"
              >
                Select
              </button>
            )}
          </div>
        )}
      </div>

      {selecting && (
        <div className="mb-2 flex h-7 flex-shrink-0 items-center justify-between gap-2 rounded-lg border border-border bg-[rgb(var(--foreground)/0.03)] px-2">
          <div className="flex items-center gap-1">
            <span className="pr-1 text-[10px] tabular-nums text-[rgb(var(--foreground)/0.5)]">
              {selected.size} selected
            </span>
            <button
              onClick={selectAll}
              disabled={bulkBusy}
              className="inline-flex h-5 items-center rounded px-1.5 text-[10px] font-medium text-[rgb(var(--foreground)/0.55)] hover:bg-[rgb(var(--foreground)/0.07)] disabled:opacity-50"
            >
              All
            </button>
            {staleCount > 0 && (
              <button
                onClick={selectStale}
                disabled={bulkBusy}
                className="inline-flex h-5 items-center rounded px-1.5 text-[10px] font-medium text-amber-300/80 hover:bg-amber-400/10 disabled:opacity-50"
              >
                Stale
              </button>
            )}
          </div>
          <button
            onClick={removeSelected}
            disabled={bulkBusy || selected.size === 0}
            className={cn(
              'inline-flex h-5 items-center gap-1 rounded px-1.5 text-[10px] font-medium transition-colors disabled:opacity-40',
              confirmBulk
                ? 'bg-[rgb(var(--error)/0.15)] text-destructive'
                : 'text-destructive hover:bg-[rgb(var(--error)/0.1)]',
            )}
          >
            {bulkBusy ? (
              <Loader className="h-3 w-3 animate-spin" />
            ) : (
              <Trash2 className="h-3 w-3" />
            )}
            {confirmBulk ? `Confirm ${selected.size}` : 'Remove'}
          </button>
        </div>
      )}

      {isLoading && rows.length === 0 ? (
        <div className="flex items-center gap-2 py-2 text-[11px] text-[rgb(var(--foreground)/0.4)]">
          <Loader className="h-3 w-3 animate-spin" /> Loading…
        </div>
      ) : rows.length === 0 ? (
        <p className="py-2 text-[11px] text-[rgb(var(--foreground)/0.4)]">No active automations.</p>
      ) : (
        <div className="-mr-1 flex min-h-0 flex-1 flex-col gap-1.5 overflow-y-auto pr-1">
          {rows.map(({ automation, mission, stale }) => (
            <AutomationRow
              key={automation.id}
              automation={automation}
              mission={mission}
              stale={stale}
              selecting={selecting}
              selected={selected.has(automation.id)}
              onToggle={() => toggle(automation.id)}
              onMutate={mutate}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function AutomationRow({
  automation,
  mission,
  stale,
  selecting,
  selected,
  onToggle,
  onMutate,
}: {
  automation: Automation;
  mission: Mission | null;
  stale: boolean;
  selecting: boolean;
  selected: boolean;
  onToggle: () => void;
  onMutate: () => void;
}) {
  const [busy, setBusy] = useState<null | 'disable' | 'delete'>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const title =
    mission?.title?.trim() ||
    automationLabel(automation) ||
    `Mission ${automation.mission_id.slice(0, 8)}`;

  async function handleDisable() {
    setBusy('disable');
    try {
      await updateAutomation(automation.id, { active: false });
      onMutate();
    } finally {
      setBusy(null);
    }
  }

  async function handleDelete() {
    setBusy('delete');
    try {
      await deleteAutomation(automation.id);
      onMutate();
    } finally {
      setBusy(null);
      setConfirmDelete(false);
    }
  }

  const heading = (
    <>
      {selecting ? (
        <span
          className={cn(
            'mt-0.5 flex h-3.5 w-3.5 flex-shrink-0 items-center justify-center rounded border transition-colors',
            selected
              ? 'border-primary bg-primary text-primary-foreground'
              : 'border-[rgb(var(--foreground)/0.25)] bg-transparent',
          )}
          aria-hidden
        >
          {selected && <Check className="h-2.5 w-2.5" strokeWidth={3} />}
        </span>
      ) : (
        <span
          className={cn(
            'mt-1 h-1.5 w-1.5 flex-shrink-0 rounded-full',
            stale ? 'harness-status-dot-amber' : 'harness-status-dot-emerald',
          )}
        />
      )}
      <span className="truncate text-[11px] font-medium text-[rgb(var(--foreground)/0.82)]">
        {title}
      </span>
    </>
  );

  return (
    <div
      className={cn(
        'rounded-lg border px-2.5 py-2 transition-colors',
        selecting && selected
          ? 'border-[rgb(var(--primary)/0.4)] bg-[rgb(var(--primary)/0.06)]'
          : 'border-border bg-[rgb(var(--foreground)/0.02)]',
        selecting && 'cursor-pointer hover:bg-[rgb(var(--foreground)/0.04)]',
      )}
      onClick={selecting ? onToggle : undefined}
      role={selecting ? 'checkbox' : undefined}
      aria-checked={selecting ? selected : undefined}
    >
      <div className="flex items-start justify-between gap-2">
        {selecting ? (
          <span
            className="flex min-w-0 flex-1 items-start gap-1.5"
            title={automationLabel(automation) || title}
          >
            {heading}
          </span>
        ) : (
          <Link
            href={`/control?mission=${automation.mission_id}`}
            className="flex min-w-0 flex-1 items-start gap-1.5 hover:underline"
            title={automationLabel(automation) || title}
          >
            {heading}
          </Link>
        )}
        <span className="inline-flex h-4 flex-shrink-0 items-center rounded border border-border bg-[rgb(var(--foreground)/0.04)] px-1.5 text-[9px] font-medium uppercase leading-none tracking-wide text-[rgb(var(--foreground)/0.5)]">
          {triggerChip(automation)}
        </span>
      </div>

      <div className="mt-1.5 flex items-center justify-between gap-2 pl-3">
        <div className="flex min-w-0 items-center gap-1.5 text-[10px] leading-none text-[rgb(var(--foreground)/0.45)]">
          {stale && (
            <span className="flex-shrink-0 font-medium text-amber-300/80">stale</span>
          )}
          {mission && (
            <span className="flex min-w-0 items-center truncate">
              {stale && <span className="pr-1 text-[rgb(var(--foreground)/0.25)]">·</span>}
              {mission.status}
            </span>
          )}
          {(stale || mission) && (
            <span className="flex-shrink-0 text-[rgb(var(--foreground)/0.25)]">·</span>
          )}
          <span className="whitespace-nowrap" title={automation.created_at}>
            {ageLabel(automation.created_at)}
          </span>
        </div>

        {!selecting && (
          <div className="flex flex-shrink-0 items-center gap-0.5">
            {confirmDelete ? (
              <>
                <button
                  onClick={handleDelete}
                  disabled={busy !== null}
                  className="rounded px-1.5 py-0.5 text-[10px] font-medium text-destructive hover:bg-[rgb(var(--error)/0.12)] disabled:opacity-50"
                >
                  {busy === 'delete' ? '…' : 'Remove'}
                </button>
                <button
                  onClick={() => setConfirmDelete(false)}
                  disabled={busy !== null}
                  className="rounded px-1.5 py-0.5 text-[10px] text-[rgb(var(--foreground)/0.45)] hover:bg-[rgb(var(--foreground)/0.06)]"
                >
                  Cancel
                </button>
              </>
            ) : (
              <>
                <button
                  onClick={handleDisable}
                  disabled={busy !== null}
                  title="Disable (keeps the row, stops firing)"
                  className="harness-icon-action text-[rgb(var(--foreground)/0.4)] hover:bg-[rgb(var(--foreground)/0.06)] hover:text-[rgb(var(--foreground)/0.75)]"
                >
                  <PauseCircle className="h-3.5 w-3.5" />
                </button>
                <button
                  onClick={() => setConfirmDelete(true)}
                  disabled={busy !== null}
                  title="Remove permanently"
                  className="harness-icon-action harness-icon-action-danger"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function isStale(
  automation: Automation,
  mission: Mission | null,
  runningMissionIds: Set<string>,
): boolean {
  if (runningMissionIds.has(automation.mission_id)) return false;
  // Mission not among recent missions and not running → it has ended long ago.
  if (!mission) return true;
  return !LIVE_STATUSES.has(mission.status);
}

/** Short, human label derived from the command source. */
function automationLabel(a: Automation): string {
  const cs = a.command_source;
  switch (cs.type) {
    case 'native_loop': {
      const objective = (cs.args as { objective?: unknown } | null | undefined)?.objective;
      return typeof objective === 'string' && objective.trim()
        ? firstLine(objective)
        : `${cs.harness} /${cs.command}`;
    }
    case 'inline':
      return firstLine(cs.content);
    case 'library':
      return cs.name;
    case 'local_file':
      return cs.path.split('/').pop() || cs.path;
    default:
      return '';
  }
}

/** Compact trigger descriptor shown as a chip. */
function triggerChip(a: Automation): string {
  if (a.command_source.type === 'native_loop') return 'goal·loop';
  const trigger = a.trigger as { type: string; seconds?: number };
  switch (trigger.type) {
    case 'agent_finished':
      return 'on finish';
    case 'interval':
      return `every ${humanizeSeconds(trigger.seconds ?? 0)}`;
    case 'webhook':
      return 'webhook';
    case 'cron':
      return 'cron';
    case 'durable_job_terminal':
      return 'on job finish';
    case 'telegram':
      return 'telegram';
    default:
      return trigger.type;
  }
}

function firstLine(text: string): string {
  const line = text.trim().split('\n')[0]?.trim() ?? '';
  return line.length > 60 ? `${line.slice(0, 57)}…` : line;
}

function humanizeSeconds(s: number): string {
  if (s > 0 && s % 86400 === 0) return `${s / 86400}d`;
  if (s > 0 && s % 3600 === 0) return `${s / 3600}h`;
  if (s > 0 && s % 60 === 0) return `${s / 60}m`;
  return `${s}s`;
}

function ageLabel(iso: string | undefined): string {
  if (!iso) return '';
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return '';
  const diff = Date.now() - t;
  const days = Math.floor(diff / 86400000);
  if (days >= 1) return `${days}d ago`;
  const hours = Math.floor(diff / 3600000);
  if (hours >= 1) return `${hours}h ago`;
  const minutes = Math.floor(diff / 60000);
  if (minutes >= 1) return `${minutes}m ago`;
  return 'just now';
}
