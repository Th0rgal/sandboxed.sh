'use client';

import { useState, useMemo, useEffect } from 'react';
import { toast } from 'sonner';
import { type Plugin } from '@/lib/api';
import {
  AlertCircle,
  Check,
  GitBranch,
  Loader,
  Plus,
  RefreshCw,
  Search,
  Trash2,
  Upload,
  X,
  Plug,
  Settings,
  ToggleLeft,
  ToggleRight,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { useLibrary } from '@/contexts/library-context';

type PluginEntry = {
  id: string;
  plugin: Plugin;
};

type PluginFormState = {
  id: string;
  package: string;
  description: string;
  enabled: boolean;
  ui: {
    icon: string;
    label: string;
    hint: string;
    category: string;
  };
};

function buildFormState(entry?: PluginEntry): PluginFormState {
  if (!entry) {
    return {
      id: '',
      package: '',
      description: '',
      enabled: true,
      ui: {
        icon: '',
        label: '',
        hint: '',
        category: '',
      },
    };
  }

  return {
    id: entry.id,
    package: entry.plugin.package,
    description: entry.plugin.description ?? '',
    enabled: entry.plugin.enabled ?? true,
    ui: {
      icon: entry.plugin.ui?.icon ?? '',
      label: entry.plugin.ui?.label ?? '',
      hint: entry.plugin.ui?.hint ?? '',
      category: entry.plugin.ui?.category ?? '',
    },
  };
}

function PluginCard({
  entry,
  isSelected,
  onSelect,
  onToggle,
}: {
  entry: PluginEntry;
  isSelected: boolean;
  onSelect: (entry: PluginEntry | null) => void;
  onToggle: (id: string, enabled: boolean) => void;
}) {
  const handleSelect = () => onSelect(isSelected ? null : entry);
  const handleToggle = (e: React.MouseEvent) => {
    e.stopPropagation();
    onToggle(entry.id, !entry.plugin.enabled);
  };

  return (
    <div
      role="button"
      tabIndex={0}
      aria-pressed={isSelected}
      onClick={handleSelect}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          handleSelect();
        }
      }}
      className={cn(
        'w-full rounded-xl p-4 text-left transition-all cursor-pointer',
        'bg-white/[0.02] border hover:bg-white/[0.04] focus:outline-none focus:ring-1 focus:ring-indigo-500/40',
        isSelected
          ? 'border-indigo-500/50 ring-1 ring-indigo-500/30'
          : 'border-white/[0.04] hover:border-white/[0.08]'
      )}
    >
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
          <Plug className="h-5 w-5 text-indigo-400" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="font-medium text-white truncate">{entry.plugin.ui?.label || entry.id}</h3>
            {entry.plugin.ui?.category && (
              <span className="tag">{entry.plugin.ui.category}</span>
            )}
          </div>
          <p className="text-xs text-white/40 truncate">
            {entry.plugin.package || 'No package specified'}
          </p>
        </div>
        <button
          onClick={handleToggle}
          className="flex-shrink-0 p-1 rounded hover:bg-white/[0.08] transition-colors"
          title={entry.plugin.enabled ? 'Disable plugin' : 'Enable plugin'}
        >
          {entry.plugin.enabled ? (
            <ToggleRight className="h-6 w-6 text-emerald-400" />
          ) : (
            <ToggleLeft className="h-6 w-6 text-white/30" />
          )}
        </button>
      </div>

      {entry.plugin.description && (
        <p className="text-xs text-white/50 mb-3 line-clamp-2">{entry.plugin.description}</p>
      )}

      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <span className="text-[10px] text-white/30">
          {entry.plugin.ui?.hint || 'Plugin'}
        </span>
        <span className={cn(
          'text-[10px]',
          entry.plugin.enabled ? 'text-emerald-400' : 'text-white/30'
        )}>
          {entry.plugin.enabled ? 'Enabled' : 'Disabled'}
        </span>
      </div>
    </div>
  );
}

function PluginDetailPanel({
  entry,
  onClose,
  onEdit,
  onDelete,
}: {
  entry: PluginEntry;
  onClose: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  return (
    <>
      <div
        className="fixed inset-0 z-40 bg-black/40 backdrop-blur-sm animate-fade-in"
        onClick={onClose}
      />
      <div
        className="fixed right-0 top-0 z-50 h-full w-96 flex flex-col glass-panel border-l border-white/[0.06] animate-slide-in-right"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between border-b border-white/[0.06] p-4">
          <div>
            <div className="flex items-center gap-2">
              <h2 className="text-lg font-semibold text-white">{entry.plugin.ui?.label || entry.id}</h2>
              <span className={cn(
                'tag',
                entry.plugin.enabled ? 'text-emerald-400 border-emerald-400/20' : ''
              )}>
                {entry.plugin.enabled ? 'Enabled' : 'Disabled'}
              </span>
            </div>
            <p className="text-xs text-white/40 mt-1">plugins.json</p>
          </div>
          <button
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
            <p className="text-xs text-white/40 mb-2">Plugin ID</p>
            <p className="text-sm text-white">{entry.id}</p>
          </div>

          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
            <p className="text-xs text-white/40 mb-2">Package</p>
            <p className="text-sm text-white">{entry.plugin.package || 'Not specified'}</p>
          </div>

          {entry.plugin.description && (
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
              <p className="text-xs text-white/40 mb-2">Description</p>
              <p className="text-sm text-white/70">{entry.plugin.description}</p>
            </div>
          )}

          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
            <p className="text-xs text-white/40 mb-2">UI Metadata</p>
            <div className="space-y-2 text-sm">
              <div className="flex justify-between">
                <span className="text-white/50">Icon</span>
                <span className="text-white">{entry.plugin.ui?.icon || '-'}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-white/50">Label</span>
                <span className="text-white">{entry.plugin.ui?.label || '-'}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-white/50">Hint</span>
                <span className="text-white">{entry.plugin.ui?.hint || '-'}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-white/50">Category</span>
                <span className="text-white">{entry.plugin.ui?.category || '-'}</span>
              </div>
            </div>
          </div>
        </div>

        <div className="border-t border-white/[0.06] p-4 flex items-center gap-2">
          <button
            onClick={onEdit}
            className="flex-1 flex items-center justify-center gap-2 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-3 py-2 text-sm text-white/80 transition-colors"
          >
            <Settings className="h-4 w-4" />
            Edit
          </button>
          <button
            onClick={onDelete}
            className="flex items-center justify-center rounded-lg bg-red-500/10 hover:bg-red-500/20 border border-red-500/20 px-3 py-2 text-sm text-red-300 transition-colors"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        </div>
      </div>
    </>
  );
}

function PluginFormModal({
  open,
  title,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  title: string;
  initial?: PluginEntry;
  onClose: () => void;
  onSave: (id: string, plugin: Plugin) => Promise<void>;
}) {
  const [form, setForm] = useState<PluginFormState>(() => buildFormState(initial));
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setForm(buildFormState(initial));
    setError(null);
    setLoading(false);
  }, [open, initial]);

  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const updateForm = (updates: Partial<PluginFormState>) => {
    setForm((prev) => ({ ...prev, ...updates }));
  };

  const updateUI = (updates: Partial<PluginFormState['ui']>) => {
    setForm((prev) => ({ ...prev, ui: { ...prev.ui, ...updates } }));
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);

    const id = form.id.trim();
    if (!id) {
      setError('Plugin ID is required');
      return;
    }

    if (!form.ui.label.trim()) {
      setError('Display label is required');
      return;
    }

    const plugin: Plugin = {
      package: form.package.trim(),
      description: form.description.trim() || null,
      enabled: form.enabled,
      ui: {
        icon: form.ui.icon.trim() || null,
        label: form.ui.label.trim(),
        hint: form.ui.hint.trim() || null,
        category: form.ui.category.trim() || null,
      },
    };

    setLoading(true);
    try {
      await onSave(id, plugin);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save plugin');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-fade-in">
      <div className="w-full max-w-md rounded-2xl glass-panel border border-white/[0.08] p-6 animate-slide-up max-h-[90vh] overflow-y-auto">
        <div className="mb-6 flex items-center justify-between">
          <h2 className="text-lg font-semibold text-white">{title}</h2>
          <button
            onClick={onClose}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-white/50 hover:bg-white/[0.04] hover:text-white transition-colors"
          >
            <X className="h-5 w-5" />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="space-y-4">
            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">Plugin ID</label>
              <input
                type="text"
                value={form.id}
                onChange={(e) => updateForm({ id: e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-') })}
                placeholder="e.g., ralph-wiggum"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
                disabled={!!initial}
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">Package Name</label>
              <input
                type="text"
                value={form.package}
                onChange={(e) => updateForm({ package: e.target.value })}
                placeholder="e.g., @opencode/ralph-wiggum"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">Description</label>
              <textarea
                value={form.description}
                onChange={(e) => updateForm({ description: e.target.value })}
                placeholder="What this plugin does..."
                rows={2}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
              />
            </div>

            <div className="pt-2 border-t border-white/[0.06]">
              <p className="text-xs font-medium text-white/60 mb-3">UI Display Settings</p>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="block text-xs text-white/40 mb-1">Icon (Lucide)</label>
                  <input
                    type="text"
                    value={form.ui.icon}
                    onChange={(e) => updateUI({ icon: e.target.value })}
                    placeholder="zap"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                  />
                </div>
                <div>
                  <label className="block text-xs text-white/40 mb-1">Label *</label>
                  <input
                    type="text"
                    value={form.ui.label}
                    onChange={(e) => updateUI({ label: e.target.value })}
                    placeholder="Ralph Wiggum"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                    required
                  />
                </div>
                <div>
                  <label className="block text-xs text-white/40 mb-1">Hint</label>
                  <input
                    type="text"
                    value={form.ui.hint}
                    onChange={(e) => updateUI({ hint: e.target.value })}
                    placeholder="continuous running"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                  />
                </div>
                <div>
                  <label className="block text-xs text-white/40 mb-1">Category</label>
                  <input
                    type="text"
                    value={form.ui.category}
                    onChange={(e) => updateUI({ category: e.target.value })}
                    placeholder="automation"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                  />
                </div>
              </div>
            </div>

            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="enabled"
                checked={form.enabled}
                onChange={(e) => updateForm({ enabled: e.target.checked })}
                className="rounded border-white/20 bg-white/5 text-indigo-500"
              />
              <label htmlFor="enabled" className="text-sm text-white/70">
                Enable plugin by default
              </label>
            </div>

            {error && (
              <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3">
                <p className="text-sm text-red-400">{error}</p>
              </div>
            )}
          </div>

          <div className="mt-6 flex justify-end gap-3">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-4 py-2.5 text-sm text-white/80 transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={loading}
              className="rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2.5 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {loading ? 'Saving...' : 'Save Plugin'}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

export default function PluginsPage() {
  const {
    status,
    plugins,
    loading,
    error,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    sync,
    commit,
    push,
    clearError,
    savePlugins,
    syncing,
    committing,
    pushing,
  } = useLibrary();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [showAddModal, setShowAddModal] = useState(false);
  const [showEditModal, setShowEditModal] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<PluginEntry | null>(null);
  const [commitMessage, setCommitMessage] = useState('');
  const [showCommitDialog, setShowCommitDialog] = useState(false);

  const entries = useMemo<PluginEntry[]>(() => {
    return Object.entries(plugins)
      .map(([id, plugin]) => ({ id, plugin }))
      .sort((a, b) => a.id.localeCompare(b.id));
  }, [plugins]);

  const selectedEntry = useMemo(
    () => entries.find((entry) => entry.id === selectedId) ?? null,
    [entries, selectedId]
  );

  const filteredEntries = useMemo(() => {
    if (!searchQuery.trim()) return entries;
    const query = searchQuery.toLowerCase();
    return entries.filter((entry) => {
      return (
        entry.id.toLowerCase().includes(query) ||
        entry.plugin.package.toLowerCase().includes(query) ||
        (entry.plugin.ui?.label ?? '').toLowerCase().includes(query) ||
        (entry.plugin.description ?? '').toLowerCase().includes(query)
      );
    });
  }, [entries, searchQuery]);

  useEffect(() => {
    if (selectedId && !plugins[selectedId]) {
      setSelectedId(null);
    }
  }, [plugins, selectedId]);

  useEffect(() => {
    if (!showCommitDialog) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setShowCommitDialog(false);
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showCommitDialog]);

  const handleSync = async () => {
    try {
      await sync();
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

  const handleTogglePlugin = async (id: string, enabled: boolean) => {
    const current = plugins[id];
    if (!current) return;

    const next = {
      ...plugins,
      [id]: { ...current, enabled },
    };
    await savePlugins(next);
    toast.success(`${enabled ? 'Enabled' : 'Disabled'} ${current.ui?.label || id}`);
  };

  const handleAddPlugin = async (id: string, plugin: Plugin) => {
    if (plugins[id]) {
      throw new Error(`Plugin "${id}" already exists`);
    }
    const next = { ...plugins, [id]: plugin };
    await savePlugins(next);
    setSelectedId(id);
    toast.success(`Added ${plugin.ui?.label || id}`);
  };

  const handleUpdatePlugin = async (id: string, plugin: Plugin) => {
    if (!selectedEntry) return;
    const next = { ...plugins };
    if (id !== selectedEntry.id) {
      delete next[selectedEntry.id];
    }
    next[id] = plugin;
    await savePlugins(next);
    setSelectedId(id);
    toast.success(`Saved ${plugin.ui?.label || id}`);
  };

  const requestDelete = (entry: PluginEntry) => {
    setPendingDelete(entry);
    setShowDeleteConfirm(true);
  };

  const handleDelete = async () => {
    if (!pendingDelete) return;
    try {
      const next = { ...plugins };
      delete next[pendingDelete.id];
      await savePlugins(next);
      toast.success(`Removed ${pendingDelete.plugin.ui?.label || pendingDelete.id}`);
      if (selectedId === pendingDelete.id) {
        setSelectedId(null);
      }
    } catch {
      toast.error(`Failed to remove ${pendingDelete.id}`);
    } finally {
      setShowDeleteConfirm(false);
      setPendingDelete(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-6xl mx-auto space-y-4">
      {libraryUnavailable ? (
        <LibraryUnavailable message={libraryUnavailableMessage} onConfigured={refresh} />
      ) : (
        <>
          {error && (
            <div className="p-4 rounded-lg bg-red-500/10 border border-red-500/20 text-red-400 flex items-center gap-2">
              <AlertCircle className="h-4 w-4 flex-shrink-0" />
              {error}
              <button onClick={clearError} className="ml-auto">
                <X className="h-4 w-4" />
              </button>
            </div>
          )}

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

          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <h1 className="text-xl font-semibold text-white">Plugins</h1>
              <p className="text-sm text-white/40">Manage OpenCode plugins stored in your library repo.</p>
            </div>
            <button
              onClick={() => setShowAddModal(true)}
              className="flex items-center gap-2 rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2 text-sm font-medium text-white transition-colors"
            >
              <Plus className="h-4 w-4" />
              Add Plugin
            </button>
          </div>

          <div className="relative">
            <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search plugins..."
              className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] pl-10 pr-4 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
            />
          </div>

          {filteredEntries.length === 0 ? (
            <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-8 text-center">
              <p className="text-sm text-white/40">
                {entries.length === 0
                  ? 'No plugins configured yet. Add your first plugin to get started.'
                  : 'No plugins match your search.'}
              </p>
            </div>
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              {filteredEntries.map((entry) => (
                <PluginCard
                  key={entry.id}
                  entry={entry}
                  isSelected={selectedId === entry.id}
                  onSelect={(next) => setSelectedId(next?.id ?? null)}
                  onToggle={handleTogglePlugin}
                />
              ))}
            </div>
          )}

          {selectedEntry && (
            <PluginDetailPanel
              entry={selectedEntry}
              onClose={() => setSelectedId(null)}
              onEdit={() => setShowEditModal(true)}
              onDelete={() => requestDelete(selectedEntry)}
            />
          )}

          <PluginFormModal
            open={showAddModal}
            title="Add Plugin"
            onClose={() => setShowAddModal(false)}
            onSave={handleAddPlugin}
          />

          <PluginFormModal
            open={showEditModal}
            title={selectedEntry ? `Edit ${selectedEntry.plugin.ui?.label || selectedEntry.id}` : 'Edit Plugin'}
            initial={selectedEntry ?? undefined}
            onClose={() => setShowEditModal(false)}
            onSave={handleUpdatePlugin}
          />

          <ConfirmDialog
            open={showDeleteConfirm}
            title={`Remove ${pendingDelete?.plugin.ui?.label || pendingDelete?.id}?`}
            description="This will remove the plugin from your library repo. This action cannot be undone."
            confirmLabel="Remove Plugin"
            variant="danger"
            onConfirm={handleDelete}
            onCancel={() => {
              setShowDeleteConfirm(false);
              setPendingDelete(null);
            }}
          />

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
        </>
      )}
    </div>
  );
}
