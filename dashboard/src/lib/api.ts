import { authHeader, clearJwt, signalAuthRequired } from "./auth";
import { getRuntimeApiBase } from "./settings";

function apiUrl(pathOrUrl: string): string {
  if (/^https?:\/\//i.test(pathOrUrl)) return pathOrUrl;
  const base = getRuntimeApiBase();
  const path = pathOrUrl.startsWith("/") ? pathOrUrl : `/${pathOrUrl}`;
  return `${base}${path}`;
}

export interface TaskState {
  id: string;
  status: "pending" | "running" | "completed" | "failed" | "cancelled";
  task: string;
  model: string;
  iterations: number;
  result: string | null;
  log: TaskLogEntry[];
}

export interface TaskLogEntry {
  timestamp: string;
  entry_type: "thinking" | "tool_call" | "tool_result" | "response" | "error";
  content: string;
}

export interface StatsResponse {
  total_tasks: number;
  active_tasks: number;
  completed_tasks: number;
  failed_tasks: number;
  total_cost_cents: number;
  success_rate: number;
}

export interface HealthResponse {
  status: string;
  version: string;
  dev_mode: boolean;
  auth_required: boolean;
  auth_mode: "disabled" | "single_tenant" | "multi_user";
  max_iterations: number;
  /** Configured library remote URL from server (LIBRARY_REMOTE env var) */
  library_remote?: string;
}

export interface LoginResponse {
  token: string;
  exp: number;
}

export function isNetworkError(error: unknown): boolean {
  if (!error) return false;
  if (error instanceof Error) {
    const message = error.message.toLowerCase();
    return (
      message.includes("failed to fetch") ||
      message.includes("networkerror") ||
      message.includes("load failed") ||
      message.includes("network request failed") ||
      message.includes("offline")
    );
  }
  return false;
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const headers: Record<string, string> = {
    ...(init?.headers ? (init.headers as Record<string, string>) : {}),
    ...authHeader(),
  };

  const res = await fetch(apiUrl(path), { ...init, headers });
  if (res.status === 401) {
    clearJwt();
    signalAuthRequired();
  }
  return res;
}

// ---------------------------------------------------------------------------
// Internal request helpers – reduce repeated Content-Type / throw / .json()
// ---------------------------------------------------------------------------

async function apiGet<T>(path: string, errorMsg: string): Promise<T> {
  const res = await apiFetch(path);
  if (!res.ok) throw new Error(errorMsg);
  return res.json();
}

async function apiPost<T = void>(
  path: string,
  body?: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const init: RequestInit = { method: "POST" };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await apiFetch(path, init);
  if (!res.ok) throw new Error(errorMsg);
  // For void returns the caller simply ignores the result
  return res.json().catch(() => undefined as unknown as T);
}

async function apiPut<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

async function apiPatch<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

async function apiDel<T = void>(path: string, errorMsg = "Request failed"): Promise<T> {
  const res = await apiFetch(path, { method: "DELETE" });
  if (!res.ok) throw new Error(errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

// Library-specific variants that use ensureLibraryResponse (handles 503 → LibraryUnavailableError)
async function libGet<T>(path: string, errorMsg: string): Promise<T> {
  const res = await apiFetch(path);
  await ensureLibraryResponse(res, errorMsg);
  return res.json();
}

async function libPost<T = void>(
  path: string,
  body?: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const init: RequestInit = { method: "POST" };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await apiFetch(path, init);
  await ensureLibraryResponse(res, errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

async function libPut<T = void>(
  path: string,
  body: unknown,
  errorMsg = "Request failed",
): Promise<T> {
  const res = await apiFetch(path, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  await ensureLibraryResponse(res, errorMsg);
  return res.json().catch(() => undefined as unknown as T);
}

async function libDel(path: string, errorMsg = "Request failed"): Promise<void> {
  const res = await apiFetch(path, { method: "DELETE" });
  await ensureLibraryResponse(res, errorMsg);
}

export class LibraryUnavailableError extends Error {
  status: number;

  constructor(message: string) {
    super(message);
    this.name = "LibraryUnavailableError";
    this.status = 503;
  }
}

async function ensureLibraryResponse(
  res: Response,
  fallbackMessage: string
): Promise<Response> {
  if (res.ok) return res;
  const text = await res.text().catch(() => "");
  if (res.status === 503) {
    throw new LibraryUnavailableError(text || "Library not initialized");
  }
  throw new Error(text || fallbackMessage);
}

export interface CreateTaskRequest {
  task: string;
  model?: string;
  workspace_path?: string;
  budget_cents?: number;
}

export interface Run {
  id: string;
  created_at: string;
  status: string;
  input_text: string;
  final_output: string | null;
  total_cost_cents: number;
  summary_text: string | null;
}

// Health check
export async function getHealth(): Promise<HealthResponse> {
  const res = await fetch(apiUrl("/api/health"));
  if (!res.ok) throw new Error("Failed to fetch health");
  return res.json();
}

export async function login(password: string, username?: string): Promise<LoginResponse> {
  const payload: { password: string; username?: string } = { password };
  if (username && username.trim().length > 0) {
    payload.username = username.trim();
  }
  const res = await fetch(apiUrl("/api/auth/login"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || "Failed to login");
  }
  return res.json();
}

// Get statistics
export async function getStats(): Promise<StatsResponse> {
  return apiGet("/api/stats", "Failed to fetch stats");
}

// List all tasks
export async function listTasks(): Promise<TaskState[]> {
  return apiGet("/api/tasks", "Failed to fetch tasks");
}

// List OpenCode agents
export async function listOpenCodeAgents(): Promise<unknown> {
  return apiGet("/api/opencode/agents", "Failed to fetch OpenCode agents");
}

// Get a specific task
export async function getTask(id: string): Promise<TaskState> {
  return apiGet(`/api/task/${id}`, "Failed to fetch task");
}

// Create a new task
export async function createTask(
  request: CreateTaskRequest
): Promise<{ id: string; status: string }> {
  return apiPost("/api/task", request, "Failed to create task");
}

// Stop a task
export async function stopTask(id: string): Promise<void> {
  return apiPost(`/api/task/${id}/stop`, undefined, "Failed to stop task");
}

// Stream task progress (SSE)
export function streamTask(
  id: string,
  onEvent: (event: { type: string; data: unknown }) => void
): () => void {
  const controller = new AbortController();
  const decoder = new TextDecoder();
  let buffer = "";
  let sawDone = false;

  void (async () => {
    try {
      const res = await apiFetch(`/api/task/${id}/stream`, {
        method: "GET",
        headers: { Accept: "text/event-stream" },
        signal: controller.signal,
      });

      if (!res.ok) {
        onEvent({
          type: "error",
          data: {
            message: `Stream request failed (${res.status})`,
            status: res.status,
          },
        });
        return;
      }
      if (!res.body) {
        onEvent({
          type: "error",
          data: { message: "Stream response had no body" },
        });
        return;
      }

      const reader = res.body.getReader();
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        let idx = buffer.indexOf("\n\n");
        while (idx !== -1) {
          const raw = buffer.slice(0, idx);
          buffer = buffer.slice(idx + 2);
          idx = buffer.indexOf("\n\n");

          let eventType = "message";
          let data = "";
          for (const line of raw.split("\n")) {
            if (line.startsWith("event:")) {
              eventType = line.slice("event:".length).trim();
            } else if (line.startsWith("data:")) {
              data += line.slice("data:".length).trim();
            }
          }

          if (!data) continue;
          try {
            if (eventType === "done") {
              sawDone = true;
            }
            onEvent({ type: eventType, data: JSON.parse(data) });
          } catch {
            // ignore parse errors
          }
        }
      }

      // If the stream ends without a done event and we didn't intentionally abort, surface it.
      if (!controller.signal.aborted && !sawDone) {
        onEvent({
          type: "error",
          data: { message: "Stream ended unexpectedly" },
        });
      }
    } catch {
      if (!controller.signal.aborted) {
        onEvent({
          type: "error",
          data: { message: "Stream connection failed" },
        });
      }
    }
  })();

  return () => controller.abort();
}

// List runs
export async function listRuns(
  limit = 20,
  offset = 0
): Promise<{ runs: Run[]; limit: number; offset: number }> {
  return apiGet(`/api/runs?limit=${limit}&offset=${offset}`, "Failed to fetch runs");
}

// Get run details
export async function getRun(id: string): Promise<Run> {
  return apiGet(`/api/runs/${id}`, "Failed to fetch run");
}

// Get run events
export async function getRunEvents(
  id: string,
  limit?: number
): Promise<{ run_id: string; events: unknown[] }> {
  const url = limit
    ? `/api/runs/${id}/events?limit=${limit}`
    : `/api/runs/${id}/events`;
  return apiGet(url, "Failed to fetch run events");
}

// Get run tasks
export async function getRunTasks(
  id: string
): Promise<{ run_id: string; tasks: unknown[] }> {
  return apiGet(`/api/runs/${id}/tasks`, "Failed to fetch run tasks");
}

// ==================== Missions ====================

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
  /** Backend used for this mission ("opencode" or "claudecode") */
  backend?: string;
  history: MissionHistoryEntry[];
  desktop_sessions?: DesktopSessionInfo[];
  created_at: string;
  updated_at: string;
  interrupted_at?: string;
  resumable?: boolean;
}

// List all missions
export async function listMissions(): Promise<Mission[]> {
  return apiGet("/api/control/missions", "Failed to fetch missions");
}

// Get a specific mission
export async function getMission(id: string): Promise<Mission> {
  return apiGet(`/api/control/missions/${id}`, "Failed to fetch mission");
}

// Stored event from SQLite (for event replay)
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

// Get mission events (for history replay including tool calls)
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

// Get current mission
export async function getCurrentMission(): Promise<Mission | null> {
  return apiGet("/api/control/missions/current", "Failed to fetch current mission");
}

// Create a new mission
export interface CreateMissionOptions {
  title?: string;
  workspaceId?: string;
  /** Agent name from library (e.g., "code-reviewer") */
  agent?: string;
  /** Override model for this mission (provider/model) */
  modelOverride?: string;
  /** Backend to use for this mission ("opencode" or "claudecode") */
  backend?: string;
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

// Load/switch to a mission
export async function loadMission(id: string): Promise<Mission> {
  return apiPost(`/api/control/missions/${id}/load`, undefined, "Failed to load mission");
}

// ==================== Parallel Missions ====================

export interface RunningMissionInfo {
  mission_id: string;
  state: "queued" | "running" | "waiting_for_tool" | "finished";
  queue_len: number;
  history_len: number;
  seconds_since_activity: number;
  expected_deliverables: number;
}

// Get all running parallel missions
export async function getRunningMissions(): Promise<RunningMissionInfo[]> {
  return apiGet("/api/control/running", "Failed to fetch running missions");
}

// Start a mission in parallel
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

// Cancel a specific mission
export async function cancelMission(missionId: string): Promise<void> {
  return apiPost(`/api/control/missions/${missionId}/cancel`, undefined, "Failed to cancel mission");
}

// Set mission status
export async function setMissionStatus(
  id: string,
  status: MissionStatus
): Promise<void> {
  return apiPost(`/api/control/missions/${id}/status`, { status }, "Failed to set mission status");
}

// Delete a mission
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

// Cleanup empty untitled missions
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

// Resume an interrupted mission
export async function resumeMission(id: string, cleanWorkspace: boolean = false): Promise<Mission> {
  const res = await apiFetch(`/api/control/missions/${id}/resume`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ clean_workspace: cleanWorkspace }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to resume mission: ${text}`);
  }
  return res.json();
}

// ==================== Global Control Session ====================

export type ControlRunState = "idle" | "running" | "waiting_for_tool";

/** File shared by the agent (images render inline, other files show as download links). */
export interface SharedFile {
  /** Display name for the file */
  name: string;
  /** Public URL to view/download */
  url: string;
  /** MIME type (e.g., "image/png", "application/pdf") */
  content_type: string;
  /** File size in bytes */
  size_bytes?: number;
  /** File kind for rendering hints: "image", "document", "archive", "code", "other" */
  kind: "image" | "document" | "archive" | "code" | "other";
}

export type ControlAgentEvent =
  | {
      type: "status";
      state: ControlRunState;
      queue_len: number;
      mission_id?: string;
    }
  | { type: "user_message"; id: string; content: string; mission_id?: string; queued?: boolean }
  | {
      type: "assistant_message";
      id: string;
      content: string;
      success: boolean;
      cost_cents: number;
      model: string | null;
      mission_id?: string;
      /** Files shared in this message (images, documents, etc.) */
      shared_files?: SharedFile[];
    }
  | { type: "thinking"; content: string; done: boolean; mission_id?: string }
  | {
      type: "tool_call";
      tool_call_id: string;
      name: string;
      args: unknown;
      mission_id?: string;
    }
  | {
      type: "tool_result";
      tool_call_id: string;
      name: string;
      result: unknown;
      mission_id?: string;
    }
  | { type: "error"; message: string; mission_id?: string };

export async function postControlMessage(
  content: string,
  options?: { agent?: string; mission_id?: string }
): Promise<{ id: string; queued: boolean }> {
  const body: { content: string; agent?: string; mission_id?: string } = { content };
  if (options?.agent) {
    body.agent = options.agent;
  }
  if (options?.mission_id) {
    body.mission_id = options.mission_id;
  }
  const res = await apiFetch("/api/control/message", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error("Failed to post control message");
  return res.json();
}

export async function postControlToolResult(payload: {
  tool_call_id: string;
  name: string;
  result: unknown;
}): Promise<void> {
  return apiPost("/api/control/tool_result", payload, "Failed to post tool result");
}

export async function cancelControl(): Promise<void> {
  return apiPost("/api/control/cancel", undefined, "Failed to cancel control session");
}

// Queue management
export interface QueuedMessage {
  id: string;
  content: string;
  agent: string | null;
}

export async function getQueue(): Promise<QueuedMessage[]> {
  return apiGet("/api/control/queue", "Failed to fetch queue");
}

export async function removeFromQueue(messageId: string): Promise<void> {
  return apiDel(`/api/control/queue/${messageId}`, "Failed to remove from queue");
}

export async function clearQueue(): Promise<{ cleared: number }> {
  return apiDel("/api/control/queue", "Failed to clear queue");
}

// Agent tree snapshot (for refresh resilience)
export interface AgentTreeNode {
  id: string;
  node_type: string;
  name: string;
  description: string;
  status: string;
  budget_allocated: number;
  budget_spent: number;
  complexity?: number;
  selected_model?: string;
  children: AgentTreeNode[];
}

export async function getAgentTree(): Promise<AgentTreeNode | null> {
  return apiGet("/api/control/tree", "Failed to fetch agent tree");
}

// Get tree for a specific mission (either live from memory or saved from database)
export async function getMissionTree(
  missionId: string
): Promise<AgentTreeNode | null> {
  return apiGet(`/api/control/missions/${missionId}/tree`, "Failed to fetch mission tree");
}

// Execution progress
export interface ExecutionProgress {
  total_subtasks: number;
  completed_subtasks: number;
  current_subtask: string | null;
  current_depth: number;
}

export async function getProgress(): Promise<ExecutionProgress> {
  return apiGet("/api/control/progress", "Failed to fetch progress");
}

export type StreamDiagnosticPhase = "connecting" | "open" | "chunk" | "event" | "closed" | "error";

export type StreamDiagnosticUpdate = {
  phase: StreamDiagnosticPhase;
  url: string;
  status?: number;
  headers?: Record<string, string>;
  bytes?: number;
  error?: string;
  timestamp: number;
};

export function streamControl(
  onEvent: (event: { type: string; data: unknown }) => void,
  onDiagnostics?: (update: StreamDiagnosticUpdate) => void
): () => void {
  const controller = new AbortController();
  const decoder = new TextDecoder();
  let buffer = "";
  let bytesRead = 0;
  const streamUrl = apiUrl("/api/control/stream");

  onDiagnostics?.({
    phase: "connecting",
    url: streamUrl,
    timestamp: Date.now(),
  });

  void (async () => {
    try {
      const res = await apiFetch(streamUrl, {
        method: "GET",
        headers: { Accept: "text/event-stream" },
        signal: controller.signal,
      });

      if (!res.ok) {
        onEvent({
          type: "error",
          data: {
            message: `Stream request failed (${res.status})`,
            status: res.status,
          },
        });
        onDiagnostics?.({
          phase: "error",
          url: streamUrl,
          status: res.status,
          error: `Stream request failed (${res.status})`,
          timestamp: Date.now(),
        });
        return;
      }
      if (!res.body) {
        onEvent({
          type: "error",
          data: { message: "Stream response had no body" },
        });
        onDiagnostics?.({
          phase: "error",
          url: streamUrl,
          status: res.status,
          error: "Stream response had no body",
          timestamp: Date.now(),
        });
        return;
      }

      const headers: Record<string, string> = {};
      res.headers.forEach((value, key) => {
        headers[key.toLowerCase()] = value;
      });
      onDiagnostics?.({
        phase: "open",
        url: streamUrl,
        status: res.status,
        headers,
        timestamp: Date.now(),
      });

      const reader = res.body.getReader();
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        if (value) {
          bytesRead += value.length;
        }
        let chunk = decoder.decode(value, { stream: true });
        if (buffer.endsWith("\r") && chunk.startsWith("\n")) {
          buffer = buffer.slice(0, -1);
        }
        buffer += chunk;
        if (buffer.includes("\r")) {
          buffer = buffer.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
        }
        onDiagnostics?.({
          phase: "chunk",
          url: streamUrl,
          bytes: bytesRead,
          timestamp: Date.now(),
        });

        let idx = buffer.indexOf("\n\n");
        while (idx !== -1) {
          const raw = buffer.slice(0, idx);
          buffer = buffer.slice(idx + 2);
          idx = buffer.indexOf("\n\n");

          let eventType = "message";
          let data = "";
          for (const line of raw.split("\n")) {
            if (line.startsWith("event:")) {
              eventType = line.slice("event:".length).trim();
            } else if (line.startsWith("data:")) {
              data += line.slice("data:".length).trim();
            }
            // SSE comments (lines starting with :) are ignored for keepalive
          }

          if (!data) continue;
          try {
            onEvent({ type: eventType, data: JSON.parse(data) });
            onDiagnostics?.({
              phase: "event",
              url: streamUrl,
              bytes: bytesRead,
              timestamp: Date.now(),
            });
          } catch {
            // ignore parse errors
          }
        }
      }

      // Stream ended normally (server closed connection)
      onEvent({
        type: "error",
        data: { message: "Stream ended - server closed connection" },
      });
      onDiagnostics?.({
        phase: "closed",
        url: streamUrl,
        bytes: bytesRead,
        timestamp: Date.now(),
      });
    } catch (err) {
      if (!controller.signal.aborted) {
        // Provide more specific error messages
        const errorMessage =
          err instanceof Error
            ? `Stream connection failed: ${err.message}`
            : "Stream connection failed";
        onEvent({
          type: "error",
          data: { message: errorMessage },
        });
        onDiagnostics?.({
          phase: "error",
          url: streamUrl,
          error: errorMessage,
          timestamp: Date.now(),
        });
      }
    }
  })();

  return () => controller.abort();
}

// ==================== MCP Management ====================

export type McpStatus = "connected" | "connecting" | "disconnected" | "error" | "disabled";
export type McpScope = "global" | "workspace";

export interface McpTransport {
  http?: { endpoint: string; headers: Record<string, string> };
  stdio?: { command: string; args: string[]; env: Record<string, string> };
}

export interface McpServerConfig {
  id: string;
  name: string;
  transport: McpTransport;
  endpoint: string;
  scope: McpScope;
  description: string | null;
  enabled: boolean;
  version: string | null;
  tools: string[];
  created_at: string;
  last_connected_at: string | null;
}

export interface McpServerState extends McpServerConfig {
  status: McpStatus;
  error: string | null;
  tool_calls: number;
  tool_errors: number;
}

export interface ToolInfo {
  name: string;
  description: string;
  source: "builtin" | { mcp: { id: string; name: string } } | { plugin: { id: string; name: string } };
  enabled: boolean;
}

// List all MCP servers
export async function listMcps(): Promise<McpServerState[]> {
  return apiGet("/api/mcp", "Failed to fetch MCPs");
}

// Get a specific MCP server
export async function getMcp(id: string): Promise<McpServerState> {
  return apiGet(`/api/mcp/${id}`, "Failed to fetch MCP");
}

// Add a new MCP server
export async function addMcp(data: {
  name: string;
  endpoint: string;
  description?: string;
  scope?: McpScope;
}): Promise<McpServerState> {
  return apiPost("/api/mcp", data, "Failed to add MCP");
}

// Remove an MCP server
export async function removeMcp(id: string): Promise<void> {
  return apiDel(`/api/mcp/${id}`, "Failed to remove MCP");
}

// Enable an MCP server
export async function enableMcp(id: string): Promise<McpServerState> {
  return apiPost(`/api/mcp/${id}/enable`, undefined, "Failed to enable MCP");
}

// Disable an MCP server
export async function disableMcp(id: string): Promise<McpServerState> {
  return apiPost(`/api/mcp/${id}/disable`, undefined, "Failed to disable MCP");
}

// Refresh an MCP server (reconnect and discover tools)
export async function refreshMcp(id: string): Promise<McpServerState> {
  return apiPost(`/api/mcp/${id}/refresh`, undefined, "Failed to refresh MCP");
}

// Update an MCP server configuration
export interface UpdateMcpRequest {
  name?: string;
  description?: string;
  enabled?: boolean;
  transport?: McpTransport;
  scope?: McpScope;
}

export async function updateMcp(id: string, data: UpdateMcpRequest): Promise<McpServerState> {
  return apiPatch(`/api/mcp/${id}`, data, "Failed to update MCP");
}

// Refresh all MCP servers
export async function refreshAllMcps(): Promise<void> {
  return apiPost("/api/mcp/refresh", undefined, "Failed to refresh MCPs");
}

// List all tools
export async function listTools(): Promise<ToolInfo[]> {
  return apiGet("/api/tools", "Failed to fetch tools");
}

// Toggle a tool
export async function toggleTool(
  name: string,
  enabled: boolean
): Promise<void> {
  return apiPost(`/api/tools/${encodeURIComponent(name)}/toggle`, { enabled }, "Failed to toggle tool");
}

// ==================== File System ====================

export interface UploadResult {
  ok: boolean;
  path: string;
  name: string;
}

export interface UploadProgress {
  loaded: number;
  total: number;
  percentage: number;
}

// Upload a file to the remote filesystem with progress tracking
export function uploadFile(
  file: File,
  remotePath: string = "./context/",
  onProgress?: (progress: UploadProgress) => void,
  workspaceId?: string,
  missionId?: string
): Promise<UploadResult> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const params = new URLSearchParams({ path: remotePath });
    if (workspaceId) {
      params.append("workspace_id", workspaceId);
    }
    if (missionId) {
      params.append("mission_id", missionId);
    }
    const url = apiUrl(`/api/fs/upload?${params}`);
    
    // Track upload progress
    xhr.upload.addEventListener("progress", (event) => {
      if (event.lengthComputable && onProgress) {
        onProgress({
          loaded: event.loaded,
          total: event.total,
          percentage: Math.round((event.loaded / event.total) * 100),
        });
      }
    });
    
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText));
        } catch {
          reject(new Error("Invalid response from server"));
        }
      } else {
        reject(new Error(`Upload failed: ${xhr.responseText || xhr.statusText}`));
      }
    });
    
    xhr.addEventListener("error", () => {
      reject(new Error("Network error during upload"));
    });
    
    xhr.addEventListener("abort", () => {
      reject(new Error("Upload cancelled"));
    });
    
    xhr.open("POST", url);
    
    // Add auth header using the same method as other API calls
    const headers = authHeader();
    if (headers.Authorization) {
      xhr.setRequestHeader("Authorization", headers.Authorization);
    }
    
    const formData = new FormData();
    formData.append("file", file);
    xhr.send(formData);
  });
}

// Upload a file in chunks with resume capability
const CHUNK_SIZE = 5 * 1024 * 1024; // 5MB chunks

export interface ChunkedUploadProgress extends UploadProgress {
  chunkIndex: number;
  totalChunks: number;
}

export async function uploadFileChunked(
  file: File,
  remotePath: string = "./context/",
  onProgress?: (progress: ChunkedUploadProgress) => void,
  workspaceId?: string,
  missionId?: string
): Promise<UploadResult> {
  const totalChunks = Math.ceil(file.size / CHUNK_SIZE);
  const uploadId = `${file.name}-${file.size}-${Date.now()}`;

  // For small files, use regular upload
  if (totalChunks <= 1) {
    return uploadFile(file, remotePath, onProgress ? (p) => onProgress({
      ...p,
      chunkIndex: 0,
      totalChunks: 1,
    }) : undefined, workspaceId, missionId);
  }

  let uploadedBytes = 0;

  for (let i = 0; i < totalChunks; i++) {
    const start = i * CHUNK_SIZE;
    const end = Math.min(start + CHUNK_SIZE, file.size);
    const chunk = file.slice(start, end);

    const chunkFile = new File([chunk], file.name, { type: file.type });

    // Upload chunk with retry
    let retries = 3;
    while (retries > 0) {
      try {
        await uploadChunk(chunkFile, remotePath, uploadId, i, totalChunks, workspaceId);
        uploadedBytes += chunk.size;

        if (onProgress) {
          onProgress({
            loaded: uploadedBytes,
            total: file.size,
            percentage: Math.round((uploadedBytes / file.size) * 100),
            chunkIndex: i + 1,
            totalChunks,
          });
        }
        break;
      } catch (err) {
        retries--;
        if (retries === 0) throw err;
        await new Promise(r => setTimeout(r, 1000)); // Wait 1s before retry
      }
    }
  }

  // Finalize the upload
  return finalizeChunkedUpload(remotePath, uploadId, file.name, totalChunks, workspaceId, missionId);
}

async function uploadChunk(
  chunk: File,
  remotePath: string,
  uploadId: string,
  chunkIndex: number,
  totalChunks: number,
  workspaceId?: string
): Promise<void> {
  const formData = new FormData();
  formData.append("file", chunk);

  const params = new URLSearchParams({
    path: remotePath,
    upload_id: uploadId,
    chunk_index: String(chunkIndex),
    total_chunks: String(totalChunks),
  });
  if (workspaceId) {
    params.append("workspace_id", workspaceId);
  }

  const res = await fetch(apiUrl(`/api/fs/upload-chunk?${params}`), {
    method: "POST",
    headers: authHeader(),
    body: formData,
  });

  if (!res.ok) {
    throw new Error(`Chunk upload failed: ${await res.text()}`);
  }
}

async function finalizeChunkedUpload(
  remotePath: string,
  uploadId: string,
  fileName: string,
  totalChunks: number,
  workspaceId?: string,
  missionId?: string
): Promise<UploadResult> {
  const body: Record<string, unknown> = {
    path: remotePath,
    upload_id: uploadId,
    file_name: fileName,
    total_chunks: totalChunks,
  };
  if (workspaceId) {
    body.workspace_id = workspaceId;
  }
  if (missionId) {
    body.mission_id = missionId;
  }

  const res = await apiFetch("/api/fs/upload-finalize", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (!res.ok) {
    throw new Error(`Failed to finalize upload: ${await res.text()}`);
  }

  return res.json();
}

// Download file from URL to server filesystem
export async function downloadFromUrl(
  url: string,
  remotePath: string = "./context/",
  fileName?: string,
  workspaceId?: string,
  missionId?: string
): Promise<UploadResult> {
  const body: Record<string, unknown> = {
    url,
    path: remotePath,
    file_name: fileName,
  };
  if (workspaceId) {
    body.workspace_id = workspaceId;
  }
  if (missionId) {
    body.mission_id = missionId;
  }

  const res = await apiFetch("/api/fs/download-url", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (!res.ok) {
    throw new Error(`Failed to download from URL: ${await res.text()}`);
  }

  return res.json();
}

// Re-export from shared module for backwards compatibility
export { formatBytes } from "./format";

// ==================== Providers ====================

export interface ProviderModel {
  id: string;
  name: string;
  description?: string;
}

export interface Provider {
  id: string;
  name: string;
  billing: "subscription" | "pay-per-token";
  description: string;
  models: ProviderModel[];
}

export interface ProvidersResponse {
  providers: Provider[];
}

// List available providers and their models
export async function listProviders(options?: { includeAll?: boolean }): Promise<ProvidersResponse> {
  const params = new URLSearchParams();
  if (options?.includeAll) {
    params.set("include_all", "true");
  }
  const query = params.toString();
  const res = await apiFetch(`/api/providers${query ? `?${query}` : ""}`);
  if (!res.ok) throw new Error("Failed to fetch providers");
  return res.json();
}

// ==================== Library (Configuration) ====================

export interface LibraryStatus {
  path: string;
  remote: string | null;
  branch: string;
  clean: boolean;
  ahead: number;
  behind: number;
  modified_files: string[];
}

// MCP Server definition (OpenCode-aligned format)
export interface McpServerDef {
  type: "local" | "remote";
  // Local (stdio) server fields
  command?: string[];
  env?: Record<string, string>;
  // Remote (HTTP) server fields
  url?: string;
  headers?: Record<string, string>;
  // Common
  enabled?: boolean;
}

// Skill file within a skill folder
export interface SkillFile {
  name: string;
  path: string;
  content: string;
}

export interface SkillSummary {
  name: string;
  description: string | null;
  path: string;
}

export interface Skill {
  name: string;
  description: string | null;
  path: string;
  content: string;
  files: SkillFile[];
  references: string[];
}

// Plugin types
export interface PluginUI {
  icon: string | null;
  label: string;
  hint: string | null;
  category: string | null;
}

export interface Plugin {
  package: string;
  description: string | null;
  enabled: boolean;
  ui: PluginUI;
}

// Rule types
export interface RuleSummary {
  name: string;
  description: string | null;
  path: string;
}

export interface Rule {
  name: string;
  description: string | null;
  path: string;
  content: string;
}

// Library Agent types
export interface LibraryAgentSummary {
  name: string;
  description: string | null;
  path: string;
}

export interface LibraryAgent {
  name: string;
  description: string | null;
  path: string;
  content: string;
  model: string | null;
  tools: Record<string, boolean>;
  permissions: Record<string, string>;
  rules: string[];
}

// Library Tool types
export interface LibraryToolSummary {
  name: string;
  description: string | null;
  path: string;
}

export interface LibraryTool {
  name: string;
  description: string | null;
  path: string;
  content: string;
}

// Migration report
export interface MigrationReport {
  directories_renamed: [string, string][];
  files_converted: string[];
  errors: string[];
  success: boolean;
}

export interface CommandParam {
  name: string;
  required: boolean;
  description: string | null;
}

export interface CommandSummary {
  name: string;
  description: string | null;
  path: string;
  params?: CommandParam[];
}

export interface Command {
  name: string;
  description: string | null;
  path: string;
  content: string;
  params?: CommandParam[];
}

// Git status
export async function getLibraryStatus(): Promise<LibraryStatus> {
  return libGet("/api/library/status", "Failed to fetch library status");
}

// Sync (git pull)
export async function syncLibrary(): Promise<void> {
  return libPost("/api/library/sync", undefined, "Failed to sync library");
}

// Commit changes
export async function commitLibrary(message: string): Promise<void> {
  return libPost("/api/library/commit", { message }, "Failed to commit library");
}

// Push changes
export async function pushLibrary(): Promise<void> {
  return libPost("/api/library/push", undefined, "Failed to push library");
}

// Get MCP servers
export async function getLibraryMcps(): Promise<Record<string, McpServerDef>> {
  return libGet("/api/library/mcps", "Failed to fetch MCPs");
}

// Save MCP servers
export async function saveLibraryMcps(
  servers: Record<string, McpServerDef>
): Promise<void> {
  return libPut("/api/library/mcps", servers, "Failed to save MCPs");
}

// List skills
export async function listLibrarySkills(): Promise<SkillSummary[]> {
  return libGet("/api/library/skills", "Failed to fetch skills");
}

// Get skill
export async function getLibrarySkill(name: string): Promise<Skill> {
  return libGet(`/api/library/skills/${encodeURIComponent(name)}`, "Failed to fetch skill");
}

// Save skill
export async function saveLibrarySkill(
  name: string,
  content: string
): Promise<void> {
  return libPut(`/api/library/skills/${encodeURIComponent(name)}`, { content }, "Failed to save skill");
}

// Delete skill
export async function deleteLibrarySkill(name: string): Promise<void> {
  return libDel(`/api/library/skills/${encodeURIComponent(name)}`, "Failed to delete skill");
}

// Get skill reference file (returns text, not JSON)
export async function getSkillReference(
  skillName: string,
  refPath: string
): Promise<string> {
  const res = await apiFetch(
    `/api/library/skills/${encodeURIComponent(skillName)}/references/${refPath}`
  );
  await ensureLibraryResponse(res, "Failed to fetch reference file");
  return res.text();
}

// Save skill reference file
export async function saveSkillReference(
  skillName: string,
  refPath: string,
  content: string
): Promise<void> {
  return libPut(
    `/api/library/skills/${encodeURIComponent(skillName)}/references/${refPath}`,
    { content },
    "Failed to save reference file",
  );
}

// Delete skill reference file
export async function deleteSkillReference(
  skillName: string,
  refPath: string
): Promise<void> {
  return libDel(
    `/api/library/skills/${encodeURIComponent(skillName)}/references/${refPath}`,
    "Failed to delete reference file",
  );
}

// Import skill from Git URL
export interface ImportSkillRequest {
  url: string;
  path?: string;
  name?: string;
}

export async function importSkill(request: ImportSkillRequest): Promise<Skill> {
  return libPost("/api/library/skills/import", request, "Failed to import skill");
}

// Validate skill name (matches backend pattern)
export function validateSkillName(name: string): { valid: boolean; error?: string } {
  if (!name || name.length === 0) {
    return { valid: false, error: "Name cannot be empty" };
  }
  if (name.length > 64) {
    return { valid: false, error: "Name must be 64 characters or less" };
  }
  if (name.startsWith("-") || name.endsWith("-")) {
    return { valid: false, error: "Name cannot start or end with a hyphen" };
  }
  if (name.includes("--")) {
    return { valid: false, error: "Name cannot contain consecutive hyphens" };
  }
  if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(name)) {
    return { valid: false, error: "Name must be lowercase alphanumeric with single hyphens" };
  }
  return { valid: true };
}

// List commands
export async function listLibraryCommands(): Promise<CommandSummary[]> {
  return libGet("/api/library/commands", "Failed to fetch commands");
}

// Builtin commands response
export interface BuiltinCommandsResponse {
  opencode: CommandSummary[];
  claudecode: CommandSummary[];
}

// Get builtin slash commands for each backend
export async function getBuiltinCommands(): Promise<BuiltinCommandsResponse> {
  const res = await apiFetch("/api/library/builtin-commands");
  if (!res.ok) {
    // Fallback to empty if endpoint not available
    return { opencode: [], claudecode: [] };
  }
  return res.json();
}

// Get command
export async function getLibraryCommand(name: string): Promise<Command> {
  return libGet(`/api/library/commands/${encodeURIComponent(name)}`, "Failed to fetch command");
}

// Save command
export async function saveLibraryCommand(
  name: string,
  content: string
): Promise<void> {
  return libPut(`/api/library/commands/${encodeURIComponent(name)}`, { content }, "Failed to save command");
}

// Delete command
export async function deleteLibraryCommand(name: string): Promise<void> {
  return libDel(`/api/library/commands/${encodeURIComponent(name)}`, "Failed to delete command");
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugins
// ─────────────────────────────────────────────────────────────────────────────

// Get all plugins
export async function getLibraryPlugins(): Promise<Record<string, Plugin>> {
  return libGet("/api/library/plugins", "Failed to fetch plugins");
}

// Save all plugins
export async function saveLibraryPlugins(
  plugins: Record<string, Plugin>
): Promise<void> {
  return libPut("/api/library/plugins", plugins, "Failed to save plugins");
}

// ─────────────────────────────────────────────────────────────────────────────
// Installed OpenCode Plugins (discovered from OpenCode config)
// ─────────────────────────────────────────────────────────────────────────────

export interface InstalledPluginInfo {
  package: string;
  spec: string;
  installed_version: string | null;
  latest_version: string | null;
  update_available: boolean;
}

export interface InstalledPluginsResponse {
  plugins: InstalledPluginInfo[];
}

// Get installed plugins from OpenCode config with version info
export async function getInstalledPlugins(): Promise<InstalledPluginsResponse> {
  return apiGet("/api/system/plugins/installed", "Failed to fetch installed plugins");
}

// Update a plugin (returns SSE stream)
export function updatePlugin(
  packageName: string,
  onEvent: (event: { event_type: string; message: string; progress?: number }) => void
): () => void {
  const url = `${window.location.origin}/api/system/plugins/${encodeURIComponent(packageName)}/update`;

  const eventSource = new EventSource(url, { withCredentials: true });

  eventSource.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      onEvent(data);
      if (data.event_type === "complete" || data.event_type === "error") {
        eventSource.close();
      }
    } catch (e) {
      console.error("Failed to parse SSE event:", e);
    }
  };

  eventSource.onerror = () => {
    eventSource.close();
    onEvent({
      event_type: "error",
      message: "Connection error: failed to connect to server",
      progress: undefined,
    });
  };

  return () => eventSource.close();
}

// ─────────────────────────────────────────────────────────────────────────────
// Rules
// ─────────────────────────────────────────────────────────────────────────────

// List rules
export async function listLibraryRules(): Promise<RuleSummary[]> {
  return libGet("/api/library/rule", "Failed to fetch rules");
}

// Get rule
export async function getLibraryRule(name: string): Promise<Rule> {
  return libGet(`/api/library/rule/${encodeURIComponent(name)}`, "Failed to fetch rule");
}

// Save rule
export async function saveLibraryRule(
  name: string,
  content: string
): Promise<void> {
  return libPut(`/api/library/rule/${encodeURIComponent(name)}`, { content }, "Failed to save rule");
}

// Delete rule
export async function deleteLibraryRule(name: string): Promise<void> {
  return libDel(`/api/library/rule/${encodeURIComponent(name)}`, "Failed to delete rule");
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Agents
// ─────────────────────────────────────────────────────────────────────────────

// List library agents
export async function listLibraryAgents(): Promise<LibraryAgentSummary[]> {
  return libGet("/api/library/agent", "Failed to fetch library agents");
}

// Get library agent
export async function getLibraryAgent(name: string): Promise<LibraryAgent> {
  return libGet(`/api/library/agent/${encodeURIComponent(name)}`, "Failed to fetch library agent");
}

// Save library agent
export async function saveLibraryAgent(
  name: string,
  agent: LibraryAgent
): Promise<void> {
  return libPut(`/api/library/agent/${encodeURIComponent(name)}`, agent, "Failed to save library agent");
}

// Delete library agent
export async function deleteLibraryAgent(name: string): Promise<void> {
  return libDel(`/api/library/agent/${encodeURIComponent(name)}`, "Failed to delete library agent");
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Tools
// ─────────────────────────────────────────────────────────────────────────────

// List library tools
export async function listLibraryTools(): Promise<LibraryToolSummary[]> {
  return libGet("/api/library/tool", "Failed to fetch library tools");
}

// Get library tool
export async function getLibraryTool(name: string): Promise<LibraryTool> {
  return libGet(`/api/library/tool/${encodeURIComponent(name)}`, "Failed to fetch library tool");
}

// Save library tool
export async function saveLibraryTool(
  name: string,
  content: string
): Promise<void> {
  return libPut(`/api/library/tool/${encodeURIComponent(name)}`, { content }, "Failed to save library tool");
}

// Delete library tool
export async function deleteLibraryTool(name: string): Promise<void> {
  return libDel(`/api/library/tool/${encodeURIComponent(name)}`, "Failed to delete library tool");
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace Templates
// ─────────────────────────────────────────────────────────────────────────────

export interface WorkspaceTemplateSummary {
  name: string;
  description?: string;
  path: string;
  distro?: string;
  skills?: string[];
}

export interface WorkspaceTemplate {
  name: string;
  description?: string;
  path: string;
  distro?: string;
  skills: string[];
  env_vars: Record<string, string>;
  encrypted_keys: string[];
  init_script: string;
  shared_network?: boolean | null;
}

export async function listWorkspaceTemplates(): Promise<WorkspaceTemplateSummary[]> {
  return libGet("/api/library/workspace-template", "Failed to fetch workspace templates");
}

export async function getWorkspaceTemplate(name: string): Promise<WorkspaceTemplate> {
  return libGet(`/api/library/workspace-template/${encodeURIComponent(name)}`, "Failed to fetch workspace template");
}

export async function saveWorkspaceTemplate(
  name: string,
  data: {
    description?: string;
    distro?: string;
    skills?: string[];
    env_vars?: Record<string, string>;
    encrypted_keys?: string[];
    init_script?: string;
    shared_network?: boolean | null;
  }
): Promise<void> {
  return libPut(`/api/library/workspace-template/${encodeURIComponent(name)}`, data, "Failed to save workspace template");
}

export async function deleteWorkspaceTemplate(name: string): Promise<void> {
  return libDel(`/api/library/workspace-template/${encodeURIComponent(name)}`, "Failed to delete workspace template");
}

export async function renameWorkspaceTemplate(oldName: string, newName: string): Promise<void> {
  // Get the existing template
  const template = await getWorkspaceTemplate(oldName);
  // Save with new name
  await saveWorkspaceTemplate(newName, {
    description: template.description,
    distro: template.distro,
    skills: template.skills,
    env_vars: template.env_vars,
    encrypted_keys: template.encrypted_keys,
    init_script: template.init_script,
    shared_network: template.shared_network,
  });
  // Delete old template
  await deleteWorkspaceTemplate(oldName);
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Rename
// ─────────────────────────────────────────────────────────────────────────────

export type LibraryItemType =
  | "skill"
  | "command"
  | "rule"
  | "agent"
  | "tool"
  | "workspace-template";

export interface RenameChange {
  type: "rename_file" | "update_reference" | "update_workspace";
  from?: string;
  to?: string;
  file?: string;
  field?: string;
  old_value?: string;
  new_value?: string;
  workspace_id?: string;
  workspace_name?: string;
}

export interface RenameResult {
  success: boolean;
  changes: RenameChange[];
  warnings: string[];
  error?: string;
}

/**
 * Rename a library item and update all references.
 * Supports dry_run mode to preview changes before applying them.
 */
export async function renameLibraryItem(
  itemType: LibraryItemType,
  oldName: string,
  newName: string,
  dryRun: boolean = false
): Promise<RenameResult> {
  return libPost(
    `/api/library/rename/${itemType}/${encodeURIComponent(oldName)}`,
    { new_name: newName, dry_run: dryRun },
    "Failed to rename item",
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Migration
// ─────────────────────────────────────────────────────────────────────────────

// Migrate library structure to new format
export async function migrateLibrary(): Promise<MigrationReport> {
  return libPost("/api/library/migrate", undefined, "Failed to migrate library");
}

// ==================== Workspaces ====================

export type WorkspaceType = "host" | "container";
export type WorkspaceStatus = "pending" | "building" | "ready" | "error";

export interface Workspace {
  id: string;
  name: string;
  workspace_type: WorkspaceType;
  path: string;
  status: WorkspaceStatus;
  error_message: string | null;
  created_at: string;
  skills: string[];
  plugins: string[];
  template?: string | null;
  distro?: string | null;
  env_vars: Record<string, string>;
  init_script?: string | null;
  shared_network?: boolean | null;
}

// List workspaces
export async function listWorkspaces(): Promise<Workspace[]> {
  return apiGet("/api/workspaces", "Failed to fetch workspaces");
}

// Get workspace
export async function getWorkspace(id: string): Promise<Workspace> {
  return apiGet(`/api/workspaces/${id}`, "Failed to fetch workspace");
}

// Create workspace
export async function createWorkspace(data: {
  name: string;
  workspace_type: WorkspaceType;
  path?: string;
  skills?: string[];
  plugins?: string[];
  template?: string;
  distro?: string;
  env_vars?: Record<string, string>;
  init_script?: string;
  shared_network?: boolean | null;
}): Promise<Workspace> {
  return apiPost("/api/workspaces", data, "Failed to create workspace");
}

// Update workspace
export async function updateWorkspace(
  id: string,
  data: {
    name?: string;
    skills?: string[];
    plugins?: string[];
    template?: string | null;
    distro?: string | null;
    env_vars?: Record<string, string>;
    init_script?: string | null;
    shared_network?: boolean | null;
  }
): Promise<Workspace> {
  return apiPut(`/api/workspaces/${id}`, data, "Failed to update workspace");
}

// Sync workspace skills
export async function syncWorkspace(id: string): Promise<Workspace> {
  return apiPost(`/api/workspaces/${id}/sync`, undefined, "Failed to sync workspace");
}

// Delete workspace
export async function deleteWorkspace(id: string): Promise<void> {
  return apiDel(`/api/workspaces/${id}`, "Failed to delete workspace");
}

// Supported Linux distributions for container workspaces
export type ContainerDistro =
  | "ubuntu-noble"
  | "ubuntu-jammy"
  | "debian-bookworm"
  | "arch-linux";

export const CONTAINER_DISTROS: { value: ContainerDistro; label: string }[] = [
  { value: "ubuntu-noble", label: "Ubuntu 24.04 LTS (Noble)" },
  { value: "ubuntu-jammy", label: "Ubuntu 22.04 LTS (Jammy)" },
  { value: "debian-bookworm", label: "Debian 12 (Bookworm)" },
  { value: "arch-linux", label: "Arch Linux (Base)" },
];

// Build a container workspace
export async function buildWorkspace(
  id: string,
  distro?: ContainerDistro,
  rebuild?: boolean
): Promise<Workspace> {
  const res = await apiFetch(`/api/workspaces/${id}/build`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: distro || rebuild ? JSON.stringify({ distro, rebuild }) : undefined,
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to build workspace");
  }
  return res.json();
}

// Debug info for a workspace container
export interface WorkspaceDebugInfo {
  id: string;
  name: string;
  status: string;
  path: string;
  path_exists: boolean;
  size_bytes: number | null;
  directories: { path: string; exists: boolean; file_count: number | null }[];
  has_bash: boolean;
  init_script_exists: boolean;
  init_script_modified: string | null;
  distro: string | null;
  last_error: string | null;
}

export interface InitLogResponse {
  exists: boolean;
  content: string | null;
  total_lines: number | null;
  log_path: string;
}

// Get workspace debug info (container state)
export async function getWorkspaceDebug(id: string): Promise<WorkspaceDebugInfo> {
  const res = await apiFetch(`/api/workspaces/${id}/debug`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to get workspace debug info");
  }
  return res.json();
}

// Get init script log from container
export async function getWorkspaceInitLog(id: string): Promise<InitLogResponse> {
  const res = await apiFetch(`/api/workspaces/${id}/init-log`);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to get init log");
  }
  return res.json();
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Connection API
// ─────────────────────────────────────────────────────────────────────────────

export interface OpenCodeConnection {
  id: string;
  name: string;
  base_url: string;
  agent: string | null;
  permissive: boolean;
  enabled: boolean;
  is_default: boolean;
  created_at: string;
  updated_at: string;
}

export interface TestConnectionResponse {
  success: boolean;
  message: string;
  version: string | null;
}

// List all OpenCode connections
export async function listOpenCodeConnections(): Promise<OpenCodeConnection[]> {
  return apiGet("/api/opencode/connections", "Failed to list OpenCode connections");
}

// Get connection by ID
export async function getOpenCodeConnection(id: string): Promise<OpenCodeConnection> {
  return apiGet(`/api/opencode/connections/${id}`, "Failed to get OpenCode connection");
}

// Create new connection
export async function createOpenCodeConnection(data: {
  name: string;
  base_url: string;
  agent?: string | null;
  permissive?: boolean;
  enabled?: boolean;
}): Promise<OpenCodeConnection> {
  return apiPost("/api/opencode/connections", data, "Failed to create OpenCode connection");
}

// Update connection
export async function updateOpenCodeConnection(
  id: string,
  data: {
    name?: string;
    base_url?: string;
    agent?: string | null;
    permissive?: boolean;
    enabled?: boolean;
  }
): Promise<OpenCodeConnection> {
  return apiPut(`/api/opencode/connections/${id}`, data, "Failed to update OpenCode connection");
}

// Delete connection
export async function deleteOpenCodeConnection(id: string): Promise<void> {
  return apiDel(`/api/opencode/connections/${id}`, "Failed to delete OpenCode connection");
}

// Test connection
export async function testOpenCodeConnection(id: string): Promise<TestConnectionResponse> {
  return apiPost(`/api/opencode/connections/${id}/test`, undefined, "Failed to test OpenCode connection");
}

// Set default connection
export async function setDefaultOpenCodeConnection(id: string): Promise<OpenCodeConnection> {
  return apiPost(`/api/opencode/connections/${id}/default`, undefined, "Failed to set default OpenCode connection");
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Settings API (oh-my-opencode.json)
// ─────────────────────────────────────────────────────────────────────────────

// Get OpenCode settings (oh-my-opencode.json)
export async function getOpenCodeSettings(): Promise<Record<string, unknown>> {
  return apiGet("/api/opencode/settings", "Failed to get OpenCode settings");
}

// Update OpenCode settings (oh-my-opencode.json)
export async function updateOpenCodeSettings(settings: Record<string, unknown>): Promise<Record<string, unknown>> {
  return apiPut("/api/opencode/settings", settings, "Failed to update OpenCode settings");
}

// Restart OpenCode service (to apply settings changes)
export async function restartOpenCodeService(): Promise<{ success: boolean; message: string }> {
  return apiPost("/api/opencode/restart", undefined, "Failed to restart OpenCode service");
}

// ─────────────────────────────────────────────────────────────────────────────
// Library-backed OpenCode Settings API
// ─────────────────────────────────────────────────────────────────────────────

// Get OpenCode settings from Library (oh-my-opencode.json)
export async function getLibraryOpenCodeSettings(): Promise<Record<string, unknown>> {
  return apiGet("/api/library/opencode/settings", "Failed to get Library OpenCode settings");
}

// Save OpenCode settings to Library and sync to system
export async function saveLibraryOpenCodeSettings(settings: Record<string, unknown>): Promise<void> {
  return apiPut("/api/library/opencode/settings", settings, "Failed to save Library OpenCode settings");
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenAgent Config API
// ─────────────────────────────────────────────────────────────────────────────

export interface OpenAgentConfig {
  hidden_agents: string[];
  default_agent: string | null;
}

// Get OpenAgent config from Library
export async function getOpenAgentConfig(): Promise<OpenAgentConfig> {
  return apiGet("/api/library/openagent/config", "Failed to get OpenAgent config");
}

// Save OpenAgent config to Library
export async function saveOpenAgentConfig(config: OpenAgentConfig): Promise<void> {
  return apiPut("/api/library/openagent/config", config, "Failed to save OpenAgent config");
}

// Get visible agents (filtered by OpenAgent config)
export async function getVisibleAgents(): Promise<unknown> {
  return apiGet("/api/library/openagent/agents", "Failed to get visible agents");
}

// Claude Code config stored in Library
export interface ClaudeCodeConfig {
  default_model: string | null;
  default_agent: string | null;
  hidden_agents: string[];
}

// Get Claude Code config from Library
export async function getClaudeCodeConfig(): Promise<ClaudeCodeConfig> {
  return apiGet("/api/library/claudecode/config", "Failed to get Claude Code config");
}

// Save Claude Code config to Library
export async function saveClaudeCodeConfig(
  config: ClaudeCodeConfig
): Promise<void> {
  return apiPut("/api/library/claudecode/config", config, "Failed to save Claude Code config");
}

// ─────────────────────────────────────────────────────────────────────────────
// AI Provider API
// ─────────────────────────────────────────────────────────────────────────────

export type AIProviderType =
  | "anthropic"
  | "openai"
  | "google"
  | "amazon-bedrock"
  | "azure"
  | "open-router"
  | "mistral"
  | "groq"
  | "xai"
  | "deep-infra"
  | "cerebras"
  | "cohere"
  | "together-ai"
  | "perplexity"
  | "github-copilot"
  | "zai"
  | "custom";

export interface AIProviderTypeInfo {
  id: string;
  name: string;
  uses_oauth: boolean;
  env_var: string | null;
}

export interface AIProviderStatus {
  type: "unknown" | "connected" | "needs_auth" | "error";
  auth_url?: string;
  message?: string;
}

export interface AIProviderAuthMethod {
  label: string;
  type: "oauth" | "api";
  description?: string;
}

export interface AIProvider {
  id: string;
  provider_type: AIProviderType;
  provider_type_name: string;
  name: string;
  google_project_id?: string | null;
  has_api_key: boolean;
  has_oauth: boolean;
  base_url: string | null;
  enabled: boolean;
  is_default: boolean;
  uses_oauth: boolean;
  auth_methods: AIProviderAuthMethod[];
  status: AIProviderStatus;
  /** Which backends this provider is used for (e.g., ["opencode", "claudecode"]) */
  use_for_backends: string[];
  created_at: string;
  updated_at: string;
}

export interface AIProviderAuthResponse {
  success: boolean;
  message: string;
  auth_url: string | null;
}

export interface OAuthAuthorizeResponse {
  url: string;
  instructions: string;
  method: "code" | "auto";
}

// List all AI providers
export async function listAIProviders(): Promise<AIProvider[]> {
  return apiGet("/api/ai/providers", "Failed to list AI providers");
}

// List available provider types
export async function listAIProviderTypes(): Promise<AIProviderTypeInfo[]> {
  return apiGet("/api/ai/providers/types", "Failed to list AI provider types");
}

// Get provider by ID
export async function getAIProvider(id: string): Promise<AIProvider> {
  return apiGet(`/api/ai/providers/${id}`, "Failed to get AI provider");
}

// Create new provider
export async function createAIProvider(data: {
  provider_type: AIProviderType;
  name: string;
  google_project_id?: string;
  api_key?: string;
  base_url?: string;
  enabled?: boolean;
  /** Which backends this provider is used for (e.g., ["opencode", "claudecode"]) */
  use_for_backends?: string[];
}): Promise<AIProvider> {
  return apiPost("/api/ai/providers", data, "Failed to create AI provider");
}

// Update provider
export async function updateAIProvider(
  id: string,
  data: {
    name?: string;
    google_project_id?: string | null;
    api_key?: string | null;
    base_url?: string | null;
    enabled?: boolean;
    /** Which backends this provider is used for (e.g., ["opencode", "claudecode"]) */
    use_for_backends?: string[];
  }
): Promise<AIProvider> {
  return apiPut(`/api/ai/providers/${id}`, data, "Failed to update AI provider");
}

// Delete provider
export async function deleteAIProvider(id: string): Promise<void> {
  return apiDel(`/api/ai/providers/${id}`, "Failed to delete AI provider");
}

// Provider credentials for a backend
export interface BackendProviderResponse {
  configured: boolean;
  provider_type: string | null;
  provider_name: string | null;
  api_key: string | null;
  oauth: {
    access_token: string;
    refresh_token: string;
    expires_at: number;
  } | null;
  has_credentials: boolean;
}

// Get provider credentials for a specific backend (e.g., "claudecode")
export async function getProviderForBackend(backendId: string): Promise<BackendProviderResponse> {
  return apiGet(`/api/ai/providers/for-backend/${backendId}`, "Failed to get provider for backend");
}

// Authenticate provider (initiate OAuth or check API key)
export async function authenticateAIProvider(id: string): Promise<AIProviderAuthResponse> {
  return apiPost(`/api/ai/providers/${id}/auth`, undefined, "Failed to authenticate AI provider");
}

// Set default provider
export async function setDefaultAIProvider(id: string): Promise<AIProvider> {
  return apiPost(`/api/ai/providers/${id}/default`, undefined, "Failed to set default AI provider");
}

// Get auth methods for a provider
export async function getAuthMethods(id: string): Promise<AIProviderAuthMethod[]> {
  return apiGet(`/api/ai/providers/${id}/auth/methods`, "Failed to get auth methods");
}

// Start OAuth authorization flow
export async function oauthAuthorize(id: string, methodIndex: number): Promise<OAuthAuthorizeResponse> {
  const res = await apiFetch(`/api/ai/providers/${id}/oauth/authorize`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ method_index: methodIndex }),
  });
  if (!res.ok) {
    const error = await res.text();
    throw new Error(error || "Failed to start OAuth authorization");
  }
  return res.json();
}

// Complete OAuth flow with authorization code
export async function oauthCallback(
  id: string,
  methodIndex: number,
  code: string,
  useForBackends?: string[]
): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}/oauth/callback`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      method_index: methodIndex,
      code,
      use_for_backends: useForBackends,
    }),
  });
  if (!res.ok) {
    const error = await res.text();
    throw new Error(error || "Failed to complete OAuth");
  }
  return res.json();
}

// ============================================================================
// Secrets API
// ============================================================================

export interface SecretsStatus {
  initialized: boolean;
  can_decrypt: boolean;
  registries: RegistryInfo[];
  default_key: string | null;
}

export interface RegistryInfo {
  name: string;
  description: string | null;
  secret_count: number;
  updated_at: string;
}

export interface SecretInfo {
  key: string;
  secret_type: 'oauth_access_token' | 'oauth_refresh_token' | 'api_key' | 'password' | 'generic' | null;
  expires_at: number | null;
  labels: Record<string, string>;
  is_expired: boolean;
}

export interface SecretMetadata {
  type?: 'oauth_access_token' | 'oauth_refresh_token' | 'api_key' | 'password' | 'generic';
  expires_at?: number;
  labels?: Record<string, string>;
}

// Get secrets status
export async function getSecretsStatus(): Promise<SecretsStatus> {
  return apiGet('/api/secrets/status', 'Failed to get secrets status');
}

// Initialize secrets system
export async function initializeSecrets(keyId: string = 'default'): Promise<{ key_id: string; message: string }> {
  return apiPost('/api/secrets/initialize', { key_id: keyId }, 'Failed to initialize secrets');
}

// Unlock secrets with passphrase
export async function unlockSecrets(passphrase: string): Promise<void> {
  const res = await apiFetch('/api/secrets/unlock', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ passphrase }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Invalid passphrase');
  }
}

// Lock secrets
export async function lockSecrets(): Promise<void> {
  return apiPost('/api/secrets/lock', undefined, 'Failed to lock secrets');
}

// List registries
export async function listSecretRegistries(): Promise<RegistryInfo[]> {
  return apiGet('/api/secrets/registries', 'Failed to list registries');
}

// List secrets in a registry
export async function listSecrets(registryName: string): Promise<SecretInfo[]> {
  return apiGet(`/api/secrets/registries/${encodeURIComponent(registryName)}`, 'Failed to list secrets');
}

// Get secret metadata (not the value)
export async function getSecretInfo(registryName: string, key: string): Promise<SecretInfo> {
  return apiGet(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}`, 'Failed to get secret info');
}

// Reveal (decrypt) a secret value
export async function revealSecret(registryName: string, key: string): Promise<string> {
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}/reveal`);
  if (!res.ok) {
    if (res.status === 401) throw new Error('Secrets are locked');
    throw new Error('Failed to reveal secret');
  }
  const data = await res.json();
  return data.value;
}

// Set a secret
export async function setSecret(
  registryName: string,
  key: string,
  value: string,
  metadata?: SecretMetadata
): Promise<void> {
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ value, metadata }),
  });
  if (!res.ok) {
    if (res.status === 401) throw new Error('Secrets are locked');
    throw new Error('Failed to set secret');
  }
}

// Delete a secret
export async function deleteSecret(registryName: string, key: string): Promise<void> {
  return apiDel(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}`, 'Failed to delete secret');
}

// Delete a registry
export async function deleteSecretRegistry(registryName: string): Promise<void> {
  return apiDel(`/api/secrets/registries/${encodeURIComponent(registryName)}`, 'Failed to delete registry');
}

// ============================================================
// Desktop Session Management
// ============================================================

export type DesktopSessionStatus = 'active' | 'orphaned' | 'stopped' | 'unknown';

export interface DesktopSessionDetail {
  display: string;
  status: DesktopSessionStatus;
  mission_id?: string;
  mission_title?: string;
  mission_status?: string;
  started_at: string;
  stopped_at?: string;
  keep_alive_until?: string;
  auto_close_in_secs?: number;
  process_running: boolean;
}

export interface ListSessionsResponse {
  sessions: DesktopSessionDetail[];
}

export interface OperationResponse {
  success: boolean;
  message?: string;
}

// List all desktop sessions
export async function listDesktopSessions(): Promise<DesktopSessionDetail[]> {
  const res = await apiFetch('/api/desktop/sessions');
  if (!res.ok) throw new Error('Failed to list desktop sessions');
  const data: ListSessionsResponse = await res.json();
  return data.sessions;
}

// Close a desktop session
export async function closeDesktopSession(display: string): Promise<OperationResponse> {
  // Remove leading colon for URL path
  const displayNum = display.replace(/^:/, '');
  const res = await apiFetch(`/api/desktop/sessions/:${displayNum}/close`, {
    method: 'POST',
  });
  if (!res.ok) {
    const err = await res.text();
    throw new Error(err || 'Failed to close desktop session');
  }
  return res.json();
}

// Extend keep-alive for a desktop session
export async function keepAliveDesktopSession(
  display: string,
  extensionSecs: number = 7200
): Promise<OperationResponse> {
  const displayNum = display.replace(/^:/, '');
  const res = await apiFetch(`/api/desktop/sessions/:${displayNum}/keep-alive`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ extension_secs: extensionSecs }),
  });
  if (!res.ok) {
    const err = await res.text();
    throw new Error(err || 'Failed to extend keep-alive');
  }
  return res.json();
}

// Close all orphaned desktop sessions
export async function cleanupOrphanedDesktopSessions(): Promise<OperationResponse> {
  return apiPost('/api/desktop/sessions/cleanup', undefined, 'Failed to cleanup orphaned sessions');
}

// ============================================
// System Components API
// ============================================

export type ComponentStatus = 'ok' | 'update_available' | 'not_installed' | 'error';

export interface ComponentInfo {
  name: string;
  version: string | null;
  installed: boolean;
  update_available: string | null;
  path: string | null;
  status: ComponentStatus;
}

export interface SystemComponentsResponse {
  components: ComponentInfo[];
}

export interface UpdateProgressEvent {
  event_type: 'log' | 'progress' | 'complete' | 'error';
  message: string;
  progress: number | null;
}

// Get all system components and their versions
export async function getSystemComponents(): Promise<SystemComponentsResponse> {
  return apiGet('/api/system/components', 'Failed to get system components');
}

// Update a system component (streams progress via SSE)
export async function updateSystemComponent(
  name: string,
  onProgress: (event: UpdateProgressEvent) => void,
  onComplete: () => void,
  onError: (error: string) => void
): Promise<void> {
  try {
    const res = await apiFetch(`/api/system/components/${name}/update`, {
      method: 'POST',
      headers: {
        'Accept': 'text/event-stream',
      },
    });

    if (!res.ok) {
      const text = await res.text();
      onError(text || 'Failed to start update');
      return;
    }

    if (!res.body) {
      onError('No response body');
      return;
    }

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });

      // Parse SSE events from buffer
      const lines = buffer.split('\n');
      buffer = lines.pop() || ''; // Keep incomplete line in buffer

      for (const line of lines) {
        if (line.startsWith('data: ')) {
          const jsonData = line.slice(6);
          try {
            const data: UpdateProgressEvent = JSON.parse(jsonData);
            onProgress(data);

            if (data.event_type === 'complete') {
              onComplete();
              return;
            } else if (data.event_type === 'error') {
              onError(data.message);
              return;
            }
          } catch (e) {
            console.error('Failed to parse SSE event:', e, jsonData);
          }
        }
      }
    }

    // Stream ended without explicit completion
    onComplete();
  } catch (e) {
    onError(e instanceof Error ? e.message : 'Unknown error');
  }
}

// ============================================
// Global Settings API
// ============================================

export interface SettingsResponse {
  library_remote: string | null;
}

export interface UpdateLibraryRemoteResponse {
  library_remote: string | null;
  library_reinitialized: boolean;
  library_error?: string;
}

// Get all settings
export async function getSettings(): Promise<SettingsResponse> {
  return apiGet('/api/settings', 'Failed to get settings');
}

// Update the library remote URL
export async function updateLibraryRemote(
  libraryRemote: string | null
): Promise<UpdateLibraryRemoteResponse> {
  const res = await apiFetch('/api/settings/library-remote', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ library_remote: libraryRemote }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to update library remote');
  }
  return res.json();
}

// ============================================
// Backends API
// ============================================

export interface Backend {
  id: string;
  name: string;
}

export interface BackendAgent {
  id: string;
  name: string;
}

export interface BackendConfig {
  id: string;
  name: string;
  enabled: boolean;
  settings: Record<string, unknown>;
}

// List all available backends
export async function listBackends(): Promise<Backend[]> {
  return apiGet('/api/backends', 'Failed to list backends');
}

// Get a specific backend
export async function getBackend(id: string): Promise<Backend> {
  return apiGet(`/api/backends/${encodeURIComponent(id)}`, 'Failed to get backend');
}

// List agents for a specific backend
export async function listBackendAgents(backendId: string): Promise<BackendAgent[]> {
  return apiGet(`/api/backends/${encodeURIComponent(backendId)}/agents`, 'Failed to list backend agents');
}

// Get backend configuration
export async function getBackendConfig(backendId: string): Promise<BackendConfig> {
  return apiGet(`/api/backends/${encodeURIComponent(backendId)}/config`, 'Failed to get backend config');
}

// Update backend configuration
export async function updateBackendConfig(
  backendId: string,
  settings: Record<string, unknown>,
  options?: { enabled?: boolean }
): Promise<{ ok: boolean; message?: string }> {
  return apiPut(
    `/api/backends/${encodeURIComponent(backendId)}/config`,
    { settings, enabled: options?.enabled },
    'Failed to update backend config',
  );
}

// ============================================
// Backup & Restore API
// ============================================

export interface RestoreBackupResponse {
  success: boolean;
  message: string;
  restored_files: string[];
  errors: string[];
}

// Download settings backup
export async function downloadBackup(): Promise<void> {
  const res = await apiFetch('/api/settings/backup');
  if (!res.ok) throw new Error('Failed to download backup');

  // Get filename from Content-Disposition header or use default
  const contentDisposition = res.headers.get('Content-Disposition');
  let filename = 'openagent-backup.zip';
  if (contentDisposition) {
    const match = contentDisposition.match(/filename="([^"]+)"/);
    if (match) filename = match[1];
  }

  // Convert response to blob and trigger download
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// Restore settings from backup file
export async function restoreBackup(file: File): Promise<RestoreBackupResponse> {
  const formData = new FormData();
  formData.append('backup', file);

  const res = await apiFetch('/api/settings/restore', {
    method: 'POST',
    body: formData,
  });

  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to restore backup');
  }

  return res.json();
}
