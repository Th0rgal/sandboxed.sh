/**
 * AI Providers API - Provider management and OAuth flows.
 */

import { apiGet, apiPost, apiPut, apiDel, apiFetch } from "./core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
  | "zai"
  | "minimax"
  | "github-copilot"
  | "amp"
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

/** Custom model definition for custom providers */
export interface CustomModel {
  id: string;
  name?: string;
  context_limit?: number;
  output_limit?: number;
}

export interface AIProvider {
  id: string;
  provider_type: AIProviderType;
  provider_type_name: string;
  name: string;
  /** Optional label to distinguish multiple accounts of the same provider type */
  label?: string | null;
  /** Priority order for fallback chains (lower = higher priority) */
  priority?: number;
  google_project_id?: string | null;
  has_api_key: boolean;
  has_oauth: boolean;
  base_url: string | null;
  /** Custom models for custom providers */
  custom_models?: CustomModel[] | null;
  /** Custom environment variable name for API key */
  custom_env_var?: string | null;
  /** NPM package for custom provider */
  npm_package?: string | null;
  enabled: boolean;
  is_default: boolean;
  uses_oauth: boolean;
  auth_methods: AIProviderAuthMethod[];
  status: AIProviderStatus;
  use_for_backends: string[];
  /** Account identifier (email) from the connected OAuth account */
  account_email?: string | null;
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

export interface BackendProviderResponse {
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

// ---------------------------------------------------------------------------
// Provider Model Types
// ---------------------------------------------------------------------------

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

export interface BackendModelOption {
  value: string;
  label: string;
  description?: string;
  provider_id?: string;
}

export interface BackendModelOptionsResponse {
  backends: Record<string, BackendModelOption[]>;
}

// ---------------------------------------------------------------------------
// API Functions
// ---------------------------------------------------------------------------

export async function listAIProviders(): Promise<AIProvider[]> {
  return apiGet("/api/ai/providers", "Failed to list AI providers");
}

export async function listAIProviderTypes(): Promise<AIProviderTypeInfo[]> {
  return apiGet("/api/ai/providers/types", "Failed to list AI provider types");
}

export async function getAIProvider(id: string): Promise<AIProvider> {
  return apiGet(`/api/ai/providers/${id}`, "Failed to get AI provider");
}

export async function createAIProvider(data: {
  provider_type: AIProviderType;
  name: string;
  /** Optional label to distinguish multiple accounts of the same provider type */
  label?: string;
  /** Priority order for fallback chains (lower = higher priority) */
  priority?: number;
  google_project_id?: string;
  api_key?: string;
  base_url?: string;
  enabled?: boolean;
  use_for_backends?: string[];
  /** Custom models for custom providers */
  custom_models?: CustomModel[];
  /** Custom environment variable name for API key */
  custom_env_var?: string;
  /** NPM package for custom provider */
  npm_package?: string;
}): Promise<AIProvider> {
  return apiPost("/api/ai/providers", data, "Failed to create AI provider");
}

export async function updateAIProvider(
  id: string,
  data: {
    name?: string;
    /** Optional label to distinguish multiple accounts of the same provider type */
    label?: string | null;
    /** Priority order for fallback chains (lower = higher priority) */
    priority?: number;
    google_project_id?: string | null;
    api_key?: string | null;
    base_url?: string | null;
    enabled?: boolean;
    use_for_backends?: string[];
    /** Account email — set by frontend when server-side userinfo fetch fails */
    account_email?: string;
  }
): Promise<AIProvider> {
  return apiPut(`/api/ai/providers/${id}`, data, "Failed to update AI provider");
}

export async function deleteAIProvider(id: string): Promise<void> {
  return apiDel(`/api/ai/providers/${id}`, "Failed to delete AI provider");
}

export async function getProviderForBackend(backendId: string): Promise<BackendProviderResponse> {
  return apiGet(`/api/ai/providers/for-backend/${backendId}`, "Failed to get provider for backend");
}

export async function authenticateAIProvider(id: string): Promise<AIProviderAuthResponse> {
  return apiPost(`/api/ai/providers/${id}/auth`, undefined, "Failed to authenticate AI provider");
}

export async function setDefaultAIProvider(id: string): Promise<AIProvider> {
  return apiPost(`/api/ai/providers/${id}/default`, undefined, "Failed to set default AI provider");
}

export async function getAuthMethods(id: string): Promise<AIProviderAuthMethod[]> {
  return apiGet(`/api/ai/providers/${id}/auth/methods`, "Failed to get auth methods");
}

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

export async function oauthCallback(
  id: string,
  methodIndex: number,
  code: string,
  useForBackends?: string[]
): Promise<AIProvider> {
  const res = await apiFetch(`/api/ai/providers/${id}/oauth/callback`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      method_index: methodIndex,
      code,
      use_for_backends: useForBackends,
    }),
  });
  if (!res.ok) {
    const error = await res.text();
    throw new Error(error || "Failed to complete OAuth");
  }
  return res.json();
}

// ---------------------------------------------------------------------------
// Usage / Rate Limits
// ---------------------------------------------------------------------------

export interface ProviderUsage {
  provider_type: string;
  provider_name: string;
  account_email?: string | null;
  account_name?: string | null;
  account_picture?: string | null;
  organization?: string | null;
  organization_id?: string | null;
  status?: string;
  error?: string;
  // Anthropic unified rate limits (2025+)
  unified_status?: string;
  unified_reset?: string;
  unified_5h_status?: string;
  unified_5h_reset?: string;
  unified_5h_utilization?: number;
  unified_7d_status?: string;
  unified_7d_reset?: string;
  unified_7d_utilization?: number;
  unified_representative_claim?: string;
  unified_fallback_pct?: number;
  unified_overage_status?: string;
  unified_overage_disabled_reason?: string;
  // Anthropic legacy / OpenAI style
  requests_limit?: number;
  requests_remaining?: number;
  requests_reset?: string;
  tokens_limit?: number;
  tokens_remaining?: number;
  tokens_reset?: string;
  input_tokens_limit?: number;
  input_tokens_remaining?: number;
  output_tokens_limit?: number;
  output_tokens_remaining?: number;
  // Cerebras style
  requests_limit_day?: number;
  requests_remaining_day?: number;
  requests_reset_day?: string;
  tokens_limit_minute?: number;
  tokens_remaining_minute?: number;
  tokens_reset_minute?: string;
  // Minimax coding plan
  coding_plan?: Record<string, unknown>;
  // Z.AI last call usage
  last_call_usage?: Record<string, unknown>;
  // Any additional fields
  [key: string]: unknown;
}

export async function getProviderUsage(id: string): Promise<ProviderUsage> {
  return apiGet(`/api/ai/providers/${id}/usage`, "Failed to get provider usage");
}

export async function listProviders(options?: { includeAll?: boolean }): Promise<ProvidersResponse> {
  const params = new URLSearchParams();
  if (options?.includeAll) {
    params.set("include_all", "true");
  }
  const query = params.toString();
  const res = await apiFetch(`/api/providers${query ? `?${query}` : ""}`);
  if (!res.ok) throw new Error("Failed to fetch providers");
  return res.json();
}

export async function listBackendModelOptions(options?: {
  includeAll?: boolean;
}): Promise<BackendModelOptionsResponse> {
  const params = new URLSearchParams();
  if (options?.includeAll) {
    params.set("include_all", "true");
  }
  const query = params.toString();
  const res = await apiFetch(
    `/api/providers/backend-models${query ? `?${query}` : ""}`
  );
  if (!res.ok) throw new Error("Failed to fetch backend model options");
  return res.json();
}
