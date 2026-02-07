'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import Link from 'next/link';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  Clock,
  Copy,
  Globe,
  History,
  Plus,
  RefreshCw,
  Trash2,
  X,
} from 'lucide-react';
import { cn, formatRelativeTime } from '@/lib/utils';
import { useLibrary } from '@/contexts/library-context';
import { ShimmerCard } from '@/components/ui/shimmer';
import {
  type Automation,
  type AutomationExecution,
  type CommandSource,
  type CreateAutomationInput,
  type TriggerType,
  listMissionAutomations,
  createMissionAutomation,
  updateAutomation,
  deleteAutomation,
  getAutomationExecutions,
} from '@/lib/api';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { toast } from '@/components/toast';
import { getRuntimeApiBase } from '@/lib/settings';

export interface MissionAutomationsDialogProps {
  open: boolean;
  missionId: string | null;
  missionLabel?: string | null;
  onClose: () => void;
}

type IntervalUnit = 'seconds' | 'minutes' | 'hours' | 'days';
type CommandSourceType = 'library' | 'inline';
type TriggerKind = 'interval' | 'webhook';

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

function buildWebhookUrl(missionId: string, webhookId: string): string {
  const base = getRuntimeApiBase();
  return `${base}/api/webhooks/${missionId}/${webhookId}`;
}

const STATUS_STYLES: Record<string, string> = {
  success: 'text-emerald-400',
  failed: 'text-red-400',
  running: 'text-blue-400',
  pending: 'text-yellow-400',
  cancelled: 'text-white/40',
  skipped: 'text-white/30',
};

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

  // -- Automations state --
  const [automations, setAutomations] = useState<Automation[]>([]);
  const [loading, setLoading] = useState(false);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [loadedMissionId, setLoadedMissionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    automationsRef.current = automations;
  }, [automations]);

  // -- Create form state --
  const [commandSourceType, setCommandSourceType] = useState<CommandSourceType>('library');
  const [commandName, setCommandName] = useState('');
  const [inlinePrompt, setInlinePrompt] = useState('');
  const [triggerKind, setTriggerKind] = useState<TriggerKind>('interval');
  const [intervalValue, setIntervalValue] = useState('5');
  const [intervalUnit, setIntervalUnit] = useState<IntervalUnit>('minutes');
  const [variables, setVariables] = useState<Array<{ key: string; value: string }>>([]);
  const [creating, setCreating] = useState(false);
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Automation | null>(null);
  const [deleting, setDeleting] = useState(false);

  // -- Execution history --
  const [expandedAutomationId, setExpandedAutomationId] = useState<string | null>(null);
  const [executions, setExecutions] = useState<AutomationExecution[]>([]);
  const [executionsLoading, setExecutionsLoading] = useState(false);

  // -- Clipboard --
  const [copiedWebhookId, setCopiedWebhookId] = useState<string | null>(null);

  const commandsByName = useMemo(() => {
    return new Map(commands.map((command) => [command.name, command]));
  }, [commands]);

  const intervalSeconds = useMemo(() => {
    const value = Number(intervalValue);
    if (!Number.isFinite(value) || value <= 0) return 0;
    return Math.round(value * UNIT_TO_SECONDS[intervalUnit]);
  }, [intervalValue, intervalUnit]);

  const getAutomationLabel = useCallback((automation: Automation) => {
    if (automation.command_source?.type === 'library') {
      return automation.command_source.name;
    }
    if (automation.command_source?.type === 'local_file') {
      return automation.command_source.path;
    }
    if (automation.command_source?.type === 'inline') {
      const content = automation.command_source.content;
      return content.length > 60 ? content.slice(0, 57) + '...' : content;
    }
    return 'Command';
  }, []);

  const getAutomationSourceTag = useCallback((automation: Automation) => {
    if (automation.command_source?.type === 'library') return 'Library';
    if (automation.command_source?.type === 'inline') return 'Prompt';
    if (automation.command_source?.type === 'local_file') return 'File';
    return '';
  }, []);

  const getAutomationScheduleLabel = useCallback((automation: Automation) => {
    if (automation.trigger?.type === 'interval') {
      return `Every ${formatInterval(automation.trigger.seconds)}`;
    }
    if (automation.trigger?.type === 'webhook') {
      return 'Webhook';
    }
    return 'Unknown';
  }, []);

  // -- Data loading --
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
      if (requestIdRef.current === requestId) setLoading(false);
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
    if (!hasLoaded) void loadAutomations();
  }, [open, missionId, loadedMissionId, hasLoaded, loadAutomations, setAutomationsForMission]);

  // -- Keyboard / click-outside --
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

  // -- Handlers --
  const handleCreate = async () => {
    if (!missionId) return;

    // Build command source
    let command_source: CommandSource;
    if (commandSourceType === 'library') {
      const name = commandName.trim();
      if (!name) {
        toast.error('Select a command for the automation');
        return;
      }
      command_source = { type: 'library', name };
    } else {
      const content = inlinePrompt.trim();
      if (!content) {
        toast.error('Enter a prompt for the automation');
        return;
      }
      command_source = { type: 'inline', content };
    }

    // Build trigger
    let trigger: TriggerType;
    if (triggerKind === 'interval') {
      if (!intervalSeconds || intervalSeconds <= 0) {
        toast.error('Interval must be greater than zero');
        return;
      }
      trigger = { type: 'interval', seconds: intervalSeconds };
    } else {
      trigger = {
        type: 'webhook',
        config: { webhook_id: '' }, // server generates it
      };
    }

    // Build variables
    const vars: Record<string, string> = {};
    for (const v of variables) {
      const k = v.key.trim();
      if (k) vars[k] = v.value;
    }

    const input: CreateAutomationInput = {
      command_source,
      trigger,
      ...(Object.keys(vars).length > 0 ? { variables: vars } : {}),
    };

    setCreating(true);
    try {
      const created = await createMissionAutomation(missionId, input);
      setAutomationsForMission(missionId, [
        created,
        ...automationsRef.current.filter((a) => a.id !== created.id),
      ]);
      // Reset form
      setCommandName('');
      setInlinePrompt('');
      setIntervalValue('5');
      setIntervalUnit('minutes');
      setVariables([]);
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
      const updated = await updateAutomation(automation.id, { active: nextActive });
      if (missionId) {
        const next = automationsRef.current.map((item) =>
          item.id === automation.id ? updated : item
        );
        setAutomationsForMission(missionId, next);
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

  const handleToggleExecutions = async (automationId: string) => {
    if (expandedAutomationId === automationId) {
      setExpandedAutomationId(null);
      setExecutions([]);
      return;
    }
    setExpandedAutomationId(automationId);
    setExecutionsLoading(true);
    try {
      const data = await getAutomationExecutions(automationId);
      setExecutions(data);
    } catch {
      setExecutions([]);
    } finally {
      setExecutionsLoading(false);
    }
  };

  const handleCopyWebhookUrl = (url: string, automationId: string) => {
    navigator.clipboard.writeText(url).then(() => {
      setCopiedWebhookId(automationId);
      setTimeout(() => setCopiedWebhookId(null), 2000);
    });
  };

  const handleAddVariable = () => {
    setVariables([...variables, { key: '', value: '' }]);
  };

  const handleRemoveVariable = (index: number) => {
    setVariables(variables.filter((_, i) => i !== index));
  };

  const handleVariableChange = (index: number, field: 'key' | 'value', val: string) => {
    setVariables(variables.map((v, i) => (i === index ? { ...v, [field]: val } : v)));
  };

  // -- Validation --
  const isCommandValid =
    commandSourceType === 'library' ? commandName.trim().length > 0 : inlinePrompt.trim().length > 0;
  const isTriggerValid = triggerKind === 'webhook' || intervalSeconds > 0;
  const allowCreate = !!missionId && !creating && isCommandValid && isTriggerValid;

  const isMissionDataReady = !!missionId && loadedMissionId === missionId;
  const showLoadingPlaceholder = !!missionId && (!isMissionDataReady || (loading && !hasLoaded));
  const visibleAutomations = isMissionDataReady ? automations : [];
  const visibleError = isMissionDataReady ? error : null;

  const selectClass =
    'rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50 appearance-none cursor-pointer';
  const selectStyle = {
    backgroundImage:
      "url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 20 20'%3e%3cpath stroke='%236b7280' stroke-linecap='round' stroke-linejoin='round' stroke-width='1.5' d='M6 8l4 4 4-4'/%3e%3c/svg%3e\")",
    backgroundPosition: 'right 0.5rem center',
    backgroundRepeat: 'no-repeat',
    backgroundSize: '1.5em 1.5em',
    paddingRight: '2.5rem',
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />
      <div
        ref={dialogRef}
        className="relative w-full max-w-3xl max-h-[85vh] overflow-hidden rounded-2xl bg-[#1a1a1a] border border-white/[0.08] shadow-xl"
      >
        {/* Header */}
        <div className="flex items-start justify-between gap-4 border-b border-white/[0.06] px-6 py-5">
          <div>
            <h3 className="text-lg font-semibold text-white">Mission Automations</h3>
            <p className="text-sm text-white/50">
              Schedule commands or prompts to run automatically.
              {missionId && (
                <span className="ml-2 text-white/30">
                  ({missionLabel ?? missionId.slice(0, 8)})
                </span>
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

        {/* Body */}
        <div className="max-h-[calc(85vh-72px)] overflow-y-auto px-6 py-5 space-y-6">
          {!missionId && (
            <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-6 text-sm text-white/50">
              Select a mission to manage automations.
            </div>
          )}

          {missionId && (
            <>
              {/* ---- Create form ---- */}
              <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-4 space-y-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2 text-sm font-medium text-white">
                    <Plus className="h-4 w-4 text-indigo-400" />
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

                {libraryUnavailable && commandSourceType === 'library' && (
                  <div className="flex items-start gap-2 rounded-lg border border-amber-500/20 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
                    <AlertTriangle className="h-3.5 w-3.5 mt-0.5" />
                    <span>
                      Library is not configured. Set it up in Settings to access commands, or use an
                      inline prompt instead.
                    </span>
                  </div>
                )}

                {/* Row 1: Command source type + Trigger type */}
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Source</label>
                    <select
                      value={commandSourceType}
                      onChange={(e) => setCommandSourceType(e.target.value as CommandSourceType)}
                      className={cn(selectClass, 'w-full')}
                      style={selectStyle}
                    >
                      <option value="library" className="bg-[#1a1a1a]">
                        Library command
                      </option>
                      <option value="inline" className="bg-[#1a1a1a]">
                        Inline prompt
                      </option>
                    </select>
                  </div>
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Trigger</label>
                    <select
                      value={triggerKind}
                      onChange={(e) => setTriggerKind(e.target.value as TriggerKind)}
                      className={cn(selectClass, 'w-full')}
                      style={selectStyle}
                    >
                      <option value="interval" className="bg-[#1a1a1a]">
                        Interval (time-based)
                      </option>
                      <option value="webhook" className="bg-[#1a1a1a]">
                        Webhook (API call)
                      </option>
                    </select>
                  </div>
                </div>

                {/* Row 2: Command details */}
                {commandSourceType === 'library' ? (
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Command</label>
                    <input
                      list="automation-command-list"
                      value={commandName}
                      onChange={(e) => setCommandName(e.target.value)}
                      placeholder={
                        commandsLoading ? 'Loading commands...' : 'Select or type a command'
                      }
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 appearance-none"
                      style={selectStyle}
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
                          Choose from library commands.{' '}
                          <Link
                            href="/config/commands"
                            className="text-indigo-400 hover:text-indigo-300"
                          >
                            Manage commands
                          </Link>
                        </span>
                      )}
                    </div>
                  </div>
                ) : (
                  <div>
                    <label className="block text-xs text-white/50 mb-1.5">Prompt</label>
                    <textarea
                      value={inlinePrompt}
                      onChange={(e) => setInlinePrompt(e.target.value)}
                      placeholder="Enter the prompt to send to the agent. Use <variable_name/> for variables."
                      rows={3}
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 resize-y"
                    />
                    <div className="mt-1 text-[11px] text-white/30">
                      Use <code className="text-indigo-400/70">&lt;variable_name/&gt;</code> to
                      insert variables. Built-in:{' '}
                      <code className="text-white/40">&lt;timestamp/&gt;</code>,{' '}
                      <code className="text-white/40">&lt;date/&gt;</code>,{' '}
                      <code className="text-white/40">&lt;mission_id/&gt;</code>
                    </div>
                  </div>
                )}

                {/* Row 3: Interval config (only for interval trigger) */}
                {triggerKind === 'interval' && (
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
                        className={cn(selectClass, 'w-32')}
                        style={selectStyle}
                      >
                        <option value="seconds" className="bg-[#1a1a1a]">
                          seconds
                        </option>
                        <option value="minutes" className="bg-[#1a1a1a]">
                          minutes
                        </option>
                        <option value="hours" className="bg-[#1a1a1a]">
                          hours
                        </option>
                        <option value="days" className="bg-[#1a1a1a]">
                          days
                        </option>
                      </select>
                    </div>
                    <div className="mt-1 text-[11px] text-white/30">
                      Runs every {formatInterval(intervalSeconds)}
                    </div>
                  </div>
                )}

                {triggerKind === 'webhook' && (
                  <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-xs text-white/40">
                    The webhook URL will be generated after creation. You can then call it via HTTP
                    POST with a JSON body. Variables from the payload can be mapped using{' '}
                    <code className="text-indigo-400/70">&lt;webhook.field.path/&gt;</code> syntax.
                  </div>
                )}

                {/* Variables */}
                <div>
                  <div className="flex items-center justify-between mb-1.5">
                    <label className="text-xs text-white/50">
                      Variables{' '}
                      <span className="text-white/30">(optional)</span>
                    </label>
                    <button
                      type="button"
                      onClick={handleAddVariable}
                      className="flex items-center gap-1 text-[11px] text-indigo-400 hover:text-indigo-300 transition-colors"
                    >
                      <Plus className="h-3 w-3" /> Add variable
                    </button>
                  </div>
                  {variables.length > 0 && (
                    <div className="space-y-2">
                      {variables.map((v, i) => (
                        <div key={i} className="flex items-center gap-2">
                          <input
                            value={v.key}
                            onChange={(e) => handleVariableChange(i, 'key', e.target.value)}
                            placeholder="key"
                            className="w-1/3 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                          />
                          <input
                            value={v.value}
                            onChange={(e) => handleVariableChange(i, 'value', e.target.value)}
                            placeholder="default value"
                            className="flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                          />
                          <button
                            onClick={() => handleRemoveVariable(i)}
                            className="p-1 text-white/30 hover:text-red-400 transition-colors"
                          >
                            <X className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      ))}
                      <div className="text-[11px] text-white/30">
                        Reference in prompt as{' '}
                        <code className="text-indigo-400/70">&lt;key/&gt;</code>. When triggered via
                        API, pass <code className="text-white/40">{'"variables": {"key": "value"}'}</code> to
                        override defaults.
                      </div>
                    </div>
                  )}
                </div>

                {/* Create button */}
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

              {/* ---- Current automations list ---- */}
              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <h4 className="text-sm font-medium text-white">Current Automations</h4>
                  {loading && <span className="text-xs text-white/40">Loading...</span>}
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

                {isMissionDataReady &&
                  !loading &&
                  visibleAutomations.length === 0 &&
                  !visibleError && (
                    <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6 text-center text-sm text-white/40">
                      No automations yet. Create one above.
                    </div>
                  )}

                <div className="space-y-2">
                  {visibleAutomations.map((automation) => {
                    const label = getAutomationLabel(automation);
                    const sourceTag = getAutomationSourceTag(automation);
                    const command =
                      automation.command_source?.type === 'library'
                        ? commandsByName.get(automation.command_source.name)
                        : undefined;
                    const scheduleLabel = getAutomationScheduleLabel(automation);
                    const lastRunLabel = automation.last_triggered_at
                      ? formatRelativeTime(new Date(automation.last_triggered_at))
                      : 'never';
                    const isWebhook = automation.trigger?.type === 'webhook';
                    const webhookUrl =
                      automation.trigger?.type === 'webhook' && missionId
                        ? buildWebhookUrl(missionId, automation.trigger.config.webhook_id)
                        : null;
                    const isExpanded = expandedAutomationId === automation.id;
                    const hasVars =
                      automation.variables && Object.keys(automation.variables).length > 0;

                    return (
                      <div
                        key={automation.id}
                        className="rounded-xl border border-white/[0.08] bg-white/[0.02] overflow-hidden"
                      >
                        {/* Main row */}
                        <div className="flex flex-col gap-3 p-4 md:flex-row md:items-center md:justify-between">
                          <div className="space-y-1 min-w-0 flex-1">
                            <div className="flex items-center gap-2 flex-wrap">
                              <span className="text-sm font-medium text-white truncate max-w-[300px]">
                                {label}
                              </span>
                              {sourceTag && (
                                <span className="shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium bg-white/[0.06] text-white/50">
                                  {sourceTag}
                                </span>
                              )}
                              {automation.command_source?.type === 'library' && !command && (
                                <span className="flex items-center gap-1 text-[11px] text-amber-300">
                                  <AlertTriangle className="h-3 w-3" />
                                  Missing
                                </span>
                              )}
                            </div>
                            {command?.description && (
                              <div className="text-xs text-white/40">{command.description}</div>
                            )}
                            <div className="flex items-center gap-2 text-xs text-white/40">
                              {isWebhook ? (
                                <span className="flex items-center gap-1">
                                  <Globe className="h-3 w-3" />
                                  {scheduleLabel}
                                </span>
                              ) : (
                                <span className="flex items-center gap-1">
                                  <Clock className="h-3 w-3" />
                                  {scheduleLabel}
                                </span>
                              )}
                              <span>·</span>
                              <span>Last run {lastRunLabel}</span>
                              {hasVars && (
                                <>
                                  <span>·</span>
                                  <span>
                                    {Object.keys(automation.variables!).length} variable
                                    {Object.keys(automation.variables!).length !== 1 ? 's' : ''}
                                  </span>
                                </>
                              )}
                            </div>
                          </div>

                          <div className="flex items-center gap-2 shrink-0">
                            <button
                              onClick={() => handleToggleExecutions(automation.id)}
                              className="flex items-center gap-1 rounded-lg border border-white/[0.08] px-2.5 py-1.5 text-xs text-white/50 hover:text-white/80 hover:border-white/20 transition-colors"
                              title="Execution history"
                            >
                              <History className="h-3.5 w-3.5" />
                              {isExpanded ? (
                                <ChevronDown className="h-3 w-3" />
                              ) : (
                                <ChevronRight className="h-3 w-3" />
                              )}
                            </button>
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
                              className="flex items-center gap-1 rounded-lg border border-white/[0.08] px-2.5 py-1.5 text-xs text-white/60 hover:text-red-300 hover:border-red-500/40 hover:bg-red-500/10 transition-colors"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </button>
                          </div>
                        </div>

                        {/* Webhook URL row */}
                        {webhookUrl && (
                          <div className="border-t border-white/[0.04] px-4 py-2.5 flex items-center gap-2">
                            <span className="text-[11px] text-white/30 shrink-0">POST</span>
                            <code className="flex-1 text-[11px] text-white/50 truncate font-mono">
                              {webhookUrl}
                            </code>
                            <button
                              onClick={() => handleCopyWebhookUrl(webhookUrl, automation.id)}
                              className="shrink-0 flex items-center gap-1 text-[11px] text-white/40 hover:text-white/70 transition-colors"
                            >
                              {copiedWebhookId === automation.id ? (
                                <>
                                  <Check className="h-3 w-3 text-emerald-400" />
                                  <span className="text-emerald-400">Copied</span>
                                </>
                              ) : (
                                <>
                                  <Copy className="h-3 w-3" />
                                  Copy
                                </>
                              )}
                            </button>
                          </div>
                        )}

                        {/* Execution history panel */}
                        {isExpanded && (
                          <div className="border-t border-white/[0.04] px-4 py-3">
                            {executionsLoading ? (
                              <div className="text-xs text-white/40 py-2">
                                Loading executions...
                              </div>
                            ) : executions.length === 0 ? (
                              <div className="text-xs text-white/30 py-2">
                                No executions recorded yet.
                              </div>
                            ) : (
                              <div className="space-y-1.5 max-h-48 overflow-y-auto">
                                <div className="grid grid-cols-[1fr_80px_80px_1fr] gap-2 text-[10px] font-medium text-white/30 uppercase tracking-wider px-1">
                                  <span>Time</span>
                                  <span>Source</span>
                                  <span>Status</span>
                                  <span>Details</span>
                                </div>
                                {executions.map((exec) => (
                                  <div
                                    key={exec.id}
                                    className="grid grid-cols-[1fr_80px_80px_1fr] gap-2 text-[11px] text-white/50 rounded px-1 py-1 hover:bg-white/[0.02]"
                                  >
                                    <span className="truncate">
                                      {formatRelativeTime(new Date(exec.triggered_at))}
                                    </span>
                                    <span className="capitalize">{exec.trigger_source}</span>
                                    <span
                                      className={cn(
                                        'capitalize font-medium',
                                        STATUS_STYLES[exec.status] ?? 'text-white/50'
                                      )}
                                    >
                                      {exec.status}
                                    </span>
                                    <span className="truncate text-white/30">
                                      {exec.error || (exec.retry_count > 0 ? `retry #${exec.retry_count}` : '-')}
                                    </span>
                                  </div>
                                ))}
                              </div>
                            )}
                          </div>
                        )}
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
        title={`Delete automation "${pendingDelete ? getAutomationLabel(pendingDelete) : ''}"?`}
        description="This will permanently remove the automation and stop scheduled runs."
        confirmLabel={deleting ? 'Deleting...' : 'Delete'}
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
