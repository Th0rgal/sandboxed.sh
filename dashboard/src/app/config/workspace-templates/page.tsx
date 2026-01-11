'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  listWorkspaceTemplates,
  getWorkspaceTemplate,
  saveWorkspaceTemplate,
  deleteWorkspaceTemplate,
  renameWorkspaceTemplate,
  listLibrarySkills,
  CHROOT_DISTROS,
  type WorkspaceTemplate,
  type WorkspaceTemplateSummary,
  type SkillSummary,
} from '@/lib/api';
import {
  GitBranch,
  RefreshCw,
  Check,
  AlertCircle,
  Loader,
  Plus,
  Save,
  Trash2,
  X,
  LayoutTemplate,
  Sparkles,
  FileText,
  Terminal,
  Upload,
  Pencil,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';
import Editor from 'react-simple-code-editor';
import { highlight, languages } from 'prismjs';
import 'prismjs/components/prism-bash';

type TemplateTab = 'overview' | 'skills' | 'environment' | 'init';

const templateTabs: { id: TemplateTab; label: string }[] = [
  { id: 'overview', label: 'Overview' },
  { id: 'skills', label: 'Skills' },
  { id: 'environment', label: 'Environment' },
  { id: 'init', label: 'Init Script' },
];

type EnvRow = { id: string; key: string; value: string };

const toEnvRows = (env: Record<string, string>): EnvRow[] =>
  Object.entries(env).map(([key, value]) => ({
    id: `${key}-${Math.random().toString(36).slice(2, 8)}`,
    key,
    value,
  }));

const envRowsToMap = (rows: EnvRow[]) => {
  const env: Record<string, string> = {};
  rows.forEach((row) => {
    const key = row.key.trim();
    if (!key) return;
    env[key] = row.value;
  });
  return env;
};

const buildSnapshot = (data: {
  description: string;
  distro: string;
  skills: string[];
  envRows: EnvRow[];
  initScript: string;
}) =>
  JSON.stringify({
    description: data.description,
    distro: data.distro,
    skills: data.skills,
    env: data.envRows.map((row) => ({ key: row.key, value: row.value })),
    initScript: data.initScript,
  });

export default function WorkspaceTemplatesPage() {
  const {
    status,
    loading,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    sync,
    commit,
    push,
    syncing,
    committing,
    pushing,
  } = useLibrary();

  const [templates, setTemplates] = useState<WorkspaceTemplateSummary[]>([]);
  const [templatesError, setTemplatesError] = useState<string | null>(null);
  const [loadingTemplates, setLoadingTemplates] = useState(false);

  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [skillsError, setSkillsError] = useState<string | null>(null);
  const [skillsFilter, setSkillsFilter] = useState('');
  const [templateFilter, setTemplateFilter] = useState('');

  const [selectedTemplate, setSelectedTemplate] = useState<WorkspaceTemplate | null>(null);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<TemplateTab>('overview');

  const [description, setDescription] = useState('');
  const [distro, setDistro] = useState<string>('');
  const [selectedSkills, setSelectedSkills] = useState<string[]>([]);
  const [envRows, setEnvRows] = useState<EnvRow[]>([]);
  const [initScript, setInitScript] = useState('');
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newTemplateName, setNewTemplateName] = useState('');
  const [newTemplateDescription, setNewTemplateDescription] = useState('');

  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');

  const [showRenameDialog, setShowRenameDialog] = useState(false);
  const [renameTemplateName, setRenameTemplateName] = useState('');
  const [renaming, setRenaming] = useState(false);

  const baselineRef = useRef('');

  const snapshot = useMemo(
    () =>
      buildSnapshot({
        description,
        distro,
        skills: selectedSkills,
        envRows,
        initScript,
      }),
    [description, distro, selectedSkills, envRows, initScript]
  );

  useEffect(() => {
    if (!selectedTemplate) {
      setDirty(false);
      return;
    }
    setDirty(snapshot !== baselineRef.current);
  }, [snapshot, selectedTemplate]);

  // Handle ESC key to close modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showNewDialog) setShowNewDialog(false);
        if (showCommitDialog) setShowCommitDialog(false);
        if (showRenameDialog) setShowRenameDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showNewDialog, showCommitDialog, showRenameDialog]);

  const loadTemplates = useCallback(async () => {
    try {
      setLoadingTemplates(true);
      setTemplatesError(null);
      const data = await listWorkspaceTemplates();
      setTemplates(data);
    } catch (err) {
      setTemplates([]);
      setTemplatesError(err instanceof Error ? err.message : 'Failed to load templates');
    } finally {
      setLoadingTemplates(false);
    }
  }, []);

  const loadSkills = useCallback(async () => {
    try {
      setSkillsError(null);
      const data = await listLibrarySkills();
      setSkills(data);
    } catch (err) {
      setSkills([]);
      setSkillsError(err instanceof Error ? err.message : 'Failed to load skills');
    }
  }, []);

  useEffect(() => {
    if (libraryUnavailable || loading) return;
    loadTemplates();
    loadSkills();
  }, [libraryUnavailable, loading, loadTemplates, loadSkills]);

  const loadTemplate = useCallback(async (name: string) => {
    try {
      const template = await getWorkspaceTemplate(name);
      setSelectedTemplate(template);
      setSelectedName(name);
      setActiveTab('overview');
      setDescription(template.description || '');
      setDistro(template.distro || '');
      setSelectedSkills(template.skills || []);
      setEnvRows(toEnvRows(template.env_vars || {}));
      setInitScript(template.init_script || '');
      baselineRef.current = buildSnapshot({
        description: template.description || '',
        distro: template.distro || '',
        skills: template.skills || [],
        envRows: toEnvRows(template.env_vars || {}),
        initScript: template.init_script || '',
      });
      setDirty(false);
    } catch (err) {
      console.error('Failed to load template:', err);
    }
  }, []);

  const handleSave = async () => {
    if (!selectedName) return;
    setSaving(true);
    try {
      await saveWorkspaceTemplate(selectedName, {
        description: description.trim() || undefined,
        distro: distro || undefined,
        skills: selectedSkills,
        env_vars: envRowsToMap(envRows),
        init_script: initScript,
      });
      baselineRef.current = snapshot;
      setDirty(false);
      await loadTemplates();
    } catch (err) {
      console.error('Failed to save template:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreate = async () => {
    const name = newTemplateName.trim();
    if (!name) return;
    setSaving(true);
    try {
      await saveWorkspaceTemplate(name, {
        description: newTemplateDescription.trim() || undefined,
        skills: [],
        env_vars: {},
        init_script: '',
      });
      setShowNewDialog(false);
      setNewTemplateName('');
      setNewTemplateDescription('');
      await loadTemplates();
      await loadTemplate(name);
    } catch (err) {
      console.error('Failed to create template:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!selectedName) return;
    if (!confirm(`Delete template "${selectedName}"?`)) return;
    setSaving(true);
    try {
      await deleteWorkspaceTemplate(selectedName);
      setSelectedTemplate(null);
      setSelectedName(null);
      setDescription('');
      setDistro('');
      setSelectedSkills([]);
      setEnvRows([]);
      setInitScript('');
      setDirty(false);
      await loadTemplates();
    } catch (err) {
      console.error('Failed to delete template:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleRename = async () => {
    const newName = renameTemplateName.trim();
    if (!selectedName || !newName || newName === selectedName) return;
    setRenaming(true);
    try {
      await renameWorkspaceTemplate(selectedName, newName);
      setShowRenameDialog(false);
      setRenameTemplateName('');
      await loadTemplates();
      await loadTemplate(newName);
    } catch (err) {
      console.error('Failed to rename template:', err);
    } finally {
      setRenaming(false);
    }
  };

  const handleSync = async () => {
    try {
      await sync();
      await loadTemplates();
      await loadSkills();
    } catch {
      // Error handled in context
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } catch {
      // Error handled in context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error handled in context
    }
  };

  const filteredTemplates = useMemo(() => {
    const term = templateFilter.trim().toLowerCase();
    if (!term) return templates;
    return templates.filter((template) => template.name.toLowerCase().includes(term));
  }, [templates, templateFilter]);

  const filteredSkills = useMemo(() => {
    const term = skillsFilter.trim().toLowerCase();
    if (!term) return skills;
    return skills.filter((skill) => skill.name.toLowerCase().includes(term));
  }, [skills, skillsFilter]);

  const toggleSkill = (name: string) => {
    setSelectedSkills((prev) =>
      prev.includes(name) ? prev.filter((s) => s !== name) : [...prev, name]
    );
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  if (libraryUnavailable) {
    return (
      <div className="min-h-screen p-6">
        <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />
      </div>
    );
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-7xl mx-auto space-y-4">
      {/* Git Status Bar */}
      {status && (
        <div className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.06]">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <div className="flex items-center gap-2">
                <GitBranch className="h-4 w-4 text-white/40" />
                <span className="text-sm font-medium text-white">{status.branch}</span>
              </div>
              <div className="flex items-center gap-2">
                {status.clean ? (
                  <span className="flex items-center gap-1 text-xs text-emerald-400">
                    <Check className="h-3 w-3" />
                    Clean
                  </span>
                ) : (
                  <span className="flex items-center gap-1 text-xs text-amber-400">
                    <AlertCircle className="h-3 w-3" />
                    {status.modified_files.length} modified
                  </span>
                )}
              </div>
              {(status.ahead > 0 || status.behind > 0) && (
                <div className="text-xs text-white/40">
                  {status.ahead > 0 && (
                    <span className="text-emerald-400">+{status.ahead}</span>
                  )}
                  {status.ahead > 0 && status.behind > 0 && ' / '}
                  {status.behind > 0 && (
                    <span className="text-amber-400">-{status.behind}</span>
                  )}
                </div>
              )}
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={handleSync}
                disabled={syncing}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
              >
                <RefreshCw className={cn('h-3 w-3', syncing && 'animate-spin')} />
                Sync
              </button>
              {!status.clean && (
                <button
                  onClick={() => setShowCommitDialog(true)}
                  disabled={committing}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                >
                  <Save className="h-3 w-3" />
                  Commit
                </button>
              )}
              <button
                onClick={handlePush}
                disabled={pushing || status.ahead === 0}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
              >
                <Upload className="h-3 w-3" />
                Push
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="grid grid-cols-12 gap-4 flex-1">
        {/* Template List */}
        <div className="col-span-4 rounded-xl bg-white/[0.02] border border-white/[0.06] flex flex-col min-h-[560px]">
          <div className="px-4 py-3 border-b border-white/[0.06] flex items-center justify-between">
            <div className="flex items-center gap-2">
              <LayoutTemplate className="h-4 w-4 text-indigo-400" />
              <p className="text-xs text-white/60 font-medium">Workspaces</p>
            </div>
            <button
              onClick={() => setShowNewDialog(true)}
              className="text-xs text-indigo-400 hover:text-indigo-300 font-medium flex items-center gap-1"
            >
              <Plus className="h-3.5 w-3.5" />
              New
            </button>
          </div>

          <div className="p-3">
            <input
              value={templateFilter}
              onChange={(e) => setTemplateFilter(e.target.value)}
              placeholder="Search templates..."
              className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
            />
          </div>

          <div className="flex-1 overflow-y-auto px-2 pb-3">
            {loadingTemplates ? (
              <div className="flex items-center justify-center py-8">
                <Loader className="h-4 w-4 animate-spin text-white/40" />
              </div>
            ) : templatesError ? (
              <p className="text-xs text-red-400 px-3 py-4 text-center">{templatesError}</p>
            ) : filteredTemplates.length === 0 ? (
              <div className="py-8 text-center">
                <LayoutTemplate className="h-8 w-8 text-white/10 mx-auto mb-2" />
                <p className="text-xs text-white/40">No templates found</p>
              </div>
            ) : (
              <div className="space-y-1.5">
                {filteredTemplates.map((template) => {
                  const isActive = selectedName === template.name;
                  return (
                    <button
                      key={template.name}
                      onClick={() => loadTemplate(template.name)}
                      className={cn(
                        'w-full text-left px-3 py-2.5 rounded-lg border transition-all',
                        isActive
                          ? 'bg-indigo-500/10 border-indigo-500/25 text-white'
                          : 'bg-black/10 border-white/[0.04] text-white/70 hover:bg-black/20 hover:border-white/[0.08]'
                      )}
                    >
                      <div className="flex items-center justify-between gap-2">
                        <span className="text-xs font-medium">{template.name}</span>
                        {isActive && dirty && (
                          <span className="text-[10px] text-amber-300">Unsaved</span>
                        )}
                      </div>
                      {template.description && (
                        <p className="mt-1 text-[11px] text-white/40 line-clamp-1">
                          {template.description}
                        </p>
                      )}
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </div>

        {/* Editor */}
        <div className="col-span-8 rounded-xl bg-white/[0.02] border border-white/[0.06] flex flex-col min-h-[560px]">
          <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
            <div>
              <p className="text-sm font-medium text-white">Workspace</p>
              <p className="text-xs text-white/40">
                {selectedName ? selectedName : 'Select a template to edit'}
              </p>
            </div>
            <div className="flex items-center gap-2">
              {selectedName && (
                <>
                  <button
                    onClick={() => {
                      setRenameTemplateName(selectedName);
                      setShowRenameDialog(true);
                    }}
                    disabled={saving || renaming}
                    className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                  >
                    <Pencil className="h-3.5 w-3.5" />
                    Rename
                  </button>
                  <button
                    onClick={handleDelete}
                    disabled={saving}
                    className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    Delete
                  </button>
                </>
              )}
              <button
                onClick={handleSave}
                disabled={!selectedName || saving || !dirty}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors disabled:opacity-50"
              >
                {saving ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                Save
              </button>
            </div>
          </div>

          {selectedName ? (
            <>
              <div className="px-5 pt-4">
                <div className="flex items-center gap-1">
                  {templateTabs.map((tab) => (
                    <button
                      key={tab.id}
                      onClick={() => setActiveTab(tab.id)}
                      className={cn(
                        'px-3.5 py-1.5 text-xs font-medium rounded-lg transition-all',
                        activeTab === tab.id
                          ? 'bg-white/[0.08] text-white'
                          : 'text-white/50 hover:text-white/80 hover:bg-white/[0.04]'
                      )}
                    >
                      {tab.label}
                    </button>
                  ))}
                </div>
              </div>

              <div className={cn(
                "flex-1 min-h-0 overflow-y-auto p-5",
                activeTab === 'init' ? "flex flex-col" : "space-y-4"
              )}>
                {activeTab === 'overview' && (
                  <div className="space-y-4">
                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] p-4">
                      <label className="text-xs text-white/40 block mb-2">Description</label>
                      <textarea
                        value={description}
                        onChange={(e) => setDescription(e.target.value)}
                        rows={3}
                        placeholder="Short description for this template"
                        className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50 resize-none"
                      />
                    </div>

                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] p-4">
                      <label className="text-xs text-white/40 block mb-2">Linux Distribution</label>
                      <select
                        value={distro}
                        onChange={(e) => setDistro(e.target.value)}
                        className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                        style={{
                          backgroundImage:
                            "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                          backgroundPosition: 'right 0.75rem center',
                          backgroundRepeat: 'no-repeat',
                          backgroundSize: '1.25em 1.25em',
                        }}
                      >
                        <option value="">Default (Workspace setting)</option>
                        {CHROOT_DISTROS.map((option) => (
                          <option key={option.value} value={option.value}>
                            {option.label}
                          </option>
                        ))}
                      </select>
                    </div>
                  </div>
                )}

                {activeTab === 'skills' && (
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden">
                    <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <Sparkles className="h-4 w-4 text-indigo-400" />
                        <p className="text-xs text-white/50 font-medium">Skills</p>
                      </div>
                      <span className="text-xs text-white/40">{selectedSkills.length} enabled</span>
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
                      ) : skills.length === 0 ? (
                        <div className="py-8 text-center">
                          <Sparkles className="h-8 w-8 text-white/10 mx-auto mb-2" />
                          <p className="text-xs text-white/40">No skills in library</p>
                        </div>
                      ) : (
                        <div className="max-h-72 overflow-y-auto space-y-1.5">
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
                                  <p className="mt-1 text-[11px] text-white/40 line-clamp-1">
                                    {skill.description}
                                  </p>
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
                        Skills are synced to new workspaces created from this template.
                      </p>
                    </div>
                  </div>
                )}

                {activeTab === 'environment' && (
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
                    <div className="p-4 space-y-3">
                      {envRows.length === 0 ? (
                        <div className="py-6 text-center">
                          <p className="text-xs text-white/40">No environment variables</p>
                          <button
                            onClick={() =>
                              setEnvRows([{ id: Math.random().toString(36).slice(2), key: '', value: '' }])
                            }
                            className="mt-3 text-xs text-indigo-400 hover:text-indigo-300"
                          >
                            Add first variable
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
                                    rows.map((r) => (r.id === row.id ? { ...r, key: e.target.value } : r))
                                  )
                                }
                                placeholder="KEY"
                                className="flex-1 px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                              />
                              <input
                                value={row.value}
                                onChange={(e) =>
                                  setEnvRows((rows) =>
                                    rows.map((r) => (r.id === row.id ? { ...r, value: e.target.value } : r))
                                  )
                                }
                                placeholder="value"
                                className="flex-1 px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                              />
                              <button
                                onClick={() => setEnvRows((rows) => rows.filter((r) => r.id !== row.id))}
                                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06] transition-colors"
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
                )}

                {activeTab === 'init' && (
                  <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden flex flex-col flex-1">
                    <div className="px-4 py-3 border-b border-white/[0.05] flex items-center gap-2 flex-shrink-0">
                      <Terminal className="h-4 w-4 text-indigo-400" />
                      <p className="text-xs text-white/50 font-medium">Init Script</p>
                    </div>
                    <div className="p-4 flex flex-col flex-1 min-h-0">
                      <div className="flex-1 min-h-[288px] rounded-lg bg-black/20 border border-white/[0.06] overflow-auto focus-within:border-indigo-500/50 transition-colors">
                        <Editor
                          value={initScript}
                          onValueChange={setInitScript}
                          highlight={(code) => highlight(code, languages.bash, 'bash')}
                          placeholder="#!/usr/bin/env bash&#10;# Install packages or setup files here"
                          padding={12}
                          style={{
                            fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
                            fontSize: 12,
                            lineHeight: 1.6,
                            minHeight: '100%',
                          }}
                          className="init-script-editor"
                          textareaClassName="focus:outline-none"
                        />
                      </div>
                      <p className="text-xs text-white/35 mt-3 flex-shrink-0">
                        Runs during build for workspaces created from this template.
                      </p>
                    </div>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-sm text-white/40">
              Select a template to view or edit.
            </div>
          )}
        </div>
      </div>

      {/* New Template Dialog */}
      {showNewDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-white">New Template</p>
                <p className="text-xs text-white/40">Create a reusable workspace template.</p>
              </div>
              <button
                onClick={() => setShowNewDialog(false)}
                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06]"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-5 space-y-4">
              <div>
                <label className="text-xs text-white/40 block mb-2">Template Name</label>
                <input
                  value={newTemplateName}
                  onChange={(e) =>
                    setNewTemplateName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))
                  }
                  placeholder="my-template"
                  className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="text-xs text-white/40 block mb-2">Description</label>
                <input
                  value={newTemplateDescription}
                  onChange={(e) => setNewTemplateDescription(e.target.value)}
                  placeholder="Short description"
                  className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
            </div>
            <div className="px-5 pb-5 flex items-center justify-end gap-2">
              <button
                onClick={() => setShowNewDialog(false)}
                className="px-4 py-2 text-xs text-white/60 hover:text-white/80"
              >
                Cancel
              </button>
              <button
                onClick={handleCreate}
                disabled={!newTemplateName.trim() || saving}
                className="flex items-center gap-2 px-4 py-2 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {saving ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                Create
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-white">Commit Changes</p>
                <p className="text-xs text-white/40">Describe your template changes.</p>
              </div>
              <button
                onClick={() => setShowCommitDialog(false)}
                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06]"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-5">
              <label className="text-xs text-white/40 block mb-2">Commit Message</label>
              <input
                value={commitMessage}
                onChange={(e) => setCommitMessage(e.target.value)}
                placeholder="Update workspace templates"
                className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
              />
            </div>
            <div className="px-5 pb-5 flex items-center justify-end gap-2">
              <button
                onClick={() => setShowCommitDialog(false)}
                className="px-4 py-2 text-xs text-white/60 hover:text-white/80"
              >
                Cancel
              </button>
              <button
                onClick={handleCommit}
                disabled={!commitMessage.trim() || committing}
                className="flex items-center gap-2 px-4 py-2 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {committing ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                Commit
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Rename Template Dialog */}
      {showRenameDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-white">Rename Template</p>
                <p className="text-xs text-white/40">Enter a new name for this template.</p>
              </div>
              <button
                onClick={() => setShowRenameDialog(false)}
                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06]"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-5">
              <label className="text-xs text-white/40 block mb-2">Template Name</label>
              <input
                value={renameTemplateName}
                onChange={(e) =>
                  setRenameTemplateName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))
                }
                placeholder="my-template"
                className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
              />
            </div>
            <div className="px-5 pb-5 flex items-center justify-end gap-2">
              <button
                onClick={() => setShowRenameDialog(false)}
                className="px-4 py-2 text-xs text-white/60 hover:text-white/80"
              >
                Cancel
              </button>
              <button
                onClick={handleRename}
                disabled={!renameTemplateName.trim() || renameTemplateName === selectedName || renaming}
                className="flex items-center gap-2 px-4 py-2 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {renaming ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Pencil className="h-3.5 w-3.5" />}
                Rename
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
