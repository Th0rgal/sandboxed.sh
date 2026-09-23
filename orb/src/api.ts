import { createSignal } from "solid-js";
import { getProjectCronFromJob, hermesPatch, normalizeControllerView, type HermesControllerView, type HermesJob } from "./cronSchema";

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
export const [connectionVersion, bumpConnectionVersion] = createSignal(0);

export function setConnection(url: string, token: string) {
  setApiUrl(url);
  localStorage.setItem(JWT_KEY, token);
  setConnected(true);
  bumpConnectionVersion(v => v + 1);
}

export function clearConnection() {
  const hadConnection = connected() || !!getJwt();
  localStorage.removeItem(JWT_KEY);
  setConnected(false);
  if (!hadConnection) return;
  bumpConnectionVersion(v => v + 1);
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
  bumpConnectionVersion(v => v + 1);
}

export class ApiError extends Error {
  constructor(public status: number, public detail: string) { super(`${status} ${detail}`.trim()); }
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const version = connectionVersion();
  const jwt = getJwt();
  const headers: Record<string, string> = {
    ...((init?.headers as Record<string, string> | undefined) ?? {}),
    ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
  };
  const res = await fetch(`${getApiUrl()}${path}`, { ...init, headers });
  if (res.status === 401) {
    if (connectionVersion() === version) clearConnection();
    throw new Error("401 Unauthorized — reconnect in Settings → Backend");
  }
  if (!res.ok) {
    const text = (await res.text().catch(() => "")).trim();
    throw new ApiError(res.status, text);
  }
  return res.json().catch(() => undefined as unknown as T);
}

export interface RemoteNodeView {
  resource_history?: { time: number; memory?: number | null; cpu?: number | null; gpu?: number | null }[];
  cpu_total?: number | null;
  mem_total_bytes?: number | null;
  mem_available_bytes?: number | null;
  disk_total_bytes?: number | null;
  disk_available_bytes?: number | null;
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

/**
 * Server-advertised typed remote launch support (`GET /api/remote-nodes`).
 * Absent on older backends, which only accept raw `remote_command` launches
 * that Orb never generates. `harnesses` are harness ids, not model ids; model
 * and provisioning validation stay server-owned on the typed create path.
 */
export interface RemoteLaunchCapability {
  typed?: boolean;
  harnesses?: string[];
  raw_command?: boolean;
  proxy_url_configured?: boolean;
  /** Harness ids that need the backend model proxy. Absent: claudecode and opencode only. */
  requires_proxy_harnesses?: string[];
  error_prefixes?: string[];
}

export interface RemoteNodesResponse {
  enabled: boolean;
  nodes: RemoteNodeView[];
  remote_launch?: RemoteLaunchCapability | null;
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

/** Read-only remote execution evidence from the durable job ledger. */
export interface RemoteJob {
  job_id: string;
  node_id: string;
  phase: string;
  node_state?: string | null;
  exit_code?: number | null;
  error?: string | null;
  accepted_at?: string | null;
  heartbeat_at?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  observed_age_secs?: number | null;
  lease_state?: string | null;
  terminal_reason?: string | null;
}

export interface Mission {
  working_directory?: string | null;
  id: string;
  status: string;
  title: string | null;
  history: MissionHistoryEntry[];
  goal_mode?: boolean;
  goal_objective?: string | null;
  remote_node_id?: string | null;
  terminal_reason?: string | null;
  status_message?: string | null;
  execution?: { state?: string; terminal_reason?: string | null };
  remote_job?: RemoteJob | null;
  workspace_name?: string | null;
  workspace_id?: string | null;
  agent?: string | null;
  backend?: string;
  model_override?: string | null;
  /** Reasoning effort in force for the next turn. Absent means backend default. */
  model_effort?: string | null;
  project?: string | null;
  tags?: string[];
  created_at: string;
  updated_at: string;
}

export interface CreateMissionBody {
  tags?: string[];
  idempotency_key?: string;
  title?: string;
  prompt?: string;
  remote_node_id?: string;
  /** Stable project identifier — groups the mission under the project. */
  project?: string;
  /**
   * Harness id (claudecode, codex, opencode, grok, gemini). Goal mode has no
   * dedicated create field: a prompt of the form `/goal <objective>` is what
   * makes the server persist `goal_mode` + `goal_objective` (control/mod.rs
   * `parse_goal_objective`).
   */
  backend?: string;
  /** Model id understood by that harness, e.g. claude-fable-5-1. */
  model_override?: string;
  model_effort?: string;
  attachments?: MissionAttachment[];
  /** `"client"` records the mission and leaves execution to this Orb process. */
  placement?: "client";
}

export type MissionAttachmentKind = "file" | "folder" | "controller";
export interface MissionAttachment {
  kind: MissionAttachmentKind;
  path?: string;
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
const HARNESS_ORDER = ["claudecode", "codex", "grok", "opencode"];

export async function listHarnessChoices(): Promise<HarnessChoice[]> {
  const [backends, models] = await Promise.all([listBackends(), listBackendModels()]);
  return backends
    .filter((b) => b.id !== "gemini" && (models[b.id]?.length ?? 0) > 0)
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
  codex_primary_window_minutes?: number;
  codex_secondary_used_percent?: number;
  codex_secondary_reset_at?: number;
  codex_secondary_window_minutes?: number;
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

export async function getProviderUsage(id: string): Promise<ProviderUsage> {
  return api(`/api/ai/providers/${encodeURIComponent(id)}/usage`);
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

/** Create (or update) a project record on the core. Slug: lowercase, dashes. */
export async function createProject(body: { slug: string; title?: string; objective?: string }): Promise<ProjectSummary> {
  return api("/api/projects", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** Display-name rename: same slug, new title. */
export const updateProject = createProject;

const archivedSlugs = new Set<string>();

/** Board archive. The roster row stays; the project leaves the live list. */
export async function archiveProject(slug: string): Promise<void> {
  await api(`/api/projects/${encodeURIComponent(slug)}/action`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action: "archive" }),
  });
  archivedSlugs.add(slug);
}

export async function listProjects(): Promise<ProjectSummary[]> {
  const data = await api<{ projects?: ProjectSummary[] }>("/api/projects");
  return (data.projects ?? []).filter(
    (p) => p.status !== "archived" && p.status !== "deleted" && !archivedSlugs.has(p.slug),
  );
}

/** A project's controller: the Hermes cron job that drives it. */
export interface ControllerJob {
  folder?: string;
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
  /** Creation-time defaults, kept separate from explicit overrides. */
  model_snapshot?: string | null;
  provider_snapshot?: string | null;
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
  folder?: string;
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
  return normalizeControllerView(await api<HermesControllerView>(`/api/projects/${encodeURIComponent(slug)}/controller`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  }));
}

export async function getProjectController(slug: string, limit = 40): Promise<ControllerView> {
  return normalizeControllerView(await api<HermesControllerView>(`/api/projects/${encodeURIComponent(slug)}/controller?limit=${limit}`));
}

export async function controllerAction(slug: string, action: "pause" | "resume" | "run"): Promise<ControllerView> {
  const view = normalizeControllerView(await api<HermesControllerView>(`/api/projects/${encodeURIComponent(slug)}/controller/action`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action }),
  }));
  if (action === "run" && (!view.job || !view.job.enabled || view.job.state === "paused")) {
    throw new Error("The scheduler did not wake the paused controller. Your steer is still saved.");
  }
  return view;
}

/** Additional Hermes jobs explicitly bound to this project by the core. */
export async function listProjectCrons(slug: string): Promise<ControllerJob[]> {
  const data = await api<{ jobs?: HermesJob[] }>(`/api/projects/${encodeURIComponent(slug)}/crons`);
  return (data.jobs ?? []).map((job) => getProjectCronFromJob(slug, job).job!);
}

export interface ProjectCronDefaults { deliver: string; route_ready: boolean; folders_supported?: boolean }
export async function getProjectCronDefaults(slug: string): Promise<ProjectCronDefaults> {
  return api(`/api/projects/${encodeURIComponent(slug)}/crons/defaults`);
}

export type ProjectCronDraft = ControllerPatch;

export async function createProjectCron(slug: string, draft: ProjectCronDraft): Promise<HermesJob> {
  const data = await api<{ job: HermesJob }>(`/api/projects/${encodeURIComponent(slug)}/crons`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(hermesPatch(draft)),
  });
  return data.job;
}

export async function getProjectCron(slug: string, id: string): Promise<ControllerView> {
  const data = await api<{ job: HermesJob }>(`/api/projects/${encodeURIComponent(slug)}/crons/${encodeURIComponent(id)}`);
  return getProjectCronFromJob(slug, data.job);
}

export async function updateProjectCron(slug: string, id: string, patch: ControllerPatch): Promise<ControllerView> {
  const current = patch.continuity === undefined ? undefined : await api<{ job: HermesJob }>(`/api/projects/${encodeURIComponent(slug)}/crons/${encodeURIComponent(id)}`);
  const data = await api<{ job: HermesJob }>(`/api/projects/${encodeURIComponent(slug)}/crons/${encodeURIComponent(id)}`, { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify(hermesPatch(patch, current?.job)) });
  return getProjectCronFromJob(slug, data.job);
}

export async function projectCronAction(slug: string, id: string, action: "pause" | "resume" | "run"): Promise<ControllerView> {
  await api<unknown>(`/api/projects/${encodeURIComponent(slug)}/crons/${encodeURIComponent(id)}/action`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ action }) });
  // trigger_job may return an execution receipt or boolean, not a job record.
  return getProjectCron(slug, id);
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
  // Remote harness support is read from the server-advertised capability
  // (`remoteLaunchPreflight` in missionLaunch.tsx) before this POST; nothing is
  // hardcoded here. The typed remote contract owns harness validation and
  // provisioning on the server. Send the exact selection; never generate shell
  // or proxy credentials here. Unsupported/older servers reject explicitly
  // without client fallback.
  return api("/api/control/missions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export interface QueuedMessage {
  id: string;
  content: string;
  /** Durable run-start proof, independent of mission busy/terminal status. */
  inflight?: boolean;
  mission_id?: string | null;
}
/** This endpoint reads the authenticated user's durable control queue. */
export async function listQueuedMessages(missionId: string): Promise<QueuedMessage[]> {
  const rows = await api<QueuedMessage[]>(`/api/control/queue?mission_id=${encodeURIComponent(missionId)}`);
  if (!Array.isArray(rows) || rows.some(row => !row || typeof row.id !== "string" || typeof row.content !== "string")) throw new Error("Invalid queue response");
  return rows.filter(row => row.mission_id === missionId);
}

export class MessageRejectedError extends Error {}

export async function sendMissionMessage(
  id: string,
  text: string,
  attachments?: MissionAttachment[],
  clientMessageId: string = crypto.randomUUID(),
): Promise<{ id: string; queued: boolean; message_accepted?: boolean }> {
  const receipt = await api<{ id: string; queued: boolean; message_accepted?: boolean }>("/api/control/message", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content: text, mission_id: id, client_message_id: clientMessageId, ...(attachments?.length ? { attachments } : {}) }),
  });
  if (receipt.message_accepted === false) throw new MessageRejectedError("Message was not accepted. Your draft is kept.");
  if (typeof receipt.id !== "string" || !receipt.id || typeof receipt.queued !== "boolean") throw new Error("Invalid message receipt. Your draft is kept.");
  return receipt;
}

export interface ProjectSteer {
  id: string;
  body: string;
  created_at: string;
  consumed_at?: string | null;
  origin: string;
}

export interface ProjectSteers {
  slug?: string;
  pending: ProjectSteer[];
  recent: ProjectSteer[];
}

export async function getProjectSteers(slug: string): Promise<ProjectSteers> {
  const result = await api<ProjectSteers>(`/api/projects/${encodeURIComponent(slug)}/steers`);
  if (!Array.isArray(result.pending) || !Array.isArray(result.recent)) throw new Error("Steer inbox unavailable");
  return result;
}

export async function addProjectSteer(slug: string, body: string, origin = "orb"): Promise<ProjectSteers> {
  return api(`/api/projects/${encodeURIComponent(slug)}/steers`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ body, origin }),
  });
}

export async function appendClientTranscript(id: string, role: "user" | "assistant", content: string, eventId = crypto.randomUUID()): Promise<void> {
  await api(`/api/control/missions/${id}/client-transcript`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id: eventId, role, content }),
  });
}

export async function setClientMissionStatus(id: string, status: "completed" | "failed" | "interrupted" | "awaiting_user"): Promise<void> {
  await api(`/api/control/missions/${id}/client-status`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ status }),
  });
}

export async function cancelMission(id: string): Promise<void> {
  await api<void>(`/api/control/missions/${id}/cancel`, { method: "POST" });
}

/** Next-turn settings. The mission must be idle; a running turn returns 409.
 * `model_effort: ""` clears the override back to the backend default — the
 * core's `normalize_string_patch` trims an empty string to a clear, while an
 * omitted field leaves the stored effort untouched. */
export async function updateMissionSettings(
  id: string,
  body: { model_override?: string; backend?: string; model_effort?: string },
): Promise<Mission> {
  return api(`/api/control/missions/${id}/settings`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

/**
 * A project's autonomy grant (`GET|POST /api/projects/:slug/grant`).
 * `parallel_missions` is the *project* concurrency limit enforced by
 * `create_mission` in `src/api/control/mod.rs`: above 0, a launch is refused
 * once that many of the project's own missions are unfinished. It is a
 * different limit from the backend-global `max_parallel_missions` setting.
 */
export interface ProjectGrant {
  merge_authority?: string | null;
  budget_per_tick?: string | null;
  parallel_missions?: number | null;
  pause_reason?: string | null;
  resume_condition?: string | null;
  material_bar?: string | null;
  answered_at?: string | null;
  /** observe | propose | act_reversible | act_full */
  autonomy_level?: string | null;
}

export async function getProjectGrant(slug: string): Promise<ProjectGrant | null> {
  const data = await api<{ slug: string; grant: ProjectGrant | null }>(
    `/api/projects/${encodeURIComponent(slug)}/grant`,
  );
  return data.grant ?? null;
}

/**
 * "No limit", as the core stores it. `set_grant` upserts every column with
 * `COALESCE(excluded.x, project_grant.x)`, so a JSON `null` *preserves* the
 * stored value rather than clearing it. `create_mission` only enforces a cap
 * when it is `> 0`, so 0 is the value that actually turns the limit off.
 */
export const NO_PROJECT_LIMIT = 0;

/** A stored cap that is not actually enforced (unset, 0 or negative). */
export function projectLimitOf(grant: ProjectGrant | null | undefined): number | null {
  const value = grant?.parallel_missions;
  return typeof value === "number" && value > 0 ? value : null;
}

/**
 * Narrow update of the one field this UI owns. The core's upsert already
 * preserves every column it is not given, atomically — so sending only
 * `parallel_missions` cannot revert an autonomy or merge-authority change made
 * between a read and this write, and needs no extra read to begin with.
 */
export async function setProjectLimit(slug: string, parallel_missions: number): Promise<ProjectGrant | null> {
  const data = await api<{ slug: string; grant: ProjectGrant | null }>(
    `/api/projects/${encodeURIComponent(slug)}/grant`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ parallel_missions }),
    },
  );
  return data.grant ?? null;
}

/** Backend-global execution settings (`GET|PUT /api/settings`). */
export interface GlobalSettings {
  /** Backend-wide mission concurrency. Distinct from a project's grant cap. */
  max_parallel_missions?: number | null;
  max_concurrent_tasks?: number | null;
}

export async function getGlobalSettings(): Promise<GlobalSettings> {
  return api("/api/settings");
}

/** PUT /api/settings is a patch: the core re-reads current settings and only
 * overwrites the fields present in the body. */
export async function updateGlobalSettings(patch: GlobalSettings): Promise<GlobalSettings> {
  return api("/api/settings", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
}

/**
 * Mission statuses that occupy a project's `parallel_missions` slot, mirroring
 * `campaign_slot_held_by` in `src/api/control/mod.rs`. Terminal missions free
 * their slot, so this is what a cap of N is counted against.
 */
export const CAP_SLOT_STATUSES = new Set([
  "pending",
  "active",
  "awaiting_user",
  "waiting_background",
  "paused",
]);

export function holdsCapSlot(status: string | null | undefined): boolean {
  return CAP_SLOT_STATUSES.has((status ?? "").toLowerCase());
}

/** A conversation fork preserves the source workspace and never switches the source harness. */
export async function forkMission(id: string, body: { backend: string; model_override: string; model_effort: string; idempotency_key: string }): Promise<Mission> {
  try {
    return await api<Mission>(`/api/control/missions/${encodeURIComponent(id)}/fork`, {
      method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
    });
  } catch (e) {
    if (e instanceof ApiError && [404, 405].includes(e.status)) throw new Error("This backend needs the conversation-fork update. The original mission has not been changed.");
    throw e;
  }
}
