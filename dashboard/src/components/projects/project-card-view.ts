/**
 * Item-first projection for the projects board card and detail pane.
 *
 * The overview row still carries a recency dump of mission chips. Operators
 * should see durable work items, the controller's last honest signal, and
 * pending owner decisions — not that dump.
 */

import type {
  ProjectDecision,
  ProjectDetailPayload,
  ProjectItem,
  ProjectItemAttempt,
  ProjectRow,
  TrackHealth,
} from "@/lib/api/projects";
import { healthDigest, isStale, parseMode } from "./project-health";

/** Actually executing. Parked `paused` / `awaiting_user` rows are open
 *  items, not movement — Verity was ranking five paused bosses above
 *  the two writers that were running. */
const LIVE_ATTEMPT = new Set([
  "created",
  "queued",
  "active",
  "pending",
  "waiting_background",
]);

export type CardOpenTrack = {
  key: string;
  verdict: string;
  live: number;
  failed: number;
};

export type CardSummary = {
  headline: string | null;
  nextAction: string | null;
  blocker: string | null;
  pendingDecisions: number;
  openTracks: CardOpenTrack[];
  openTrackCount: number;
  liveAttempts: number;
  lastSignalAt: string | null;
  /** Newest live mission / active-track activity. Distinct from lastSignalAt
   *  so a silent controller cannot hide writers that are still ticking. */
  lastWorkAt: string | null;
  stale: boolean;
  /** next_action is set, nothing is running, and the owner is not being asked. */
  idleNextAction: boolean;
  /** Live work is more than 15 minutes newer than the last controller signal. */
  controllerBehind: boolean;
};

/** Overview-row facts the card should lead with. */
export function cardSummary(project: ProjectRow): CardSummary {
  const tracks = (project.health?.tracks ?? []).filter(
    (track) => track.verdict !== "done",
  );
  const nextAction = nonblank(project.next_action);
  const blocker =
    nonblank(project.latest_update?.blocker) ??
    (parseMode(project)?.base === "blocked"
      ? parseMode(project)?.cause
      : null);
  const headline =
    nextAction ??
    healthDigest(project.health) ??
    project.attention_reasons[0] ??
    nonblank(project.latest_update?.headline) ??
    nonblank(project.tracker?.status_line);
  const lastSignalAt =
    project.latest_update?.at ?? project.controller_heartbeat_at ?? null;
  const lastWorkAt = latestLiveWorkAt(project);
  const liveAttempts = project.health?.active ?? 0;
  const pendingDecisions = project.pending_decisions ?? 0;
  const mode = parseMode(project);
  return {
    headline,
    nextAction,
    blocker: blocker ?? null,
    pendingDecisions,
    openTracks: tracks.slice(0, 3).map(toOpenTrack),
    openTrackCount: tracks.length,
    liveAttempts,
    lastSignalAt,
    lastWorkAt,
    stale: isStale(project),
    idleNextAction:
      nextAction !== null &&
      liveAttempts === 0 &&
      pendingDecisions === 0 &&
      mode?.base !== "paused",
    controllerBehind: isControllerBehind(lastSignalAt, lastWorkAt, liveAttempts),
  };
}

const CONTROLLER_BEHIND_MS = 15 * 60 * 1000;

function latestLiveWorkAt(project: ProjectRow): string | null {
  const fromMissions = (project.missions ?? [])
    .filter((mission) => LIVE_ATTEMPT.has(mission.status))
    .map((mission) => mission.updated_at);
  const fromTracks = (project.health?.tracks ?? [])
    .filter((track) => track.active > 0)
    .map((track) => track.last_activity_at);
  return latestTimestamp([...fromMissions, ...fromTracks]);
}

function isControllerBehind(
  lastSignalAt: string | null,
  lastWorkAt: string | null,
  liveAttempts: number,
): boolean {
  if (liveAttempts < 1 || !lastSignalAt || !lastWorkAt) return false;
  const signalMs = Date.parse(lastSignalAt);
  const workMs = Date.parse(lastWorkAt);
  if (!Number.isFinite(signalMs) || !Number.isFinite(workMs)) return false;
  return workMs - signalMs > CONTROLLER_BEHIND_MS;
}

function latestTimestamp(values: Array<string | null | undefined>): string | null {
  let best: string | null = null;
  let bestMs = Number.NEGATIVE_INFINITY;
  for (const value of values) {
    if (!value) continue;
    const ms = Date.parse(value);
    if (!Number.isFinite(ms) || ms <= bestMs) continue;
    bestMs = ms;
    best = value;
  }
  return best;
}

function toOpenTrack(track: TrackHealth): CardOpenTrack {
  return {
    key: track.track ?? "untracked",
    verdict: track.verdict,
    live: track.active,
    failed: track.failed,
  };
}

export type ViewAttempt = {
  id: string;
  status: string;
  title: string | null;
  updated_at: string;
  live: boolean;
};

export type ViewItem = {
  key: string;
  open: boolean;
  desiredState: string | null;
  attempts: ViewAttempt[];
  moving: boolean;
};

export type ViewDecision = {
  question: string;
  at: string | null;
  count: number;
  status: string | null;
};

export type ViewSignal = {
  mode: string | null;
  nextAction: string | null;
  blocker: string | null;
  updatedAt: string | null;
  pendingDecisions: number;
};

/** Items still open on the shipped get_project payload. Historical
 *  (acknowledged / completed / superseded) attempts never appear here.
 *  Live attempts sort first so a graveyard of failed retries cannot bury
 *  the one item that is actually moving. */
export function viewOpenItems(payload: ProjectDetailPayload): ViewItem[] {
  return (payload.items ?? [])
    .filter((item) => item.open)
    .map(toViewItem)
    .sort((left, right) => Number(right.moving) - Number(left.moving));
}

export function viewMovingItems(items: ViewItem[]): ViewItem[] {
  return items.filter((item) => item.moving);
}

export function viewStalledItems(items: ViewItem[]): ViewItem[] {
  return items.filter((item) => !item.moving);
}

/** Collapse duplicate owner questions (Coldcard recorded the same
 *  checkpoint prompt twice two seconds apart). */
export function viewPendingDecisions(
  payload: ProjectDetailPayload,
): ViewDecision[] {
  const grouped = new Map<string, ViewDecision>();
  for (const raw of payload.open_decisions ?? []) {
    if (!isPendingDecision(raw)) continue;
    const question = nonblank(raw.question);
    if (!question) continue;
    const key = question.toLowerCase();
    const existing = grouped.get(key);
    const at = raw.at ?? raw.created_at ?? null;
    if (!existing) {
      grouped.set(key, {
        question,
        at,
        count: 1,
        status: raw.status ?? null,
      });
      continue;
    }
    existing.count += 1;
    if (at && (!existing.at || at < existing.at)) existing.at = at;
  }
  return [...grouped.values()];
}

export function viewControllerSignal(payload: ProjectDetailPayload): ViewSignal {
  const record = payload.project ?? {};
  const pending = viewPendingDecisions(payload);
  return {
    mode: record.mode ?? null,
    nextAction: nonblank(record.next_action),
    blocker: nonblank(record.blocker),
    updatedAt: record.updated_at ?? null,
    pendingDecisions: pending.reduce((sum, decision) => sum + decision.count, 0),
  };
}

function toViewItem(item: ProjectItem): ViewItem {
  const attempts = (item.attempts ?? []).map(toViewAttempt);
  return {
    key: item.key,
    open: item.open,
    desiredState: item.desired_state ?? item.status ?? null,
    attempts,
    moving: attempts.some((attempt) => attempt.live),
  };
}

function isPendingDecision(decision: ProjectDecision): boolean {
  const status = decision.status?.trim();
  return !status || status === "pending_user";
}

function toViewAttempt(attempt: ProjectItemAttempt): ViewAttempt {
  return {
    id: attempt.id,
    status: attempt.status,
    title: attempt.title ?? null,
    updated_at: attempt.updated_at,
    live: LIVE_ATTEMPT.has(attempt.status),
  };
}

function nonblank(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}
