import { api, connectionVersion } from "./api";

export interface ChainEntry {
  provider_id: string;
  model_id: string;
}

export interface ModelChain {
  id: string;
  name: string;
  entries: ChainEntry[];
  is_default: boolean;
  /**
   * Strip `<think>…</think>` blocks and stray orphan `</think>` tags from
   * responses routed through this chain. Useful for models (MiniMax, GLM) that
   * leak reasoning into `content`.
   */
  strip_thinking: boolean;
  created_at: string;
  updated_at: string;
}

export interface ResolvedEntry {
  provider_id: string;
  model_id: string;
  account_id: string;
  has_credentials: boolean;
  auth_kind: "api_key" | "oauth" | "none";
  has_base_url: boolean;
}

export interface RateLimitSnapshot {
  requests_limit: number | null;
  requests_remaining: number | null;
  requests_reset: string | null;
  tokens_limit: number | null;
  tokens_remaining: number | null;
  tokens_reset: string | null;
  input_tokens_limit: number | null;
  input_tokens_remaining: number | null;
  output_tokens_limit: number | null;
  output_tokens_remaining: number | null;
  updated_at: string;
}

export interface AccountHealthSnapshot {
  account_id: string;
  provider_id: string | null;
  is_healthy: boolean;
  cooldown_remaining_secs: number | null;
  consecutive_failures: number;
  last_failure_reason: string | null;
  last_failure_at: string | null;
  total_requests: number;
  total_successes: number;
  total_rate_limits: number;
  total_errors: number;
  avg_latency_ms: number | null;
  total_input_tokens: number;
  total_output_tokens: number;
  is_degraded: boolean;
  rate_limit_snapshot: RateLimitSnapshot | null;
}

export interface FallbackEvent {
  timestamp: string;
  chain_id: string;
  from_provider: string;
  from_model: string;
  from_account_id: string;
  reason: string;
  cooldown_secs: number | null;
  to_provider: string | null;
  latency_ms: number | null;
  attempt_number: number;
  chain_length: number;
}

export interface ChainTestResult {
  ok: boolean;
  status: number;
  response: {
    model?: string;
    choices?: { message: { content: string | null } }[];
    error?: { message?: string };
    [key: string]: unknown;
  };
}

export interface RoutingCatalog {
  providers: {
    id: string;
    name: string;
    models: { id: string; name: string }[];
  }[];
  configured_ids?: string[];
}
const root = "/api/model-routing";
const chainPath = (id: string) => `${root}/chains/${encodeURIComponent(id)}`;
// Memory-only, connection-scoped cache. Never reuse another server/account's data.
function cached<T>(load: () => Promise<T>, ttl: number) {
  let state: { version: number; at: number; value?: T; pending?: Promise<T> } | undefined;
  return (force = false): Promise<T> => {
    const version = connectionVersion();
    if (!state || state.version !== version) state = { version, at: 0 };
    const current = state;
    if (current.pending) return current.pending;
    if (!force && current.value !== undefined && Date.now() - current.at < ttl) return Promise.resolve(current.value);
    current.pending = load().then(value => {
      current.value = value;
      current.at = Date.now();
      return value;
    }).finally(() => { current.pending = undefined; });
    return current.pending;
  };
}
export const listChains = cached(() => api<ModelChain[]>(`${root}/chains`), 30_000);
export const listHealth = () => api<AccountHealthSnapshot[]>(`${root}/health`);
export const listEvents = () => api<FallbackEvent[]>(`${root}/events`);
export const routingCatalog = cached(() =>
  api<RoutingCatalog>(
    "/api/providers?include_all=true&include_unverified=true",
  ), 300_000);
export type ChainDraft = Pick<
  ModelChain,
  "id" | "name" | "entries" | "strip_thinking"
>;
export const saveChain = (draft: ChainDraft, existing: boolean) =>
  api<ModelChain>(existing ? chainPath(draft.id) : `${root}/chains`, {
    method: existing ? "PUT" : "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(draft),
  });
export const setDefaultChain = (id: string) =>
  api<ModelChain>(chainPath(id), {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ is_default: true }),
  });
export const deleteChain = (id: string) =>
  api(chainPath(id), { method: "DELETE" });
export const resolveChain = (id: string) =>
  api<ResolvedEntry[]>(`${chainPath(id)}/resolve`);
export const testChain = (id: string) =>
  api<ChainTestResult>(`${chainPath(id)}/test`, { method: "POST" });
export const clearCooldown = (id: string) =>
  api(`${root}/health/${encodeURIComponent(id)}/clear`, { method: "POST" });

export interface ModelDiscovery {
  connections: {
    provider_id: string;
    access_profile: string;
    status: string;
    source: "discovery" | "stale_discovery" | "snapshot";
    diagnostic?: string;
    checked_at: string;
    last_success?: { observed_at: string; completeness: "complete" | "partial"; models: { id: string }[] };
  }[];
}
export const modelDiscovery = () => api<ModelDiscovery>("/api/providers/discovery");
export const refreshModelDiscovery = () => api("/api/providers/catalog/refresh", { method: "POST" });
export function modelEvidence(discovery: ModelDiscovery | undefined, provider: string, model: string): string {
  const connections = discovery?.connections?.filter(c => c.provider_id === provider) ?? [];
  if (connections.some(c => c.status === "discovered" && c.last_success?.models.some(m => m.id === model))) return "Listed by provider";
  if (connections.some(c => c.last_success?.models.some(m => m.id === model))) return "Previously listed · refresh failed";
  return "Fallback / unverified";
}
