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
  max_iterations: number;
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

export type MissionStatus = "active" | "completed" | "failed" | "interrupted" | "blocked" | "not_feasible";

export interface MissionHistoryEntry {
  role: string;
  content: string;
}

export interface Mission {
  id: string;
  status: MissionStatus;
  title: string | null;
  model_override: string | null;
  history: MissionHistoryEntry[];
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
export async function createMission(
  title?: string,
  modelOverride?: string
): Promise<Mission> {
  const body: { title?: string; model_override?: string } = {};
  if (title) body.title = title;
  if (modelOverride) body.model_override = modelOverride;

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
  model_override: string | null;
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

// Resume an interrupted mission
export async function resumeMission(id: string): Promise<Mission> {
  const res = await apiFetch(`/api/control/missions/${id}/resume`, {
    method: "POST",
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`Failed to resume mission: ${text}`);
  }
  return res.json();
}

// ==================== Global Control Session ====================

export type ControlRunState = "idle" | "running" | "waiting_for_tool";

export type ControlAgentEvent =
  | {
      type: "status";
      state: ControlRunState;
      queue_len: number;
      mission_id?: string;
    }
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
        const errorMessage =
          err instanceof Error
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

export interface UploadProgress {
  loaded: number;
  total: number;
  percentage: number;
}

// Upload a file to the remote filesystem with progress tracking
export function uploadFile(
  file: File,
  remotePath: string = "/root/context/",
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
  remotePath: string = "/root/context/",
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
  remotePath: string = "/root/context/",
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

// ==================== Models ====================

export interface ModelsResponse {
  models: string[];
  count: number;
}

// List available models
export async function listModels(tier?: string): Promise<ModelsResponse> {
  const params = tier ? `?tier=${encodeURIComponent(tier)}` : "";
  const res = await apiFetch(`/api/models${params}`);
  if (!res.ok) throw new Error("Failed to fetch models");
  return res.json();
}

// Friendly display names for models
const MODEL_DISPLAY_NAMES: Record<string, string> = {
  // OpenAI - simplified (newest first)
  "openai/gpt-5.2-pro": "gpt-5.2-pro",
  "openai/gpt-5.2": "gpt-5.2",
  "openai/gpt-5.2-chat": "gpt-5.2",
  "openai/gpt-4.1-mini": "gpt-4-mini",
  "openai/gpt-4.1": "gpt-4",
  "openai/o1": "o1",
  "openai/o3-mini-high": "o3-mini",
  // Anthropic - simplified
  "anthropic/claude-sonnet-4.5": "4.5-sonnet",
  "anthropic/claude-opus-4.5": "4.5-opus",
  "anthropic/claude-haiku-4.5": "4.5-haiku",
  // Google
  "google/gemini-3-flash-preview": "gemini-3-flash",
  "google/gemini-3-pro-image-preview": "gemini-3-pro",
  // DeepSeek
  "deepseek/deepseek-r1": "deepseek-r1",
  "deepseek/deepseek-chat-v3-0324": "deepseek-v3",
  // Qwen
  "qwen/qwq-32b": "qwq-32b",
  "qwen/qwen-2.5-72b-instruct": "qwen-72b",
  "qwen/qwen3-next-80b-a3b-thinking": "qwen3-thinking",
  // Mistral
  "mistralai/mistral-small-24b-instruct-2501": "mistral-small",
  "mistralai/mistral-medium-3.1": "mistral-medium",
  "mistralai/mistral-large-2512": "mistral-large",
  // Meta
  "meta-llama/llama-3.1-405b": "llama-405b",
  "meta-llama/llama-3.2-90b-vision-instruct": "llama-90b-vision",
  "meta-llama/llama-3.3-70b-instruct:free": "llama-70b (free)",
};

// Get display name for a model
export function getModelDisplayName(modelId: string): string {
  if (MODEL_DISPLAY_NAMES[modelId]) {
    return MODEL_DISPLAY_NAMES[modelId];
  }
  // Fallback: strip provider prefix
  return modelId.includes("/") ? modelId.split("/").pop()! : modelId;
}

// Model categories for sorting
const MODEL_CATEGORIES: Record<string, { order: number; label: string }> = {
  "google": { order: 1, label: "Google" },
  "deepseek": { order: 2, label: "DeepSeek" },
  "qwen": { order: 3, label: "Qwen" },
  "anthropic": { order: 4, label: "Anthropic" },
  "mistralai": { order: 5, label: "Mistral" },
  "openai": { order: 6, label: "OpenAI" },
};

// Models to exclude from the dropdown
const EXCLUDED_MODEL_PATTERNS = [
  /^meta-llama\//,      // All Llama models
  /^openai\/o[0-9]/,    // OpenAI o-series (o1, o3, o4, etc.)
];

// Filter and sort models for the dropdown
export function filterAndSortModels(models: string[]): string[] {
  return models
    // Filter out excluded models
    .filter(model => !EXCLUDED_MODEL_PATTERNS.some(pattern => pattern.test(model)))
    // Sort by category then alphabetically within category
    .sort((a, b) => {
      const providerA = a.split("/")[0];
      const providerB = b.split("/")[0];
      const catA = MODEL_CATEGORIES[providerA]?.order ?? 99;
      const catB = MODEL_CATEGORIES[providerB]?.order ?? 99;
      if (catA !== catB) return catA - catB;
      // Within same category, sort by display name
      return getModelDisplayName(a).localeCompare(getModelDisplayName(b));
    });
}

// Get the category label for a model
export function getModelCategory(modelId: string): string {
  const provider = modelId.split("/")[0];
  return MODEL_CATEGORIES[provider]?.label ?? "Other";
}
