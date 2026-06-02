'use client';

import { useState, useCallback } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  getSettings,
  updateLibraryRemote,
  updateSettings,
  downloadBackup,
  restoreBackup,
} from '@/lib/api';
import {
  GitBranch,
  Loader,
  Check,
  X,
  Download,
  Upload,
  Archive,
  Trash2,
} from 'lucide-react';
import { cn } from '@/lib/utils';

export default function DataSettingsPage() {
  const [editingLibraryRemote, setEditingLibraryRemote] = useState(false);
  const [libraryRemoteValue, setLibraryRemoteValue] = useState('');
  const [savingLibraryRemote, setSavingLibraryRemote] = useState(false);
  const [editingRepoPath, setEditingRepoPath] = useState(false);
  const [repoPathValue, setRepoPathValue] = useState('');
  const [savingRepoPath, setSavingRepoPath] = useState(false);
  const [togglingAutoCleanup, setTogglingAutoCleanup] = useState(false);
  const [editingCleanupDays, setEditingCleanupDays] = useState(false);
  const [cleanupDaysValue, setCleanupDaysValue] = useState('');
  const [savingCleanupDays, setSavingCleanupDays] = useState(false);

  const [downloadingBackup, setDownloadingBackup] = useState(false);
  const [restoringBackup, setRestoringBackup] = useState(false);
  const fileInputRef = useCallback((node: HTMLInputElement | null) => {
    if (node) {
      node.value = '';
    }
  }, []);

  const { data: serverSettings, isLoading: settingsLoading, mutate: mutateSettings } = useSWR(
    'settings',
    getSettings,
    { revalidateOnFocus: false }
  );

  const handleStartEditLibraryRemote = () => {
    setLibraryRemoteValue(serverSettings?.library_remote || '');
    setEditingLibraryRemote(true);
  };

  const handleCancelEditLibraryRemote = () => {
    setEditingLibraryRemote(false);
    setLibraryRemoteValue('');
  };

  const handleSaveLibraryRemote = async () => {
    setSavingLibraryRemote(true);
    try {
      const trimmed = libraryRemoteValue.trim();
      const result = await updateLibraryRemote(trimmed || null);

      mutateSettings();

      setEditingLibraryRemote(false);

      if (result.library_reinitialized) {
        if (result.library_error) {
          toast.error(`Library saved but failed to initialize: ${result.library_error}`);
        } else if (result.library_remote) {
          toast.success('Library remote updated and reinitialized');
        } else {
          toast.success('Library remote cleared');
        }
      } else {
        toast.success('Library remote saved (no change)');
      }
    } catch (err) {
      toast.error(
        `Failed to save: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setSavingLibraryRemote(false);
    }
  };

  const handleStartEditRepoPath = () => {
    setRepoPathValue(serverSettings?.sandboxed_repo_path || '');
    setEditingRepoPath(true);
  };

  const handleCancelEditRepoPath = () => {
    setEditingRepoPath(false);
    setRepoPathValue('');
  };

  const handleSaveRepoPath = async () => {
    setSavingRepoPath(true);
    try {
      const trimmed = repoPathValue.trim();
      await updateSettings({ sandboxed_repo_path: trimmed || null });
      mutateSettings();
      setEditingRepoPath(false);
      if (trimmed) {
        toast.success('Source repo path updated');
      } else {
        toast.success('Source repo path cleared');
      }
    } catch (err) {
      toast.error(
        `Failed to save: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setSavingRepoPath(false);
    }
  };

  const handleToggleAutoCleanup = async (enabled: boolean) => {
    setTogglingAutoCleanup(true);
    try {
      const next = await updateSettings({ auto_cleanup_enabled: enabled });
      mutateSettings(next, { revalidate: false });
      toast.success(
        enabled
          ? 'Auto-cleanup enabled — old mission files will be deleted on the next hourly sweep'
          : 'Auto-cleanup disabled'
      );
    } catch (err) {
      toast.error(
        `Failed to update auto-cleanup: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setTogglingAutoCleanup(false);
    }
  };

  const handleStartEditCleanupDays = () => {
    setCleanupDaysValue(
      serverSettings?.auto_cleanup_days != null
        ? String(serverSettings.auto_cleanup_days)
        : '7'
    );
    setEditingCleanupDays(true);
  };

  const handleCancelEditCleanupDays = () => {
    setEditingCleanupDays(false);
    setCleanupDaysValue('');
  };

  const handleSaveCleanupDays = async () => {
    const parsed = Number.parseInt(cleanupDaysValue, 10);
    if (!Number.isFinite(parsed) || parsed < 1) {
      toast.error('Retention must be a whole number of days (1 or more)');
      return;
    }
    setSavingCleanupDays(true);
    try {
      const next = await updateSettings({ auto_cleanup_days: parsed });
      mutateSettings(next, { revalidate: false });
      setEditingCleanupDays(false);
      toast.success(`Retention set to ${parsed} day${parsed === 1 ? '' : 's'}`);
    } catch (err) {
      toast.error(
        `Failed to update retention: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setSavingCleanupDays(false);
    }
  };

  const handleDownloadBackup = async () => {
    setDownloadingBackup(true);
    try {
      await downloadBackup();
      toast.success('Backup downloaded successfully');
    } catch (err) {
      toast.error(
        `Failed to download backup: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setDownloadingBackup(false);
    }
  };

  const handleRestoreBackup = async (file: File) => {
    setRestoringBackup(true);
    try {
      const result = await restoreBackup(file);
      if (result.success) {
        toast.success(result.message);
        mutateSettings();
      } else {
        toast.error(result.message);
        if (result.errors.length > 0) {
          result.errors.forEach((error) => toast.error(error));
        }
      }
    } catch (err) {
      toast.error(
        `Failed to restore backup: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setRestoringBackup(false);
    }
  };

  return (
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <div className="w-full max-w-4xl space-y-6">
        <header>
          <h1 className="text-xl font-semibold text-white">Data</h1>
          <p className="mt-1 text-sm text-white/50">
            Library settings and backup management
          </p>
        </header>

        <div className="space-y-5">
          <div className="grid gap-5 md:grid-cols-2">
          {/* Library Settings */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 flex flex-col">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10 flex-shrink-0">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div className="min-w-0">
                <h2 className="text-sm font-medium text-white">Library</h2>
                <p className="text-xs text-white/40">
                  Git-based configuration library for skills, tools, and agents
                </p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Library Remote
              </label>
              {settingsLoading ? (
                <div className="flex items-center gap-2 py-2.5">
                  <Loader className="h-4 w-4 animate-spin text-white/40" />
                  <span className="text-sm text-white/40">Loading...</span>
                </div>
              ) : editingLibraryRemote ? (
                <div className="space-y-2">
                  <input
                    type="text"
                    value={libraryRemoteValue}
                    onChange={(e) => setLibraryRemoteValue(e.target.value)}
                    placeholder="git@github.com:your-org/agent-library.git"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white font-mono focus:outline-none focus:border-indigo-500/50"
                    autoFocus
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleSaveLibraryRemote();
                      if (e.key === 'Escape') handleCancelEditLibraryRemote();
                    }}
                  />
                  <div className="flex items-center gap-2">
                    <button
                      onClick={handleSaveLibraryRemote}
                      disabled={savingLibraryRemote}
                      className="flex items-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50"
                    >
                      {savingLibraryRemote ? (
                        <Loader className="h-3 w-3 animate-spin" />
                      ) : (
                        <Check className="h-3 w-3" />
                      )}
                      Save
                    </button>
                    <button
                      onClick={handleCancelEditLibraryRemote}
                      disabled={savingLibraryRemote}
                      className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
                    >
                      <X className="h-3 w-3" />
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <div
                  onClick={handleStartEditLibraryRemote}
                  className={cn(
                    'w-full rounded-lg border px-3 py-2.5 text-sm font-mono cursor-pointer transition-colors',
                    serverSettings?.library_remote
                      ? 'border-white/[0.06] bg-white/[0.01] text-white/70 hover:border-indigo-500/30 hover:bg-white/[0.02]'
                      : 'border-amber-500/20 bg-amber-500/5 text-amber-400/80 hover:border-amber-500/30 hover:bg-amber-500/10'
                  )}
                  title="Click to edit"
                >
                  {serverSettings?.library_remote || 'Not configured'}
                </div>
              )}
              <p className="mt-1.5 text-xs text-white/30">
                Git remote URL for skills, tools, agents, and rules. Click to edit.
              </p>
            </div>
          </div>

          {/* sandboxed.sh Source */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5 flex flex-col">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10 flex-shrink-0">
                <Archive className="h-5 w-5 text-emerald-400" />
              </div>
              <div className="min-w-0">
                <h2 className="text-sm font-medium text-white">sandboxed.sh Source</h2>
                <p className="text-xs text-white/40">
                  Path to the sandboxed.sh git checkout used for updates
                </p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Source Repo Path
              </label>
              {settingsLoading ? (
                <div className="flex items-center gap-2 py-2.5">
                  <Loader className="h-4 w-4 animate-spin text-white/40" />
                  <span className="text-sm text-white/40">Loading...</span>
                </div>
              ) : editingRepoPath ? (
                <div className="space-y-2">
                  <input
                    type="text"
                    value={repoPathValue}
                    onChange={(e) => setRepoPathValue(e.target.value)}
                    placeholder="/opt/sandboxed-sh/vaduz-v1"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white font-mono focus:outline-none focus:border-emerald-500/50"
                    autoFocus
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleSaveRepoPath();
                      if (e.key === 'Escape') handleCancelEditRepoPath();
                    }}
                  />
                  <div className="flex items-center gap-2">
                    <button
                      onClick={handleSaveRepoPath}
                      disabled={savingRepoPath}
                      className="flex items-center gap-1.5 rounded-lg bg-emerald-500 px-3 py-1.5 text-xs text-white hover:bg-emerald-600 transition-colors cursor-pointer disabled:opacity-50"
                    >
                      {savingRepoPath ? (
                        <Loader className="h-3 w-3 animate-spin" />
                      ) : (
                        <Check className="h-3 w-3" />
                      )}
                      Save
                    </button>
                    <button
                      onClick={handleCancelEditRepoPath}
                      disabled={savingRepoPath}
                      className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
                    >
                      <X className="h-3 w-3" />
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <div
                  onClick={handleStartEditRepoPath}
                  className={cn(
                    'w-full rounded-lg border px-3 py-2.5 text-sm font-mono cursor-pointer transition-colors',
                    serverSettings?.sandboxed_repo_path
                      ? 'border-white/[0.06] bg-white/[0.01] text-white/70 hover:border-emerald-500/30 hover:bg-white/[0.02]'
                      : 'border-amber-500/20 bg-amber-500/5 text-amber-400/80 hover:border-amber-500/30 hover:bg-amber-500/10'
                  )}
                  title="Click to edit"
                >
                  {serverSettings?.sandboxed_repo_path || 'Using default path'}
                </div>
              )}
              <p className="mt-1.5 text-xs text-white/30">
                Leave blank to use the server default or <span className="font-mono">SANDBOXED_SH_REPO_PATH</span>.
              </p>
            </div>
          </div>
          </div>


          {/* Auto-cleanup of old mission workspace files */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-rose-500/10">
                <Trash2 className="h-5 w-5 text-rose-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Auto-cleanup old mission files</h2>
                <p className="text-xs text-white/40">
                  Periodically delete on-disk sandbox files for missions that ended a while ago
                </p>
              </div>
            </div>

            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <p className="text-sm text-white/70">
                  {serverSettings?.auto_cleanup_enabled
                    ? `Sweeping every hour — anything completed, failed, or interrupted more than ${
                        serverSettings?.auto_cleanup_days ?? 7
                      } day${(serverSettings?.auto_cleanup_days ?? 7) === 1 ? '' : 's'} ago is removed`
                    : 'Disabled — old mission directories accumulate until you clean them manually'}
                </p>
                <p className="mt-1 text-xs text-white/40">
                  Only the agent&apos;s sandbox files are deleted. Conversation history stays in the
                  mission store, so the mission can still be reopened. Running and awaiting-user
                  missions are never touched.
                </p>
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                {togglingAutoCleanup && (
                  <Loader className="h-3.5 w-3.5 animate-spin text-white/40" />
                )}
                <button
                  type="button"
                  aria-label="Toggle auto-cleanup"
                  aria-pressed={Boolean(serverSettings?.auto_cleanup_enabled)}
                  onClick={() =>
                    handleToggleAutoCleanup(!Boolean(serverSettings?.auto_cleanup_enabled))
                  }
                  disabled={togglingAutoCleanup || settingsLoading}
                  className={cn(
                    'relative inline-flex h-6 w-11 items-center rounded-full transition-colors disabled:opacity-60 disabled:cursor-not-allowed',
                    serverSettings?.auto_cleanup_enabled ? 'bg-rose-500' : 'bg-white/10'
                  )}
                >
                  <span
                    className={cn(
                      'inline-block h-4 w-4 rounded-full bg-white transition-transform',
                      serverSettings?.auto_cleanup_enabled ? 'translate-x-6' : 'translate-x-1'
                    )}
                  />
                </button>
              </div>
            </div>

            <div className="mt-4 flex items-center justify-between gap-4">
              <div className="min-w-0">
                <p className="text-xs uppercase tracking-wide text-white/40">
                  Retention window
                </p>
                {editingCleanupDays ? (
                  <div className="mt-1 flex items-center gap-2">
                    <input
                      type="number"
                      min={1}
                      value={cleanupDaysValue}
                      onChange={(e) => setCleanupDaysValue(e.target.value)}
                      className="w-20 rounded-md bg-white/[0.04] border border-white/10 px-2 py-1 text-sm text-white focus:outline-none focus:border-white/30"
                      autoFocus
                    />
                    <span className="text-sm text-white/60">days</span>
                  </div>
                ) : (
                  <p className="mt-1 text-sm text-white/80">
                    {serverSettings?.auto_cleanup_days ?? 7} day
                    {(serverSettings?.auto_cleanup_days ?? 7) === 1 ? '' : 's'}
                  </p>
                )}
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                {editingCleanupDays ? (
                  <>
                    <button
                      type="button"
                      onClick={handleSaveCleanupDays}
                      disabled={savingCleanupDays}
                      className="flex items-center gap-1 rounded-md bg-rose-500/20 hover:bg-rose-500/30 px-2 py-1 text-xs text-rose-200 disabled:opacity-60"
                    >
                      {savingCleanupDays ? (
                        <Loader className="h-3 w-3 animate-spin" />
                      ) : (
                        <Check className="h-3 w-3" />
                      )}
                      Save
                    </button>
                    <button
                      type="button"
                      onClick={handleCancelEditCleanupDays}
                      disabled={savingCleanupDays}
                      className="flex items-center gap-1 rounded-md bg-white/[0.04] hover:bg-white/10 px-2 py-1 text-xs text-white/70 disabled:opacity-60"
                    >
                      <X className="h-3 w-3" />
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={handleStartEditCleanupDays}
                    disabled={settingsLoading}
                    className="rounded-md bg-white/[0.04] hover:bg-white/10 px-2 py-1 text-xs text-white/70 disabled:opacity-60"
                  >
                    Edit
                  </button>
                )}
              </div>
            </div>
          </div>

          {/* Backup & Restore */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-amber-500/10">
                <Archive className="h-5 w-5 text-amber-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Backup & Restore</h2>
                <p className="text-xs text-white/40">
                  Export or import your settings, credentials, and provider configurations
                </p>
              </div>
            </div>

            <div className="space-y-3">
              <p className="text-xs text-white/50">
                Backup includes: AI provider credentials, backend settings,
                workspace definitions, MCP configurations, encrypted secrets, and the
                library encryption key.
              </p>

              <div className="flex flex-wrap items-center gap-3">
                <button
                  onClick={handleDownloadBackup}
                  disabled={downloadingBackup}
                  className="flex items-center gap-2 rounded-lg bg-indigo-500 px-4 py-2 text-sm text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                >
                  {downloadingBackup ? (
                    <Loader className="h-4 w-4 animate-spin" />
                  ) : (
                    <Download className="h-4 w-4" />
                  )}
                  Download Backup
                </button>

                <label className="flex items-center gap-2 rounded-lg border border-white/[0.08] bg-white/[0.02] px-4 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer">
                  {restoringBackup ? (
                    <Loader className="h-4 w-4 animate-spin" />
                  ) : (
                    <Upload className="h-4 w-4" />
                  )}
                  Restore Backup
                  <input
                    type="file"
                    accept=".zip"
                    className="hidden"
                    ref={fileInputRef}
                    disabled={restoringBackup}
                    onChange={(e) => {
                      const file = e.target.files?.[0];
                      if (file) {
                        handleRestoreBackup(file);
                        e.target.value = '';
                      }
                    }}
                  />
                </label>
              </div>

              <p className="text-xs text-white/30">
                After restoring, a server restart may be required to apply credential changes.
              </p>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
