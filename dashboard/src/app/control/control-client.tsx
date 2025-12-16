'use client';

import { useEffect, useMemo, useRef, useState } from 'react';
import Markdown from 'react-markdown';
import { cn } from '@/lib/utils';
import {
  cancelControl,
  postControlMessage,
  postControlToolResult,
  streamControl,
  type ControlRunState,
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

export default function ControlClient() {
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState('');

  const [runState, setRunState] = useState<ControlRunState>('idle');
  const [queueLen, setQueueLen] = useState(0);

  const isBusy = runState !== 'idle';

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const streamCleanupRef = useRef<null | (() => void)>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  };

  useEffect(() => {
    scrollToBottom();
  }, [items]);

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
        setItems((prev) => [
          ...prev,
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

  return (
    <div className="flex h-screen flex-col p-6">
      {/* Header */}
      <div className="mb-6 flex items-start justify-between">
        <div>
          <h1 className="text-xl font-semibold text-white">
            Agent Control
          </h1>
          <p className="mt-1 text-sm text-white/50">
            Talk to the global RootAgent session
          </p>
        </div>

        <div className="flex items-center gap-3">
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
                <h2 className="text-lg font-medium text-white">
                  Start a conversation
                </h2>
                <p className="mt-2 text-sm text-white/40 max-w-sm">
                  Ask the agent to do something — messages queue while it&apos;s busy
                </p>
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
