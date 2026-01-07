'use client';

import { useEffect, useState } from 'react';
import {
  listWorkspaces,
  getWorkspace,
  createWorkspace,
  deleteWorkspace,
  type Workspace,
} from '@/lib/api';
import {
  Plus,
  Trash2,
  X,
  Loader,
  AlertCircle,
  Server,
  FolderOpen,
  Clock,
} from 'lucide-react';
import { cn } from '@/lib/utils';

export default function WorkspacesPage() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [showNewWorkspaceDialog, setShowNewWorkspaceDialog] = useState(false);
  const [newWorkspaceName, setNewWorkspaceName] = useState('');
  const [newWorkspaceType, setNewWorkspaceType] = useState<'host' | 'chroot'>('chroot');

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const workspacesData = await listWorkspaces();
      setWorkspaces(workspacesData);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load workspaces');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  // Handle Escape key for modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (selectedWorkspace) setSelectedWorkspace(null);
        if (showNewWorkspaceDialog) setShowNewWorkspaceDialog(false);
      }
    };
    if (selectedWorkspace || showNewWorkspaceDialog) {
      document.addEventListener('keydown', handleKeyDown);
      return () => document.removeEventListener('keydown', handleKeyDown);
    }
  }, [selectedWorkspace, showNewWorkspaceDialog]);

  const loadWorkspace = async (id: string) => {
    try {
      const workspace = await getWorkspace(id);
      setSelectedWorkspace(workspace);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace');
    }
  };

  const handleCreateWorkspace = async () => {
    if (!newWorkspaceName.trim()) return;
    try {
      setCreating(true);
      await createWorkspace({
        name: newWorkspaceName,
        workspace_type: newWorkspaceType,
      });
      await loadData();
      setShowNewWorkspaceDialog(false);
      setNewWorkspaceName('');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create workspace');
    } finally {
      setCreating(false);
    }
  };

  const handleDeleteWorkspace = async (id: string, name: string) => {
    if (!confirm(`Delete workspace "${name}"?`)) return;
    try {
      await deleteWorkspace(id);
      setSelectedWorkspace(null);
      await loadData();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete workspace');
    }
  };

  const formatDate = (dateStr: string) => {
    const date = new Date(dateStr);
    return date.toLocaleDateString() + ' ' + date.toLocaleTimeString();
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-7xl mx-auto space-y-4">
      {error && (
        <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
          <button onClick={() => setError(null)} className="ml-auto">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold text-white">Workspaces</h1>
          <p className="text-sm text-white/60 mt-1">
            Isolated execution environments for running missions
          </p>
        </div>
        <button
          onClick={() => setShowNewWorkspaceDialog(true)}
          className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
        >
          <Plus className="h-4 w-4" />
          New Workspace
        </button>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {workspaces.length === 0 ? (
          <div className="col-span-full p-12 text-center">
            <Server className="h-12 w-12 text-white/20 mx-auto mb-4" />
            <p className="text-white/40">No workspaces yet</p>
            <p className="text-sm text-white/30 mt-1">Create a workspace to get started</p>
          </div>
        ) : (
          workspaces.map((workspace) => (
            <div
              key={workspace.id}
              className="p-4 rounded-xl bg-white/[0.02] border border-white/[0.06] hover:border-white/[0.12] transition-colors cursor-pointer"
              onClick={() => loadWorkspace(workspace.id)}
            >
              <div className="flex items-start justify-between mb-3">
                <div className="flex items-center gap-2">
                  <Server className="h-5 w-5 text-indigo-400" />
                  <h3 className="text-sm font-medium text-white">{workspace.name}</h3>
                </div>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDeleteWorkspace(workspace.id, workspace.name);
                  }}
                  className="p-1 rounded-lg text-red-400 hover:bg-red-500/10 transition-colors"
                  title="Delete workspace"
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>

              <div className="space-y-2">
                <div className="flex items-center gap-2 text-xs text-white/60">
                  <span className="px-2 py-0.5 rounded bg-white/[0.04] border border-white/[0.08] font-mono">
                    {workspace.workspace_type}
                  </span>
                  <span
                    className={cn(
                      'px-2 py-0.5 rounded text-xs font-medium',
                      workspace.status === 'ready'
                        ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                        : workspace.status === 'building' || workspace.status === 'pending'
                        ? 'bg-amber-500/10 text-amber-400 border border-amber-500/20'
                        : 'bg-red-500/10 text-red-400 border border-red-500/20'
                    )}
                  >
                    {workspace.status}
                  </span>
                </div>

                <div className="flex items-center gap-2 text-xs text-white/40">
                  <FolderOpen className="h-3.5 w-3.5" />
                  <span className="truncate font-mono">{workspace.path}</span>
                </div>

                <div className="flex items-center gap-2 text-xs text-white/40">
                  <Clock className="h-3.5 w-3.5" />
                  <span>Created {formatDate(workspace.created_at)}</span>
                </div>

                {workspace.error_message && (
                  <div className="text-xs text-red-400 mt-2">
                    Error: {workspace.error_message}
                  </div>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {/* Workspace Details Modal */}
      {selectedWorkspace && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
          onClick={() => setSelectedWorkspace(null)}
        >
          <div
            className="w-full max-w-2xl p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between mb-4">
              <div className="flex items-center gap-3">
                <Server className="h-6 w-6 text-indigo-400" />
                <div>
                  <h3 className="text-lg font-medium text-white">{selectedWorkspace.name}</h3>
                  <p className="text-sm text-white/60">Workspace Details</p>
                </div>
              </div>
              <button
                onClick={() => setSelectedWorkspace(null)}
                className="p-2 rounded-lg hover:bg-white/[0.04] transition-colors"
              >
                <X className="h-4 w-4 text-white/60" />
              </button>
            </div>

            <div className="space-y-4">
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <label className="text-xs text-white/40 block mb-1">Type</label>
                  <span className="px-2 py-1 rounded bg-white/[0.04] border border-white/[0.08] font-mono text-sm text-white">
                    {selectedWorkspace.workspace_type}
                  </span>
                </div>
                <div>
                  <label className="text-xs text-white/40 block mb-1">Status</label>
                  <span
                    className={cn(
                      'inline-block px-2 py-1 rounded text-sm font-medium',
                      selectedWorkspace.status === 'ready'
                        ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                        : selectedWorkspace.status === 'building' || selectedWorkspace.status === 'pending'
                        ? 'bg-amber-500/10 text-amber-400 border border-amber-500/20'
                        : 'bg-red-500/10 text-red-400 border border-red-500/20'
                    )}
                  >
                    {selectedWorkspace.status}
                  </span>
                </div>
              </div>

              <div>
                <label className="text-xs text-white/40 block mb-1">Path</label>
                <code className="block px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/80 font-mono break-all">
                  {selectedWorkspace.path}
                </code>
              </div>

              <div>
                <label className="text-xs text-white/40 block mb-1">ID</label>
                <code className="block px-3 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-sm text-white/80 font-mono">
                  {selectedWorkspace.id}
                </code>
              </div>

              <div>
                <label className="text-xs text-white/40 block mb-1">Created</label>
                <span className="text-sm text-white/80">
                  {formatDate(selectedWorkspace.created_at)}
                </span>
              </div>

              {selectedWorkspace.error_message && (
                <div>
                  <label className="text-xs text-white/40 block mb-1">Error</label>
                  <div className="px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/20 text-sm text-red-400">
                    {selectedWorkspace.error_message}
                  </div>
                </div>
              )}
            </div>

            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => setSelectedWorkspace(null)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Close
              </button>
              <button
                onClick={() => {
                  handleDeleteWorkspace(selectedWorkspace.id, selectedWorkspace.name);
                  setSelectedWorkspace(null);
                }}
                className="px-4 py-2 text-sm font-medium text-white bg-red-500 hover:bg-red-600 rounded-lg"
              >
                Delete Workspace
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New Workspace Dialog */}
      {showNewWorkspaceDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">New Workspace</h3>
            <div className="space-y-4">
              <div>
                <label className="text-xs text-white/60 mb-1 block">Name</label>
                <input
                  type="text"
                  placeholder="my-workspace"
                  value={newWorkspaceName}
                  onChange={(e) => setNewWorkspaceName(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="text-xs text-white/60 mb-1 block">Type</label>
                <select
                  value={newWorkspaceType}
                  onChange={(e) => setNewWorkspaceType(e.target.value as 'host' | 'chroot')}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="host">Host (uses main filesystem)</option>
                  <option value="chroot">Chroot (isolated environment)</option>
                </select>
                <p className="text-xs text-white/40 mt-1.5">
                  {newWorkspaceType === 'host'
                    ? 'Runs directly on the host machine filesystem'
                    : 'Creates an isolated chroot environment for better security'}
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => setShowNewWorkspaceDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleCreateWorkspace}
                disabled={!newWorkspaceName.trim() || creating}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {creating ? 'Creating...' : 'Create'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
