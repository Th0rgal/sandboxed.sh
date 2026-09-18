import { createSignal } from "solid-js";

const URL_KEY = "orb.apiUrl";
const JWT_KEY = "orb.jwt";
const DEFAULT_URL = "https://agent-backend.thomas.md";

export function getApiUrl(): string {
  return localStorage.getItem(URL_KEY) ?? DEFAULT_URL;
}

export function setApiUrl(url: string) {
  localStorage.setItem(URL_KEY, url.trim().replace(/\/+$/, ""));
}

export function getJwt(): string | null {
  return localStorage.getItem(JWT_KEY);
}

const [connected, setConnected] = createSignal(!!getJwt());
export const isConnected = connected;

export function setConnection(url: string, token: string) {
  setApiUrl(url);
  localStorage.setItem(JWT_KEY, token);
  setConnected(true);
}

export function clearConnection() {
  localStorage.removeItem(JWT_KEY);
  setConnected(false);
}

export async function login(password: string): Promise<void> {
  const res = await fetch(`${getApiUrl()}/api/auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
  });
  if (!res.ok) {
    const text = (await res.text().catch(() => "")).trim();
    throw new Error(text || `Login failed (${res.status})`);
  }
  const data = (await res.json()) as { token?: string };
  if (!data.token) throw new Error("Login response did not include a token");
  localStorage.setItem(JWT_KEY, data.token);
  setConnected(true);
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const jwt = getJwt();
  const headers: Record<string, string> = {
    ...((init?.headers as Record<string, string> | undefined) ?? {}),
    ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
  };
  const res = await fetch(`${getApiUrl()}${path}`, { ...init, headers });
  if (res.status === 401) {
    clearConnection();
    throw new Error("401 Unauthorized — reconnect in Settings → Backend");
  }
  if (!res.ok) {
    const text = (await res.text().catch(() => "")).trim();
    throw new Error(`${res.status} ${text.slice(0, 200)}`.trim());
  }
  return res.json().catch(() => undefined as unknown as T);
}

export interface RemoteNodeView {
  id: string;
  base_url: string;
  token_env: string;
  status: "online" | "degraded" | "offline" | "unknown" | string;
  labels: string[];
  version: string | null;
  capacity_total: number | null;
  capacity_available: number | null;
  active_jobs: number | null;
  queued_jobs: number | null;
  last_seen: string | null;
  error: string | null;
  cordoned: boolean;
}

export interface RemoteNodesResponse {
  enabled: boolean;
  nodes: RemoteNodeView[];
}

export interface AIProvider {
  id: string;
  provider_type: string;
  provider_type_name: string;
  name: string;
  label?: string | null;
  enabled: boolean;
  uses_oauth: boolean;
  account_email?: string | null;
  status: { type: string; reason?: string; message?: string };
}

export interface MissionHistoryEntry {
  role: string;
  content: string;
}

export interface Mission {
  id: string;
  status: string;
  title: string | null;
  history: MissionHistoryEntry[];
  workspace_name?: string | null;
  agent?: string | null;
  backend?: string;
  created_at: string;
  updated_at: string;
}

export interface CreateMissionBody {
  title?: string;
  prompt?: string;
  remote_node_id?: string;
  remote_command?: string;
}

export async function getRemoteNodes(): Promise<RemoteNodesResponse> {
  return api("/api/remote-nodes");
}

export async function listProviders(): Promise<AIProvider[]> {
  const data = await api<AIProvider[] | { providers?: AIProvider[] }>("/api/ai/providers");
  if (Array.isArray(data)) return data;
  return Array.isArray(data.providers) ? data.providers : [];
}

export async function listMissions(): Promise<Mission[]> {
  return api("/api/control/missions");
}

export async function getMission(id: string): Promise<Mission> {
  return api(`/api/control/missions/${id}`);
}

export async function createMission(body: CreateMissionBody): Promise<Mission> {
  return api("/api/control/missions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export async function sendMissionMessage(id: string, text: string): Promise<{ id: string; queued: boolean }> {
  return api("/api/control/message", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content: text, mission_id: id }),
  });
}

export async function cancelMission(id: string): Promise<void> {
  await api<void>(`/api/control/missions/${id}/cancel`, { method: "POST" });
}

const NODE_KEY_NAME = "orb-remote-agents";
const NODE_KEY_STORAGE = "orb.nodeAgentKey";

interface ProxyKeySummary {
  id: string;
  name: string;
}

/**
 * Remote nodes run jobs with a scrubbed env (env_clear on the node runner), so
 * the only way to hand an agent CLI credentials is inline in the command. We
 * mint a dedicated, revocable proxy API key on the core backend and point the
 * CLI at the core's /v1 proxy — the OAuth subscriptions stay on the core.
 */
export async function ensureNodeAgentKey(): Promise<string> {
  const stored = localStorage.getItem(NODE_KEY_STORAGE);
  if (stored) return stored;
  const keys = await api<ProxyKeySummary[]>("/api/proxy-keys");
  const stale = keys.filter((k) => k.name === NODE_KEY_NAME);
  // Raw values are only returned at creation time, so a stored key we no
  // longer have the value for is useless — delete and re-mint.
  for (const k of stale) {
    await api(`/api/proxy-keys/${k.id}`, { method: "DELETE" }).catch(() => {});
  }
  const created = await api<{ key: string }>("/api/proxy-keys", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: NODE_KEY_NAME }),
  });
  localStorage.setItem(NODE_KEY_STORAGE, created.key);
  return created.key;
}

/**
 * Command dispatched to a remote node. Prefers claude (headless) through the
 * core proxy via the Anthropic-native /v1/messages endpoint; falls back to
 * opencode through the OpenAI-compatible /v1/chat/completions endpoint.
 */
export function buildRemoteAgentCommand(prompt: string, nodeKey: string): string {
  const quoted = `'${prompt.replace(/'/g, `'\\''`)}'`;
  const base = getApiUrl();
  return (
    `if command -v claude >/dev/null 2>&1; then ` +
    `ANTHROPIC_BASE_URL='${base}' ANTHROPIC_AUTH_TOKEN='${nodeKey}' claude -p ${quoted}; ` +
    `elif command -v opencode >/dev/null 2>&1; then ` +
    `OPENAI_BASE_URL='${base}/v1' OPENAI_API_KEY='${nodeKey}' opencode run ${quoted}; ` +
    `else echo 'no agent CLI on this node' >&2; exit 127; fi`
  );
}
