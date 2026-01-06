'use client';

import { useState } from 'react';
import { Loader, Plus, Save, Trash2, X, AlertCircle, Wrench } from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { useLibrary } from '@/contexts/library-context';

export default function LibraryToolsPage() {
  const {
    libraryTools,
    loading,
    error,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    clearError,
    getLibraryTool,
    saveLibraryTool,
    removeLibraryTool,
  } = useLibrary();

  const [selectedTool, setSelectedTool] = useState<string | null>(null);
  const [toolContent, setToolContent] = useState('');
  const [loadingTool, setLoadingTool] = useState(false);
  const [saving, setSaving] = useState(false);
  const [isDirty, setIsDirty] = useState(false);
  const [showNewDialog, setShowNewDialog] = useState(false);
  const [newToolName, setNewToolName] = useState('');
  const [newToolError, setNewToolError] = useState<string | null>(null);

  const loadTool = async (name: string) => {
    try {
      setLoadingTool(true);
      const tool = await getLibraryTool(name);
      setSelectedTool(name);
      setToolContent(tool.content);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to load tool:', err);
    } finally {
      setLoadingTool(false);
    }
  };

  const handleSave = async () => {
    if (!selectedTool) return;
    setSaving(true);
    try {
      await saveLibraryTool(selectedTool, toolContent);
      setIsDirty(false);
    } catch (err) {
      console.error('Failed to save tool:', err);
    } finally {
      setSaving(false);
    }
  };

  const handleCreate = async () => {
    const name = newToolName.trim();
    if (!name) {
      setNewToolError('Please enter a name');
      return;
    }
    if (!/^[a-z0-9-]+$/.test(name)) {
      setNewToolError('Name must be lowercase alphanumeric with hyphens');
      return;
    }

    const template = `---
description: A custom tool
parameters:
  - name: input
    type: string
    description: The input parameter
    required: true
---

# ${name}

Tool implementation instructions here.

## Usage

Describe how the agent should use this tool.

## Examples

\`\`\`
Example input and expected output
\`\`\`
`;

    try {
      setSaving(true);
      await saveLibraryTool(name, template);
      setShowNewDialog(false);
      setNewToolName('');
      setNewToolError(null);
      await loadTool(name);
    } catch (err) {
      setNewToolError(err instanceof Error ? err.message : 'Failed to create tool');
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!selectedTool) return;
    if (!confirm(`Delete tool "${selectedTool}"?`)) return;

    try {
      await removeLibraryTool(selectedTool);
      setSelectedTool(null);
      setToolContent('');
    } catch (err) {
      console.error('Failed to delete tool:', err);
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
        {/* Tools List */}
        <div className="w-64 border-r border-white/[0.06] flex flex-col min-h-0">
          <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
            <span className="text-xs font-medium text-white/60">
              Library Tools{libraryTools.length ? ` (${libraryTools.length})` : ''}
            </span>
            <button
              onClick={() => setShowNewDialog(true)}
              className="p-1.5 rounded-lg hover:bg-white/[0.06] transition-colors"
              title="New Tool"
            >
              <Plus className="h-3.5 w-3.5 text-white/60" />
            </button>
          </div>
          <div className="flex-1 min-h-0 overflow-y-auto p-2">
            {libraryTools.length === 0 ? (
              <div className="text-center py-8">
                <Wrench className="h-8 w-8 text-white/20 mx-auto mb-3" />
                <p className="text-xs text-white/40 mb-3">No library tools yet</p>
                <button
                  onClick={() => setShowNewDialog(true)}
                  className="text-xs text-indigo-400 hover:text-indigo-300"
                >
                  Create your first tool
                </button>
              </div>
            ) : (
              libraryTools.map((tool) => (
                <button
                  key={tool.name}
                  onClick={() => loadTool(tool.name)}
                  className={cn(
                    'w-full text-left p-2.5 rounded-lg transition-colors mb-1',
                    selectedTool === tool.name
                      ? 'bg-white/[0.08] text-white'
                      : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                  )}
                >
                  <p className="text-sm font-medium truncate">{tool.name}</p>
                  {tool.description && (
                    <p className="text-xs text-white/40 truncate">{tool.description}</p>
                  )}
                </button>
              ))
            )}
          </div>
        </div>

        {/* Editor */}
        <div className="flex-1 min-h-0 flex flex-col">
          {selectedTool ? (
            <>
              <div className="p-3 border-b border-white/[0.06] flex items-center justify-between">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-white truncate">{selectedTool}.md</p>
                  <p className="text-xs text-white/40">tool/{selectedTool}.md</p>
                </div>
                <div className="flex items-center gap-2">
                  {isDirty && <span className="text-xs text-amber-400">Unsaved</span>}
                  <button
                    onClick={handleDelete}
                    className="p-1.5 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                    title="Delete Tool"
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
                {loadingTool ? (
                  <div className="flex items-center justify-center h-full">
                    <Loader className="h-5 w-5 animate-spin text-white/40" />
                  </div>
                ) : (
                  <textarea
                    value={toolContent}
                    onChange={(e) => {
                      setToolContent(e.target.value);
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
              Select a tool to edit or create a new one
            </div>
          )}
        </div>
      </div>

      {/* New Tool Dialog */}
      {showNewDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Library Tool</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1.5">Tool Name</label>
                <input
                  type="text"
                  placeholder="my-tool"
                  value={newToolName}
                  onChange={(e) => {
                    setNewToolName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'));
                    setNewToolError(null);
                  }}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
                <p className="text-xs text-white/40 mt-1">
                  Lowercase alphanumeric with hyphens (e.g., fetch-data)
                </p>
              </div>
              {newToolError && <p className="text-sm text-red-400">{newToolError}</p>}
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => {
                    setShowNewDialog(false);
                    setNewToolName('');
                    setNewToolError(null);
                  }}
                  className="px-4 py-2 text-sm text-white/60 hover:text-white"
                >
                  Cancel
                </button>
                <button
                  onClick={handleCreate}
                  disabled={!newToolName.trim() || saving}
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
