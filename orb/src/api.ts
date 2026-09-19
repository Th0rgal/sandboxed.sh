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
  /** "cli_proxy" when CLIProxyAPI owns the OAuth credential (reconnect via proxy login). */
  credential_owner?: "cli_proxy" | "sandboxed_sh";
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
  /** Stable project identifier — groups the mission under the project. */
  project?: string;
  /** Harness id (claudecode, codex, opencode, grok, gemini). */
  backend?: string;
  /** Model id understood by that harness, e.g. claude-fable-5-1. */
  model_override?: string;
  model_effort?: string;
}

export interface BackendInfo {
  id: string;
  name: string;
}

export interface BackendModelOption {
  value: string;
  label: string;
  description?: string;
  provider_id?: string;
}

/** Harness (backend) with the models it can run right now. */
export interface HarnessChoice {
  backend: BackendInfo;
  models: BackendModelOption[];
}

export async function listBackends(): Promise<BackendInfo[]> {
  const data = await api<BackendInfo[] | { backends?: BackendInfo[] }>("/api/backends");
  return Array.isArray(data) ? data : (data.backends ?? []);
}

export async function listBackendModels(): Promise<Record<string, BackendModelOption[]>> {
  const data = await api<{ backends?: Record<string, BackendModelOption[]> }>("/api/providers/backend-models");
  return data.backends ?? {};
}

/** Harness order for the composer: the native agents first, then routers. */
const HARNESS_ORDER = ["claudecode", "codex", "grok", "gemini", "opencode"];

export async function listHarnessChoices(): Promise<HarnessChoice[]> {
  const [backends, models] = await Promise.all([listBackends(), listBackendModels()]);
  return backends
    .filter((b) => (models[b.id]?.length ?? 0) > 0)
    .sort((a, b) => {
      const ia = HARNESS_ORDER.indexOf(a.id);
      const ib = HARNESS_ORDER.indexOf(b.id);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    })
    .map((b) => ({ backend: b, models: models[b.id] ?? [] }));
}

/** "Claude (Subscription) — Claude Fable 5.1" → "Fable 5.1". */
export function shortModelLabel(label: string): string {
  const tail = label.includes("—") ? label.slice(label.lastIndexOf("—") + 1).trim() : label.trim();
  // "Claude Fable 5.1" → "Fable 5.1"; other vendors keep their family name.
  return tail.replace(/^Claude\s+/, "");
}

export async function getRemoteNodes(): Promise<RemoteNodesResponse> {
  return api("/api/remote-nodes");
}

export async function listProviders(): Promise<AIProvider[]> {
  const data = await api<AIProvider[] | { providers?: AIProvider[] }>("/api/ai/providers");
  if (Array.isArray(data)) return data;
  return Array.isArray(data.providers) ? data.providers : [];
}

export interface ProviderUsage {
  provider_type: string;
  error?: string;
  status?: string;
  account_email?: string | null;
  account_name?: string | null;
  organization?: string | null;
  unified_status?: string;
  unified_5h_utilization?: number;
  unified_5h_reset?: string;
  unified_5h_status?: string;
  unified_7d_utilization?: number;
  unified_7d_reset?: string;
  unified_7d_status?: string;
  requests_limit?: number;
  requests_remaining?: number;
  requests_reset?: string;
  tokens_limit?: number;
  tokens_remaining?: number;
  tokens_reset?: string;
  codex_plan_type?: string;
  codex_primary_used_percent?: number;
  codex_primary_reset_at?: number;
  codex_secondary_used_percent?: number;
  codex_secondary_reset_at?: number;
  minimax_interval_remaining_percent?: number;
  minimax_interval_reset?: number;
  minimax_weekly_remaining_percent?: number;
  minimax_weekly_reset?: number;
  model_usage?: Array<{
    model: string;
    interval_remaining_percent: number;
    weekly_remaining_percent: number;
    interval_reset: number;
    weekly_reset: number;
  }>;
  zai_plan?: string;
  zai_tokens_percentage?: number;
  zai_tokens_reset?: number;
  zai_mcp_percentage?: number;
  zai_mcp_reset?: number;
}

export async function getAllProviderUsage(): Promise<Record<string, ProviderUsage>> {
  const data = await api<{ entries?: Record<string, ProviderUsage> }>("/api/ai/providers/usage");
  return data.entries ?? {};
}

export type CliProxyLoginStatus = "pending" | "completing" | "completed" | "failed";

export interface CliProxyLoginStart {
  session_id: string;
  auth_url: string;
  flow?: "redirect" | "device";
}

export interface CliProxyLoginState {
  status: CliProxyLoginStatus;
  auth_url?: string;
  message?: string;
}

export async function startCliProxyLogin(provider: string): Promise<CliProxyLoginStart> {
  return api("/api/ai/providers/cli-proxy-login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ provider }),
  });
}

export async function getCliProxyLogin(sessionId: string): Promise<CliProxyLoginState> {
  return api(`/api/ai/providers/cli-proxy-login/${sessionId}`);
}

export async function submitCliProxyLoginCallback(sessionId: string, url: string): Promise<CliProxyLoginState> {
  return api(`/api/ai/providers/cli-proxy-login/${sessionId}/callback`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url }),
  });
}

/** Open a URL in the system browser (tauri shell when native, tab in dev). */
export async function openExternalUrl(url: string): Promise<void> {
  try {
    const g = window as unknown as {
      __TAURI_INTERNALS__?: { invoke?: (c: string, a?: Record<string, unknown>) => Promise<unknown> };
      __TAURI__?: { core?: { invoke?: (c: string, a?: Record<string, unknown>) => Promise<unknown> } };
    };
    const invoke = g.__TAURI__?.core?.invoke ?? g.__TAURI_INTERNALS__?.invoke;
    if (invoke) {
      await invoke("open_url", { url });
      return;
    }
  } catch {
    /* fall through */
  }
  window.open(url, "_blank");
}

export interface ProjectSummary {
  slug: string;
  title?: string | null;
  objective?: string | null;
  status: string;
  updated_at: string;
}

export interface ProjectFileEntry {
  name: string;
  kind: "dir" | "file";
  size?: number;
  modified?: string;
}

export async function listProjects(): Promise<ProjectSummary[]> {
  const data = await api<{ projects?: ProjectSummary[] }>("/api/projects");
  return data.projects ?? [];
}

/** Create (or update) a project record on the core. Slug: lowercase, dashes. */
export async function createProject(body: { slug: string; title?: string; objective?: string }): Promise<ProjectSummary> {
  return api("/api/projects", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** A project's controller: the Hermes cron job that drives it. */
export interface ControllerJob {
  id: string;
  name: string;
  schedule?: string | null;
  enabled: boolean;
  state?: string | null;
  paused_reason?: string | null;
  next_run_at?: string | null;
  last_run_at?: string | null;
  last_status?: string | null;
  last_error?: string | null;
  failure_streak: number;
  deliver?: string | null;
}

export interface ControllerRun {
  id: string;
  at?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  duration_secs?: number | null;
  status?: string | null;
  source?: string | null;
  delivery_outcome?: string | null;
  silent: boolean;
  report: string;
  ctrl?: string | null;
  signature?: string | null;
  error?: string | null;
}

/** What the cron does: a prompt run on a schedule by a fresh agent. */
export interface ControllerSettings {
  prompt: string;
  prompt_chars: number;
  skills: string[];
  deliver?: string | null;
  failure_deliver?: string | null;
  repeat_times?: number | null;
  repeat_completed: number;
  model?: string | null;
  provider?: string | null;
  reasoning_effort?: string | null;
  workdir?: string | null;
  script?: string | null;
  no_agent: boolean;
  continuity: boolean;
  monitor_url?: string | null;
  monitor_script?: string | null;
  enabled_toolsets: string[];
  created_at?: string | null;
  binding?: Record<string, unknown> | null;
  /** Cap on prompt + preloaded skills for scope-bound controllers. */
  prompt_budget?: number | null;
}

export interface ControllerView {
  slug: string;
  job: ControllerJob | null;
  settings?: ControllerSettings | null;
  runs: ControllerRun[];
}

/** Only the fields that changed; "" clears an optional pin. */
export interface ControllerPatch {
  name?: string;
  schedule?: string;
  prompt?: string;
  skills?: string[];
  deliver?: string;
  failure_deliver?: string;
  repeat?: number;
  workdir?: string;
  model?: string;
  provider?: string;
  reasoning_effort?: string;
  continuity?: boolean;
}

export async function updateController(slug: string, patch: ControllerPatch): Promise<ControllerView> {
  return api(`/api/projects/${encodeURIComponent(slug)}/controller`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
}

export async function getProjectController(slug: string, limit = 40): Promise<ControllerView> {
  return api(`/api/projects/${encodeURIComponent(slug)}/controller?limit=${limit}`);
}

export async function controllerAction(slug: string, action: "pause" | "resume" | "run"): Promise<ControllerView> {
  return api(`/api/projects/${encodeURIComponent(slug)}/controller/action`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action }),
  });
}

/** Bumped after a project is created so every list re-fetches. */
const [projectsVersion, setProjectsVersion] = createSignal(0);
export { projectsVersion };
export const bumpProjects = () => setProjectsVersion((v) => v + 1);

/** "Pareto Credit Vault" → "pareto-credit-vault". */
export function slugify(title: string): string {
  return title
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

/** Missions tagged with this project (exact slug match on the backend). */
export async function listProjectMissions(slug: string): Promise<Mission[]> {
  return api(`/api/control/missions?project=${encodeURIComponent(slug)}&limit=100&all=true`);
}

export async function listProjectFiles(slug: string, path: string): Promise<ProjectFileEntry[]> {
  const data = await api<{ entries?: ProjectFileEntry[] }>(
    `/api/projects/${encodeURIComponent(slug)}/files?path=${encodeURIComponent(path)}`,
  );
  return data.entries ?? [];
}

export async function readProjectFile(slug: string, path: string): Promise<string> {
  const data = await api<{ content?: string }>(
    `/api/projects/${encodeURIComponent(slug)}/file?path=${encodeURIComponent(path)}`,
  );
  return data.content ?? "";
}

export async function writeProjectFile(slug: string, path: string, content: string): Promise<void> {
  await api(`/api/projects/${encodeURIComponent(slug)}/file`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path, content }),
  });
}

export async function mkdirProjectFile(slug: string, path: string): Promise<void> {
  await api(`/api/projects/${encodeURIComponent(slug)}/file/mkdir`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ path }),
  });
}

export async function deleteProjectFile(slug: string, path: string): Promise<void> {
  await api(`/api/projects/${encodeURIComponent(slug)}/file?path=${encodeURIComponent(path)}`, {
    method: "DELETE",
  });
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
