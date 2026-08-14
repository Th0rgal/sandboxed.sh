/**
 * Item-first projection for the projects board card and detail pane.
 *
 * The overview row still carries a recency dump of mission chips. Operators
 * should see durable work items, the controller's last honest signal, and
 * pending owner decisions — not that dump.
 */

import type {
  ProjectDetailPayload,
  ProjectItem,
  ProjectItemAttempt,
  ProjectRow,
  TrackHealth,
} from "@/lib/api/projects";
import { healthDigest, isStale, parseMode } from "./project-health";

const LIVE_ATTEMPT = new Set([
  "created",
  "queued",
  "active",
  "pending",
  "waiting_background",
  "awaiting_user",
  "paused",
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
  stale: boolean;
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
  return {
    headline,
    nextAction,
    blocker: blocker ?? null,
    pendingDecisions: project.pending_decisions ?? 0,
    openTracks: tracks.slice(0, 3).map(toOpenTrack),
    openTrackCount: tracks.length,
    liveAttempts: project.health?.active ?? 0,
    lastSignalAt,
    stale: isStale(project),
  };
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
};

export type ViewSignal = {
  mode: string | null;
  nextAction: string | null;
  blocker: string | null;
  updatedAt: string | null;
  pendingDecisions: number;
};

/** Items still open on the shipped get_project payload. Historical
 *  (acknowledged / completed / superseded) attempts never appear here. */
export function viewOpenItems(payload: ProjectDetailPayload): ViewItem[] {
  return (payload.items ?? [])
    .filter((item) => item.open)
    .map(toViewItem);
}

export function viewControllerSignal(payload: ProjectDetailPayload): ViewSignal {
  const record = payload.project ?? {};
  return {
    mode: record.mode ?? null,
    nextAction: nonblank(record.next_action),
    blocker: nonblank(record.blocker),
    updatedAt: record.updated_at ?? null,
    pendingDecisions: (payload.open_decisions ?? []).length,
  };
}

function toViewItem(item: ProjectItem): ViewItem {
  return {
    key: item.key,
    open: item.open,
    desiredState: item.desired_state ?? item.status ?? null,
    attempts: (item.attempts ?? []).map(toViewAttempt),
  };
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
