/**
 * Mission status utilities - shared logic for categorizing missions
 * based on runtime state and stored status.
 */

import type { MissionStatus } from './api/missions';

export type MissionCategory = 'running' | 'needs-you' | 'finished' | 'other';
export type FinishedTone = 'green' | 'red';

// "Needs You" is reserved for missions where the agent finished its turn
// cleanly (`awaiting_user`) and is waiting on the user. Failure paths
// (interrupted, blocked, not_feasible, failed) live in Finished with a red
// tone instead.
export const NEEDS_ATTENTION_STATUSES: MissionStatus[] = ['awaiting_user'];
export const FINISHED_STATUSES: MissionStatus[] = [
  'completed',
  'acknowledged',
  'failed',
  'interrupted',
  'blocked',
  'not_feasible',
];

const FINISHED_GREEN_STATUSES: MissionStatus[] = ['completed', 'acknowledged'];

/**
 * Check if a mission is in a finished state based on its stored status.
 */
export function isFinishedStatus(status: MissionStatus): boolean {
  return FINISHED_STATUSES.includes(status);
}

/**
 * Check if a mission needs user attention based on its stored status.
 */
export function needsAttentionStatus(status: MissionStatus): boolean {
  return NEEDS_ATTENTION_STATUSES.includes(status);
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
 * 1. Needs You (waiting for tool) - the agent is parked on a frontend tool
 *    (e.g. AskUserQuestion). The backend still reports this mission as
 *    "running" (run state `waiting_for_tool`) because its harness is alive,
 *    but it is blocked on the user, so it belongs in Needs You — not Running.
 * 2. Running - mission is actually executing a turn
 * 3. Needs You - awaiting_user AND not running
 * 4. Finished - completed/acknowledged/failed/interrupted/blocked/not_feasible AND not running
 * 5. Other - anything else (active-but-not-running, pending)
 */
export function categorizeMission(
  status: MissionStatus,
  isActuallyRunning: boolean,
  isWaitingForTool = false
): MissionCategory {
  if (isWaitingForTool) {
    return 'needs-you';
  }

  if (isActuallyRunning) {
    return 'running';
  }

  if (needsAttentionStatus(status)) {
    return 'needs-you';
  }

  if (isFinishedStatus(status)) {
    return 'finished';
  }

  return 'other';
}

/**
 * Categorize multiple missions into columns for display.
 * Returns missions grouped by category with each mission only in one category.
 *
 * `waitingForToolMissionIds` is the subset of running missions parked on a
 * frontend tool (run state `waiting_for_tool`); they are surfaced as Needs You
 * rather than Running even though the backend still reports them as running.
 */
export function categorizeMissions<T extends { id: string; status: MissionStatus }>(
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
    const category = categorizeMission(mission.status, isActuallyRunning, isWaitingForTool);
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
};

export const STATUS_LABELS: Record<MissionStatus, string> = {
  pending: 'Pending',
  active: 'Active',
  awaiting_user: 'Needs You',
  acknowledged: 'Acknowledged',
  completed: 'Completed',
  failed: 'Failed',
  interrupted: 'Interrupted',
  blocked: 'Blocked',
  not_feasible: 'Not Feasible',
};

/**
 * Get the display dot color for a mission, considering runtime state.
 * Running missions always show indigo regardless of stored status.
 */
export function getMissionDotColor(status: MissionStatus, isActuallyRunning: boolean): string {
  if (isActuallyRunning) {
    return 'bg-indigo-400';
  }
  return STATUS_DOT_COLORS[status] || 'bg-gray-400';
}

/**
 * Get the display text color for a mission, considering runtime state.
 */
export function getMissionTextColor(status: MissionStatus, isActuallyRunning: boolean): string {
  if (isActuallyRunning) {
    return 'text-indigo-400';
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
