'use client';

import { useState } from 'react';
import {
  Plus,
  Save,
  Trash2,
  X,
  Loader,
  AlertCircle,
  Users,
  GitBranch,
  RefreshCw,
  Check,
  Upload,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';

export default function AgentsPage() {
  const {
    status,
    libraryAgents,
    loading,
    error,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    clearError,
    getLibraryAgent,
    saveLibraryAgent,
    removeLibraryAgent,
    sync,
    commit,
    push,
  } = useLibrary();

  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const [agentContent, setAgentContent] = useState('');
  const [loadingAgent, setLoadingAgent] = useState(false);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [showNewAgentDialog, setShowNewAgentDialog] = useState(false);
  const [newAgentName, setNewAgentName] = useState('');
  const [newAgentError, setNewAgentError] = useState<string | null>(null);

  // Git operations state
  const [syncing, setSyncing] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [pushing, setPushing] = useState(false);
  const [showCommitDialog, setShowCommitDialog] = useState(false);
  const [commitMessage, setCommitMessage] = useState('');

  const loadAgent = async (name: string) => {
    try {
      setLoadingAgent(true);
      const agent = await getLibraryAgent(name);
      setSelectedAgent(name);
      setAgentContent(agent.content);
      setDirty(false);
    } catch (err) {
      console.error('Failed to load agent:', err);
    } finally {
      setLoadingAgent(false);
    }
  };

  const handleSaveAgent = async () => {
    if (!selectedAgent) return;
    setSaving(true);
    try {
      await saveLibraryAgent(selectedAgent, agentContent);
      setDirty(false);
    } catch (err) {
      console.error('Failed to save agent:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreateAgent = async () => {
    const name = newAgentName.trim();
    if (!name) {
      setNewAgentError('Please enter a name');
      return;
    }
    if (!/^[a-z0-9-]+$/.test(name)) {
      setNewAgentError('Name must be lowercase alphanumeric with hyphens');
      return;
    }

    const template = `---
model: claude-sonnet-4-20250514
tools:
  - Read
  - Edit
  - Bash
---

# ${name}

Agent instructions here.
`;
    try {
      setSaving(true);
      await saveLibraryAgent(name, template);
      setShowNewAgentDialog(false);
      setNewAgentName('');
      setNewAgentError(null);
      await loadAgent(name);
    } catch (err) {
      setNewAgentError(err instanceof Error ? err.message : 'Failed to create agent');
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteAgent = async () => {
    if (!selectedAgent) return;
    if (!confirm(`Delete agent "${selectedAgent}"?`)) return;

    try {
      await removeLibraryAgent(selectedAgent);
      setSelectedAgent(null);
      setAgentContent('');
    } catch (err) {
      console.error('Failed to delete agent:', err);
    }
  };

  const handleSync = async () => {
    setSyncing(true);
    try {
      await sync();
    } finally {
      setSyncing(false);
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    setCommitting(true);
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } finally {
      setCommitting(false);
    }
  };

  const handlePush = async () => {
    setPushing(true);
    try {
      await push();
    } finally {
      setPushing(false);
    }
  };

  // Handle Escape key
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      if (showNewAgentDialog) setShowNewAgentDialog(false);
      if (showCommitDialog) setShowCommitDialog(false);
    }
  };

  if (loading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
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
    <div className="min-h-screen flex flex-col p-6 max-w-7xl mx-auto space-y-4" onKeyDown={handleKeyDown}>
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
          <button onClick={clearError} className="ml-auto">
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

      <div className="flex-1 min-h-0 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden flex">
        {/* Agent List */}
        <div className="w-64 border-r border-white/[0.06] flex flex-col min-h-0">
          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
            <span className="text-xs font-medium text-white/60">
              Agents{libraryAgents.length ? ` (${libraryAgents.length})` : ''}
            </span>
            <button
              onClick={() => setShowNewAgentDialog(true)}
              className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
              title="New Agent"
            >
              <Plus className="h-3.5 w-3.5 text-white/60" />
            </button>
          </div>
          <div className="flex-1 min-h-0 overflow-y-auto p-2">
            {libraryAgents.length === 0 ? (
              <div className="text-center py-8">
                <Users className="h-8 w-8 text-white/20 mx-auto mb-3" />
                <p className="text-xs text-white/40 mb-3">No agents yet</p>
                <button
                  onClick={() => setShowNewAgentDialog(true)}
                  className="text-xs text-indigo-400 hover:text-indigo-300"
                >
                  Create your first agent
                </button>
              </div>
            ) : (
              libraryAgents.map((agent) => (
                <button
                  key={agent.name}
                  onClick={() => loadAgent(agent.name)}
                  className={cn(
                    'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                    selectedAgent === agent.name
                      ? 'bg-white/[0.08] text-white'
                      : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                  )}
                >
                  <p className="text-sm font-medium truncate">{agent.name}</p>
                  {agent.description && (
                    <p className="text-xs text-white/40 truncate">{agent.description}</p>
                  )}
                </button>
              ))
            )}
          </div>
        </div>

        {/* Agent Editor */}
        <div className="flex-1 min-h-0 flex flex-col">
          {selectedAgent ? (
            <>
              <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-white truncate">{selectedAgent}</p>
                  <p className="text-xs text-white/40">agent/{selectedAgent}.md</p>
                </div>
                <div className="flex items-center gap-2">
                  {dirty && <span className="text-xs text-amber-400">Unsaved</span>}
                  <button
                    onClick={handleDeleteAgent}
                    className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                    title="Delete Agent"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                  <button
                    onClick={handleSaveAgent}
                    disabled={saving || !dirty}
                    className={cn(
                      'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                      dirty
                        ? 'text-white bg-indigo-500 hover:bg-indigo-600'
                        : 'text-white/40 bg-white/[0.04]'
                    )}
                  >
                    <Save className={cn('h-3 w-3', saving && 'animate-pulse')} />
                    Save
                  </button>
                </div>
              </div>

              <div className="flex-1 min-h-0 overflow-y-auto p-3">
                {loadingAgent ? (
                  <div className="flex items-center justify-center h-full">
                    <Loader className="h-5 w-5 animate-spin text-white/40" />
                  </div>
                ) : (
                  <textarea
                    value={agentContent}
                    onChange={(e) => {
                      setAgentContent(e.target.value);
                      setDirty(true);
                    }}
                    className="w-full h-full font-mono text-sm bg-[#0d0d0e] border border-white/[0.06] rounded-lg p-4 text-white/90 resize-none focus:outline-none focus:border-indigo-500/50"
                    spellCheck={false}
                    disabled={saving}
                  />
                )}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-white/40 text-sm">
              Select an agent to edit or create a new one
            </div>
          )}
        </div>
      </div>

      {/* New Agent Dialog */}
      {showNewAgentDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Agent</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1.5">Agent Name</label>
                <input
                  type="text"
                  placeholder="code-reviewer"
                  value={newAgentName}
                  onChange={(e) => {
                    setNewAgentName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'));
                    setNewAgentError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                  autoFocus
                />
                <p className="text-xs text-white/40 mt-1">
                  Lowercase alphanumeric with hyphens (e.g., code-reviewer)
                </p>
              </div>
              {newAgentError && <p className="text-sm text-red-400">{newAgentError}</p>}
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => {
                    setShowNewAgentDialog(false);
                    setNewAgentName('');
                    setNewAgentError(null);
                  }}
                  className="px-4 py-2 text-sm text-white/60 hover:text-white"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreateAgent}
                  disabled={!newAgentName.trim() || saving}
                  className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
                >
                  {saving ? 'Creating...' : 'Create'}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Commit Dialog */}
      {showCommitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Commit Changes</h3>
            <textarea
              className="w-full h-24 px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 resize-none"
              placeholder="Commit message..."
              value={commitMessage}
              onChange={(e) => setCommitMessage(e.target.value)}
              autoFocus
            />
            <div className="flex justify-end gap-2 mt-4">
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
    </div>
  );
}
