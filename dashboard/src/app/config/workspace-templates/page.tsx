'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  listWorkspaceTemplates,
  getWorkspaceTemplate,
  saveWorkspaceTemplate,
  deleteWorkspaceTemplate,
  renameWorkspaceTemplate,
  listLibrarySkills,
  listInitScripts,
  getInitScript,
  saveInitScript,
  deleteInitScript,
  listConfigProfiles,
  CONTAINER_DISTROS,
  type WorkspaceTemplate,
  type WorkspaceTemplateSummary,
  type SkillSummary,
  type InitScriptSummary,
  type InitScript,
  type ConfigProfileSummary,
  type TailscaleMode,
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
  Terminal,
  Upload,
  Pencil,
  ChevronUp,
  ChevronDown,
  FileCode,
  Layers,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';
import { EnvVarsEditor, type EnvRow, toEnvRows, envRowsToMap, getEncryptedKeys } from '@/components/env-vars-editor';
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

const buildSnapshot = (data: {
  description: string;
  distro: string;
  skills: string[];
  envRows: EnvRow[];
  initScripts: string[];
  initScript: string;
  sharedNetwork: boolean | null;
  tailscaleMode: TailscaleMode | null;
  configProfile: string | null;
}) =>
  JSON.stringify({
    description: data.description,
    distro: data.distro,
    skills: data.skills,
    env: data.envRows.map((row) => ({ key: row.key, value: row.value, encrypted: row.encrypted })),
    initScripts: data.initScripts,
    initScript: data.initScript,
    sharedNetwork: data.sharedNetwork,
    tailscaleMode: data.tailscaleMode,
    configProfile: data.configProfile,
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

  const [initScriptFragments, setInitScriptFragments] = useState<InitScriptSummary[]>([]);
  const [initScriptFragmentsError, setInitScriptFragmentsError] = useState<string | null>(null);
  const [initScriptFragmentsFilter, setInitScriptFragmentsFilter] = useState('');

  const [selectedTemplate, setSelectedTemplate] = useState<WorkspaceTemplate | null>(null);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<TemplateTab>('overview');

  const [description, setDescription] = useState('');
  const [distro, setDistro] = useState<string>('');
  const [selectedSkills, setSelectedSkills] = useState<string[]>([]);
  const [envRows, setEnvRows] = useState<EnvRow[]>([]);
  const [selectedInitScripts, setSelectedInitScripts] = useState<string[]>([]);
  const [initScript, setInitScript] = useState('');
  const [sharedNetwork, setSharedNetwork] = useState<boolean | null>(null);
  const [tailscaleMode, setTailscaleMode] = useState<TailscaleMode | null>(null);
  const [configProfile, setConfigProfile] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);

  // Config profiles state
  const [configProfiles, setConfigProfiles] = useState<ConfigProfileSummary[]>([]);

  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newTemplateName, setNewTemplateName] = useState('');
  const [newTemplateDescription, setNewTemplateDescription] = useState('');

  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');

  const [showRenameDialog, setShowRenameDialog] = useState(false);
  const [renameTemplateName, setRenameTemplateName] = useState('');
  const [renaming, setRenaming] = useState(false);

  // Script Fragment editing state
  const [selectedFragment, setSelectedFragment] = useState<InitScript | null>(null);
  const [selectedFragmentName, setSelectedFragmentName] = useState<string | null>(null);
  const [fragmentContent, setFragmentContent] = useState('');
  const [originalFragmentContent, setOriginalFragmentContent] = useState('');
  const [fragmentDirty, setFragmentDirty] = useState(false);
  const [fragmentFilter, setFragmentFilter] = useState('');
  const [showNewFragmentDialog, setShowNewFragmentDialog] = useState(false);
  const [newFragmentName, setNewFragmentName] = useState('');
  const [newFragmentError, setNewFragmentError] = useState<string | null>(null);

  const baselineRef = useRef('');

  const snapshot = useMemo(
    () =>
      buildSnapshot({
        description,
        distro,
        skills: selectedSkills,
        envRows,
        initScripts: selectedInitScripts,
        initScript,
        sharedNetwork,
        tailscaleMode,
        configProfile,
      }),
    [description, distro, selectedSkills, envRows, selectedInitScripts, initScript, sharedNetwork, tailscaleMode, configProfile]
  );

  useEffect(() => {
    if (!selectedTemplate) {
      setDirty(false);
      return;
    }
    setDirty(snapshot !== baselineRef.current);
  }, [snapshot, selectedTemplate]);

  // Track fragment dirty state
  useEffect(() => {
    setFragmentDirty(fragmentContent !== originalFragmentContent);
  }, [fragmentContent, originalFragmentContent]);

  // Handle ESC key to close modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showNewDialog) setShowNewDialog(false);
        if (showCommitDialog) setShowCommitDialog(false);
        if (showRenameDialog) setShowRenameDialog(false);
        if (showNewFragmentDialog) setShowNewFragmentDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showNewDialog, showCommitDialog, showRenameDialog, showNewFragmentDialog]);

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

  const loadInitScriptFragments = useCallback(async () => {
    try {
      setInitScriptFragmentsError(null);
      const data = await listInitScripts();
      setInitScriptFragments(data);
    } catch (err) {
      setInitScriptFragments([]);
      setInitScriptFragmentsError(err instanceof Error ? err.message : 'Failed to load init scripts');
    }
  }, []);

  const loadConfigProfiles = useCallback(async () => {
    try {
      const data = await listConfigProfiles();
      setConfigProfiles(data);
    } catch (err) {
      setConfigProfiles([]);
      console.error('Failed to load config profiles:', err);
    }
  }, []);

  useEffect(() => {
    if (libraryUnavailable || loading) return;
    loadTemplates();
    loadSkills();
    loadInitScriptFragments();
    loadConfigProfiles();
  }, [libraryUnavailable, loading, loadTemplates, loadSkills, loadInitScriptFragments, loadConfigProfiles]);

  const loadTemplate = useCallback(async (name: string) => {
    try {
      // Clear fragment selection when selecting a template
      setSelectedFragment(null);
      setSelectedFragmentName(null);
      setFragmentContent('');
      setOriginalFragmentContent('');
      setFragmentDirty(false);

      const template = await getWorkspaceTemplate(name);
      setSelectedTemplate(template);
      setSelectedName(name);
      setActiveTab('overview');
      setDescription(template.description || '');
      setDistro(template.distro || '');
      setSelectedSkills(template.skills || []);
      const rows = toEnvRows(template.env_vars || {}, template.encrypted_keys);
      setEnvRows(rows);
      setSelectedInitScripts(template.init_scripts || []);
      setInitScript(template.init_script || '');
      setSharedNetwork(template.shared_network ?? null);
      setTailscaleMode(template.tailscale_mode ?? null);
      setConfigProfile(template.config_profile ?? null);
      baselineRef.current = buildSnapshot({
        description: template.description || '',
        distro: template.distro || '',
        skills: template.skills || [],
        envRows: rows,
        initScripts: template.init_scripts || [],
        initScript: template.init_script || '',
        sharedNetwork: template.shared_network ?? null,
        tailscaleMode: template.tailscale_mode ?? null,
        configProfile: template.config_profile ?? null,
      });
      setDirty(false);
    } catch (err) {
      console.error('Failed to load template:', err);
    }
  }, []);

  const loadFragment = useCallback(async (name: string) => {
    try {
      // Clear template selection when selecting a fragment
      setSelectedTemplate(null);
      setSelectedName(null);
      setDirty(false);

      const fragment = await getInitScript(name);
      setSelectedFragment(fragment);
      setSelectedFragmentName(name);
      setFragmentContent(fragment.content);
      setOriginalFragmentContent(fragment.content);
      setFragmentDirty(false);
    } catch (err) {
      console.error('Failed to load fragment:', err);
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
        encrypted_keys: getEncryptedKeys(envRows),
        init_scripts: selectedInitScripts,
        init_script: initScript,
        shared_network: sharedNetwork,
        tailscale_mode: tailscaleMode,
        config_profile: configProfile ?? undefined,
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
      setSelectedInitScripts([]);
      setInitScript('');
      setSharedNetwork(null);
      setTailscaleMode(null);
      setConfigProfile(null);
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

  // Fragment handlers
  const handleSaveFragment = async () => {
    if (!selectedFragmentName) return;
    setSaving(true);
    try {
      await saveInitScript(selectedFragmentName, fragmentContent);
      setOriginalFragmentContent(fragmentContent);
      setFragmentDirty(false);
      await loadInitScriptFragments();
    } catch (err) {
      console.error('Failed to save fragment:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteFragment = async () => {
    if (!selectedFragmentName) return;
    if (!confirm(`Delete script fragment "${selectedFragmentName}"?`)) return;
    setSaving(true);
    try {
      await deleteInitScript(selectedFragmentName);
      setSelectedFragment(null);
      setSelectedFragmentName(null);
      setFragmentContent('');
      setOriginalFragmentContent('');
      setFragmentDirty(false);
      await loadInitScriptFragments();
    } catch (err) {
      console.error('Failed to delete fragment:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreateFragment = async () => {
    if (!newFragmentName.trim()) return;
    if (initScriptFragments.some((f) => f.name === newFragmentName)) {
      setNewFragmentError('A fragment with this name already exists');
      return;
    }
    setSaving(true);
    try {
      const defaultContent = `#!/usr/bin/env bash
# Description: Add a description here

set -e

# Your setup commands here
`;
      await saveInitScript(newFragmentName, defaultContent);
      await loadInitScriptFragments();
      await loadFragment(newFragmentName);
      setShowNewFragmentDialog(false);
      setNewFragmentName('');
      setNewFragmentError(null);
    } catch (err) {
      setNewFragmentError(err instanceof Error ? err.message : 'Failed to create fragment');
    } finally {
      setSaving(false);
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

  const filteredInitScriptFragments = useMemo(() => {
    const term = initScriptFragmentsFilter.trim().toLowerCase();
    if (!term) return initScriptFragments;
    return initScriptFragments.filter((fragment) => fragment.name.toLowerCase().includes(term));
  }, [initScriptFragments, initScriptFragmentsFilter]);

  const filteredFragmentsForList = useMemo(() => {
    const term = fragmentFilter.trim().toLowerCase();
    if (!term) return initScriptFragments;
    return initScriptFragments.filter((fragment) => fragment.name.toLowerCase().includes(term));
  }, [initScriptFragments, fragmentFilter]);

  const toggleSkill = (name: string) => {
    setSelectedSkills((prev) =>
      prev.includes(name) ? prev.filter((s) => s !== name) : [...prev, name]
    );
  };

  const toggleInitScript = (name: string) => {
    setSelectedInitScripts((prev) =>
      prev.includes(name) ? prev.filter((s) => s !== name) : [...prev, name]
    );
  };

  const moveInitScript = (name: string, direction: 'up' | 'down') => {
    setSelectedInitScripts((prev) => {
      const idx = prev.indexOf(name);
      if (idx === -1) return prev;
      const newIdx = direction === 'up' ? idx - 1 : idx + 1;
      if (newIdx < 0 || newIdx >= prev.length) return prev;
      const updated = [...prev];
      [updated[idx], updated[newIdx]] = [updated[newIdx], updated[idx]];
      return updated;
    });
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
    <div className="h-screen flex flex-col overflow-hidden p-6 max-w-7xl mx-auto space-y-4">
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

      <div className="grid grid-cols-12 gap-4 flex-1 min-h-0">
        {/* Left Panel - Templates and Script Fragments */}
        <div className="col-span-4 flex flex-col gap-4 min-h-0">
          {/* Template List */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] flex flex-col min-h-0 flex-1">
            <div className="px-4 py-3 border-b border-white/[0.06] flex items-center justify-between flex-shrink-0">
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

            <div className="p-3 flex-shrink-0">
              <input
                value={templateFilter}
                onChange={(e) => setTemplateFilter(e.target.value)}
                placeholder="Search templates..."
                className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
              />
            </div>

            <div className="flex-1 overflow-y-auto px-2 pb-3 min-h-0">
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

          {/* Script Fragments List */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] flex flex-col min-h-0 flex-1">
            <div className="px-4 py-3 border-b border-white/[0.06] flex items-center justify-between flex-shrink-0">
              <div className="flex items-center gap-2">
                <FileCode className="h-4 w-4 text-emerald-400" />
                <p className="text-xs text-white/60 font-medium">Script Fragments</p>
              </div>
              <button
                onClick={() => setShowNewFragmentDialog(true)}
                className="text-xs text-emerald-400 hover:text-emerald-300 font-medium flex items-center gap-1"
              >
                <Plus className="h-3.5 w-3.5" />
                New
              </button>
            </div>

            <div className="p-3 flex-shrink-0">
              <input
                value={fragmentFilter}
                onChange={(e) => setFragmentFilter(e.target.value)}
                placeholder="Search fragments..."
                className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-emerald-500/50"
              />
            </div>

            <div className="flex-1 overflow-y-auto px-2 pb-3 min-h-0">
              {initScriptFragmentsError ? (
                <p className="text-xs text-red-400 px-3 py-4 text-center">{initScriptFragmentsError}</p>
              ) : filteredFragmentsForList.length === 0 ? (
                <div className="py-8 text-center">
                  <FileCode className="h-8 w-8 text-white/10 mx-auto mb-2" />
                  <p className="text-xs text-white/40">
                    {fragmentFilter ? 'No matching fragments' : 'No script fragments'}
                  </p>
                </div>
              ) : (
                <div className="space-y-1.5">
                  {filteredFragmentsForList.map((fragment) => {
                    const isActive = selectedFragmentName === fragment.name;
                    return (
                      <button
                        key={fragment.name}
                        onClick={() => loadFragment(fragment.name)}
                        className={cn(
                          'w-full text-left px-3 py-2.5 rounded-lg border transition-all',
                          isActive
                            ? 'bg-emerald-500/10 border-emerald-500/25 text-white'
                            : 'bg-black/10 border-white/[0.04] text-white/70 hover:bg-black/20 hover:border-white/[0.08]'
                        )}
                      >
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-xs font-medium">{fragment.name}</span>
                          {isActive && fragmentDirty && (
                            <span className="text-[10px] text-amber-300">Unsaved</span>
                          )}
                        </div>
                        {fragment.description && (
                          <p className="mt-1 text-[11px] text-white/40 line-clamp-1">
                            {fragment.description}
                          </p>
                        )}
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Editor Panel */}
        <div className="col-span-8 rounded-xl bg-white/[0.02] border border-white/[0.06] flex flex-col min-h-0">
          {/* Show Fragment Editor when a fragment is selected */}
          {selectedFragmentName ? (
            <>
              <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
                <div>
                  <p className="text-sm font-medium text-white">Script Fragment</p>
                  <p className="text-xs text-white/40">
                    init-script/{selectedFragmentName}/SCRIPT.sh
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleDeleteFragment}
                    disabled={saving}
                    className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    Delete
                  </button>
                  <button
                    onClick={handleSaveFragment}
                    disabled={saving || !fragmentDirty}
                    className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-emerald-500 hover:bg-emerald-600 rounded-lg transition-colors disabled:opacity-50"
                  >
                    {saving ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                    Save
                  </button>
                </div>
              </div>

              <div className="flex-1 min-h-0 p-4 overflow-hidden">
                <div className="h-full rounded-lg bg-black/20 border border-white/[0.06] overflow-auto focus-within:border-emerald-500/50 transition-colors">
                  <Editor
                    value={fragmentContent}
                    onValueChange={setFragmentContent}
                    highlight={(code) => highlight(code, languages.bash, 'bash')}
                    placeholder="#!/usr/bin/env bash"
                    padding={16}
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
              </div>
            </>
          ) : (
            /* Show Template Editor when a template is selected */
            <>
              <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
                <div>
                  <p className="text-sm font-medium text-white">Workspace</p>
                  <p className="text-xs text-white/40">
                    {selectedName ? selectedName : 'Select a template or fragment to edit'}
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
                "flex-1 min-h-0 p-5",
                activeTab === 'environment' || activeTab === 'init'
                  ? "flex flex-col overflow-hidden"
                  : "overflow-y-auto space-y-4"
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

                    {/* Config Profile Selector */}
                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] p-4">
                      <div className="flex items-center gap-2 mb-2">
                        <Layers className="h-4 w-4 text-indigo-400" />
                        <label className="text-xs text-white/40">Config Profile</label>
                      </div>
                      <p className="text-[10px] text-white/25 mb-3">
                        Configuration profile to use for workspaces created from this template.
                      </p>
                      <select
                        value={configProfile || ''}
                        onChange={(e) => setConfigProfile(e.target.value || null)}
                        className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                        style={{
                          backgroundImage:
                            "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                          backgroundPosition: 'right 0.75rem center',
                          backgroundRepeat: 'no-repeat',
                          backgroundSize: '1.25em 1.25em',
                        }}
                      >
                        <option value="">Default</option>
                        {configProfiles.map((profile) => (
                          <option key={profile.name} value={profile.name}>
                            {profile.name}{profile.is_default ? ' (default)' : ''}
                          </option>
                        ))}
                      </select>
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
                        {CONTAINER_DISTROS.map((option) => (
                          <option key={option.value} value={option.value}>
                            {option.label}
                          </option>
                        ))}
                      </select>
                    </div>

                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] p-4">
                      <div className="flex items-center justify-between">
                        <div>
                          <label className="text-xs text-white/40 block mb-1">Shared Network</label>
                          <p className="text-[10px] text-white/25">
                            Share host network and DNS. Disable for isolated networking (e.g., Tailscale).
                          </p>
                        </div>
                        <button
                          onClick={() => {
                            // Toggle: null (default=true) -> false -> true -> null
                            if (sharedNetwork === null) setSharedNetwork(false);
                            else if (sharedNetwork === false) setSharedNetwork(true);
                            else setSharedNetwork(null);
                          }}
                          className={cn(
                            "relative w-11 h-6 rounded-full transition-colors",
                            sharedNetwork === null
                              ? "bg-white/10" // default (true)
                              : sharedNetwork
                                ? "bg-emerald-500/50"
                                : "bg-red-500/30"
                          )}
                        >
                          <span
                            className={cn(
                              "absolute top-1 w-4 h-4 rounded-full bg-white transition-all",
                              sharedNetwork === null
                                ? "left-6" // default position (on)
                                : sharedNetwork
                                  ? "left-6"
                                  : "left-1"
                            )}
                          />
                        </button>
                      </div>
                      <p className="text-[10px] text-white/30 mt-2">
                        {sharedNetwork === null
                          ? "Default (enabled)"
                          : sharedNetwork
                            ? "Enabled"
                            : "Disabled (isolated)"}
                      </p>

                      {/* Tailscale Mode - only show when shared_network is disabled */}
                      {sharedNetwork === false && (
                        <div className="mt-4 pt-4 border-t border-white/[0.05]">
                          <label className="text-xs text-white/40 block mb-2">Tailscale Mode</label>
                          <p className="text-[10px] text-white/25 mb-3">
                            How to handle networking when Tailscale is configured via TS_AUTHKEY.
                          </p>
                          <select
                            value={tailscaleMode || 'exit_node'}
                            onChange={(e) => setTailscaleMode(e.target.value as TailscaleMode)}
                            className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                            style={{
                              backgroundImage:
                                "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                              backgroundPosition: 'right 0.75rem center',
                              backgroundRepeat: 'no-repeat',
                              backgroundSize: '1.25em 1.25em',
                            }}
                          >
                            <option value="exit_node">Exit Node (route all traffic via Tailscale)</option>
                            <option value="tailnet_only">Tailnet Only (use host internet, Tailscale for device access)</option>
                          </select>
                          <p className="text-[10px] text-white/30 mt-2">
                            {tailscaleMode === 'tailnet_only'
                              ? "Internet via host gateway, Tailscale only for accessing tailnet devices."
                              : "All traffic routed through Tailscale exit node (requires TS_EXIT_NODE)."}
                          </p>
                        </div>
                      )}
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
                  <EnvVarsEditor
                    rows={envRows}
                    onChange={setEnvRows}
                    className="flex-1"
                    description="Injected into workspace shells and MCP tool runs. Sensitive values (keys, tokens, passwords) are encrypted at rest."
                    showEncryptionToggle
                  />
                )}

                {activeTab === 'init' && (
                  <div className="flex flex-col gap-4 flex-1 min-h-0 overflow-y-auto">
                    {/* Init Script Fragments Selector */}
                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden flex-shrink-0">
                      <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between">
                        <div className="flex items-center gap-2">
                          <FileCode className="h-4 w-4 text-indigo-400" />
                          <p className="text-xs text-white/50 font-medium">Init Script Fragments</p>
                        </div>
                        <span className="text-xs text-white/40">{selectedInitScripts.length} selected</span>
                      </div>
                      <div className="p-4">
                        {/* Selected fragments with reorder controls */}
                        {selectedInitScripts.length > 0 && (
                          <div className="mb-3 space-y-1.5">
                            {selectedInitScripts.map((name, idx) => {
                              const fragment = initScriptFragments.find((f) => f.name === name);
                              return (
                                <div
                                  key={name}
                                  className="flex items-center gap-2 px-3 py-2 rounded-lg bg-indigo-500/10 border border-indigo-500/25"
                                >
                                  <span className="text-[10px] text-indigo-300 font-medium w-5">{idx + 1}.</span>
                                  <span className="text-xs text-white flex-1">{name}</span>
                                  {fragment?.description && (
                                    <span className="text-[10px] text-white/40 truncate max-w-[150px]">
                                      {fragment.description}
                                    </span>
                                  )}
                                  <div className="flex items-center gap-0.5">
                                    <button
                                      onClick={() => moveInitScript(name, 'up')}
                                      disabled={idx === 0}
                                      className="p-1 rounded hover:bg-white/10 disabled:opacity-30"
                                    >
                                      <ChevronUp className="h-3 w-3 text-white/60" />
                                    </button>
                                    <button
                                      onClick={() => moveInitScript(name, 'down')}
                                      disabled={idx === selectedInitScripts.length - 1}
                                      className="p-1 rounded hover:bg-white/10 disabled:opacity-30"
                                    >
                                      <ChevronDown className="h-3 w-3 text-white/60" />
                                    </button>
                                    <button
                                      onClick={() => toggleInitScript(name)}
                                      className="p-1 rounded hover:bg-white/10"
                                    >
                                      <X className="h-3 w-3 text-white/60" />
                                    </button>
                                  </div>
                                </div>
                              );
                            })}
                          </div>
                        )}
                        {/* Available fragments */}
                        <input
                          value={initScriptFragmentsFilter}
                          onChange={(e) => setInitScriptFragmentsFilter(e.target.value)}
                          placeholder="Search fragments..."
                          className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-3"
                        />
                        {initScriptFragmentsError ? (
                          <p className="text-xs text-red-400 py-4 text-center">{initScriptFragmentsError}</p>
                        ) : initScriptFragments.length === 0 ? (
                          <div className="py-4 text-center">
                            <FileCode className="h-6 w-6 text-white/10 mx-auto mb-2" />
                            <p className="text-xs text-white/40">No init script fragments in library</p>
                          </div>
                        ) : (
                          <div className="max-h-40 overflow-y-auto space-y-1.5">
                            {filteredInitScriptFragments.filter((f) => !selectedInitScripts.includes(f.name)).map((fragment) => (
                              <button
                                key={fragment.name}
                                onClick={() => toggleInitScript(fragment.name)}
                                className="w-full text-left px-3 py-2 rounded-lg border bg-black/10 border-white/[0.04] text-white/70 hover:bg-black/20 hover:border-white/[0.08] transition-all"
                              >
                                <div className="flex items-center justify-between gap-3">
                                  <span className="text-xs font-medium">{fragment.name}</span>
                                  <Plus className="h-3 w-3 text-white/40" />
                                </div>
                                {fragment.description && (
                                  <p className="mt-1 text-[11px] text-white/40 line-clamp-1">
                                    {fragment.description}
                                  </p>
                                )}
                              </button>
                            ))}
                            {filteredInitScriptFragments.filter((f) => !selectedInitScripts.includes(f.name)).length === 0 && (
                              <p className="text-xs text-white/40 py-2 text-center">
                                {selectedInitScripts.length > 0 ? 'All fragments selected' : 'No matching fragments'}
                              </p>
                            )}
                          </div>
                        )}
                        <p className="text-xs text-white/35 mt-3 pt-3 border-t border-white/[0.04]">
                          Fragments are executed in order during workspace build.
                        </p>
                      </div>
                    </div>

                    {/* Custom Init Script */}
                    <div className="rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden flex flex-col flex-1 min-h-[250px]">
                      <div className="px-4 py-3 border-b border-white/[0.05] flex items-center gap-2 flex-shrink-0">
                        <Terminal className="h-4 w-4 text-indigo-400" />
                        <p className="text-xs text-white/50 font-medium">Custom Init Script</p>
                      </div>
                      <div className="p-4 flex flex-col flex-1 min-h-0">
                        <div className="flex-1 min-h-[150px] rounded-lg bg-black/20 border border-white/[0.06] overflow-auto focus-within:border-indigo-500/50 transition-colors">
                          <Editor
                            value={initScript}
                            onValueChange={setInitScript}
                            highlight={(code) => highlight(code, languages.bash, 'bash')}
                            placeholder="#!/usr/bin/env bash&#10;# Additional setup that runs after fragments"
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
                          Runs after fragments during build.
                        </p>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-sm text-white/40">
              Select a template or fragment to edit
            </div>
          )}
            </>
          )}
        </div>
      </div>

      {/* New Fragment Dialog */}
      {showNewFragmentDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-white">New Script Fragment</p>
                <p className="text-xs text-white/40">Create a reusable init script.</p>
              </div>
              <button
                onClick={() => {
                  setShowNewFragmentDialog(false);
                  setNewFragmentName('');
                  setNewFragmentError(null);
                }}
                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06]"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-5 space-y-4">
              <div>
                <label className="text-xs text-white/40 block mb-2">Fragment Name</label>
                <input
                  value={newFragmentName}
                  onChange={(e) =>
                    setNewFragmentName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))
                  }
                  placeholder="my-fragment"
                  className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-emerald-500/50"
                />
                <p className="text-[10px] text-white/30 mt-1">
                  Lowercase alphanumeric with hyphens
                </p>
              </div>
              {newFragmentError && (
                <p className="text-xs text-red-400">{newFragmentError}</p>
              )}
            </div>
            <div className="px-5 pb-5 flex items-center justify-end gap-2">
              <button
                onClick={() => {
                  setShowNewFragmentDialog(false);
                  setNewFragmentName('');
                  setNewFragmentError(null);
                }}
                className="px-4 py-2 text-xs text-white/60 hover:text-white/80"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateFragment}
                disabled={!newFragmentName.trim() || saving}
                className="flex items-center gap-2 px-4 py-2 text-xs font-medium text-white bg-emerald-500 hover:bg-emerald-600 rounded-lg disabled:opacity-50"
              >
                {saving ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                Create
              </button>
            </div>
          </div>
        </div>
      )}

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
