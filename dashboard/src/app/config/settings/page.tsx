'use client';

import { useState, useEffect, useCallback, useRef } from 'react';
import useSWR from 'swr';
import {
  getLibraryOpenCodeSettings,
  saveLibraryOpenCodeSettings,
  getOpenCodeSettings,
  restartOpenCodeService,
  getOpenAgentConfig,
  saveOpenAgentConfig,
  listOpenCodeAgents,
  OpenAgentConfig,
  listBackends,
  getBackendConfig,
  getClaudeCodeConfig,
  saveClaudeCodeConfig,
  ClaudeCodeConfig,
  listBackendAgents,
  listProviders,
  listConfigProfiles,
  createConfigProfile,
  getLibraryOpenCodeSettingsForProfile,
  saveLibraryOpenCodeSettingsForProfile,
  getOpenAgentConfigForProfile,
  saveOpenAgentConfigForProfile,
  getClaudeCodeConfigForProfile,
  saveClaudeCodeConfigForProfile,
  ConfigProfileSummary,
  DivergedHistoryError,
} from '@/lib/api';
import { Save, Loader, AlertCircle, Check, RefreshCw, RotateCcw, Eye, EyeOff, AlertTriangle, X, GitBranch, Upload, Info, Download, GitMerge, ChevronDown, Plus, Layers } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ConfigCodeEditor } from '@/components/config-code-editor';
import { useLibrary } from '@/contexts/library-context';

// Parse agents from OpenCode response (handles both object and array formats)
function parseAgentNames(agents: unknown): string[] {
  if (typeof agents === 'object' && agents !== null) {
    if (Array.isArray(agents)) {
      return agents.map((a) => (typeof a === 'string' ? a : a?.name || '')).filter(Boolean);
    }
    return Object.keys(agents);
  }
  return [];
}

export default function SettingsPage() {
  const {
    status,
    sync,
    forceSync,
    forcePush,
    clearDivergedHistory,
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
  const [activeHarness, setActiveHarness] = useState<'opencode' | 'claudecode' | 'amp'>('opencode');

  // Fetch backends and their config to show enabled harnesses
  const { data: backends = [] } = useSWR('backends', listBackends, {
    revalidateOnFocus: false,
    fallbackData: [
      { id: 'opencode', name: 'OpenCode' },
      { id: 'claudecode', name: 'Claude Code' },
      { id: 'amp', name: 'Amp' },
    ],
  });
  const { data: opencodeConfig } = useSWR('backend-opencode-config', () => getBackendConfig('opencode'), {
    revalidateOnFocus: false,
  });
  const { data: claudecodeConfig } = useSWR('backend-claudecode-config', () => getBackendConfig('claudecode'), {
    revalidateOnFocus: false,
  });
  const { data: ampConfig } = useSWR('backend-amp-config', () => getBackendConfig('amp'), {
    revalidateOnFocus: false,
  });

  const {
    data: providerCatalogData,
    isLoading: providerCatalogLoading,
    mutate: mutateProviderCatalog,
  } = useSWR('providers-catalog', () => listProviders({ includeAll: true }), {
    revalidateOnFocus: false,
  });

  const providerCatalog = providerCatalogData?.providers ?? [];

  // Filter to only enabled backends
  const enabledBackends = backends.filter((b) => {
    if (b.id === 'opencode') return opencodeConfig?.enabled !== false;
    if (b.id === 'claudecode') return claudecodeConfig?.enabled !== false;
    if (b.id === 'amp') return ampConfig?.enabled !== false;
    return true;
  });

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

  // OpenCode settings state
  const [settings, setSettings] = useState<string>('');
  const [originalSettings, setOriginalSettings] = useState<string>('');
  const [systemSettings, setSystemSettings] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [restartSuccess, setRestartSuccess] = useState(false);
  const [needsRestart, setNeedsRestart] = useState(false);
  const [showRestartModal, setShowRestartModal] = useState(false);

  // OpenAgent config state
  const [openAgentConfig, setOpenAgentConfig] = useState<OpenAgentConfig>({
    hidden_agents: [],
    default_agent: null,
  });
  const [originalOpenAgentConfig, setOriginalOpenAgentConfig] = useState<OpenAgentConfig>({
    hidden_agents: [],
    default_agent: null,
  });
  const [allAgents, setAllAgents] = useState<string[]>([]);
  const [savingOpenAgent, setSavingOpenAgent] = useState(false);
  const [openAgentSaveSuccess, setOpenAgentSaveSuccess] = useState(false);

  // Claude Code config state
  const [claudeCodeConfig, setClaudeCodeConfig] = useState<ClaudeCodeConfig>({
    default_model: null,
    default_agent: null,
    hidden_agents: [],
  });
  const [originalClaudeCodeConfig, setOriginalClaudeCodeConfig] = useState<ClaudeCodeConfig>({
    default_model: null,
    default_agent: null,
    hidden_agents: [],
  });
  const [allClaudeCodeAgents, setAllClaudeCodeAgents] = useState<string[]>([]);
  const [savingClaudeCode, setSavingClaudeCode] = useState(false);
  const [claudeCodeSaveSuccess, setClaudeCodeSaveSuccess] = useState(false);

  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');
  const [showForceSyncDialog, setShowForceSyncDialog] = useState(false);

  // Ref for profile dropdown click-outside handling
  const profileDropdownRef = useRef<HTMLDivElement>(null);

  const isDirty = settings !== originalSettings;
  const isOpenAgentDirty =
    JSON.stringify(openAgentConfig) !== JSON.stringify(originalOpenAgentConfig);
  const isClaudeCodeDirty =
    JSON.stringify(claudeCodeConfig) !== JSON.stringify(originalClaudeCodeConfig);

  const anthropicModels =
    providerCatalog.find((provider) => provider.id === 'anthropic')?.models ?? [];
  const claudeDefaultModel = claudeCodeConfig.default_model || '';
  const claudeDefaultModelKnown =
    claudeDefaultModel.length === 0 ||
    anthropicModels.some((model) => model.id === claudeDefaultModel);

  // Check if Library and System settings are in sync (ignoring whitespace differences)
  const normalizeJson = (s: string) => {
    try { return JSON.stringify(JSON.parse(s)); } catch { return s; }
  };
  const isOutOfSync = systemSettings && originalSettings &&
    normalizeJson(systemSettings) !== normalizeJson(originalSettings);

  const loadSettings = useCallback(async (profile: string = selectedProfile) => {
    try {
      setLoading(true);
      setError(null);

      // Load OpenCode settings from Library for the selected profile
      const data = await getLibraryOpenCodeSettingsForProfile(profile);
      const formatted = JSON.stringify(data, null, 2);
      setSettings(formatted);
      setOriginalSettings(formatted);

      // Load system settings (for sync status comparison) - only relevant for default profile
      if (profile === 'default') {
        try {
          const sysData = await getOpenCodeSettings();
          setSystemSettings(JSON.stringify(sysData, null, 2));
        } catch {
          // System settings might not exist yet
          setSystemSettings('');
        }
      } else {
        setSystemSettings('');
      }

      // Load OpenAgent config for the selected profile
      const openAgentData = await getOpenAgentConfigForProfile(profile);
      setOpenAgentConfig(openAgentData);
      setOriginalOpenAgentConfig(openAgentData);

      // Load all agents for the checkbox list
      const agents = await listOpenCodeAgents();
      setAllAgents(parseAgentNames(agents));

      // Load Claude Code config for the selected profile
      try {
        const claudeData = await getClaudeCodeConfigForProfile(profile);
        setClaudeCodeConfig({
          ...claudeData,
          hidden_agents: claudeData.hidden_agents || [],
        });
        setOriginalClaudeCodeConfig({
          ...claudeData,
          hidden_agents: claudeData.hidden_agents || [],
        });
      } catch {
        // Claude Code config might not exist yet
        setClaudeCodeConfig({
          default_model: null,
          default_agent: null,
          hidden_agents: [],
        });
        setOriginalClaudeCodeConfig({
          default_model: null,
          default_agent: null,
          hidden_agents: [],
        });
      }

      // Load Claude Code agents for visibility settings
      try {
        const claudeAgents = await listBackendAgents('claudecode');
        setAllClaudeCodeAgents(claudeAgents.map(a => a.name));
      } catch {
        // Claude Code agents might not be available
        setAllClaudeCodeAgents([]);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load settings');
    } finally {
      setLoading(false);
    }
  }, [selectedProfile]);

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  // Validate JSON on change
  useEffect(() => {
    if (!settings.trim()) {
      setParseError(null);
      return;
    }
    try {
      JSON.parse(settings);
      setParseError(null);
    } catch (err) {
      setParseError(err instanceof Error ? err.message : 'Invalid JSON');
    }
  }, [settings]);

  // Handle keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        if (isDirty && !parseError) {
          handleSave();
        }
      }
      if (e.key === 'Escape') {
        if (showProfileDropdown) setShowProfileDropdown(false);
        if (showCommitDialog) setShowCommitDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isDirty, parseError, settings, showCommitDialog, showProfileDropdown]);

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

  const handleSave = async () => {
    if (parseError) return;

    try {
      setSaving(true);
      setError(null);
      const parsed = JSON.parse(settings);
      await saveLibraryOpenCodeSettingsForProfile(selectedProfile, parsed);
      setOriginalSettings(settings);
      if (selectedProfile === 'default') {
        setSystemSettings(settings); // Sync happened, update local system state
        setShowRestartModal(true); // Show modal asking to restart - only for default profile
      }
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2000);
      await refreshStatus(); // Update git status bar
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings');
    } finally {
      setSaving(false);
    }
  };

  const handleRestartFromModal = async () => {
    setShowRestartModal(false);
    await handleRestart();
  };

  const handleSkipRestart = () => {
    setShowRestartModal(false);
    setNeedsRestart(true);
  };

  const handleSaveOpenAgent = async () => {
    try {
      setSavingOpenAgent(true);
      setError(null);
      await saveOpenAgentConfigForProfile(selectedProfile, openAgentConfig);
      setOriginalOpenAgentConfig({ ...openAgentConfig });
      setOpenAgentSaveSuccess(true);
      setTimeout(() => setOpenAgentSaveSuccess(false), 2000);
      await refreshStatus(); // Update git status bar
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save OpenAgent config');
    } finally {
      setSavingOpenAgent(false);
    }
  };

  const handleSaveClaudeCode = async () => {
    try {
      setSavingClaudeCode(true);
      setError(null);
      await saveClaudeCodeConfigForProfile(selectedProfile, claudeCodeConfig);
      setOriginalClaudeCodeConfig({ ...claudeCodeConfig });
      setClaudeCodeSaveSuccess(true);
      setTimeout(() => setClaudeCodeSaveSuccess(false), 2000);
      await refreshStatus(); // Update git status bar
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save Claude Code config');
    } finally {
      setSavingClaudeCode(false);
    }
  };

  const handleCreateProfile = async () => {
    if (!newProfileName.trim()) return;
    try {
      setCreatingProfile(true);
      setError(null);
      await createConfigProfile(newProfileName.trim(), selectedProfile);
      await mutateProfiles();
      setSelectedProfile(newProfileName.trim());
      setNewProfileName('');
      setShowNewProfileDialog(false);
      await loadSettings(newProfileName.trim());
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
    await loadSettings(profileName);
  };

  const handleRestart = async () => {
    try {
      setRestarting(true);
      setError(null);
      // Sync config before restarting
      await sync();
      await restartOpenCodeService();
      setRestartSuccess(true);
      setNeedsRestart(false);
      setTimeout(() => setRestartSuccess(false), 3000);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to restart OpenCode');
    } finally {
      setRestarting(false);
    }
  };

  const handleReset = () => {
    setSettings(originalSettings);
    setParseError(null);
  };

  const handleSync = async () => {
    try {
      await sync();
      await loadSettings();
    } catch (err) {
      // If diverged history error, show the force sync dialog
      if (err instanceof DivergedHistoryError) {
        setShowForceSyncDialog(true);
      }
      // Other errors handled by context
    }
  };

  const handleForceSync = async () => {
    try {
      await forceSync();
      setShowForceSyncDialog(false);
      await loadSettings();
    } catch {
      // Error handled by context
    }
  };

  const handleForcePush = async () => {
    try {
      await forcePush();
      setShowForceSyncDialog(false);
      await refreshStatus();
    } catch {
      // Error handled by context
    }
  };

  const handleDismissForceSyncDialog = () => {
    setShowForceSyncDialog(false);
    clearDivergedHistory();
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
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

  const toggleHiddenAgent = (agentName: string) => {
    setOpenAgentConfig((prev) => {
      const hidden = prev.hidden_agents.includes(agentName)
        ? prev.hidden_agents.filter((a) => a !== agentName)
        : [...prev.hidden_agents, agentName];
      return { ...prev, hidden_agents: hidden };
    });
  };

  const visibleAgents = allAgents.filter((a) => !openAgentConfig.hidden_agents.includes(a));
  const visibleClaudeCodeAgents = allClaudeCodeAgents.filter((a) => !claudeCodeConfig.hidden_agents.includes(a));

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-5xl mx-auto space-y-6">
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

      {/* Diverged History Warning */}
      {divergedHistory && (
        <div className="p-4 rounded-lg bg-amber-500/10 border border-amber-500/20 flex items-start gap-3">
          <GitMerge className="h-5 w-5 text-amber-400 flex-shrink-0 mt-0.5" />
          <div className="flex-1">
            <p className="text-sm font-medium text-amber-400">Git History Diverged</p>
            <p className="text-sm text-amber-400/80 mt-1">
              Local and remote histories have diverged. This usually happens after a force push on the remote.
            </p>
            <button
              onClick={() => setShowForceSyncDialog(true)}
              className="mt-2 flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/30 rounded-lg transition-colors"
            >
              <GitMerge className="h-3.5 w-3.5" />
              Resolve Conflict
            </button>
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

      {/* Out of Sync Warning */}
      {isOutOfSync && (
        <div className="p-4 rounded-lg bg-amber-500/10 border border-amber-500/20 flex items-start gap-3">
          <AlertTriangle className="h-5 w-5 text-amber-400 flex-shrink-0 mt-0.5" />
          <div className="flex-1">
            <p className="text-sm font-medium text-amber-400">Settings out of sync</p>
            <p className="text-sm text-amber-400/80 mt-1">
              The Library settings differ from what OpenCode is currently using.
              This can happen if settings were changed outside the Library.
            </p>
            <div className="flex items-center gap-3 mt-3">
              <button
                onClick={() => {
                  setSettings(systemSettings);
                }}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/30 rounded-lg transition-colors"
              >
                <Download className="h-3.5 w-3.5" />
                Import from OpenCode
              </button>
              <span className="text-xs text-amber-400/60">or edit and save to overwrite</span>
            </div>
          </div>
        </div>
      )}

      {/* Restart Modal */}
      {showRestartModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
          <div className="w-full max-w-md mx-4 p-6 rounded-xl bg-[#1a1a1f] border border-white/10 shadow-2xl">
            <div className="flex items-start justify-between mb-4">
              <div className="flex items-center gap-3">
                <div className="p-2 rounded-lg bg-emerald-500/10">
                  <Check className="h-5 w-5 text-emerald-400" />
                </div>
                <h3 className="text-lg font-semibold text-white">Settings Saved</h3>
              </div>
              <button
                onClick={handleSkipRestart}
                className="p-1 text-white/40 hover:text-white transition-colors"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            <p className="text-sm text-white/60 mb-6">
              Your settings have been saved to the Library and synced to the system.
              OpenCode needs to be restarted for the changes to take effect.
            </p>
            <div className="flex gap-3">
              <button
                onClick={handleSkipRestart}
                className="flex-1 px-4 py-2 text-sm font-medium text-white/70 bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors"
              >
                Restart Later
              </button>
              <button
                onClick={handleRestartFromModal}
                disabled={restarting}
                className="flex-1 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors disabled:opacity-50 flex items-center justify-center gap-2"
              >
                {restarting ? (
                  <>
                    <Loader className="h-4 w-4 animate-spin" />
                    Restarting...
                  </>
                ) : (
                  <>
                    <RotateCcw className="h-4 w-4" />
                    Restart Now
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Harness Tabs and Profile Selector */}
      <div className="flex items-center justify-between mb-2">
        {/* Harness Tabs - Left */}
        <div className="flex items-center gap-2">
          {enabledBackends.map((backend) => (
            <button
              key={backend.id}
              onClick={() => setActiveHarness(backend.id as 'opencode' | 'claudecode' | 'amp')}
              className={cn(
                'px-4 py-2 rounded-lg text-sm font-medium border transition-colors',
                activeHarness === backend.id
                  ? 'bg-white/[0.08] border-white/[0.12] text-white'
                  : 'bg-white/[0.02] border-white/[0.06] text-white/50 hover:text-white/70'
              )}
            >
              {backend.name}
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
              Create a new configuration profile. It will be initialized with a copy of the current profile's settings.
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

      {activeHarness === 'opencode' ? (
        <>
          {/* OpenCode Settings Section */}
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-medium text-white">OpenCode Settings</h2>
            <p className="text-sm text-white/50">Configure oh-my-opencode plugin (agents, models)</p>
          </div>
          <div className="flex items-center gap-2">
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
              disabled={saving || !isDirty || !!parseError}
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
            <button
              onClick={() => loadSettings()}
              disabled={loading}
              title="Reloads the source from disk"
              className="flex items-center gap-2 px-3 py-1.5 text-sm text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
            >
              <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
              Reload
            </button>
            <button
              onClick={handleRestart}
              disabled={restarting}
              title="Syncs config, and restarts OpenCode"
              className={cn(
                'flex items-center gap-2 px-4 py-1.5 text-sm font-medium rounded-lg transition-colors',
                needsRestart
                  ? 'text-white bg-amber-500 hover:bg-amber-600'
                  : restartSuccess
                    ? 'text-emerald-400 bg-emerald-500/10'
                    : 'text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08]'
              )}
            >
              {restarting ? (
                <Loader className="h-4 w-4 animate-spin" />
              ) : restartSuccess ? (
                <Check className="h-4 w-4" />
              ) : (
                <RotateCcw className="h-4 w-4" />
              )}
              {restarting ? 'Restarting...' : restartSuccess ? 'Restarted!' : 'Restart'}
            </button>
          </div>
        </div>

        {/* Status Bar */}
        <div className="flex items-center gap-4 text-xs text-white/50">
          {isDirty && <span className="text-amber-400">Unsaved changes</span>}
          {parseError && (
            <span className="text-red-400 flex items-center gap-1">
              <AlertCircle className="h-3 w-3" />
              {parseError}
            </span>
          )}
          {needsRestart && !isDirty && (
            <span className="text-amber-400">Settings saved - restart OpenCode to apply changes</span>
          )}
        </div>

        {/* Editor */}
        <div className="min-h-[400px] rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
          <ConfigCodeEditor
            value={settings}
            onChange={setSettings}
            placeholder='{\n  "agents": {\n    "Sisyphus": {\n      "model": "anthropic/claude-opus-4-5"\n    }\n  }\n}'
            disabled={saving}
            className="h-full"
            minHeight={400}
            padding={16}
            language="json"
          />
        </div>

        {/* Supported Models */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-5 space-y-3">
          <div className="flex items-center justify-between">
            <div>
              <h3 className="text-sm font-medium text-white/80">Supported Models</h3>
              <p className="text-xs text-white/50">Available model overrides for OpenCode agents.</p>
            </div>
            <button
              onClick={() => mutateProviderCatalog()}
              disabled={providerCatalogLoading}
              className="flex items-center gap-2 px-3 py-1.5 text-xs text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
            >
              <RefreshCw className={cn('h-3 w-3', providerCatalogLoading && 'animate-spin')} />
              Refresh
            </button>
          </div>

          {providerCatalogLoading ? (
            <div className="text-xs text-white/40 flex items-center gap-2">
              <Loader className="h-3 w-3 animate-spin" />
              Loading models…
            </div>
          ) : providerCatalog.length === 0 ? (
            <p className="text-xs text-white/40">
              No providers found. Connect providers in Settings → AI Providers.
            </p>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              {providerCatalog.map((provider) => (
                <div
                  key={provider.id}
                  className="rounded-lg border border-white/[0.06] bg-white/[0.03] p-3"
                >
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-sm text-white/80">{provider.name}</span>
                    <span className="text-[10px] uppercase tracking-wide text-white/40">
                      {provider.id}
                    </span>
                  </div>
                  {provider.models.length === 0 ? (
                    <span className="text-xs text-white/40">No models listed</span>
                  ) : (
                    <div className="flex flex-wrap gap-2">
                      {provider.models.map((model) => (
                        <div
                          key={`${provider.id}/${model.id}`}
                          className="px-2 py-1 rounded-md border border-white/[0.06] bg-white/[0.02] text-xs text-white/80"
                          title={`${provider.id}/${model.id}`}
                        >
                          <div className="font-medium">{model.name || model.id}</div>
                          <div className="text-[10px] text-white/40">{model.id}</div>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* OpenAgent Settings Section */}
      <div className="p-6 rounded-xl bg-white/[0.02] border border-white/[0.06] space-y-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-medium text-white">OpenAgent Settings</h2>
            <p className="text-sm text-white/50">Configure agent visibility in mission dialog</p>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleSaveOpenAgent}
              disabled={savingOpenAgent || !isOpenAgentDirty}
              className={cn(
                'flex items-center gap-2 px-4 py-1.5 text-sm font-medium rounded-lg transition-colors',
                isOpenAgentDirty
                  ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                  : 'text-white/40 bg-white/[0.04] cursor-not-allowed'
              )}
            >
              {savingOpenAgent ? (
                <Loader className="h-4 w-4 animate-spin" />
              ) : openAgentSaveSuccess ? (
                <Check className="h-4 w-4 text-emerald-400" />
              ) : (
                <Save className="h-4 w-4" />
              )}
              {savingOpenAgent ? 'Saving...' : openAgentSaveSuccess ? 'Saved!' : 'Save'}
            </button>
            <button
              onClick={() => loadSettings()}
              disabled={loading}
              title="Reloads the source from disk"
              className="flex items-center gap-2 px-3 py-1.5 text-sm text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
            >
              <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
              Reload
            </button>
            <button
              onClick={handleRestart}
              disabled={restarting}
              title="Syncs config, and restarts OpenCode"
              className={cn(
                'flex items-center gap-2 px-4 py-1.5 text-sm font-medium rounded-lg transition-colors',
                needsRestart
                  ? 'text-white bg-amber-500 hover:bg-amber-600'
                  : restartSuccess
                    ? 'text-emerald-400 bg-emerald-500/10'
                    : 'text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08]'
              )}
            >
              {restarting ? (
                <Loader className="h-4 w-4 animate-spin" />
              ) : restartSuccess ? (
                <Check className="h-4 w-4" />
              ) : (
                <RotateCcw className="h-4 w-4" />
              )}
              {restarting ? 'Restarting...' : restartSuccess ? 'Restarted!' : 'Restart'}
            </button>
          </div>
        </div>

        {/* Agent Visibility */}
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-medium text-white/80">Agent Visibility</h3>
            <span className="text-xs text-white/40">
              {visibleAgents.length} visible, {openAgentConfig.hidden_agents.length} hidden
            </span>
          </div>
          <p className="text-xs text-white/50">
            Hidden agents will not appear in the mission dialog dropdown. They can still be used via API.
          </p>
          <div className="grid grid-cols-2 md:grid-cols-3 gap-2 mt-2">
            {allAgents.map((agent) => {
              const isHidden = openAgentConfig.hidden_agents.includes(agent);
              return (
                <button
                  key={agent}
                  onClick={() => toggleHiddenAgent(agent)}
                  className={cn(
                    'flex items-center gap-2 px-3 py-2 text-sm rounded-lg border transition-colors text-left',
                    isHidden
                      ? 'text-white/40 bg-white/[0.02] border-white/[0.04] hover:bg-white/[0.04]'
                      : 'text-white/80 bg-white/[0.04] border-white/[0.08] hover:bg-white/[0.06]'
                  )}
                >
                  {isHidden ? (
                    <EyeOff className="h-4 w-4 flex-shrink-0 text-white/30" />
                  ) : (
                    <Eye className="h-4 w-4 flex-shrink-0 text-emerald-400" />
                  )}
                  <span className="truncate">{agent}</span>
                </button>
              );
            })}
          </div>
        </div>

        {/* Default Agent */}
        <div className="space-y-2">
          <h3 className="text-sm font-medium text-white/80">Default Agent</h3>
          <p className="text-xs text-white/50">Pre-selected agent when creating a new mission.</p>
          <select
            value={openAgentConfig.default_agent || ''}
            onChange={(e) =>
              setOpenAgentConfig((prev) => ({
                ...prev,
                default_agent: e.target.value || null,
              }))
            }
            className="w-full max-w-xs px-3 py-2 text-sm text-white bg-white/[0.04] border border-white/[0.08] rounded-lg focus:outline-none focus:ring-2 focus:ring-indigo-500/50"
          >
            <option value="">Default (OpenCode default)</option>
            {visibleAgents.map((agent) => (
              <option key={agent} value={agent}>
                {agent}
              </option>
            ))}
          </select>
        </div>
      </div>
        </>
      ) : activeHarness === 'claudecode' ? (
        /* Claude Code Section */
        <div className="space-y-4">
          {/* Claude Code Settings Header */}
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-lg font-medium text-white">Claude Code Settings</h2>
              <p className="text-sm text-white/50">Configure default model and agent for Claude Code missions</p>
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={handleSaveClaudeCode}
                disabled={savingClaudeCode || !isClaudeCodeDirty}
                className={cn(
                  'flex items-center gap-2 px-4 py-1.5 text-sm font-medium rounded-lg transition-colors',
                  isClaudeCodeDirty
                    ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                    : 'text-white/40 bg-white/[0.04] cursor-not-allowed'
                )}
              >
                {savingClaudeCode ? (
                  <Loader className="h-4 w-4 animate-spin" />
                ) : claudeCodeSaveSuccess ? (
                  <Check className="h-4 w-4 text-emerald-400" />
                ) : (
                  <Save className="h-4 w-4" />
                )}
                {savingClaudeCode ? 'Saving...' : claudeCodeSaveSuccess ? 'Saved!' : 'Save'}
              </button>
              <button
                onClick={() => loadSettings()}
                disabled={loading}
                title="Reloads the source from disk"
                className="flex items-center gap-2 px-3 py-1.5 text-sm text-white/70 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors disabled:opacity-50"
              >
                <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
                Reload
              </button>
            </div>
          </div>

          {/* Default Model */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium text-white/80">Default Model</h3>
            <p className="text-xs text-white/50">Model used for new Claude Code missions if not overridden.</p>
            <select
              value={claudeDefaultModel}
              onChange={(e) =>
                setClaudeCodeConfig((prev) => ({
                  ...prev,
                  default_model: e.target.value || null,
                }))
              }
              className="w-full max-w-md px-3 py-2 text-sm text-white bg-white/[0.04] border border-white/[0.08] rounded-lg focus:outline-none focus:ring-2 focus:ring-indigo-500/50"
            >
              <option value="">Default (Claude Code default)</option>
              {anthropicModels.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.name || model.id}
                </option>
              ))}
              {!claudeDefaultModelKnown && claudeDefaultModel && (
                <option value={claudeDefaultModel}>
                  {claudeDefaultModel} (custom)
                </option>
              )}
            </select>
            <p className="text-xs text-white/30">
              {providerCatalogLoading
                ? 'Loading Anthropic models…'
                : anthropicModels.length === 0
                  ? 'No Anthropic models found. Connect Anthropic in Settings → AI Providers.'
                  : 'Choose from supported Anthropic models.'}
            </p>
          </div>

          {/* Default Agent */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium text-white/80">Default Agent</h3>
            <p className="text-xs text-white/50">Pre-selected agent when creating a new Claude Code mission.</p>
            <select
              value={claudeCodeConfig.default_agent || ''}
              onChange={(e) =>
                setClaudeCodeConfig((prev) => ({
                  ...prev,
                  default_agent: e.target.value || null,
                }))
              }
              className="w-full max-w-xs px-3 py-2 text-sm text-white bg-white/[0.04] border border-white/[0.08] rounded-lg focus:outline-none focus:ring-2 focus:ring-indigo-500/50"
            >
              <option value="">Default (Claude Code default)</option>
              {visibleClaudeCodeAgents.map((agent) => (
                <option key={agent} value={agent}>
                  {agent}
                </option>
              ))}
            </select>
          </div>

          {/* Agent Visibility */}
          {allClaudeCodeAgents.length > 0 && (
            <div className="p-6 rounded-xl bg-white/[0.02] border border-white/[0.06] space-y-4 mt-4">
              <div className="flex items-center justify-between">
                <h3 className="text-sm font-medium text-white/80">Agent Visibility</h3>
                <span className="text-xs text-white/40">
                  {allClaudeCodeAgents.filter(a => !claudeCodeConfig.hidden_agents.includes(a)).length} visible, {claudeCodeConfig.hidden_agents.length} hidden
                </span>
              </div>
              <p className="text-xs text-white/50">
                Hidden agents will not appear in the mission dialog dropdown. They can still be used via API.
              </p>
              <div className="grid grid-cols-2 md:grid-cols-3 gap-2 mt-2">
                {allClaudeCodeAgents.map((agent) => {
                  const isHidden = claudeCodeConfig.hidden_agents.includes(agent);
                  return (
                    <button
                      key={agent}
                      onClick={() => {
                        setClaudeCodeConfig((prev) => {
                          const hidden = prev.hidden_agents.includes(agent)
                            ? prev.hidden_agents.filter((a) => a !== agent)
                            : [...prev.hidden_agents, agent];
                          return { ...prev, hidden_agents: hidden };
                        });
                      }}
                      className={cn(
                        'flex items-center gap-2 px-3 py-2 text-sm rounded-lg border transition-colors text-left',
                        isHidden
                          ? 'text-white/40 bg-white/[0.02] border-white/[0.04] hover:bg-white/[0.04]'
                          : 'text-white/80 bg-white/[0.04] border-white/[0.08] hover:bg-white/[0.06]'
                      )}
                    >
                      {isHidden ? (
                        <EyeOff className="h-4 w-4 flex-shrink-0 text-white/30" />
                      ) : (
                        <Eye className="h-4 w-4 flex-shrink-0 text-emerald-400" />
                      )}
                      <span className="truncate">{agent}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}

          {/* Backend Settings Link */}
          <div className="p-4 rounded-xl bg-blue-500/5 border border-blue-500/20">
            <div className="flex items-start gap-3">
              <Info className="h-5 w-5 text-blue-400 flex-shrink-0 mt-0.5" />
              <div className="text-sm text-blue-400/80">
                <p className="font-medium text-blue-400">Backend Settings</p>
                <p className="mt-1">
                  To configure the Claude CLI path or enable/disable the backend, visit the{' '}
                  <a href="/settings" className="underline hover:text-blue-300">Settings page</a> → Backends → Claude Code.
                </p>
              </div>
            </div>
          </div>
        </div>
      ) : activeHarness === 'amp' ? (
        /* Amp Section */
        <div className="space-y-4">
          {/* Amp Settings Header */}
          <div className="flex items-center justify-between">
            <div>
              <h2 className="text-lg font-medium text-white">Amp Settings</h2>
              <p className="text-sm text-white/50">Configure default mode and agent for Amp missions</p>
            </div>
          </div>

          {/* Amp Info */}
          <div className="p-4 rounded-xl bg-indigo-500/5 border border-indigo-500/20">
            <div className="flex items-start gap-3">
              <Info className="h-5 w-5 text-indigo-400 flex-shrink-0 mt-0.5" />
              <div className="text-sm text-indigo-400/80">
                <p className="font-medium text-indigo-400">Amp Configuration</p>
                <p className="mt-1">
                  Amp uses built-in modes (Smart/Rush) and doesn't require agent configuration in the Library.
                  Configure CLI path and mode in the{' '}
                  <a href="/settings" className="underline hover:text-indigo-300">Settings page</a> → Backends → Amp.
                </p>
              </div>
            </div>
          </div>

          {/* Amp Modes */}
          <div className="space-y-2">
            <h3 className="text-sm font-medium text-white/80">Available Modes</h3>
            <div className="grid grid-cols-2 gap-3">
              <div className="p-3 rounded-lg border border-white/[0.06] bg-white/[0.02]">
                <div className="font-medium text-white/80">Smart Mode</div>
                <p className="text-xs text-white/50 mt-1">Full capability mode with comprehensive tool access.</p>
              </div>
              <div className="p-3 rounded-lg border border-white/[0.06] bg-white/[0.02]">
                <div className="font-medium text-white/80">Rush Mode</div>
                <p className="text-xs text-white/50 mt-1">Faster, cheaper mode for simpler tasks.</p>
              </div>
            </div>
          </div>
        </div>
      ) : null}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-md rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-white">Commit Changes</p>
                <p className="text-xs text-white/40">Describe your configuration changes.</p>
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
                placeholder="Update configuration settings"
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

      {/* Force Sync Dialog - shown when git history has diverged */}
      {showForceSyncDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-md px-4">
          <div className="w-full max-w-lg rounded-2xl bg-[#161618] border border-white/[0.06] shadow-[0_25px_100px_rgba(0,0,0,0.7)] overflow-hidden">
            <div className="px-5 py-4 border-b border-white/[0.06] flex items-center justify-between">
              <div className="flex items-center gap-3">
                <div className="p-2 rounded-lg bg-amber-500/10">
                  <GitMerge className="h-5 w-5 text-amber-400" />
                </div>
                <div>
                  <p className="text-sm font-medium text-white">Git History Diverged</p>
                  <p className="text-xs text-white/40">Local and remote histories have diverged</p>
                </div>
              </div>
              <button
                onClick={handleDismissForceSyncDialog}
                className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06]"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="p-5 space-y-4">
              <div className="p-3 rounded-lg bg-amber-500/10 border border-amber-500/20">
                <p className="text-sm text-amber-400/90">
                  {divergedHistoryMessage || 'The remote repository has been force-pushed, causing local and remote histories to diverge.'}
                </p>
              </div>
              <p className="text-sm text-white/60">
                Choose how to resolve this conflict:
              </p>
              <div className="space-y-3">
                <div className="p-4 rounded-lg border border-white/[0.08] bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
                  <div className="flex items-start justify-between">
                    <div>
                      <p className="text-sm font-medium text-white flex items-center gap-2">
                        <Download className="h-4 w-4 text-blue-400" />
                        Force Pull (Reset to Remote)
                      </p>
                      <p className="text-xs text-white/50 mt-1">
                        Discard all local changes and reset to match the remote. Use this if you want to accept the remote&apos;s version.
                      </p>
                    </div>
                    <button
                      onClick={handleForceSync}
                      disabled={syncing || pushing}
                      className="flex-shrink-0 flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-blue-500 hover:bg-blue-600 rounded-lg disabled:opacity-50 transition-colors"
                    >
                      {syncing ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}
                      Force Pull
                    </button>
                  </div>
                </div>
                <div className="p-4 rounded-lg border border-white/[0.08] bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
                  <div className="flex items-start justify-between">
                    <div>
                      <p className="text-sm font-medium text-white flex items-center gap-2">
                        <Upload className="h-4 w-4 text-orange-400" />
                        Force Push (Overwrite Remote)
                      </p>
                      <p className="text-xs text-white/50 mt-1">
                        Overwrite the remote with your local changes. Use this if your local changes are correct.
                      </p>
                    </div>
                    <button
                      onClick={handleForcePush}
                      disabled={syncing || pushing}
                      className="flex-shrink-0 flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-orange-500 hover:bg-orange-600 rounded-lg disabled:opacity-50 transition-colors"
                    >
                      {pushing ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Upload className="h-3.5 w-3.5" />}
                      Force Push
                    </button>
                  </div>
                </div>
              </div>
            </div>
            <div className="px-5 pb-5 flex items-center justify-end">
              <button
                onClick={handleDismissForceSyncDialog}
                className="px-4 py-2 text-xs text-white/60 hover:text-white/80"
              >
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
