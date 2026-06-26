'use client';

import { useState, useCallback, useEffect, useMemo } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  listAIProviders,
  listAIProviderTypes,
  updateAIProvider,
  deleteAIProvider,
  authenticateAIProvider,
  setDefaultAIProvider,
  getProviderUsage,
  refreshProviderUsage,
  getAllProviderUsage,
  AIProvider,
  AIProviderTypeInfo,
  ProviderUsage,
} from '@/lib/api';
import {
  Cpu,
  Plus,
  Trash2,
  Star,
  ExternalLink,
  Loader,
  Key,
  BarChart3,
  ChevronUp,
  RefreshCw,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { AddProviderModal } from '@/components/ui/add-provider-modal';
import {
  ReconnectProviderModal,
  providerSupportsOAuthReconnect,
} from '@/components/ui/reconnect-provider-modal';
import { AsyncButton } from '@/components/ui/async-button';
import { UsageOverview } from '@/components/ui/usage-overview';
import type { UsageWindow } from '@/lib/api';

const providerConfig: Record<string, { color: string; icon: string }> = {
  anthropic: { color: 'bg-orange-500/10 text-orange-400', icon: '🧠' },
  openai: { color: 'bg-emerald-500/10 text-emerald-400', icon: '🤖' },
  google: { color: 'bg-blue-500/10 text-blue-400', icon: '🔮' },
  'amazon-bedrock': { color: 'bg-amber-500/10 text-amber-400', icon: '☁️' },
  azure: { color: 'bg-sky-500/10 text-sky-400', icon: '⚡' },
  'open-router': { color: 'bg-purple-500/10 text-purple-400', icon: '🔀' },
  mistral: { color: 'bg-indigo-500/10 text-indigo-400', icon: '🌪️' },
  groq: { color: 'bg-pink-500/10 text-pink-400', icon: '⚡' },
  xai: { color: 'bg-slate-500/10 text-slate-400', icon: '𝕏' },
  zai: { color: 'bg-cyan-500/10 text-cyan-400', icon: 'Z' },
  minimax: { color: 'bg-teal-500/10 text-teal-400', icon: 'M' },
  'github-copilot': { color: 'bg-gray-500/10 text-gray-400', icon: '🐙' },
  'deep-infra': { color: 'bg-blue-500/10 text-blue-400', icon: '🔗' },
  cerebras: { color: 'bg-lime-500/10 text-lime-400', icon: 'C' },
  'together-ai': { color: 'bg-orange-500/10 text-orange-400', icon: '🤝' },
  perplexity: { color: 'bg-cyan-500/10 text-cyan-400', icon: '🔍' },
  cohere: { color: 'bg-rose-500/10 text-rose-400', icon: '💬' },
  kimi: { color: 'bg-violet-500/10 text-violet-400', icon: '🌙' },
  custom: { color: 'bg-white/10 text-white/60', icon: '🔧' },
};

function getProviderConfig(type: string) {
  return providerConfig[type] || providerConfig.custom;
}

const defaultProviderTypes: AIProviderTypeInfo[] = [
  { id: 'anthropic', name: 'Anthropic', uses_oauth: true, env_var: 'ANTHROPIC_API_KEY' },
  { id: 'openai', name: 'OpenAI', uses_oauth: true, env_var: 'OPENAI_API_KEY' },
  { id: 'google', name: 'Google AI', uses_oauth: true, env_var: 'GOOGLE_API_KEY' },
  { id: 'open-router', name: 'OpenRouter', uses_oauth: false, env_var: 'OPENROUTER_API_KEY' },
  { id: 'groq', name: 'Groq', uses_oauth: false, env_var: 'GROQ_API_KEY' },
  { id: 'mistral', name: 'Mistral AI', uses_oauth: false, env_var: 'MISTRAL_API_KEY' },
  { id: 'xai', name: 'xAI', uses_oauth: true, env_var: 'XAI_API_KEY' },
  { id: 'zai', name: 'Z.AI', uses_oauth: false, env_var: 'ZHIPU_API_KEY' },
  { id: 'minimax', name: 'Minimax', uses_oauth: false, env_var: 'MINIMAX_API_KEY' },
  { id: 'github-copilot', name: 'GitHub Copilot', uses_oauth: true, env_var: null },
  { id: 'kimi', name: 'Kimi', uses_oauth: true, env_var: null },
];

/** Format a number with commas */
function fmt(n: number | undefined | null): string {
  if (n == null) return '-';
  return n.toLocaleString();
}

/** Format a reset time (ISO string or relative like "2s", "1m30s") */
function fmtReset(v: string | undefined | null): string {
  if (!v) return '-';
  // Try ISO timestamp
  const d = new Date(v);
  if (!isNaN(d.getTime())) {
    const diff = d.getTime() - Date.now();
    if (diff <= 0) return 'now';
    if (diff < 60_000) return `${Math.ceil(diff / 1000)}s`;
    if (diff < 3600_000) return `${Math.ceil(diff / 60_000)}m`;
    return `${Math.round(diff / 3600_000)}h`;
  }
  // Already a relative string
  return v;
}

/** Format a unix epoch-seconds reset time as a relative "in Xh" string. */
function fmtResetEpoch(secs: number | undefined | null): string {
  if (secs == null) return '-';
  return fmtReset(new Date(secs * 1000).toISOString());
}

/**
 * Usage bar: fills the *consumed* portion of a limit.
 *
 * `remaining` is how much is LEFT (e.g. `tokens_remaining`, or
 * `(1 - utilization) * 100` for percentage windows). The bar shows
 * `limit - remaining` consumed. (Previously this prop was named `used`, which
 * was misleading — every call site has always passed a remaining value.)
 */
function UsageBar({
  remaining,
  limit,
  label,
}: {
  remaining?: number | null;
  limit?: number | null;
  label: string;
}) {
  if (limit == null && remaining == null) return null;
  const left = remaining ?? 0;
  const total = limit ?? 0;
  const usedCount = total - left;
  const pct = total > 0 ? Math.min(100, (usedCount / total) * 100) : 0;
  const color = pct > 90 ? 'bg-red-400' : pct > 70 ? 'bg-amber-400' : 'bg-emerald-400';

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between text-[11px]">
        <span className="text-white/40">{label}</span>
        <span className="text-white/60 font-mono">
          {fmt(usedCount)} / {fmt(total)}
        </span>
      </div>
      <div className="h-1 rounded-full bg-white/[0.06] overflow-hidden">
        <div className={cn('h-full rounded-full transition-all', color)} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

/** Render provider-specific usage details */
function UsageDetails({ usage, loading }: { usage: ProviderUsage | null; loading: boolean }) {
  if (loading) {
    return (
      <div className="flex items-center justify-center py-4">
        <Loader className="h-4 w-4 animate-spin text-white/30" />
        <span className="ml-2 text-xs text-white/30">Fetching usage...</span>
      </div>
    );
  }

  if (!usage) return null;

  if (usage.error) {
    return (
      <div className="py-2 text-xs text-red-400/70">{usage.error}</div>
    );
  }

  const type = usage.provider_type;

  return (
    <div className="space-y-3">
      {/* Account info row */}
      <div className="flex items-center gap-3 flex-wrap">
        {usage.account_email && (
          <div className="text-[11px] text-white/50">
            <span className="text-white/30">Account:</span> {usage.account_email}
          </div>
        )}
        {usage.account_name && (
          <div className="text-[11px] text-white/50">
            <span className="text-white/30">Name:</span> {usage.account_name}
          </div>
        )}
        {usage.organization && (
          <div className="text-[11px] text-white/50">
            <span className="text-white/30">Org:</span> {usage.organization}
          </div>
        )}
      </div>

      {/* Anthropic unified rate limits (2025+) */}
      {type === 'anthropic' && usage.unified_status && (
        <div className="space-y-2">
          {usage.organization_id && (
            <div className="text-[11px] text-white/50">
              <span className="text-white/30">Org ID:</span>{' '}
              <span className="font-mono text-[10px]">{usage.organization_id}</span>
            </div>
          )}
          <div className="flex items-center gap-2">
            <span className="text-[11px] text-white/40">Status:</span>
            <span className={cn(
              'text-[11px] font-medium',
              usage.unified_status === 'ok' ? 'text-emerald-400' :
              usage.unified_status === 'warning' ? 'text-amber-400' : 'text-red-400'
            )}>
              {usage.unified_status}
            </span>
          </div>
          {usage.unified_5h_utilization != null && (
            <UsageBar
              remaining={Math.round((1 - usage.unified_5h_utilization) * 100)}
              limit={100}
              label={`5h window (${usage.unified_5h_status || ''})`}
            />
          )}
          {usage.unified_7d_utilization != null && (
            <UsageBar
              remaining={Math.round((1 - usage.unified_7d_utilization) * 100)}
              limit={100}
              label={`7d window (${usage.unified_7d_status || ''})`}
            />
          )}
          <div className="flex gap-4 text-[10px] text-white/30 flex-wrap">
            {usage.unified_5h_reset && (
              <span>5h reset: {fmtReset(usage.unified_5h_reset)}</span>
            )}
            {usage.unified_7d_reset && (
              <span>7d reset: {fmtReset(usage.unified_7d_reset)}</span>
            )}
            {usage.unified_representative_claim && (
              <span>Claim: {usage.unified_representative_claim}</span>
            )}
            {usage.unified_fallback_pct != null && (
              <span>Fallback: {usage.unified_fallback_pct}%</span>
            )}
          </div>
        </div>
      )}

      {/* Anthropic legacy rate limits */}
      {type === 'anthropic' && !usage.unified_status && usage.requests_limit != null && (
        <div className="space-y-2">
          <UsageBar remaining={usage.requests_remaining} limit={usage.requests_limit} label="Requests" />
          <UsageBar remaining={usage.tokens_remaining} limit={usage.tokens_limit} label="Tokens" />
          <UsageBar remaining={usage.input_tokens_remaining} limit={usage.input_tokens_limit} label="Input tokens" />
          <UsageBar remaining={usage.output_tokens_remaining} limit={usage.output_tokens_limit} label="Output tokens" />
        </div>
      )}

      {/* OpenAI style rate limits */}
      {type === 'openai' && usage.requests_limit != null && (
        <div className="space-y-2">
          <UsageBar remaining={usage.requests_remaining} limit={usage.requests_limit} label="Requests" />
          <UsageBar remaining={usage.tokens_remaining} limit={usage.tokens_limit} label="Tokens" />
          <div className="flex gap-4 text-[10px] text-white/30">
            {usage.requests_reset && (
              <span>Requests reset: {fmtReset(usage.requests_reset)}</span>
            )}
            {usage.tokens_reset && (
              <span>Tokens reset: {fmtReset(usage.tokens_reset)}</span>
            )}
          </div>
        </div>
      )}

      {/* Codex (OpenAI ChatGPT subscription) usage & limits */}
      {type === 'openai' && usage.codex_primary_used_percent != null && (
        <div className="space-y-2">
          <div className="flex items-center gap-3 flex-wrap text-[11px] text-white/50">
            {usage.codex_plan_type && (
              <span><span className="text-white/30">Plan:</span> {usage.codex_plan_type}</span>
            )}
            {usage.codex_active_limit && (
              <span><span className="text-white/30">Tier:</span> {usage.codex_active_limit}</span>
            )}
            {usage.codex_source === 'passive' && (
              <span className="text-amber-400/60">(from recent traffic)</span>
            )}
          </div>
          {usage.codex_primary_used_percent != null && (
            <UsageBar
              remaining={Math.round(100 - usage.codex_primary_used_percent)}
              limit={100}
              label="5h window"
            />
          )}
          {usage.codex_secondary_used_percent != null && (
            <UsageBar
              remaining={Math.round(100 - usage.codex_secondary_used_percent)}
              limit={100}
              label="Weekly window"
            />
          )}
          <div className="flex gap-4 text-[10px] text-white/30 flex-wrap">
            {usage.codex_primary_reset_at != null && (
              <span>5h reset: {fmtResetEpoch(usage.codex_primary_reset_at)}</span>
            )}
            {usage.codex_secondary_reset_at != null && (
              <span>Weekly reset: {fmtResetEpoch(usage.codex_secondary_reset_at)}</span>
            )}
            {usage.codex_credits_unlimited === false && usage.codex_credits_balance != null && (
              <span>Credits: {usage.codex_credits_balance}</span>
            )}
            {usage.codex_credits_unlimited === true && <span>Credits: unlimited</span>}
          </div>
        </div>
      )}

      {/* OpenAI connected without rate limits (OAuth without API key, no Codex data) */}
      {type === 'openai' &&
        usage.status === 'connected' &&
        usage.requests_limit == null &&
        usage.codex_primary_used_percent == null && (
          <div className="text-[11px] text-emerald-400/70">Connected via OAuth</div>
        )}

      {/* Cerebras style rate limits */}
      {type === 'cerebras' && (
        <div className="space-y-2">
          <UsageBar remaining={usage.requests_remaining_day} limit={usage.requests_limit_day} label="Requests (daily)" />
          <UsageBar remaining={usage.tokens_remaining_minute} limit={usage.tokens_limit_minute} label="Tokens (per minute)" />
          <div className="flex gap-4 text-[10px] text-white/30">
            {usage.requests_reset_day && (
              <span>Daily reset: {fmtReset(usage.requests_reset_day)}</span>
            )}
            {usage.tokens_reset_minute && (
              <span>Minute reset: {fmtReset(usage.tokens_reset_minute)}</span>
            )}
          </div>
        </div>
      )}

      {/* Minimax coding plan — per-category 5h + weekly windows (remaining %) */}
      {type === 'minimax' && Array.isArray(usage.model_usage) && usage.model_usage.length > 0 && (
        <div className="space-y-3">
          {usage.model_usage.map((m) => (
            <div key={m.model} className="space-y-1">
              <div className="text-[11px] text-white/50 font-medium">{m.model}</div>
              <UsageBar remaining={m.interval_remaining_percent} limit={100} label="5h window" />
              <UsageBar remaining={m.weekly_remaining_percent} limit={100} label="Weekly" />
              <div className="flex gap-4 text-[10px] text-white/30">
                {m.interval_reset > 0 && (
                  <span>5h reset: {fmtResetEpoch(m.interval_reset)}</span>
                )}
                {m.weekly_reset > 0 && (
                  <span>Weekly reset: {fmtResetEpoch(m.weekly_reset)}</span>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Minimax connected without model data */}
      {type === 'minimax' && usage.status === 'connected' &&
        !(Array.isArray(usage.model_usage) && usage.model_usage.length > 0) && (
        <div className="text-[11px] text-emerald-400/70">Connected</div>
      )}

      {/* Z.AI GLM coding plan — 5h + weekly token windows (percent used) */}
      {type === 'zai' && usage.zai_weekly_used_percent != null && (
        <div className="space-y-2">
          {usage.zai_plan && (
            <div className="text-[11px] text-white/50">
              <span className="text-white/30">Plan:</span> {usage.zai_plan}
            </div>
          )}
          {usage.zai_5h_used_percent != null && (
            <UsageBar remaining={100 - usage.zai_5h_used_percent} limit={100} label="5h window" />
          )}
          <UsageBar remaining={100 - usage.zai_weekly_used_percent} limit={100} label="Weekly" />
          <div className="flex gap-4 text-[10px] text-white/30">
            {usage.zai_5h_reset != null && usage.zai_5h_reset > 0 && (
              <span>5h reset: {fmtResetEpoch(usage.zai_5h_reset)}</span>
            )}
            {usage.zai_weekly_reset != null && usage.zai_weekly_reset > 0 && (
              <span>Weekly reset: {fmtResetEpoch(usage.zai_weekly_reset)}</span>
            )}
          </div>
        </div>
      )}

      {/* Z.AI connected without quota data (non-coding-plan key) */}
      {type === 'zai' && usage.status === 'connected' && usage.zai_weekly_used_percent == null && (
        <div className="text-[11px] text-emerald-400/70">Connected</div>
      )}

      {/* Google - account info only */}
      {type === 'google' && usage.status === 'connected' && !usage.account_email && (
        <div className="text-[11px] text-emerald-400/70">Connected</div>
      )}

      {/* Generic connected status */}
      {!['anthropic', 'openai', 'cerebras', 'minimax', 'zai', 'google'].includes(type) && usage.status === 'connected' && (
        <div className="text-[11px] text-emerald-400/70">Connected</div>
      )}
    </div>
  );
}

export default function ProvidersPage() {
  const [showAddModal, setShowAddModal] = useState(false);
  const [reconnectProvider, setReconnectProvider] = useState<AIProvider | null>(null);
  const [authenticatingProviderId, setAuthenticatingProviderId] = useState<string | null>(null);
  const [editingProvider, setEditingProvider] = useState<string | null>(null);
  const [expandedProvider, setExpandedProvider] = useState<string | null>(null);
  const [usageData, setUsageData] = useState<Record<string, ProviderUsage>>({});
  const [usageLoading, setUsageLoading] = useState<Record<string, boolean>>({});
  const [usageWindow, setUsageWindow] = useState<UsageWindow>('7d');
  const [editForm, setEditForm] = useState<{
    name?: string;
    label?: string;
    google_project_id?: string;
    api_key?: string;
    base_url?: string;
    enabled?: boolean;
  }>({});

  const { data: providers = [], isLoading: providersLoading, mutate: mutateProviders } = useSWR(
    'ai-providers',
    listAIProviders,
    { revalidateOnFocus: false }
  );

  const { data: providerTypes = defaultProviderTypes } = useSWR(
    'ai-provider-types',
    listAIProviderTypes,
    { revalidateOnFocus: false, fallbackData: defaultProviderTypes }
  );

  // Pull the entire bulk cache so the rate-limit panel is instant the moment
  // the user expands a provider. Refreshes itself on a slow tick — the server
  // already refreshes in the background, so we just need to read the snapshot.
  const { data: bulkUsage, mutate: mutateBulkUsage } = useSWR(
    'ai-providers-usage-bulk',
    getAllProviderUsage,
    { revalidateOnFocus: false, refreshInterval: 60_000 }
  );

  // Merge the bulk snapshot into local usage state — only for providers we
  // haven't already fetched fresh (or that have errored) so an explicit
  // refresh doesn't get overwritten by the cache.
  const cachedEntries = bulkUsage?.entries;
  const cachedSeen = useMemo(() => {
    if (!cachedEntries) return {};
    const next: Record<string, ProviderUsage> = {};
    for (const [k, v] of Object.entries(cachedEntries)) {
      next[k] = v;
    }
    return next;
  }, [cachedEntries]);

  useEffect(() => {
    if (!cachedEntries) return;
    setUsageData((prev) => {
      let changed = false;
      const next = { ...prev };
      for (const [id, usage] of Object.entries(cachedEntries)) {
        if (next[id] !== usage) {
          next[id] = usage;
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [cachedEntries]);

  const fetchUsage = useCallback(async (providerId: string, force = false) => {
    setUsageLoading((prev) => ({ ...prev, [providerId]: true }));
    try {
      const data = force
        ? await refreshProviderUsage(providerId)
        : await getProviderUsage(providerId);
      setUsageData((prev) => ({ ...prev, [providerId]: data }));
      if (force) {
        // Keep the bulk snapshot in sync so the periodic poll doesn't roll
        // this row back to the prior error within the next 60s.
        mutateBulkUsage(
          (current) =>
            current
              ? { ...current, entries: { ...current.entries, [providerId]: data } }
              : current,
          { revalidate: false }
        );
      }
    } catch {
      setUsageData((prev) => ({
        ...prev,
        [providerId]: {
          provider_type: '',
          provider_name: '',
          error: 'Failed to fetch usage',
        },
      }));
    } finally {
      setUsageLoading((prev) => ({ ...prev, [providerId]: false }));
    }
  }, [mutateBulkUsage]);

  const toggleUsage = useCallback(
    (providerId: string) => {
      if (expandedProvider === providerId) {
        setExpandedProvider(null);
      } else {
        setExpandedProvider(providerId);
        // Seed from the bulk cache immediately for an instant render…
        if (cachedSeen[providerId]) {
          setUsageData((prev) => ({ ...prev, [providerId]: cachedSeen[providerId] }));
          return;
        }
        // …then fetch via the cached endpoint (cheap, server returns cached
        // copy if fresh).
        fetchUsage(providerId);
      }
    },
    [expandedProvider, fetchUsage, cachedSeen]
  );

  // After a fresh usage probe, surface the real provider health rather than a
  // misleading "authenticated" message, and patch the cached snapshots so the
  // status indicator doesn't revert to the stale error before the next poll.
  const probeProviderHealth = async (providerId: string) => {
    const fresh = await refreshProviderUsage(providerId);
    setUsageData((prev) => ({ ...prev, [providerId]: fresh }));
    mutateBulkUsage(
      (current) =>
        current
          ? { ...current, entries: { ...current.entries, [providerId]: fresh } }
          : current,
      { revalidate: false }
    );
    return fresh;
  };

  const handleReconnectSuccess = async (providerId: string) => {
    mutateProviders();
    try {
      const fresh = await probeProviderHealth(providerId);
      const stillBroken =
        !!fresh?.error ||
        (typeof fresh?.status === 'string' &&
          !['connected', 'ok'].includes(fresh.status));
      if (stillBroken) {
        toast.error(
          `Reconnected, but provider check still fails: ${String(
            fresh?.error || fresh?.status || 'unknown error'
          )}`
        );
      } else {
        toast.success('Provider reconnected');
      }
    } catch {
      // Health probe is best-effort; the reconnect itself already succeeded, so
      // still surface success rather than leaving the action without feedback.
      toast.success('Provider reconnected');
    }
  };

  const handleAuthenticate = async (provider: AIProvider) => {
    // OAuth-backed providers (e.g. xAI/Grok, Anthropic) must go through the real
    // OAuth flow to mint fresh tokens. The /auth endpoint only checks whether
    // credentials *exist* (expired tokens still count), so it would report
    // success and never open the authorization link. Route them to the OAuth
    // reconnect modal instead. API-key providers keep the legacy path below.
    if (providerSupportsOAuthReconnect(provider)) {
      setReconnectProvider(provider);
      return;
    }

    setAuthenticatingProviderId(provider.id);
    try {
      const result = await authenticateAIProvider(provider.id);
      if (result.success) {
        mutateProviders();
        // Auth succeeding does not mean the provider is healthy — a 429 or
        // network error from the live usage check will still leave the row
        // red. Force a fresh usage probe so the status indicator reflects
        // reality instead of the stale cached error, then surface the real
        // outcome rather than a misleading "authenticated" message.
        const fresh = await probeProviderHealth(provider.id);
        const stillBroken =
          !!fresh?.error ||
          (typeof fresh?.status === 'string' &&
            !['connected', 'ok'].includes(fresh.status));
        if (stillBroken) {
          toast.error(
            `Re-authenticated, but provider check still fails: ${String(
              fresh?.error || fresh?.status || 'unknown error'
            )}`
          );
        } else {
          toast.success(result.message);
        }
      } else {
        if (result.auth_url) {
          window.open(result.auth_url, '_blank');
          toast.info(result.message);
        } else {
          toast.error(result.message);
        }
      }
    } catch (err) {
      toast.error(
        `Authentication failed: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setAuthenticatingProviderId(null);
    }
  };

  const handleSetDefault = async (id: string) => {
    try {
      await setDefaultAIProvider(id);
      toast.success('Default provider updated');
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to set default: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleDeleteProvider = async (id: string) => {
    if (!confirm('Are you sure you want to delete this provider?')) return;
    try {
      await deleteAIProvider(id);
      toast.success('Provider removed');
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to delete: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleStartEdit = (provider: AIProvider) => {
    setEditingProvider(provider.id);
    setEditForm({
      name: provider.name,
      label: provider.label || '',
      google_project_id: provider.google_project_id ?? '',
      api_key: '',
      base_url: provider.base_url || '',
      enabled: provider.enabled,
    });
  };

  const handleSaveEdit = async () => {
    if (!editingProvider) return;

    try {
      await updateAIProvider(editingProvider, {
        name: editForm.name,
        label: editForm.label?.trim() || null,
        google_project_id:
          editForm.google_project_id === ''
            ? null
            : editForm.google_project_id || undefined,
        api_key: editForm.api_key || undefined,
        base_url: editForm.base_url || undefined,
        enabled: editForm.enabled,
      });
      toast.success('Provider updated');
      setEditingProvider(null);
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to update: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleCancelEdit = () => {
    setEditingProvider(null);
    setEditForm({});
  };

  return (
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <AddProviderModal
        open={showAddModal}
        onClose={() => setShowAddModal(false)}
        onSuccess={() => mutateProviders()}
        providerTypes={providerTypes}
      />

      <ReconnectProviderModal
        provider={reconnectProvider}
        open={reconnectProvider !== null}
        onClose={() => setReconnectProvider(null)}
        onSuccess={(id) => handleReconnectSuccess(id)}
      />

      <div className="w-full max-w-4xl space-y-6">
        <header>
          <h1 className="text-xl font-semibold text-white">AI Providers</h1>
          <p className="mt-1 text-sm text-white/50">
            Manage API keys and authentication
          </p>
        </header>

        <UsageOverview window={usageWindow} onWindowChange={setUsageWindow} />

        <section className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
          <div className="flex flex-wrap items-center justify-between gap-3 mb-4">
            <div className="flex items-center gap-3 min-w-0">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-violet-500/10 flex-shrink-0">
                <Cpu className="h-5 w-5 text-violet-400" />
              </div>
              <div className="min-w-0">
                <h2 className="text-sm font-medium text-white">Configured Providers</h2>
                <p className="text-xs text-white/40 truncate">
                  Inference providers for OpenCode, Claude Code, Gemini, and Codex
                </p>
              </div>
            </div>
            <button
              onClick={() => setShowAddModal(true)}
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer flex-shrink-0"
            >
              <Plus className="h-3 w-3" />
              Add Provider
            </button>
          </div>

          <div className="space-y-2">
            {providersLoading ? (
              <div aria-busy="true" aria-label="Loading providers" className="space-y-2">
                {Array.from({ length: 3 }).map((_, i) => (
                  <div
                    key={i}
                    className="rounded-lg border border-white/[0.06] bg-white/[0.01] animate-pulse"
                  >
                    <div className="flex items-center gap-3 px-3 py-2.5">
                      <div className="h-4 w-4 rounded bg-white/[0.06]" />
                      <div className="flex-1 min-w-0 space-y-1.5">
                        <div className="h-3 w-32 rounded bg-white/[0.06]" />
                        <div className="h-2.5 w-44 rounded bg-white/[0.04]" />
                      </div>
                      <div className="h-1.5 w-1.5 rounded-full bg-white/[0.06]" />
                      <div className="flex items-center gap-0.5">
                        {Array.from({ length: 3 }).map((__, j) => (
                          <div key={j} className="h-6 w-6 rounded-md bg-white/[0.04]" />
                        ))}
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            ) : providers.length === 0 ? (
              <div className="text-center py-8">
                <div className="flex justify-center mb-3">
                  <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-white/[0.04]">
                    <Cpu className="h-6 w-6 text-white/30" />
                  </div>
                </div>
                <p className="text-sm text-white/50 mb-1">No providers configured</p>
                <p className="text-xs text-white/30 mb-4">
                  Add an AI provider to enable inference capabilities
                </p>
                <button
                  onClick={() => setShowAddModal(true)}
                  className="inline-flex items-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer"
                >
                  <Plus className="h-3 w-3" />
                  Add Provider
                </button>
              </div>
            ) : (
              providers.map((provider) => {
                const config = getProviderConfig(provider.provider_type);
                const cachedUsage = usageData[provider.id] ?? cachedSeen[provider.id];
                const usageIndicatesDisconnected =
                  !!cachedUsage?.error ||
                  (typeof cachedUsage?.status === 'string' &&
                    !['connected', 'ok'].includes(cachedUsage.status));
                const effectiveStatus = usageIndicatesDisconnected
                  ? 'error'
                  : provider.status.type;
                const statusColor = effectiveStatus === 'connected'
                  ? 'bg-emerald-400'
                  : effectiveStatus === 'needs_auth'
                  ? 'bg-amber-400'
                  : 'bg-red-400';
                const statusTitle = usageIndicatesDisconnected
                  ? String(cachedUsage?.error || cachedUsage?.status || 'Provider usage check failed')
                  : provider.status.message || provider.status.reason || provider.status.type;
                const isExpanded = expandedProvider === provider.id;

                return (
                  <div
                    key={provider.id}
                    className="group rounded-lg border border-white/[0.06] bg-white/[0.01] hover:bg-white/[0.02] transition-colors"
                  >
                    {editingProvider === provider.id ? (
                      <div className="p-3 space-y-3">
                        <input
                          type="text"
                          value={editForm.name ?? ''}
                          onChange={(e) =>
                            setEditForm({ ...editForm, name: e.target.value })
                          }
                          placeholder="Name"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                        />
                        <input
                          type="text"
                          value={editForm.label ?? ''}
                          onChange={(e) =>
                            setEditForm({ ...editForm, label: e.target.value })
                          }
                          placeholder="Account label (e.g. Team, Personal)"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                        />
                        <input
                          type="password"
                          value={editForm.api_key ?? ''}
                          onChange={(e) =>
                            setEditForm({ ...editForm, api_key: e.target.value })
                          }
                          placeholder="New API key (leave empty to keep)"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                        />
                        <input
                          type="text"
                          value={editForm.base_url ?? ''}
                          onChange={(e) =>
                            setEditForm({ ...editForm, base_url: e.target.value })
                          }
                          placeholder="Base URL (optional)"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                        />
                        {provider.provider_type === 'google' && (
                          <input
                            type="text"
                            value={editForm.google_project_id ?? ''}
                            onChange={(e) =>
                              setEditForm({ ...editForm, google_project_id: e.target.value })
                            }
                            placeholder="Google Cloud project ID (required for Gemini)"
                            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                          />
                        )}
                        <div className="flex items-center justify-between pt-1">
                          <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                            <input
                              type="checkbox"
                              checked={editForm.enabled ?? true}
                              onChange={(e) =>
                                setEditForm({ ...editForm, enabled: e.target.checked })
                              }
                              className="rounded border-white/20 cursor-pointer"
                            />
                            Enabled
                          </label>
                          <div className="flex items-center gap-2">
                            <button
                              onClick={handleCancelEdit}
                              className="rounded-lg px-3 py-1.5 text-xs text-white/60 hover:text-white/80 transition-colors cursor-pointer"
                            >
                              Cancel
                            </button>
                            <button
                              onClick={handleSaveEdit}
                              className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer"
                            >
                              Save
                            </button>
                          </div>
                        </div>
                      </div>
                    ) : (
                      <>
                        <div className={cn(
                          'flex items-center gap-3 px-3 py-2.5',
                          !provider.enabled && 'opacity-40'
                        )}>
                          <span className="text-base">{config.icon}</span>
                          <div className="flex-1 min-w-0">
                            <span className="text-sm text-white/80 truncate block">
                              {provider.name}
                              {provider.label && (
                                <span className="ml-1.5 text-xs text-white/40">({provider.label})</span>
                              )}
                            </span>
                            {provider.account_email && (
                              <span className="text-[11px] text-white/35 truncate block">
                                {provider.account_email}
                              </span>
                            )}
                          </div>

                          {provider.use_for_backends && provider.use_for_backends.length > 0 && (
                            <div className="flex items-center gap-1">
                              {provider.use_for_backends.map((backend) => (
                                <span
                                  key={backend}
                                  className="px-1.5 py-0.5 text-[10px] rounded bg-white/[0.06] text-white/50"
                                >
                                  {backend === 'claudecode'
                                    ? 'Claude'
                                    : backend === 'opencode'
                                    ? 'OC'
                                    : backend === 'codex'
                                    ? 'Codex'
                                    : backend === 'gemini'
                                    ? 'Gemini'
                                    : backend === 'grok'
                                    ? 'Grok'
                                    : backend}
                                </span>
                              ))}
                            </div>
                          )}

                          <div className="flex items-center gap-2">
                            {provider.is_default && (
                              <Star className="h-3 w-3 text-indigo-400 fill-indigo-400" />
                            )}
                            <span
                              className={cn('h-1.5 w-1.5 rounded-full', statusColor)}
                              title={statusTitle}
                            />
                          </div>

                          <div className="flex items-center gap-0.5 opacity-60 group-hover:opacity-100 transition-opacity">
                            {(provider.status.type === 'connected' || cachedUsage) && (
                              <button
                                onClick={() => toggleUsage(provider.id)}
                                className={cn(
                                  'p-1.5 rounded-md hover:bg-white/[0.04] transition-colors cursor-pointer',
                                  usageIndicatesDisconnected
                                    ? 'text-red-400/70 hover:text-red-300'
                                    : 'text-white/30 hover:text-white/60'
                                )}
                                title={usageIndicatesDisconnected ? statusTitle : 'View usage'}
                              >
                                {isExpanded ? (
                                  <ChevronUp className="h-3.5 w-3.5" />
                                ) : (
                                  <BarChart3 className="h-3.5 w-3.5" />
                                )}
                              </button>
                            )}
                            {(effectiveStatus === 'needs_auth' || effectiveStatus === 'needs_reauth' || effectiveStatus === 'error') && (
                              <button
                                onClick={() => handleAuthenticate(provider)}
                                disabled={authenticatingProviderId === provider.id}
                                className={cn(
                                  'p-1.5 rounded-md hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50',
                                  effectiveStatus === 'needs_auth' || effectiveStatus === 'needs_reauth'
                                    ? 'text-amber-400'
                                    : 'text-red-400 hover:text-red-300'
                                )}
                                title={
                                  effectiveStatus === 'needs_auth'
                                    ? 'Connect'
                                    : effectiveStatus === 'needs_reauth'
                                    ? 'Reconnect'
                                    : `Reconnect (${
                                        provider.has_oauth
                                          ? 'OAuth'
                                          : provider.has_api_key
                                          ? 'API key'
                                          : 'auth'
                                      })`
                                }
                              >
                                {authenticatingProviderId === provider.id ? (
                                  <Loader className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <ExternalLink className="h-3.5 w-3.5" />
                                )}
                              </button>
                            )}
                            {!provider.is_default && provider.enabled && (
                              <AsyncButton
                                onClick={() => handleSetDefault(provider.id)}
                                className="p-1.5 rounded-md text-white/30 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:cursor-not-allowed"
                                title="Set as default"
                              >
                                <Star className="h-3.5 w-3.5" />
                              </AsyncButton>
                            )}
                            <button
                              onClick={() => handleStartEdit(provider)}
                              className="p-1.5 rounded-md text-white/30 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
                              title="Edit"
                            >
                              <Key className="h-3.5 w-3.5" />
                            </button>
                            <AsyncButton
                              onClick={() => handleDeleteProvider(provider.id)}
                              className="p-1.5 rounded-md text-white/30 hover:text-red-400 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:cursor-not-allowed"
                              title="Delete"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </AsyncButton>
                          </div>
                        </div>

                        {/* Expandable usage panel */}
                        {isExpanded && (
                          <div className="px-3 pb-3 border-t border-white/[0.04]">
                            <div className="flex items-center justify-between pt-2 pb-1">
                              <span className="text-[11px] text-white/30 uppercase tracking-wider">Usage & Limits</span>
                              <button
                                onClick={() => fetchUsage(provider.id, true)}
                                disabled={usageLoading[provider.id]}
                                className="p-1 rounded text-white/20 hover:text-white/50 transition-colors cursor-pointer disabled:opacity-50"
                                title="Refresh"
                              >
                                <RefreshCw className={cn('h-3 w-3', usageLoading[provider.id] && 'animate-spin')} />
                              </button>
                            </div>
                            <UsageDetails
                              usage={usageData[provider.id] ?? cachedSeen[provider.id] ?? null}
                              loading={usageLoading[provider.id] ?? false}
                            />
                          </div>
                        )}
                      </>
                    )}
                  </div>
                );
              })
            )}
          </div>
        </section>
      </div>
    </div>
  );
}
