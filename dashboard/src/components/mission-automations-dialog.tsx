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
  Info,
  Pencil,
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
  type StopPolicy,
  type FreshSession,
  listMissionAutomations,
  createMissionAutomation,
  updateAutomation,
  deleteAutomation,
  getAutomationExecutions,
  getLibraryCommand,
} from '@/lib/api';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { toast } from '@/components/toast';
import { getRuntimeApiBase } from '@/lib/settings';

const BUILTIN_VARIABLES = new Set([
  'timestamp',
  'date',
  'unix_time',
  'mission_id',
  'mission_name',
  'cwd',
  'encrypted',
]);

/** Extract `<word/>` placeholders from text, excluding built-ins and webhook patterns. */
function extractPromptVariables(text: string): string[] {
  const matches = text.matchAll(/<(\w+)\/>/g);
  const seen = new Set<string>();
  const result: string[] = [];
  for (const m of matches) {
    const name = m[1];
    if (!BUILTIN_VARIABLES.has(name) && !name.startsWith('webhook') && !seen.has(name)) {
      seen.add(name);
      result.push(name);
    }
  }
  return result;
}

/** Extract detected built-in variable names from text. */
function extractDetectedBuiltins(text: string): string[] {
  const matches = text.matchAll(/<(\w+)\/>/g);
  const result: string[] = [];
  const seen = new Set<string>();
  for (const m of matches) {
    const name = m[1];
    if (BUILTIN_VARIABLES.has(name) && !seen.has(name)) {
      seen.add(name);
      result.push(name);
    }
  }
  return result;
}

export interface MissionAutomationsDialogProps {
  open: boolean;
  missionId: string | null;
  missionLabel?: string | null;
  onClose: () => void;
}

type IntervalUnit = 'seconds' | 'minutes' | 'hours' | 'days';
type CommandSourceType = 'library' | 'inline';
type TriggerKind = 'interval' | 'agent_finished' | 'webhook';

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
  const commandNameRef = useRef('');
  const [inlinePrompt, setInlinePrompt] = useState('');
  const [triggerKind, setTriggerKind] = useState<TriggerKind>('interval');
  const [intervalValue, setIntervalValue] = useState('5');
  const [intervalUnit, setIntervalUnit] = useState<IntervalUnit>('minutes');
  const [startImmediately, setStartImmediately] = useState(true);
  const [stopPolicy, setStopPolicy] = useState<StopPolicy>({ type: 'never' });
  const [freshSession, setFreshSession] = useState<'always' | 'keep'>('keep');
  const [variables, setVariables] = useState<Array<{ key: string; value: string }>>([]);
  const [creating, setCreating] = useState(false);
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Automation | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [editingAutomationId, setEditingAutomationId] = useState<string | null>(null);
  const [editingPrompt, setEditingPrompt] = useState('');
  const [savingEditId, setSavingEditId] = useState<string | null>(null);

  // -- Execution history --
  const [expandedAutomationId, setExpandedAutomationId] = useState<string | null>(null);
  const [executions, setExecutions] = useState<AutomationExecution[]>([]);
  const [executionsLoading, setExecutionsLoading] = useState(false);

  // -- Clipboard --
  const [copiedWebhookId, setCopiedWebhookId] = useState<string | null>(null);

  const commandsByName = useMemo(() => {
    return new Map(commands.map((command) => [command.name, command]));
  }, [commands]);

  // Helper to add auto-populated variables (merges with existing, never overwrites manual)
  const addAutoVariables = useCallback((names: string[]) => {
    setVariables((prev) => {
      const existingKeys = new Set(prev.map((v) => v.key));
      const newVars = [...prev];
      for (const name of names) {
        if (!existingKeys.has(name)) {
          newVars.push({ key: name, value: '' });
        }
      }
      return newVars;
    });
  }, []);

  // Auto-populate variables when a library command is selected
  const handleCommandNameChange = useCallback(
    (name: string) => {
      setCommandName(name);
      commandNameRef.current = name;
      const cmd = commandsByName.get(name);
      if (cmd?.params?.length) {
        addAutoVariables(cmd.params.map((p) => p.name));
      } else if (name) {
        // Fetch full command content to extract <variable/> placeholders
        const capturedName = name;
        getLibraryCommand(name)
          .then((full) => {
            // Guard against stale response if user changed selection
            if (commandNameRef.current !== capturedName) return;
            const fromParams = full.params?.map((p) => p.name) ?? [];
            const fromContent = extractPromptVariables(full.content);
            const all = [...new Set([...fromParams, ...fromContent])];
            if (all.length > 0) addAutoVariables(all);
          })
          .catch(() => {});
      }
    },
    [commandsByName, addAutoVariables]
  );

  // Re-populate variables when commands finish loading (fixes late-load race condition)
  useEffect(() => {
    if (commandSourceType !== 'library' || !commandName) return;
    const cmd = commandsByName.get(commandName);
    if (cmd?.params?.length) {
      addAutoVariables(cmd.params.map((p) => p.name));
    }
  }, [commandsByName, commandName, commandSourceType, addAutoVariables]);

  // Debounced inline prompt variable parsing
  const promptTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleInlinePromptChange = useCallback(
    (text: string) => {
      setInlinePrompt(text);
      if (promptTimerRef.current) clearTimeout(promptTimerRef.current);
      promptTimerRef.current = setTimeout(() => {
        const detected = extractPromptVariables(text);
        addAutoVariables(detected);
      }, 400);
    },
    [addAutoVariables]
  );

  // Detected built-in variables in inline prompt
  const detectedBuiltins = useMemo(
    () => (commandSourceType === 'inline' ? extractDetectedBuiltins(inlinePrompt) : []),
    [commandSourceType, inlinePrompt]
  );

  // Required params for current command (for validation hints)
  const requiredParams = useMemo(() => {
    if (commandSourceType !== 'library' || !commandName) return new Set<string>();
    const cmd = commandsByName.get(commandName);
    if (!cmd?.params) return new Set<string>();
    return new Set(cmd.params.filter((p) => p.required).map((p) => p.name));
  }, [commandSourceType, commandName, commandsByName]);

  // Param descriptions for placeholder text
  const paramDescriptions = useMemo(() => {
    if (commandSourceType !== 'library' || !commandName) return new Map<string, string>();
    const cmd = commandsByName.get(commandName);
    if (!cmd?.params) return new Map<string, string>();
    return new Map(
      cmd.params
        .filter((p) => p.description)
        .map((p) => [p.name, p.description!])
    );
  }, [commandSourceType, commandName, commandsByName]);

  // Warning: required params with no value
  const missingRequiredParams = useMemo(() => {
    if (requiredParams.size === 0) return [];
    const filledKeys = new Map(variables.map((v) => [v.key, v.value]));
    return [...requiredParams].filter((k) => !filledKeys.get(k)?.trim());
  }, [requiredParams, variables]);

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
    if (automation.trigger?.type === 'agent_finished') {
      return 'After agent finishes';
    }
    if (automation.trigger?.type === 'webhook') {
      return 'Webhook';
    }
    return 'Unknown';
  }, []);

  const getStopPolicyLabel = useCallback((policy?: StopPolicy) => {
    if (!policy || policy.type === 'never') return 'Never';
    if (policy.type === 'on_consecutive_failures') {
      return `After ${policy.count} consecutive failures`;
    }
    return 'Never';
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
    } else if (triggerKind === 'agent_finished') {
      trigger = { type: 'agent_finished' };
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
      stop_policy: stopPolicy,
      fresh_session: freshSession,
      ...(Object.keys(vars).length > 0 ? { variables: vars } : {}),
      start_immediately: startImmediately,
    };

    setCreating(true);
    try {
      const shouldStartImmediately = startImmediately;
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
      setStopPolicy({ type: 'never' });
      setFreshSession('keep');
      setVariables([]);
      if (promptTimerRef.current) {
        clearTimeout(promptTimerRef.current);
        promptTimerRef.current = null;
      }
      setStartImmediately(true);
      toast.success(shouldStartImmediately ? 'Automation created' : 'Automation created (scheduled)');
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

  const handleStartEdit = (automation: Automation) => {
    if (automation.command_source?.type !== 'inline') return;
    setEditingAutomationId(automation.id);
    setEditingPrompt(automation.command_source.content ?? '');
  };

  const handleCancelEdit = () => {
    setEditingAutomationId(null);
    setEditingPrompt('');
  };

  const handleSaveEdit = async (automation: Automation) => {
    if (!missionId) return;
    if (automation.command_source?.type !== 'inline') return;
    const content = editingPrompt.trim();
    if (!content) {
      toast.error('Enter a prompt for the automation');
      return;
    }
    setSavingEditId(automation.id);
    try {
      const updated = await updateAutomation(automation.id, {
        command_source: { type: 'inline', content },
      });
      const next = automationsRef.current.map((item) =>
        item.id === automation.id ? updated : item
      );
      setAutomationsForMission(missionId, next);
      toast.success('Automation updated');
      handleCancelEdit();
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Failed to update automation';
      toast.error(message);
    } finally {
      setSavingEditId(null);
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
  const isTriggerValid =
    triggerKind === 'webhook' || triggerKind === 'agent_finished' || intervalSeconds > 0;
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
                      <option value="agent_finished" className="bg-[#1a1a1a]">
                        After agent finishes (restart)
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
                    <select
                      value={commandName}
                      onChange={(e) => handleCommandNameChange(e.target.value)}
                      className={cn(selectClass, 'w-full')}
                      style={selectStyle}
                    >
                      <option value="" className="bg-[#1a1a1a]">
                        {commandsLoading ? 'Loading commands...' : 'Select a command'}
                      </option>
                      {commands.map((command) => (
                        <option key={command.name} value={command.name} className="bg-[#1a1a1a]">
                          {command.name}
                        </option>
                      ))}
                    </select>
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
                      onChange={(e) => handleInlinePromptChange(e.target.value)}
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
                    {detectedBuiltins.length > 0 && (
                      <div className="mt-1.5 flex items-center gap-1.5 text-[11px] text-white/40">
                        <Info className="h-3 w-3 shrink-0" />
                        <span>
                          Built-in variables detected:{' '}
                          {detectedBuiltins.map((b, i) => (
                            <span key={b}>
                              {i > 0 && ', '}
                              <code className="text-indigo-400/60">&lt;{b}/&gt;</code>
                            </span>
                          ))}
                          {' '}— these are substituted automatically.
                        </span>
                      </div>
                    )}
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

                {triggerKind === 'agent_finished' && (
                  <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-xs text-white/40">
                    Runs immediately after the agent finishes a turn for this mission (useful for
                    continuous loops).
                  </div>
                )}

                {triggerKind === 'webhook' && (
                  <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-xs text-white/40">
                    The webhook URL will be generated after creation. You can then call it via HTTP
                    POST with a JSON body. Variables from the payload can be mapped using{' '}
                    <code className="text-indigo-400/70">&lt;webhook.field.path/&gt;</code> syntax.
                  </div>
                )}

                <div>
                  <label className="block text-xs text-white/50 mb-1.5">Stop policy</label>
                  <select
                    value={stopPolicy.type}
                    onChange={(e) =>
                      setStopPolicy(
                        e.target.value === 'on_consecutive_failures'
                          ? { type: 'on_consecutive_failures', count: 2 }
                          : { type: 'never' }
                      )
                    }
                    className={cn(selectClass, 'w-full')}
                    style={selectStyle}
                  >
                    <option value="never" className="bg-[#1a1a1a]">
                      Never (recommended)
                    </option>
                    <option value="on_consecutive_failures" className="bg-[#1a1a1a]">
                      After 2 consecutive failures
                    </option>
                  </select>
                  <div className="mt-1 text-[11px] text-white/30">
                    Auto-disables this automation after repeated failures. Use "Never" for automations that should keep running.
                  </div>
                </div>

                <div>
                  <label className="block text-xs text-white/50 mb-1.5">Session mode</label>
                  <select
                    value={freshSession || 'keep'}
                    onChange={(e) => setFreshSession(e.target.value as 'always' | 'keep')}
                    className={cn(selectClass, 'w-full')}
                    style={selectStyle}
                  >
                    <option value="keep" className="bg-[#1a1a1a]">
                      Keep session (default)
                    </option>
                    <option value="always" className="bg-[#1a1a1a]">
                      Fresh session (clear context each run)
                    </option>
                  </select>
                  <div className="mt-1 text-[11px] text-white/30">
                    Fresh session clears all conversation history before each run.
                  </div>
                </div>

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
                      {variables.map((v, i) => {
                        const isRequired = requiredParams.has(v.key);
                        const description = paramDescriptions.get(v.key);
                        return (
                          <div key={i} className="flex items-center gap-2">
                            <div className="relative w-1/3">
                              <input
                                value={v.key}
                                onChange={(e) => handleVariableChange(i, 'key', e.target.value)}
                                placeholder="key"
                                className={cn(
                                  'w-full rounded-lg border bg-white/[0.02] px-2.5 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50',
                                  isRequired
                                    ? 'border-amber-500/30'
                                    : 'border-white/[0.06]'
                                )}
                              />
                              {isRequired && (
                                <span className="absolute right-2 top-1/2 -translate-y-1/2 text-amber-400 text-[10px]">
                                  *
                                </span>
                              )}
                            </div>
                            <input
                              value={v.value}
                              onChange={(e) => handleVariableChange(i, 'value', e.target.value)}
                              placeholder={description || 'default value'}
                              className="flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-xs text-white placeholder:text-white/25 focus:outline-none focus:border-indigo-500/50"
                            />
                            <button
                              onClick={() => handleRemoveVariable(i)}
                              className="p-1 text-white/30 hover:text-red-400 transition-colors"
                            >
                              <X className="h-3.5 w-3.5" />
                            </button>
                          </div>
                        );
                      })}
                      <div className="text-[11px] text-white/30">
                        Reference in prompt as{' '}
                        <code className="text-indigo-400/70">&lt;key/&gt;</code>. When triggered via
                        API, pass <code className="text-white/40">{'"variables": {"key": "value"}'}</code> to
                        override defaults.
                      </div>
                    </div>
                  )}
                  {missingRequiredParams.length > 0 && (
                    <div className="mt-2 flex items-start gap-2 rounded-lg border border-amber-500/20 bg-amber-500/5 px-3 py-2 text-[11px] text-amber-200/80">
                      <AlertTriangle className="h-3 w-3 mt-0.5 shrink-0" />
                      <span>
                        Required params with no default:{' '}
                        {missingRequiredParams.map((k) => (
                          <code key={k} className="text-amber-300">
                            {k}
                          </code>
                        )).reduce<React.ReactNode[]>((acc, el, i) => (i === 0 ? [el] : [...acc, ', ', el]), [])}
                        . Values can be provided at trigger time.
                      </span>
                    </div>
                  )}
                </div>

                {/* Start behavior */}
                <div className="flex items-center justify-between gap-3 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2">
                  <div className="min-w-0">
                    <div className="text-xs font-medium text-white/70">Start immediately</div>
                    <div className="text-[11px] text-white/35">
                      If off, the automation waits for its next trigger instead of running right away.
                    </div>
                  </div>
                  <label className="flex items-center gap-2 shrink-0 cursor-pointer select-none">
                    <input
                      type="checkbox"
                      checked={startImmediately}
                      onChange={(e) => setStartImmediately(e.target.checked)}
                      className="h-4 w-4 rounded border-white/20 bg-white/5 text-indigo-500 focus:ring-indigo-500/40"
                    />
                    <span className="text-xs text-white/50">
                      {startImmediately ? 'On' : 'Off'}
                    </span>
                  </label>
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
              <div className="space-y-3 pb-2">
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
                    const isInline = automation.command_source?.type === 'inline';
                    const isEditing = editingAutomationId === automation.id;
                    const canSaveEdit = isEditing && editingPrompt.trim().length > 0;

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
                              {automation.stop_policy &&
                                automation.stop_policy.type !== 'never' && (
                                <>
                                  <span>·</span>
                                  <span>Stop: {getStopPolicyLabel(automation.stop_policy)}</span>
                                </>
                              )}
                              {automation.fresh_session === 'always' && (
                                <>
                                  <span>·</span>
                                  <span className="text-amber-400/70">Fresh session</span>
                                </>
                              )}
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
                            {isInline && (
                              <button
                                onClick={() => handleStartEdit(automation)}
                                className="flex items-center gap-1 rounded-lg border border-white/[0.08] px-2.5 py-1.5 text-xs text-white/60 hover:text-white/80 hover:border-white/20 transition-colors"
                                title="Edit inline prompt"
                              >
                                <Pencil className="h-3.5 w-3.5" />
                              </button>
                            )}
                            <button
                              onClick={() => setPendingDelete(automation)}
                              className="flex items-center gap-1 rounded-lg border border-white/[0.08] px-2.5 py-1.5 text-xs text-white/60 hover:text-red-300 hover:border-red-500/40 hover:bg-red-500/10 transition-colors"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </button>
                          </div>
                        </div>

                        {/* Inline prompt editor */}
                        {isInline && isEditing && (
                          <div className="border-t border-white/[0.04] px-4 py-3 space-y-2">
                            <label className="block text-xs text-white/50">Edit prompt</label>
                            <textarea
                              value={editingPrompt}
                              onChange={(e) => setEditingPrompt(e.target.value)}
                              rows={3}
                              className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 resize-y"
                            />
                            <div className="flex items-center justify-between gap-3">
                              <div className="text-[11px] text-white/30">
                                Use <code className="text-indigo-400/70">&lt;variable_name/&gt;</code>{' '}
                                to insert variables.
                              </div>
                              <div className="flex items-center gap-2">
                                <button
                                  onClick={handleCancelEdit}
                                  className="rounded-lg border border-white/[0.08] px-3 py-1.5 text-xs text-white/60 hover:text-white/80 hover:border-white/20 transition-colors"
                                >
                                  Cancel
                                </button>
                                <button
                                  onClick={() => handleSaveEdit(automation)}
                                  disabled={!canSaveEdit || savingEditId === automation.id}
                                  className="rounded-lg bg-indigo-500 hover:bg-indigo-600 px-3 py-1.5 text-xs font-medium text-white transition-colors disabled:opacity-50"
                                >
                                  {savingEditId === automation.id ? 'Saving...' : 'Save'}
                                </button>
                              </div>
                            </div>
                          </div>
                        )}

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
