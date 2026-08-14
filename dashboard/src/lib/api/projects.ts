/**
 * Projects board API — read-only overview joining Hermes project trackers,
 * project-tagged missions, and cron delivery updates.
 */

import { apiDel, apiGet, apiPost, apiPut } from "./core";

export interface ProjectTracker {
  slug: string;
  status_line: string | null;
  updated_at: string | null;
}

export interface ProjectMissionChip {
  id: string;
  status: string;
  title: string | null;
  updated_at: string;
  github_pr: string | null;
}

export interface ProjectDeliveryUpdate {
  headline: string;
  body: string | null;
  session_id: string;
  at: string;
  signature: string | null;
  /** Descriptor fields after the routing key in the `[STATE_SIGNATURE: …]`
   *  trailer. The board detects a stall by seeing this repeat unchanged. */
  state?: string | null;
  /** Controller-reported mode from the `[CTRL: … mode=… ]` trailer:
   *  `active`, `blocked[:cause]` or `paused[:reason]`. Absent for controllers
   *  that have not adopted the trailer — render nothing, never "unknown". */
  mode?: string | null;
  blocker: string | null;
}

export type TrackVerdict = "failing" | "overdue" | "active" | "done" | "idle";

export interface TrackHealth {
  track: string | null;
  verdict: TrackVerdict;
  missions: number;
  active: number;
  failed: number;
  completed: number;
  overdue: number;
  desired_states: Record<string, number>;
  last_activity_at: string | null;
}

export interface ProjectHealth {
  missions: number;
  active: number;
  failed: number;
  overdue: number;
  tracks_needing_attention: number;
  /** Worst-first, per the backend rollup. */
  tracks: TrackHealth[];
}

export type ProjectBucket = "attention" | "active" | "paused" | "archived";

export interface ProjectConversation {
  session_id: string;
  /** `binding` = declared by an operator; `latest_update` = guessed from the
   *  newest delivery, which for a cron-driven project is a throwaway session
   *  that has already ended. Render the two differently. */
  source: "binding" | "latest_update";
  bound_at?: string;
}

export interface ProjectRow {
  slug: string;
  /** Roster title, when set — shown instead of the raw slug. */
  title?: string | null;
  /** The controller's declared next step, from the roster record. */
  next_action?: string | null;
  bucket: ProjectBucket;
  tracker: ProjectTracker | null;
  missions: ProjectMissionChip[];
  latest_update: ProjectDeliveryUpdate | null;
  updates_count: number;
  attention_reasons: string[];
  health: ProjectHealth;
  conversation?: ProjectConversation | null;
  /** Roster mode (`active` / `blocked[:cause]` / `paused`). Prefer this over
   *  guessing from `latest_update` when the last delivery was `[SILENT]`. */
  mode?: string | null;
  /** Last successful controller cron run, even if the tick was `[SILENT]`. */
  controller_heartbeat_at?: string | null;
  controller_health?: "healthy" | "stale" | "missing" | null;
  delivery_health?: "reaching_user" | "misrouted" | "dropped" | null;
  progress_state?: "working" | "waiting_external" | "blocked" | null;
  pending_decisions?: number;
  autonomy_level?: string | null;
}

export interface ProjectsOverview {
  projects: ProjectRow[];
  archived: string[];
  unrouted_updates: ProjectDeliveryUpdate[];
  sources: { trackers: boolean; hermes_db: boolean };
}

export interface ProjectUpdatesResponse {
  slug: string;
  updates: ProjectDeliveryUpdate[];
}

export interface ProjectItemAttempt {
  id: string;
  status: string;
  title?: string | null;
  updated_at: string;
  role?: string | null;
}

export interface ProjectItem {
  key: string;
  kind: string;
  desired_state?: string | null;
  status?: string | null;
  open: boolean;
  attempts: ProjectItemAttempt[];
}

export interface ProjectDecision {
  question?: string;
  kind?: string;
  status?: string;
  created_at?: string;
  at?: string;
  rationale?: string | null;
}

export interface ProjectRecord {
  slug?: string;
  title?: string | null;
  status?: string | null;
  mode?: string | null;
  next_action?: string | null;
  blocker?: string | null;
  updated_at?: string | null;
}

export interface ProjectDetailPayload {
  project?: ProjectRecord;
  items?: ProjectItem[];
  open_decisions?: ProjectDecision[];
  conversation?: ProjectConversation | null;
  tracks?: unknown[];
}

export function getProjectsOverview(): Promise<ProjectsOverview> {
  return apiGet("/api/projects/overview", "Failed to load projects overview");
}

export function getProject(slug: string): Promise<ProjectDetailPayload> {
  return apiGet(
    `/api/projects/${encodeURIComponent(slug)}`,
    "Failed to load project",
  );
}

export function getProjectBySession(
  sessionId: string,
): Promise<{ slug: string; session_id: string }> {
  return apiGet(
    `/api/projects/by-session/${encodeURIComponent(sessionId)}`,
    "Failed to resolve project for session",
  );
}

export function getProjectUpdates(
  slug: string,
  limit = 50,
): Promise<ProjectUpdatesResponse> {
  return apiGet(
    `/api/projects/${encodeURIComponent(slug)}/updates?limit=${limit}`,
    "Failed to load project updates",
  );
}

export type ProjectAction =
  | "pause"
  | "resume"
  | "archive"
  | "unarchive"
  | "delete"
  | "restore";

export async function postProjectAction(
  slug: string,
  action: ProjectAction,
): Promise<void> {
  await apiPost(
    `/api/projects/${encodeURIComponent(slug)}/action`,
    { action },
    "Failed to apply project action",
  );
}

export async function bindProjectConversation(
  slug: string,
  sessionId: string,
): Promise<void> {
  await apiPut(
    `/api/projects/${encodeURIComponent(slug)}/conversation`,
    { session_id: sessionId },
    "Failed to bind the project conversation",
  );
}

export async function unbindProjectConversation(slug: string): Promise<void> {
  await apiDel(
    `/api/projects/${encodeURIComponent(slug)}/conversation`,
    "Failed to unbind the project conversation",
  );
}
