'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';
import {
  getLibraryStatus,
  syncLibrary,
  commitLibrary,
  pushLibrary,
  getLibraryMcps,
  saveLibraryMcps,
  type LibraryStatus,
  type McpServerDef,
  LibraryUnavailableError,
} from '@/lib/api';
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
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { CopyButton } from '@/components/ui/copy-button';

type McpEntry = {
  name: string;
  def: McpServerDef;
};

type McpFormState = {
  name: string;
  type: McpServerDef['type'];
  url: string;
  command: string;
  args: string;
  env: string;
};

const typeLabels: Record<McpServerDef['type'], string> = {
  http: 'HTTP',
  stdio: 'STDIO',
};

function formatEndpoint(def: McpServerDef): string {
  if (def.type === 'http') return def.url ?? '';
  const parts = [def.command, ...(def.args ?? [])].filter(Boolean);
  return parts.join(' ');
}

function serializeArgs(args?: string[]): string {
  if (!args || args.length === 0) return '';
  return args.join(', ');
}

function parseArgs(value: string): string[] {
  return value
    .split(',')
    .map((arg) => arg.trim())
    .filter((arg) => arg.length > 0);
}

function serializeEnv(env?: Record<string, string>): string {
  if (!env || Object.keys(env).length === 0) return '';
  return Object.entries(env)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n');
}

function parseEnv(value: string): { env: Record<string, string>; error?: string } {
  const trimmed = value.trim();
  if (!trimmed) return { env: {} };
  const env: Record<string, string> = {};
  for (const rawLine of trimmed.split('\n')) {
    const line = rawLine.trim();
    if (!line) continue;
    const idx = line.indexOf('=');
    if (idx <= 0) {
      return { env: {}, error: `Invalid env line: "${rawLine}"` };
    }
    const key = line.slice(0, idx).trim();
    const val = line.slice(idx + 1).trim();
    if (!key) {
      return { env: {}, error: `Invalid env key in line: "${rawLine}"` };
    }
    env[key] = val;
  }
  return { env };
}

function buildFormState(entry?: McpEntry): McpFormState {
  if (!entry) {
    return {
      name: '',
      type: 'http',
      url: '',
      command: '',
      args: '',
      env: '',
    };
  }

  return {
    name: entry.name,
    type: entry.def.type,
    url: entry.def.type === 'http' ? entry.def.url ?? '' : '',
    command: entry.def.type === 'stdio' ? entry.def.command ?? '' : '',
    args: entry.def.type === 'stdio' ? serializeArgs(entry.def.args) : '',
    env: entry.def.type === 'stdio' ? serializeEnv(entry.def.env) : '',
  };
}

function McpCard({
  entry,
  isSelected,
  onSelect,
}: {
  entry: McpEntry;
  isSelected: boolean;
  onSelect: (entry: McpEntry | null) => void;
}) {
  const endpoint = formatEndpoint(entry.def);
  const args = entry.def.type === 'stdio' ? entry.def.args ?? [] : [];

  return (
    <button
      onClick={() => onSelect(isSelected ? null : entry)}
      className={cn(
        'w-full rounded-xl p-4 text-left transition-all',
        'bg-white/[0.02] border hover:bg-white/[0.04]',
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
            <h3 className="font-medium text-white truncate">{entry.name}</h3>
            <span className="tag">{typeLabels[entry.def.type]}</span>
          </div>
          <div className="flex items-center gap-1 group">
            <p className="text-xs text-white/40 truncate">
              {endpoint || 'No endpoint configured'}
            </p>
            {endpoint && <CopyButton text={endpoint} showOnHover label="Copied endpoint" />}
          </div>
        </div>
      </div>

      <div className="flex flex-wrap gap-1 mb-3">
        {args.slice(0, 3).map((arg) => (
          <span key={arg} className="tag">
            {arg}
          </span>
        ))}
        {args.length > 3 && <span className="tag">+{args.length - 3}</span>}
        {entry.def.type === 'stdio' && args.length === 0 && (
          <span className="text-[10px] text-white/30">No args</span>
        )}
      </div>

      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <span className="text-[10px] text-white/30">
          {entry.def.type === 'http' ? 'Remote MCP' : 'Local MCP'}
        </span>
        <span className="text-[10px] text-white/40">Library config</span>
      </div>
    </button>
  );
}

function McpDetailPanel({
  entry,
  onClose,
  onEdit,
  onDelete,
}: {
  entry: McpEntry;
  onClose: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const endpoint = formatEndpoint(entry.def);
  const args = entry.def.type === 'stdio' ? entry.def.args ?? [] : [];
  const envEntries = entry.def.type === 'stdio' ? Object.entries(entry.def.env ?? {}) : [];

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
              <h2 className="text-lg font-semibold text-white">{entry.name}</h2>
              <span className="tag">{typeLabels[entry.def.type]}</span>
            </div>
            <p className="text-xs text-white/40 mt-1">mcp/servers.json</p>
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
            <p className="text-xs text-white/40 mb-2">Endpoint</p>
            <div className="flex items-center gap-2 group">
              <p className="text-sm text-white break-all">
                {endpoint || 'Not configured'}
              </p>
              {endpoint && <CopyButton text={endpoint} showOnHover label="Copied endpoint" />}
            </div>
          </div>

          {entry.def.type === 'stdio' && (
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
              <p className="text-xs text-white/40 mb-2">Arguments</p>
              {args.length === 0 ? (
                <p className="text-sm text-white/40">No args configured</p>
              ) : (
                <div className="flex flex-wrap gap-1">
                  {args.map((arg) => (
                    <span key={arg} className="tag">
                      {arg}
                    </span>
                  ))}
                </div>
              )}
            </div>
          )}

          {entry.def.type === 'stdio' && (
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
              <p className="text-xs text-white/40 mb-2">Environment</p>
              {envEntries.length === 0 ? (
                <p className="text-sm text-white/40">No environment variables</p>
              ) : (
                <div className="space-y-2">
                  {envEntries.map(([key, value]) => (
                    <div key={key} className="flex items-center justify-between text-sm">
                      <span className="text-white/70">{key}</span>
                      <span className="text-white/40 truncate max-w-[200px]">{value}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
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

function McpFormModal({
  open,
  title,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  title: string;
  initial?: McpEntry;
  onClose: () => void;
  onSave: (name: string, def: McpServerDef) => Promise<void>;
}) {
  const [form, setForm] = useState<McpFormState>(() => buildFormState(initial));
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setForm(buildFormState(initial));
    setError(null);
    setLoading(false);
  }, [open, initial]);

  if (!open) return null;

  const updateForm = (updates: Partial<McpFormState>) => {
    setForm((prev) => ({ ...prev, ...updates }));
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);

    const name = form.name.trim();
    if (!name) {
      setError('Name is required');
      return;
    }

    if (form.type === 'http') {
      if (!form.url.trim()) {
        setError('Endpoint URL is required');
        return;
      }
    } else {
      if (!form.command.trim()) {
        setError('Command is required');
        return;
      }
    }

    const parsedEnv = form.type === 'stdio' ? parseEnv(form.env) : { env: {} };
    if (parsedEnv.error) {
      setError(parsedEnv.error);
      return;
    }

    const def: McpServerDef =
      form.type === 'http'
        ? { type: 'http', url: form.url.trim() }
        : {
            type: 'stdio',
            command: form.command.trim(),
            args: parseArgs(form.args),
            env: parsedEnv.env,
          };

    setLoading(true);
    try {
      await onSave(name, def);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save MCP');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4 animate-fade-in">
      <div className="w-full max-w-md rounded-2xl glass-panel border border-white/[0.08] p-6 animate-slide-up">
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
              <label className="block text-xs font-medium text-white/60 mb-1.5">Name</label>
              <input
                type="text"
                value={form.name}
                onChange={(e) => updateForm({ name: e.target.value })}
                placeholder="e.g., Supabase MCP"
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                required
              />
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">Type</label>
              <select
                value={form.type}
                onChange={(e) => updateForm({ type: e.target.value as McpServerDef['type'] })}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none transition-colors"
              >
                <option value="http">HTTP</option>
                <option value="stdio">STDIO</option>
              </select>
            </div>

            {form.type === 'http' ? (
              <div>
                <label className="block text-xs font-medium text-white/60 mb-1.5">Endpoint URL</label>
                <input
                  type="text"
                  value={form.url}
                  onChange={(e) => updateForm({ url: e.target.value })}
                  placeholder="https://mcp.example.com/mcp"
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                  required
                />
              </div>
            ) : (
              <>
                <div>
                  <label className="block text-xs font-medium text-white/60 mb-1.5">Command</label>
                  <input
                    type="text"
                    value={form.command}
                    onChange={(e) => updateForm({ command: e.target.value })}
                    placeholder="bunx"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                    required
                  />
                </div>
                <div>
                  <label className="block text-xs font-medium text-white/60 mb-1.5">Args (comma-separated)</label>
                  <input
                    type="text"
                    value={form.args}
                    onChange={(e) => updateForm({ args: e.target.value })}
                    placeholder="@playwright/mcp@latest"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                  />
                </div>
                <div>
                  <label className="block text-xs font-medium text-white/60 mb-1.5">Environment (KEY=VALUE)</label>
                  <textarea
                    value={form.env}
                    onChange={(e) => updateForm({ env: e.target.value })}
                    placeholder="OPENAI_API_KEY=..."
                    rows={3}
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
                  />
                </div>
              </>
            )}

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
              {loading ? 'Saving...' : 'Save MCP'}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

export default function McpsPage() {
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

  const [mcps, setMcps] = useState<Record<string, McpServerDef>>({});
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [showAddModal, setShowAddModal] = useState(false);
  const [showEditModal, setShowEditModal] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<McpEntry | null>(null);

  const entries = useMemo<McpEntry[]>(() => {
    return Object.entries(mcps)
      .map(([name, def]) => ({ name, def }))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [mcps]);

  const selectedEntry = useMemo(
    () => entries.find((entry) => entry.name === selectedName) ?? null,
    [entries, selectedName]
  );

  const filteredEntries = useMemo(() => {
    if (!searchQuery.trim()) return entries;
    const query = searchQuery.toLowerCase();
    return entries.filter((entry) => {
      const endpoint = formatEndpoint(entry.def).toLowerCase();
      const args = (entry.def.args ?? []).join(' ').toLowerCase();
      return (
        entry.name.toLowerCase().includes(query) ||
        endpoint.includes(query) ||
        args.includes(query)
      );
    });
  }, [entries, searchQuery]);

  const loadData = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      setLibraryUnavailable(false);
      setLibraryUnavailableMessage(null);
      const [statusData, mcpsData] = await Promise.all([
        getLibraryStatus(),
        getLibraryMcps(),
      ]);
      setStatus(statusData);
      setMcps(mcpsData);
      if (selectedName && !mcpsData[selectedName]) {
        setSelectedName(null);
      }
    } catch (err) {
      if (err instanceof LibraryUnavailableError) {
        setLibraryUnavailable(true);
        setLibraryUnavailableMessage(err.message);
        setStatus(null);
        setMcps({});
        setSelectedName(null);
        return;
      }
      setError(err instanceof Error ? err.message : 'Failed to load MCPs');
    } finally {
      setLoading(false);
    }
  }, [selectedName]);

  useEffect(() => {
    loadData();
  }, [loadData]);

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

  const saveNext = async (next: Record<string, McpServerDef>, name?: string) => {
    await saveLibraryMcps(next);
    setMcps(next);
    if (name) setSelectedName(name);
    // Refresh status only (not MCPs since we just saved the new state)
    const statusData = await getLibraryStatus();
    setStatus(statusData);
  };

  const handleAddMcp = async (name: string, def: McpServerDef) => {
    if (mcps[name]) {
      throw new Error(`MCP "${name}" already exists`);
    }
    const next = { ...mcps, [name]: def };
    await saveNext(next, name);
    toast.success(`Added ${name}`);
  };

  const handleUpdateMcp = async (name: string, def: McpServerDef) => {
    if (!selectedEntry) return;
    if (name !== selectedEntry.name && mcps[name]) {
      throw new Error(`MCP "${name}" already exists`);
    }
    const next = { ...mcps };
    delete next[selectedEntry.name];
    next[name] = def;
    await saveNext(next, name);
    toast.success(`Saved ${name}`);
  };

  const requestDelete = (entry: McpEntry) => {
    setPendingDelete(entry);
    setShowDeleteConfirm(true);
  };

  const handleDelete = async () => {
    if (!pendingDelete) return;
    try {
      const next = { ...mcps };
      delete next[pendingDelete.name];
      await saveNext(next);
      toast.success(`Removed ${pendingDelete.name}`);
      if (selectedName === pendingDelete.name) {
        setSelectedName(null);
      }
    } catch (err) {
      toast.error(`Failed to remove ${pendingDelete.name}`);
    } finally {
      setShowDeleteConfirm(false);
      setPendingDelete(null);
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

          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <h1 className="text-xl font-semibold text-white">MCP Servers</h1>
              <p className="text-sm text-white/40">Configure MCP definitions stored in your library repo.</p>
            </div>
            <button
              onClick={() => setShowAddModal(true)}
              className="flex items-center gap-2 rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2 text-sm font-medium text-white transition-colors"
            >
              <Plus className="h-4 w-4" />
              Add MCP
            </button>
          </div>

          <div className="relative">
            <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search MCPs..."
              className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] pl-10 pr-4 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
            />
          </div>

          {filteredEntries.length === 0 ? (
            <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-8 text-center">
              <p className="text-sm text-white/40">
                {entries.length === 0
                  ? 'No MCP servers configured yet.'
                  : 'No MCPs match your search.'}
              </p>
            </div>
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              {filteredEntries.map((entry) => (
                <McpCard
                  key={entry.name}
                  entry={entry}
                  isSelected={selectedName === entry.name}
                  onSelect={(next) => setSelectedName(next?.name ?? null)}
                />
              ))}
            </div>
          )}

          {selectedEntry && (
            <McpDetailPanel
              entry={selectedEntry}
              onClose={() => setSelectedName(null)}
              onEdit={() => setShowEditModal(true)}
              onDelete={() => requestDelete(selectedEntry)}
            />
          )}

          <McpFormModal
            open={showAddModal}
            title="Add MCP Server"
            onClose={() => setShowAddModal(false)}
            onSave={handleAddMcp}
          />

          <McpFormModal
            open={showEditModal}
            title={selectedEntry ? `Edit ${selectedEntry.name}` : 'Edit MCP'}
            initial={selectedEntry ?? undefined}
            onClose={() => setShowEditModal(false)}
            onSave={handleUpdateMcp}
          />

          <ConfirmDialog
            open={showDeleteConfirm}
            title={`Remove ${pendingDelete?.name}?`}
            description="This will remove the MCP definition from your library repo. This action cannot be undone."
            confirmLabel="Remove MCP"
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
