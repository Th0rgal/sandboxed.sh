'use client';

import { useState, useMemo } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  listModelChains,
  createModelChain,
  updateModelChain,
  deleteModelChain,
  resolveModelChain,
  listAccountHealth,
  clearAccountCooldown,
  listFallbackEvents,
  getRtkStats,
  type ModelChain,
  type ChainEntry,
  type ResolvedEntry,
  type AccountHealthSnapshot,
  type FallbackEvent,
  type RtkStats,
} from '@/lib/api/model-routing';
import {
  listProxyApiKeys,
  createProxyApiKey,
  deleteProxyApiKey,
  type ProxyApiKeySummary,
} from '@/lib/api/proxy-keys';
import {
  listProviders,
  type Provider,
} from '@/lib/api/providers';
import { getRuntimeApiBase } from '@/lib/settings';
import {
  GitBranch,
  Plus,
  Trash2,
  Star,
  Loader,
  ChevronDown,
  ChevronRight,
  Heart,
  AlertTriangle,
  Clock,
  RotateCcw,
  ArrowDown,
  ArrowUp,
  Activity,
  ArrowRight,
  Key,
  Copy,
  Check,
  Pencil,
  Zap,
} from 'lucide-react';
import { cn } from '@/lib/utils';

// ─────────────────────────────────────────────────────────────────────────────
// Chain Entry Editor
// ─────────────────────────────────────────────────────────────────────────────

function EntryEditor({
  entries,
  onChange,
  providers,
}: {
  entries: ChainEntry[];
  onChange: (entries: ChainEntry[]) => void;
  providers: Provider[];
}) {
  const addEntry = () => {
    onChange([...entries, { provider_id: '', model_id: '' }]);
  };

  const removeEntry = (index: number) => {
    onChange(entries.filter((_, i) => i !== index));
  };

  const updateEntry = (index: number, field: keyof ChainEntry, value: string) => {
    const updated = entries.map((e, i) => {
      if (i !== index) return e;
      if (field === 'provider_id' && value !== e.provider_id) {
        return { ...e, provider_id: value, model_id: '' };
      }
      return { ...e, [field]: value };
    });
    onChange(updated);
  };

  const moveEntry = (index: number, direction: 'up' | 'down') => {
    const newIndex = direction === 'up' ? index - 1 : index + 1;
    if (newIndex < 0 || newIndex >= entries.length) return;
    const updated = [...entries];
    [updated[index], updated[newIndex]] = [updated[newIndex], updated[index]];
    onChange(updated);
  };

  const getModelsForProvider = (providerId: string) => {
    const provider = providers.find((p) => p.id === providerId);
    return provider?.models ?? [];
  };

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <span className="text-xs text-white/40">
          Fallback chain (tried in order)
        </span>
        <button
          onClick={addEntry}
          className="flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors cursor-pointer"
        >
          <Plus className="h-3 w-3" />
          Add entry
        </button>
      </div>
      {entries.map((entry, i) => {
        const models = getModelsForProvider(entry.provider_id);
        return (
          <div
            key={i}
            className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.01] px-2 py-1.5"
          >
            <span className="text-[10px] text-white/30 w-4 flex-shrink-0">
              {i + 1}.
            </span>
            <select
              value={entry.provider_id}
              onChange={(e) => updateEntry(i, 'provider_id', e.target.value)}
              className="flex-1 min-w-0 rounded border border-white/[0.06] bg-white/[0.02] px-2 py-1 text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
              style={{
                backgroundImage: "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                backgroundPosition: 'right 0.25rem center',
                backgroundRepeat: 'no-repeat',
                backgroundSize: '1.2em 1.2em',
                paddingRight: '1.5rem',
              }}
            >
              <option value="" className="bg-[#1a1a1a]">Select provider</option>
              {providers.map((p) => (
                <option key={p.id} value={p.id} className="bg-[#1a1a1a]">{p.name}</option>
              ))}
            </select>
            <span className="text-white/20">/</span>
            <select
              value={entry.model_id}
              onChange={(e) => updateEntry(i, 'model_id', e.target.value)}
              disabled={!entry.provider_id}
              className="flex-1 min-w-0 rounded border border-white/[0.06] bg-white/[0.02] px-2 py-1 text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                backgroundImage: "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                backgroundPosition: 'right 0.25rem center',
                backgroundRepeat: 'no-repeat',
                backgroundSize: '1.2em 1.2em',
                paddingRight: '1.5rem',
              }}
            >
              <option value="" className="bg-[#1a1a1a]">{entry.provider_id ? 'Select model' : 'Select provider first'}</option>
              {models.map((m) => (
                <option key={m.id} value={m.id} className="bg-[#1a1a1a]">{m.name || m.id}</option>
              ))}
            </select>
            <div className="flex items-center gap-0.5 flex-shrink-0">
              <button
                onClick={() => moveEntry(i, 'up')}
                disabled={i === 0}
                className="p-0.5 text-white/20 hover:text-white/60 disabled:opacity-20 cursor-pointer"
              >
                <ArrowUp className="h-3 w-3" />
              </button>
              <button
                onClick={() => moveEntry(i, 'down')}
                disabled={i === entries.length - 1}
                className="p-0.5 text-white/20 hover:text-white/60 disabled:opacity-20 cursor-pointer"
              >
                <ArrowDown className="h-3 w-3" />
              </button>
              <button
                onClick={() => removeEntry(i)}
                className="p-0.5 text-white/20 hover:text-red-400 cursor-pointer"
              >
                <Trash2 className="h-3 w-3" />
              </button>
            </div>
          </div>
        );
      })}
      {entries.length === 0 && (
        <p className="text-xs text-white/30 text-center py-3">
          No entries yet. Add provider/model pairs to define the fallback chain.
        </p>
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain Card
// ─────────────────────────────────────────────────────────────────────────────

function ChainCard({
  chain,
  onUpdate,
  onDelete,
  onSetDefault,
  providers,
}: {
  chain: ModelChain;
  onUpdate: (id: string, data: { name?: string; entries?: ChainEntry[]; is_default?: boolean }) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onSetDefault: (id: string) => Promise<void>;
  providers: Provider[];
}) {
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState(chain.name);
  const [editEntries, setEditEntries] = useState<ChainEntry[]>(chain.entries);
  const [resolved, setResolved] = useState<ResolvedEntry[] | null>(null);
  const [loadingResolve, setLoadingResolve] = useState(false);

  const handleResolve = async () => {
    setLoadingResolve(true);
    try {
      const entries = await resolveModelChain(chain.id);
      setResolved(entries);
    } catch (err) {
      toast.error(`Failed to resolve: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setLoadingResolve(false);
    }
  };

  const handleSave = async () => {
    const validEntries = editEntries.filter(
      (e) => e.provider_id.trim() && e.model_id.trim()
    );
    if (validEntries.length === 0) {
      toast.error('At least one valid entry is required');
      return;
    }
    try {
      await onUpdate(chain.id, { name: editName, entries: validEntries });
      setEditing(false);
    } catch {
      // onUpdate already shows a toast; stay in edit mode so changes aren't lost
    }
  };

  const handleStartEdit = () => {
    setEditName(chain.name);
    setEditEntries([...chain.entries]);
    setEditing(true);
    setExpanded(true);
  };

  return (
    <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] hover:bg-white/[0.02] transition-colors">
      <div
        className="flex items-center gap-3 px-3 py-2.5 cursor-pointer"
        onClick={() => !editing && setExpanded(!expanded)}
      >
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 text-white/30 flex-shrink-0" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 text-white/30 flex-shrink-0" />
        )}
        <GitBranch className="h-4 w-4 text-indigo-400 flex-shrink-0" />
        <div className="flex-1 min-w-0">
          <span className="text-sm text-white/80">{chain.name}</span>
          <span className="ml-2 text-xs text-white/30 font-mono">{chain.id}</span>
        </div>
        <span className="text-[10px] text-white/30">
          {chain.entries.length} {chain.entries.length === 1 ? 'entry' : 'entries'}
        </span>
        {chain.is_default && (
          <Star className="h-3 w-3 text-indigo-400 fill-indigo-400 flex-shrink-0" />
        )}
        <div
          className="flex items-center gap-0.5"
          onClick={(e) => e.stopPropagation()}
        >
          {!chain.is_default && (
            <button
              onClick={() => onSetDefault(chain.id)}
              className="p-1.5 rounded-md text-white/20 hover:text-indigo-400 hover:bg-white/[0.04] transition-colors cursor-pointer"
              title="Set as default"
            >
              <Star className="h-3.5 w-3.5" />
            </button>
          )}
          <button
            onClick={handleStartEdit}
            className="p-1.5 rounded-md text-white/20 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
            title="Edit"
          >
            <Pencil className="h-3.5 w-3.5" />
          </button>
          <button
            onClick={() => onDelete(chain.id)}
            className="p-1.5 rounded-md text-white/20 hover:text-red-400 hover:bg-white/[0.04] transition-colors cursor-pointer"
            title="Delete"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      {expanded && (
        <div className="border-t border-white/[0.04] px-3 py-3 space-y-3">
          {editing ? (
            <>
              <div>
                <label className="text-xs text-white/40 mb-1 block">Chain name</label>
                <input
                  type="text"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <EntryEditor entries={editEntries} onChange={setEditEntries} providers={providers} />
              <div className="flex items-center justify-end gap-2 pt-1">
                <button
                  onClick={() => setEditing(false)}
                  className="rounded-lg px-3 py-1.5 text-xs text-white/60 hover:text-white/80 transition-colors cursor-pointer"
                >
                  Cancel
                </button>
                <button
                  onClick={handleSave}
                  className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer"
                >
                  Save
                </button>
              </div>
            </>
          ) : (
            <>
              {/* Entries list */}
              <div className="space-y-1">
                {chain.entries.map((entry, i) => (
                  <div
                    key={i}
                    className="flex items-center gap-2 text-xs"
                  >
                    <span className="text-white/20 w-4">{i + 1}.</span>
                    <span className="text-white/60 font-mono">
                      {entry.provider_id}/{entry.model_id}
                    </span>
                  </div>
                ))}
              </div>

              {/* Resolve button */}
              <div className="pt-1">
                <button
                  onClick={handleResolve}
                  disabled={loadingResolve}
                  className="text-xs text-indigo-400 hover:text-indigo-300 transition-colors cursor-pointer disabled:opacity-50"
                >
                  {loadingResolve ? (
                    <span className="flex items-center gap-1">
                      <Loader className="h-3 w-3 animate-spin" />
                      Resolving...
                    </span>
                  ) : (
                    'Test chain resolution'
                  )}
                </button>
                {resolved && (
                  <div className="mt-2 space-y-1 rounded-lg bg-white/[0.02] border border-white/[0.04] p-2">
                    <span className="text-[10px] text-white/30 uppercase tracking-wider">
                      Resolved entries ({resolved.length})
                    </span>
                    {resolved.length === 0 ? (
                      <p className="text-xs text-amber-400">
                        No healthy accounts available for this chain
                      </p>
                    ) : (
                      resolved.map((r, i) => (
                        <div key={i} className="flex items-center gap-2 text-xs">
                          <span className="text-white/20 w-4">{i + 1}.</span>
                          <span className="text-white/60 font-mono">
                            {r.provider_id}/{r.model_id}
                          </span>
                          <span className="text-white/20 font-mono text-[10px]">
                            {r.account_id.slice(0, 8)}
                          </span>
                          <span className={cn(
                            'h-1.5 w-1.5 rounded-full',
                            r.has_api_key ? 'bg-emerald-400' : 'bg-red-400'
                          )} />
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

function formatTokenCount(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return `${tokens}`;
}

// ─────────────────────────────────────────────────────────────────────────────
// Health Dashboard
// ─────────────────────────────────────────────────────────────────────────────

function HealthDashboard({
  health,
  onClear,
  isLoading,
}: {
  health: AccountHealthSnapshot[];
  onClear: (accountId: string) => Promise<void>;
  isLoading: boolean;
}) {
  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader className="h-5 w-5 animate-spin text-white/40" />
      </div>
    );
  }

  if (health.length === 0) {
    return (
      <div className="text-center py-6">
        <p className="text-xs text-white/30">
          No health data yet. Health tracking begins when the proxy handles requests.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {health.map((h) => (
        <div
          key={h.account_id}
          className="flex items-center gap-3 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2"
        >
          <span
            className={cn(
              'h-2 w-2 rounded-full flex-shrink-0',
              h.is_degraded
                ? 'bg-amber-400'
                : h.is_healthy
                  ? 'bg-emerald-400'
                  : 'bg-red-400'
            )}
          />
          <span className="text-xs text-white/50 font-mono flex-shrink-0" title={h.account_id}>
            {h.provider_id || h.account_id.slice(0, 8)}
          </span>
          <div className="flex-1 flex items-center gap-3 text-[10px] text-white/30">
            <span>{h.total_requests} req</span>
            <span className="text-emerald-400/60">
              {h.total_requests > 0
                ? `${Math.round((h.total_successes / h.total_requests) * 100)}%`
                : '—'}
            </span>
            {h.is_degraded && (
              <span className="text-amber-400/80 font-medium">degraded</span>
            )}
            {h.total_rate_limits > 0 && (
              <span className="text-amber-400/60">{h.total_rate_limits} rate-limited</span>
            )}
            {h.total_errors > 0 && (
              <span className="text-red-400/60">{h.total_errors} err</span>
            )}
            {h.avg_latency_ms != null && (
              <span className="text-blue-400/60">{Math.round(h.avg_latency_ms)}ms</span>
            )}
            {(h.total_input_tokens > 0 || h.total_output_tokens > 0) && (
              <span className="text-purple-400/60">
                {formatTokenCount(h.total_input_tokens)}↑ {formatTokenCount(h.total_output_tokens)}↓
              </span>
            )}
          </div>
          {!h.is_healthy && (
            <div className="flex items-center gap-2 flex-shrink-0">
              {h.cooldown_remaining_secs != null && (
                <span className="flex items-center gap-1 text-[10px] text-amber-400">
                  <Clock className="h-3 w-3" />
                  {Math.ceil(h.cooldown_remaining_secs)}s
                </span>
              )}
              {h.last_failure_reason && (
                <span className="text-[10px] text-red-400/60">
                  {h.last_failure_reason}
                </span>
              )}
              <button
                onClick={() => onClear(h.account_id)}
                className="p-1 rounded text-amber-400 hover:bg-white/[0.04] transition-colors cursor-pointer"
                title="Clear cooldown"
              >
                <RotateCcw className="h-3 w-3" />
              </button>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────────
// Main Page
// ─────────────────────────────────────────────────────────────────────────────

export default function ModelRoutingPage() {
  const [showCreate, setShowCreate] = useState(false);
  const [createForm, setCreateForm] = useState({
    id: '',
    name: '',
    entries: [{ provider_id: '', model_id: '' }] as ChainEntry[],
    is_default: false,
  });

  const {
    data: chains = [],
    isLoading: chainsLoading,
    mutate: mutateChains,
  } = useSWR('model-chains', listModelChains, { revalidateOnFocus: false });

  const { data: providersData } = useSWR(
    'routing-providers',
    () => listProviders({ includeAll: true }),
    { revalidateOnFocus: false }
  );
  const providers = useMemo(
    () => providersData?.providers ?? [],
    [providersData]
  );

  const {
    data: health = [],
    isLoading: healthLoading,
    mutate: mutateHealth,
  } = useSWR('account-health', listAccountHealth, {
    revalidateOnFocus: false,
    refreshInterval: 10000, // Poll health every 10s
  });

  const {
    data: events = [],
    isLoading: eventsLoading,
  } = useSWR('fallback-events', listFallbackEvents, {
    revalidateOnFocus: false,
    refreshInterval: 10000, // Poll events every 10s
  });

  const {
    data: rtkStats,
    isLoading: rtkStatsLoading,
  } = useSWR('rtk-stats', getRtkStats, {
    revalidateOnFocus: false,
    refreshInterval: 10000, // Poll RTK stats every 10s
  });

  const handleCreate = async () => {
    if (!createForm.id.trim() || !createForm.name.trim()) {
      toast.error('Chain ID and name are required');
      return;
    }
    const validEntries = createForm.entries.filter(
      (e) => e.provider_id.trim() && e.model_id.trim()
    );
    if (validEntries.length === 0) {
      toast.error('At least one valid entry is required');
      return;
    }
    try {
      await createModelChain({
        id: createForm.id.trim(),
        name: createForm.name.trim(),
        entries: validEntries,
        is_default: createForm.is_default,
      });
      toast.success('Chain created');
      setShowCreate(false);
      setCreateForm({
        id: '',
        name: '',
        entries: [{ provider_id: '', model_id: '' }],
        is_default: false,
      });
      mutateChains();
    } catch (err) {
      toast.error(`Failed to create: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  };

  const handleUpdate = async (
    id: string,
    data: { name?: string; entries?: ChainEntry[]; is_default?: boolean }
  ) => {
    try {
      await updateModelChain(id, data);
      toast.success('Chain updated');
      mutateChains();
    } catch (err) {
      toast.error(`Failed to update: ${err instanceof Error ? err.message : 'Unknown error'}`);
      throw err;
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm(`Delete chain "${id}"? This cannot be undone.`)) return;
    try {
      await deleteModelChain(id);
      toast.success('Chain deleted');
      mutateChains();
    } catch (err) {
      toast.error(`Failed to delete: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  };

  const handleSetDefault = async (id: string) => {
    try {
      await updateModelChain(id, { is_default: true });
      toast.success('Default chain updated');
      mutateChains();
    } catch (err) {
      toast.error(`Failed to set default: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  };

  const handleClearCooldown = async (accountId: string) => {
    try {
      await clearAccountCooldown(accountId);
      toast.success('Cooldown cleared');
      mutateHealth();
    } catch (err) {
      toast.error(`Failed to clear: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  };

  // ── Proxy API Keys ──
  const {
    data: apiKeys = [],
    isLoading: apiKeysLoading,
    mutate: mutateApiKeys,
  } = useSWR('proxy-api-keys', listProxyApiKeys, { revalidateOnFocus: false });

  const [showCreateKey, setShowCreateKey] = useState(false);
  const [newKeyName, setNewKeyName] = useState('');
  const [createdKey, setCreatedKey] = useState<string | null>(null);
  const [creatingKey, setCreatingKey] = useState(false);
  const [copiedText, setCopiedText] = useState<string | null>(null);

  const handleCreateKey = async () => {
    if (!newKeyName.trim()) {
      toast.error('Key name is required');
      return;
    }
    setCreatingKey(true);
    try {
      const result = await createProxyApiKey(newKeyName.trim());
      setCreatedKey(result.key);
      setNewKeyName('');
      mutateApiKeys();
      toast.success('API key created');
    } catch (err) {
      toast.error(`Failed to create: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setCreatingKey(false);
    }
  };

  const handleDeleteKey = async (id: string) => {
    if (!confirm('Revoke this API key? External tools using it will stop working.')) return;
    try {
      await deleteProxyApiKey(id);
      toast.success('API key deleted');
      mutateApiKeys();
    } catch (err) {
      toast.error(`Failed to delete: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  };

  const handleCopyKey = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedText(text);
      setTimeout(() => setCopiedText(null), 2000);
    } catch {
      toast.error('Failed to copy to clipboard');
    }
  };

  const proxyUrl = `${getRuntimeApiBase()}/v1`;

  return (
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <div className="w-full max-w-2xl">
        <div className="mb-8">
          <h1 className="text-xl font-semibold text-white">Model Routing</h1>
          <p className="mt-1 text-sm text-white/50">
            Configure fallback chains and monitor provider health
          </p>
        </div>

        {/* ── Chains Section ── */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 mb-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Fallback Chains</h2>
                <p className="text-xs text-white/40">
                  Define provider/model fallback order for the proxy
                </p>
              </div>
            </div>
            <button
              onClick={() => setShowCreate(!showCreate)}
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer"
            >
              <Plus className="h-3 w-3" />
              New Chain
            </button>
          </div>

          {/* Create form */}
          {showCreate && (
            <div className="mb-4 rounded-lg border border-indigo-500/20 bg-indigo-500/[0.03] p-4 space-y-3">
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="text-xs text-white/40 mb-1 block">Chain ID</label>
                  <input
                    type="text"
                    value={createForm.id}
                    onChange={(e) =>
                      setCreateForm({ ...createForm, id: e.target.value })
                    }
                    placeholder="e.g. my/fast"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                  />
                </div>
                <div>
                  <label className="text-xs text-white/40 mb-1 block">Display name</label>
                  <input
                    type="text"
                    value={createForm.name}
                    onChange={(e) =>
                      setCreateForm({ ...createForm, name: e.target.value })
                    }
                    placeholder="e.g. Fast Chain"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                  />
                </div>
              </div>
              <EntryEditor
                entries={createForm.entries}
                onChange={(entries) =>
                  setCreateForm({ ...createForm, entries })
                }
                providers={providers}
              />
              <div className="flex items-center justify-between pt-1">
                <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={createForm.is_default}
                    onChange={(e) =>
                      setCreateForm({ ...createForm, is_default: e.target.checked })
                    }
                    className="rounded border-white/20 cursor-pointer"
                  />
                  Set as default chain
                </label>
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => {
                      setShowCreate(false);
                      setCreateForm({ id: '', name: '', entries: [{ provider_id: '', model_id: '' }], is_default: false });
                    }}
                    className="rounded-lg px-3 py-1.5 text-xs text-white/60 hover:text-white/80 transition-colors cursor-pointer"
                  >
                    Cancel
                  </button>
                  <button
                    onClick={handleCreate}
                    className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer"
                  >
                    Create
                  </button>
                </div>
              </div>
            </div>
          )}

          {/* Chain list */}
          <div className="space-y-2">
            {chainsLoading ? (
              <div className="flex items-center justify-center py-8">
                <Loader className="h-5 w-5 animate-spin text-white/40" />
              </div>
            ) : chains.length === 0 ? (
              <div className="text-center py-8">
                <div className="flex justify-center mb-3">
                  <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-white/[0.04]">
                    <GitBranch className="h-6 w-6 text-white/30" />
                  </div>
                </div>
                <p className="text-sm text-white/50 mb-1">No chains configured</p>
                <p className="text-xs text-white/30">
                  The default builtin/smart chain is created automatically on first mission
                </p>
              </div>
            ) : (
              chains.map((chain) => (
                <ChainCard
                  key={chain.id}
                  chain={chain}
                  onUpdate={handleUpdate}
                  onDelete={handleDelete}
                  onSetDefault={handleSetDefault}
                  providers={providers}
                />
              ))
            )}
          </div>
        </div>

        {/* ── Health Section ── */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10">
              <Heart className="h-5 w-5 text-emerald-400" />
            </div>
            <div>
              <h2 className="text-sm font-medium text-white">Provider Health</h2>
              <p className="text-xs text-white/40">
                Per-account health status and cooldown tracking
              </p>
            </div>
            {health.some((h) => !h.is_healthy) && (
              <div className="ml-auto flex items-center gap-1.5">
                <AlertTriangle className="h-3.5 w-3.5 text-amber-400" />
                <span className="text-xs text-amber-400">
                  {health.filter((h) => !h.is_healthy).length} in cooldown
                </span>
              </div>
            )}
          </div>

          <HealthDashboard
            health={health}
            onClear={handleClearCooldown}
            isLoading={healthLoading}
          />
        </div>

        {/* ── RTK Token Savings Section ── */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 mt-6">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-purple-500/10">
              <Zap className="h-5 w-5 text-purple-400" />
            </div>
            <div>
              <h2 className="text-sm font-medium text-white">RTK Token Savings</h2>
              <p className="text-xs text-white/40">
                CLI output compression reduces LLM token consumption
              </p>
            </div>
            {rtkStats && rtkStats.commands_processed > 0 && (
              <span className="ml-auto text-xs text-emerald-400">
                {rtkStats.savings_percent.toFixed(1)}% saved
              </span>
            )}
          </div>

          {rtkStatsLoading ? (
            <div className="flex items-center justify-center py-6">
              <Loader className="h-4 w-4 animate-spin text-white/30" />
            </div>
          ) : !rtkStats || rtkStats.commands_processed === 0 ? (
            <div className="text-center py-4">
              <p className="text-xs text-white/30">
                No RTK data yet. Enable RTK in <a href="/settings/data" className="text-white/50 underline">Settings</a>
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-4 gap-3">
              <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
                <div className="text-[10px] text-white/30 uppercase tracking-wider mb-1">Commands</div>
                <div className="text-lg font-medium text-white">{rtkStats.commands_processed.toLocaleString()}</div>
              </div>
              <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
                <div className="text-[10px] text-white/30 uppercase tracking-wider mb-1">Original</div>
                <div className="text-lg font-medium text-white">{formatTokenCount(rtkStats.original_chars)}</div>
              </div>
              <div className="rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
                <div className="text-[10px] text-white/30 uppercase tracking-wider mb-1">Compressed</div>
                <div className="text-lg font-medium text-white">{formatTokenCount(rtkStats.compressed_chars)}</div>
              </div>
              <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/5 px-3 py-2">
                <div className="text-[10px] text-emerald-400/60 uppercase tracking-wider mb-1">Saved</div>
                <div className="text-lg font-medium text-emerald-400">
                  {formatTokenCount(rtkStats.chars_saved)} ({rtkStats.savings_percent.toFixed(0)}%)
                </div>
              </div>
            </div>
          )}
        </div>

        {/* ── Fallback Events Section ── */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 mt-6">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-blue-500/10">
              <Activity className="h-5 w-5 text-blue-400" />
            </div>
            <div>
              <h2 className="text-sm font-medium text-white">Recent Fallback Events</h2>
              <p className="text-xs text-white/40">
                Provider failovers during chain resolution
              </p>
            </div>
            {events.length > 0 && (
              <span className="ml-auto text-xs text-white/30">
                {events.length} events
              </span>
            )}
          </div>

          {eventsLoading ? (
            <div className="flex items-center justify-center py-8">
              <Loader className="h-5 w-5 animate-spin text-white/40" />
            </div>
          ) : events.length === 0 ? (
            <div className="text-center py-6">
              <p className="text-xs text-white/30">
                No fallback events yet. Events appear when the proxy fails over to the next provider.
              </p>
            </div>
          ) : (
            <div className="space-y-1.5">
              {[...events].reverse().map((evt, i) => (
                <div
                  key={i}
                  className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-1.5 text-[10px]"
                >
                  <span className={cn(
                    'h-1.5 w-1.5 rounded-full flex-shrink-0',
                    evt.reason === 'rate_limit' ? 'bg-amber-400' :
                    evt.reason === 'auth_error' ? 'bg-red-400' :
                    evt.reason === 'overloaded' ? 'bg-orange-400' :
                    'bg-gray-400'
                  )} />
                  <span className="text-white/40 flex-shrink-0 w-16">
                    {new Date(evt.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}
                  </span>
                  <span className="text-white/20 flex-shrink-0">
                    {evt.attempt_number}/{evt.chain_length}
                  </span>
                  <span className="text-white/60 font-mono">
                    {evt.from_provider}/{evt.from_model}
                  </span>
                  {evt.to_provider && (
                    <>
                      <ArrowRight className="h-2.5 w-2.5 text-white/20 flex-shrink-0" />
                      <span className="text-emerald-400/80 font-mono">
                        {evt.to_provider}
                      </span>
                    </>
                  )}
                  {!evt.to_provider && (
                    <span className="text-red-400/60">exhausted</span>
                  )}
                  <span className={cn(
                    'flex-shrink-0',
                    evt.reason === 'rate_limit' ? 'text-amber-400/60' :
                    evt.reason === 'auth_error' ? 'text-red-400/60' :
                    'text-white/30'
                  )}>
                    {evt.reason.replaceAll('_', ' ')}
                  </span>
                  {evt.latency_ms != null && (
                    <span className="text-blue-400/50 flex-shrink-0">
                      {evt.latency_ms}ms
                    </span>
                  )}
                  {evt.cooldown_secs != null && (
                    <span className="text-white/20 flex-shrink-0">
                      cd {Math.round(evt.cooldown_secs)}s
                    </span>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>

        {/* ── Proxy API Keys Section ── */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 mt-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Key className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Proxy API Keys</h2>
                <p className="text-xs text-white/40">
                  Generate keys for external tools (Cursor, Windsurf, etc.)
                </p>
              </div>
            </div>
            <button
              onClick={() => { setShowCreateKey(true); setCreatedKey(null); }}
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer"
            >
              <Plus className="h-3 w-3" />
              New Key
            </button>
          </div>

          {/* Proxy endpoint URL */}
          <div className="mb-4 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2">
            <div className="flex items-center justify-between">
              <div>
                <span className="text-[10px] text-white/30 uppercase tracking-wider">Proxy Endpoint</span>
                <p className="text-xs text-white/60 font-mono mt-0.5">{proxyUrl}</p>
              </div>
              <button
                onClick={() => handleCopyKey(proxyUrl)}
                className={`p-1.5 rounded-md transition-colors cursor-pointer ${copiedText === proxyUrl ? 'text-emerald-400' : 'text-white/20 hover:text-white/60 hover:bg-white/[0.04]'}`}
                title="Copy endpoint URL"
              >
                {copiedText === proxyUrl ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
              </button>
            </div>
          </div>

          {/* Create key form */}
          {showCreateKey && (
            <div className="mb-4 rounded-lg border border-indigo-500/20 bg-indigo-500/5 p-3 space-y-3">
              <input
                type="text"
                placeholder="Key name (e.g. Cursor, CI pipeline)"
                value={newKeyName}
                onChange={(e) => setNewKeyName(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleCreateKey()}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder:text-white/20 focus:outline-none focus:border-indigo-500/50"
              />
              {createdKey && (
                <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/5 p-3">
                  <p className="text-[10px] text-emerald-400/80 mb-1">
                    Copy this key now — it won&apos;t be shown again
                  </p>
                  <div className="flex items-center gap-2">
                    <code className="flex-1 text-xs text-emerald-300 font-mono break-all">
                      {createdKey}
                    </code>
                    <button
                      onClick={() => handleCopyKey(createdKey)}
                      className="flex-shrink-0 p-1.5 rounded-md text-emerald-400/60 hover:text-emerald-300 hover:bg-white/[0.04] transition-colors cursor-pointer"
                    >
                      {copiedText === createdKey ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
                    </button>
                  </div>
                </div>
              )}
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => { setShowCreateKey(false); setCreatedKey(null); setNewKeyName(''); }}
                  className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/50 hover:bg-white/[0.04] transition-colors cursor-pointer"
                >
                  {createdKey ? 'Done' : 'Cancel'}
                </button>
                {!createdKey && (
                  <button
                    onClick={handleCreateKey}
                    disabled={creatingKey}
                    className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50"
                  >
                    {creatingKey ? 'Creating...' : 'Create'}
                  </button>
                )}
              </div>
            </div>
          )}

          {/* Key list */}
          {apiKeysLoading ? (
            <div className="flex items-center justify-center py-6">
              <Loader className="h-4 w-4 animate-spin text-white/30" />
            </div>
          ) : apiKeys.length === 0 ? (
            <p className="text-xs text-white/30 text-center py-4">
              No API keys yet. Create one to connect external tools.
            </p>
          ) : (
            <div className="space-y-1.5">
              {apiKeys.map((k) => (
                <div
                  key={k.id}
                  className="flex items-center gap-3 rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2"
                >
                  <Key className="h-3.5 w-3.5 text-white/20 flex-shrink-0" />
                  <span className="text-xs text-white/70 flex-1 min-w-0 truncate">
                    {k.name}
                  </span>
                  <span className="text-[10px] text-white/30 font-mono flex-shrink-0">
                    {k.key_prefix}...
                  </span>
                  <span className="text-[10px] text-white/20 flex-shrink-0">
                    {new Date(k.created_at).toLocaleDateString()}
                  </span>
                  <button
                    onClick={() => handleDeleteKey(k.id)}
                    className="p-1.5 rounded-md text-white/20 hover:text-red-400 hover:bg-white/[0.04] transition-colors cursor-pointer flex-shrink-0"
                    title="Revoke key"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
