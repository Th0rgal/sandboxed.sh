'use client';

import { useEffect, useRef, useState, useMemo } from 'react';
import { useRouter } from 'next/navigation';
import { Plus, X, ExternalLink } from 'lucide-react';
import useSWR from 'swr';
import { getVisibleAgents, getOpenAgentConfig, listBackends, listBackendAgents, getBackendConfig, getClaudeCodeConfig, type Backend, type BackendAgent } from '@/lib/api';
import type { Provider, Workspace } from '@/lib/api';

export interface CreateMissionOptions {
  workspaceId?: string;
  agent?: string;
  modelOverride?: string;
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
}

interface NewMissionDialogProps {
  workspaces: Workspace[];
  providers?: Provider[];
  disabled?: boolean;
  /** Creates a mission and returns its ID for navigation */
  onCreate: (options?: CreateMissionOptions) => Promise<CreatedMission>;
  /** Path to the control page (default: '/control') */
  controlPath?: string;
  /** Initial values to pre-fill the form (from current mission) */
  initialValues?: InitialMissionValues;
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

export function NewMissionDialog({
  workspaces,
  providers = [],
  disabled = false,
  onCreate,
  controlPath = '/control',
  initialValues,
}: NewMissionDialogProps) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [newMissionWorkspace, setNewMissionWorkspace] = useState('');
  // Combined value: "backend:agent" or empty for default
  const [selectedAgentValue, setSelectedAgentValue] = useState('');
  const [newMissionModelOverride, setNewMissionModelOverride] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [defaultSet, setDefaultSet] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);

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

  // Filter to only enabled backends
  const enabledBackends = useMemo(() => {
    return backends?.filter((b) => {
      if (b.id === 'opencode') return opencodeConfig?.enabled !== false;
      if (b.id === 'claudecode') return claudecodeConfig?.enabled !== false;
      if (b.id === 'amp') return ampConfig?.enabled !== false;
      return true;
    }) || [];
  }, [backends, opencodeConfig, claudecodeConfig, ampConfig]);

  // SWR: fetch agents for each enabled backend
  const { data: opencodeAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'opencode') ? 'backend-opencode-agents' : null,
    () => listBackendAgents('opencode'),
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );
  const { data: claudecodeAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'claudecode') ? 'backend-claudecode-agents' : null,
    () => listBackendAgents('claudecode'),
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );
  const { data: ampAgents } = useSWR<BackendAgent[]>(
    enabledBackends.some(b => b.id === 'amp') ? 'backend-amp-agents' : null,
    () => listBackendAgents('amp'),
    { revalidateOnFocus: false, dedupingInterval: 30000 }
  );

  // SWR: fallback for opencode agents
  const { data: agentsPayload } = useSWR('opencode-agents', getVisibleAgents, {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });
  const { data: config } = useSWR('openagent-config', getOpenAgentConfig, {
    revalidateOnFocus: false,
    dedupingInterval: 30000,
  });

  // SWR: fetch Claude Code config for hidden agents
  const { data: claudeCodeLibConfig } = useSWR(
    enabledBackends.some(b => b.id === 'claudecode') ? 'claudecode-lib-config' : null,
    getClaudeCodeConfig,
    { revalidateOnFocus: false, dedupingInterval: 30000 }
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
        const backendAgents = opencodeAgents || [];
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
  }, [enabledBackends, opencodeAgents, claudecodeAgents, ampAgents, agentsPayload, config, claudeCodeLibConfig]);

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

  // Get the currently selected backend
  const selectedBackend = useMemo(() => {
    const parsed = parseSelectedValue(selectedAgentValue);
    return parsed?.backend || null;
  }, [selectedAgentValue]);

  // Filter providers based on selected backend
  // Claude Code and Amp only support Anthropic models
  const filteredProviders = useMemo(() => {
    if (selectedBackend === 'claudecode' || selectedBackend === 'amp') {
      // Only show Anthropic (Claude) models for Claude Code and Amp
      return providers.filter(p => p.id === 'anthropic');
    }
    // Show all providers for OpenCode or when no backend is selected
    return providers;
  }, [providers, selectedBackend]);

  const formatWorkspaceType = (type: Workspace['workspace_type']) =>
    type === 'host' ? 'host' : 'isolated';

  // Reset model override when switching to Claude Code or Amp if current model is provider-prefixed
  useEffect(() => {
    if ((selectedBackend === 'claudecode' || selectedBackend === 'amp') && newMissionModelOverride) {
      // Claude Code and Amp expect raw model IDs (no provider prefix).
      if (newMissionModelOverride.includes('/')) {
        setNewMissionModelOverride('');
      }
    }
  }, [selectedBackend, newMissionModelOverride]);

  // Click outside and Escape key handler
  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false);
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open]);

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

    // Fallback: try Sisyphus in OpenCode
    const sisyphus = allAgents.find(a => a.backend === 'opencode' && a.agent === 'Sisyphus');
    if (sisyphus) {
      setSelectedAgentValue(sisyphus.value);
      setDefaultSet(true);
      return;
    }

    // Fallback: use first available agent
    if (allAgents.length > 0) {
      setSelectedAgentValue(allAgents[0].value);
    }
    setDefaultSet(true);
  }, [open, defaultSet, allAgents, config, initialValues]);

  const resetForm = () => {
    setNewMissionWorkspace('');
    setSelectedAgentValue('');
    setNewMissionModelOverride('');
    setDefaultSet(false);
  };

  const handleClose = () => {
    setOpen(false);
    resetForm();
  };

  const getCreateOptions = (): CreateMissionOptions => {
    const parsed = parseSelectedValue(selectedAgentValue);
    return {
      workspaceId: newMissionWorkspace || undefined,
      agent: parsed?.agent || undefined,
      modelOverride: newMissionModelOverride || undefined,
      backend: parsed?.backend || 'opencode',
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
      } else {
        router.push(url);
      }

      setOpen(false);
      resetForm();
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
          {/* Header with close button */}
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-medium text-white">Create New Mission</h3>
            <button
              type="button"
              onClick={handleClose}
              className="p-1 rounded-md text-white/40 hover:text-white/70 hover:bg-white/[0.04] transition-colors"
            >
              <X className="h-4 w-4" />
            </button>
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
              <label className="block text-xs text-white/50 mb-1.5">Model Override</label>
              <select
                value={newMissionModelOverride}
                onChange={(e) => setNewMissionModelOverride(e.target.value)}
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
                  Default (agent or global)
                </option>
                {filteredProviders.map((provider) => (
                  <optgroup key={provider.id} label={provider.name} className="bg-[#1a1a1a]">
                    {provider.models.map((model) => {
                      const value =
                        (selectedBackend === 'claudecode' || selectedBackend === 'amp') ? model.id : `${provider.id}/${model.id}`;
                      return (
                        <option
                          key={`${provider.id}/${model.id}`}
                          value={value}
                          className="bg-[#1a1a1a]"
                        >
                          {model.name || model.id}
                        </option>
                      );
                    })}
                  </optgroup>
                ))}
              </select>
              <p className="text-xs text-white/30 mt-1.5">
                Overrides the model for this mission
              </p>
            </div>

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
