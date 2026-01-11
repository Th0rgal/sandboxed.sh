'use client';

import { useEffect, useRef, useState } from 'react';
import { Plus } from 'lucide-react';
import { getVisibleAgents, getOpenAgentConfig } from '@/lib/api';
import type { Provider, Workspace } from '@/lib/api';

interface NewMissionDialogProps {
  workspaces: Workspace[];
  providers?: Provider[];
  disabled?: boolean;
  onCreate: (options?: {
    workspaceId?: string;
    agent?: string;
    modelOverride?: string;
  }) => Promise<void> | void;
}

export function NewMissionDialog({
  workspaces,
  providers = [],
  disabled = false,
  onCreate,
}: NewMissionDialogProps) {
  const [open, setOpen] = useState(false);
  const [newMissionWorkspace, setNewMissionWorkspace] = useState('');
  const [newMissionAgent, setNewMissionAgent] = useState('');
  const [newMissionModelOverride, setNewMissionModelOverride] = useState('');
  const [opencodeAgents, setOpencodeAgents] = useState<string[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);

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

  const formatWorkspaceType = (type: Workspace['workspace_type']) =>
    type === 'host' ? 'host' : 'isolated';

  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;

    const loadAgentsAndConfig = async () => {
      try {
        // Load visible agents (pre-filtered by OpenAgent config)
        const payload = await getVisibleAgents();
        if (cancelled) return;
        const agents = parseAgentNames(payload);
        setOpencodeAgents(agents);

        // Load OpenAgent config for default agent
        const config = await getOpenAgentConfig();
        if (cancelled) return;

        // Set default agent from config, or fallback to Sisyphus if available
        if (config.default_agent && agents.includes(config.default_agent)) {
          setNewMissionAgent(config.default_agent);
        } else if (agents.includes("Sisyphus")) {
          setNewMissionAgent("Sisyphus");
        }
      } catch {
        if (!cancelled) {
          setOpencodeAgents([]);
        }
      }
    };

    void loadAgentsAndConfig();
    return () => {
      cancelled = true;
    };
  }, [open]);

  const resetForm = () => {
    setNewMissionWorkspace('');
    setNewMissionAgent('');
    setNewMissionModelOverride('');
  };

  const handleCancel = () => {
    setOpen(false);
    resetForm();
  };

  const handleCreate = async () => {
    if (disabled || submitting) return;
    setSubmitting(true);
    try {
      await onCreate({
        workspaceId: newMissionWorkspace || undefined,
        agent: newMissionAgent || undefined,
        modelOverride: newMissionModelOverride || undefined,
      });
      setOpen(false);
      resetForm();
    } finally {
      setSubmitting(false);
    }
  };

  const isBusy = disabled || submitting;
  const defaultAgentLabel = 'Default (OpenCode default)';

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
          <h3 className="text-sm font-medium text-white mb-3">Create New Mission</h3>
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

            {/* Agent selection */}
            <div>
              <label className="block text-xs text-white/50 mb-1.5">Agent Configuration</label>
              <select
                value={newMissionAgent}
                onChange={(e) => {
                  setNewMissionAgent(e.target.value);
                }}
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
                  {defaultAgentLabel}
                </option>
                {opencodeAgents.includes("Sisyphus") && (
                  <option value="Sisyphus" className="bg-[#1a1a1a]">
                    Sisyphus (recommended)
                  </option>
                )}
                {opencodeAgents.length > 0 && (
                  <optgroup label="OpenCode Agents" className="bg-[#1a1a1a]">
                    {opencodeAgents.map((agent) => (
                      <option key={agent} value={agent} className="bg-[#1a1a1a]">
                        {agent}
                      </option>
                    ))}
                  </optgroup>
                )}
              </select>
              <p className="text-xs text-white/30 mt-1.5">
                OpenCode agents are provided by plugins; defaults are recommended
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
                {providers.map((provider) => (
                  <optgroup key={provider.id} label={provider.name} className="bg-[#1a1a1a]">
                    {provider.models.map((model) => (
                      <option
                        key={`${provider.id}/${model.id}`}
                        value={`${provider.id}/${model.id}`}
                        className="bg-[#1a1a1a]"
                      >
                        {model.name || model.id}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
              <p className="text-xs text-white/30 mt-1.5">
                Overrides the model for this mission
              </p>
            </div>

            <div className="flex gap-2 pt-1">
              <button
                type="button"
                onClick={handleCancel}
                className="flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={handleCreate}
                disabled={isBusy}
                className="flex-1 rounded-lg bg-indigo-500 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
              >
                Create
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
