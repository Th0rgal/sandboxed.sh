'use client';

import { useEffect, useState } from 'react';
import {
  getLibraryStatus,
  syncLibrary,
  commitLibrary,
  pushLibrary,
  listLibraryCommands,
  getLibraryCommand,
  saveLibraryCommand,
  deleteLibraryCommand,
  type LibraryStatus,
  type CommandSummary,
  type Command,
  LibraryUnavailableError,
} from '@/lib/api';
import {
  GitBranch,
  RefreshCw,
  Upload,
  Check,
  AlertCircle,
  Loader,
  Plus,
  Save,
  Trash2,
  X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';

export default function CommandsPage() {
  const [status, setStatus] = useState<LibraryStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [pushing, setPushing] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');
  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [libraryUnavailable, setLibraryUnavailable] = useState(false);
  const [libraryUnavailableMessage, setLibraryUnavailableMessage] = useState<string | null>(null);

  const [commands, setCommands] = useState<CommandSummary[]>([]);
  const [selectedCommand, setSelectedCommand] = useState<Command | null>(null);
  const [commandContent, setCommandContent] = useState('');
  const [commandDirty, setCommandDirty] = useState(false);
  const [commandSaving, setCommandSaving] = useState(false);
  const [loadingCommand, setLoadingCommand] = useState(false);
  const [showNewCommandDialog, setShowNewCommandDialog] = useState(false);
  const [newCommandName, setNewCommandName] = useState('');

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      setLibraryUnavailable(false);
      setLibraryUnavailableMessage(null);
      const [statusData, commandsData] = await Promise.all([
        getLibraryStatus(),
        listLibraryCommands(),
      ]);
      setStatus(statusData);
      setCommands(commandsData);
    } catch (err) {
      if (err instanceof LibraryUnavailableError) {
        setLibraryUnavailable(true);
        setLibraryUnavailableMessage(err.message);
        setStatus(null);
        setCommands([]);
        setSelectedCommand(null);
        setCommandContent('');
        setCommandDirty(false);
        return;
      }
      setError(err instanceof Error ? err.message : 'Failed to load commands');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const handleSync = async () => {
    try {
      setSyncing(true);
      await syncLibrary();
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to sync');
    } finally {
      setSyncing(false);
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      setCommitting(true);
      await commitLibrary(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to commit');
    } finally {
      setCommitting(false);
    }
  };

  const handlePush = async () => {
    try {
      setPushing(true);
      await pushLibrary();
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to push');
    } finally {
      setPushing(false);
    }
  };

  const loadCommand = async (name: string) => {
    try {
      setLoadingCommand(true);
      const command = await getLibraryCommand(name);
      setSelectedCommand(command);
      setCommandContent(command.content);
      setCommandDirty(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load command');
    } finally {
      setLoadingCommand(false);
    }
  };

  const handleCommandSave = async () => {
    if (!selectedCommand) return;
    try {
      setCommandSaving(true);
      await saveLibraryCommand(selectedCommand.name, commandContent);
      setCommandDirty(false);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save command');
    } finally {
      setCommandSaving(false);
    }
  };

  const handleCommandCreate = async () => {
    if (!newCommandName.trim()) return;
    const template = `---
description: A new command
---

Describe what this command does.
`;
    try {
      setCommandSaving(true);
      await saveLibraryCommand(newCommandName, template);
      await loadData();
      setShowNewCommandDialog(false);
      setNewCommandName('');
      loadCommand(newCommandName);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create command');
    } finally {
      setCommandSaving(false);
    }
  };

  const handleCommandDelete = async () => {
    if (!selectedCommand) return;
    if (!confirm(`Delete command "${selectedCommand.name}"?`)) return;
    try {
      await deleteLibraryCommand(selectedCommand.name);
      setSelectedCommand(null);
      setCommandContent('');
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete command');
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-6xl mx-auto space-y-4">
      {libraryUnavailable ? (
        <LibraryUnavailable message={libraryUnavailableMessage} />
      ) : (
        <>
          {error && (
            <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
              <AlertCircle className="h-4 w-4 flex-shrink-0" />
              {error}
              <button onClick={() => setError(null)} className="ml-auto">
                <X className="h-4 w-4" />
              </button>
            </div>
          )}

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
                      <Check className="h-3 w-3" />
                      Commit
                    </button>
                  )}
                  {status.ahead > 0 && (
                    <button
                      onClick={handlePush}
                      disabled={pushing}
                      className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-emerald-400 hover:text-emerald-300 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors disabled:opacity-50"
                    >
                      <Upload className={cn('h-3 w-3', pushing && 'animate-pulse')} />
                      Push
                    </button>
                  )}
                </div>
              </div>
            </div>
          )}

          {/* Commands Editor */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
            <div className="flex h-[500px]">
              {/* Commands List */}
              <div className="w-64 border-r border-white/[0.06] flex flex-col">
                <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                  <span className="text-xs font-medium text-white/60">
                    Commands{commands.length ? ` (${commands.length})` : ''}
                  </span>
                  <button
                    onClick={() => setShowNewCommandDialog(true)}
                    className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
                  >
                    <Plus className="h-3.5 w-3.5 text-white/60" />
                  </button>
                </div>
                <div className="flex-1 overflow-y-auto p-2">
                  {commands.length === 0 ? (
                    <p className="text-xs text-white/40 text-center py-4">No commands yet</p>
                  ) : (
                    commands.map((command) => (
                      <button
                        key={command.name}
                        onClick={() => loadCommand(command.name)}
                        className={cn(
                          'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                          selectedCommand?.name === command.name
                            ? 'bg-white/[0.08] text-white'
                            : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                        )}
                      >
                        <p className="text-sm font-medium truncate">/{command.name}</p>
                        {command.description && (
                          <p className="text-xs text-white/40 truncate">{command.description}</p>
                        )}
                      </button>
                    ))
                  )}
                </div>
              </div>

              {/* Commands Editor */}
              <div className="flex-1 flex flex-col">
                {selectedCommand ? (
                  <>
                    <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                      <div className="min-w-0">
                        <p className="text-sm font-medium text-white truncate">
                          /{selectedCommand.name}
                        </p>
                        <p className="text-xs text-white/40">{selectedCommand.path}</p>
                      </div>
                      <div className="flex items-center gap-2">
                        {commandDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                        <button
                          onClick={handleCommandDelete}
                          className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </button>
                        <button
                          onClick={handleCommandSave}
                          disabled={commandSaving || !commandDirty}
                          className={cn(
                            'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                            commandDirty
                              ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                              : 'text-white/40 bg-white/[0.04]'
                          )}
                        >
                          <Save className={cn('h-3 w-3', commandSaving && 'animate-pulse')} />
                          Save
                        </button>
                      </div>
                    </div>
                    <div className="flex-1 p-3 overflow-hidden">
                      {loadingCommand ? (
                        <div className="flex items-center justify-center h-full">
                          <Loader className="h-5 w-5 animate-spin text-white/40" />
                        </div>
                      ) : (
                        <textarea
                          value={commandContent}
                          onChange={(e) => {
                            setCommandContent(e.target.value);
                            setCommandDirty(true);
                          }}
                          className="w-full h-full font-mono text-sm bg-[#0d0d0e] border border-white/[0.06] rounded-lg p-4 text-white/90 resize-none focus:outline-none focus:border-indigo-500/50"
                          spellCheck={false}
                        />
                      )}
                    </div>
                  </>
                ) : (
                  <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
                    Select a command to edit
                  </div>
                )}
              </div>
            </div>
          </div>
        </>
      )}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Commit Changes</h3>
            <input
              type="text"
              placeholder="Commit message..."
              value={commitMessage}
              onChange={(e) => setCommitMessage(e.target.value)}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowCommitDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCommit}
                disabled={!commitMessage.trim() || committing}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {committing ? 'Committing...' : 'Commit'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New Command Dialog */}
      {showNewCommandDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Command</h3>
            <input
              type="text"
              placeholder="Command name (e.g., my-command)"
              value={newCommandName}
              onChange={(e) => setNewCommandName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowNewCommandDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCommandCreate}
                disabled={!newCommandName.trim() || commandSaving}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {commandSaving ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
