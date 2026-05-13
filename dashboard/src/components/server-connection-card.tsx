'use client';

import { useState } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  getSystemComponents,
  getComponentsByWorkspace,
  updateSystemComponent,
  uninstallSystemComponent,
  ComponentInfo,
  ComponentWorkspaceReport,
  WorkspaceComponentInfo,
  UpdateProgressEvent,
} from '@/lib/api';
import {
  Server,
  RefreshCw,
  ArrowUp,
  Loader,
  ChevronDown,
  ChevronUp,
  Trash2,
} from 'lucide-react';
import { cn } from '@/lib/utils';

// Component display names
const componentNames: Record<string, string> = {
  open_agent: 'sandboxed.sh',
  sandboxed_sh: 'sandboxed.sh',
  opencode: 'OpenCode',
  claude_code: 'Claude Code',
  codex: 'Codex',
  amp: 'Amp',
  oh_my_opencode: 'oh-my-opencode',
};

// Component icons
const componentIcons: Record<string, string> = {
  open_agent: '🚀',
  sandboxed_sh: '🚀',
  opencode: '⚡',
  claude_code: '🤖',
  codex: '🧠',
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

// A logical "in-flight" operation key — either a host component update
// (component name) or a per-workspace update (`${component}:${workspaceId}`).
type OpKey = string;

function workspaceOpKey(component: string, workspaceId: string): OpKey {
  return `${component}:${workspaceId}`;
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
  const [activeOp, setActiveOp] = useState<OpKey | null>(null);
  const [activeOpKind, setActiveOpKind] = useState<'update' | 'uninstall' | null>(null);
  const [updateLogs, setUpdateLogs] = useState<UpdateLog[]>([]);
  const [expandedRows, setExpandedRows] = useState<Record<string, boolean>>({});

  // Fetch the legacy host-only summary AND the new per-workspace report in parallel.
  // The legacy endpoint still owns host-only components (sandboxed.sh, oh-my-opencode).
  const { data: legacyData, isLoading: legacyLoading, mutate: mutateLegacy } = useSWR(
    'system-components',
    async () => (await getSystemComponents()).components,
    { revalidateOnFocus: false, dedupingInterval: 0 }
  );
  const { data: wsData, isLoading: wsLoading, mutate: mutateWs } = useSWR(
    'system-components-by-workspace',
    async () => (await getComponentsByWorkspace()).components,
    { revalidateOnFocus: false, dedupingInterval: 0 }
  );

  const components = legacyData ?? [];
  const workspaceReports = wsData ?? [];
  const wsByName = new Map<string, ComponentWorkspaceReport>(
    workspaceReports.map((r) => [r.name, r])
  );
  const loading = legacyLoading || wsLoading;
  const refreshAll = () => {
    mutateLegacy(undefined, { revalidate: true });
    mutateWs(undefined, { revalidate: true });
  };

  // ---- operation runner --------------------------------------------------

  const runOperation = async (
    opKey: OpKey,
    kind: 'update' | 'uninstall',
    operationFn: (
      onProgress: (event: UpdateProgressEvent) => void,
      onComplete: () => Promise<void>,
      onError: (error: string) => void
    ) => Promise<void>,
    successLabel: string
  ) => {
    if (activeOp) return;
    setActiveOp(opKey);
    setActiveOpKind(kind);
    setUpdateLogs([]);

    await operationFn(
      (event) => {
        setUpdateLogs((prev) => [
          ...prev,
          {
            message: event.message,
            progress: event.progress ?? undefined,
            type:
              event.event_type === 'complete'
                ? 'complete'
                : event.event_type === 'error'
                ? 'error'
                : 'log',
          },
        ]);
      },
      async () => {
        toast.success(`${successLabel} ${kind === 'update' ? 'updated' : 'uninstalled'} successfully!`);
        setActiveOp(null);
        setActiveOpKind(null);
        refreshAll();
      },
      (error) => {
        toast.error(`${kind === 'update' ? 'Update' : 'Uninstall'} failed: ${error}`);
        setActiveOp(null);
        setActiveOpKind(null);
      }
    );
  };

  const handleHostUpdate = (component: ComponentInfo) => {
    runOperation(
      component.name,
      'update',
      (onP, onC, onE) => updateSystemComponent(component.name, onP, onC, onE),
      componentNames[component.name] || component.name
    );
  };

  const handleHostUninstall = (component: ComponentInfo) => {
    if (component.name === 'sandboxed_sh') {
      toast.error('Cannot uninstall sandboxed.sh - it is the main application');
      return;
    }
    runOperation(
      component.name,
      'uninstall',
      (onP, onC, onE) => uninstallSystemComponent(component.name, onP, onC, onE),
      componentNames[component.name] || component.name
    );
  };

  const handleWorkspaceUpdate = (componentName: string, ws: WorkspaceComponentInfo) => {
    runOperation(
      workspaceOpKey(componentName, ws.workspace_id),
      'update',
      (onP, onC, onE) =>
        updateSystemComponent(componentName, onP, onC, onE, ws.workspace_id),
      `${componentNames[componentName] || componentName} in '${ws.workspace_name}'`
    );
  };

  const handleSyncAll = (report: ComponentWorkspaceReport) => {
    const outOfSync = report.workspaces.filter((w) => !w.in_sync);
    if (outOfSync.length === 0) return;
    // Sequential to avoid concurrent installs racing the same package manager.
    void (async () => {
      for (const ws of outOfSync) {
        // Skip workspaces that aren't ready — the user gets a clear note next to them.
        if (ws.workspace_status !== 'ready') continue;
        await new Promise<void>((resolve) => {
          runOperation(
            workspaceOpKey(report.name, ws.workspace_id),
            'update',
            (onP, onC, onE) =>
              updateSystemComponent(
                report.name,
                onP,
                async () => {
                  await onC();
                  resolve();
                },
                (err) => {
                  onE(err);
                  resolve();
                },
                ws.workspace_id
              ),
            `${componentNames[report.name] || report.name} in '${ws.workspace_name}'`
          );
        });
      }
    })();
  };

  // ---- status helpers ----------------------------------------------------

  const componentSyncSummary = (
    component: ComponentInfo,
    report: ComponentWorkspaceReport | undefined
  ) => {
    if (!report || report.workspaces.length === 0) {
      // No workspace-level data: surface host status only.
      if (component.status === 'update_available') {
        return { label: 'Update available', tone: 'amber' as const };
      }
      if (component.status === 'not_installed' || component.status === 'error') {
        return { label: 'Not installed', tone: 'red' as const };
      }
      return { label: 'Synced', tone: 'emerald' as const };
    }
    const total = report.workspaces.length;
    const synced = report.workspaces.filter((w) => w.in_sync).length;
    const hostBehindUpstream = !!report.host_update_available;
    if (synced === total && !hostBehindUpstream) {
      return { label: `All ${total} synced`, tone: 'emerald' as const };
    }
    if (synced === total && hostBehindUpstream) {
      return { label: `${total}/${total} on host, upstream newer`, tone: 'amber' as const };
    }
    return {
      label: `${synced}/${total} synced`,
      tone: synced === 0 ? ('red' as const) : ('amber' as const),
    };
  };

  const toneBadgeClass = (tone: 'emerald' | 'amber' | 'red') =>
    tone === 'emerald'
      ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-300'
      : tone === 'amber'
      ? 'border-amber-500/30 bg-amber-500/10 text-amber-300'
      : 'border-red-500/30 bg-red-500/10 text-red-300';

  const toneDotClass = (tone: 'emerald' | 'amber' | 'red') =>
    tone === 'emerald' ? 'bg-emerald-400' : tone === 'amber' ? 'bg-amber-400' : 'bg-red-400';

  const isOpInProgress = (key: OpKey) => activeOp === key && activeOpKind === 'update';

  // Default rows to expanded when the component has any drift, so the user
  // immediately sees the problem the page is meant to expose.
  const rowExpanded = (component: ComponentInfo, report?: ComponentWorkspaceReport) => {
    if (component.name in expandedRows) return expandedRows[component.name];
    if (!report) return false;
    const hasDrift = report.workspaces.some((w) => !w.in_sync);
    return hasDrift;
  };

  const toggleRow = (name: string, current: boolean) =>
    setExpandedRows((prev) => ({ ...prev, [name]: !current }));

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
        <div className="flex items-center justify-between">
          <label className="text-xs font-medium text-white/60">API URL</label>
          <div className="flex items-center gap-2">
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
            <button
              onClick={testApiConnection}
              disabled={testingConnection}
              className="p-1 rounded-md text-white/40 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
              title="Test connection"
            >
              <RefreshCw className={cn('h-3.5 w-3.5', testingConnection && 'animate-spin')} />
            </button>
          </div>
        </div>

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

      <div className="border-t border-white/[0.06] my-4" />

      {/* System Components Section */}
      <div>
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-white/60">System Components</span>
            <span className="text-xs text-white/30">
              {components.length > 0
                ? (() => {
                    const backends = [];
                    if (components.some((c) => c.name === 'opencode' && c.installed)) backends.push('OpenCode');
                    if (components.some((c) => c.name === 'claude_code' && c.installed)) backends.push('Claude Code');
                    if (components.some((c) => c.name === 'codex' && c.installed)) backends.push('Codex');
                    return backends.length > 0 ? `${backends.join(' + ')} stack` : 'No backends';
                  })()
                : 'Loading...'}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={refreshAll}
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
              {componentsExpanded ? <ChevronUp className="h-4 w-4" /> : <ChevronDown className="h-4 w-4" />}
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
              components.map((component) => {
                const report = wsByName.get(component.name);
                const summary = componentSyncSummary(component, report);
                const outOfSync = report ? report.workspaces.filter((w) => !w.in_sync) : [];
                const isExpanded = rowExpanded(component, report);
                const hostOpInFlight = isOpInProgress(component.name);
                const updateAvailableLine = component.update_available
                  ? `v${component.update_available} available upstream`
                  : null;

                return (
                  <div
                    key={component.name}
                    className="group rounded-lg border border-white/[0.06] bg-white/[0.01] hover:bg-white/[0.02] transition-colors"
                  >
                    {/* Aggregate row */}
                    <div
                      role={report ? 'button' : undefined}
                      tabIndex={report ? 0 : undefined}
                      onClick={() => {
                        if (report) toggleRow(component.name, isExpanded);
                      }}
                      onKeyDown={(e) => {
                        if (!report) return;
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault();
                          toggleRow(component.name, isExpanded);
                        }
                      }}
                      className={cn(
                        'w-full flex items-center gap-3 px-3 py-2.5 text-left',
                        report ? 'cursor-pointer' : 'cursor-default'
                      )}
                    >
                      <span className="text-base">{componentIcons[component.name] || '📦'}</span>

                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-sm text-white/80">
                            {componentNames[component.name] || component.name}
                          </span>
                          {component.version && (
                            <span className="text-xs text-white/40">v{component.version}</span>
                          )}
                          <span
                            className={cn(
                              'inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-[10px] font-medium',
                              toneBadgeClass(summary.tone)
                            )}
                          >
                            <span className={cn('h-1.5 w-1.5 rounded-full', toneDotClass(summary.tone))} />
                            {summary.label}
                          </span>
                        </div>
                        {updateAvailableLine && (
                          <div className="text-xs text-amber-400/80 mt-0.5">{updateAvailableLine}</div>
                        )}
                        {!component.installed && (
                          <div className="text-xs text-red-400/80 mt-0.5">Not installed on host</div>
                        )}
                      </div>

                      <div className="flex items-center gap-2" onClick={(e) => e.stopPropagation()}>
                        {/* Host-level Update/Install button stays available even when collapsed. */}
                        {component.status === 'update_available' && (
                          <button
                            onClick={() => handleHostUpdate(component)}
                            disabled={activeOp !== null}
                            className="flex items-center gap-1.5 rounded-lg bg-indigo-500/20 border border-indigo-500/30 px-2.5 py-1 text-xs text-indigo-300 hover:bg-indigo-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                            title="Update host installation"
                          >
                            {hostOpInFlight ? (
                              <Loader className="h-3 w-3 animate-spin" />
                            ) : (
                              <ArrowUp className="h-3 w-3" />
                            )}
                            Update host
                          </button>
                        )}
                        {component.status === 'not_installed' && (
                          <button
                            onClick={() => handleHostUpdate(component)}
                            disabled={activeOp !== null}
                            className="flex items-center gap-1.5 rounded-lg bg-emerald-500/20 border border-emerald-500/30 px-2.5 py-1 text-xs text-emerald-300 hover:bg-emerald-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                          >
                            <ArrowUp className="h-3 w-3" />
                            Install
                          </button>
                        )}
                        {report && outOfSync.length > 0 && (
                          <button
                            onClick={() => handleSyncAll(report)}
                            disabled={activeOp !== null}
                            className="flex items-center gap-1.5 rounded-lg bg-amber-500/20 border border-amber-500/30 px-2.5 py-1 text-xs text-amber-300 hover:bg-amber-500/30 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                            title="Sync all out-of-sync workspaces"
                          >
                            <ArrowUp className="h-3 w-3" />
                            Sync {outOfSync.length}
                          </button>
                        )}
                        {component.installed && component.name !== 'sandboxed_sh' && (
                          <button
                            onClick={() => handleHostUninstall(component)}
                            disabled={activeOp !== null}
                            className="p-1.5 rounded-lg text-white/30 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                            title={`Uninstall ${componentNames[component.name] || component.name}`}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </button>
                        )}
                        {report && (
                          <span className="p-1 text-white/40">
                            {isExpanded ? (
                              <ChevronUp className="h-3.5 w-3.5" />
                            ) : (
                              <ChevronDown className="h-3.5 w-3.5" />
                            )}
                          </span>
                        )}
                      </div>
                    </div>

                    {/* Per-workspace detail rows */}
                    {report && isExpanded && (
                      <div className="border-t border-white/[0.06] px-3 py-2 space-y-1.5">
                        <div className="text-[10px] uppercase tracking-wider text-white/30 px-1">
                          Workspaces
                        </div>
                        {report.workspaces.map((ws) => {
                          const opKey = workspaceOpKey(component.name, ws.workspace_id);
                          const inFlight = isOpInProgress(opKey);
                          const versionLabel = ws.version ? `v${ws.version}` : 'not installed';
                          const dotTone = ws.in_sync ? 'bg-emerald-400' : ws.version ? 'bg-amber-400' : 'bg-red-400';
                          return (
                            <div key={ws.workspace_id} className="flex items-center gap-2 text-xs">
                              <span className={cn('h-1.5 w-1.5 rounded-full shrink-0', dotTone)} />
                              <span className="text-white/70 truncate">{ws.workspace_name}</span>
                              <span className="text-white/30 shrink-0">
                                {ws.workspace_type === 'host' ? 'host' : 'container'}
                              </span>
                              <span
                                className={cn(
                                  'shrink-0',
                                  ws.in_sync ? 'text-emerald-300/80' : 'text-amber-300/80'
                                )}
                              >
                                {versionLabel}
                              </span>
                              {ws.note && (
                                <span className="text-white/40 shrink-0 italic">({ws.note})</span>
                              )}
                              <span className="flex-1" />
                              {!ws.in_sync && ws.workspace_status === 'ready' && ws.workspace_type === 'container' && (
                                <button
                                  onClick={() => handleWorkspaceUpdate(component.name, ws)}
                                  disabled={activeOp !== null}
                                  className="flex items-center gap-1.5 rounded-md bg-indigo-500/15 border border-indigo-500/25 px-2 py-0.5 text-[10px] text-indigo-300 hover:bg-indigo-500/25 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                  {inFlight ? (
                                    <Loader className="h-3 w-3 animate-spin" />
                                  ) : (
                                    <ArrowUp className="h-3 w-3" />
                                  )}
                                  Sync
                                </button>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    )}

                    {/* In-flight operation logs (host or workspace) */}
                    {activeOp &&
                      (activeOp === component.name ||
                        activeOp.startsWith(`${component.name}:`)) &&
                      updateLogs.length > 0 && (
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
                );
              })
            )}
          </div>
        )}
      </div>
    </div>
  );
}
