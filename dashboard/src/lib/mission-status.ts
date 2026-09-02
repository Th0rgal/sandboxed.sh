/**
 * Mission status utilities - shared logic for categorizing missions
 * based on runtime state and stored status.
 */

import type { AwaitingKind, MissionStatus } from './api/missions';

export type MissionCategory = 'running' | 'needs-you' | 'finished' | 'other';
export type FinishedTone = 'green' | 'red';

// "Needs You" is a qualified operator page (`needs_operator` from the API).
// A bare `awaiting_user` list is not enough — ack and in-grace controller
// waits stay out of that column. Failure paths live in Finished (red).
export const NEEDS_ATTENTION_STATUSES: MissionStatus[] = [];
export const FINISHED_STATUSES: MissionStatus[] = [
  'completed',
  'acknowledged',
  'failed',
  'interrupted',
  'blocked',
  'not_feasible',
];

/** Statuses where a goal-loop automation may still legitimately fire. */
export const LIVE_AUTOMATION_STATUSES: MissionStatus[] = [
  'pending',
  'active',
  'awaiting_user',
  'blocked',
  'waiting_background',
];

/**
 * An automation should not promote its host into Running / Active unless
 * the harness is alive or the stored status is still in the live set.
 * Stale `agent_finished` rows on acknowledged missions used to inflate
 * Overview "Active" to 100+.
 */
export function isLiveAutomationHost(
  status: MissionStatus | undefined,
  isActuallyRunning: boolean,
): boolean {
  if (isActuallyRunning) return true;
  if (!status) return false;
  return LIVE_AUTOMATION_STATUSES.includes(status);
}

const FINISHED_GREEN_STATUSES: MissionStatus[] = ['completed', 'acknowledged'];

/**
 * Check if a mission is in a finished state based on its stored status.
 */
export function isFinishedStatus(status: MissionStatus): boolean {
  return FINISHED_STATUSES.includes(status);
}

/**
 * Check if a mission needs the operator. Trust the API `needs_operator`
 * flag so the dashboard does not reimplement controller-triage grace.
 */
export function needsAttentionStatus(
  _status: MissionStatus,
  needsOperator = false
): boolean {
  return needsOperator;
}

/**
 * Within the Finished column, "green" tone = agent-declared completion or
 * user acknowledgement, "red" tone = anything that ended badly.
 */
export function finishedTone(status: MissionStatus): FinishedTone {
  return FINISHED_GREEN_STATUSES.includes(status) ? 'green' : 'red';
}

/**
 * Categorize a mission based on runtime state and stored status.
 *
 * Priority order:
 * 1. Needs You — only when the API says `needs_operator` (decision /
 *    AskUserQuestion after grace / no origin).
 * 2. Running — actually executing, or waiting_for_tool that is not an
 *    operator page (controller still has first triage).
 * 3. Finished — completed/acked/failed/blocked, plus awaiting_user ack
 *    (sky Awaiting Review).
 * 4. Running — in-grace parked decisions (controller still owns triage).
 * 5. Other — anything else (active-but-not-running).
 */
export function categorizeMission(
  status: MissionStatus,
  isActuallyRunning: boolean,
  isWaitingForTool = false,
  needsOperator = false,
  awaitingKind?: AwaitingKind | null
): MissionCategory {
  // A live turn beats a stale awaiting_user/needs_operator flag (resume).
  // waiting_for_tool is the exception: the harness is alive but parked on
  // the user, so a qualified page still belongs in Needs You.
  if (needsAttentionStatus(status, needsOperator) && (!isActuallyRunning || isWaitingForTool)) {
    return 'needs-you';
  }

  if (isActuallyRunning || isWaitingForTool) {
    return 'running';
  }

  // The agent's turn ended but background shell jobs it spawned are still
  // live. Work is genuinely in progress, so surface it in Running rather than
  // letting it fall through to Other (where it would look idle/dropped).
  if (status === 'waiting_background') {
    return 'running';
  }

  if (isFinishedStatus(status) || (status === 'awaiting_user' && awaitingKind !== 'decision')) {
    return 'finished';
  }

  // Controller still owns triage — keep the card on the board in Running
  // rather than dropping it into the unrendered `other` bucket.
  if (status === 'awaiting_user' && awaitingKind === 'decision') {
    return 'running';
  }

  return 'other';
}

/**
 * Categorize multiple missions into columns for display.
 * Returns missions grouped by category with each mission only in one category.
 *
 * `waitingForToolMissionIds` is the subset of running missions parked on a
 * frontend tool (run state `waiting_for_tool`). They stay in Running unless
 * the API marked them `needs_operator`.
 */
export function categorizeMissions<
  T extends {
    id: string;
    status: MissionStatus;
    needs_operator?: boolean;
    awaiting_kind?: AwaitingKind | null;
  },
>(
  missions: T[],
  runningMissionIds: Set<string>,
  waitingForToolMissionIds: Set<string> = new Set()
): Record<MissionCategory, T[]> {
  const result: Record<MissionCategory, T[]> = {
    running: [],
    'needs-you': [],
    finished: [],
    other: [],
  };

  for (const mission of missions) {
    const isWaitingForTool = waitingForToolMissionIds.has(mission.id);
    const isActuallyRunning = runningMissionIds.has(mission.id);
    const category = categorizeMission(
      mission.status,
      isActuallyRunning,
      isWaitingForTool,
      mission.needs_operator === true,
      mission.awaiting_kind
    );
    result[category].push(mission);
  }

  return result;
}

/**
 * Status display utilities
 */
export const STATUS_DOT_COLORS: Record<MissionStatus, string> = {
  pending: 'bg-amber-400',
  active: 'bg-indigo-400',
  awaiting_user: 'bg-amber-400',
  acknowledged: 'bg-emerald-400',
  completed: 'bg-emerald-400',
  failed: 'bg-red-400',
  interrupted: 'bg-red-400',
  blocked: 'bg-red-400',
  not_feasible: 'bg-red-400',
  waiting_background: 'bg-indigo-400',
};

export const STATUS_TEXT_COLORS: Record<MissionStatus, string> = {
  pending: 'text-amber-400',
  active: 'text-indigo-400',
  awaiting_user: 'text-amber-400',
  acknowledged: 'text-emerald-400',
  completed: 'text-emerald-400',
  failed: 'text-red-400',
  interrupted: 'text-red-400',
  blocked: 'text-red-400',
  not_feasible: 'text-red-400',
  waiting_background: 'text-indigo-400',
};

export const STATUS_LABELS: Record<MissionStatus, string> = {
  pending: 'Pending',
  active: 'Active',
  awaiting_user: 'Awaiting Review',
  acknowledged: 'Acknowledged',
  completed: 'Completed',
  failed: 'Failed',
  interrupted: 'Interrupted',
  blocked: 'Blocked',
  not_feasible: 'Not Feasible',
  waiting_background: 'Working (Background)',
};

/**
 * Labels for the two flavors of `awaiting_user`. `decision` means the agent
 * asked a real question; `ack` means it finished and is waiting to be
 * acknowledged/merged. The old single "Needs You" / "Waiting for your input"
 * conflated these, which was misleading.
 */
export const AWAITING_KIND_LABELS: Record<AwaitingKind, string> = {
  decision: 'Needs Decision',
  ack: 'Awaiting Review',
};

/**
 * Display label for a mission status, refined by `awaiting_kind` when the
 * mission is parked in `awaiting_user`. Missing kind is Awaiting Review
 * unless the API marked the mission `needs_operator`.
 */
export function statusLabel(
  status: MissionStatus,
  awaitingKind?: AwaitingKind | null,
  needsOperator = false,
): string {
  if (status === 'awaiting_user') {
    if (awaitingKind) {
      return AWAITING_KIND_LABELS[awaitingKind];
    }
    return needsOperator ? 'Needs You' : AWAITING_KIND_LABELS.ack;
  }
  return STATUS_LABELS[status] ?? status;
}

/**
 * Get the display dot color for a mission, considering runtime state.
 * Running missions always show indigo regardless of stored status.
 */
export function getMissionDotColor(
  status: MissionStatus,
  isActuallyRunning: boolean,
  awaitingKind?: AwaitingKind | null,
): string {
  if (isActuallyRunning) {
    return 'bg-indigo-400';
  }
  if (status === 'awaiting_user' && awaitingKind === 'ack') {
    return 'bg-sky-400';
  }
  return STATUS_DOT_COLORS[status] || 'bg-gray-400';
}

/**
 * Get the display text color for a mission, considering runtime state.
 */
export function getMissionTextColor(
  status: MissionStatus,
  isActuallyRunning: boolean,
  awaitingKind?: AwaitingKind | null,
): string {
  if (isActuallyRunning) {
    return 'text-indigo-400';
  }
  if (status === 'awaiting_user' && awaitingKind === 'ack') {
    return 'text-sky-400';
  }
  return STATUS_TEXT_COLORS[status] || 'text-white/40';
}

// NOTE: Lucide-component status icons live in
// `@/components/ui/status-icons` (STATUS_ICONS + getStatusIcon). The previous
// string-typed STATUS_ICONS map here was unused by any consumer (kept
// duplicated icon names out of sync with the lucide map) — deleted.

/**
 * Get mission title from mission data.
 * Prioritizes explicit title, falls back to truncated first user message.
 */
export function getMissionTitle(
  mission: { title?: string | null; history?: Array<{ role: string; content?: string | null }> | null },
  options?: { maxLength?: number; fallback?: string }
): string {
  const { maxLength = 50, fallback = 'Untitled Mission' } = options || {};
  
  if (mission.title) return mission.title;
  
  const firstUserMessage = mission.history?.find(h => h.role === 'user');
  if (firstUserMessage?.content) {
    const content = firstUserMessage.content.trim();
    return content.length > maxLength ? content.slice(0, maxLength) + '...' : content;
  }
  
  return fallback;
}
