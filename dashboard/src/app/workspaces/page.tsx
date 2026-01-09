'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
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
  CHROOT_DISTROS,
  type Workspace,
  type ChrootDistro,
  type WorkspaceTemplateSummary,
  type SkillSummary,
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
  FileText,
  Sparkles,
} from 'lucide-react';
import { cn } from '@/lib/utils';

// The nil UUID represents the default "host" workspace which cannot be deleted
const DEFAULT_WORKSPACE_ID = '00000000-0000-0000-0000-000000000000';

export default function WorkspacesPage() {
  const router = useRouter();
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [showNewWorkspaceDialog, setShowNewWorkspaceDialog] = useState(false);
  const [newWorkspaceName, setNewWorkspaceName] = useState('');
  const [newWorkspaceType, setNewWorkspaceType] = useState<'host' | 'chroot'>('chroot');
  const [newWorkspaceTemplate, setNewWorkspaceTemplate] = useState('');
  const [templates, setTemplates] = useState<WorkspaceTemplateSummary[]>([]);
  const [templatesError, setTemplatesError] = useState<string | null>(null);
  const [availableSkills, setAvailableSkills] = useState<SkillSummary[]>([]);
  const [skillsError, setSkillsError] = useState<string | null>(null);
  const [skillsFilter, setSkillsFilter] = useState('');
  const [selectedSkills, setSelectedSkills] = useState<string[]>([]);

  // Build state
  const [building, setBuilding] = useState(false);
  const [selectedDistro, setSelectedDistro] = useState<ChrootDistro>('ubuntu-noble');

  // Workspace settings state
  const [envRows, setEnvRows] = useState<{ id: string; key: string; value: string }[]>([]);
  const [initScript, setInitScript] = useState('');
  const [savingWorkspace, setSavingWorkspace] = useState(false);
  const [savingTemplate, setSavingTemplate] = useState(false);
  const [templateName, setTemplateName] = useState('');
  const [templateDescription, setTemplateDescription] = useState('');

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const workspacesData = await listWorkspaces();
      setWorkspaces(workspacesData);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load workspaces');
    } finally {
      setLoading(false);
    }
  };

  const loadTemplates = async () => {
    try {
      setTemplatesError(null);
      const templateData = await listWorkspaceTemplates();
      setTemplates(templateData);
    } catch (err) {
      setTemplates([]);
      setTemplatesError(err instanceof Error ? err.message : 'Failed to load templates');
    }
  };

  const loadSkills = async () => {
    try {
      setSkillsError(null);
      const skills = await listLibrarySkills();
      setAvailableSkills(skills);
    } catch (err) {
      setAvailableSkills([]);
      setSkillsError(err instanceof Error ? err.message : 'Failed to load skills');
    }
  };

  const toEnvRows = (env: Record<string, string>) =>
    Object.entries(env).map(([key, value]) => ({
      id: `${key}-${Math.random().toString(36).slice(2, 8)}`,
      key,
      value,
    }));

  const envRowsToMap = (rows: { key: string; value: string }[]) => {
    const env: Record<string, string> = {};
    rows.forEach((row) => {
      const key = row.key.trim();
      if (!key) return;
      env[key] = row.value;
    });
    return env;
  };

  useEffect(() => {
    loadData();
    loadTemplates();
    loadSkills();
  }, []);

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

  useEffect(() => {
    if (!selectedWorkspace) return;
    if (selectedWorkspace.distro) {
      setSelectedDistro(selectedWorkspace.distro as ChrootDistro);
    } else {
      setSelectedDistro('ubuntu-noble');
    }
    setEnvRows(toEnvRows(selectedWorkspace.env_vars ?? {}));
    setInitScript(selectedWorkspace.init_script ?? '');
    setSelectedSkills(selectedWorkspace.skills ?? []);
    setTemplateName(`${selectedWorkspace.name}-template`);
    setTemplateDescription('');
  }, [selectedWorkspace]);

  useEffect(() => {
    if (newWorkspaceTemplate) {
      setNewWorkspaceType('chroot');
    }
  }, [newWorkspaceTemplate]);

  const loadWorkspace = async (id: string) => {
    try {
      const workspace = await getWorkspace(id);
      setSelectedWorkspace(workspace);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace');
    }
  };

  const handleCreateWorkspace = async () => {
    if (!newWorkspaceName.trim()) return;
    try {
      setCreating(true);
      await createWorkspace({
        name: newWorkspaceName,
        workspace_type: newWorkspaceTemplate ? 'chroot' : newWorkspaceType,
        template: newWorkspaceTemplate || undefined,
      });
      await loadData();
      setShowNewWorkspaceDialog(false);
      setNewWorkspaceName('');
      setNewWorkspaceTemplate('');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create workspace');
    } finally {
      setCreating(false);
    }
  };

  const handleDeleteWorkspace = async (id: string, name: string) => {
    if (!confirm(`Delete workspace "${name}"?`)) return;
    try {
      await deleteWorkspace(id);
      setSelectedWorkspace(null);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete workspace');
    }
  };

  const handleBuildWorkspace = async (rebuild = false) => {
    if (!selectedWorkspace) return;
    try {
      setBuilding(true);
      setError(null);
      const updated = await buildWorkspace(selectedWorkspace.id, selectedDistro, rebuild);
      setSelectedWorkspace(updated);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to build workspace');
      // Refresh to get latest status
      await loadData();
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
      });
      setSelectedWorkspace(updated);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save workspace settings');
    } finally {
      setSavingWorkspace(false);
    }
  };

  const handleSaveTemplate = async () => {
    if (!selectedWorkspace) return;
    const trimmedName = templateName.trim();
    if (!trimmedName) {
      setError('Template name is required');
      return;
    }
    try {
      setSavingTemplate(true);
      const env_vars = envRowsToMap(envRows);
      await saveWorkspaceTemplate(trimmedName, {
        description: templateDescription.trim() || undefined,
        distro: selectedDistro,
        skills: selectedSkills,
        env_vars,
        init_script: initScript,
      });
      await loadTemplates();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save workspace template');
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
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
          <button onClick={() => setError(null)} className="ml-auto">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

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

                {workspace.error_message && (
                  <div className="text-xs text-red-400 mt-2">
                    Error: {workspace.error_message}
                  </div>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {/* Workspace Details Modal */}
      {selectedWorkspace && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
          onClick={() => setSelectedWorkspace(null)}
        >
          <div
            className="w-full max-w-5xl p-6 rounded-2xl bg-[#171719] border border-white/[0.08] shadow-[0_20px_80px_rgba(0,0,0,0.55)]"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between mb-6">
              <div className="flex items-center gap-3">
                <div className="h-12 w-12 rounded-xl bg-indigo-500/15 border border-indigo-500/30 flex items-center justify-center">
                  <Server className="h-6 w-6 text-indigo-300" />
                </div>
                <div>
                  <h3 className="text-xl font-semibold text-white">{selectedWorkspace.name}</h3>
                  <p className="text-sm text-white/60">Workspace details & runtime settings</p>
                </div>
              </div>
              <button
                onClick={() => setSelectedWorkspace(null)}
                className="p-2 rounded-lg hover:bg-white/[0.04] transition-colors"
              >
                <X className="h-4 w-4 text-white/60" />
              </button>
            </div>

            <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
              <div className="space-y-4">
                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <p className="text-xs text-white/50 uppercase tracking-wide mb-3">Overview</p>
                  <div className="flex flex-wrap gap-2 items-center">
                    <span className="px-2 py-1 rounded bg-white/[0.04] border border-white/[0.08] font-mono text-xs text-white">
                      {formatWorkspaceType(selectedWorkspace.workspace_type)}
                    </span>
                    <span
                      className={cn(
                        'inline-flex items-center px-2 py-1 rounded text-xs font-medium',
                        selectedWorkspace.status === 'ready'
                          ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                          : selectedWorkspace.status === 'building' || selectedWorkspace.status === 'pending'
                          ? 'bg-amber-500/10 text-amber-400 border border-amber-500/20'
                          : 'bg-red-500/10 text-red-400 border border-red-500/20'
                      )}
                    >
                      {selectedWorkspace.status}
                    </span>
                  </div>
                  <div className="mt-3 space-y-1 text-sm text-white/70">
                    <div className="flex items-center justify-between">
                      <span className="text-white/40">Template</span>
                      <span>{selectedWorkspace.template || 'None'}</span>
                    </div>
                    <div className="flex items-center justify-between">
                      <span className="text-white/40">Distro</span>
                      <span>{selectedWorkspace.distro || 'Default'}</span>
                    </div>
                  </div>
                </div>

                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <p className="text-xs text-white/50 uppercase tracking-wide mb-3">Location</p>
                  <div className="space-y-3">
                    <div>
                      <label className="text-xs text-white/40 block mb-1">Path</label>
                      <code className="block px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white/80 font-mono break-all">
                        {selectedWorkspace.path}
                      </code>
                    </div>
                    <div>
                      <label className="text-xs text-white/40 block mb-1">ID</label>
                      <code className="block px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white/80 font-mono">
                        {selectedWorkspace.id}
                      </code>
                    </div>
                    <div className="text-xs text-white/60">
                      Created {formatDate(selectedWorkspace.created_at)}
                    </div>
                  </div>
                </div>

                {selectedWorkspace.error_message && (
                  <div className="rounded-xl bg-red-500/10 border border-red-500/20 p-4 text-sm text-red-300">
                    {selectedWorkspace.error_message}
                  </div>
                )}

                {selectedWorkspace.workspace_type === 'chroot' && (
                  <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                    <p className="text-xs text-white/50 uppercase tracking-wide mb-3">Build Environment</p>
                    <div className="space-y-3">
                      <div>
                        <label className="text-xs text-white/50 block mb-1">Linux Distribution</label>
                        <select
                          value={selectedDistro}
                          onChange={(e) => setSelectedDistro(e.target.value as ChrootDistro)}
                          disabled={building || selectedWorkspace.status === 'building'}
                          className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-sm text-white focus:outline-none focus:border-indigo-500/50 disabled:opacity-50"
                        >
                          {CHROOT_DISTROS.map((distro) => (
                            <option key={distro.value} value={distro.value}>
                              {distro.label}
                            </option>
                          ))}
                        </select>
                      </div>
                      <button
                        onClick={() => handleBuildWorkspace(selectedWorkspace.status === 'ready')}
                        disabled={building || selectedWorkspace.status === 'building'}
                        className="flex items-center justify-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50 transition-colors"
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
                      <p className="text-xs text-white/40">
                        {selectedWorkspace.status === 'ready'
                          ? 'Rebuild will destroy the existing container and rerun the init script.'
                          : 'This creates an isolated Linux filesystem using debootstrap (Ubuntu/Debian) or pacstrap (Arch).'}
                      </p>
                    </div>
                  </div>
                )}

                {selectedWorkspace.status === 'building' && (
                  <div className="rounded-xl bg-amber-500/10 border border-amber-500/20 p-4">
                    <div className="flex items-center gap-2 text-amber-300">
                      <Loader className="h-4 w-4 animate-spin" />
                      <span className="text-sm">Building isolated environment...</span>
                    </div>
                    <p className="text-xs text-white/50 mt-2">
                      This may take several minutes. You can keep editing settings while it runs.
                    </p>
                  </div>
                )}
              </div>

              <div className="lg:col-span-2 space-y-4">
                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <div className="flex items-center justify-between mb-3">
                    <div className="flex items-center gap-2 text-white/80">
                      <Sparkles className="h-4 w-4 text-indigo-300" />
                      <p className="text-sm font-medium">Skills</p>
                    </div>
                    <span className="text-xs text-white/40">
                      {selectedSkills.length} selected
                    </span>
                  </div>

                  <div className="flex items-center gap-2 mb-3">
                    <input
                      value={skillsFilter}
                      onChange={(e) => setSkillsFilter(e.target.value)}
                      placeholder="Filter skills"
                      className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                    />
                  </div>

                  {skillsError ? (
                    <p className="text-xs text-red-400">{skillsError}</p>
                  ) : availableSkills.length === 0 ? (
                    <p className="text-xs text-white/40">
                      No skills found in the library.
                    </p>
                  ) : (
                    <div className="max-h-48 overflow-y-auto pr-2 space-y-2">
                      {filteredSkills.map((skill) => {
                        const active = selectedSkills.includes(skill.name);
                        return (
                          <button
                            key={skill.name}
                            onClick={() => toggleSkill(skill.name)}
                            className={cn(
                              'w-full text-left px-3 py-2 rounded-lg border transition-colors',
                              active
                                ? 'bg-indigo-500/10 border-indigo-500/30 text-white'
                                : 'bg-black/20 border-white/[0.08] text-white/70 hover:border-white/[0.2]'
                            )}
                          >
                            <div className="flex items-center justify-between gap-3">
                              <div className="text-xs font-medium">{skill.name}</div>
                              <div
                                className={cn(
                                  'text-[10px] uppercase tracking-wide',
                                  active ? 'text-indigo-200' : 'text-white/40'
                                )}
                              >
                                {active ? 'Enabled' : 'Disabled'}
                              </div>
                            </div>
                            {skill.description && (
                              <p className="mt-1 text-[11px] text-white/40">{skill.description}</p>
                            )}
                          </button>
                        );
                      })}
                      {filteredSkills.length === 0 && (
                        <p className="text-xs text-white/40">No skills match your filter.</p>
                      )}
                    </div>
                  )}

                  <p className="text-xs text-white/40 mt-3">
                    Enabled skills are synced into mission workspaces before a run starts.
                  </p>
                </div>

                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <div className="flex items-center justify-between mb-3">
                    <div className="flex items-center gap-2 text-white/80">
                      <FileText className="h-4 w-4 text-indigo-300" />
                      <p className="text-sm font-medium">Environment Variables</p>
                    </div>
                    <button
                      onClick={() =>
                        setEnvRows((rows) => [
                          ...rows,
                          { id: Math.random().toString(36).slice(2), key: '', value: '' },
                        ])
                      }
                      className="text-xs text-indigo-300 hover:text-indigo-200"
                    >
                      + Add variable
                    </button>
                  </div>

                  {envRows.length === 0 ? (
                    <p className="text-xs text-white/40">No environment variables configured.</p>
                  ) : (
                    <div className="space-y-2">
                      {envRows.map((row, idx) => (
                        <div key={row.id} className="grid grid-cols-[1fr_1fr_auto] gap-2">
                          <input
                            value={row.key}
                            onChange={(e) =>
                              setEnvRows((rows) =>
                                rows.map((r) =>
                                  r.id === row.id ? { ...r, key: e.target.value } : r
                                )
                              )
                            }
                            placeholder="KEY"
                            className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                          />
                          <input
                            value={row.value}
                            onChange={(e) =>
                              setEnvRows((rows) =>
                                rows.map((r) =>
                                  r.id === row.id ? { ...r, value: e.target.value } : r
                                )
                              )
                            }
                            placeholder="value"
                            className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                          />
                          <button
                            onClick={() => setEnvRows((rows) => rows.filter((r) => r.id !== row.id))}
                            className="px-2 py-2 text-white/40 hover:text-red-300"
                          >
                            <X className="h-4 w-4" />
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                  <p className="text-xs text-white/40 mt-3">
                    These variables are injected into workspace shells and MCP tool runs.
                  </p>
                </div>

                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <div className="flex items-center gap-2 text-white/80 mb-3">
                    <FileText className="h-4 w-4 text-indigo-300" />
                    <p className="text-sm font-medium">Init Script</p>
                  </div>
                  <textarea
                    value={initScript}
                    onChange={(e) => setInitScript(e.target.value)}
                    rows={8}
                    placeholder="#!/usr/bin/env bash\n# Install packages or setup files here"
                    className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 font-mono focus:outline-none focus:border-indigo-500/50"
                  />
                  <p className="text-xs text-white/40 mt-3">
                    Changes apply on the next build or rebuild.
                  </p>
                </div>

                <div className="rounded-xl bg-white/[0.03] border border-white/[0.08] p-4">
                  <div className="flex items-center gap-2 text-white/80 mb-3">
                    <Bookmark className="h-4 w-4 text-indigo-300" />
                    <p className="text-sm font-medium">Save as Template</p>
                  </div>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    <div>
                      <label className="text-xs text-white/40 block mb-1">Template Name</label>
                      <input
                        value={templateName}
                        onChange={(e) => setTemplateName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
                        placeholder="my-template"
                        className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                      />
                    </div>
                    <div>
                      <label className="text-xs text-white/40 block mb-1">Description</label>
                      <input
                        value={templateDescription}
                        onChange={(e) => setTemplateDescription(e.target.value)}
                        placeholder="Short description"
                        className="w-full px-3 py-2 rounded-lg bg-black/30 border border-white/[0.08] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                      />
                    </div>
                  </div>
                  <div className="flex items-center justify-between mt-4">
                    <p className="text-xs text-white/40">
                      Saves distro, env vars, and init script into the library.
                    </p>
                    <button
                      onClick={handleSaveTemplate}
                      disabled={savingTemplate}
                      className="flex items-center gap-2 px-3 py-2 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
                    >
                      {savingTemplate ? <Loader className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
                      Save Template
                    </button>
                  </div>
                </div>
              </div>
            </div>

            <div className="flex flex-wrap items-center justify-between gap-3 mt-6 pt-4 border-t border-white/[0.08]">
              <button
                onClick={() => setSelectedWorkspace(null)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Close
              </button>
              <div className="flex flex-wrap items-center gap-2">
                <button
                  onClick={handleSaveWorkspace}
                  disabled={savingWorkspace}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-white/[0.06] hover:bg-white/[0.1] border border-white/[0.08] rounded-lg disabled:opacity-50"
                >
                  {savingWorkspace ? <Loader className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
                  Save Settings
                </button>
                {selectedWorkspace.status === 'ready' && (
                  <button
                    onClick={() => {
                      router.push(`/console?workspace=${selectedWorkspace.id}&name=${encodeURIComponent(selectedWorkspace.name)}`);
                    }}
                    className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg"
                  >
                    <Terminal className="h-4 w-4" />
                    Open Shell
                  </button>
                )}
                {selectedWorkspace.id !== DEFAULT_WORKSPACE_ID && (
                  <button
                    onClick={() => {
                      handleDeleteWorkspace(selectedWorkspace.id, selectedWorkspace.name);
                      setSelectedWorkspace(null);
                    }}
                    className="px-4 py-2 text-sm font-medium text-white bg-red-500 hover:bg-red-600 rounded-lg"
                  >
                    Delete Workspace
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* New Workspace Dialog */}
      {showNewWorkspaceDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Workspace</h3>
            <div className="space-y-4">
              <div>
                <label className="text-xs text-white/60 mb-1 block">Name</label>
                <input
                  type="text"
                  placeholder="my-workspace"
                  value={newWorkspaceName}
                  onChange={(e) => setNewWorkspaceName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="text-xs text-white/60 mb-1 block">Template</label>
                <select
                  value={newWorkspaceTemplate}
                  onChange={(e) => setNewWorkspaceTemplate(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="">(No template)</option>
                  {templates.map((template) => (
                    <option key={template.name} value={template.name}>
                      {template.name}
                      {template.distro ? ` • ${template.distro}` : ''}
                    </option>
                  ))}
                </select>
                {templatesError ? (
                  <p className="text-xs text-red-400 mt-1.5">{templatesError}</p>
                ) : templates.length === 0 ? (
                  <p className="text-xs text-white/40 mt-1.5">No workspace templates found in the library.</p>
                ) : null}
                {newWorkspaceTemplate && (
                  <p className="text-xs text-white/40 mt-1.5">
                    Templates always create isolated workspaces and can set distro, env vars, and init scripts.
                  </p>
                )}
              </div>
              <div>
                <label className="text-xs text-white/60 mb-1 block">Type</label>
                <select
                  value={newWorkspaceType}
                  onChange={(e) => setNewWorkspaceType(e.target.value as 'host' | 'chroot')}
                  disabled={Boolean(newWorkspaceTemplate)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="host">Host (uses main filesystem)</option>
                  <option value="chroot">Isolated (root filesystem)</option>
                </select>
                <p className="text-xs text-white/40 mt-1.5">
                  {newWorkspaceTemplate
                    ? 'Template-selected workspaces always use an isolated root filesystem'
                    : newWorkspaceType === 'host'
                    ? 'Runs directly on the host machine filesystem'
                    : 'Creates an isolated root filesystem for running missions'}
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => {
                  setShowNewWorkspaceDialog(false);
                  setNewWorkspaceTemplate('');
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateWorkspace}
                disabled={!newWorkspaceName.trim() || creating}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {creating ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
