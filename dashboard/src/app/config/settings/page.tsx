'use client';

import { useState, useEffect, useCallback, useRef } from 'react';
import useSWR from 'swr';
import {
  getBackendConfig,
  listConfigProfiles,
  createConfigProfile,
  listConfigProfileFiles,
  getConfigProfileFile,
  saveConfigProfileFile,
  deleteConfigProfileFile,
  getHarnessDefaultFile,
  ConfigProfileSummary,
  DivergedHistoryError,
} from '@/lib/api';
import { Save, Loader, AlertCircle, Check, RefreshCw, X, GitBranch, Upload, Download, GitMerge, ChevronDown, Plus, Layers, FileJson, FolderOpen, Trash2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ConfigCodeEditor } from '@/components/config-code-editor';
import { useLibrary } from '@/contexts/library-context';

// Harness configuration metadata
// Maps harness IDs to their profile directory and library directory
const HARNESS_CONFIG = {
  opencode: {
    name: 'OpenCode',
    dir: '.opencode',           // Directory in config profiles
    libraryDir: 'opencode',     // Directory in library root
    files: [
      { name: 'settings.json', description: 'Main settings (agents, models, providers)', libraryName: 'settings.json' },
      { name: 'oh-my-opencode.json', description: 'oh-my-opencode plugin configuration', libraryName: 'oh-my-opencode.json' },
    ],
  },
  claudecode: {
    name: 'Claude Code',
    dir: '.claudecode',
    libraryDir: 'claudecode',
    files: [
      { name: 'settings.json', description: 'Default model, agent, visibility settings', libraryName: 'config.json' },
    ],
  },
  ampcode: {
    name: 'Amp',
    dir: '.ampcode',
    libraryDir: 'ampcode',
    files: [
      { name: 'settings.json', description: 'Default mode (smart/rush)', libraryName: 'config.json' },
    ],
  },
  openagent: {
    name: 'OpenAgent',
    dir: '.openagent',
    libraryDir: 'openagent',
    files: [
      { name: 'config.json', description: 'Agent visibility and defaults for mission dialog', libraryName: 'config.json' },
    ],
  },
};

// Fallback defaults when library file doesn't exist
const FALLBACK_DEFAULTS: Record<string, Record<string, string>> = {
  opencode: {
    'settings.json': JSON.stringify({ agents: {} }, null, 2),
    'oh-my-opencode.json': JSON.stringify({ agents: {}, categories: {} }, null, 2),
  },
  claudecode: {
    'settings.json': JSON.stringify({ default_model: null, hidden_agents: [] }, null, 2),
  },
  ampcode: {
    'settings.json': JSON.stringify({ default_mode: 'smart' }, null, 2),
  },
  openagent: {
    'config.json': JSON.stringify({ hidden_agents: [], default_agent: null }, null, 2),
  },
};

type HarnessId = keyof typeof HARNESS_CONFIG;

export default function SettingsPage() {
  const {
    status,
    sync,
    forceSync,
    forcePush,
    commit,
    push,
    syncing,
    committing,
    pushing,
    refreshStatus,
    divergedHistory,
    divergedHistoryMessage,
  } = useLibrary();

  // Harness tab state
  const [activeHarness, setActiveHarness] = useState<HarnessId>('opencode');

  // Fetch backend configs to determine which harnesses are enabled
  const { data: opencodeConfig } = useSWR('backend-opencode-config', () => getBackendConfig('opencode'), {
    revalidateOnFocus: false,
  });
  const { data: claudecodeConfig } = useSWR('backend-claudecode-config', () => getBackendConfig('claudecode'), {
    revalidateOnFocus: false,
  });
  const { data: ampConfig } = useSWR('backend-amp-config', () => getBackendConfig('amp'), {
    revalidateOnFocus: false,
  });

  // Filter to only enabled backends
  const enabledHarnesses: HarnessId[] = ['opencode', 'claudecode', 'ampcode', 'openagent'].filter((id) => {
    if (id === 'opencode') return opencodeConfig?.enabled !== false;
    if (id === 'claudecode') return claudecodeConfig?.enabled !== false;
    if (id === 'ampcode') return ampConfig?.enabled !== false;
    return true; // openagent is always enabled
  }) as HarnessId[];

  // Config Profiles
  const { data: profiles = [], mutate: mutateProfiles } = useSWR(
    'config-profiles',
    listConfigProfiles,
    { revalidateOnFocus: false }
  );
  const [selectedProfile, setSelectedProfile] = useState<string>('default');
  const [showProfileDropdown, setShowProfileDropdown] = useState(false);
  const [showNewProfileDialog, setShowNewProfileDialog] = useState(false);
  const [newProfileName, setNewProfileName] = useState('');
  const [creatingProfile, setCreatingProfile] = useState(false);

  // File editing state
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [fileContent, setFileContent] = useState<string>('');
  const [originalFileContent, setOriginalFileContent] = useState<string>('');
  const [isLibraryDefault, setIsLibraryDefault] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);

  // Profile files list
  const [profileFiles, setProfileFiles] = useState<string[]>([]);

  const profileDropdownRef = useRef<HTMLDivElement>(null);
  // Track current load request to prevent race conditions when switching files rapidly
  const currentLoadRequestRef = useRef<number>(0);

  const isDirty = fileContent !== originalFileContent;

  // Load profile files when profile or harness changes
  const loadProfileFiles = useCallback(async () => {
    try {
      const files = await listConfigProfileFiles(selectedProfile);
      setProfileFiles(files);
    } catch {
      setProfileFiles([]);
    }
  }, [selectedProfile]);

  useEffect(() => {
    loadProfileFiles();
  }, [loadProfileFiles]);

  // Load file content
  const loadFile = useCallback(async (filePath: string) => {
    // Increment request ID to track this specific load request
    const requestId = ++currentLoadRequestRef.current;

    // Helper to check if this request is still the current one
    const isStale = () => currentLoadRequestRef.current !== requestId;

    try {
      setLoading(true);
      setError(null);
      setIsLibraryDefault(false);
      const content = await getConfigProfileFile(selectedProfile, filePath);

      // Discard stale responses to prevent race conditions
      if (isStale()) return;

      setFileContent(content);
      setOriginalFileContent(content);
      setSelectedFile(filePath);
    } catch {
      // Discard stale responses
      if (isStale()) return;

      // File doesn't exist in profile, try to load library default
      const harness = Object.entries(HARNESS_CONFIG).find(([, cfg]) =>
        filePath.startsWith(cfg.dir)
      );
      if (harness) {
        const [harnessId, harnessConfig] = harness;
        const fileName = filePath.split('/').pop() || '';
        const fileConfig = harnessConfig.files.find(f => f.name === fileName);
        const libraryFileName = fileConfig?.libraryName || fileName;

        try {
          // Try to fetch from library defaults
          const libraryContent = await getHarnessDefaultFile(harnessConfig.libraryDir, libraryFileName);

          // Discard stale responses
          if (isStale()) return;

          setFileContent(libraryContent);
          setOriginalFileContent(libraryContent);
          setIsLibraryDefault(true);
          setSelectedFile(filePath);
        } catch {
          // Discard stale responses
          if (isStale()) return;

          // Library default doesn't exist either, use fallback
          const fallback = FALLBACK_DEFAULTS[harnessId]?.[fileName] || '{}';
          setFileContent(fallback);
          setOriginalFileContent(''); // Mark as new file
          setIsLibraryDefault(false);
          setSelectedFile(filePath);
        }
      } else {
        setError('Unknown harness for file path');
      }
    } finally {
      // Only clear loading if this is still the current request
      if (!isStale()) {
        setLoading(false);
      }
    }
  }, [selectedProfile]);

  // Auto-select first file when harness changes
  useEffect(() => {
    const harnessConfig = HARNESS_CONFIG[activeHarness];
    if (harnessConfig && harnessConfig.files.length > 0) {
      const filePath = `${harnessConfig.dir}/${harnessConfig.files[0].name}`;
      loadFile(filePath);
    }
  }, [activeHarness, selectedProfile, loadFile]);

  // Validate JSON on change
  useEffect(() => {
    if (!fileContent.trim()) {
      setParseError(null);
      return;
    }
    try {
      JSON.parse(fileContent);
      setParseError(null);
    } catch (err) {
      setParseError(err instanceof Error ? err.message : 'Invalid JSON');
    }
  }, [fileContent]);

  const handleSave = useCallback(async () => {
    if (parseError || !selectedFile) return;

    try {
      setSaving(true);
      setError(null);
      await saveConfigProfileFile(selectedProfile, selectedFile, fileContent);
      setOriginalFileContent(fileContent);
      setIsLibraryDefault(false); // No longer showing library default after save
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2000);
      await refreshStatus();
      await loadProfileFiles();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save file');
    } finally {
      setSaving(false);
    }
  }, [parseError, selectedFile, selectedProfile, fileContent, refreshStatus, loadProfileFiles]);

  const handleDelete = useCallback(async () => {
    if (!selectedFile) return;

    // Confirm deletion
    if (!confirm(`Delete ${selectedFile.split('/').pop()}? This will remove your customizations and revert to library defaults.`)) {
      return;
    }

    try {
      setDeleting(true);
      setError(null);
      await deleteConfigProfileFile(selectedProfile, selectedFile);
      // Reload the file (will now show library default)
      await loadProfileFiles();
      await loadFile(selectedFile);
      await refreshStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete file');
    } finally {
      setDeleting(false);
    }
  }, [selectedFile, selectedProfile, loadProfileFiles, loadFile, refreshStatus]);

  // Handle keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        if (isDirty && !parseError && selectedFile) {
          handleSave();
        }
      }
      if (e.key === 'Escape') {
        if (showProfileDropdown) setShowProfileDropdown(false);
        if (showNewProfileDialog) setShowNewProfileDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isDirty, parseError, selectedFile, showProfileDropdown, showNewProfileDialog, handleSave]);

  // Click outside to close profile dropdown
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (profileDropdownRef.current && !profileDropdownRef.current.contains(e.target as Node)) {
        setShowProfileDropdown(false);
      }
    };
    if (showProfileDropdown) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [showProfileDropdown]);

  const handleCreateProfile = async () => {
    if (!newProfileName.trim()) return;
    try {
      setCreatingProfile(true);
      setError(null);
      // Create empty profile (no base) so it falls back to library defaults
      await createConfigProfile(newProfileName.trim());
      await mutateProfiles();
      setSelectedProfile(newProfileName.trim());
      setNewProfileName('');
      setShowNewProfileDialog(false);
      await loadProfileFiles();
      await refreshStatus();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create profile');
    } finally {
      setCreatingProfile(false);
    }
  };

  const handleProfileChange = async (profileName: string) => {
    setSelectedProfile(profileName);
    setShowProfileDropdown(false);
    // File will be reloaded by useEffect
  };

  const handleReset = () => {
    setFileContent(originalFileContent);
    setParseError(null);
  };

  const handleSync = async () => {
    try {
      await sync();
      await loadProfileFiles();
      if (selectedFile) {
        await loadFile(selectedFile);
      }
    } catch (err) {
      if (err instanceof DivergedHistoryError) {
        // Handled by context
      }
    }
  };

  const handleCommit = async (message: string) => {
    if (!message.trim()) return;
    try {
      await commit(message);
    } catch {
      // Error handled by context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error handled by context
    }
  };

  const harnessConfig = HARNESS_CONFIG[activeHarness];

  if (loading && !fileContent) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="h-screen flex flex-col p-6 gap-4 overflow-hidden">
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
                  {status.ahead > 0 && <span className="text-emerald-400">+{status.ahead}</span>}
                  {status.ahead > 0 && status.behind > 0 && ' / '}
                  {status.behind > 0 && <span className="text-amber-400">-{status.behind}</span>}
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
                  onClick={() => {
                    const message = prompt('Commit message:');
                    if (message) handleCommit(message);
                  }}
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

      {/* Diverged History Warning */}
      {divergedHistory && (
        <div className="p-4 rounded-lg bg-amber-500/10 border border-amber-500/20 flex items-start gap-3">
          <GitMerge className="h-5 w-5 text-amber-400 flex-shrink-0 mt-0.5" />
          <div className="flex-1">
            <p className="text-sm font-medium text-amber-400">Git History Diverged</p>
            <p className="text-sm text-amber-400/80 mt-1">
              {divergedHistoryMessage || 'Local and remote histories have diverged.'}
            </p>
            <div className="flex gap-2 mt-2">
              <button
                onClick={() => forceSync()}
                className="px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/30 rounded-lg transition-colors"
              >
                <Download className="h-3.5 w-3.5 inline mr-1" />
                Force Pull
              </button>
              <button
                onClick={() => forcePush()}
                className="px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/30 rounded-lg transition-colors"
              >
                <Upload className="h-3.5 w-3.5 inline mr-1" />
                Force Push
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Error Display */}
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 flex items-start gap-3">
          <AlertCircle className="h-5 w-5 text-red-400 flex-shrink-0 mt-0.5" />
          <div>
            <p className="text-sm font-medium text-red-400">Error</p>
            <p className="text-sm text-red-400/80">{error}</p>
          </div>
        </div>
      )}

      {/* Harness Tabs and Profile Selector */}
      <div className="flex items-center justify-between mb-2">
        {/* Harness Tabs - Left */}
        <div className="flex items-center gap-2">
          {enabledHarnesses.map((harnessId) => (
            <button
              key={harnessId}
              onClick={() => setActiveHarness(harnessId)}
              className={cn(
                'px-4 py-2 rounded-lg text-sm font-medium border transition-colors',
                activeHarness === harnessId
                  ? 'bg-white/[0.08] border-white/[0.12] text-white'
                  : 'bg-white/[0.02] border-white/[0.06] text-white/50 hover:text-white/70'
              )}
            >
              {HARNESS_CONFIG[harnessId].name}
            </button>
          ))}
        </div>

        {/* Profile Selector - Right */}
        <div className="relative" ref={profileDropdownRef}>
          <button
            onClick={() => setShowProfileDropdown(!showProfileDropdown)}
            className="flex items-center gap-2 px-3 py-2 rounded-lg text-sm font-medium border border-white/[0.08] bg-white/[0.04] hover:bg-white/[0.06] text-white/80 transition-colors"
          >
            <Layers className="h-4 w-4 text-white/50" />
            <span>{selectedProfile}</span>
            <ChevronDown className={cn('h-4 w-4 text-white/40 transition-transform', showProfileDropdown && 'rotate-180')} />
          </button>

          {/* Profile Dropdown */}
          {showProfileDropdown && (
            <div className="absolute right-0 top-full mt-1 w-56 rounded-lg border border-white/[0.08] bg-[#1a1a1f] shadow-xl z-50">
              <div className="p-1">
                {profiles.map((profile) => (
                  <button
                    key={profile.name}
                    onClick={() => handleProfileChange(profile.name)}
                    className={cn(
                      'w-full flex items-center gap-2 px-3 py-2 text-sm rounded-md transition-colors text-left',
                      selectedProfile === profile.name
                        ? 'bg-indigo-500/20 text-indigo-300'
                        : 'text-white/70 hover:bg-white/[0.06]'
                    )}
                  >
                    <Layers className="h-4 w-4 flex-shrink-0" />
                    <span className="flex-1 truncate">{profile.name}</span>
                    {profile.is_default && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/[0.06] text-white/40">default</span>
                    )}
                  </button>
                ))}
              </div>
              <div className="border-t border-white/[0.06] p-1">
                <button
                  onClick={() => {
                    setShowProfileDropdown(false);
                    setShowNewProfileDialog(true);
                  }}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.06] rounded-md transition-colors"
                >
                  <Plus className="h-4 w-4" />
                  <span>New Profile</span>
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* New Profile Dialog */}
      {showNewProfileDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
          <div className="w-full max-w-md mx-4 p-6 rounded-xl bg-[#1a1a1f] border border-white/10 shadow-2xl">
            <div className="flex items-start justify-between mb-4">
              <div className="flex items-center gap-3">
                <div className="p-2 rounded-lg bg-indigo-500/10">
                  <Plus className="h-5 w-5 text-indigo-400" />
                </div>
                <h3 className="text-lg font-semibold text-white">New Config Profile</h3>
              </div>
              <button
                onClick={() => {
                  setShowNewProfileDialog(false);
                  setNewProfileName('');
                }}
                className="p-1 text-white/40 hover:text-white transition-colors"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            <p className="text-sm text-white/60 mb-4">
              Create a new configuration profile. It will start empty and use library defaults until you customize specific files.
            </p>
            <div className="mb-6">
              <label className="text-xs text-white/40 block mb-2">Profile Name</label>
              <input
                value={newProfileName}
                onChange={(e) => setNewProfileName(e.target.value)}
                placeholder="e.g., development, production"
                className="w-full px-3 py-2 rounded-lg bg-black/20 border border-white/[0.06] text-sm text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && newProfileName.trim()) {
                    handleCreateProfile();
                  }
                  if (e.key === 'Escape') {
                    setShowNewProfileDialog(false);
                    setNewProfileName('');
                  }
                }}
                autoFocus
              />
            </div>
            <div className="flex gap-3">
              <button
                onClick={() => {
                  setShowNewProfileDialog(false);
                  setNewProfileName('');
                }}
                className="flex-1 px-4 py-2 text-sm font-medium text-white/70 bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateProfile}
                disabled={creatingProfile || !newProfileName.trim()}
                className="flex-1 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors disabled:opacity-50 flex items-center justify-center gap-2"
              >
                {creatingProfile ? (
                  <>
                    <Loader className="h-4 w-4 animate-spin" />
                    Creating...
                  </>
                ) : (
                  <>
                    <Plus className="h-4 w-4" />
                    Create Profile
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Main Content: File Browser + Editor */}
      <div className="flex gap-4 flex-1 min-h-0">
        {/* File Browser Sidebar */}
        <div className="w-64 flex-shrink-0 rounded-xl bg-white/[0.02] border border-white/[0.06] p-4 flex flex-col">
          <div className="flex items-center gap-2 mb-4">
            <FolderOpen className="h-4 w-4 text-white/50" />
            <span className="text-sm font-medium text-white">{harnessConfig.dir}/</span>
          </div>
          <div className="space-y-1 flex-1 overflow-y-auto">
            {/* Show all files that exist in the profile for this harness */}
            {profileFiles
              .filter((file) => file.startsWith(harnessConfig.dir))
              .map((filePath) => {
                const fileName = filePath.split('/').pop() || '';
                const isSelected = selectedFile === filePath;
                const fileConfig = harnessConfig.files.find(f => f.name === fileName);
                return (
                  <button
                    key={filePath}
                    onClick={() => loadFile(filePath)}
                    className={cn(
                      'w-full flex items-start gap-2 px-3 py-2 text-sm rounded-lg transition-colors text-left',
                      isSelected
                        ? 'bg-indigo-500/20 text-indigo-300'
                        : 'text-white/70 hover:bg-white/[0.06]'
                    )}
                  >
                    <FileJson className="h-4 w-4 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <div className="truncate">{fileName}</div>
                      {fileConfig && (
                        <div className="text-[10px] text-white/40 truncate">{fileConfig.description}</div>
                      )}
                    </div>
                  </button>
                );
              })}
            {/* Show predefined files that don't exist yet */}
            {harnessConfig.files
              .filter((file) => {
                const filePath = `${harnessConfig.dir}/${file.name}`;
                return !profileFiles.includes(filePath);
              })
              .map((file) => {
                const filePath = `${harnessConfig.dir}/${file.name}`;
                const isSelected = selectedFile === filePath;
                return (
                  <button
                    key={filePath}
                    onClick={() => loadFile(filePath)}
                    className={cn(
                      'w-full flex items-start gap-2 px-3 py-2 text-sm rounded-lg transition-colors text-left',
                      isSelected
                        ? 'bg-indigo-500/20 text-indigo-300'
                        : 'text-white/70 hover:bg-white/[0.06]'
                    )}
                  >
                    <FileJson className="h-4 w-4 flex-shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <div className="truncate">{file.name}</div>
                      <div className="text-[10px] text-white/40 truncate">{file.description}</div>
                      <div className="text-[10px] text-sky-400/60 mt-1">Using library default</div>
                    </div>
                  </button>
                );
              })}
          </div>
        </div>

        {/* Editor */}
        <div className="flex-1 flex flex-col min-h-0">
          {/* Editor Header */}
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-3">
              <h2 className="text-lg font-medium text-white">
                {selectedFile ? selectedFile.split('/').pop() : 'Select a file'}
                {isLibraryDefault && !isDirty && (
                  <span className="text-sky-400 text-sm font-normal ml-2">(library default)</span>
                )}
                {isLibraryDefault && isDirty && (
                  <span className="text-amber-400 text-sm font-normal ml-2">(modified from library)</span>
                )}
                {!isLibraryDefault && isDirty && (
                  <span className="text-amber-400 text-sm font-normal ml-2">(unsaved)</span>
                )}
              </h2>
              {parseError && (
                <span className="text-red-400 text-xs flex items-center gap-1">
                  <AlertCircle className="h-3 w-3" />
                  {parseError}
                </span>
              )}
            </div>
            <div className="flex items-center gap-2">
              {selectedFile && profileFiles.includes(selectedFile) && (
                <button
                  onClick={handleDelete}
                  disabled={deleting}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-red-400/70 hover:text-red-400 hover:bg-red-500/10 rounded-lg transition-colors disabled:opacity-50"
                  title="Delete customizations and revert to library default"
                >
                  {deleting ? (
                    <Loader className="h-4 w-4 animate-spin" />
                  ) : (
                    <Trash2 className="h-4 w-4" />
                  )}
                  Delete
                </button>
              )}
              {isDirty && (
                <button
                  onClick={handleReset}
                  className="px-3 py-1.5 text-sm text-white/60 hover:text-white transition-colors"
                >
                  Reset
                </button>
              )}
              <button
                onClick={handleSave}
                disabled={saving || !isDirty || !!parseError || !selectedFile}
                className={cn(
                  'flex items-center gap-2 px-4 py-1.5 text-sm font-medium rounded-lg transition-colors',
                  isDirty && !parseError
                    ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                    : 'text-white/40 bg-white/[0.04] cursor-not-allowed'
                )}
              >
                {saving ? (
                  <Loader className="h-4 w-4 animate-spin" />
                ) : saveSuccess ? (
                  <Check className="h-4 w-4 text-emerald-400" />
                ) : (
                  <Save className="h-4 w-4" />
                )}
                {saving ? 'Saving...' : saveSuccess ? 'Saved!' : 'Save'}
              </button>
            </div>
          </div>

          {/* Editor - fills remaining space */}
          <div className="flex-1 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
            <ConfigCodeEditor
              value={fileContent}
              onChange={setFileContent}
              placeholder='{\n  "key": "value"\n}'
              disabled={saving || !selectedFile}
              className="h-full"
              padding={16}
              language="json"
            />
          </div>
        </div>
      </div>
    </div>
  );
}
