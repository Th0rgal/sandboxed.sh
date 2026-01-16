'use client';

import { useState, useEffect, useCallback } from 'react';
import {
  getLibraryOpenCodeSettings,
  saveLibraryOpenCodeSettings,
  getOpenCodeSettings,
  restartOpenCodeService,
  getOpenAgentConfig,
  saveOpenAgentConfig,
  listOpenCodeAgents,
  OpenAgentConfig,
} from '@/lib/api';
import { Save, Loader, AlertCircle, Check, RefreshCw, RotateCcw, Eye, EyeOff, AlertTriangle, X, GitBranch, Upload } from 'lucide-react';
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
    commit,
    push,
    syncing,
    committing,
    pushing,
    refreshStatus,
  } = useLibrary();

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

  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');

  const isDirty = settings !== originalSettings;
  const isOpenAgentDirty =
    JSON.stringify(openAgentConfig) !== JSON.stringify(originalOpenAgentConfig);

  // Check if Library and System settings are in sync (ignoring whitespace differences)
  const normalizeJson = (s: string) => {
    try { return JSON.stringify(JSON.parse(s)); } catch { return s; }
  };
  const isOutOfSync = systemSettings && originalSettings &&
    normalizeJson(systemSettings) !== normalizeJson(originalSettings);

  const loadSettings = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);

      // Load OpenCode settings from Library
      const data = await getLibraryOpenCodeSettings();
      const formatted = JSON.stringify(data, null, 2);
      setSettings(formatted);
      setOriginalSettings(formatted);

      // Load system settings (for sync status comparison)
      try {
        const sysData = await getOpenCodeSettings();
        setSystemSettings(JSON.stringify(sysData, null, 2));
      } catch {
        // System settings might not exist yet
        setSystemSettings('');
      }

      // Load OpenAgent config
      const openAgentData = await getOpenAgentConfig();
      setOpenAgentConfig(openAgentData);
      setOriginalOpenAgentConfig(openAgentData);

      // Load all agents for the checkbox list
      const agents = await listOpenCodeAgents();
      setAllAgents(parseAgentNames(agents));
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load settings');
    } finally {
      setLoading(false);
    }
  }, []);

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
        if (showCommitDialog) setShowCommitDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [isDirty, parseError, settings, showCommitDialog]);

  const handleSave = async () => {
    if (parseError) return;

    try {
      setSaving(true);
      setError(null);
      const parsed = JSON.parse(settings);
      await saveLibraryOpenCodeSettings(parsed);
      setOriginalSettings(settings);
      setSystemSettings(settings); // Sync happened, update local system state
      setSaveSuccess(true);
      setShowRestartModal(true); // Show modal asking to restart
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
      await saveOpenAgentConfig(openAgentConfig);
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
    } catch {
      // Error handled by context
    }
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
              Save your current settings to sync them to OpenCode.
            </p>
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
              onClick={loadSettings}
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
            language="json"
            placeholder='{\n  "agents": {\n    "Sisyphus": {\n      "model": "anthropic/claude-opus-4-5"\n    }\n  }\n}'
            disabled={saving}
            className="h-full"
            minHeight={400}
            padding={16}
          />
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
              onClick={loadSettings}
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
    </div>
  );
}
