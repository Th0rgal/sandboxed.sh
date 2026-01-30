'use client';

import { useState } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  getSystemComponents,
  updateSystemComponent,
  ComponentInfo,
  UpdateProgressEvent,
} from '@/lib/api';
import {
  Server,
  RefreshCw,
  ArrowUp,
  Check,
  AlertCircle,
  Loader,
  ChevronDown,
  ChevronUp,
} from 'lucide-react';
import { cn } from '@/lib/utils';

// Component display names
const componentNames: Record<string, string> = {
  open_agent: 'sandboxed.sh',
  opencode: 'OpenCode',
  claude_code: 'Claude Code',
  amp: 'Amp',
  oh_my_opencode: 'oh-my-opencode',
};

// Component icons
const componentIcons: Record<string, string> = {
  open_agent: '🚀',
  opencode: '⚡',
  claude_code: '🤖',
  amp: '⚡',
  oh_my_opencode: '🎭',
};

interface UpdateLog {
  message: string;
  progress?: number;
  type: 'log' | 'complete' | 'error';
}

interface ServerConnectionCardProps {
  apiUrl: string;
  setApiUrl: (url: string) => void;
  urlError: string | null;
  validateUrl: (url: string) => void;
  health: { version: string } | null;
  healthLoading: boolean;
  testingConnection: boolean;
  testApiConnection: () => void;
}

export function ServerConnectionCard({
  apiUrl,
  setApiUrl,
  urlError,
  validateUrl,
  health,
  healthLoading,
  testingConnection,
  testApiConnection,
}: ServerConnectionCardProps) {
  const [componentsExpanded, setComponentsExpanded] = useState(true);
  const [updatingComponent, setUpdatingComponent] = useState<string | null>(null);
  const [updateLogs, setUpdateLogs] = useState<UpdateLog[]>([]);

  // SWR: fetch system components
  const { data, isLoading: loading, mutate } = useSWR(
    'system-components',
    async () => {
      const result = await getSystemComponents();
      return result.components;
    },
    { revalidateOnFocus: false, dedupingInterval: 0 }
  );
  const components = data ?? [];

  const handleUpdate = async (component: ComponentInfo) => {
    if (updatingComponent) return;

    setUpdatingComponent(component.name);
    setUpdateLogs([]);

    await updateSystemComponent(
      component.name,
      (event: UpdateProgressEvent) => {
        setUpdateLogs((prev) => [
          ...prev,
          {
            message: event.message,
            progress: event.progress ?? undefined,
            type: event.event_type === 'complete'
              ? 'complete'
              : event.event_type === 'error'
              ? 'error'
              : 'log',
          },
        ]);
      },
      async () => {
        toast.success(
          `${componentNames[component.name] || component.name} updated successfully!`
        );
        setUpdatingComponent(null);
        // Force SWR to refetch fresh data (bypass cache).
        await mutate(undefined, { revalidate: true });
      },
      (error: string) => {
        toast.error(`Update failed: ${error}`);
        setUpdatingComponent(null);
      }
    );
  };

  const getStatusIcon = (component: ComponentInfo) => {
    if (updatingComponent === component.name) {
      return <Loader className="h-3.5 w-3.5 animate-spin text-indigo-400" />;
    }
    if (component.status === 'update_available') {
      return <ArrowUp className="h-3.5 w-3.5 text-amber-400" />;
    }
    if (component.status === 'not_installed' || component.status === 'error') {
      return <AlertCircle className="h-3.5 w-3.5 text-red-400" />;
    }
    return <Check className="h-3.5 w-3.5 text-emerald-400" />;
  };

  const getStatusDot = (component: ComponentInfo) => {
    if (component.status === 'update_available') {
      return 'bg-amber-400';
    }
    if (component.status === 'not_installed' || component.status === 'error') {
      return 'bg-red-400';
    }
    return 'bg-emerald-400';
  };

  return (
    <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
      {/* Header */}
      <div className="flex items-center gap-3 mb-4">
        <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
          <Server className="h-5 w-5 text-indigo-400" />
        </div>
        <div>
          <h2 className="text-sm font-medium text-white">Server Connection</h2>
          <p className="text-xs text-white/40">Backend endpoint & system components</p>
        </div>
      </div>

      {/* API URL Input */}
      <div className="space-y-2">
        {/* Header row: Label + Status + Refresh */}
        <div className="flex items-center justify-between">
          <label className="text-xs font-medium text-white/60">
            API URL
          </label>
          <div className="flex items-center gap-2">
            {/* Status indicator */}
            {healthLoading ? (
              <span className="flex items-center gap-1.5 text-xs text-white/40">
                <RefreshCw className="h-3 w-3 animate-spin" />
                Checking...
              </span>
            ) : health ? (
              <span className="flex items-center gap-1.5 text-xs text-emerald-400">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                Connected (v{health.version})
              </span>
            ) : (
              <span className="flex items-center gap-1.5 text-xs text-red-400">
                <span className="h-1.5 w-1.5 rounded-full bg-red-400" />
                Disconnected
              </span>
            )}
            {/* Refresh button */}
            <button
              onClick={testApiConnection}
              disabled={testingConnection}
              className="p-1 rounded-md text-white/40 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
              title="Test connection"
            >
              <RefreshCw
                className={cn('h-3.5 w-3.5', testingConnection && 'animate-spin')}
              />
            </button>
          </div>
        </div>

        {/* URL input */}
        <input
          type="text"
          value={apiUrl}
          onChange={(e) => {
            setApiUrl(e.target.value);
            validateUrl(e.target.value);
          }}
          className={cn(
            'w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors',
            urlError
              ? 'border-red-500/50 focus:border-red-500/50'
              : 'border-white/[0.06] focus:border-indigo-500/50'
          )}
        />
        {urlError && <p className="mt-1.5 text-xs text-red-400">{urlError}</p>}
      </div>

      {/* Divider */}
      <div className="border-t border-white/[0.06] my-4" />

      {/* System Components Section */}
      <div>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-white/60">System Components</span>
            <span className="text-xs text-white/30">
              {components.length > 0 ? (
                // Show installed backends dynamically
                (() => {
                  const backends = [];
                  if (components.some(c => c.name === 'opencode' && c.installed)) backends.push('OpenCode');
                  if (components.some(c => c.name === 'claude_code' && c.installed)) backends.push('Claude Code');
                  return backends.length > 0 ? `${backends.join(' + ')} stack` : 'No backends';
                })()
              ) : 'Loading...'}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => mutate()}
              disabled={loading}
              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
            >
              <RefreshCw className={cn('h-3 w-3', loading && 'animate-spin')} />
              Refresh
            </button>
            <button
              onClick={() => setComponentsExpanded(!componentsExpanded)}
              className="p-1 rounded-lg text-white/40 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
            >
              {componentsExpanded ? (
                <ChevronUp className="h-4 w-4" />
              ) : (
                <ChevronDown className="h-4 w-4" />
              )}
            </button>
          </div>
        </div>

        {componentsExpanded && (
          <div className="space-y-2">
            {loading ? (
              <div className="flex items-center justify-center py-4">
                <Loader className="h-5 w-5 animate-spin text-white/40" />
              </div>
            ) : (
              components.map((component) => (
                <div
                  key={component.name}
                  className="group rounded-lg border border-white/[0.06] bg-white/[0.01] hover:bg-white/[0.02] transition-colors"
                >
                  <div className="flex items-center gap-3 px-3 py-2.5">
                    {/* Icon */}
                    <span className="text-base">
                      {componentIcons[component.name] || '📦'}
                    </span>

                    {/* Name & Version */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-sm text-white/80">
                          {componentNames[component.name] || component.name}
                        </span>
                        {component.version && (
                          <span className="text-xs text-white/40">
                            v{component.version}
                          </span>
                        )}
                      </div>
                      {component.update_available && (
                        <div className="text-xs text-amber-400/80 mt-0.5">
                          v{component.update_available} available
                        </div>
                      )}
                      {!component.installed && (
                        <div className="text-xs text-red-400/80 mt-0.5">
                          Not installed
                        </div>
                      )}
                    </div>

                    {/* Status */}
                    <div className="flex items-center gap-2">
                      {getStatusIcon(component)}
                      <span className={cn('h-1.5 w-1.5 rounded-full', getStatusDot(component))} />
                    </div>

                    {/* Update button */}
                    {component.status === 'update_available' && (
                      <button
                        onClick={() => handleUpdate(component)}
                        disabled={updatingComponent !== null}
                        className="flex items-center gap-1.5 rounded-lg bg-indigo-500/20 border border-indigo-500/30 px-2.5 py-1 text-xs text-indigo-300 hover:bg-indigo-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        <ArrowUp className="h-3 w-3" />
                        Update
                      </button>
                    )}

                    {/* Install button for not installed components */}
                    {component.status === 'not_installed' && (
                      <button
                        onClick={() => handleUpdate(component)}
                        disabled={updatingComponent !== null}
                        className="flex items-center gap-1.5 rounded-lg bg-emerald-500/20 border border-emerald-500/30 px-2.5 py-1 text-xs text-emerald-300 hover:bg-emerald-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        <ArrowUp className="h-3 w-3" />
                        Install
                      </button>
                    )}
                  </div>

                  {/* Update logs */}
                  {updatingComponent === component.name && updateLogs.length > 0 && (
                    <div className="border-t border-white/[0.06] px-3 py-2">
                      <div className="max-h-32 overflow-y-auto text-xs space-y-1 font-mono">
                        {updateLogs.map((log, i) => (
                          <div
                            key={i}
                            className={cn(
                              'flex items-start gap-2',
                              log.type === 'error' && 'text-red-400',
                              log.type === 'complete' && 'text-emerald-400',
                              log.type === 'log' && 'text-white/50'
                            )}
                          >
                            {log.progress !== undefined && (
                              <span className="text-white/30">[{log.progress}%]</span>
                            )}
                            <span className="break-all">{log.message}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        )}
      </div>
    </div>
  );
}
