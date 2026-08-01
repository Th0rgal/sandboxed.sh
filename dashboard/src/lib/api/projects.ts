/**
 * Projects board API — read-only overview joining Hermes project trackers,
 * project-tagged missions, and cron delivery updates.
 */

import { apiGet, apiPost } from "./core";

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
  blocker: string | null;
}

export type ProjectBucket = "attention" | "active" | "paused" | "archived";

export interface ProjectRow {
  slug: string;
  bucket: ProjectBucket;
  tracker: ProjectTracker | null;
  missions: ProjectMissionChip[];
  latest_update: ProjectDeliveryUpdate | null;
  updates_count: number;
  attention_reasons: string[];
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

export function getProjectsOverview(): Promise<ProjectsOverview> {
  return apiGet("/api/projects/overview", "Failed to load projects overview");
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
