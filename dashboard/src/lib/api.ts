import { authHeader, clearJwt, signalAuthRequired } from "./auth";
import { getRuntimeApiBase, getRuntimeTaskDefaults } from "./settings";

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
}

export interface LoginResponse {
  token: string;
  exp: number;
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

export async function login(password: string): Promise<LoginResponse> {
  const res = await fetch(apiUrl("/api/auth/login"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
  });
  if (!res.ok) throw new Error("Failed to login");
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
  const defaults = getRuntimeTaskDefaults();
  const merged: CreateTaskRequest = {
    ...defaults,
    ...request,
  };
  const res = await apiFetch("/api/task", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(merged),
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

export type MissionStatus = "active" | "completed" | "failed";

export interface MissionHistoryEntry {
  role: string;
  content: string;
}

export interface Mission {
  id: string;
  status: MissionStatus;
  title: string | null;
  history: MissionHistoryEntry[];
  created_at: string;
  updated_at: string;
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
export async function createMission(): Promise<Mission> {
  const res = await apiFetch("/api/control/missions", { method: "POST" });
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
  model_override: string | null;
  state: "queued" | "running" | "waiting_for_tool" | "finished";
  queue_len: number;
  history_len: number;
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

// ==================== Global Control Session ====================

export type ControlRunState = "idle" | "running" | "waiting_for_tool";

export type ControlAgentEvent =
  | { type: "status"; state: ControlRunState; queue_len: number; mission_id?: string }
  | { type: "user_message"; id: string; content: string; mission_id?: string }
  | {
      type: "assistant_message";
      id: string;
      content: string;
      success: boolean;
      cost_cents: number;
      model: string | null;
      mission_id?: string;
    }
  | { type: "thinking"; content: string; done: boolean; mission_id?: string }
  | { type: "tool_call"; tool_call_id: string; name: string; args: unknown; mission_id?: string }
  | { type: "tool_result"; tool_call_id: string; name: string; result: unknown; mission_id?: string }
  | { type: "error"; message: string; mission_id?: string };

export async function postControlMessage(
  content: string
): Promise<{ id: string; queued: boolean }> {
  const res = await apiFetch("/api/control/message", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
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

export function streamControl(
  onEvent: (event: { type: string; data: unknown }) => void
): () => void {
  const controller = new AbortController();
  const decoder = new TextDecoder();
  let buffer = "";

  void (async () => {
    try {
      const res = await apiFetch("/api/control/stream", {
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
            // SSE comments (lines starting with :) are ignored for keepalive
          }

          if (!data) continue;
          try {
            onEvent({ type: eventType, data: JSON.parse(data) });
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
    } catch (err) {
      if (!controller.signal.aborted) {
        // Provide more specific error messages
        const errorMessage = err instanceof Error 
          ? `Stream connection failed: ${err.message}`
          : "Stream connection failed";
        onEvent({
          type: "error",
          data: { message: errorMessage },
        });
      }
    }
  })();

  return () => controller.abort();
}

// ==================== MCP Management ====================

export type McpStatus = "connected" | "disconnected" | "error" | "disabled";

export interface McpServerConfig {
  id: string;
  name: string;
  endpoint: string;
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
  source: "builtin" | { mcp: { id: string; name: string } };
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

// Upload a file to the remote filesystem
export async function uploadFile(
  file: File,
  remotePath: string = "/root/context/"
): Promise<UploadResult> {
  const formData = new FormData();
  formData.append("file", file);

  const url = apiUrl(`/api/fs/upload?path=${encodeURIComponent(remotePath)}`);
  const res = await fetch(url, {
    method: "POST",
    headers: authHeader(),
    body: formData,
  });

  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to upload file: ${text}`);
  }

  return res.json();
}
