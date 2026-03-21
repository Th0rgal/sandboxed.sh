/**
 * Tasks API — command-mode and agent-mode task management.
 */

import { apiGet, apiPost } from "./core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type TaskStatus = "pending" | "running" | "completed" | "failed" | "cancelled";
export type TaskMode = "agent" | "command";

export type LogEntryType = "thinking" | "tool_call" | "tool_result" | "response" | "error";

export interface TaskLogEntry {
  timestamp: string;
  entry_type: LogEntryType;
  content: string;
}

export interface TaskStep {
  name: string;
  iteration?: number;
  status: string;
  started_at?: string;
  completed_at?: string;
  duration_s?: number;
  metadata?: Record<string, unknown>;
}

export interface Task {
  id: string;
  status: TaskStatus;
  task: string;
  mode: TaskMode;
  model: string;
  iterations: number;
  workspace_id?: string;
  workspace_name?: string;
  result?: string;
  log: TaskLogEntry[];
  steps?: TaskStep[];
  created_at?: string;
  started_at?: string;
  completed_at?: string;
  duration_secs?: number;
}

export interface CreateCommandTaskRequest {
  task: string;
  command: string;
  workspace_id: string;
  timeout_secs?: number;
}

export interface CreateAgentTaskRequest {
  task: string;
  model?: string;
  working_dir?: string;
  budget_cents?: number;
}

// ---------------------------------------------------------------------------
// API calls
// ---------------------------------------------------------------------------

export async function listTasks(): Promise<Task[]> {
  return apiGet<Task[]>("/api/tasks", "Failed to list tasks");
}

export async function getTask(id: string): Promise<Task> {
  return apiGet<Task>(`/api/task/${id}`, "Failed to get task");
}

export async function createTask(
  req: CreateCommandTaskRequest | CreateAgentTaskRequest
): Promise<{ id: string; status: TaskStatus }> {
  return apiPost("/api/task", req);
}

export async function stopTask(id: string): Promise<void> {
  await apiPost(`/api/task/${id}/stop`, {});
}
