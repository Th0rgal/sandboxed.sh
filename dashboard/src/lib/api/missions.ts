/**
 * Missions API - CRUD and control operations for missions.
 */

import { apiGet, apiPost, apiFetch } from "./core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type MissionStatus = "active" | "completed" | "failed" | "interrupted" | "blocked" | "not_feasible";

export interface MissionHistoryEntry {
  role: string;
  content: string;
}

export interface DesktopSessionInfo {
  display: string;
  resolution?: string;
  started_at: string;
  stopped_at?: string;
  screenshots_dir?: string;
  browser?: string;
  url?: string;
}

export interface Mission {
  id: string;
  status: MissionStatus;
  title: string | null;
  workspace_id?: string;
  workspace_name?: string;
  agent?: string;
  backend?: string;
  history: MissionHistoryEntry[];
  desktop_sessions?: DesktopSessionInfo[];
  created_at: string;
  updated_at: string;
  interrupted_at?: string;
  resumable?: boolean;
}

export interface StoredEvent {
  id: number;
  mission_id: string;
  sequence: number;
  event_type: string;
  timestamp: string;
  event_id?: string;
  tool_call_id?: string;
  tool_name?: string;
  content: string;
  metadata: Record<string, unknown>;
}

export interface CreateMissionOptions {
  title?: string;
  workspaceId?: string;
  agent?: string;
  modelOverride?: string;
  backend?: string;
}

export interface RunningMissionInfo {
  mission_id: string;
  state: "queued" | "running" | "waiting_for_tool" | "finished";
  queue_len: number;
  history_len: number;
  seconds_since_activity: number;
  expected_deliverables: number;
}

// ---------------------------------------------------------------------------
// API Functions
// ---------------------------------------------------------------------------

export async function listMissions(): Promise<Mission[]> {
  return apiGet("/api/control/missions", "Failed to fetch missions");
}

export async function getMission(id: string): Promise<Mission> {
  return apiGet(`/api/control/missions/${id}`, "Failed to fetch mission");
}

export async function getMissionEvents(
  id: string,
  options?: { types?: string[]; limit?: number; offset?: number }
): Promise<StoredEvent[]> {
  const params = new URLSearchParams();
  if (options?.types?.length) params.set("types", options.types.join(","));
  if (options?.limit) params.set("limit", String(options.limit));
  if (options?.offset) params.set("offset", String(options.offset));
  const query = params.toString();
  return apiGet(`/api/control/missions/${id}/events${query ? `?${query}` : ""}`, "Failed to fetch mission events");
}

export async function getCurrentMission(): Promise<Mission | null> {
  return apiGet("/api/control/missions/current", "Failed to fetch current mission");
}

export async function createMission(
  options?: CreateMissionOptions
): Promise<Mission> {
  const body: {
    title?: string;
    workspace_id?: string;
    agent?: string;
    model_override?: string;
    backend?: string;
  } = {};

  if (options?.title) body.title = options.title;
  if (options?.workspaceId) body.workspace_id = options.workspaceId;
  if (options?.agent) body.agent = options.agent;
  if (options?.modelOverride) body.model_override = options.modelOverride;
  if (options?.backend) body.backend = options.backend;

  const res = await apiFetch("/api/control/missions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: Object.keys(body).length > 0 ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error("Failed to create mission");
  return res.json();
}

export async function loadMission(id: string): Promise<Mission> {
  return apiPost(`/api/control/missions/${id}/load`, undefined, "Failed to load mission");
}

export async function getRunningMissions(): Promise<RunningMissionInfo[]> {
  return apiGet("/api/control/running", "Failed to fetch running missions");
}

export async function startMissionParallel(
  missionId: string,
  content: string
): Promise<{ ok: boolean; mission_id: string }> {
  const res = await apiFetch(`/api/control/missions/${missionId}/parallel`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to start parallel mission: ${text}`);
  }
  return res.json();
}

export async function cancelMission(missionId: string): Promise<void> {
  return apiPost(`/api/control/missions/${missionId}/cancel`, undefined, "Failed to cancel mission");
}

export async function setMissionStatus(
  id: string,
  status: MissionStatus
): Promise<void> {
  return apiPost(`/api/control/missions/${id}/status`, { status }, "Failed to set mission status");
}

export async function deleteMission(id: string): Promise<{ ok: boolean; deleted: string }> {
  const res = await apiFetch(`/api/control/missions/${id}`, {
    method: "DELETE",
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to delete mission: ${text}`);
  }
  return res.json();
}

export async function cleanupEmptyMissions(): Promise<{ ok: boolean; deleted_count: number }> {
  const res = await apiFetch("/api/control/missions/cleanup", {
    method: "POST",
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to cleanup missions: ${text}`);
  }
  return res.json();
}

export async function resumeMission(
  id: string,
  options?: { skipMessage?: boolean }
): Promise<Mission> {
  const res = await apiFetch(`/api/control/missions/${id}/resume`, {
    method: "POST",
    headers: options ? { "Content-Type": "application/json" } : undefined,
    body: options ? JSON.stringify({ skip_message: options.skipMessage }) : undefined,
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to resume mission: ${text}`);
  }
  return res.json();
}
