import { authHeader, clearJwt, signalAuthRequired } from './auth';
import { getRuntimeApiBase, getRuntimeTaskDefaults } from './settings';

function apiUrl(pathOrUrl: string): string {
  if (/^https?:\/\//i.test(pathOrUrl)) return pathOrUrl;
  const base = getRuntimeApiBase();
  const path = pathOrUrl.startsWith('/') ? pathOrUrl : `/${pathOrUrl}`;
  return `${base}${path}`;
}

export interface TaskState {
  id: string;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  task: string;
  model: string;
  iterations: number;
  result: string | null;
  log: TaskLogEntry[];
}

export interface TaskLogEntry {
  timestamp: string;
  entry_type: 'thinking' | 'tool_call' | 'tool_result' | 'response' | 'error';
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
  const res = await fetch(apiUrl('/api/health'));
  if (!res.ok) throw new Error('Failed to fetch health');
  return res.json();
}

export async function login(password: string): Promise<LoginResponse> {
  const res = await fetch(apiUrl('/api/auth/login'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password }),
  });
  if (!res.ok) throw new Error('Failed to login');
  return res.json();
}

// Get statistics
export async function getStats(): Promise<StatsResponse> {
  const res = await apiFetch('/api/stats');
  if (!res.ok) throw new Error('Failed to fetch stats');
  return res.json();
}

// List all tasks
export async function listTasks(): Promise<TaskState[]> {
  const res = await apiFetch('/api/tasks');
  if (!res.ok) throw new Error('Failed to fetch tasks');
  return res.json();
}

// Get a specific task
export async function getTask(id: string): Promise<TaskState> {
  const res = await apiFetch(`/api/task/${id}`);
  if (!res.ok) throw new Error('Failed to fetch task');
  return res.json();
}

// Create a new task
export async function createTask(request: CreateTaskRequest): Promise<{ id: string; status: string }> {
  const defaults = getRuntimeTaskDefaults();
  const merged: CreateTaskRequest = {
    ...defaults,
    ...request,
  };
  const res = await apiFetch('/api/task', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(merged),
  });
  if (!res.ok) throw new Error('Failed to create task');
  return res.json();
}

// Stop a task
export async function stopTask(id: string): Promise<void> {
  const res = await apiFetch(`/api/task/${id}/stop`, {
    method: 'POST',
  });
  if (!res.ok) throw new Error('Failed to stop task');
}

// Stream task progress (SSE)
export function streamTask(id: string, onEvent: (event: { type: string; data: unknown }) => void): () => void {
  const controller = new AbortController();
  const decoder = new TextDecoder();
  let buffer = '';
  let sawDone = false;

  void (async () => {
    try {
      const res = await apiFetch(`/api/task/${id}/stream`, {
        method: 'GET',
        headers: { Accept: 'text/event-stream' },
        signal: controller.signal,
      });

      if (!res.ok) {
        onEvent({
          type: 'error',
          data: { message: `Stream request failed (${res.status})`, status: res.status },
        });
        return;
      }
      if (!res.body) {
        onEvent({ type: 'error', data: { message: 'Stream response had no body' } });
        return;
      }

      const reader = res.body.getReader();
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });

        let idx = buffer.indexOf('\n\n');
        while (idx !== -1) {
          const raw = buffer.slice(0, idx);
          buffer = buffer.slice(idx + 2);
          idx = buffer.indexOf('\n\n');

          let eventType = 'message';
          let data = '';
          for (const line of raw.split('\n')) {
            if (line.startsWith('event:')) {
              eventType = line.slice('event:'.length).trim();
            } else if (line.startsWith('data:')) {
              data += line.slice('data:'.length).trim();
            }
          }

          if (!data) continue;
          try {
            if (eventType === 'done') {
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
        onEvent({ type: 'error', data: { message: 'Stream ended unexpectedly' } });
      }
    } catch {
      if (!controller.signal.aborted) {
        onEvent({ type: 'error', data: { message: 'Stream connection failed' } });
      }
    }
  })();

  return () => controller.abort();
}

// List runs
export async function listRuns(limit = 20, offset = 0): Promise<{ runs: Run[]; limit: number; offset: number }> {
  const res = await apiFetch(`/api/runs?limit=${limit}&offset=${offset}`);
  if (!res.ok) throw new Error('Failed to fetch runs');
  return res.json();
}

// Get run details
export async function getRun(id: string): Promise<Run> {
  const res = await apiFetch(`/api/runs/${id}`);
  if (!res.ok) throw new Error('Failed to fetch run');
  return res.json();
}

// Get run events
export async function getRunEvents(id: string, limit?: number): Promise<{ run_id: string; events: unknown[] }> {
  const url = limit ? `/api/runs/${id}/events?limit=${limit}` : `/api/runs/${id}/events`;
  const res = await apiFetch(url);
  if (!res.ok) throw new Error('Failed to fetch run events');
  return res.json();
}

// Get run tasks
export async function getRunTasks(id: string): Promise<{ run_id: string; tasks: unknown[] }> {
  const res = await apiFetch(`/api/runs/${id}/tasks`);
  if (!res.ok) throw new Error('Failed to fetch run tasks');
  return res.json();
}

