/**
 * Missions API - CRUD and control operations for missions.
 */

import { apiGet, apiPost, apiFetch } from "./core";
import { isAutoTitleEnabled } from "../llm-settings";
import { generateMissionTitle } from "../llm";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type MissionStatus = "active" | "completed" | "failed" | "interrupted" | "blocked" | "not_feasible";

export type ModelEffort = "low" | "medium" | "high" | "xhigh" | "max";

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
  short_description?: string | null;
  metadata_updated_at?: string | null;
  metadata_source?: string | null;
  metadata_model?: string | null;
  metadata_version?: string | null;
  workspace_id?: string;
  workspace_name?: string | null;
  agent?: string | null;
  model_override?: string | null;
  model_effort?: ModelEffort | null;
  backend?: string;
  config_profile?: string | null;
  history: MissionHistoryEntry[];
  desktop_sessions?: DesktopSessionInfo[];
  created_at: string;
  updated_at: string;
  interrupted_at?: string;
  resumable?: boolean;
  session_id?: string | null;
  parent_mission_id?: string;
  working_directory?: string;
  mission_mode?: "task" | "assistant";
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
  modelEffort?: ModelEffort;
  configProfile?: string;
  backend?: string;
}

export interface UpdateMissionSettingsOptions {
  backend?: string;
  agent?: string | null;
  modelOverride?: string | null;
  modelEffort?: ModelEffort | null;
  configProfile?: string | null;
}

export interface RunningMissionInfo {
  mission_id: string;
  state: "queued" | "running" | "waiting_for_tool" | "finished";
  queue_len: number;
  history_len: number;
  seconds_since_activity: number;
  health: MissionHealth;
  expected_deliverables: number;
  current_activity?: string;
  subtask_total: number;
  subtask_completed: number;
}

export type MissionStallSeverity = "warning" | "severe";

export type MissionHealth =
  | { status: "healthy" }
  | {
      status: "stalled";
      seconds_since_activity: number;
      last_state: string;
      severity: MissionStallSeverity;
    }
  | { status: "missing_deliverables"; missing: string[] }
  | { status: "unexpected_end"; reason: string };

export interface MissionSearchResult {
  mission: Mission;
  relevance_score: number;
}

export interface MissionMomentSearchResult {
  mission: Mission;
  entry_index: number;
  role: string;
  snippet: string;
  rationale: string;
  relevance_score: number;
}

// ---------------------------------------------------------------------------
// API Functions
// ---------------------------------------------------------------------------

export async function listMissions(): Promise<Mission[]> {
  return apiGet("/api/control/missions", "Failed to fetch missions");
}

export async function searchMissions(
  query: string,
  options?: { limit?: number }
): Promise<MissionSearchResult[]> {
  const params = new URLSearchParams();
  params.set("q", query);
  if (options?.limit) params.set("limit", String(options.limit));
  return apiGet(
    `/api/control/missions/search?${params.toString()}`,
    "Failed to search missions"
  );
}

export async function searchMissionMoments(
  query: string,
  options?: { limit?: number; missionId?: string }
): Promise<MissionMomentSearchResult[]> {
  const params = new URLSearchParams();
  params.set("q", query);
  if (options?.limit) params.set("limit", String(options.limit));
  if (options?.missionId) params.set("mission_id", options.missionId);
  return apiGet(
    `/api/control/missions/search/moments?${params.toString()}`,
    "Failed to search mission moments"
  );
}

export async function getMission(id: string): Promise<Mission> {
  return apiGet(`/api/control/missions/${id}`, "Failed to fetch mission");
}

export async function getMissionEvents(
  id: string,
  options?: {
    types?: string[];
    limit?: number;
    offset?: number;
    latest?: boolean;
    /** When set, request only events with `sequence > sinceSeq`.
     * Takes precedence over `offset`/`latest` on the server. */
    sinceSeq?: number;
    /** When set, request only events with `sequence < beforeSeq`,
     * returned ASC. Takes precedence over `sinceSeq`/`offset`/`latest`.
     * Used for backwards pagination. */
    beforeSeq?: number;
  }
): Promise<StoredEvent[]> {
  const { events } = await getMissionEventsWithMeta(id, options);
  return events;
}

export interface MissionEventsMeta {
  /** Total events stored for this mission (matching the type filter if
   * one was supplied). Read from `X-Total-Events`. `undefined` if the
   * header was missing or unparseable. */
  totalEvents?: number;
  /** Highest `sequence` value stored for this mission, regardless of
   * filter. Read from `X-Max-Sequence`. Use this as the `sinceSeq` of
   * the next call to resume from exactly where the server is. */
  maxSequence?: number;
}

/**
 * Like `getMissionEvents` but also returns the `X-Total-Events` and
 * `X-Max-Sequence` headers. Prefer this at call sites that reconnect
 * or paginate — it lets the client skip a second count round-trip and
 * resume from the server's latest sequence on the next delta fetch.
 */
export async function getMissionEventsWithMeta(
  id: string,
  options?: {
    types?: string[];
    limit?: number;
    offset?: number;
    latest?: boolean;
    sinceSeq?: number;
    beforeSeq?: number;
  }
): Promise<{ events: StoredEvent[]; meta: MissionEventsMeta }> {
  const params = new URLSearchParams();
  if (options?.types?.length) params.set("types", options.types.join(","));
  if (options?.limit) params.set("limit", String(options.limit));
  if (options?.offset) params.set("offset", String(options.offset));
  if (options?.latest) params.set("latest", "true");
  if (options?.sinceSeq !== undefined) params.set("since_seq", String(options.sinceSeq));
  if (options?.beforeSeq !== undefined) params.set("before_seq", String(options.beforeSeq));
  const query = params.toString();
  const res = await apiFetch(
    `/api/control/missions/${id}/events${query ? `?${query}` : ""}`
  );
  if (!res.ok) throw new Error("Failed to fetch mission events");
  const events = (await res.json()) as StoredEvent[];

  const totalHeader = res.headers.get("x-total-events");
  const maxSeqHeader = res.headers.get("x-max-sequence");
  const totalEvents =
    totalHeader !== null && totalHeader !== "" && !Number.isNaN(Number(totalHeader))
      ? Number(totalHeader)
      : undefined;
  const maxSequence =
    maxSeqHeader !== null && maxSeqHeader !== "" && !Number.isNaN(Number(maxSeqHeader))
      ? Number(maxSeqHeader)
      : undefined;
  return { events, meta: { totalEvents, maxSequence } };
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
    model_effort?: ModelEffort;
    config_profile?: string;
    backend?: string;
  } = {};

  if (options?.title) body.title = options.title;
  if (options?.workspaceId) body.workspace_id = options.workspaceId;
  if (options?.agent) body.agent = options.agent;
  if (options?.modelOverride) body.model_override = options.modelOverride;
  if (options?.modelEffort) body.model_effort = options.modelEffort;
  if (options?.configProfile) body.config_profile = options.configProfile;
  if (options?.backend) body.backend = options.backend;

  const res = await apiFetch("/api/control/missions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: Object.keys(body).length > 0 ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || "Failed to create mission");
  }
  return res.json();
}

export async function loadMission(id: string): Promise<Mission | null> {
  const res = await apiFetch(`/api/control/missions/${id}/load`, { method: "POST" });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error("Failed to load mission");
  return res.json();
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

export async function updateMissionSettings(
  id: string,
  options: UpdateMissionSettingsOptions
): Promise<Mission> {
  const body: {
    backend?: string;
    agent?: string | null;
    model_override?: string | null;
    model_effort?: ModelEffort | null;
    config_profile?: string | null;
  } = {};
  if ("backend" in options) body.backend = options.backend;
  if ("agent" in options) body.agent = options.agent;
  if ("modelOverride" in options) body.model_override = options.modelOverride;
  if ("modelEffort" in options) body.model_effort = options.modelEffort;
  if ("configProfile" in options) body.config_profile = options.configProfile;

  const res = await apiFetch(`/api/control/missions/${id}/settings`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to update mission settings: ${text}`);
  }
  return res.json();
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

// ---------------------------------------------------------------------------
// Title management
// ---------------------------------------------------------------------------

/** Rename a mission via the backend API. */
export async function updateMissionTitle(
  id: string,
  title: string
): Promise<void> {
  return apiPost(
    `/api/control/missions/${id}/title`,
    { title },
    "Failed to update mission title"
  );
}

/**
 * Auto-generate a mission title using the configured LLM provider.
 * Fires-and-forgets: errors are silently ignored so it never disrupts the UI.
 * Returns the generated title if successful, null otherwise.
 */
export async function autoGenerateMissionTitle(
  missionId: string,
  userMessage: string,
  assistantReply: string
): Promise<string | null> {
  if (!isAutoTitleEnabled()) return null;
  try {
    const title = await generateMissionTitle(userMessage, assistantReply);
    if (title) {
      await updateMissionTitle(missionId, title);
      return title;
    }
  } catch (err) {
    console.warn("[AutoTitle] Failed to generate mission title:", err);
  }
  return null;
}
