/**
 * Main API module - re-exports from split modules for backward compatibility.
 * 
 * New code should import from specific modules when possible:
 * - Core utilities: @/lib/api/core
 * - Missions: @/lib/api/missions
 * - Workspaces: @/lib/api/workspaces
 * - Providers: @/lib/api/providers
 */

import { authHeader } from "./auth";

// Re-export from split modules
export * from "./api/core";
export * from "./api/missions";
export * from "./api/workspaces";
export * from "./api/providers";
export * from "./api/automations";

// Import core utilities for use in this file (remaining APIs not yet split)
import {
  apiUrl,
  apiFetch,
  apiGet,
  apiPost,
  apiPut,
  apiPatch,
  apiDel,
  libGet,
  libPost,
  libPut,
  libDel,
  ensureLibraryResponse,
} from "./api/core";

// Types that remain in this file (not yet migrated to modules)
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

// ==================== Auth Management ====================

export interface AuthStatusResponse {
  auth_mode: "disabled" | "single_tenant" | "multi_user";
  password_source: "dashboard" | "environment" | "none";
  password_changed_at: string | null;
  dev_mode: boolean;
}

export async function getAuthStatus(): Promise<AuthStatusResponse> {
  return apiGet("/api/auth/status", "Failed to fetch auth status");
}

export interface ChangePasswordRequest {
  current_password?: string;
  new_password: string;
}

export interface ChangePasswordResponse {
  success: boolean;
  password_changed_at: string;
}

export async function changePassword(
  request: ChangePasswordRequest
): Promise<ChangePasswordResponse> {
  return apiPost("/api/auth/change-password", request, "Failed to change password");
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
  mission_id: string | null;
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

// BackendModelOption, BackendModelOptionsResponse, listProviders, and listBackendModelOptions
// are now exported from ./api/providers (see line 17)

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

// Skill source/provenance - local or from skills.sh registry
export type SkillSource =
  | { type: "Local" }
  | {
      type: "SkillsRegistry";
      identifier: string;
      skill_name?: string;
      version?: string;
      installed_at?: string;
      updated_at?: string;
    };

export interface SkillSummary {
  name: string;
  description: string | null;
  path: string;
  source?: SkillSource;
}

export interface Skill {
  name: string;
  description: string | null;
  path: string;
  source?: SkillSource;
  content: string;
  files: SkillFile[];
  references: string[];
}

// Skills registry (skills.sh) types
export interface RegistrySkillListing {
  identifier: string;
  name: string;
  description: string | null;
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

// Error class for diverged git history
export class DivergedHistoryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DivergedHistoryError";
  }
}

// Sync (git pull)
// Throws DivergedHistoryError if local and remote histories have diverged
export async function syncLibrary(): Promise<void> {
  const res = await apiFetch("/api/library/sync", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
  });

  if (!res.ok) {
    const text = await res.text().catch(() => "");
    // Check for diverged history error (409 Conflict with DIVERGED_HISTORY prefix)
    if (res.status === 409 && text.includes("DIVERGED_HISTORY")) {
      throw new DivergedHistoryError(text.replace("DIVERGED_HISTORY: ", ""));
    }
    throw new Error(text || "Failed to sync library");
  }
}

// Force sync (reset local to remote)
// Use this when histories have diverged after a force push
export async function forceSyncLibrary(): Promise<void> {
  return libPost("/api/library/force-sync", undefined, "Failed to force sync library");
}

// Force push (overwrite remote with local)
// Use this when you want to keep local changes and overwrite remote
export async function forcePushLibrary(): Promise<void> {
  return libPost("/api/library/force-push", undefined, "Failed to force push library");
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

// Import skill from file (.zip or .md)
export async function importSkill(name: string, file: File): Promise<Skill> {
  const formData = new FormData();
  formData.append("file", file);

  const res = await apiFetch(`/api/library/skills/import?name=${encodeURIComponent(name)}`, {
    method: "POST",
    body: formData,
  });

  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "Failed to import skill");
  }

  return res.json();
}

// Skills Registry (skills.sh) API

export async function searchSkillsRegistry(query: string): Promise<RegistrySkillListing[]> {
  return libGet(
    `/api/library/skill/registry/search?q=${encodeURIComponent(query)}`,
    "Failed to search skills registry"
  );
}

export async function listRepoSkills(identifier: string): Promise<string[]> {
  return libGet(
    `/api/library/skill/registry/list/${encodeURIComponent(identifier)}`,
    "Failed to list repo skills"
  );
}

export interface InstallFromRegistryRequest {
  identifier: string;
  skills?: string[];
  name?: string;
}

export async function installFromRegistry(request: InstallFromRegistryRequest): Promise<Skill> {
  return libPost("/api/library/skill/registry/install", request, "Failed to install from registry");
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
// Workspace Templates
// ─────────────────────────────────────────────────────────────────────────────

export interface WorkspaceTemplateSummary {
  name: string;
  description?: string;
  path: string;
  distro?: string;
  skills?: string[];
  init_scripts?: string[];
}

export type TailscaleMode = "exit_node" | "tailnet_only";

export interface WorkspaceTemplate {
  name: string;
  description?: string;
  path: string;
  distro?: string;
  skills: string[];
  env_vars: Record<string, string>;
  encrypted_keys: string[];
  init_scripts: string[];
  init_script: string;
  shared_network?: boolean | null;
  tailscale_mode?: TailscaleMode | null;
  config_profile?: string;
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
    init_scripts?: string[];
    init_script?: string;
    shared_network?: boolean | null;
    tailscale_mode?: TailscaleMode | null;
    config_profile?: string;
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
    init_scripts: template.init_scripts,
    init_script: template.init_script,
    shared_network: template.shared_network,
    tailscale_mode: template.tailscale_mode,
    config_profile: template.config_profile,
  });
  // Delete old template
  await deleteWorkspaceTemplate(oldName);
}

// ─────────────────────────────────────────────────────────────────────────────
// Init Scripts
// ─────────────────────────────────────────────────────────────────────────────

export interface InitScriptSummary {
  name: string;
  description?: string | null;
  path: string;
}

export interface InitScript extends InitScriptSummary {
  content: string;
}

export async function listInitScripts(): Promise<InitScriptSummary[]> {
  return libGet("/api/library/init-script", "Failed to fetch init scripts");
}

export async function getInitScript(name: string): Promise<InitScript> {
  return libGet(`/api/library/init-script/${encodeURIComponent(name)}`, "Failed to fetch init script");
}

export async function saveInitScript(name: string, content: string): Promise<void> {
  return libPut(`/api/library/init-script/${encodeURIComponent(name)}`, { content }, "Failed to save init script");
}

export async function deleteInitScript(name: string): Promise<void> {
  return libDel(`/api/library/init-script/${encodeURIComponent(name)}`, "Failed to delete init script");
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

// ─────────────────────────────────────────────────────────────────────────────
// OpenCode Config API (opencode.json)
// ─────────────────────────────────────────────────────────────────────────────

// Get OpenCode config (opencode.json)
export async function getOpenCodeConfig(): Promise<Record<string, unknown>> {
  return apiGet("/api/opencode/config", "Failed to get OpenCode config");
}

// Update OpenCode config (opencode.json)
export async function updateOpenCodeConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return apiPut("/api/opencode/config", config, "Failed to update OpenCode config");
}

// ─────────────────────────────────────────────────────────────────────────────
// Claude Code Host Config API
// ─────────────────────────────────────────────────────────────────────────────

export async function getClaudeCodeHostConfig(): Promise<Record<string, unknown>> {
  return apiGet("/api/claudecode/config", "Failed to get Claude Code host config");
}

export async function updateClaudeCodeHostConfig(
  config: Record<string, unknown>
): Promise<Record<string, unknown>> {
  return apiPut("/api/claudecode/config", config, "Failed to update Claude Code host config");
}

// ─────────────────────────────────────────────────────────────────────────────
// Amp Host Config API
// ─────────────────────────────────────────────────────────────────────────────

export async function getAmpCodeHostConfig(): Promise<Record<string, unknown>> {
  return apiGet("/api/amp/config", "Failed to get Amp host config");
}

export async function updateAmpCodeHostConfig(
  config: Record<string, unknown>
): Promise<Record<string, unknown>> {
  return apiPut("/api/amp/config", config, "Failed to update Amp host config");
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
  try {
    return await apiGet("/api/library/sandboxed-sh/config", "Failed to get OpenAgent config");
  } catch {
    // Return default config if endpoint doesn't exist (not yet implemented)
    return { hidden_agents: [], default_agent: null };
  }
}

// Save OpenAgent config to Library
export async function saveOpenAgentConfig(config: OpenAgentConfig): Promise<void> {
  return apiPut("/api/library/sandboxed-sh/config", config, "Failed to save OpenAgent config");
}

// Get visible agents (filtered by OpenAgent config)
export async function getVisibleAgents(): Promise<unknown> {
  try {
    return await apiGet("/api/library/sandboxed-sh/agents", "Failed to get visible agents");
  } catch {
    // Return empty array if endpoint doesn't exist (not yet implemented)
    return [];
  }
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
// Config Profiles API
// ─────────────────────────────────────────────────────────────────────────────

export interface ConfigProfileSummary {
  name: string;
  is_default: boolean;
  path: string;
}

export interface ConfigProfileFile {
  path: string;
  content: string;
}

export interface AmpCodeConfig {
  default_mode?: string | null;
}

export interface ConfigProfile {
  name: string;
  is_default: boolean;
  path: string;
  files: ConfigProfileFile[];
  opencode_settings: Record<string, unknown>;
  openagent_config: OpenAgentConfig;
  claudecode_config: ClaudeCodeConfig;
  ampcode_config: AmpCodeConfig;
}

// List all config profiles
export async function listConfigProfiles(): Promise<ConfigProfileSummary[]> {
  return apiGet("/api/library/config-profile", "Failed to list config profiles");
}

// Create a new config profile
export async function createConfigProfile(
  name: string,
  baseProfile?: string
): Promise<ConfigProfile> {
  return apiPost(
    "/api/library/config-profile",
    { name, base_profile: baseProfile },
    "Failed to create config profile"
  );
}

// Get a config profile by name
export async function getConfigProfile(name: string): Promise<ConfigProfile> {
  return apiGet(`/api/library/config-profile/${encodeURIComponent(name)}`, "Failed to get config profile");
}

// Save a config profile
export async function saveConfigProfile(name: string, profile: ConfigProfile): Promise<void> {
  return apiPut(`/api/library/config-profile/${encodeURIComponent(name)}`, profile, "Failed to save config profile");
}

// Delete a config profile
export async function deleteConfigProfile(name: string): Promise<void> {
  return apiDel(`/api/library/config-profile/${encodeURIComponent(name)}`, "Failed to delete config profile");
}

// Get OpenCode settings for a specific profile
export async function getLibraryOpenCodeSettingsForProfile(profile: string): Promise<Record<string, unknown>> {
  return apiGet(
    `/api/library/config-profile/${encodeURIComponent(profile)}/opencode/settings`,
    "Failed to get OpenCode settings for profile"
  );
}

// Save OpenCode settings for a specific profile
export async function saveLibraryOpenCodeSettingsForProfile(
  profile: string,
  settings: Record<string, unknown>
): Promise<void> {
  return apiPut(
    `/api/library/config-profile/${encodeURIComponent(profile)}/opencode/settings`,
    settings,
    "Failed to save OpenCode settings for profile"
  );
}

// Get OpenAgent config for a specific profile
export async function getOpenAgentConfigForProfile(profile: string): Promise<OpenAgentConfig> {
  try {
    return await apiGet(
      `/api/library/config-profile/${encodeURIComponent(profile)}/sandboxed-sh/config`,
      "Failed to get OpenAgent config for profile"
    );
  } catch {
    // Return default config if endpoint doesn't exist (not yet implemented)
    return { hidden_agents: [], default_agent: null };
  }
}

// Save OpenAgent config for a specific profile
export async function saveOpenAgentConfigForProfile(
  profile: string,
  config: OpenAgentConfig
): Promise<void> {
  return apiPut(
    `/api/library/config-profile/${encodeURIComponent(profile)}/sandboxed-sh/config`,
    config,
    "Failed to save OpenAgent config for profile"
  );
}

// Get Claude Code config for a specific profile
export async function getClaudeCodeConfigForProfile(profile: string): Promise<ClaudeCodeConfig> {
  return apiGet(
    `/api/library/config-profile/${encodeURIComponent(profile)}/claudecode/config`,
    "Failed to get Claude Code config for profile"
  );
}

// Save Claude Code config for a specific profile
export async function saveClaudeCodeConfigForProfile(
  profile: string,
  config: ClaudeCodeConfig
): Promise<void> {
  return apiPut(
    `/api/library/config-profile/${encodeURIComponent(profile)}/claudecode/config`,
    config,
    "Failed to save Claude Code config for profile"
  );
}

// Get Amp Code config for a specific profile
export async function getAmpCodeConfigForProfile(profile: string): Promise<AmpCodeConfig> {
  return apiGet(
    `/api/library/config-profile/${encodeURIComponent(profile)}/ampcode/config`,
    "Failed to get Amp Code config for profile"
  );
}

// Save Amp Code config for a specific profile
export async function saveAmpCodeConfigForProfile(
  profile: string,
  config: AmpCodeConfig
): Promise<void> {
  return apiPut(
    `/api/library/config-profile/${encodeURIComponent(profile)}/ampcode/config`,
    config,
    "Failed to save Amp Code config for profile"
  );
}

// List all files in a config profile
export async function listConfigProfileFiles(profile: string): Promise<string[]> {
  return apiGet(
    `/api/library/config-profile/${encodeURIComponent(profile)}/files`,
    "Failed to list config profile files"
  );
}

// Get a specific file from a config profile
export async function getConfigProfileFile(profile: string, filePath: string): Promise<string> {
  const response = await fetch(
    apiUrl(`/api/library/config-profile/${encodeURIComponent(profile)}/file/${filePath}`),
    {
      headers: authHeader(),
    }
  );
  if (!response.ok) {
    throw new Error(`Failed to get config profile file: ${response.statusText}`);
  }
  return response.text();
}

// Save a specific file in a config profile
export async function saveConfigProfileFile(
  profile: string,
  filePath: string,
  content: string
): Promise<void> {
  const response = await fetch(
    apiUrl(`/api/library/config-profile/${encodeURIComponent(profile)}/file/${filePath}`),
    {
      method: "PUT",
      headers: {
        ...authHeader(),
        "Content-Type": "text/plain",
      },
      body: content,
    }
  );
  if (!response.ok) {
    throw new Error(`Failed to save config profile file: ${response.statusText}`);
  }
}

// Delete a specific file from a config profile
export async function deleteConfigProfileFile(
  profile: string,
  filePath: string
): Promise<void> {
  const response = await fetch(
    apiUrl(`/api/library/config-profile/${encodeURIComponent(profile)}/file/${filePath}`),
    {
      method: "DELETE",
      headers: authHeader(),
    }
  );
  if (!response.ok) {
    throw new Error(`Failed to delete config profile file: ${response.statusText}`);
  }
}

// List default files for a harness from the library
export async function listHarnessDefaultFiles(harness: string): Promise<string[]> {
  return apiGet(
    `/api/library/harness-default/${encodeURIComponent(harness)}`,
    "Failed to list harness default files"
  );
}

// Get a harness default file from the library
export async function getHarnessDefaultFile(harness: string, fileName: string): Promise<string> {
  const response = await fetch(
    apiUrl(`/api/library/harness-default/${encodeURIComponent(harness)}/${fileName}`),
    {
      headers: authHeader(),
    }
  );
  if (!response.ok) {
    if (response.status === 404) {
      throw new Error(`Harness default file not found: ${harness}/${fileName}`);
    }
    throw new Error(`Failed to get harness default file: ${response.statusText}`);
  }
  return response.text();
}

// Save a harness default file in the library
export async function saveHarnessDefaultFile(
  harness: string,
  fileName: string,
  content: string
): Promise<void> {
  const response = await fetch(
    apiUrl(`/api/library/harness-default/${encodeURIComponent(harness)}/${fileName}`),
    {
      method: "PUT",
      headers: {
        ...authHeader(),
        "Content-Type": "text/plain",
      },
      body: content,
    }
  );
  if (!response.ok) {
    throw new Error(`Failed to save harness default file: ${response.statusText}`);
  }
}

// AI Provider types and functions are now exported from ./api/providers
// Legacy interface removed - types come from providers module

interface _RemovedLegacyAIProvider {
  _removed: true;
  has_api_key: boolean;
}

// Provider types and BackendProviderResponse are now in ./api/providers

// This legacy interface is kept due to redacted content but is not exported
interface _LegacyBackendProviderResponse {
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

// ============================================================================
// Secrets API
// ============================================================================

export interface SecretsStatus {
  initialized: boolean;
  can_decrypt: boolean;
  registries: RegistryInfo[];
  default_key: string | null;
}

export interface EncryptionStatus {
  key_available: boolean;
  key_source: 'environment' | 'file' | null;
  key_file_path: string | null;
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

// Get encryption status (for skill content encryption)
export async function getEncryptionStatus(): Promise<EncryptionStatus> {
  return apiGet('/api/secrets/encryption', 'Failed to get encryption status');
}

// Get private key (hex-encoded)
export interface PrivateKeyResponse {
  key_hex: string | null;
  key_source: string | null;
}

export async function getPrivateKey(): Promise<PrivateKeyResponse> {
  return apiGet('/api/secrets/encryption/key', 'Failed to get private key');
}

// Set/update private key
export interface SetPrivateKeyResponse {
  success: boolean;
  message: string;
  reencrypted_count: number;
  failed_count: number;
}

export async function setPrivateKey(keyHex: string): Promise<SetPrivateKeyResponse> {
  return apiPut('/api/secrets/encryption/key', { key_hex: keyHex }, 'Failed to set private key');
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

// Remove all stopped desktop session records from storage
export async function cleanupStoppedDesktopSessions(): Promise<OperationResponse> {
  return apiPost('/api/desktop/sessions/cleanup-stopped', undefined, 'Failed to cleanup stopped sessions');
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
  source_path?: string | null;
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

// Shared helper for streaming system component operations via SSE
async function streamComponentOperation(
  url: string,
  operationName: string,
  onProgress: (event: UpdateProgressEvent) => void,
  onComplete: () => void,
  onError: (error: string) => void
): Promise<void> {
  try {
    const res = await apiFetch(url, {
      method: 'POST',
      headers: {
        'Accept': 'text/event-stream',
      },
    });

    if (!res.ok) {
      const text = await res.text();
      onError(text || `Failed to start ${operationName}`);
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

// Update a system component (streams progress via SSE)
export async function updateSystemComponent(
  name: string,
  onProgress: (event: UpdateProgressEvent) => void,
  onComplete: () => void,
  onError: (error: string) => void
): Promise<void> {
  return streamComponentOperation(
    `/api/system/components/${name}/update`,
    'update',
    onProgress,
    onComplete,
    onError
  );
}

// Uninstall a system component (streams progress via SSE)
export async function uninstallSystemComponent(
  name: string,
  onProgress: (event: UpdateProgressEvent) => void,
  onComplete: () => void,
  onError: (error: string) => void
): Promise<void> {
  return streamComponentOperation(
    `/api/system/components/${name}/uninstall`,
    'uninstall',
    onProgress,
    onComplete,
    onError
  );
}

// ============================================
// Global Settings API
// ============================================

export interface SettingsResponse {
  library_remote: string | null;
  sandboxed_repo_path: string | null;
  rtk_enabled: boolean | null;
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

export async function updateSettings(
  settings: Partial<SettingsResponse>
): Promise<SettingsResponse> {
  const res = await apiFetch('/api/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(settings),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to update settings');
  }
  return res.json();
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

// Update the RTK enabled setting
export async function updateRtkEnabled(
  rtkEnabled: boolean
): Promise<{ rtk_enabled: boolean; previous_value: boolean | null }> {
  const res = await apiFetch('/api/settings/rtk-enabled', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ rtk_enabled: rtkEnabled }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || 'Failed to update RTK setting');
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
  /** Whether the CLI for this backend is available on the system */
  cli_available?: boolean;
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
  let filename = 'sandboxed-backup.zip';
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
