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
import { useToast } from '@/components/toast';
import { ConfigCodeEditor } from '@/components/config-code-editor';

// The nil UUID represents the default "host" workspace which cannot be deleted
const DEFAULT_WORKSPACE_ID = '00000000-0000-0000-0000-000000000000';

export default function WorkspacesPage() {
  const router = useRouter();
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const { showError } = useToast();

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
  const [workspaceTab, setWorkspaceTab] = useState<'overview' | 'skills' | 'environment' | 'template'>('overview');

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
      const workspacesData = await listWorkspaces();
      setWorkspaces(workspacesData);
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to load workspaces');
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

  const workspaceTabs = [
    { id: 'overview', label: 'Overview' },
    { id: 'skills', label: 'Skills' },
    { id: 'environment', label: 'Env & Init' },
    { id: 'template', label: 'Template' },
  ] as const;

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
    setWorkspaceTab('overview');
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
      showError(err instanceof Error ? err.message : 'Failed to load workspace');
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
      await loadData();
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
      await loadData();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to build workspace');
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
      await saveWorkspaceTemplate(trimmedName, {
        description: templateDescription.trim() || undefined,
        distro: selectedDistro,
        skills: selectedSkills,
        env_vars,
        init_script: initScript,
      });
      await loadTemplates();
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
            {/* Header */}
            <div className="px-6 pt-5 pb-4 border-b border-white/[0.06]">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-4">
                  <div className="h-11 w-11 rounded-xl bg-gradient-to-br from-indigo-500/20 to-indigo-600/10 border border-indigo-500/20 flex items-center justify-center">
                    <Server className="h-5 w-5 text-indigo-400" />
                  </div>
                  <div>
                    <h3 className="text-lg font-medium text-white">{selectedWorkspace.name}</h3>
                    <div className="flex items-center gap-2 mt-0.5">
                      <span className="text-xs text-white/40 font-mono">
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
                        {selectedWorkspace.status}
                      </span>
                    </div>
                  </div>
                </div>
                <button
                  onClick={() => setSelectedWorkspace(null)}
                  className="p-2 -mr-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06] transition-colors"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>

              {/* Tabs */}
              <div className="mt-4 flex items-center gap-1">
                {workspaceTabs.map((tab) => (
                  <button
                    key={tab.id}
                    onClick={() => setWorkspaceTab(tab.id)}
                    className={cn(
                      'px-3.5 py-1.5 text-xs font-medium rounded-lg transition-all',
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
                <div className="px-6 py-5 space-y-5">
                  {/* Quick Info Grid */}
                  <div className="grid grid-cols-2 gap-3">
                    <div className="p-3.5 rounded-xl bg-white/[0.02] border border-white/[0.05]">
                      <p className="text-[10px] text-white/40 uppercase tracking-wider mb-1">Template</p>
                      <p className="text-sm text-white/90 font-medium">
                        {selectedWorkspace.template || 'None'}
                      </p>
                    </div>
                    <div className="p-3.5 rounded-xl bg-white/[0.02] border border-white/[0.05]">
                      <p className="text-[10px] text-white/40 uppercase tracking-wider mb-1">Distribution</p>
                      <p className="text-sm text-white/90 font-medium">
                        {selectedWorkspace.distro || 'Default'}
                      </p>
                    </div>
                  </div>

                  {/* Details Section */}
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                    <div className="px-4 py-3 border-b border-white/[0.05]">
                      <p className="text-xs text-white/50 font-medium">Details</p>
                    </div>
                    <div className="divide-y divide-white/[0.04]">
                      <div className="px-4 py-3 flex items-start justify-between gap-4">
                        <span className="text-xs text-white/40 shrink-0">Path</span>
                        <code className="text-xs text-white/70 font-mono break-all text-right">
                          {selectedWorkspace.path}
                        </code>
                      </div>
                      <div className="px-4 py-3 flex items-center justify-between gap-4">
                        <span className="text-xs text-white/40">ID</span>
                        <code className="text-xs text-white/70 font-mono">
                          {selectedWorkspace.id}
                        </code>
                      </div>
                      <div className="px-4 py-3 flex items-center justify-between">
                        <span className="text-xs text-white/40">Created</span>
                        <span className="text-xs text-white/70">
                          {formatDate(selectedWorkspace.created_at)}
                        </span>
                      </div>
                    </div>
                  </div>

                  {selectedWorkspace.error_message && (
                    <div className="rounded-xl bg-red-500/5 border border-red-500/15 p-4">
                      <div className="flex items-start gap-3">
                        <AlertCircle className="h-4 w-4 text-red-400 shrink-0 mt-0.5" />
                        <p className="text-sm text-red-300">{extractErrorSummary(selectedWorkspace.error_message)}</p>
                      </div>
                    </div>
                  )}

                  {/* Build Environment */}
                  {selectedWorkspace.workspace_type === 'chroot' && (
                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                      <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between">
                        <p className="text-xs text-white/50 font-medium">Build Environment</p>
                        {selectedWorkspace.status === 'building' && (
                          <span className="flex items-center gap-1.5 text-xs text-amber-400">
                            <Loader className="h-3 w-3 animate-spin" />
                            Building...
                          </span>
                        )}
                      </div>
                      <div className="p-4 space-y-4">
                        <div>
                          <label className="text-xs text-white/40 block mb-2">Linux Distribution</label>
                          <select
                            value={selectedDistro}
                            onChange={(e) => setSelectedDistro(e.target.value as ChrootDistro)}
                            disabled={building || selectedWorkspace.status === 'building'}
                            className="w-full px-3 py-2.5 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white focus:outline-none focus:border-indigo-500/50 disabled:opacity-50 appearance-none cursor-pointer"
                            style={{
                              backgroundImage:
                                "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                              backgroundPosition: 'right 0.75rem center',
                              backgroundRepeat: 'no-repeat',
                              backgroundSize: '1.25em 1.25em',
                            }}
                          >
                            {CHROOT_DISTROS.map((distro) => (
                              <option key={distro.value} value={distro.value} className="bg-[#161618]">
                                {distro.label}
                              </option>
                            ))}
                          </select>
                        </div>
                        <div className="flex items-center gap-3">
                          <button
                            onClick={() => handleBuildWorkspace(selectedWorkspace.status === 'ready')}
                            disabled={building || selectedWorkspace.status === 'building'}
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
                          <p className="text-xs text-white/40 flex-1">
                            {selectedWorkspace.status === 'ready'
                              ? 'Destroys container and reruns init script'
                              : 'Creates isolated Linux filesystem'}
                          </p>
                        </div>
                      </div>
                    </div>
                  )}

                  {selectedWorkspace.status === 'building' && (
                    <div className="rounded-xl bg-amber-500/5 border border-amber-500/15 p-4">
                      <div className="flex items-start gap-3">
                        <Loader className="h-4 w-4 text-amber-400 animate-spin shrink-0 mt-0.5" />
                        <div>
                          <p className="text-sm text-amber-300 font-medium">Building isolated environment</p>
                          <p className="text-xs text-white/50 mt-1">
                            This may take several minutes. You can edit settings while it runs.
                          </p>
                        </div>
                      </div>
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
                        <p className="text-xs text-red-400 py-4 text-center">{skillsError}</p>
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
                  {/* Environment Variables */}
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                    <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <FileText className="h-4 w-4 text-indigo-400" />
                        <p className="text-xs text-white/50 font-medium">Environment Variables</p>
                      </div>
                      <button
                        onClick={() =>
                          setEnvRows((rows) => [
                            ...rows,
                            { id: Math.random().toString(36).slice(2), key: '', value: '' },
                          ])
                        }
                        className="text-xs text-indigo-400 hover:text-indigo-300 font-medium"
                      >
                        + Add
                      </button>
                    </div>

                    <div className="p-4">
                      {envRows.length === 0 ? (
                        <div className="py-6 text-center">
                          <p className="text-xs text-white/40">No environment variables</p>
                          <button
                            onClick={() =>
                              setEnvRows([{ id: Math.random().toString(36).slice(2), key: '', value: '' }])
                            }
                            className="text-xs text-indigo-400 hover:text-indigo-300 mt-2"
                          >
                            Add your first variable
                          </button>
                        </div>
                      ) : (
                        <div className="space-y-2">
                          {envRows.map((row) => (
                            <div key={row.id} className="flex items-center gap-2">
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
                                className="flex-1 px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 font-mono focus:outline-none focus:border-indigo-500/50"
                              />
                              <span className="text-white/20">=</span>
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
                                className="flex-1 px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 font-mono focus:outline-none focus:border-indigo-500/50"
                              />
                              <button
                                onClick={() => setEnvRows((rows) => rows.filter((r) => r.id !== row.id))}
                                className="p-2 text-white/30 hover:text-red-400 hover:bg-red-500/10 rounded-lg transition-colors"
                              >
                                <X className="h-3.5 w-3.5" />
                              </button>
                            </div>
                          ))}
                        </div>
                      )}
                      {envRows.length > 0 && (
                        <p className="text-xs text-white/35 mt-4 pt-3 border-t border-white/[0.04]">
                          Injected into workspace shells and MCP tool runs.
                        </p>
                      )}
                    </div>
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
                        language="bash"
                        placeholder="#!/usr/bin/env bash&#10;# Install packages or setup files here"
                        className="min-h-[220px]"
                        minHeight={220}
                      />
                      <p className="text-xs text-white/35 mt-3">
                        Runs during build. Changes require rebuild to take effect.
                      </p>
                    </div>
                  </div>
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
                  <p className="text-xs text-red-400 mt-1.5">{templatesError}</p>
                )}
              </div>

              <div>
                <label className="text-xs text-white/40 mb-2 block">Type</label>
                <select
                  value={newWorkspaceType}
                  onChange={(e) => setNewWorkspaceType(e.target.value as 'host' | 'chroot')}
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
                  <option value="chroot" className="bg-[#161618]">Isolated (root filesystem)</option>
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
