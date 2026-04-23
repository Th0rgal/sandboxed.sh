'use client';

import { useMemo, useState, useEffect } from 'react';
import {
  X,
  Loader2,
  CheckCircle,
  XCircle,
  Clock,
  Users,
} from 'lucide-react';
import { cn } from '@/lib/utils';

/** Narrowed shape of the in-mission Task / sub-agent tool call.
 *  Matches the `kind: "tool"` variant of ChatItem in control-client.tsx
 *  but avoids a direct import to keep this component standalone. */
export interface SubagentEntry {
  id: string;
  toolCallId: string;
  name: string;
  args: unknown;
  result?: unknown;
  startTime: number;
  endTime?: number;
}

function extractAgentName(args: unknown): string | null {
  if (!args || typeof args !== 'object') return null;
  const obj = args as Record<string, unknown>;
  for (const key of ['agent', 'subagent_type', 'name']) {
    const v = obj[key];
    if (typeof v === 'string' && v.trim()) return v;
  }
  return null;
}

function extractDescription(args: unknown): string | null {
  if (!args || typeof args !== 'object') return null;
  const obj = args as Record<string, unknown>;
  const d = obj['description'];
  if (typeof d === 'string' && d.trim()) return d;
  const p = obj['prompt'];
  if (typeof p === 'string' && p.trim()) return p.slice(0, 140);
  return null;
}

type SubagentStatus = 'running' | 'completed' | 'failed' | 'cancelled';

function statusOf(entry: SubagentEntry): SubagentStatus {
  if (entry.result === undefined) return 'running';
  if (entry.result && typeof entry.result === 'object') {
    const r = entry.result as Record<string, unknown>;
    const cancelled =
      r.cancelled === true ||
      (typeof r.status === 'string' && r.status.toLowerCase().includes('cancel'));
    if (cancelled) return 'cancelled';
    const success =
      r.success === true ||
      (typeof r.status === 'string' && r.status.toLowerCase() === 'completed');
    if (success) return 'completed';
    if (r.error || r.success === false) return 'failed';
  }
  return 'completed';
}

function statusBadge(status: SubagentStatus) {
  switch (status) {
    case 'running':
      return {
        icon: <Loader2 className="h-3.5 w-3.5 animate-spin" />,
        label: 'Running',
        color: 'text-indigo-400',
        bg: 'bg-indigo-500/10 border-indigo-500/20',
      };
    case 'completed':
      return {
        icon: <CheckCircle className="h-3.5 w-3.5" />,
        label: 'Done',
        color: 'text-emerald-400',
        bg: 'bg-emerald-500/10 border-emerald-500/20',
      };
    case 'failed':
      return {
        icon: <XCircle className="h-3.5 w-3.5" />,
        label: 'Failed',
        color: 'text-red-400',
        bg: 'bg-red-500/10 border-red-500/20',
      };
    case 'cancelled':
      return {
        icon: <Clock className="h-3.5 w-3.5" />,
        label: 'Cancelled',
        color: 'text-white/50',
        bg: 'bg-white/[0.04] border-white/[0.08]',
      };
  }
}

function formatDuration(start: number, end?: number): string | null {
  const endMs = end ?? Date.now();
  const ms = Math.max(0, endMs - start);
  if (ms < 1_000) return `${ms}ms`;
  const s = Math.round(ms / 1_000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}m${r > 0 ? ` ${r}s` : ''}`;
}

interface SubagentsPanelProps {
  subagents: SubagentEntry[];
  onFocusItem: (itemId: string) => void;
  onClose: () => void;
  className?: string;
}

/** Initial window size — missions that orchestrate heavily can produce
 *  thousands of Task calls; rendering them all at once is a measurable
 *  paint cost. Most of the useful information (running + most-recent
 *  activity) is in the first couple hundred. */
const INITIAL_VISIBLE_SUBAGENTS = 200;
const LOAD_MORE_SUBAGENTS = 500;

/**
 * Sidebar panel listing in-mission sub-agent tool calls (Claude Code
 * `Task`, generic `background_task`, `spawn_agent`, `delegate`). These
 * aren't separate missions — they run inside the same harness process
 * — so the existing WorkerPanel (which expects Mission records with
 * `parent_mission_id`) can't represent them. This panel gives the
 * boss-orchestrated sub-agents a summary view with live status and
 * click-to-scroll into the chat.
 */
export function SubagentsPanel({
  subagents,
  onFocusItem,
  onClose,
  className,
}: SubagentsPanelProps) {
  // Most-recent first; active ones pinned to the top.
  const sorted = useMemo(() => {
    const enriched = subagents.map((s) => ({ entry: s, status: statusOf(s) }));
    enriched.sort((a, b) => {
      const aActive = a.status === 'running' ? 1 : 0;
      const bActive = b.status === 'running' ? 1 : 0;
      if (aActive !== bActive) return bActive - aActive;
      return b.entry.startTime - a.entry.startTime;
    });
    return enriched;
  }, [subagents]);

  // Windowed render: show a leading slice to keep paint cost bounded
  // on long-running orchestrator missions (thousands of Task calls).
  // Live-sorted so the tail of the list is the oldest entries — safe
  // to hide behind a "Show older" affordance. Reset whenever the
  // underlying list shrinks (e.g. switching missions).
  const [visibleCount, setVisibleCount] = useState(INITIAL_VISIBLE_SUBAGENTS);
  useEffect(() => {
    if (sorted.length < visibleCount) {
      setVisibleCount(Math.max(INITIAL_VISIBLE_SUBAGENTS, sorted.length));
    }
  }, [sorted.length, visibleCount]);
  const visible = sorted.slice(0, visibleCount);
  const hidden = Math.max(0, sorted.length - visibleCount);

  const counts = useMemo(() => {
    let active = 0;
    let done = 0;
    let failed = 0;
    let cancelled = 0;
    for (const { status } of sorted) {
      if (status === 'running') active += 1;
      else if (status === 'completed') done += 1;
      else if (status === 'failed') failed += 1;
      else cancelled += 1;
    }
    return { active, done, failed, cancelled };
  }, [sorted]);

  return (
    <div
      className={cn(
        'flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden',
        className
      )}
    >
      <div className="flex items-center justify-between px-4 py-3 border-b border-white/[0.06]">
        <div className="flex items-center gap-2">
          <Users className="h-4 w-4 text-violet-400" />
          <span className="text-sm font-medium text-white/90">Sub-agents</span>
          <span className="text-xs text-white/40 font-mono">{sorted.length}</span>
        </div>
        <button
          onClick={onClose}
          className="p-1 rounded hover:bg-white/[0.06] text-white/40 hover:text-white/70 transition-colors"
          title="Close sub-agents panel"
        >
          <X className="h-4 w-4" />
        </button>
      </div>

      {sorted.length > 0 && (
        <div className="flex items-center gap-3 px-4 py-2 border-b border-white/[0.04] text-[10px]">
          {counts.active > 0 && (
            <span className="flex items-center gap-1 text-indigo-400">
              <span className="h-1.5 w-1.5 rounded-full bg-indigo-400 animate-pulse" />
              {counts.active} active
            </span>
          )}
          {counts.done > 0 && (
            <span className="flex items-center gap-1 text-emerald-400">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
              {counts.done} done
            </span>
          )}
          {counts.failed > 0 && (
            <span className="flex items-center gap-1 text-red-400">
              <span className="h-1.5 w-1.5 rounded-full bg-red-400" />
              {counts.failed} failed
            </span>
          )}
          {counts.cancelled > 0 && (
            <span className="flex items-center gap-1 text-white/40">
              <span className="h-1.5 w-1.5 rounded-full bg-white/30" />
              {counts.cancelled} cancelled
            </span>
          )}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-3 space-y-2">
        {sorted.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full py-8 text-center">
            <Users className="h-8 w-8 text-white/10 mb-2" />
            <p className="text-sm text-white/30">No sub-agents yet</p>
            <p className="text-[11px] text-white/20 mt-1">
              Sub-agents appear here when the agent delegates tasks
            </p>
          </div>
        ) : (
          <>
            {visible.map(({ entry, status }) => {
              const badge = statusBadge(status);
              const agentName = extractAgentName(entry.args);
              const description = extractDescription(entry.args);
              const duration = formatDuration(entry.startTime, entry.endTime);
              return (
                // `content-visibility: auto` lets the browser skip layout
                // and paint for off-screen rows — significant on missions
                // with hundreds of sub-agents. `contain-intrinsic-size`
                // reserves space so scroll position stays stable.
                <div
                  key={entry.id}
                  style={{
                    contentVisibility: 'auto',
                    containIntrinsicSize: 'auto 68px',
                  }}
                >
                  <button
                    onClick={() => onFocusItem(entry.id)}
                    className={cn(
                      'w-full text-left rounded-xl border px-3 py-2.5 transition-colors',
                      'hover:bg-white/[0.04]',
                      badge.bg
                    )}
                  >
                    <div className="flex items-center gap-2">
                      <span className={cn('shrink-0', badge.color)}>{badge.icon}</span>
                      <span className="text-xs font-medium text-white/90 truncate">
                        {agentName ?? entry.name ?? 'Task'}
                      </span>
                      <span className={cn('ml-auto text-[10px] shrink-0', badge.color)}>
                        {badge.label}
                      </span>
                    </div>
                    {description && (
                      <p className="mt-1 text-[11px] text-white/50 line-clamp-2">
                        {description}
                      </p>
                    )}
                    {duration && (
                      <p className="mt-1 text-[10px] text-white/30 font-mono">{duration}</p>
                    )}
                  </button>
                </div>
              );
            })}
            {hidden > 0 && (
              <button
                onClick={() =>
                  setVisibleCount((prev) => prev + LOAD_MORE_SUBAGENTS)
                }
                className="w-full py-2 text-xs text-white/40 hover:text-white/70 hover:bg-white/[0.03] rounded-lg transition-colors"
              >
                Show {Math.min(LOAD_MORE_SUBAGENTS, hidden)} older sub-agents
                <span className="text-white/25 ml-2">({hidden} hidden)</span>
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
}
