'use client';

import { useState } from 'react';
import Link from 'next/link';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  getLlmRoles,
  getSettings,
  updateSettings,
  listProviders,
  type LlmRoleStatus,
  type Provider,
} from '@/lib/api';
import { listModelChains, type ModelChain } from '@/lib/api/model-routing';
import {
  ArrowUpRight,
  Check,
  Key,
  Loader,
  RotateCcw,
  Sparkles,
  Type,
} from 'lucide-react';

const SOURCE_LABELS: Record<string, string> = {
  settings: 'Custom',
  env: 'Env override',
  auto: 'Auto',
};

/** Compact resolved provider/model chip with a real availability state. */
function RoleStatusChip({
  role,
  loading,
}: {
  role: LlmRoleStatus | undefined;
  loading: boolean;
}) {
  if (loading) {
    return (
      <div className="inline-flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5">
        <Loader className="h-3 w-3 animate-spin text-white/40" />
        <span className="text-xs text-white/40">Resolving...</span>
      </div>
    );
  }
  if (!role?.available) {
    return (
      <div className="inline-flex items-center gap-2 rounded-lg border border-amber-500/20 bg-amber-500/5 px-2.5 py-1.5">
        <span className="h-1.5 w-1.5 rounded-full bg-amber-400" />
        <span className="text-xs text-amber-400/90">No provider available</span>
      </div>
    );
  }
  return (
    <div className="inline-flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5">
      <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
      <span className="text-xs text-white/70">{role.provider}</span>
      <span className="text-white/20">/</span>
      <span className="font-mono text-xs text-white/70">{role.model}</span>
    </div>
  );
}

/**
 * One backend LLM role: identity row, resolved status, and a model picker
 * fed by Routing chains and configured provider models. Both cards on this
 * page share this exact anatomy so the roles read as siblings.
 */
function RoleCard({
  icon,
  iconTint,
  title,
  subtitle,
  role,
  source,
  rolesLoading,
  settingsLoading,
  savedValue,
  defaultLabel,
  extraOptions,
  chains,
  providers,
  helpText,
  onSave,
}: {
  icon: React.ReactNode;
  iconTint: string;
  title: string;
  subtitle: string;
  role: LlmRoleStatus | undefined;
  source: string | undefined;
  rolesLoading: boolean;
  settingsLoading: boolean;
  savedValue: string;
  defaultLabel: string;
  /** Extra fixed options between the default and the chains group. */
  extraOptions?: { value: string; label: string }[];
  chains: ModelChain[];
  providers: Provider[];
  helpText: React.ReactNode;
  onSave: (value: string) => Promise<void>;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [customMode, setCustomMode] = useState(false);
  const [saving, setSaving] = useState(false);

  const effectiveValue = draft ?? savedValue;
  const dirty = draft !== null && draft.trim() !== savedValue;

  const chainIds = chains.map((c) => c.id);
  const knownValues = new Set<string>([
    '',
    ...(extraOptions?.map((o) => o.value) ?? []),
    ...chainIds,
    ...providers.flatMap((p) => p.models.map((m) => `${p.id}/${m.id}`)),
  ]);
  const selectValue =
    customMode || !knownValues.has(effectiveValue) ? '__custom__' : effectiveValue;

  const save = async (value: string) => {
    setSaving(true);
    try {
      await onSave(value.trim());
      setDraft(null);
      setCustomMode(false);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col rounded-xl border border-white/[0.04] bg-white/[0.02] p-5">
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <div
            className={`flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-xl ${iconTint}`}
          >
            {icon}
          </div>
          <div className="min-w-0">
            <h2 className="text-sm font-medium text-white">{title}</h2>
            <p className="text-xs text-white/40">{subtitle}</p>
          </div>
        </div>
        {!rolesLoading && source && (
          <span className="flex-shrink-0 rounded-md bg-white/[0.04] px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-white/40">
            {SOURCE_LABELS[source] ?? source}
          </span>
        )}
      </div>

      <div className="mb-4">
        <RoleStatusChip role={role} loading={rolesLoading} />
      </div>

      <div className="mt-auto">
        <label className="mb-1.5 block text-xs font-medium text-white/60">
          Model
        </label>
        {settingsLoading ? (
          <div className="space-y-2">
            <div className="h-9 animate-pulse rounded-lg bg-white/[0.04]" />
            <div className="h-7 w-24 animate-pulse rounded-lg bg-white/[0.03]" />
          </div>
        ) : (
          <div className="space-y-2">
            <select
              value={selectValue}
              onChange={(e) => {
                const v = e.target.value;
                if (v === '__custom__') {
                  setCustomMode(true);
                  setDraft(effectiveValue);
                } else {
                  setCustomMode(false);
                  setDraft(v);
                }
              }}
              className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:border-indigo-500/50 focus:outline-none [&>optgroup]:bg-slate-900 [&>optgroup]:text-white/70 [&>option]:bg-slate-800 [&>option]:text-white"
            >
              <option value="">{defaultLabel}</option>
              {extraOptions?.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
              {chainIds.length > 0 && (
                <optgroup label="Routing chains (with fallbacks)">
                  {chainIds.map((id) => (
                    <option key={id} value={id}>
                      {id}
                    </option>
                  ))}
                </optgroup>
              )}
              {providers.map(
                (p) =>
                  p.models.length > 0 && (
                    <optgroup key={p.id} label={p.name}>
                      {p.models.map((m) => (
                        <option key={`${p.id}/${m.id}`} value={`${p.id}/${m.id}`}>
                          {p.id}/{m.id}
                        </option>
                      ))}
                    </optgroup>
                  )
              )}
              <option value="__custom__">Custom...</option>
            </select>
            {selectValue === '__custom__' && (
              <input
                type="text"
                value={effectiveValue}
                onChange={(e) => setDraft(e.target.value)}
                placeholder="chain id or provider/model"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 font-mono text-sm text-white placeholder:text-white/20 focus:border-indigo-500/50 focus:outline-none"
                onKeyDown={(e) => {
                  if (e.key === 'Enter') save(effectiveValue);
                }}
              />
            )}
            <div className="flex items-center gap-2">
              <button
                onClick={() => save(effectiveValue)}
                disabled={saving || !dirty}
                className="flex cursor-pointer items-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white transition-colors hover:bg-indigo-600 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50"
              >
                {saving ? (
                  <Loader className="h-3 w-3 animate-spin" />
                ) : (
                  <Check className="h-3 w-3" />
                )}
                Save
              </button>
              {savedValue && (
                <button
                  onClick={() => save('')}
                  disabled={saving}
                  className="flex cursor-pointer items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/60 transition-colors hover:bg-white/[0.04] active:scale-[0.98] disabled:opacity-50"
                >
                  <RotateCcw className="h-3 w-3" />
                  Reset
                </button>
              )}
            </div>
            <p className="text-xs leading-relaxed text-white/30">{helpText}</p>
          </div>
        )}
      </div>
    </div>
  );
}

export default function LLMSettingsPage() {
  const {
    data: serverSettings,
    isLoading: settingsLoading,
    mutate: mutateSettings,
  } = useSWR('settings', getSettings, { revalidateOnFocus: false });
  const {
    data: roles,
    isLoading: rolesLoading,
    mutate: mutateRoles,
  } = useSWR('llm-roles', getLlmRoles, { revalidateOnFocus: false });
  const { data: chains = [] } = useSWR<ModelChain[]>('model-chains', listModelChains, {
    revalidateOnFocus: false,
    dedupingInterval: 60000,
  });
  const { data: providersData } = useSWR(
    'providers-all',
    () => listProviders({ includeAll: true }),
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );
  const providers = providersData?.providers ?? [];

  const saveRole = async (
    field: 'ask_assistant_model' | 'metadata_model',
    label: string,
    value: string
  ) => {
    try {
      // "" (not null) clears: a present empty string is normalized to None
      // server-side, whereas JSON null means "no change".
      await updateSettings({ [field]: value });
      mutateSettings();
      mutateRoles();
      toast.success(value ? `${label} model updated` : `${label} model reset to default`);
    } catch (err) {
      toast.error(
        `Failed to save: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
      throw err;
    }
  };

  const anyUnavailable =
    !rolesLoading && roles && (!roles.assistant.available || !roles.metadata.available);

  return (
    <div className="flex flex-1 flex-col items-center overflow-auto p-6">
      <div className="w-full max-w-4xl space-y-6">
        <header>
          <h1 className="text-xl font-semibold text-white">LLM</h1>
          <p className="mt-1 text-sm text-white/50">
            Server-side models powering the copilot and mission metadata
          </p>
        </header>

        <div className="grid gap-5 md:grid-cols-2">
          <RoleCard
            icon={<Sparkles className="h-5 w-5 text-sky-400" />}
            iconTint="bg-sky-500/10"
            title="Copilot"
            subtitle="Mission co-pilot (Ask panel)"
            role={roles?.assistant}
            source={roles?.assistant_source}
            rolesLoading={rolesLoading}
            settingsLoading={settingsLoading}
            savedValue={serverSettings?.ask_assistant_model ?? ''}
            defaultLabel="Default: Cerebras gpt-oss-120b"
            extraOptions={[
              { value: 'zai-glm-4.7', label: 'Cerebras zai-glm-4.7 (larger, slower)' },
            ]}
            chains={chains}
            providers={providers}
            helpText={
              <>
                Chains and provider picks route through /v1 with fallbacks and
                usage accounting. Bare model ids stay on direct Cerebras.
              </>
            }
            onSave={(v) => saveRole('ask_assistant_model', 'Copilot', v)}
          />

          <RoleCard
            icon={<Type className="h-5 w-5 text-amber-400" />}
            iconTint="bg-amber-500/10"
            title="Mission Titles & Status"
            subtitle="Summarizes missions after each turn"
            role={roles?.metadata}
            source={roles?.metadata_source}
            rolesLoading={rolesLoading}
            settingsLoading={settingsLoading}
            savedValue={serverSettings?.metadata_model ?? ''}
            defaultLabel="Auto: fastest configured provider"
            chains={chains}
            providers={providers}
            helpText={
              <>
                Auto picks the cheapest fast provider and follows provider
                changes without a restart. Overrides must be a chain or
                provider/model.
              </>
            }
            onSave={(v) => saveRole('metadata_model', 'Metadata', v)}
          />
        </div>

        <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/[0.04] bg-white/[0.02] px-5 py-4">
          <div className="flex min-w-0 items-center gap-3">
            <Key className="h-4 w-4 flex-shrink-0 text-indigo-400" />
            <p className="text-xs text-white/50">
              Defaults need an OpenAI-compatible provider. Chains and models come
              from Routing and Providers.
            </p>
          </div>
          <div className="flex flex-shrink-0 items-center gap-2">
            <Link
              href="/model-routing"
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.08] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 transition-colors hover:bg-white/[0.04]"
            >
              Routing
              <ArrowUpRight className="h-3 w-3" />
            </Link>
            <Link
              href="/settings/providers"
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.08] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 transition-colors hover:bg-white/[0.04]"
            >
              Providers
              <ArrowUpRight className="h-3 w-3" />
            </Link>
          </div>
        </div>

        {anyUnavailable && (
          <p className="rounded-lg border border-amber-500/20 bg-amber-500/5 px-3 py-2 text-xs text-amber-400/90">
            {!roles?.assistant.available && !roles?.metadata.available
              ? 'No usable provider found: the copilot and title generation are disabled until one is configured.'
              : !roles?.assistant.available
                ? 'No usable provider found for the copilot; it is disabled until one is configured.'
                : 'No usable provider found for mission titles; they fall back to raw text until one is configured.'}
          </p>
        )}
      </div>
    </div>
  );
}
