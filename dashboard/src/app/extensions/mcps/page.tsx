'use client';

import { useEffect, useMemo, useState } from 'react';
import { toast } from '@/components/toast';
import { type McpScope, type McpServerDef, type McpServerState, type McpTransport, type McpStatus, type UpdateMcpRequest, listMcps, enableMcp, disableMcp, refreshMcp, updateMcp } from '@/lib/api';
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
  Power,
  Zap,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { LibraryUnavailable } from '@/components/library-unavailable';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { CopyButton } from '@/components/ui/copy-button';
import { useLibrary } from '@/contexts/library-context';

type McpEntry = {
  name: string;
  def: McpServerDef;
};

type McpFormState = {
  name: string;
  type: McpServerDef['type'];
  url: string;
  command: string; // Full command string (will be split into array)
  env: string;
  headers: string;
};

const typeLabels: Record<McpServerDef['type'], string> = {
  local: 'Local',
  remote: 'Remote',
};

function formatEndpoint(def: McpServerDef): string {
  if (def.type === 'remote') return def.url ?? '';
  const parts = def.command ?? [];
  return parts.join(' ');
}

function serializeCommand(command?: string[]): string {
  if (!command || command.length === 0) return '';
  // Join with newlines for multi-line editing
  return command.join('\n');
}

function parseCommand(value: string): string[] {
  // Split by newlines for multi-line format, or by spaces for single-line
  const trimmed = value.trim();
  if (!trimmed) return [];
  if (trimmed.includes('\n')) {
    return trimmed.split('\n').map((line) => line.trim()).filter(Boolean);
  }
  // Single line: split by spaces but preserve quoted strings
  return trimmed.split(/\s+/).filter(Boolean);
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

function serializeHeaders(headers?: Record<string, string>): string {
  if (!headers || Object.keys(headers).length === 0) return '';
  return Object.entries(headers)
    .map(([key, value]) => `${key}: ${value}`)
    .join('\n');
}

function parseHeaders(value: string): { headers: Record<string, string>; error?: string } {
  const trimmed = value.trim();
  if (!trimmed) return { headers: {} };
  const headers: Record<string, string> = {};
  for (const rawLine of trimmed.split('\n')) {
    const line = rawLine.trim();
    if (!line) continue;
    const idx = line.indexOf(':');
    if (idx <= 0) {
      return { headers: {}, error: `Invalid header line: "${rawLine}"` };
    }
    const key = line.slice(0, idx).trim();
    const val = line.slice(idx + 1).trim();
    if (!key) {
      return { headers: {}, error: `Invalid header key in line: "${rawLine}"` };
    }
    headers[key] = val;
  }
  return { headers };
}

function buildFormState(entry?: McpEntry): McpFormState {
  if (!entry) {
    return {
      name: '',
      type: 'local',
      url: '',
      command: '',
      env: '',
      headers: '',
    };
  }

  return {
    name: entry.name,
    type: entry.def.type,
    url: entry.def.type === 'remote' ? entry.def.url ?? '' : '',
    command: entry.def.type === 'local' ? serializeCommand(entry.def.command) : '',
    env: entry.def.type === 'local' ? serializeEnv(entry.def.env) : '',
    headers: entry.def.type === 'remote' ? serializeHeaders(entry.def.headers) : '',
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
  const commandParts = entry.def.type === 'local' ? entry.def.command ?? [] : [];

  const handleSelect = () => onSelect(isSelected ? null : entry);

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
        {commandParts.slice(0, 3).map((part, idx) => (
          <span key={idx} className="tag">
            {part}
          </span>
        ))}
        {commandParts.length > 3 && <span className="tag">+{commandParts.length - 3}</span>}
        {entry.def.type === 'local' && commandParts.length === 0 && (
          <span className="text-[10px] text-white/30">No command</span>
        )}
      </div>

      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <span className="text-[10px] text-white/30">
          {entry.def.type === 'remote' ? 'Remote MCP' : 'Local MCP'}
        </span>
        <span className="text-[10px] text-white/40">Library config</span>
      </div>
    </div>
  );
}

function RuntimeMcpCard({
  mcp,
  isSelected,
  onSelect,
  onToggle,
  onRefresh,
}: {
  mcp: McpServerState;
  isSelected: boolean;
  onSelect: (mcp: McpServerState | null) => void;
  onToggle: (id: string, enabled: boolean) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
}) {
  const [toggling, setToggling] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  const statusColor: Record<McpStatus, string> = {
    connected: 'text-emerald-400',
    connecting: 'text-amber-400',
    disconnected: 'text-white/40',
    disabled: 'text-white/40',
    error: 'text-red-400',
  };

  const statusLabel: Record<McpStatus, string> = {
    connected: 'Connected',
    connecting: 'Connecting...',
    disconnected: 'Disconnected',
    disabled: 'Disabled',
    error: 'Error',
  };

  const handleToggle = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setToggling(true);
    try {
      await onToggle(mcp.id, !mcp.enabled);
    } finally {
      setToggling(false);
    }
  };

  const handleRefresh = async (e: React.MouseEvent) => {
    e.stopPropagation();
    setRefreshing(true);
    try {
      await onRefresh(mcp.id);
    } finally {
      setRefreshing(false);
    }
  };

  const handleSelect = () => onSelect(isSelected ? null : mcp);

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
        'bg-white/[0.02] border hover:bg-white/[0.04] focus:outline-none focus:ring-1 focus:ring-cyan-500/40',
        isSelected
          ? 'border-cyan-500/40 bg-cyan-500/5'
          : 'border-white/[0.04] hover:border-white/[0.08]'
      )}
    >
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-cyan-500/10">
          <Zap className="h-5 w-5 text-cyan-400" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="font-medium text-white truncate">{mcp.name}</h3>
            <span className="tag bg-cyan-500/10 text-cyan-400 border-cyan-500/20">Runtime</span>
            <span
              className={cn(
                'tag',
                mcp.scope === 'workspace'
                  ? 'bg-amber-500/10 text-amber-400 border-amber-500/20'
                  : 'bg-white/[0.04] text-white/50 border-white/[0.08]'
              )}
            >
              {mcp.scope === 'workspace' ? 'Workspace' : 'Global'}
            </span>
          </div>
          <div className="flex items-center gap-1 group">
            <p className="text-xs text-white/40 truncate">
              {mcp.endpoint || 'stdio'}
            </p>
            {mcp.endpoint && <CopyButton text={mcp.endpoint} showOnHover label="Copied endpoint" />}
          </div>
        </div>
      </div>

      <div className="flex flex-wrap gap-1 mb-3">
        {mcp.tools.slice(0, 3).map((tool) => (
          <span key={tool} className="tag">
            {tool}
          </span>
        ))}
        {mcp.tools.length > 3 && <span className="tag">+{mcp.tools.length - 3}</span>}
        {mcp.tools.length === 0 && (
          <span className="text-[10px] text-white/30">No tools discovered</span>
        )}
      </div>

      <div className="flex items-center justify-between pt-3 border-t border-white/[0.04]">
        <div className="flex items-center gap-2">
          <span className={cn(
            'h-2 w-2 rounded-full',
            mcp.status === 'connected' && 'bg-emerald-400',
            mcp.status === 'connecting' && 'bg-amber-400 animate-pulse',
            mcp.status === 'disconnected' && 'bg-white/40',
            mcp.status === 'disabled' && 'bg-white/40',
            mcp.status === 'error' && 'bg-red-400'
          )} />
          <span className={cn('text-[10px]', statusColor[mcp.status])}>{statusLabel[mcp.status]}</span>
          {mcp.error && (
            <span className="text-[10px] text-red-400 truncate max-w-[120px]" title={mcp.error}>
              {mcp.error}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={handleRefresh}
            disabled={refreshing}
            className="flex h-6 w-6 items-center justify-center rounded text-white/40 hover:bg-white/[0.06] hover:text-white transition-colors disabled:opacity-50"
            title="Refresh MCP"
          >
            <RefreshCw className={cn('h-3 w-3', refreshing && 'animate-spin')} />
          </button>
          <button
            onClick={handleToggle}
            disabled={toggling}
            className={cn(
              'flex h-6 w-6 items-center justify-center rounded transition-colors disabled:opacity-50',
              mcp.enabled
                ? 'text-emerald-400 hover:bg-emerald-500/10'
                : 'text-white/40 hover:bg-white/[0.06]'
            )}
            title={mcp.enabled ? 'Disable MCP' : 'Enable MCP'}
          >
            <Power className="h-3 w-3" />
          </button>
        </div>
      </div>
    </div>
  );
}

function RuntimeMcpDetailPanel({
  mcp,
  onClose,
  onUpdate,
  onRefresh,
}: {
  mcp: McpServerState;
  onClose: () => void;
  onUpdate: (id: string, updates: UpdateMcpRequest) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
}) {
  const isStdio = 'stdio' in (mcp.transport ?? {});
  const isHttp = 'http' in (mcp.transport ?? {});
  const stdioConfig = isStdio ? (mcp.transport as { stdio: { command: string; args: string[]; env: Record<string, string> } }).stdio : null;
  const httpConfig = isHttp ? (mcp.transport as { http: { endpoint: string; headers: Record<string, string> } }).http : null;
  const [scope, setScope] = useState<McpScope>(mcp.scope ?? 'global');

  // For stdio: env vars; for http: headers
  const [keyValuePairs, setKeyValuePairs] = useState<Array<{ key: string; value: string }>>(
    () => {
      if (isStdio) {
        return Object.entries(stdioConfig?.env ?? {}).map(([key, value]) => ({ key, value }));
      }
      if (isHttp) {
        return Object.entries(httpConfig?.headers ?? {}).map(([key, value]) => ({ key, value }));
      }
      return [];
    }
  );
  const [newKey, setNewKey] = useState('');
  const [newValue, setNewValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  // Sync state when mcp prop changes (e.g., after refresh)
  useEffect(() => {
    if ('stdio' in (mcp.transport ?? {})) {
      const config = (mcp.transport as { stdio: { command: string; args: string[]; env: Record<string, string> } }).stdio;
      setKeyValuePairs(Object.entries(config?.env ?? {}).map(([key, value]) => ({ key, value })));
    } else if ('http' in (mcp.transport ?? {})) {
      const config = (mcp.transport as { http: { endpoint: string; headers: Record<string, string> } }).http;
      setKeyValuePairs(Object.entries(config?.headers ?? {}).map(([key, value]) => ({ key, value })));
    }
    setScope(mcp.scope ?? 'global');
  }, [mcp.transport, mcp.scope]);

  // Handle Escape key to close panel
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  const handleAddKeyValue = () => {
    if (!newKey.trim()) return;
    setKeyValuePairs((prev) => [...prev, { key: newKey.trim(), value: newValue }]);
    setNewKey('');
    setNewValue('');
  };

  const handleRemoveKeyValue = (index: number) => {
    setKeyValuePairs((prev) => prev.filter((_, i) => i !== index));
  };

  const handleUpdateKeyValue = (index: number, field: 'key' | 'value', value: string) => {
    setKeyValuePairs((prev) =>
      prev.map((item, i) => (i === index ? { ...item, [field]: value } : item))
    );
  };

  const handleSave = async () => {
    if (!stdioConfig && !httpConfig) return;
    setSaving(true);
    try {
      const newMap: Record<string, string> = {};
      keyValuePairs.forEach(({ key, value }) => {
        if (key.trim()) {
          newMap[key.trim()] = value;
        }
      });

      let transport: McpTransport;
      if (stdioConfig) {
        transport = {
          stdio: {
            command: stdioConfig.command,
            args: stdioConfig.args,
            env: newMap,
          },
        };
      } else if (httpConfig) {
        transport = {
          http: {
            endpoint: httpConfig.endpoint,
            headers: newMap,
          },
        };
      } else {
        return;
      }

      await onUpdate(mcp.id, { transport, scope });
      toast.success(isStdio ? 'Saved environment variables' : 'Saved headers');
    } catch {
      toast.error('Failed to save');
    } finally {
      setSaving(false);
    }
  };

  const handleRefresh = async () => {
    setRefreshing(true);
    try {
      await onRefresh(mcp.id);
      toast.success('Refreshed MCP');
    } catch {
      toast.error('Failed to refresh');
    } finally {
      setRefreshing(false);
    }
  };

  const statusColorMap: Record<McpStatus, string> = {
    connected: 'text-emerald-400',
    connecting: 'text-amber-400',
    disconnected: 'text-white/40',
    disabled: 'text-white/40',
    error: 'text-red-400',
  };

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
              <h2 className="text-lg font-semibold text-white">{mcp.name}</h2>
              <span className="tag bg-cyan-500/10 text-cyan-400 border-cyan-500/20">Runtime</span>
              <span
                className={cn(
                  'tag',
                  scope === 'workspace'
                    ? 'bg-amber-500/10 text-amber-400 border-amber-500/20'
                    : 'bg-white/[0.04] text-white/50 border-white/[0.08]'
                )}
              >
                {scope === 'workspace' ? 'Workspace' : 'Global'}
              </span>
            </div>
            <div className="flex items-center gap-2 mt-1">
              <span className={cn(
                'h-2 w-2 rounded-full',
                mcp.status === 'connected' && 'bg-emerald-400',
                mcp.status === 'connecting' && 'bg-amber-400 animate-pulse',
                mcp.status === 'disconnected' && 'bg-white/40',
                mcp.status === 'disabled' && 'bg-white/40',
                mcp.status === 'error' && 'bg-red-400'
              )} />
              <span className={cn('text-xs', statusColorMap[mcp.status])}>
                {mcp.status.charAt(0).toUpperCase() + mcp.status.slice(1)}
              </span>
              {mcp.version && <span className="text-xs text-white/30">v{mcp.version}</span>}
            </div>
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
            <p className="text-xs text-white/40 mb-2">Scope</p>
            <div className="flex items-center gap-2">
              <select
                value={scope}
                onChange={(e) => setScope(e.target.value as McpScope)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-2 text-xs text-white focus:border-cyan-500/50 focus:outline-none"
              >
                <option value="global">Global (host-level)</option>
                <option value="workspace">Workspace (installed per workspace)</option>
              </select>
            </div>
            <p className="mt-2 text-[11px] text-white/40">
              Workspace-scoped MCPs must be installed in the workspace init script.
            </p>
          </div>

          {isStdio && stdioConfig && (
            <>
              <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
                <p className="text-xs text-white/40 mb-2">Command</p>
                <div className="flex flex-wrap gap-1">
                  <span className="tag">{stdioConfig.command}</span>
                  {stdioConfig.args.map((arg, idx) => (
                    <span key={idx} className="tag">
                      {arg}
                    </span>
                  ))}
                </div>
              </div>

              <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
                <div className="flex items-center justify-between mb-3">
                  <p className="text-xs text-white/40">Environment Variables</p>
                </div>

                {keyValuePairs.length === 0 ? (
                  <p className="text-sm text-white/40 mb-3">No environment variables configured</p>
                ) : (
                  <div className="space-y-2 mb-3">
                    {keyValuePairs.map((item, idx) => (
                      <div key={idx} className="flex items-center gap-2">
                        <input
                          type="text"
                          value={item.key}
                          onChange={(e) => handleUpdateKeyValue(idx, 'key', e.target.value)}
                          placeholder="KEY"
                          className="flex-1 min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                        />
                        <input
                          type="text"
                          value={item.value}
                          onChange={(e) => handleUpdateKeyValue(idx, 'value', e.target.value)}
                          placeholder="value"
                          className="flex-[2] min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                        />
                        <button
                          onClick={() => handleRemoveKeyValue(idx)}
                          className="flex h-7 w-7 items-center justify-center rounded-lg text-white/40 hover:bg-red-500/10 hover:text-red-400 transition-colors"
                        >
                          <X className="h-3 w-3" />
                        </button>
                      </div>
                    ))}
                  </div>
                )}

                <div className="flex items-center gap-2 pt-2 border-t border-white/[0.04]">
                  <input
                    type="text"
                    value={newKey}
                    onChange={(e) => setNewKey(e.target.value)}
                    placeholder="NEW_KEY"
                    className="flex-1 min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                    onKeyDown={(e) => e.key === 'Enter' && handleAddKeyValue()}
                  />
                  <input
                    type="text"
                    value={newValue}
                    onChange={(e) => setNewValue(e.target.value)}
                    placeholder="value"
                    className="flex-[2] min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                    onKeyDown={(e) => e.key === 'Enter' && handleAddKeyValue()}
                  />
                  <button
                    onClick={handleAddKeyValue}
                    disabled={!newKey.trim()}
                    className="flex h-7 w-7 items-center justify-center rounded-lg bg-cyan-500/10 text-cyan-400 hover:bg-cyan-500/20 transition-colors disabled:opacity-50"
                  >
                    <Plus className="h-3 w-3" />
                  </button>
                </div>
              </div>
            </>
          )}

          {isHttp && httpConfig && (
            <>
              <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
                <p className="text-xs text-white/40 mb-2">Endpoint</p>
                <div className="flex items-center gap-2 group">
                  <p className="text-sm text-white break-all">
                    {httpConfig.endpoint || 'HTTP transport'}
                  </p>
                  {httpConfig.endpoint && <CopyButton text={httpConfig.endpoint} showOnHover label="Copied endpoint" />}
                </div>
              </div>

              <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
                <div className="flex items-center justify-between mb-3">
                  <p className="text-xs text-white/40">Headers</p>
                </div>

                {keyValuePairs.length === 0 ? (
                  <p className="text-sm text-white/40 mb-3">No headers configured</p>
                ) : (
                  <div className="space-y-2 mb-3">
                    {keyValuePairs.map((item, idx) => (
                      <div key={idx} className="flex items-center gap-2">
                        <input
                          type="text"
                          value={item.key}
                          onChange={(e) => handleUpdateKeyValue(idx, 'key', e.target.value)}
                          placeholder="Header-Name"
                          className="flex-1 min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                        />
                        <input
                          type="text"
                          value={item.value}
                          onChange={(e) => handleUpdateKeyValue(idx, 'value', e.target.value)}
                          placeholder="value"
                          className="flex-[2] min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                        />
                        <button
                          onClick={() => handleRemoveKeyValue(idx)}
                          className="flex h-7 w-7 items-center justify-center rounded-lg text-white/40 hover:bg-red-500/10 hover:text-red-400 transition-colors"
                        >
                          <X className="h-3 w-3" />
                        </button>
                      </div>
                    ))}
                  </div>
                )}

                <div className="flex items-center gap-2 pt-2 border-t border-white/[0.04]">
                  <input
                    type="text"
                    value={newKey}
                    onChange={(e) => setNewKey(e.target.value)}
                    placeholder="Authorization"
                    className="flex-1 min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                    onKeyDown={(e) => e.key === 'Enter' && handleAddKeyValue()}
                  />
                  <input
                    type="text"
                    value={newValue}
                    onChange={(e) => setNewValue(e.target.value)}
                    placeholder="Bearer ..."
                    className="flex-[2] min-w-0 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2 py-1.5 text-xs text-white placeholder-white/30 focus:border-cyan-500/50 focus:outline-none"
                    onKeyDown={(e) => e.key === 'Enter' && handleAddKeyValue()}
                  />
                  <button
                    onClick={handleAddKeyValue}
                    disabled={!newKey.trim()}
                    className="flex h-7 w-7 items-center justify-center rounded-lg bg-cyan-500/10 text-cyan-400 hover:bg-cyan-500/20 transition-colors disabled:opacity-50"
                  >
                    <Plus className="h-3 w-3" />
                  </button>
                </div>
              </div>
            </>
          )}

          <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
            <p className="text-xs text-white/40 mb-2">Tools ({mcp.tools.length})</p>
            {mcp.tools.length === 0 ? (
              <p className="text-sm text-white/40">No tools discovered</p>
            ) : (
              <div className="flex flex-wrap gap-1 max-h-32 overflow-y-auto">
                {mcp.tools.map((tool) => (
                  <span key={tool} className="tag">
                    {tool}
                  </span>
                ))}
              </div>
            )}
          </div>

          {mcp.error && (
            <div className="rounded-xl bg-red-500/10 border border-red-500/20 p-4">
              <p className="text-xs text-red-400 mb-1">Error</p>
              <p className="text-sm text-red-300">{mcp.error}</p>
            </div>
          )}
        </div>

        <div className="border-t border-white/[0.06] p-4 flex items-center gap-2">
          {(isStdio || isHttp) && (
            <button
              onClick={handleSave}
              disabled={saving}
              className="flex-1 flex items-center justify-center gap-2 rounded-lg bg-cyan-500/10 hover:bg-cyan-500/20 border border-cyan-500/20 px-3 py-2 text-sm text-cyan-300 transition-colors disabled:opacity-50"
            >
              {saving ? <Loader className="h-4 w-4 animate-spin" /> : <Check className="h-4 w-4" />}
              Save
            </button>
          )}
          <button
            onClick={handleRefresh}
            disabled={refreshing}
            className="flex items-center justify-center gap-2 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] px-3 py-2 text-sm text-white/80 transition-colors disabled:opacity-50"
          >
            <RefreshCw className={cn('h-4 w-4', refreshing && 'animate-spin')} />
            Refresh
          </button>
        </div>
      </div>
    </>
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
  const commandParts = entry.def.type === 'local' ? entry.def.command ?? [] : [];
  const envEntries = entry.def.type === 'local' ? Object.entries(entry.def.env ?? {}) : [];
  const headerEntries = entry.def.type === 'remote' ? Object.entries(entry.def.headers ?? {}) : [];

  // Handle Escape key to close panel
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
      }
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

          {entry.def.type === 'local' && (
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
              <p className="text-xs text-white/40 mb-2">Command Parts</p>
              {commandParts.length === 0 ? (
                <p className="text-sm text-white/40">No command configured</p>
              ) : (
                <div className="flex flex-wrap gap-1">
                  {commandParts.map((part, idx) => (
                    <span key={idx} className="tag">
                      {part}
                    </span>
                  ))}
                </div>
              )}
            </div>
          )}

          {entry.def.type === 'local' && (
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

          {entry.def.type === 'remote' && (
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-4">
              <p className="text-xs text-white/40 mb-2">Headers</p>
              {headerEntries.length === 0 ? (
                <p className="text-sm text-white/40">No headers configured</p>
              ) : (
                <div className="space-y-2">
                  {headerEntries.map(([key, value]) => (
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

  // Handle Escape key to close modal
  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

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

    if (form.type === 'remote') {
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

    const parsedEnv = form.type === 'local' ? parseEnv(form.env) : { env: {} };
    if (parsedEnv.error) {
      setError(parsedEnv.error);
      return;
    }

    const parsedHeaders = form.type === 'remote' ? parseHeaders(form.headers) : { headers: {} };
    if (parsedHeaders.error) {
      setError(parsedHeaders.error);
      return;
    }

    const def: McpServerDef =
      form.type === 'remote'
        ? { type: 'remote', url: form.url.trim(), headers: parsedHeaders.headers }
        : {
            type: 'local',
            command: parseCommand(form.command),
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
                <option value="local">Local (stdio)</option>
                <option value="remote">Remote (HTTP)</option>
              </select>
            </div>

            {form.type === 'remote' ? (
              <>
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
                <div>
                  <label className="block text-xs font-medium text-white/60 mb-1.5">Headers (Key: Value)</label>
                  <textarea
                    value={form.headers}
                    onChange={(e) => updateForm({ headers: e.target.value })}
                    placeholder="Authorization: Bearer ..."
                    rows={3}
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
                  />
                </div>
              </>
            ) : (
              <>
                <div>
                  <label className="block text-xs font-medium text-white/60 mb-1.5">Command (one part per line or space-separated)</label>
                  <textarea
                    value={form.command}
                    onChange={(e) => updateForm({ command: e.target.value })}
                    placeholder="npx&#10;@playwright/mcp@latest"
                    rows={3}
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors resize-none"
                    required
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
  const {
    status,
    mcps,
    loading,
    libraryUnavailable,
    libraryUnavailableMessage,
    refresh,
    sync,
    commit,
    push,
    saveMcps,
    syncing,
    committing,
    pushing,
  } = useLibrary();

  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [showAddModal, setShowAddModal] = useState(false);
  const [showEditModal, setShowEditModal] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<McpEntry | null>(null);
  const [commitMessage, setCommitMessage] = useState('');
  const [showCommitDialog, setShowCommitDialog] = useState(false);

  // Runtime MCPs state
  const [runtimeMcps, setRuntimeMcps] = useState<McpServerState[]>([]);
  const [runtimeLoading, setRuntimeLoading] = useState(true);
  const [selectedRuntimeMcp, setSelectedRuntimeMcp] = useState<McpServerState | null>(null);

  // Fetch runtime MCPs with polling for status updates
  useEffect(() => {
    const fetchRuntimeMcps = async () => {
      try {
        const mcps = await listMcps();
        setRuntimeMcps(mcps);
      } catch (err) {
        console.error('Failed to fetch runtime MCPs:', err);
      } finally {
        setRuntimeLoading(false);
      }
    };
    fetchRuntimeMcps();

    // Poll every 5 seconds for status updates
    const interval = setInterval(async () => {
      try {
        const mcps = await listMcps();
        setRuntimeMcps(mcps);
        // Update selected MCP if it changed
        setSelectedRuntimeMcp((prev) => {
          if (!prev) return null;
          const updated = mcps.find((m) => m.id === prev.id);
          return updated ?? null;
        });
      } catch (err) {
        console.error('Failed to poll runtime MCPs:', err);
      }
    }, 5000);

    return () => clearInterval(interval);
  }, []);

  const handleToggleRuntimeMcp = async (id: string, enabled: boolean) => {
    try {
      const updated = enabled ? await enableMcp(id) : await disableMcp(id);
      setRuntimeMcps((prev) => prev.map((m) => (m.id === id ? updated : m)));
      toast.success(`${enabled ? 'Enabled' : 'Disabled'} MCP`);
    } catch {
      toast.error('Failed to toggle MCP');
    }
  };

  const handleRefreshRuntimeMcp = async (id: string) => {
    try {
      const updated = await refreshMcp(id);
      setRuntimeMcps((prev) => prev.map((m) => (m.id === id ? updated : m)));
      // Also update selected if it's the same
      setSelectedRuntimeMcp((prev) => (prev?.id === id ? updated : prev));
      toast.success('Refreshed MCP');
    } catch {
      toast.error('Failed to refresh MCP');
    }
  };

  const handleUpdateRuntimeMcp = async (id: string, updates: UpdateMcpRequest) => {
    const updated = await updateMcp(id, updates);
    setRuntimeMcps((prev) => prev.map((m) => (m.id === id ? updated : m)));
    setSelectedRuntimeMcp((prev) => (prev?.id === id ? updated : prev));
  };

  const filteredRuntimeMcps = useMemo(() => {
    if (!searchQuery.trim()) return runtimeMcps;
    const query = searchQuery.toLowerCase();
    return runtimeMcps.filter((mcp) => {
      return (
        mcp.name.toLowerCase().includes(query) ||
        mcp.endpoint?.toLowerCase().includes(query) ||
        mcp.tools.some((t) => t.toLowerCase().includes(query))
      );
    });
  }, [runtimeMcps, searchQuery]);

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
      const command = entry.def.type === 'local' ? (entry.def.command ?? []).join(' ').toLowerCase() : '';
      return (
        entry.name.toLowerCase().includes(query) ||
        endpoint.includes(query) ||
        command.includes(query)
      );
    });
  }, [entries, searchQuery]);

  // Clear selection if the selected item no longer exists
  useEffect(() => {
    if (selectedName && !mcps[selectedName]) {
      setSelectedName(null);
    }
  }, [mcps, selectedName]);

  // Handle Escape key for commit dialog
  useEffect(() => {
    if (!showCommitDialog) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setShowCommitDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showCommitDialog]);

  const handleSync = async () => {
    try {
      await sync();
    } catch {
      // Error is handled by context
    }
  };

  const handleCommit = async () => {
    if (!commitMessage.trim()) return;
    try {
      await commit(commitMessage);
      setCommitMessage('');
      setShowCommitDialog(false);
    } catch {
      // Error is handled by context
    }
  };

  const handlePush = async () => {
    try {
      await push();
    } catch {
      // Error is handled by context
    }
  };

  const handleAddMcp = async (name: string, def: McpServerDef) => {
    if (mcps[name]) {
      throw new Error(`MCP "${name}" already exists`);
    }
    const next = { ...mcps, [name]: def };
    await saveMcps(next);
    setSelectedName(name);
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
    await saveMcps(next);
    setSelectedName(name);
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
      await saveMcps(next);
      toast.success(`Removed ${pendingDelete.name}`);
      if (selectedName === pendingDelete.name) {
        setSelectedName(null);
      }
    } catch {
      toast.error(`Failed to remove ${pendingDelete.name}`);
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

          {/* Runtime MCPs Section */}
          {filteredRuntimeMcps.length > 0 && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Zap className="h-4 w-4 text-cyan-400" />
                <h2 className="text-sm font-medium text-white/70">Runtime MCPs</h2>
                <span className="text-xs text-white/40">Auto-registered servers</span>
              </div>
              <div className="grid gap-4 md:grid-cols-2">
                {filteredRuntimeMcps.map((mcp) => (
                  <RuntimeMcpCard
                    key={mcp.id}
                    mcp={mcp}
                    isSelected={selectedRuntimeMcp?.id === mcp.id}
                    onSelect={setSelectedRuntimeMcp}
                    onToggle={handleToggleRuntimeMcp}
                    onRefresh={handleRefreshRuntimeMcp}
                  />
                ))}
              </div>
            </div>
          )}

          {runtimeLoading && filteredRuntimeMcps.length === 0 && (
            <div className="flex items-center justify-center p-4">
              <Loader className="h-5 w-5 animate-spin text-white/40" />
            </div>
          )}

          {/* Library MCPs Section */}
          {(filteredEntries.length > 0 || filteredRuntimeMcps.length > 0) && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Plug className="h-4 w-4 text-indigo-400" />
                <h2 className="text-sm font-medium text-white/70">Library MCPs</h2>
                <span className="text-xs text-white/40">From your config repo</span>
              </div>
              {filteredEntries.length === 0 ? (
                <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6 text-center">
                  <p className="text-sm text-white/40">
                    {entries.length === 0
                      ? 'No library MCPs configured yet. Click "Add MCP" to create one.'
                      : 'No library MCPs match your search.'}
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
            </div>
          )}

          {/* Empty state when no MCPs at all */}
          {filteredEntries.length === 0 && filteredRuntimeMcps.length === 0 && !runtimeLoading && (
            <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-8 text-center">
              <p className="text-sm text-white/40">
                {searchQuery.trim()
                  ? 'No MCPs match your search.'
                  : 'No MCP servers configured yet.'}
              </p>
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

          {selectedRuntimeMcp && (
            <RuntimeMcpDetailPanel
              mcp={selectedRuntimeMcp}
              onClose={() => setSelectedRuntimeMcp(null)}
              onUpdate={handleUpdateRuntimeMcp}
              onRefresh={handleRefreshRuntimeMcp}
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
