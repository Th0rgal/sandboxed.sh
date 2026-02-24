'use client';

import { useEffect, useRef, useState, useMemo } from 'react';
import { useRouter } from 'next/navigation';
import { Plus, X, ExternalLink, RefreshCw } from 'lucide-react';
import useSWR from 'swr';
import { getVisibleAgents, getOpenAgentConfig, listBackends, listBackendAgents, getBackendConfig, getClaudeCodeConfig, getLibraryOpenCodeSettingsForProfile, listBackendModelOptions, listProviders, type Backend, type BackendAgent, type BackendModelOption, type Provider } from '@/lib/api';
import type { Workspace } from '@/lib/api';

/** Options returned by the dialog's getCreateOptions() method */
export interface NewMissionDialogOptions {
  workspaceId?: string;
  agent?: string;
  /** @deprecated Use workspace config profiles instead */
  modelOverride?: string;
  modelEffort?: 'low' | 'medium' | 'high';
  configProfile?: string;
  backend?: string;
  /** Whether the mission will be opened in a new tab (skip local state updates) */
  openInNewTab?: boolean;
}

export interface CreatedMission {
  id: string;
}

/** Initial values to pre-fill the dialog (e.g., from current mission) */
export interface InitialMissionValues {
  workspaceId?: string;
  agent?: string;
  backend?: string;
  modelOverride?: string;
  modelEffort?: 'low' | 'medium' | 'high';
}

interface NewMissionDialogProps {
  workspaces: Workspace[];
  disabled?: boolean;
  /** Creates a mission and returns its ID for navigation */
  onCreate: (options?: NewMissionDialogOptions) => Promise<CreatedMission>;
  /** Path to the control page (default: '/control') */
  controlPath?: string;
  /** Initial values to pre-fill the form (from current mission) */
  initialValues?: InitialMissionValues;
  /** Auto-open the dialog on mount (e.g., when navigating from workspaces page) */
  autoOpen?: boolean;
  /** Callback when dialog closes (for clearing URL params, etc.) */
  onClose?: () => void;
}

// Combined agent with backend info
interface CombinedAgent {
  backend: string;
  backendName: string;
  agent: string;
  displayName: string; // User-friendly name for UI display
  value: string; // "backend:agent" format
}

// Parse agent names from API response
const parseAgentNames = (payload: unknown): string[] => {
  const normalizeEntry = (entry: unknown): string | null => {
    if (typeof entry === 'string') return entry;
    if (entry && typeof entry === 'object') {
      const name = (entry as { name?: unknown }).name;
      if (typeof name === 'string') return name;
      const id = (entry as { id?: unknown }).id;
      if (typeof id === 'string') return id;
    }
    return null;
  };

  const raw = Array.isArray(payload)
    ? payload
    : (payload as { agents?: unknown })?.agents;
  if (!Array.isArray(raw)) return [];

  const names = raw
    .map(normalizeEntry)
    .filter((name): name is string => Boolean(name));
  return Array.from(new Set(names));
};

const parseAgentNamesFromSettings = (settings: Record<string, unknown> | null | undefined): string[] => {
  if (!settings) return [];
  const agents = (settings as { agents?: unknown }).agents;
  if (!agents) return [];
  if (Array.isArray(agents)) {
    return parseAgentNames(agents);
  }
  if (agents && typeof agents === 'object') {
    return Object.keys(agents as Record<string, unknown>);
  }
  return [];
};

export function NewMissionDialog({
  workspaces,
  disabled = false,
  onCreate,
  controlPath = '/control',
  initialValues,
  autoOpen = false,
  onClose,
}: NewMissionDialogProps) {
  const router = useRouter();
  const [open, setOpen] = useState(autoOpen);
  const [newMissionWorkspace, setNewMissionWorkspace] = useState('');
  // Combined value: "backend:agent" or empty for default
  const [selectedAgentValue, setSelectedAgentValue] = useState('');
  const [modelOverride, setModelOverride] = useState('');
  const [modelEffort, setModelEffort] = useState<'low' | 'medium' | 'high' | ''>('');
  const [submitting, setSubmitting] = useState(false);
  const [defaultSet, setDefaultSet] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const prevBackendRef = useRef<string | null>(null);

  // SWR: fetch backends
  const { data: backends } = useSWR<Backend[]>('backends', listBackends, {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
    fallbackData: [{ id: 'opencode', name: 'OpenCode' }, { id: 'claudecode', name: 'Claude Code' }, { id: 'amp', name: 'Amp' }],
  });

  // SWR: fetch backend configs to check enabled status
  const { data: opencodeConfig } = useSWR('backend-opencode-config', () => getBackendConfig('opencode'), {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });
  const { data: claudecodeConfig } = useSWR('backend-claudecode-config', () => getBackendConfig('claudecode'), {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });
  const { data: ampConfig } = useSWR('backend-amp-config', () => getBackendConfig('amp'), {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });
  const { data: codexConfig } = useSWR('backend-codex-config', () => getBackendConfig('codex'), {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });

  const { data: providersResponse } = useSWR(
    'model-providers',
    () => listProviders({ includeAll: true }),
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );
  const { data: backendModelOptions, mutate: mutateBackendModelOptions } = useSWR(
    'backend-model-options',
    () => listBackendModelOptions({ includeAll: true }),
    { revalidateOnFocus: false, dedupingInterval: 60000 }
  );

  // Filter to only enabled backends with CLI available
  const enabledBackends = useMemo(() => {
    return backends?.filter((b) => {
      if (b.id === 'opencode') {
        return opencodeConfig?.enabled !== false && opencodeConfig?.cli_available !== false;
      }
      if (b.id === 'claudecode') {
        return claudecodeConfig?.enabled !== false && claudecodeConfig?.cli_available !== false;
      }
      if (b.id === 'amp') {
        return ampConfig?.enabled !== false && ampConfig?.cli_available !== false;
      }
      if (b.id === 'codex') {
        return codexConfig?.enabled !== false && codexConfig?.cli_available !== false;
      }
      return true;
    }) || [];
  }, [backends, opencodeConfig, claudecodeConfig, ampConfig, codexConfig]);

  // SWR: fetch agents for each enabled backend
  const { data: opencodeAgents, mutate: mutateOpencodeAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'opencode') ? 'backend-opencode-agents' : null,
    () => listBackendAgents('opencode'),
    { revalidateOnFocus: true, dedupingInterval: 5000 }
  );
  const { data: claudecodeAgents, mutate: mutateClaudecodeAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'claudecode') ? 'backend-claudecode-agents' : null,
    () => listBackendAgents('claudecode'),
    { revalidateOnFocus: true, dedupingInterval: 5000 }
  );
  const { data: ampAgents, mutate: mutateAmpAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'amp') ? 'backend-amp-agents' : null,
    () => listBackendAgents('amp'),
    { revalidateOnFocus: true, dedupingInterval: 5000 }
  );
  const { data: codexAgents, mutate: mutateCodexAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'codex') ? 'backend-codex-agents' : null,
    () => listBackendAgents('codex'),
    { revalidateOnFocus: true, dedupingInterval: 5000 }
  );

  // SWR: fallback for opencode agents
  const { data: agentsPayload, mutate: mutateAgentsPayload } = useSWR('opencode-agents', getVisibleAgents, {
    revalidateOnFocus: true,
    dedupingInterval: 5000,
  });
  const { data: config, mutate: mutateConfig } = useSWR('openagent-config', getOpenAgentConfig, {
    revalidateOnFocus: true,
    dedupingInterval: 5000,
  });

  // SWR: fetch Claude Code config for hidden agents
  const { data: claudeCodeLibConfig } = useSWR(
    enabledBackends.some(b => b.id === 'claudecode') ? 'claudecode-lib-config' : null,
    getClaudeCodeConfig,
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );

  const workspaceProfile = useMemo(() => {
    const targetWorkspace = newMissionWorkspace
      ? workspaces.find((workspace) => workspace.id === newMissionWorkspace)
      : workspaces.find((workspace) => workspace.id === '00000000-0000-0000-0000-000000000000')
        || workspaces.find((workspace) => workspace.workspace_type === 'host');
    return targetWorkspace?.config_profile || null;
  }, [newMissionWorkspace, workspaces]);

  const effectiveProfileForAgents = workspaceProfile || 'default';

  const { data: opencodeProfileSettings } = useSWR(
    effectiveProfileForAgents ? ['opencode-profile-settings', effectiveProfileForAgents] : null,
    ([, profile]) => getLibraryOpenCodeSettingsForProfile(profile as string),
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );

  const opencodeProfileAgentNames = useMemo(
    () => parseAgentNamesFromSettings(opencodeProfileSettings as Record<string, unknown> | null),
    [opencodeProfileSettings]
  );

  // Combine all agents from enabled backends
  const allAgents = useMemo((): CombinedAgent[] => {
    const result: CombinedAgent[] = [];
    const openCodeHiddenAgents = config?.hidden_agents || [];
    const claudeCodeHiddenAgents = claudeCodeLibConfig?.hidden_agents || [];

    for (const backend of enabledBackends) {
      // Use consistent {id, name} format for all backends
      let agents: { id: string; name: string }[] = [];

      if (backend.id === 'opencode') {
        // Filter out hidden OpenCode agents by name
        const profileAgents = opencodeProfileAgentNames.map(name => ({ id: name, name }));
        const backendAgents = profileAgents.length > 0 ? profileAgents : (opencodeAgents || []);
        const visibleAgents = backendAgents.filter(a => !openCodeHiddenAgents.includes(a.name));
        if (visibleAgents.length > 0) {
          agents = visibleAgents;
        } else if (backendAgents.length > 0) {
          // If all OpenCode agents are hidden, fall back to the raw list so the backend remains usable.
          agents = backendAgents;
        } else {
          // Fallback to parsing agent names from raw payload
          const fallbackNames = parseAgentNames(agentsPayload).filter(
            name => !openCodeHiddenAgents.includes(name)
          );
          agents = fallbackNames.map(name => ({ id: name, name }));
        }

      } else if (backend.id === 'claudecode') {
        // Filter out hidden Claude Code agents by name
        const allClaudeAgents = claudecodeAgents || [];
        agents = allClaudeAgents.filter(a => !claudeCodeHiddenAgents.includes(a.name));
      } else if (backend.id === 'amp') {
        // Amp has built-in modes: smart and rush
        agents = ampAgents || [
          { id: 'smart', name: 'Smart Mode' },
          { id: 'rush', name: 'Rush Mode' },
        ];
      } else if (backend.id === 'codex') {
        // Codex agents
        agents = codexAgents || [
          { id: 'default', name: 'Codex Agent' },
        ];
      }

      // Use agent.id for CLI value, agent.name for display (consistent across all backends)
      for (const agent of agents) {
        result.push({
          backend: backend.id,
          backendName: backend.name,
          agent: agent.id,
          displayName: agent.name,
          value: `${backend.id}:${agent.id}`,
        });
      }
    }

    return result;
  }, [enabledBackends, opencodeAgents, opencodeProfileAgentNames, claudecodeAgents, ampAgents, codexAgents, agentsPayload, config, claudeCodeLibConfig]);

  // Group agents by backend for display
  const agentsByBackend = useMemo(() => {
    const groups: Record<string, CombinedAgent[]> = {};
    for (const agent of allAgents) {
      if (!groups[agent.backend]) {
        groups[agent.backend] = [];
      }
      groups[agent.backend].push(agent);
    }
    return groups;
  }, [allAgents]);

  // Parse selected value to get backend and agent
  const parseSelectedValue = (value: string): { backend: string; agent: string } | null => {
    if (!value) return null;
    const [backend, ...agentParts] = value.split(':');
    const agent = agentParts.join(':'); // Handle agent names with colons
    return backend && agent ? { backend, agent } : null;
  };

  const selectedBackend = useMemo(() => {
    return parseSelectedValue(selectedAgentValue)?.backend || 'claudecode';
  }, [selectedAgentValue]);

  const providerAllowlist = useMemo(() => {
    if (selectedBackend === 'claudecode') return new Set(['anthropic']);
    if (selectedBackend === 'codex') return new Set(['openai']);
    return null;
  }, [selectedBackend]);

  const modelOptions = useMemo(() => {
    const backendOptions = backendModelOptions?.backends?.[selectedBackend];
    if (backendOptions && backendOptions.length > 0) {
      return backendOptions as BackendModelOption[];
    }
    const providers = (providersResponse?.providers || []) as Provider[];
    const options: Array<{ value: string; label: string; description?: string }> = [];
    for (const provider of providers) {
      if (providerAllowlist && !providerAllowlist.has(provider.id)) continue;
      for (const model of provider.models) {
        const value =
          selectedBackend === 'opencode'
            ? `${provider.id}/${model.id}`
            : model.id;
        options.push({
          value,
          label: `${provider.name} — ${model.name}`,
          description: model.description,
        });
      }
    }
    return options;
  }, [backendModelOptions, providersResponse, providerAllowlist, selectedBackend]);

  const formatWorkspaceType = (type: Workspace['workspace_type']) =>
    type === 'host' ? 'host' : 'isolated';

  // Click outside and Escape key handler
  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(event.target as Node)) {
        setOpen(false);
        setDefaultSet(false);
        onClose?.();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false);
        setDefaultSet(false);
        onClose?.();
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open, onClose]);

  // Revalidate backend model options when dialog opens to pick up chain configuration changes
  useEffect(() => {
    if (open) {
      mutateBackendModelOptions();
    }
  }, [open, mutateBackendModelOptions]);

  // Set initial values when dialog opens (only once per open)
  useEffect(() => {
    if (!open || defaultSet) return;
    // Wait for config to finish loading
    if (config === undefined) return;
    // Wait for agents to load
    if (allAgents.length === 0) return;

    // Set workspace from initialValues if provided
    if (initialValues?.workspaceId) {
      setNewMissionWorkspace(initialValues.workspaceId);
    }

    // Set model override from initialValues if provided
    if (initialValues?.modelOverride) {
      setModelOverride(initialValues.modelOverride);
    }
    if (initialValues?.modelEffort) {
      setModelEffort(initialValues.modelEffort);
    }

    // Try to use initialValues for agent (from current mission)
    if (initialValues?.backend && initialValues?.agent) {
      const matchingAgent = allAgents.find(
        a => a.backend === initialValues.backend && a.agent === initialValues.agent
      );
      if (matchingAgent) {
        setSelectedAgentValue(matchingAgent.value);
        setDefaultSet(true);
        return;
      }
    }

    // Fallback: try to find the default agent from config
    if (config?.default_agent) {
      const defaultAgent = allAgents.find(a => a.agent === config.default_agent);
      if (defaultAgent) {
        setSelectedAgentValue(defaultAgent.value);
        setDefaultSet(true);
        return;
      }
    }

    // Fallback: use first available backend with priority claudecode → opencode → amp
    // Try Claude Code first
    const claudeCodeAgent = allAgents.find(a => a.backend === 'claudecode');
    if (claudeCodeAgent) {
      setSelectedAgentValue(claudeCodeAgent.value);
      setDefaultSet(true);
      return;
    }

    // Try OpenCode second (prefer Sisyphus if available)
    const sisyphus = allAgents.find(a => a.backend === 'opencode' && a.agent === 'Sisyphus');
    if (sisyphus) {
      setSelectedAgentValue(sisyphus.value);
      setDefaultSet(true);
      return;
    }
    const openCodeAgent = allAgents.find(a => a.backend === 'opencode');
    if (openCodeAgent) {
      setSelectedAgentValue(openCodeAgent.value);
      setDefaultSet(true);
      return;
    }

    // Try Amp third
    const ampAgent = allAgents.find(a => a.backend === 'amp');
    if (ampAgent) {
      setSelectedAgentValue(ampAgent.value);
      setDefaultSet(true);
      return;
    }

    // Final fallback: use first available agent (shouldn't reach here)
    if (allAgents.length > 0) {
      setSelectedAgentValue(allAgents[0].value);
    }
    setDefaultSet(true);
  }, [open, defaultSet, allAgents, config, initialValues]);

  useEffect(() => {
    if (selectedBackend === 'amp' && modelOverride) {
      setModelOverride('');
    }
    if (selectedBackend !== 'codex' && modelEffort) {
      setModelEffort('');
    }
    // When switching backends, clear model override if current value isn't valid for the new backend
    if (prevBackendRef.current !== null && prevBackendRef.current !== selectedBackend && modelOverride) {
      const isValidForNewBackend = modelOptions.some(opt => opt.value === modelOverride);
      if (!isValidForNewBackend) {
        setModelOverride('');
      }
    }
    prevBackendRef.current = selectedBackend;
  }, [selectedBackend, modelOverride, modelEffort, modelOptions]);

  const resetForm = () => {
    setNewMissionWorkspace('');
    setSelectedAgentValue('');
    setModelOverride('');
    setModelEffort('');
    setDefaultSet(false);
  };

  const handleClose = () => {
    setOpen(false);
    resetForm();
    onClose?.();
  };

  const handleRefreshAgents = async () => {
    // Revalidate all agent lists
    await Promise.all([
      mutateOpencodeAgents?.(),
      mutateClaudecodeAgents?.(),
      mutateAmpAgents?.(),
      mutateCodexAgents?.(),
      mutateAgentsPayload?.(),
      mutateConfig?.(),
    ]);
  };

  const getCreateOptions = (): NewMissionDialogOptions => {
    const parsed = parseSelectedValue(selectedAgentValue);
    const trimmedModel = modelOverride.trim();
    const normalizedModel =
      selectedBackend === 'opencode'
        ? trimmedModel
        : trimmedModel.includes('/')
          ? trimmedModel.split('/').pop() || ''
          : trimmedModel;
    const modelOverrideValue =
      selectedBackend === 'amp' || !normalizedModel ? undefined : normalizedModel;
    const modelEffortValue =
      selectedBackend === 'codex' && modelEffort ? modelEffort : undefined;
    return {
      workspaceId: newMissionWorkspace || undefined,
      agent: parsed?.agent || undefined,
      backend: parsed?.backend || 'claudecode',
      modelOverride: modelOverrideValue,
      modelEffort: modelEffortValue,
      configProfile: workspaceProfile || undefined,
    };
  };

  const handleCreate = async (openInNewTab: boolean) => {
    if (disabled || submitting) return;
    setSubmitting(true);
    try {
      const options = getCreateOptions();
      const mission = await onCreate({ ...options, openInNewTab });
      const url = `${controlPath}?mission=${mission.id}`;

      if (openInNewTab) {
        window.open(url, '_blank');
        setOpen(false);
        resetForm();
        onClose?.();
      } else {
        // Close dialog state first, then navigate.
        // Do NOT call onClose here – it would trigger router.replace('/')
        // which overwrites the router.push below and prevents navigation
        // to the control page.
        setOpen(false);
        resetForm();
        router.push(url);
      }
    } finally {
      setSubmitting(false);
    }
  };

  const isBusy = disabled || submitting;

  return (
    <div className="relative" ref={dialogRef}>
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        disabled={isBusy}
        className="flex items-center gap-2 rounded-lg bg-indigo-500/20 px-3 py-2 text-sm font-medium text-indigo-400 hover:bg-indigo-500/30 transition-colors disabled:opacity-50"
      >
        <Plus className="h-4 w-4" />
        <span className="hidden sm:inline">New</span> Mission
      </button>
      {open && (
        <div className="absolute right-0 top-full mt-1 w-96 rounded-lg border border-white/[0.06] bg-[#1a1a1a] p-4 shadow-xl z-50">
          {/* Header with refresh and close buttons */}
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-medium text-white">Create New Mission</h3>
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={handleRefreshAgents}
                className="p-1 rounded-md text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors"
                title="Refresh agent list"
              >
                <RefreshCw className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={handleClose}
                className="p-1 rounded-md text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </div>

          <div className="space-y-3">
            {/* Workspace selection */}
            <div>
              <label className="block text-xs text-white/50 mb-1.5">Workspace</label>
              <select
                value={newMissionWorkspace}
                onChange={(e) => setNewMissionWorkspace(e.target.value)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none appearance-none cursor-pointer"
                style={{
                  backgroundImage:
                    "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                  backgroundPosition: 'right 0.5rem center',
                  backgroundRepeat: 'no-repeat',
                  backgroundSize: '1.5em 1.5em',
                  paddingRight: '2.5rem',
                }}
              >
                <option value="" className="bg-[#1a1a1a]">
                  Host (default)
                </option>
                {workspaces
                  .filter(
                    (ws) =>
                      ws.status === 'ready' &&
                      ws.id !== '00000000-0000-0000-0000-000000000000'
                  )
                  .map((workspace) => (
                    <option
                      key={workspace.id}
                      value={workspace.id}
                      className="bg-[#1a1a1a]"
                    >
                      {workspace.name} ({formatWorkspaceType(workspace.workspace_type)})
                    </option>
                  ))}
              </select>
              <p className="text-xs text-white/30 mt-1.5">Where the mission will run</p>
            </div>

            {/* Agent selection (includes backend) */}
            <div>
              <label className="block text-xs text-white/50 mb-1.5">Agent</label>
              <select
                value={selectedAgentValue}
                onChange={(e) => setSelectedAgentValue(e.target.value)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none appearance-none cursor-pointer"
                style={{
                  backgroundImage:
                    "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                  backgroundPosition: 'right 0.5rem center',
                  backgroundRepeat: 'no-repeat',
                  backgroundSize: '1.5em 1.5em',
                  paddingRight: '2.5rem',
                }}
              >
                {enabledBackends.map((backend) => {
                  const backendAgentsList = agentsByBackend[backend.id] || [];
                  if (backendAgentsList.length === 0) return null;

                  return (
                    <optgroup key={backend.id} label={backend.name} className="bg-[#1a1a1a]">
                      {backendAgentsList.map((agent) => (
                        <option key={agent.value} value={agent.value} className="bg-[#1a1a1a]">
                          {agent.displayName}{agent.backend === 'opencode' && agent.agent === 'Sisyphus' ? ' (recommended)' : ''}
                        </option>
                      ))}
                    </optgroup>
                  );
                })}
              </select>
              <p className="text-xs text-white/30 mt-1.5">
                Select an agent and backend to power this mission
              </p>
            </div>

            {/* Model override */}
            <div>
              <label className="block text-xs text-white/50 mb-1.5">Model override (optional)</label>
              <select
                value={modelOverride}
                onChange={(e) => setModelOverride(e.target.value)}
                disabled={selectedBackend === 'amp'}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none disabled:opacity-60 [&>option]:bg-slate-800 [&>option]:text-white [&>optgroup]:bg-slate-900 [&>optgroup]:text-white/70"
              >
                <option value="">
                  {selectedBackend === 'amp'
                    ? 'No override (Amp ignores model overrides)'
                    : 'No override (use default)'}
                </option>
                {(() => {
                  // Group options by provider
                  const providers = (providersResponse?.providers || []) as Provider[];
                  const groupedOptions = new Map<string, Array<{ value: string; label: string; description?: string; provider_id?: string }>>();

                  for (const option of modelOptions) {
                    // Extract provider from the label (format: "Provider Name — Model Name")
                    const providerName = option.label.split(' — ')[0] || 'Other';
                    if (!groupedOptions.has(providerName)) {
                      groupedOptions.set(providerName, []);
                    }
                    groupedOptions.get(providerName)!.push(option);
                  }

                  return Array.from(groupedOptions.entries()).map(([providerName, options]) => {
                    // For custom providers, include the provider ID in the label
                    const firstOption = options[0];
                    const groupLabel = firstOption?.provider_id
                      ? `${providerName} (ID: ${firstOption.provider_id})`
                      : providerName;

                    return (
                      <optgroup key={providerName} label={groupLabel}>
                        {options.map((option) => {
                          // Extract just the model name from the label
                          const modelName = option.label.split(' — ')[1] || option.label;
                          const displayText = option.description
                            ? `${modelName} - ${option.description}`
                            : modelName;
                          return (
                            <option key={option.value} value={option.value}>
                              {displayText}
                            </option>
                          );
                        })}
                      </optgroup>
                    );
                  });
                })()}
              </select>
              <p className="text-xs text-white/30 mt-1.5">
                {selectedBackend === 'amp'
                  ? 'Amp ignores model overrides.'
                  : selectedBackend === 'opencode'
                    ? 'Use provider/model format (e.g., openai/gpt-5-codex).'
                    : 'Use the raw model ID (e.g., gpt-5-codex or claude-opus-4-6).'}
              </p>
            </div>

            {selectedBackend === 'codex' && (
              <div>
                <label className="block text-xs text-white/50 mb-1.5">Model effort (optional)</label>
                <select
                  value={modelEffort}
                  onChange={(e) => setModelEffort(e.target.value as 'low' | 'medium' | 'high' | '')}
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none [&>option]:bg-slate-800 [&>option]:text-white"
                >
                  <option value="">Default effort</option>
                  <option value="low">Low</option>
                  <option value="medium">Medium</option>
                  <option value="high">High</option>
                </select>
                <p className="text-xs text-white/30 mt-1.5">
                  Passed to Codex as reasoning effort.
                </p>
              </div>
            )}

            {/* Action buttons */}
            <div className="flex gap-2 pt-1">
              <button
                type="button"
                onClick={() => handleCreate(false)}
                disabled={isBusy}
                className="flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors disabled:opacity-50"
              >
                Create here
              </button>
              <button
                type="button"
                onClick={() => handleCreate(true)}
                disabled={isBusy}
                className="flex-1 flex items-center justify-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
              >
                New Tab
                <ExternalLink className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
