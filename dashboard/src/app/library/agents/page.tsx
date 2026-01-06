'use client';

import { useState } from 'react';
import { Loader, Plus, Save, Trash2, X, AlertCircle, Users } from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';

export default function LibraryAgentsPage() {
  const {
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
  } = useLibrary();

  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const [agentContent, setAgentContent] = useState('');
  const [loadingAgent, setLoadingAgent] = useState(false);
  const [saving, setSaving] = useState(false);
  const [isDirty, setIsDirty] = useState(false);
  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newAgentName, setNewAgentName] = useState('');
  const [newAgentError, setNewAgentError] = useState<string | null>(null);

  const loadAgent = async (name: string) => {
    try {
      setLoadingAgent(true);
      const agent = await getLibraryAgent(name);
      setSelectedAgent(name);
      setAgentContent(agent.content);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to load agent:', err);
    } finally {
      setLoadingAgent(false);
    }
  };

  const handleSave = async () => {
    if (!selectedAgent) return;
    setSaving(true);
    try {
      await saveLibraryAgent(selectedAgent, agentContent);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to save agent:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreate = async () => {
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
      setShowNewDialog(false);
      setNewAgentName('');
      setNewAgentError(null);
      await loadAgent(name);
    } catch (err) {
      setNewAgentError(err instanceof Error ? err.message : 'Failed to create agent');
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
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

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  if (libraryUnavailable) {
    return <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />;
  }

  return (
    <div className="min-h-screen flex flex-col p-6 max-w-7xl mx-auto space-y-4">
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
          <button onClick={clearError} className="ml-auto">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      <div className="flex-1 min-h-0 rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden flex">
        {/* Agents List */}
        <div className="w-64 border-r border-white/[0.06] flex flex-col min-h-0">
          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
            <span className="text-xs font-medium text-white/60">
              Library Agents{libraryAgents.length ? ` (${libraryAgents.length})` : ''}
            </span>
            <button
              onClick={() => setShowNewDialog(true)}
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
                <p className="text-xs text-white/40 mb-3">No library agents yet</p>
                <button
                  onClick={() => setShowNewDialog(true)}
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
                  {agent.model && (
                    <p className="text-xs text-indigo-400/60 truncate">{agent.model}</p>
                  )}
                </button>
              ))
            )}
          </div>
        </div>

        {/* Editor */}
        <div className="flex-1 min-h-0 flex flex-col">
          {selectedAgent ? (
            <>
              <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-white truncate">{selectedAgent}.md</p>
                  <p className="text-xs text-white/40">agent/{selectedAgent}.md</p>
                </div>
                <div className="flex items-center gap-2">
                  {isDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                  <button
                    onClick={handleDelete}
                    className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                    title="Delete Agent"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                  <button
                    onClick={handleSave}
                    disabled={saving || !isDirty}
                    className={cn(
                      'flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg transition-colors',
                      isDirty
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
                      setIsDirty(true);
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
      {showNewDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Library Agent</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1.5">Agent Name</label>
                <input
                  type="text"
                  placeholder="my-agent"
                  value={newAgentName}
                  onChange={(e) => {
                    setNewAgentName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'));
                    setNewAgentError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
                <p className="text-xs text-white/40 mt-1">
                  Lowercase alphanumeric with hyphens (e.g., code-reviewer)
                </p>
              </div>
              {newAgentError && <p className="text-sm text-red-400">{newAgentError}</p>}
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => {
                    setShowNewDialog(false);
                    setNewAgentName('');
                    setNewAgentError(null);
                  }}
                  className="px-4 py-2 text-sm text-white/60 hover:text-white"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreate}
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
    </div>
  );
}
