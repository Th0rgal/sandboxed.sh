'use client';

import { useEffect, useRef, useState } from 'react';
import { useRouter } from 'next/navigation';
import useSWR from 'swr';
import {
  listWorkspaces,
  getWorkspace,
  createWorkspace,
  deleteWorkspace,
  buildWorkspace,
  updateWorkspace,
  listWorkspaceTemplates,
  saveWorkspaceTemplate,
  listLibrarySkills,
  getWorkspaceDebug,
  getWorkspaceInitLog,
  CONTAINER_DISTROS,
  type Workspace,
  type ContainerDistro,
  type WorkspaceTemplateSummary,
  type SkillSummary,
  type WorkspaceDebugInfo,
  type InitLogResponse,
  type TailscaleMode,
} from '@/lib/api';
import {
  Plus,
  Trash2,
  X,
  Loader,
  AlertCircle,
  Server,
  FolderOpen,
  Clock,
  Hammer,
  Terminal,
  RefreshCw,
  Save,
  Bookmark,
  Sparkles,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useToast } from '@/components/toast';
import { ConfigCodeEditor } from '@/components/config-code-editor';
import { EnvVarsEditor, type EnvRow, toEnvRows, envRowsToMap, getEncryptedKeys } from '@/components/env-vars-editor';

// The nil UUID represents the default "host" workspace which cannot be deleted
const DEFAULT_WORKSPACE_ID = '00000000-0000-0000-0000-000000000000';

// Format bytes into human-readable size
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  if (gb < 1024) return `${gb.toFixed(2)} GB`;
  const tb = gb / 1024;
  return `${tb.toFixed(2)} TB`;
}

export default function WorkspacesPage() {
  const router = useRouter();
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [creating, setCreating] = useState(false);
  const { showError, showInfo } = useToast();

  const [showNewWorkspaceDialog, setShowNewWorkspaceDialog] = useState(false);
  const [newWorkspaceName, setNewWorkspaceName] = useState('');
  const [newWorkspaceType, setNewWorkspaceType] = useState<'host' | 'container'>('container');
  const [newWorkspaceTemplate, setNewWorkspaceTemplate] = useState('');
  const [skillsFilter, setSkillsFilter] = useState('');
  const [selectedSkills, setSelectedSkills] = useState<string[]>([]);
  const [workspaceTab, setWorkspaceTab] = useState<'overview' | 'skills' | 'environment' | 'template' | 'build'>('overview');

  // Build state
  const [building, setBuilding] = useState(false);
  const [selectedDistro, setSelectedDistro] = useState<ContainerDistro>('ubuntu-noble');
  const [buildDebug, setBuildDebug] = useState<WorkspaceDebugInfo | null>(null);
  const [buildLog, setBuildLog] = useState<InitLogResponse | null>(null);
  const buildLogRef = useRef<HTMLPreElement>(null);

  // Workspace settings state
  const [envRows, setEnvRows] = useState<EnvRow[]>([]);
  const [initScript, setInitScript] = useState('');
  const [sharedNetwork, setSharedNetwork] = useState<boolean | null>(null);
  const [tailscaleMode, setTailscaleMode] = useState<TailscaleMode | null>(null);
  const [savingWorkspace, setSavingWorkspace] = useState(false);
  const [savingTemplate, setSavingTemplate] = useState(false);
  const [templateName, setTemplateName] = useState('');
  const [templateDescription, setTemplateDescription] = useState('');

  // SWR: fetch workspaces (shared key with overview page)
  const { data: workspaces = [], isLoading: loading, mutate: mutateWorkspaces } = useSWR(
    'workspaces',
    listWorkspaces,
    { revalidateOnFocus: false }
  );

  // SWR: fetch templates
  const { data: templates = [], error: templatesError, mutate: mutateTemplates } = useSWR(
    'workspace-templates',
    listWorkspaceTemplates,
    { revalidateOnFocus: false }
  );

  // SWR: fetch skills (shared key with library)
  const { data: availableSkills = [], error: skillsError } = useSWR(
    'library-skills',
    listLibrarySkills,
    { revalidateOnFocus: false }
  );

  // Dynamic tabs based on workspace state - Build tab only shows for container workspaces
  const getWorkspaceTabs = (workspace: Workspace | null) => {
    const tabs: { id: 'overview' | 'skills' | 'environment' | 'template' | 'build'; label: string }[] = [
      { id: 'overview', label: 'Overview' },
      { id: 'skills', label: 'Skills' },
      { id: 'environment', label: 'Env' },
    ];
    // Add Build tab for container workspaces
    if (workspace?.workspace_type === 'container') {
      tabs.push({ id: 'build', label: 'Build' });
    }
    tabs.push({ id: 'template', label: 'Template' });
    return tabs;
  };
  const workspaceTabs = getWorkspaceTabs(selectedWorkspace);


  // Handle Escape key for modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (selectedWorkspace) setSelectedWorkspace(null);
        if (showNewWorkspaceDialog) setShowNewWorkspaceDialog(false);
      }
    };
    if (selectedWorkspace || showNewWorkspaceDialog) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [selectedWorkspace, showNewWorkspaceDialog]);

  // Track the last selected workspace ID to avoid resetting state on refresh
  const [lastSelectedId, setLastSelectedId] = useState<string | null>(null);

  useEffect(() => {
    if (!selectedWorkspace) {
      setLastSelectedId(null);
      return;
    }

    // Only reset form state when switching to a DIFFERENT workspace, not on refresh
    const isDifferentWorkspace = lastSelectedId !== selectedWorkspace.id;

    if (isDifferentWorkspace) {
      setLastSelectedId(selectedWorkspace.id);
      if (selectedWorkspace.distro) {
        setSelectedDistro(selectedWorkspace.distro as ContainerDistro);
      } else {
        setSelectedDistro('ubuntu-noble');
      }
      setEnvRows(toEnvRows(selectedWorkspace.env_vars ?? {}));
      setInitScript(selectedWorkspace.init_script ?? '');
      setSharedNetwork(selectedWorkspace.shared_network ?? null);
      setTailscaleMode(selectedWorkspace.tailscale_mode ?? null);
      setSelectedSkills(selectedWorkspace.skills ?? []);
      setTemplateName(`${selectedWorkspace.name}-template`);
      setTemplateDescription('');
      setWorkspaceTab('overview');
    }
  }, [selectedWorkspace, lastSelectedId]);

  useEffect(() => {
    if (newWorkspaceTemplate) {
      setNewWorkspaceType('container');
    }
  }, [newWorkspaceTemplate]);

  // Poll build progress when workspace is building, or fetch logs on error
  useEffect(() => {
    if (!selectedWorkspace) {
      setBuildDebug(null);
      setBuildLog(null);
      return;
    }

    const isBuilding = selectedWorkspace.status === 'building';
    const hasError = selectedWorkspace.status === 'error';

    // Clear state when transitioning to ready or other non-error states
    if (!isBuilding && !hasError) {
      setBuildDebug(null);
      setBuildLog(null);
      return;
    }

    // Auto-switch to Build tab when building starts or on error
    if (isBuilding || hasError) {
      setWorkspaceTab('build');
    }

    let cancelled = false;

    const fetchBuildInfo = async () => {
      try {
        const [debug, log] = await Promise.all([
          getWorkspaceDebug(selectedWorkspace.id).catch(() => null),
          getWorkspaceInitLog(selectedWorkspace.id).catch(() => null),
        ]);
        if (cancelled) return;
        if (debug) setBuildDebug(debug);
        if (log) setBuildLog(log);

        // Only poll for status updates when building (not when already in error state)
        if (isBuilding) {
          const updated = await getWorkspace(selectedWorkspace.id);
          if (cancelled) return;
          if (updated.status !== selectedWorkspace.status) {
            setSelectedWorkspace(updated);
            await mutateWorkspaces();
          }
        }
      } catch {
        // Ignore errors during polling
      }
    };

    // Fetch immediately
    fetchBuildInfo();

    // Only poll repeatedly when building, not when in error state
    if (isBuilding) {
      const interval = setInterval(fetchBuildInfo, 3000);
      return () => {
        cancelled = true;
        clearInterval(interval);
      };
    }

    return () => {
      cancelled = true;
    };
  }, [selectedWorkspace?.id, selectedWorkspace?.status]);

  // Auto-scroll build log to bottom when new content arrives
  useEffect(() => {
    if (buildLogRef.current) {
      buildLogRef.current.scrollTop = buildLogRef.current.scrollHeight;
    }
  }, [buildLog?.content]);

  const loadWorkspace = async (id: string) => {
    try {
      const workspace = await getWorkspace(id);
      setSelectedWorkspace(workspace);
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to load workspace');
    }
  };

  const handleCreateWorkspace = async () => {
    if (!newWorkspaceName.trim()) return;
    try {
      setCreating(true);
      const workspaceType = newWorkspaceTemplate ? 'container' : newWorkspaceType;
      const created = await createWorkspace({
        name: newWorkspaceName,
        workspace_type: workspaceType,
        template: newWorkspaceTemplate || undefined,
      });

      // Refresh workspace list immediately after creation so it appears in the UI
      // even if the build step fails later
      await mutateWorkspaces();

      // For container workspaces WITHOUT a template, trigger build manually.
      // Template-based workspaces are auto-built by the backend, so skip the explicit build call.
      let workspaceToShow = created;
      if (workspaceType === 'container' && !newWorkspaceTemplate) {
        try {
          workspaceToShow = await buildWorkspace(
            created.id,
            (created.distro as ContainerDistro) || 'ubuntu-noble',
            false
          );
        } catch (buildErr) {
          showError(buildErr instanceof Error ? buildErr.message : 'Failed to start build');
          // Refresh workspace to get error status
          try {
            workspaceToShow = await getWorkspace(created.id);
          } catch {
            // If getWorkspace also fails, use created workspace
          }
          // Refresh list again to show error status
          await mutateWorkspaces();
        }
      }
      setShowNewWorkspaceDialog(false);
      setNewWorkspaceName('');
      setNewWorkspaceTemplate('');
      setSelectedWorkspace(workspaceToShow);
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to create workspace');
    } finally {
      setCreating(false);
    }
  };

  const handleDeleteWorkspace = async (id: string, name: string) => {
    if (!confirm(`Delete workspace "${name}"?`)) return;
    try {
      await deleteWorkspace(id);
      setSelectedWorkspace(null);
      await mutateWorkspaces();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to delete workspace');
    }
  };

  const handleBuildWorkspace = async (rebuild = false) => {
    if (!selectedWorkspace) return;
    try {
      setBuilding(true);
      const updated = await buildWorkspace(selectedWorkspace.id, selectedDistro, rebuild);
      setSelectedWorkspace(updated);
      await mutateWorkspaces();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to build workspace');
      // Refresh to get latest status
      await mutateWorkspaces();
      if (selectedWorkspace) {
        const refreshed = await getWorkspace(selectedWorkspace.id);
        setSelectedWorkspace(refreshed);
      }
    } finally {
      setBuilding(false);
    }
  };

  const handleSaveWorkspace = async () => {
    if (!selectedWorkspace) return;
    try {
      setSavingWorkspace(true);
      const env_vars = envRowsToMap(envRows);
      const updated = await updateWorkspace(selectedWorkspace.id, {
        env_vars,
        init_script: initScript,
        skills: selectedSkills,
        shared_network: sharedNetwork,
        tailscale_mode: tailscaleMode,
      });
      setSelectedWorkspace(updated);
      await mutateWorkspaces();
      showInfo('Changes will apply to new missions', 'Saved');
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to save workspace settings');
    } finally {
      setSavingWorkspace(false);
    }
  };

  const handleSaveTemplate = async () => {
    if (!selectedWorkspace) return;
    const trimmedName = templateName.trim();
    if (!trimmedName) {
      showError('Template name is required');
      return;
    }
    try {
      setSavingTemplate(true);
      const env_vars = envRowsToMap(envRows);
      const encrypted_keys = getEncryptedKeys(envRows);
      await saveWorkspaceTemplate(trimmedName, {
        description: templateDescription.trim() || undefined,
        distro: selectedDistro,
        skills: selectedSkills,
        env_vars,
        encrypted_keys,
        init_script: initScript,
      });
      await mutateTemplates();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to save workspace template');
    } finally {
      setSavingTemplate(false);
    }
  };

  const toggleSkill = (name: string) => {
    setSelectedSkills((prev) =>
      prev.includes(name) ? prev.filter((skill) => skill !== name) : [...prev, name]
    );
  };

  const formatDate = (dateStr: string) => {
    const date = new Date(dateStr);
    return date.toLocaleDateString() + ' ' + date.toLocaleTimeString();
  };

  const formatWorkspaceType = (type: Workspace['workspace_type']) =>
    type === 'host' ? 'host' : 'isolated';

  // Extract the first meaningful line from error messages (ignores build output noise)
  const extractErrorSummary = (errorMessage: string): string => {
    // Split on newlines first, then on pipe (build output separator)
    const firstLine = errorMessage.split('\n')[0].trim();
    const beforePipe = firstLine.split(' | ')[0].trim();
    return beforePipe || 'Unknown error';
  };

  const filteredSkills = availableSkills.filter((skill) => {
    if (!skillsFilter.trim()) return true;
    const term = skillsFilter.trim().toLowerCase();
    return (
      skill.name.toLowerCase().includes(term) ||
      (skill.description ?? '').toLowerCase().includes(term)
    );
  });

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-7xl mx-auto space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold text-white">Workspaces</h1>
          <p className="text-sm text-white/60 mt-1">
            Isolated execution environments for running missions
          </p>
        </div>
        <button
          onClick={() => setShowNewWorkspaceDialog(true)}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
        >
          <Plus className="h-4 w-4" />
          New Workspace
        </button>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {workspaces.length === 0 ? (
          <div className="col-span-full p-12 text-center">
            <Server className="h-12 w-12 text-white/20 mx-auto mb-4" />
            <p className="text-white/40">No workspaces yet</p>
            <p className="text-sm text-white/30 mt-1">Create a workspace to get started</p>
          </div>
        ) : (
          workspaces.map((workspace) => (
            <div
              key={workspace.id}
              className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.06] hover:border-white/[0.12] transition-colors cursor-pointer"
              onClick={() => loadWorkspace(workspace.id)}
            >
              <div className="flex items-start justify-between mb-3">
                <div className="flex items-center gap-2">
                  <Server className="h-5 w-5 text-indigo-400" />
                  <h3 className="text-sm font-medium text-white">{workspace.name}</h3>
                </div>
                {workspace.id !== DEFAULT_WORKSPACE_ID && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleDeleteWorkspace(workspace.id, workspace.name);
                    }}
                    className="p-1 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                    title="Delete workspace"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                )}
              </div>

              <div className="space-y-2">
                <div className="flex items-center gap-2 text-xs text-white/60">
                  <span className="px-2 py-0.5 rounded bg-white/[0.04] border border-white/[0.08] font-mono">
                    {formatWorkspaceType(workspace.workspace_type)}
                  </span>
                  <span
                    className={cn(
                      'px-2 py-0.5 rounded text-xs font-medium',
                      workspace.status === 'ready'
                        ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                        : workspace.status === 'building' || workspace.status === 'pending'
                        ? 'bg-amber-500/10 text-amber-400 border border-amber-500/20'
                        : 'bg-red-500/10 text-red-400 border border-red-500/20'
                    )}
                  >
                    {workspace.status}
                  </span>
                </div>

                <div className="flex items-center gap-2 text-xs text-white/40">
                  <FolderOpen className="h-3.5 w-3.5" />
                  <span className="truncate font-mono">{workspace.path}</span>
                </div>

                <div className="flex items-center gap-2 text-xs text-white/40">
                  <Clock className="h-3.5 w-3.5" />
                  <span>Created {formatDate(workspace.created_at)}</span>
                </div>
              </div>
            </div>
          ))
        )}
      </div>

      {/* Workspace Details Modal */}
      {selectedWorkspace && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4 py-6"
          onClick={() => setSelectedWorkspace(null)}
        >
          <div
            className="w-full max-w-2xl max-h-[85vh] rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] flex flex-col overflow-hidden animate-scale-in-simple"
            onClick={(e) => e.stopPropagation()}
          >
            {/* Header - Compact */}
            <div className="px-5 pt-4 pb-3 border-b border-white/[0.06]">
              <div className="flex items-center justify-between mb-3">
                <div className="flex items-center gap-2">
                  <h3 className="text-base font-medium text-white">{selectedWorkspace.name}</h3>
                  <span className="text-white/20">·</span>
                  <span className="text-xs text-white/40">
                    {formatWorkspaceType(selectedWorkspace.workspace_type)}
                  </span>
                  <span className="text-white/20">·</span>
                  <span
                    className={cn(
                      'text-xs font-medium',
                      selectedWorkspace.status === 'ready'
                        ? 'text-emerald-400'
                        : selectedWorkspace.status === 'building' || selectedWorkspace.status === 'pending'
                        ? 'text-amber-400'
                        : 'text-red-400'
                    )}
                  >
                    {selectedWorkspace.status === 'building' && (
                      <Loader className="inline h-3 w-3 animate-spin mr-1" />
                    )}
                    {selectedWorkspace.status}
                  </span>
                </div>
                <button
                  onClick={() => setSelectedWorkspace(null)}
                  className="p-1.5 -mr-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06] transition-colors"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>

              {/* Tabs */}
              <div className="flex items-center gap-1">
                {workspaceTabs.map((tab) => (
                  <button
                    key={tab.id}
                    onClick={() => setWorkspaceTab(tab.id)}
                    className={cn(
                      'px-3 py-1.5 text-xs font-medium rounded-lg transition-all',
                      workspaceTab === tab.id
                        ? 'bg-white/[0.08] text-white'
                        : 'text-white/50 hover:text-white/80 hover:bg-white/[0.04]'
                    )}
                  >
                    {tab.label}
                  </button>
                ))}
              </div>
            </div>

            {/* Content */}
            <div className="flex-1 min-h-0 overflow-y-auto">
              {workspaceTab === 'overview' && (
                <div className="px-5 py-4 space-y-4">
                  {/* Quick Info - Inline badges */}
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    {selectedWorkspace.template && (
                      <span className="px-2.5 py-1 rounded-md bg-white/[0.04] border border-white/[0.06] text-white/70">
                        Template: <span className="text-white/90">{selectedWorkspace.template}</span>
                      </span>
                    )}
                    {selectedWorkspace.distro && (
                      <span className="px-2.5 py-1 rounded-md bg-white/[0.04] border border-white/[0.06] text-white/70">
                        Distro: <span className="text-white/90">{selectedWorkspace.distro}</span>
                      </span>
                    )}
                    <span className="px-2.5 py-1 rounded-md bg-white/[0.04] border border-white/[0.06] text-white/50">
                      {formatDate(selectedWorkspace.created_at)}
                    </span>
                  </div>

                  {/* Path - Minimal */}
                  <div className="text-xs text-white/40">
                    <code className="font-mono text-white/60">{selectedWorkspace.path}</code>
                  </div>

                  {selectedWorkspace.error_message && (
                    <div className="rounded-lg bg-red-500/5 border border-red-500/15 p-3">
                      <div className="flex items-start gap-2">
                        <AlertCircle className="h-4 w-4 text-red-400 shrink-0 mt-0.5" />
                        <p className="text-sm text-red-300">{extractErrorSummary(selectedWorkspace.error_message)}</p>
                      </div>
                    </div>
                  )}

                  {/* Network settings for container workspaces */}
                  {selectedWorkspace.workspace_type === 'container' && (
                    <div className="rounded-lg bg-white/[0.02] border border-white/[0.05] p-3">
                      <div className="flex items-center justify-between">
                        <div>
                          <p className="text-xs text-white/60 font-medium">Shared Network</p>
                          <p className="text-[10px] text-white/30 mt-0.5">
                            Share host network and DNS. Disable for isolated networking.
                          </p>
                        </div>
                        <button
                          onClick={() => {
                            if (sharedNetwork === null) setSharedNetwork(false);
                            else if (sharedNetwork === false) setSharedNetwork(true);
                            else setSharedNetwork(null);
                          }}
                          className={cn(
                            "relative w-10 h-5 rounded-full transition-colors",
                            sharedNetwork === null
                              ? "bg-white/10"
                              : sharedNetwork
                                ? "bg-emerald-500/50"
                                : "bg-red-500/30"
                          )}
                        >
                          <span
                            className={cn(
                              "absolute top-0.5 w-4 h-4 rounded-full bg-white transition-all",
                              sharedNetwork === null || sharedNetwork
                                ? "left-5"
                                : "left-0.5"
                            )}
                          />
                        </button>
                      </div>
                      <p className="text-[10px] text-white/25 mt-1.5">
                        {sharedNetwork === null
                          ? "Default (enabled)"
                          : sharedNetwork
                            ? "Enabled"
                            : "Disabled (isolated)"}
                      </p>

                      {/* Tailscale Mode - only show when shared_network is disabled */}
                      {sharedNetwork === false && (
                        <div className="mt-3 pt-3 border-t border-white/[0.05]">
                          <p className="text-xs text-white/60 font-medium mb-1">Tailscale Mode</p>
                          <p className="text-[10px] text-white/30 mb-2">
                            How to route traffic when Tailscale is configured (via TS_AUTHKEY).
                          </p>
                          <select
                            value={tailscaleMode || 'exit_node'}
                            onChange={(e) => setTailscaleMode(e.target.value as TailscaleMode)}
                            className="w-full px-2.5 py-1.5 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                            style={{
                              backgroundImage:
                                "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                              backgroundPosition: 'right 0.5rem center',
                              backgroundRepeat: 'no-repeat',
                              backgroundSize: '1em 1em',
                            }}
                          >
                            <option value="exit_node">Exit Node (route all traffic via Tailscale)</option>
                            <option value="tailnet_only">Tailnet Only (host internet, Tailscale for devices)</option>
                          </select>
                          <p className="text-[10px] text-white/25 mt-1.5">
                            {tailscaleMode === 'tailnet_only'
                              ? "Use host gateway for internet, Tailscale only for tailnet devices."
                              : "Route all traffic through Tailscale exit node (requires TS_EXIT_NODE)."}
                          </p>
                        </div>
                      )}
                    </div>
                  )}

                  {/* Action hint for container workspaces */}
                  {selectedWorkspace.workspace_type === 'container' && selectedWorkspace.status !== 'building' && selectedWorkspace.status !== 'ready' && (
                    <div className="rounded-lg bg-amber-500/5 border border-amber-500/15 p-3">
                      <p className="text-xs text-amber-300/80">
                        Go to the <button onClick={() => setWorkspaceTab('build')} className="underline hover:text-amber-200">Build</button> tab to create the container environment.
                      </p>
                    </div>
                  )}
                </div>
              )}

              {workspaceTab === 'build' && selectedWorkspace.workspace_type === 'container' && (
                <div className="px-5 py-4 flex flex-col h-full">
                  {/* Build controls - shown when not building */}
                  {selectedWorkspace.status !== 'building' && (
                    <div className="space-y-4 mb-4">
                      <div className="flex items-center gap-3">
                        <select
                          value={selectedDistro}
                          onChange={(e) => setSelectedDistro(e.target.value as ContainerDistro)}
                          disabled={building}
                          className="px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white focus:outline-none focus:border-indigo-500/50 disabled:opacity-50 appearance-none cursor-pointer"
                          style={{
                            backgroundImage:
                              "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                            backgroundPosition: 'right 0.5rem center',
                            backgroundRepeat: 'no-repeat',
                            backgroundSize: '1.25em 1.25em',
                            paddingRight: '2rem',
                          }}
                        >
                          {CONTAINER_DISTROS.map((distro) => (
                            <option key={distro.value} value={distro.value} className="bg-[#161618]">
                              {distro.label}
                            </option>
                          ))}
                        </select>
                        <button
                          onClick={() => handleBuildWorkspace(selectedWorkspace.status === 'ready')}
                          disabled={building}
                          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 transition-colors"
                        >
                          {building ? (
                            <>
                              <Loader className="h-4 w-4 animate-spin" />
                              {selectedWorkspace.status === 'ready' ? 'Rebuilding...' : 'Building...'}
                            </>
                          ) : selectedWorkspace.status === 'ready' ? (
                            <>
                              <RefreshCw className="h-4 w-4" />
                              Rebuild
                            </>
                          ) : (
                            <>
                              <Hammer className="h-4 w-4" />
                              Build
                            </>
                          )}
                        </button>
                        <span className="text-xs text-white/40">
                          {selectedWorkspace.status === 'ready'
                            ? 'Destroys container and reruns init script'
                            : 'Creates isolated Linux filesystem'}
                        </span>
                      </div>

                      {/* Init Script */}
                      <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                        <div className="px-4 py-3 border-b border-white/[0.05] flex items-center gap-2">
                          <Terminal className="h-4 w-4 text-indigo-400" />
                          <p className="text-xs text-white/50 font-medium">Init Script</p>
                        </div>
                        <div className="p-4">
                          <ConfigCodeEditor
                            value={initScript}
                            onChange={setInitScript}
                            placeholder="#!/usr/bin/env bash&#10;# Install packages or setup files here"
                            className="min-h-[180px]"
                            minHeight={180}
                            language="bash"
                          />
                          <p className="text-xs text-white/35 mt-3">
                            Runs during build. Save changes, then Rebuild to apply.
                          </p>
                        </div>
                      </div>
                    </div>
                  )}

                  {/* Build Progress - shown when building or on error */}
                  {(selectedWorkspace.status === 'building' || selectedWorkspace.status === 'error') && (
                    <div className="flex-1 flex flex-col min-h-0">
                      {/* Error message */}
                      {selectedWorkspace.status === 'error' && selectedWorkspace.error_message && (
                        <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 mb-3">
                          <div className="flex items-start gap-2">
                            <AlertCircle className="h-4 w-4 text-red-400 shrink-0 mt-0.5" />
                            <div className="flex-1 min-w-0">
                              <p className="text-sm text-red-300 font-medium">Build Failed</p>
                              <p className="text-xs text-red-300/70 mt-1 break-words">{selectedWorkspace.error_message}</p>
                              {selectedWorkspace.error_message.includes('signal KILL') && (
                                <p className="text-xs text-red-300/50 mt-2">
                                  SIGKILL usually indicates out-of-memory. Try reducing packages installed or increasing server memory.
                                </p>
                              )}
                            </div>
                          </div>
                        </div>
                      )}

                      {/* Status header */}
                      <div className="flex items-center justify-between mb-3">
                        <div className="flex flex-wrap items-center gap-2">
                          {buildDebug?.has_bash && (
                            <span className="px-2 py-0.5 text-[10px] font-medium bg-emerald-500/10 text-emerald-400 border border-emerald-500/20 rounded">
                              bash ready
                            </span>
                          )}
                          {selectedWorkspace.status === 'building' && buildDebug?.init_script_exists && (
                            <span className="px-2 py-0.5 text-[10px] font-medium bg-blue-500/10 text-blue-400 border border-blue-500/20 rounded">
                              init script running
                            </span>
                          )}
                          {buildDebug?.distro && (
                            <span className="px-2 py-0.5 text-[10px] font-mono text-white/40 bg-white/[0.04] border border-white/[0.06] rounded">
                              {buildDebug.distro}
                            </span>
                          )}
                        </div>
                        {buildDebug?.size_bytes != null && buildDebug.size_bytes > 0 && (
                          <span className="text-[10px] text-white/40 font-mono">
                            {formatBytes(buildDebug.size_bytes)}
                          </span>
                        )}
                      </div>

                      {/* Log output - constrained height with internal scroll */}
                      {buildLog?.exists && buildLog.content ? (
                        <div className="max-h-64 rounded-lg bg-black/30 border border-white/[0.06] overflow-hidden flex flex-col">
                          <div className="px-3 py-1.5 border-b border-white/[0.06] flex items-center justify-between shrink-0">
                            <span className="text-[10px] text-white/40 font-mono">{buildLog.log_path}</span>
                            {buildLog.total_lines && (
                              <span className="text-[10px] text-white/30">{buildLog.total_lines} lines</span>
                            )}
                          </div>
                          <pre
                            ref={buildLogRef}
                            className="flex-1 min-h-0 p-3 text-[11px] font-mono text-white/70 overflow-auto whitespace-pre-wrap break-all"
                          >
                            {buildLog.content.split('\n').slice(-100).join('\n')}
                          </pre>
                        </div>
                      ) : selectedWorkspace.status === 'error' ? (
                        <div className="h-20 flex items-center justify-center text-xs text-white/40">
                          <span>No build log available</span>
                        </div>
                      ) : (
                        <div className="h-32 flex items-center justify-center text-xs text-white/40">
                          <Loader className="h-3 w-3 animate-spin mr-2" />
                          <span>Waiting for build output...</span>
                        </div>
                      )}
                    </div>
                  )}

                  {/* Ready state info */}
                  {selectedWorkspace.status === 'ready' && (
                    <div className="text-xs text-white/40 mt-2">
                      Container is ready. Use Rebuild to recreate with updated init script or distro.
                    </div>
                  )}
                </div>
              )}

              {workspaceTab === 'skills' && (
                <div className="px-6 py-5">
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                    <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <Sparkles className="h-4 w-4 text-indigo-400" />
                        <p className="text-xs text-white/50 font-medium">Skills</p>
                      </div>
                      <span className="text-xs text-white/40">
                        {selectedSkills.length} enabled
                      </span>
                    </div>

                    <div className="p-4">
                      <input
                        value={skillsFilter}
                        onChange={(e) => setSkillsFilter(e.target.value)}
                        placeholder="Search skills..."
                        className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-3"
                      />

                      {skillsError ? (
                        <p className="text-xs text-red-400 py-4 text-center">{skillsError instanceof Error ? skillsError.message : 'Failed to load skills'}</p>
                      ) : availableSkills.length === 0 ? (
                        <div className="py-8 text-center">
                          <Sparkles className="h-8 w-8 text-white/10 mx-auto mb-2" />
                          <p className="text-xs text-white/40">No skills in library</p>
                        </div>
                      ) : (
                        <div className="max-h-64 overflow-y-auto space-y-1.5">
                          {filteredSkills.map((skill) => {
                            const active = selectedSkills.includes(skill.name);
                            return (
                              <button
                                key={skill.name}
                                onClick={() => toggleSkill(skill.name)}
                                className={cn(
                                  'w-full text-left px-3 py-2.5 rounded-lg border transition-all',
                                  active
                                    ? 'bg-indigo-500/10 border-indigo-500/25 text-white'
                                    : 'bg-black/10 border-white/[0.04] text-white/70 hover:bg-black/20 hover:border-white/[0.08]'
                                )}
                              >
                                <div className="flex items-center justify-between gap-3">
                                  <span className="text-xs font-medium">{skill.name}</span>
                                  <span
                                    className={cn(
                                      'text-[10px] font-medium uppercase tracking-wider',
                                      active ? 'text-indigo-300' : 'text-white/30'
                                    )}
                                  >
                                    {active ? 'On' : 'Off'}
                                  </span>
                                </div>
                                {skill.description && (
                                  <p className="mt-1 text-[11px] text-white/40 line-clamp-1">{skill.description}</p>
                                )}
                              </button>
                            );
                          })}
                          {filteredSkills.length === 0 && (
                            <p className="text-xs text-white/40 py-4 text-center">No matching skills</p>
                          )}
                        </div>
                      )}

                      <p className="text-xs text-white/35 mt-4 pt-3 border-t border-white/[0.04]">
                        Skills are synced to workspace before each mission run.
                      </p>
                    </div>
                  </div>
                </div>
              )}

              {workspaceTab === 'environment' && (
                <div className="px-6 py-5 space-y-4">
                  <EnvVarsEditor
                    rows={envRows}
                    onChange={setEnvRows}
                    description="Injected into workspace shells and MCP tool runs. Use workspace templates to configure encryption for sensitive values."
                  />
                  <p className="text-xs text-white/35">
                    Applied to new missions automatically. Running missions keep their original values.
                  </p>
                </div>
              )}

              {workspaceTab === 'template' && (
                <div className="px-6 py-5">
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                    <div className="px-4 py-3 border-b border-white/[0.05] flex items-center gap-2">
                      <Bookmark className="h-4 w-4 text-indigo-400" />
                      <p className="text-xs text-white/50 font-medium">Save as Template</p>
                    </div>
                    <div className="p-4 space-y-4">
                      <div className="grid grid-cols-2 gap-3">
                        <div>
                          <label className="text-xs text-white/40 block mb-2">Template Name</label>
                          <input
                            value={templateName}
                            onChange={(e) => setTemplateName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
                            placeholder="my-template"
                            className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                          />
                        </div>
                        <div>
                          <label className="text-xs text-white/40 block mb-2">Description</label>
                          <input
                            value={templateDescription}
                            onChange={(e) => setTemplateDescription(e.target.value)}
                            placeholder="Short description"
                            className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                          />
                        </div>
                      </div>

                      <div className="pt-3 border-t border-white/[0.04]">
                        <div className="flex items-center justify-between">
                          <p className="text-xs text-white/35">
                            Saves current distro, env vars, and init script to library.
                          </p>
                          <button
                            onClick={handleSaveTemplate}
                            disabled={savingTemplate || !templateName.trim()}
                            className="flex items-center gap-2 px-4 py-2 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 transition-colors"
                          >
                            {savingTemplate ? (
                              <Loader className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <Save className="h-3.5 w-3.5" />
                            )}
                            Save Template
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
              )}
            </div>

            {/* Footer */}
            <div className="px-6 py-4 border-t border-white/[0.06] flex items-center justify-between gap-4">
              <button
                onClick={() => setSelectedWorkspace(null)}
                className="text-sm text-white/50 hover:text-white/80 transition-colors"
              >
                Close
              </button>
              <div className="flex items-center gap-2">
                {selectedWorkspace.id !== DEFAULT_WORKSPACE_ID && (
                  <button
                    onClick={() => {
                      handleDeleteWorkspace(selectedWorkspace.id, selectedWorkspace.name);
                      setSelectedWorkspace(null);
                    }}
                    className="px-4 py-2 text-sm font-medium text-red-400 hover:text-red-300 hover:bg-red-500/10 rounded-lg transition-colors"
                  >
                    Delete
                  </button>
                )}
                <button
                  onClick={handleSaveWorkspace}
                  disabled={savingWorkspace}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-white/[0.06] hover:bg-white/[0.1] rounded-lg disabled:opacity-50 transition-colors"
                >
                  {savingWorkspace ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                  Save
                </button>
                {selectedWorkspace.status === 'ready' && (
                  <button
                    onClick={() => {
                      router.push(`/console?workspace=${selectedWorkspace.id}&name=${encodeURIComponent(selectedWorkspace.name)}`);
                    }}
                    className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
                  >
                    <Terminal className="h-3.5 w-3.5" />
                    Shell
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* New Workspace Dialog */}
      {showNewWorkspaceDialog && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4"
          onClick={() => {
            setShowNewWorkspaceDialog(false);
            setNewWorkspaceTemplate('');
          }}
        >
          <div
            className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden animate-scale-in-simple"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="px-6 pt-5 pb-4 border-b border-white/[0.06]">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-3">
                  <div className="h-10 w-10 rounded-xl bg-gradient-to-br from-indigo-500/20 to-indigo-600/10 border border-indigo-500/20 flex items-center justify-center">
                    <Plus className="h-5 w-5 text-indigo-400" />
                  </div>
                  <h3 className="text-lg font-medium text-white">New Workspace</h3>
                </div>
                <button
                  onClick={() => {
                    setShowNewWorkspaceDialog(false);
                    setNewWorkspaceTemplate('');
                  }}
                  className="p-2 -mr-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06] transition-colors"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
            </div>

            <div className="px-6 py-5 space-y-4">
              <div>
                <label className="text-xs text-white/40 mb-2 block">Workspace Name</label>
                <input
                  type="text"
                  placeholder="my-workspace"
                  value={newWorkspaceName}
                  onChange={(e) => setNewWorkspaceName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
                  className="w-full px-3 py-2.5 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                  autoFocus
                />
              </div>

              <div>
                <label className="text-xs text-white/40 mb-2 block">Template</label>
                <select
                  value={newWorkspaceTemplate}
                  onChange={(e) => setNewWorkspaceTemplate(e.target.value)}
                  className="w-full px-3 py-2.5 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                  style={{
                    backgroundImage:
                      "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                    backgroundPosition: 'right 0.75rem center',
                    backgroundRepeat: 'no-repeat',
                    backgroundSize: '1.25em 1.25em',
                  }}
                >
                  <option value="" className="bg-[#161618]">None</option>
                  {templates.map((template) => (
                    <option key={template.name} value={template.name} className="bg-[#161618]">
                      {template.name}
                      {template.distro ? ` · ${template.distro}` : ''}
                    </option>
                  ))}
                </select>
                {templatesError && (
                  <p className="text-xs text-red-400 mt-1.5">{templatesError instanceof Error ? templatesError.message : 'Failed to load templates'}</p>
                )}
              </div>

              <div>
                <label className="text-xs text-white/40 mb-2 block">Type</label>
                <select
                  value={newWorkspaceType}
                  onChange={(e) => setNewWorkspaceType(e.target.value as 'host' | 'container')}
                  disabled={Boolean(newWorkspaceTemplate)}
                  className="w-full px-3 py-2.5 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer disabled:opacity-50"
                  style={{
                    backgroundImage:
                      "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                    backgroundPosition: 'right 0.75rem center',
                    backgroundRepeat: 'no-repeat',
                    backgroundSize: '1.25em 1.25em',
                  }}
                >
                  <option value="host" className="bg-[#161618]">Host (main filesystem)</option>
                  <option value="container" className="bg-[#161618]">Isolated (container)</option>
                </select>
                <p className="text-xs text-white/35 mt-2">
                  {newWorkspaceTemplate
                    ? 'Templates always create isolated workspaces'
                    : newWorkspaceType === 'host'
                    ? 'Runs directly on host machine'
                    : 'Creates isolated Linux filesystem'}
                </p>
              </div>
            </div>

            <div className="px-6 py-4 border-t border-white/[0.06] flex items-center justify-end gap-2">
              <button
                onClick={() => {
                  setShowNewWorkspaceDialog(false);
                  setNewWorkspaceTemplate('');
                }}
                className="px-4 py-2 text-sm text-white/50 hover:text-white/80 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateWorkspace}
                disabled={!newWorkspaceName.trim() || creating}
                className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 transition-colors"
              >
                {creating && <Loader className="h-3.5 w-3.5 animate-spin" />}
                {creating ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
