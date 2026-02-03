'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import Link from 'next/link';
import { AlertTriangle, Clock, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import { cn, formatRelativeTime } from '@/lib/utils';
import { useLibrary } from '@/contexts/library-context';
import { ShimmerCard } from '@/components/ui/shimmer';
import {
  type Automation,
  listMissionAutomations,
  createMissionAutomation,
  updateAutomationActive,
  deleteAutomation,
} from '@/lib/api';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { toast } from '@/components/toast';

export interface MissionAutomationsDialogProps {
  open: boolean;
  missionId: string | null;
  missionLabel?: string | null;
  onClose: () => void;
}

type IntervalUnit = 'seconds' | 'minutes' | 'hours' | 'days';

const UNIT_TO_SECONDS: Record<IntervalUnit, number> = {
  seconds: 1,
  minutes: 60,
  hours: 3600,
  days: 86400,
};

function formatInterval(seconds: number): string {
  if (seconds <= 0 || !Number.isFinite(seconds)) return '0s';
  if (seconds % 86400 === 0) return `${seconds / 86400}d`;
  if (seconds % 3600 === 0) return `${seconds / 3600}h`;
  if (seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

export function MissionAutomationsDialog({
  open,
  missionId,
  missionLabel,
  onClose,
}: MissionAutomationsDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const requestIdRef = useRef(0);
  const cacheRef = useRef<Map<string, Automation[]>>(new Map());
  const automationsRef = useRef<Automation[]>([]);
  const { commands, loading: commandsLoading, libraryUnavailable } = useLibrary();

  const [automations, setAutomations] = useState<Automation[]>([]);
  const [loading, setLoading] = useState(false);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [loadedMissionId, setLoadedMissionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    automationsRef.current = automations;
  }, [automations]);

  const [commandName, setCommandName] = useState('');
  const [intervalValue, setIntervalValue] = useState('5');
  const [intervalUnit, setIntervalUnit] = useState<IntervalUnit>('minutes');
  const [createActive, setCreateActive] = useState(true);
  const [creating, setCreating] = useState(false);
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Automation | null>(null);
  const [deleting, setDeleting] = useState(false);

  const commandsByName = useMemo(() => {
    return new Map(commands.map((command) => [command.name, command]));
  }, [commands]);

  const intervalSeconds = useMemo(() => {
    const value = Number(intervalValue);
    if (!Number.isFinite(value) || value <= 0) return 0;
    const unitMultiplier = UNIT_TO_SECONDS[intervalUnit];
    return Math.round(value * unitMultiplier);
  }, [intervalValue, intervalUnit]);

  const setAutomationsForMission = useCallback(
    (targetMissionId: string, nextAutomations: Automation[]) => {
      cacheRef.current.set(targetMissionId, nextAutomations);
      setAutomations(nextAutomations);
      setHasLoaded(true);
      setLoadedMissionId(targetMissionId);
    },
    []
  );

  const loadAutomations = useCallback(async (force = false) => {
    if (!missionId) {
      setAutomations([]);
      setHasLoaded(false);
      setLoadedMissionId(null);
      return;
    }
    const cached = cacheRef.current.get(missionId);
    if (cached && !force) {
      setAutomationsForMission(missionId, cached);
      return;
    }
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setLoading(true);
    setError(null);
    try {
      const data = await listMissionAutomations(missionId);
      if (requestIdRef.current !== requestId) return;
      setAutomationsForMission(missionId, data);
    } catch (err) {
      if (requestIdRef.current !== requestId) return;
      const message = err instanceof Error ? err.message : 'Failed to load automations';
      setError(message);
      setHasLoaded(true);
      setLoadedMissionId(missionId);
    } finally {
      if (requestIdRef.current === requestId) {
        setLoading(false);
      }
    }
  }, [missionId, setAutomationsForMission]);

  useEffect(() => {
    if (!open) return;
    if (!missionId) {
      setAutomations([]);
      setHasLoaded(false);
      setLoadedMissionId(null);
      return;
    }
    if (missionId !== loadedMissionId) {
      const cached = cacheRef.current.get(missionId);
      if (cached) {
        setAutomationsForMission(missionId, cached);
      } else {
        setAutomations([]);
        setHasLoaded(false);
        void loadAutomations();
      }
      return;
    }
    if (!hasLoaded) {
      void loadAutomations();
    }
  }, [open, missionId, loadedMissionId, hasLoaded, loadAutomations, setAutomationsForMission]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || !open) return;
      if (pendingDelete) {
        if (!deleting) setPendingDelete(null);
        return;
      }
      onClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose, pendingDelete, deleting]);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (pendingDelete) return;
      if (dialogRef.current && !dialogRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [open, onClose, pendingDelete]);

  if (!open) return null;

  const handleCreate = async () => {
    if (!missionId) return;
    const name = commandName.trim();
    if (!name) {
      toast.error('Select a command for the automation');
      return;
    }
    if (!intervalSeconds || intervalSeconds <= 0) {
      toast.error('Interval must be greater than zero');
      return;
    }
    setCreating(true);
    try {
      let created = await createMissionAutomation(missionId, {
        commandName: name,
        intervalSeconds,
      });
      if (!createActive) {
        created = await updateAutomationActive(created.id, false);
      }
      setAutomationsForMission(
        missionId,
        [created, ...automationsRef.current.filter((a) => a.id !== created.id)]
      );
      setCommandName('');
      setIntervalValue('5');
      setIntervalUnit('minutes');
      setCreateActive(true);
      toast.success('Automation created');
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to create automation';
      toast.error(message);
    } finally {
      setCreating(false);
    }
  };

  const handleToggle = async (automation: Automation, nextActive: boolean) => {
    setTogglingId(automation.id);
    try {
      const updated = await updateAutomationActive(automation.id, nextActive);
      if (missionId) {
        const next = automationsRef.current.map((item) =>
          item.id === automation.id ? updated : item
        );
        setAutomationsForMission(missionId, next);
      } else {
        setAutomations((prev) => prev.map((item) => (item.id === automation.id ? updated : item)));
      }
      toast.success(nextActive ? 'Automation enabled' : 'Automation paused');
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to update automation';
      toast.error(message);
    } finally {
      setTogglingId(null);
    }
  };

  const handleDelete = async () => {
    if (!pendingDelete) return;
    setDeleting(true);
    try {
      await deleteAutomation(pendingDelete.id);
      if (missionId) {
        const next = automationsRef.current.filter((item) => item.id !== pendingDelete.id);
        setAutomationsForMission(missionId, next);
      } else {
        setAutomations((prev) => prev.filter((item) => item.id !== pendingDelete.id));
      }
      toast.success('Automation deleted');
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to delete automation';
      toast.error(message);
    } finally {
      setDeleting(false);
      setPendingDelete(null);
    }
  };

  const allowCreate =
    !!missionId &&
    !libraryUnavailable &&
    !creating &&
    commandName.trim().length > 0 &&
    intervalSeconds > 0;

  const isMissionDataReady = !!missionId && loadedMissionId === missionId;
  const showLoadingPlaceholder = !!missionId && (!isMissionDataReady || (loading && !hasLoaded));
  const visibleAutomations = isMissionDataReady ? automations : [];
  const visibleError = isMissionDataReady ? error : null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />
      <div
        ref={dialogRef}
        className="relative w-full max-w-3xl max-h-[85vh] overflow-hidden rounded-2xl bg-[#1a1a1a] border border-white/[0.08] shadow-xl"
      >
        <div className="flex items-start justify-between gap-4 border-b border-white/[0.06] px-6 py-5">
          <div>
            <h3 className="text-lg font-semibold text-white">Mission Automations</h3>
            <p className="text-sm text-white/50">
              Schedule commands to send automated messages for this mission.
              {missionId && (
                <span className="ml-2 text-white/30">({missionLabel ?? missionId.slice(0, 8)})</span>
              )}
            </p>
          </div>
          <button
            onClick={onClose}
            className="rounded-lg p-1 text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="max-h-[calc(85vh-72px)] overflow-y-auto px-6 py-5 space-y-6">
          {!missionId && (
            <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-6 text-sm text-white/50">
              Select a mission to manage automations.
            </div>
          )}

          {missionId && (
            <>
              <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-4 space-y-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2 text-sm font-medium text-white">
                    <Clock className="h-4 w-4 text-indigo-400" />
                    Create Automation
                  </div>
                  <button
                    onClick={() => loadAutomations(true)}
                    className="flex items-center gap-2 text-xs text-white/50 hover:text-white/80 transition-colors"
                  >
                    <RefreshCw className={cn('h-3 w-3', loading && 'animate-spin')} />
                    Refresh
                  </button>
                </div>

                {libraryUnavailable && (
                  <div className="flex items-start gap-2 rounded-lg border border-amber-500/20 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                    <AlertTriangle className="h-3.5 w-3.5 mt-0.5" />
                    <span>
                      Library is not configured. Set it up in Settings to access commands.
                    </span>
                  </div>
                )}

                <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Command</label>
                    <input
                      list="automation-command-list"
                      value={commandName}
                      onChange={(e) => setCommandName(e.target.value)}
                      placeholder={commandsLoading ? 'Loading commands…' : 'Select or type a command'}
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                    />
                    <datalist id="automation-command-list">
                      {commands.map((command) => (
                        <option key={command.name} value={command.name} />
                      ))}
                    </datalist>
                    <div className="mt-1 text-[11px] text-white/30">
                      {commandName && commandsByName.get(commandName)?.description}
                      {!commandName && (
                        <span>
                          Choose from library commands.
                          <Link
                            href="/config/commands"
                            className="ml-1 text-indigo-400 hover:text-indigo-300"
                          >
                            Manage commands
                          </Link>
                        </span>
                      )}
                    </div>
                  </div>

                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Interval</label>
                    <div className="flex gap-2">
                      <input
                        type="number"
                        min={1}
                        value={intervalValue}
                        onChange={(e) => setIntervalValue(e.target.value)}
                        className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                      />
                      <select
                        value={intervalUnit}
                        onChange={(e) => setIntervalUnit(e.target.value as IntervalUnit)}
                        className="w-32 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer"
                        style={{
                          backgroundImage:
                            "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
                          backgroundPosition: 'right 0.5rem center',
                          backgroundRepeat: 'no-repeat',
                          backgroundSize: '1.5em 1.5em',
                          paddingRight: '2.5rem',
                        }}
                      >
                        <option value="seconds" className="bg-[#1a1a1a]">seconds</option>
                        <option value="minutes" className="bg-[#1a1a1a]">minutes</option>
                        <option value="hours" className="bg-[#1a1a1a]">hours</option>
                        <option value="days" className="bg-[#1a1a1a]">days</option>
                      </select>
                    </div>
                    <div className="mt-1 text-[11px] text-white/30">
                      Runs every {formatInterval(intervalSeconds)}
                    </div>
                  </div>
                </div>

                <label className="flex items-center gap-2 text-xs text-white/60">
                  <input
                    type="checkbox"
                    checked={createActive}
                    onChange={(e) => setCreateActive(e.target.checked)}
                    className="rounded border-white/20"
                  />
                  Active immediately
                </label>

                <div className="flex justify-end">
                  <button
                    onClick={handleCreate}
                    disabled={!allowCreate}
                    className="flex items-center gap-2 rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2 text-sm font-medium text-white transition-colors disabled:opacity-50"
                  >
                    {creating ? (
                      <RefreshCw className="h-4 w-4 animate-spin" />
                    ) : (
                      <Plus className="h-4 w-4" />
                    )}
                    Create automation
                  </button>
                </div>
              </div>

              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <h4 className="text-sm font-medium text-white">Current Automations</h4>
                  {loading && (
                    <span className="text-xs text-white/40">Loading…</span>
                  )}
                </div>

                {visibleError && (
                  <div className="rounded-lg border border-red-500/20 bg-red-500/10 px-3 py-2 text-xs text-red-200">
                    {visibleError}
                  </div>
                )}

                {showLoadingPlaceholder && (
                  <div className="space-y-3">
                    <ShimmerCard />
                    <ShimmerCard />
                  </div>
                )}

                {isMissionDataReady && !loading && visibleAutomations.length === 0 && !visibleError && (
                  <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6 text-center text-sm text-white/40">
                    No automations yet. Create one to start scheduled command runs.
                  </div>
                )}

                <div className="space-y-2">
                  {visibleAutomations.map((automation) => {
                    const command = commandsByName.get(automation.command_name);
                    const lastRunLabel = automation.last_triggered_at
                      ? formatRelativeTime(new Date(automation.last_triggered_at))
                      : 'never';

                    return (
                      <div
                        key={automation.id}
                        className="flex flex-col gap-3 rounded-xl border border-white/[0.08] bg-white/[0.02] p-4 md:flex-row md:items-center md:justify-between"
                      >
                        <div className="space-y-1">
                          <div className="flex items-center gap-2">
                            <span className="text-sm font-medium text-white">{automation.command_name}</span>
                            {!command && (
                              <span className="flex items-center gap-1 text-[11px] text-amber-300">
                                <AlertTriangle className="h-3 w-3" />
                                Missing command
                              </span>
                            )}
                          </div>
                          {command?.description && (
                            <div className="text-xs text-white/40">{command.description}</div>
                          )}
                          <div className="text-xs text-white/40">
                            Every {formatInterval(automation.interval_seconds)} · Last run {lastRunLabel}
                          </div>
                        </div>

                        <div className="flex items-center gap-3">
                          <label className="flex items-center gap-2 text-xs text-white/60">
                            <input
                              type="checkbox"
                              checked={automation.active}
                              onChange={(e) => handleToggle(automation, e.target.checked)}
                              disabled={togglingId === automation.id}
                              className="rounded border-white/20"
                            />
                            {automation.active ? 'Active' : 'Paused'}
                          </label>
                          <button
                            onClick={() => setPendingDelete(automation)}
                            className="flex items-center gap-1.5 rounded-lg border border-white/[0.08] px-3 py-1.5 text-xs text-white/60 hover:text-red-300 hover:border-red-500/40 hover:bg-red-500/10 transition-colors"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                            Delete
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            </>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={!!pendingDelete}
        title={`Delete automation “${pendingDelete?.command_name ?? ''}”?`}
        description="This will permanently remove the automation and stop scheduled runs."
        confirmLabel={deleting ? 'Deleting…' : 'Delete'}
        variant="danger"
        onConfirm={handleDelete}
        onCancel={() => {
          if (deleting) return;
          setPendingDelete(null);
        }}
      />
    </div>
  );
}
