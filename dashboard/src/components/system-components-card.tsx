'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from '@/components/toast';
import {
  getSystemComponents,
  updateSystemComponent,
  ComponentInfo,
  UpdateProgressEvent,
} from '@/lib/api';
import {
  Cpu,
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
  oh_my_opencode: 'oh-my-opencode',
};

// Component icons
const componentIcons: Record<string, string> = {
  open_agent: '🚀',
  opencode: '⚡',
  oh_my_opencode: '🎭',
};

interface UpdateLog {
  message: string;
  progress?: number;
  type: 'log' | 'complete' | 'error';
}

export function SystemComponentsCard() {
  const [components, setComponents] = useState<ComponentInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState(true);
  const [updatingComponent, setUpdatingComponent] = useState<string | null>(null);
  const [updateLogs, setUpdateLogs] = useState<UpdateLog[]>([]);

  const loadComponents = useCallback(async () => {
    try {
      setLoading(true);
      const data = await getSystemComponents();
      setComponents(data.components);
    } catch (err) {
      console.error('Failed to load system components:', err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadComponents();
  }, [loadComponents]);

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
        await loadComponents();
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
    if (component.status === 'not_installed') {
      return <AlertCircle className="h-3.5 w-3.5 text-red-400" />;
    }
    if (component.status === 'error') {
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
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-cyan-500/10">
            <Cpu className="h-5 w-5 text-cyan-400" />
          </div>
          <div>
            <h2 className="text-sm font-medium text-white">System Components</h2>
            <p className="text-xs text-white/40">OpenCode stack versions</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={loadComponents}
            disabled={loading}
            className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
          >
            <RefreshCw className={cn('h-3 w-3', loading && 'animate-spin')} />
            Refresh
          </button>
          <button
            onClick={() => setExpanded(!expanded)}
            className="p-1.5 rounded-lg text-white/40 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
          >
            {expanded ? (
              <ChevronUp className="h-4 w-4" />
            ) : (
              <ChevronDown className="h-4 w-4" />
            )}
          </button>
        </div>
      </div>

      {expanded && (
        <div className="space-y-2">
          {loading ? (
            <div className="flex items-center justify-center py-6">
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
                  {component.status === 'update_available' && component.name !== 'open_agent' && (
                    <button
                      onClick={() => handleUpdate(component)}
                      disabled={updatingComponent !== null}
                      className="flex items-center gap-1.5 rounded-lg bg-indigo-500/20 border border-indigo-500/30 px-2.5 py-1 text-xs text-indigo-300 hover:bg-indigo-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <ArrowUp className="h-3 w-3" />
                      Update
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
  );
}
