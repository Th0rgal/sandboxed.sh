import { authHeader, clearJwt, signalAuthRequired } from "./auth";
import { getRuntimeApiBase, getRuntimeLibraryRemote } from "./settings";

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
  const libraryRemote = getRuntimeLibraryRemote();
  if (libraryRemote) {
    headers["x-openagent-library-remote"] = libraryRemote;
  }

  const res = await fetch(apiUrl(path), { ...init, headers });
  if (res.status === 401) {
    clearJwt();
    signalAuthRequired();
  }
  return res;
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
  const res = await apiFetch("/api/stats");
  if (!res.ok) throw new Error("Failed to fetch stats");
  return res.json();
}

// List all tasks
export async function listTasks(): Promise<TaskState[]> {
  const res = await apiFetch("/api/tasks");
  if (!res.ok) throw new Error("Failed to fetch tasks");
  return res.json();
}

// List OpenCode agents
export async function listOpenCodeAgents(): Promise<unknown> {
  const res = await apiFetch("/api/opencode/agents");
  if (!res.ok) throw new Error("Failed to fetch OpenCode agents");
  return res.json();
}

// Get a specific task
export async function getTask(id: string): Promise<TaskState> {
  const res = await apiFetch(`/api/task/${id}`);
  if (!res.ok) throw new Error("Failed to fetch task");
  return res.json();
}

// Create a new task
export async function createTask(
  request: CreateTaskRequest
): Promise<{ id: string; status: string }> {
  const res = await apiFetch("/api/task", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  if (!res.ok) throw new Error("Failed to create task");
  return res.json();
}

// Stop a task
export async function stopTask(id: string): Promise<void> {
  const res = await apiFetch(`/api/task/${id}/stop`, {
    method: "POST",
  });
  if (!res.ok) throw new Error("Failed to stop task");
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
  const res = await apiFetch(`/api/runs?limit=${limit}&offset=${offset}`);
  if (!res.ok) throw new Error("Failed to fetch runs");
  return res.json();
}

// Get run details
export async function getRun(id: string): Promise<Run> {
  const res = await apiFetch(`/api/runs/${id}`);
  if (!res.ok) throw new Error("Failed to fetch run");
  return res.json();
}

// Get run events
export async function getRunEvents(
  id: string,
  limit?: number
): Promise<{ run_id: string; events: unknown[] }> {
  const url = limit
    ? `/api/runs/${id}/events?limit=${limit}`
    : `/api/runs/${id}/events`;
  const res = await apiFetch(url);
  if (!res.ok) throw new Error("Failed to fetch run events");
  return res.json();
}

// Get run tasks
export async function getRunTasks(
  id: string
): Promise<{ run_id: string; tasks: unknown[] }> {
  const res = await apiFetch(`/api/runs/${id}/tasks`);
  if (!res.ok) throw new Error("Failed to fetch run tasks");
  return res.json();
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
  history: MissionHistoryEntry[];
  desktop_sessions?: DesktopSessionInfo[];
  created_at: string;
  updated_at: string;
  interrupted_at?: string;
  resumable?: boolean;
}

// List all missions
export async function listMissions(): Promise<Mission[]> {
  const res = await apiFetch("/api/control/missions");
  if (!res.ok) throw new Error("Failed to fetch missions");
  return res.json();
}

// Get a specific mission
export async function getMission(id: string): Promise<Mission> {
  const res = await apiFetch(`/api/control/missions/${id}`);
  if (!res.ok) throw new Error("Failed to fetch mission");
  return res.json();
}

// Get current mission
export async function getCurrentMission(): Promise<Mission | null> {
  const res = await apiFetch("/api/control/missions/current");
  if (!res.ok) throw new Error("Failed to fetch current mission");
  return res.json();
}

// Create a new mission
export interface CreateMissionOptions {
  title?: string;
  workspaceId?: string;
  /** Agent name from library (e.g., "code-reviewer") */
  agent?: string;
  /** Override model for this mission (provider/model) */
  modelOverride?: string;
}

export async function createMission(
  options?: CreateMissionOptions
): Promise<Mission> {
  const body: {
    title?: string;
    workspace_id?: string;
    agent?: string;
    model_override?: string;
  } = {};

  if (options?.title) body.title = options.title;
  if (options?.workspaceId) body.workspace_id = options.workspaceId;
  if (options?.agent) body.agent = options.agent;
  if (options?.modelOverride) body.model_override = options.modelOverride;

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
  const res = await apiFetch(`/api/control/missions/${id}/load`, {
    method: "POST",
  });
  if (!res.ok) throw new Error("Failed to load mission");
  return res.json();
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
  const res = await apiFetch("/api/control/running");
  if (!res.ok) throw new Error("Failed to fetch running missions");
  return res.json();
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
  const res = await apiFetch(`/api/control/missions/${missionId}/cancel`, {
    method: "POST",
  });
  if (!res.ok) throw new Error("Failed to cancel mission");
}

// Set mission status
export async function setMissionStatus(
  id: string,
  status: MissionStatus
): Promise<void> {
  const res = await apiFetch(`/api/control/missions/${id}/status`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ status }),
  });
  if (!res.ok) throw new Error("Failed to set mission status");
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
  options?: { agent?: string }
): Promise<{ id: string; queued: boolean }> {
  const body: { content: string; agent?: string } = { content };
  if (options?.agent) {
    body.agent = options.agent;
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
  const res = await apiFetch("/api/control/tool_result", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) throw new Error("Failed to post tool result");
}

export async function cancelControl(): Promise<void> {
  const res = await apiFetch("/api/control/cancel", { method: "POST" });
  if (!res.ok) throw new Error("Failed to cancel control session");
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
  const res = await apiFetch("/api/control/tree");
  if (!res.ok) throw new Error("Failed to fetch agent tree");
  return res.json();
}

// Get tree for a specific mission (either live from memory or saved from database)
export async function getMissionTree(
  missionId: string
): Promise<AgentTreeNode | null> {
  const res = await apiFetch(`/api/control/missions/${missionId}/tree`);
  if (!res.ok) throw new Error("Failed to fetch mission tree");
  return res.json();
}

// Execution progress
export interface ExecutionProgress {
  total_subtasks: number;
  completed_subtasks: number;
  current_subtask: string | null;
  current_depth: number;
}

export async function getProgress(): Promise<ExecutionProgress> {
  const res = await apiFetch("/api/control/progress");
  if (!res.ok) throw new Error("Failed to fetch progress");
  return res.json();
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
  const res = await apiFetch("/api/mcp");
  if (!res.ok) throw new Error("Failed to fetch MCPs");
  return res.json();
}

// Get a specific MCP server
export async function getMcp(id: string): Promise<McpServerState> {
  const res = await apiFetch(`/api/mcp/${id}`);
  if (!res.ok) throw new Error("Failed to fetch MCP");
  return res.json();
}

// Add a new MCP server
export async function addMcp(data: {
  name: string;
  endpoint: string;
  description?: string;
  scope?: McpScope;
}): Promise<McpServerState> {
  const res = await apiFetch("/api/mcp", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to add MCP");
  return res.json();
}

// Remove an MCP server
export async function removeMcp(id: string): Promise<void> {
  const res = await apiFetch(`/api/mcp/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error("Failed to remove MCP");
}

// Enable an MCP server
export async function enableMcp(id: string): Promise<McpServerState> {
  const res = await apiFetch(`/api/mcp/${id}/enable`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to enable MCP");
  return res.json();
}

// Disable an MCP server
export async function disableMcp(id: string): Promise<McpServerState> {
  const res = await apiFetch(`/api/mcp/${id}/disable`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to disable MCP");
  return res.json();
}

// Refresh an MCP server (reconnect and discover tools)
export async function refreshMcp(id: string): Promise<McpServerState> {
  const res = await apiFetch(`/api/mcp/${id}/refresh`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to refresh MCP");
  return res.json();
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
  const res = await apiFetch(`/api/mcp/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to update MCP");
  return res.json();
}

// Refresh all MCP servers
export async function refreshAllMcps(): Promise<void> {
  const res = await apiFetch("/api/mcp/refresh", { method: "POST" });
  if (!res.ok) throw new Error("Failed to refresh MCPs");
}

// List all tools
export async function listTools(): Promise<ToolInfo[]> {
  const res = await apiFetch("/api/tools");
  if (!res.ok) throw new Error("Failed to fetch tools");
  return res.json();
}

// Toggle a tool
export async function toggleTool(
  name: string,
  enabled: boolean
): Promise<void> {
  const res = await apiFetch(`/api/tools/${encodeURIComponent(name)}/toggle`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ enabled }),
  });
  if (!res.ok) throw new Error("Failed to toggle tool");
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
  onProgress?: (progress: UploadProgress) => void
): Promise<UploadResult> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const url = apiUrl(`/api/fs/upload?path=${encodeURIComponent(remotePath)}`);
    
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
  onProgress?: (progress: ChunkedUploadProgress) => void
): Promise<UploadResult> {
  const totalChunks = Math.ceil(file.size / CHUNK_SIZE);
  const uploadId = `${file.name}-${file.size}-${Date.now()}`;
  
  // For small files, use regular upload
  if (totalChunks <= 1) {
    return uploadFile(file, remotePath, onProgress ? (p) => onProgress({
      ...p,
      chunkIndex: 0,
      totalChunks: 1,
    }) : undefined);
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
        await uploadChunk(chunkFile, remotePath, uploadId, i, totalChunks);
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
  return finalizeChunkedUpload(remotePath, uploadId, file.name, totalChunks);
}

async function uploadChunk(
  chunk: File,
  remotePath: string,
  uploadId: string,
  chunkIndex: number,
  totalChunks: number
): Promise<void> {
  const formData = new FormData();
  formData.append("file", chunk);
  
  const params = new URLSearchParams({
    path: remotePath,
    upload_id: uploadId,
    chunk_index: String(chunkIndex),
    total_chunks: String(totalChunks),
  });
  
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
  totalChunks: number
): Promise<UploadResult> {
  const res = await apiFetch("/api/fs/upload-finalize", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      path: remotePath,
      upload_id: uploadId,
      file_name: fileName,
      total_chunks: totalChunks,
    }),
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
  fileName?: string
): Promise<UploadResult> {
  const res = await apiFetch("/api/fs/download-url", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      url,
      path: remotePath,
      file_name: fileName,
    }),
  });
  
  if (!res.ok) {
    throw new Error(`Failed to download from URL: ${await res.text()}`);
  }
  
  return res.json();
}

// Format bytes for display (handles up to petabyte scale)
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  if (bytes === 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  
  const units = ["KB", "MB", "GB", "TB", "PB"] as const;
  let value = bytes / 1024;
  let unitIndex = 0;
  
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex++;
  }
  
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unitIndex]}`;
}

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
export async function listProviders(): Promise<ProvidersResponse> {
  const res = await apiFetch("/api/providers");
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

export interface CommandSummary {
  name: string;
  description: string | null;
  path: string;
}

export interface Command {
  name: string;
  description: string | null;
  path: string;
  content: string;
}

// Git status
export async function getLibraryStatus(): Promise<LibraryStatus> {
  const res = await apiFetch("/api/library/status");
  await ensureLibraryResponse(res, "Failed to fetch library status");
  return res.json();
}

// Sync (git pull)
export async function syncLibrary(): Promise<void> {
  const res = await apiFetch("/api/library/sync", { method: "POST" });
  await ensureLibraryResponse(res, "Failed to sync library");
}

// Commit changes
export async function commitLibrary(message: string): Promise<void> {
  const res = await apiFetch("/api/library/commit", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message }),
  });
  await ensureLibraryResponse(res, "Failed to commit library");
}

// Push changes
export async function pushLibrary(): Promise<void> {
  const res = await apiFetch("/api/library/push", { method: "POST" });
  await ensureLibraryResponse(res, "Failed to push library");
}

// Get MCP servers
export async function getLibraryMcps(): Promise<Record<string, McpServerDef>> {
  const res = await apiFetch("/api/library/mcps");
  await ensureLibraryResponse(res, "Failed to fetch MCPs");
  return res.json();
}

// Save MCP servers
export async function saveLibraryMcps(
  servers: Record<string, McpServerDef>
): Promise<void> {
  const res = await apiFetch("/api/library/mcps", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(servers),
  });
  await ensureLibraryResponse(res, "Failed to save MCPs");
}

// List skills
export async function listLibrarySkills(): Promise<SkillSummary[]> {
  const res = await apiFetch("/api/library/skills");
  await ensureLibraryResponse(res, "Failed to fetch skills");
  return res.json();
}

// Get skill
export async function getLibrarySkill(name: string): Promise<Skill> {
  const res = await apiFetch(`/api/library/skills/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch skill");
  return res.json();
}

// Save skill
export async function saveLibrarySkill(
  name: string,
  content: string
): Promise<void> {
  const res = await apiFetch(`/api/library/skills/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  await ensureLibraryResponse(res, "Failed to save skill");
}

// Delete skill
export async function deleteLibrarySkill(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/skills/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete skill");
}

// Get skill reference file
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
  const res = await apiFetch(
    `/api/library/skills/${encodeURIComponent(skillName)}/references/${refPath}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content }),
    }
  );
  await ensureLibraryResponse(res, "Failed to save reference file");
}

// Delete skill reference file
export async function deleteSkillReference(
  skillName: string,
  refPath: string
): Promise<void> {
  const res = await apiFetch(
    `/api/library/skills/${encodeURIComponent(skillName)}/references/${refPath}`,
    { method: "DELETE" }
  );
  await ensureLibraryResponse(res, "Failed to delete reference file");
}

// Import skill from Git URL
export interface ImportSkillRequest {
  url: string;
  path?: string;
  name?: string;
}

export async function importSkill(request: ImportSkillRequest): Promise<Skill> {
  const res = await apiFetch("/api/library/skills/import", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  await ensureLibraryResponse(res, "Failed to import skill");
  return res.json();
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
  const res = await apiFetch("/api/library/commands");
  await ensureLibraryResponse(res, "Failed to fetch commands");
  return res.json();
}

// Get command
export async function getLibraryCommand(name: string): Promise<Command> {
  const res = await apiFetch(`/api/library/commands/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch command");
  return res.json();
}

// Save command
export async function saveLibraryCommand(
  name: string,
  content: string
): Promise<void> {
  const res = await apiFetch(`/api/library/commands/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  await ensureLibraryResponse(res, "Failed to save command");
}

// Delete command
export async function deleteLibraryCommand(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/commands/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete command");
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugins
// ─────────────────────────────────────────────────────────────────────────────

// Get all plugins
export async function getLibraryPlugins(): Promise<Record<string, Plugin>> {
  const res = await apiFetch("/api/library/plugins");
  await ensureLibraryResponse(res, "Failed to fetch plugins");
  return res.json();
}

// Save all plugins
export async function saveLibraryPlugins(
  plugins: Record<string, Plugin>
): Promise<void> {
  const res = await apiFetch("/api/library/plugins", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(plugins),
  });
  await ensureLibraryResponse(res, "Failed to save plugins");
}

// ─────────────────────────────────────────────────────────────────────────────
// Rules
// ─────────────────────────────────────────────────────────────────────────────

// List rules
export async function listLibraryRules(): Promise<RuleSummary[]> {
  const res = await apiFetch("/api/library/rule");
  await ensureLibraryResponse(res, "Failed to fetch rules");
  return res.json();
}

// Get rule
export async function getLibraryRule(name: string): Promise<Rule> {
  const res = await apiFetch(`/api/library/rule/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch rule");
  return res.json();
}

// Save rule
export async function saveLibraryRule(
  name: string,
  content: string
): Promise<void> {
  const res = await apiFetch(`/api/library/rule/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  await ensureLibraryResponse(res, "Failed to save rule");
}

// Delete rule
export async function deleteLibraryRule(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/rule/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete rule");
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Agents
// ─────────────────────────────────────────────────────────────────────────────

// List library agents
export async function listLibraryAgents(): Promise<LibraryAgentSummary[]> {
  const res = await apiFetch("/api/library/agent");
  await ensureLibraryResponse(res, "Failed to fetch library agents");
  return res.json();
}

// Get library agent
export async function getLibraryAgent(name: string): Promise<LibraryAgent> {
  const res = await apiFetch(`/api/library/agent/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch library agent");
  return res.json();
}

// Save library agent
export async function saveLibraryAgent(
  name: string,
  agent: LibraryAgent
): Promise<void> {
  const res = await apiFetch(`/api/library/agent/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(agent),
  });
  await ensureLibraryResponse(res, "Failed to save library agent");
}

// Delete library agent
export async function deleteLibraryAgent(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/agent/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete library agent");
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Tools
// ─────────────────────────────────────────────────────────────────────────────

// List library tools
export async function listLibraryTools(): Promise<LibraryToolSummary[]> {
  const res = await apiFetch("/api/library/tool");
  await ensureLibraryResponse(res, "Failed to fetch library tools");
  return res.json();
}

// Get library tool
export async function getLibraryTool(name: string): Promise<LibraryTool> {
  const res = await apiFetch(`/api/library/tool/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch library tool");
  return res.json();
}

// Save library tool
export async function saveLibraryTool(
  name: string,
  content: string
): Promise<void> {
  const res = await apiFetch(`/api/library/tool/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  await ensureLibraryResponse(res, "Failed to save library tool");
}

// Delete library tool
export async function deleteLibraryTool(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/tool/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete library tool");
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
  init_script: string;
}

export async function listWorkspaceTemplates(): Promise<WorkspaceTemplateSummary[]> {
  const res = await apiFetch("/api/library/workspace-template");
  await ensureLibraryResponse(res, "Failed to fetch workspace templates");
  return res.json();
}

export async function getWorkspaceTemplate(name: string): Promise<WorkspaceTemplate> {
  const res = await apiFetch(`/api/library/workspace-template/${encodeURIComponent(name)}`);
  await ensureLibraryResponse(res, "Failed to fetch workspace template");
  return res.json();
}

export async function saveWorkspaceTemplate(
  name: string,
  data: {
    description?: string;
    distro?: string;
    skills?: string[];
    env_vars?: Record<string, string>;
    init_script?: string;
  }
): Promise<void> {
  const res = await apiFetch(`/api/library/workspace-template/${encodeURIComponent(name)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  await ensureLibraryResponse(res, "Failed to save workspace template");
}

export async function deleteWorkspaceTemplate(name: string): Promise<void> {
  const res = await apiFetch(`/api/library/workspace-template/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  await ensureLibraryResponse(res, "Failed to delete workspace template");
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
    init_script: template.init_script,
  });
  // Delete old template
  await deleteWorkspaceTemplate(oldName);
}

// ─────────────────────────────────────────────────────────────────────────────
// Library Migration
// ─────────────────────────────────────────────────────────────────────────────

// Migrate library structure to new format
export async function migrateLibrary(): Promise<MigrationReport> {
  const res = await apiFetch("/api/library/migrate", { method: "POST" });
  await ensureLibraryResponse(res, "Failed to migrate library");
  return res.json();
}

// ==================== Workspaces ====================

export type WorkspaceType = "host" | "chroot";
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
}

// List workspaces
export async function listWorkspaces(): Promise<Workspace[]> {
  const res = await apiFetch("/api/workspaces");
  if (!res.ok) throw new Error("Failed to fetch workspaces");
  return res.json();
}

// Get workspace
export async function getWorkspace(id: string): Promise<Workspace> {
  const res = await apiFetch(`/api/workspaces/${id}`);
  if (!res.ok) throw new Error("Failed to fetch workspace");
  return res.json();
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
}): Promise<Workspace> {
  const res = await apiFetch("/api/workspaces", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to create workspace");
  return res.json();
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
  }
): Promise<Workspace> {
  const res = await apiFetch(`/api/workspaces/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to update workspace");
  return res.json();
}

// Sync workspace skills
export async function syncWorkspace(id: string): Promise<Workspace> {
  const res = await apiFetch(`/api/workspaces/${id}/sync`, {
    method: "POST",
  });
  if (!res.ok) throw new Error("Failed to sync workspace");
  return res.json();
}

// Delete workspace
export async function deleteWorkspace(id: string): Promise<void> {
  const res = await apiFetch(`/api/workspaces/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error("Failed to delete workspace");
}

// Supported Linux distributions for chroot workspaces
export type ChrootDistro =
  | "ubuntu-noble"
  | "ubuntu-jammy"
  | "debian-bookworm"
  | "arch-linux";

export const CHROOT_DISTROS: { value: ChrootDistro; label: string }[] = [
  { value: "ubuntu-noble", label: "Ubuntu 24.04 LTS (Noble)" },
  { value: "ubuntu-jammy", label: "Ubuntu 22.04 LTS (Jammy)" },
  { value: "debian-bookworm", label: "Debian 12 (Bookworm)" },
  { value: "arch-linux", label: "Arch Linux (Base)" },
];

// Build a chroot workspace
export async function buildWorkspace(
  id: string,
  distro?: ChrootDistro,
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
  const res = await apiFetch("/api/opencode/connections");
  if (!res.ok) throw new Error("Failed to list OpenCode connections");
  return res.json();
}

// Get connection by ID
export async function getOpenCodeConnection(id: string): Promise<OpenCodeConnection> {
  const res = await apiFetch(`/api/opencode/connections/${id}`);
  if (!res.ok) throw new Error("Failed to get OpenCode connection");
  return res.json();
}

// Create new connection
export async function createOpenCodeConnection(data: {
  name: string;
  base_url: string;
  agent?: string | null;
  permissive?: boolean;
  enabled?: boolean;
}): Promise<OpenCodeConnection> {
  const res = await apiFetch("/api/opencode/connections", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to create OpenCode connection");
  return res.json();
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
  const res = await apiFetch(`/api/opencode/connections/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to update OpenCode connection");
  return res.json();
}

// Delete connection
export async function deleteOpenCodeConnection(id: string): Promise<void> {
  const res = await apiFetch(`/api/opencode/connections/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error("Failed to delete OpenCode connection");
}

// Test connection
export async function testOpenCodeConnection(id: string): Promise<TestConnectionResponse> {
  const res = await apiFetch(`/api/opencode/connections/${id}/test`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to test OpenCode connection");
  return res.json();
}

// Set default connection
export async function setDefaultOpenCodeConnection(id: string): Promise<OpenCodeConnection> {
  const res = await apiFetch(`/api/opencode/connections/${id}/default`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to set default OpenCodeconnection");
  return res.json();
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Settings API (oh-my-opencode.json)
// ─────────────────────────────────────────────────────────────────────────────

// Get OpenCode settings (oh-my-opencode.json)
export async function getOpenCodeSettings(): Promise<Record<string, unknown>> {
  const res = await apiFetch("/api/opencode/settings");
  if (!res.ok) throw new Error("Failed to get OpenCode settings");
  return res.json();
}

// Update OpenCode settings (oh-my-opencode.json)
export async function updateOpenCodeSettings(settings: Record<string, unknown>): Promise<Record<string, unknown>> {
  const res = await apiFetch("/api/opencode/settings", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new Error("Failed to update OpenCode settings");
  return res.json();
}

// Restart OpenCode service (to apply settings changes)
export async function restartOpenCodeService(): Promise<{ success: boolean; message: string }> {
  const res = await apiFetch("/api/opencode/restart", { method: "POST" });
  if (!res.ok) throw new Error("Failed to restart OpenCode service");
  return res.json();
}

// ─────────────────────────────────────────────────────────────────────────────
// Library-backed OpenCode Settings API
// ─────────────────────────────────────────────────────────────────────────────

// Get OpenCode settings from Library (oh-my-opencode.json)
export async function getLibraryOpenCodeSettings(): Promise<Record<string, unknown>> {
  const res = await apiFetch("/api/library/opencode/settings");
  if (!res.ok) throw new Error("Failed to get Library OpenCode settings");
  return res.json();
}

// Save OpenCode settings to Library and sync to system
export async function saveLibraryOpenCodeSettings(settings: Record<string, unknown>): Promise<void> {
  const res = await apiFetch("/api/library/opencode/settings", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(settings),
  });
  if (!res.ok) throw new Error("Failed to save Library OpenCode settings");
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
  const res = await apiFetch("/api/library/openagent/config");
  if (!res.ok) throw new Error("Failed to get OpenAgent config");
  return res.json();
}

// Save OpenAgent config to Library
export async function saveOpenAgentConfig(config: OpenAgentConfig): Promise<void> {
  const res = await apiFetch("/api/library/openagent/config", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(config),
  });
  if (!res.ok) throw new Error("Failed to save OpenAgent config");
}

// Get visible agents (filtered by OpenAgent config)
export async function getVisibleAgents(): Promise<unknown> {
  const res = await apiFetch("/api/library/openagent/agents");
  if (!res.ok) throw new Error("Failed to get visible agents");
  return res.json();
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
  has_api_key: boolean;
  has_oauth: boolean;
  base_url: string | null;
  enabled: boolean;
  is_default: boolean;
  uses_oauth: boolean;
  auth_methods: AIProviderAuthMethod[];
  status: AIProviderStatus;
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
  const res = await apiFetch("/api/ai/providers");
  if (!res.ok) throw new Error("Failed to list AI providers");
  return res.json();
}

// List available provider types
export async function listAIProviderTypes(): Promise<AIProviderTypeInfo[]> {
  const res = await apiFetch("/api/ai/providers/types");
  if (!res.ok) throw new Error("Failed to list AI provider types");
  return res.json();
}

// Get provider by ID
export async function getAIProvider(id: string): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}`);
  if (!res.ok) throw new Error("Failed to get AI provider");
  return res.json();
}

// Create new provider
export async function createAIProvider(data: {
  provider_type: AIProviderType;
  name: string;
  api_key?: string;
  base_url?: string;
  enabled?: boolean;
}): Promise<AIProvider> {
  const res = await apiFetch("/api/ai/providers", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to create AI provider");
  return res.json();
}

// Update provider
export async function updateAIProvider(
  id: string,
  data: {
    name?: string;
    api_key?: string | null;
    base_url?: string | null;
    enabled?: boolean;
  }
): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw new Error("Failed to update AI provider");
  return res.json();
}

// Delete provider
export async function deleteAIProvider(id: string): Promise<void> {
  const res = await apiFetch(`/api/ai/providers/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error("Failed to delete AI provider");
}

// Authenticate provider (initiate OAuth or check API key)
export async function authenticateAIProvider(id: string): Promise<AIProviderAuthResponse> {
  const res = await apiFetch(`/api/ai/providers/${id}/auth`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to authenticate AI provider");
  return res.json();
}

// Set default provider
export async function setDefaultAIProvider(id: string): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}/default`, { method: "POST" });
  if (!res.ok) throw new Error("Failed to set default AI provider");
  return res.json();
}

// Get auth methods for a provider
export async function getAuthMethods(id: string): Promise<AIProviderAuthMethod[]> {
  const res = await apiFetch(`/api/ai/providers/${id}/auth/methods`);
  if (!res.ok) throw new Error("Failed to get auth methods");
  return res.json();
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
export async function oauthCallback(id: string, methodIndex: number, code: string): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}/oauth/callback`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ method_index: methodIndex, code }),
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
  const res = await apiFetch('/api/secrets/status');
  if (!res.ok) throw new Error('Failed to get secrets status');
  return res.json();
}

// Initialize secrets system
export async function initializeSecrets(keyId: string = 'default'): Promise<{ key_id: string; message: string }> {
  const res = await apiFetch('/api/secrets/initialize', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ key_id: keyId }),
  });
  if (!res.ok) throw new Error('Failed to initialize secrets');
  return res.json();
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
  const res = await apiFetch('/api/secrets/lock', { method: 'POST' });
  if (!res.ok) throw new Error('Failed to lock secrets');
}

// List registries
export async function listSecretRegistries(): Promise<RegistryInfo[]> {
  const res = await apiFetch('/api/secrets/registries');
  if (!res.ok) throw new Error('Failed to list registries');
  return res.json();
}

// List secrets in a registry
export async function listSecrets(registryName: string): Promise<SecretInfo[]> {
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}`);
  if (!res.ok) throw new Error('Failed to list secrets');
  return res.json();
}

// Get secret metadata (not the value)
export async function getSecretInfo(registryName: string, key: string): Promise<SecretInfo> {
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}`);
  if (!res.ok) throw new Error('Failed to get secret info');
  return res.json();
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
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}/${encodeURIComponent(key)}`, {
    method: 'DELETE',
  });
  if (!res.ok) throw new Error('Failed to delete secret');
}

// Delete a registry
export async function deleteSecretRegistry(registryName: string): Promise<void> {
  const res = await apiFetch(`/api/secrets/registries/${encodeURIComponent(registryName)}`, {
    method: 'DELETE',
  });
  if (!res.ok) throw new Error('Failed to delete registry');
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
  const res = await apiFetch('/api/desktop/sessions/cleanup', {
    method: 'POST',
  });
  if (!res.ok) throw new Error('Failed to cleanup orphaned sessions');
  return res.json();
}
