'use client';

import { useEffect, useMemo, useRef, useState, useCallback } from 'react';
import { useSearchParams, useRouter } from 'next/navigation';
import Markdown from 'react-markdown';
import { cn } from '@/lib/utils';
import {
  cancelControl,
  postControlMessage,
  postControlToolResult,
  streamControl,
  loadMission,
  createMission,
  setMissionStatus,
  getCurrentMission,
  type ControlRunState,
  type Mission,
  type MissionStatus,
} from '@/lib/api';
import {
  Send,
  Square,
  Bot,
  User,
  Loader,
  CheckCircle,
  XCircle,
  Ban,
  Clock,
  Plus,
  ChevronDown,
  ChevronRight,
  Target,
} from 'lucide-react';
import {
  OptionList,
  OptionListErrorBoundary,
  parseSerializableOptionList,
  type OptionListSelection,
} from '@/components/tool-ui/option-list';
import {
  DataTable,
  parseSerializableDataTable,
} from '@/components/tool-ui/data-table';

type ChatItem =
  | {
      kind: 'user';
      id: string;
      content: string;
    }
  | {
      kind: 'assistant';
      id: string;
      content: string;
      success: boolean;
      costCents: number;
      model: string | null;
    }
  | {
      kind: 'thinking';
      id: string;
      content: string;
      done: boolean;
      startTime: number;
    }
  | {
      kind: 'tool';
      id: string;
      toolCallId: string;
      name: string;
      args: unknown;
      result?: unknown;
    }
  | {
      kind: 'system';
      id: string;
      content: string;
    };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function statusLabel(state: ControlRunState): {
  label: string;
  Icon: typeof Loader;
  className: string;
} {
  switch (state) {
    case 'idle':
      return { label: 'Idle', Icon: Clock, className: 'text-white/40' };
    case 'running':
      return { label: 'Running', Icon: Loader, className: 'text-indigo-400' };
    case 'waiting_for_tool':
      return { label: 'Waiting', Icon: Loader, className: 'text-amber-400' };
  }
}

function missionStatusLabel(status: MissionStatus): {
  label: string;
  className: string;
} {
  switch (status) {
    case 'active':
      return { label: 'Active', className: 'bg-indigo-500/20 text-indigo-400' };
    case 'completed':
      return { label: 'Completed', className: 'bg-emerald-500/20 text-emerald-400' };
    case 'failed':
      return { label: 'Failed', className: 'bg-red-500/20 text-red-400' };
  }
}

// Thinking item component with collapsible UI (Cursor-style)
function ThinkingItem({ item }: { item: Extract<ChatItem, { kind: 'thinking' }> }) {
  const [expanded, setExpanded] = useState(false);
  const [elapsedSeconds, setElapsedSeconds] = useState(0);

  // Update elapsed time while thinking is active
  useEffect(() => {
    if (item.done) return;
    const interval = setInterval(() => {
      setElapsedSeconds(Math.floor((Date.now() - item.startTime) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [item.done, item.startTime]);

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${seconds}s`;
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}m${secs > 0 ? ` ${secs}s` : ''}`;
  };

  const duration = item.done 
    ? formatDuration(Math.floor((Date.now() - item.startTime) / 1000))
    : formatDuration(elapsedSeconds);

  return (
    <div className="my-2">
      {/* Compact header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 text-white/40 hover:text-white/60 transition-colors"
      >
        <span className="text-xs">
          {item.done ? 'Thought' : 'Thinking'} for {duration}
        </span>
        {expanded ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
      </button>
      
      {/* Expandable content */}
      {expanded && (
        <div className="mt-2 pl-0.5 border-l-2 border-white/10">
          <div className="pl-3 text-xs text-white/50 whitespace-pre-wrap max-h-80 overflow-y-auto leading-relaxed">
            {item.content}
          </div>
        </div>
      )}
    </div>
  );
}

export default function ControlClient() {
  const searchParams = useSearchParams();
  const router = useRouter();
  
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState('');

  const [runState, setRunState] = useState<ControlRunState>('idle');
  const [queueLen, setQueueLen] = useState(0);
  
  // Mission state
  const [currentMission, setCurrentMission] = useState<Mission | null>(null);
  const [showStatusMenu, setShowStatusMenu] = useState(false);
  const [missionLoading, setMissionLoading] = useState(false);

  const isBusy = runState !== 'idle';

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const streamCleanupRef = useRef<null | (() => void)>(null);
  const statusMenuRef = useRef<HTMLDivElement>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  };

  useEffect(() => {
    scrollToBottom();
  }, [items]);
  
  // Close status menu when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (statusMenuRef.current && !statusMenuRef.current.contains(event.target as Node)) {
        setShowStatusMenu(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  // Convert mission history to chat items
  const missionHistoryToItems = useCallback((mission: Mission): ChatItem[] => {
    return mission.history.map((entry, i) => {
      if (entry.role === 'user') {
        return {
          kind: 'user' as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
        };
      } else {
        return {
          kind: 'assistant' as const,
          id: `history-${mission.id}-${i}`,
          content: entry.content,
          success: true,
          costCents: 0,
          model: null,
        };
      }
    });
  }, []);

  // Load mission from URL param on mount
  useEffect(() => {
    const missionId = searchParams.get('mission');
    if (missionId) {
      setMissionLoading(true);
      loadMission(missionId)
        .then((mission) => {
          setCurrentMission(mission);
          setItems(missionHistoryToItems(mission));
        })
        .catch((err) => {
          console.error('Failed to load mission:', err);
          setItems((prev) => [
            ...prev,
            { kind: 'system', id: `err-${Date.now()}`, content: `Failed to load mission: ${err.message}` },
          ]);
        })
        .finally(() => setMissionLoading(false));
    } else {
      // Try to get current mission
      getCurrentMission()
        .then((mission) => {
          if (mission) {
            setCurrentMission(mission);
            setItems(missionHistoryToItems(mission));
            // Update URL without navigation
            router.replace(`/control?mission=${mission.id}`, { scroll: false });
          }
        })
        .catch((err) => {
          console.error('Failed to get current mission:', err);
        });
    }
  }, [searchParams, router, missionHistoryToItems]);

  // Handle creating a new mission
  const handleNewMission = async () => {
    try {
      setMissionLoading(true);
      const mission = await createMission();
      setCurrentMission(mission);
      setItems([]);
      router.replace(`/control?mission=${mission.id}`, { scroll: false });
    } catch (err) {
      console.error('Failed to create mission:', err);
      setItems((prev) => [
        ...prev,
        { kind: 'system', id: `err-${Date.now()}`, content: 'Failed to create new mission.' },
      ]);
    } finally {
      setMissionLoading(false);
    }
  };

  // Handle setting mission status
  const handleSetStatus = async (status: MissionStatus) => {
    if (!currentMission) return;
    try {
      await setMissionStatus(currentMission.id, status);
      setCurrentMission({ ...currentMission, status });
      setShowStatusMenu(false);
    } catch (err) {
      console.error('Failed to set mission status:', err);
      setItems((prev) => [
        ...prev,
        { kind: 'system', id: `err-${Date.now()}`, content: 'Failed to update mission status.' },
      ]);
    }
  };

  useEffect(() => {
    streamCleanupRef.current?.();

    const cleanup = streamControl((event) => {
      const data: unknown = event.data;

      if (event.type === 'status' && isRecord(data)) {
        const st = data['state'];
        setRunState(typeof st === 'string' ? (st as ControlRunState) : 'idle');
        const q = data['queue_len'];
        setQueueLen(typeof q === 'number' ? q : 0);
        return;
      }

      if (event.type === 'user_message' && isRecord(data)) {
        setItems((prev) => [
          ...prev,
          {
            kind: 'user',
            id: String(data['id'] ?? Date.now()),
            content: String(data['content'] ?? ''),
          },
        ]);
        return;
      }

      if (event.type === 'assistant_message' && isRecord(data)) {
        // Remove any pending thinking items when we get the final message
        setItems((prev) => [
          ...prev.filter(it => it.kind !== 'thinking' || it.done),
          {
            kind: 'assistant',
            id: String(data['id'] ?? Date.now()),
            content: String(data['content'] ?? ''),
            success: Boolean(data['success']),
            costCents: Number(data['cost_cents'] ?? 0),
            model: data['model'] ? String(data['model']) : null,
          },
        ]);
        return;
      }

      if (event.type === 'thinking' && isRecord(data)) {
        const content = String(data['content'] ?? '');
        const done = Boolean(data['done']);
        
        setItems((prev) => {
          // Find existing thinking item that's not done
          const existingIdx = prev.findIndex(it => it.kind === 'thinking' && !it.done);
          if (existingIdx >= 0) {
            // Update existing thinking item
            const updated = [...prev];
            const existing = updated[existingIdx] as Extract<ChatItem, { kind: 'thinking' }>;
            updated[existingIdx] = {
              ...existing,
              content: existing.content + '\n\n---\n\n' + content,
              done,
            };
            return updated;
          } else {
            // Create new thinking item
            return [
              ...prev,
              {
                kind: 'thinking' as const,
                id: `thinking-${Date.now()}`,
                content,
                done,
                startTime: Date.now(),
              },
            ];
          }
        });
        return;
      }

      if (event.type === 'tool_call' && isRecord(data)) {
        const name = String(data['name'] ?? '');
        if (!name.startsWith('ui_')) return;

        setItems((prev) => [
          ...prev,
          {
            kind: 'tool',
            id: `tool-${String(data['tool_call_id'] ?? Date.now())}`,
            toolCallId: String(data['tool_call_id'] ?? ''),
            name,
            args: data['args'],
          },
        ]);
        return;
      }

      if (event.type === 'tool_result' && isRecord(data)) {
        const name = String(data['name'] ?? '');
        if (!name.startsWith('ui_')) return;

        const toolCallId = String(data['tool_call_id'] ?? '');
        setItems((prev) =>
          prev.map((it) =>
            it.kind === 'tool' && it.toolCallId === toolCallId
              ? { ...it, result: data['result'] }
              : it,
          ),
        );
        return;
      }

      if (event.type === 'error') {
        const msg =
          (isRecord(data) && data['message'] ? String(data['message']) : null) ??
          'An error occurred.';
        setItems((prev) => [
          ...prev,
          { kind: 'system', id: `err-${Date.now()}`, content: msg },
        ]);
      }
    });

    streamCleanupRef.current = cleanup;

    return () => {
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
    };
  }, []);

  const status = useMemo(() => statusLabel(runState), [runState]);
  const StatusIcon = status.Icon;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const content = input.trim();
    if (!content) return;

    setInput('');

    try {
      await postControlMessage(content);
    } catch (err) {
      console.error(err);
      setItems((prev) => [
        ...prev,
        {
          kind: 'system',
          id: `err-${Date.now()}`,
          content: 'Failed to send message to control session.',
        },
      ]);
    }
  };

  const handleStop = async () => {
    try {
      await cancelControl();
    } catch (err) {
      console.error(err);
      setItems((prev) => [
        ...prev,
        {
          kind: 'system',
          id: `err-${Date.now()}`,
          content: 'Failed to cancel control session.',
        },
      ]);
    }
  };

  const missionStatus = currentMission ? missionStatusLabel(currentMission.status) : null;
  const missionTitle = currentMission?.title 
    ? (currentMission.title.length > 60 ? currentMission.title.slice(0, 60) + '...' : currentMission.title)
    : 'New Mission';

  return (
    <div className="flex h-screen flex-col p-6">
      {/* Header */}
      <div className="mb-6 flex items-start justify-between">
        <div className="flex items-center gap-4">
          {/* Mission indicator */}
          <div className="flex items-center gap-3">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/20">
              <Target className="h-5 w-5 text-indigo-400" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h1 className="text-lg font-semibold text-white">
                  {missionLoading ? 'Loading...' : missionTitle}
                </h1>
                {missionStatus && (
                  <span className={cn('px-2 py-0.5 rounded-full text-xs font-medium', missionStatus.className)}>
                    {missionStatus.label}
                  </span>
                )}
              </div>
              <p className="text-xs text-white/40">
                {currentMission ? `Mission ${currentMission.id.slice(0, 8)}...` : 'No active mission'}
              </p>
            </div>
          </div>
        </div>

        <div className="flex items-center gap-3">
          {/* Status dropdown */}
          {currentMission && (
            <div className="relative" ref={statusMenuRef}>
              <button
                onClick={() => setShowStatusMenu(!showStatusMenu)}
                className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors"
              >
                Set Status
                <ChevronDown className="h-4 w-4" />
              </button>
              {showStatusMenu && (
                <div className="absolute right-0 top-full mt-1 w-40 rounded-lg border border-white/[0.06] bg-[#1a1a1a] py-1 shadow-xl z-10">
                  <button
                    onClick={() => handleSetStatus('completed')}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                  >
                    <CheckCircle className="h-4 w-4 text-emerald-400" />
                    Mark Complete
                  </button>
                  <button
                    onClick={() => handleSetStatus('failed')}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                  >
                    <XCircle className="h-4 w-4 text-red-400" />
                    Mark Failed
                  </button>
                  {currentMission.status !== 'active' && (
                    <button
                      onClick={() => handleSetStatus('active')}
                      className="flex w-full items-center gap-2 px-3 py-2 text-sm text-white/70 hover:bg-white/[0.04]"
                    >
                      <Clock className="h-4 w-4 text-indigo-400" />
                      Reactivate
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
          
          {/* New Mission button */}
          <button
            onClick={handleNewMission}
            disabled={missionLoading}
            className="flex items-center gap-2 rounded-lg bg-indigo-500/20 px-3 py-2 text-sm font-medium text-indigo-400 hover:bg-indigo-500/30 transition-colors disabled:opacity-50"
          >
            <Plus className="h-4 w-4" />
            New Mission
          </button>

          {/* Run status */}
          <div className={cn('flex items-center gap-2 text-sm', status.className)}>
            <StatusIcon className={cn('h-4 w-4', runState !== 'idle' && 'animate-spin')} />
            <span>{status.label}</span>
            <span className="text-white/20">•</span>
            <span className="text-white/40">Queue: {queueLen}</span>
          </div>
        </div>
      </div>

      {/* Chat container */}
      <div className="flex-1 min-h-0 flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden">
        {/* Messages */}
        <div className="flex-1 overflow-y-auto p-6">
          {items.length === 0 ? (
            <div className="flex h-full items-center justify-center">
              <div className="text-center">
                <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10">
                  <Bot className="h-8 w-8 text-indigo-400" />
                </div>
                {currentMission && currentMission.status !== 'active' ? (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      No conversation history
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      This mission was {currentMission.status} without any messages.
                      {currentMission.status === 'completed' && ' You can reactivate it to continue.'}
                    </p>
                  </>
                ) : (
                  <>
                    <h2 className="text-lg font-medium text-white">
                      Start a conversation
                    </h2>
                    <p className="mt-2 text-sm text-white/40 max-w-sm">
                      Ask the agent to do something — messages queue while it&apos;s busy
                    </p>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="mx-auto max-w-3xl space-y-6">
              {items.map((item) => {
                if (item.kind === 'user') {
                  return (
                    <div key={item.id} className="flex justify-end gap-3">
                      <div className="max-w-[80%] rounded-2xl rounded-br-md bg-indigo-500 px-4 py-3 text-white">
                        <p className="whitespace-pre-wrap text-sm">{item.content}</p>
                      </div>
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.08]">
                        <User className="h-4 w-4 text-white/60" />
                      </div>
                    </div>
                  );
                }

                if (item.kind === 'assistant') {
                  const statusIcon = item.success ? CheckCircle : XCircle;
                  const StatusIcon = statusIcon;
                  const displayModel = item.model 
                    ? (item.model.includes('/') ? item.model.split('/').pop() : item.model)
                    : null;
                  return (
                    <div key={item.id} className="flex justify-start gap-3">
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                        <Bot className="h-4 w-4 text-indigo-400" />
                      </div>
                      <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                        <div className="mb-2 flex items-center gap-2 text-xs text-white/40">
                          <StatusIcon
                            className={cn(
                              'h-3 w-3',
                              item.success ? 'text-emerald-400' : 'text-red-400',
                            )}
                          />
                          <span>{item.success ? 'Completed' : 'Failed'}</span>
                          {displayModel && (
                            <>
                              <span>•</span>
                              <span className="font-mono truncate max-w-[120px]" title={item.model ?? undefined}>{displayModel}</span>
                            </>
                          )}
                          {item.costCents > 0 && (
                            <>
                              <span>•</span>
                              <span className="text-emerald-400">${(item.costCents / 100).toFixed(4)}</span>
                            </>
                          )}
                        </div>
                        <div className="prose-glass text-sm [&_p]:my-2 [&_code]:text-xs">
                          <Markdown>{item.content}</Markdown>
                        </div>
                      </div>
                    </div>
                  );
                }

                if (item.kind === 'thinking') {
                  return <ThinkingItem key={item.id} item={item} />;
                }

                if (item.kind === 'tool') {
                  if (item.name === 'ui_optionList') {
                    const toolCallId = item.toolCallId;
                    const rawArgs: Record<string, unknown> = isRecord(item.args) ? item.args : {};

                    let optionList: ReturnType<typeof parseSerializableOptionList> | null = null;
                    let parseErr: string | null = null;
                    try {
                      optionList = parseSerializableOptionList({
                        ...rawArgs,
                        id:
                          typeof rawArgs['id'] === 'string' && rawArgs['id']
                            ? (rawArgs['id'] as string)
                            : `option-list-${toolCallId}`,
                      });
                    } catch (e) {
                      parseErr = e instanceof Error ? e.message : 'Invalid option list payload';
                    }

                    const confirmed = item.result as OptionListSelection | undefined;

                    return (
                      <div key={item.id} className="flex justify-start gap-3">
                        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                          <Bot className="h-4 w-4 text-indigo-400" />
                        </div>
                        <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                          <div className="mb-2 text-xs text-white/40">
                            Tool: <span className="font-mono text-indigo-400">{item.name}</span>
                          </div>

                          {parseErr || !optionList ? (
                            <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-400">
                              {parseErr ?? 'Failed to render OptionList'}
                            </div>
                          ) : (
                            <OptionListErrorBoundary>
                              <OptionList
                                {...optionList}
                                value={undefined}
                                confirmed={confirmed}
                                onConfirm={async (selection) => {
                                  setItems((prev) =>
                                    prev.map((it) =>
                                      it.kind === 'tool' && it.toolCallId === toolCallId
                                        ? { ...it, result: selection }
                                        : it,
                                    ),
                                  );
                                  await postControlToolResult({
                                    tool_call_id: toolCallId,
                                    name: item.name,
                                    result: selection,
                                  });
                                }}
                                onCancel={async () => {
                                  setItems((prev) =>
                                    prev.map((it) =>
                                      it.kind === 'tool' && it.toolCallId === toolCallId
                                        ? { ...it, result: null }
                                        : it,
                                    ),
                                  );
                                  await postControlToolResult({
                                    tool_call_id: toolCallId,
                                    name: item.name,
                                    result: null,
                                  });
                                }}
                              />
                            </OptionListErrorBoundary>
                          )}
                        </div>
                      </div>
                    );
                  }

                  if (item.name === 'ui_dataTable') {
                    const rawArgs: Record<string, unknown> = isRecord(item.args) ? item.args : {};
                    const dataTable = parseSerializableDataTable(rawArgs);

                    return (
                      <div key={item.id} className="flex justify-start gap-3">
                        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                          <Bot className="h-4 w-4 text-indigo-400" />
                        </div>
                        <div className="max-w-[90%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                          <div className="mb-2 text-xs text-white/40">
                            Tool: <span className="font-mono text-indigo-400">{item.name}</span>
                          </div>
                          {dataTable ? (
                            <DataTable
                              id={dataTable.id}
                              title={dataTable.title}
                              columns={dataTable.columns}
                              rows={dataTable.rows}
                            />
                          ) : (
                            <div className="rounded-lg bg-red-500/10 border border-red-500/20 p-3 text-sm text-red-400">
                              Failed to render DataTable
                            </div>
                          )}
                        </div>
                      </div>
                    );
                  }

                  // Unknown UI tool.
                  return (
                    <div key={item.id} className="flex justify-start gap-3">
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-indigo-500/20">
                        <Bot className="h-4 w-4 text-indigo-400" />
                      </div>
                      <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.03] border border-white/[0.06] px-4 py-3">
                        <p className="text-sm text-white/60">
                          Unsupported Tool: <span className="font-mono text-indigo-400">{item.name}</span>
                        </p>
                      </div>
                    </div>
                  );
                }

                // system
                return (
                  <div key={item.id} className="flex justify-start gap-3">
                    <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-white/[0.04]">
                      <Ban className="h-4 w-4 text-white/40" />
                    </div>
                    <div className="max-w-[80%] rounded-2xl rounded-bl-md bg-white/[0.02] border border-white/[0.04] px-4 py-3">
                      <p className="whitespace-pre-wrap text-sm text-white/60">{item.content}</p>
                    </div>
                  </div>
                );
              })}
              <div ref={messagesEndRef} />
            </div>
          )}
        </div>

        {/* Input */}
        <div className="border-t border-white/[0.06] bg-white/[0.01] p-4">
          <form onSubmit={handleSubmit} className="mx-auto flex max-w-3xl gap-3">
            <input
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder="Message the root agent…"
              className="flex-1 rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
            />

            {isBusy ? (
              <button
                type="button"
                onClick={handleStop}
                className="flex items-center gap-2 rounded-xl bg-red-500 hover:bg-red-600 px-5 py-3 text-sm font-medium text-white transition-colors"
              >
                <Square className="h-4 w-4" />
                Stop
              </button>
            ) : (
              <button
                type="submit"
                disabled={!input.trim()}
                className="flex items-center gap-2 rounded-xl bg-indigo-500 hover:bg-indigo-600 px-5 py-3 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Send className="h-4 w-4" />
                Send
              </button>
            )}
          </form>
        </div>
      </div>
    </div>
  );
}
