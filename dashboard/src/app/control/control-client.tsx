'use client';

import { useCallback, useEffect, useState, useRef } from 'react';
import { useSearchParams } from 'next/navigation';
import { cn } from '@/lib/utils';
import {
  createTask,
  getTask,
  stopTask,
  streamTask,
  TaskLogEntry,
} from '@/lib/api';
import {
  Send,
  Square,
  Bot,
  User,
  Loader,
  Terminal,
  CheckCircle,
  XCircle,
  Code,
  FileText,
  Ban,
  Clock,
} from 'lucide-react';

interface Message {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: Date;
  status?: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  logs?: TaskLogEntry[];
}

export default function ControlClient() {
  const searchParams = useSearchParams();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [currentTaskId, setCurrentTaskId] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const loadedFromUrlTaskIdRef = useRef<string | null>(null);
  const [expandedLogs, setExpandedLogs] = useState<Set<string>>(new Set());
  const streamCleanupRef = useRef<null | (() => void)>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages]);

  useEffect(() => {
    return () => {
      // Ensure we don't leak SSE connections when navigating away.
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
    };
  }, []);

  const loadTask = useCallback(async (taskId: string) => {
    try {
      const task = await getTask(taskId);
      const userMessage: Message = {
        id: `user-${taskId}`,
        role: 'user',
        content: task.task,
        timestamp: new Date(),
      };
      const assistantMessage: Message = {
        id: `assistant-${taskId}`,
        role: 'assistant',
        content: task.result || 'Processing...',
        timestamp: new Date(),
        status: task.status,
        logs: task.log,
      };
      setMessages([userMessage, assistantMessage]);
      const shouldStream = task.status === 'running' || task.status === 'pending';
      setCurrentTaskId(shouldStream ? taskId : null);

      // If the task is still running, attach streaming so the UI keeps updating (and Stop button is visible).
      if (shouldStream) {
        setIsLoading(true);
        streamCleanupRef.current?.();

        const cleanup = streamTask(taskId, (event) => {
          if (event.type === 'log') {
            const logEntry = event.data as TaskLogEntry;
            setMessages((prev) =>
              prev.map((m) =>
                m.id === `assistant-${taskId}` ? { ...m, logs: [...(m.logs || []), logEntry] } : m
              )
            );
          } else if (event.type === 'done') {
            const doneData = event.data as { status: string; result: string | null };
            setMessages((prev) =>
              prev.map((m) =>
                m.id === `assistant-${taskId}`
                  ? {
                      ...m,
                      content: doneData.result || 'Task completed',
                      status: doneData.status as Message['status'],
                    }
                  : m
              )
            );
            setCurrentTaskId(null);
            setIsLoading(false);
            streamCleanupRef.current = null;
          } else if (event.type === 'error') {
            const err = event.data as { message?: string; status?: number };
            setMessages((prev) =>
              prev.map((m) =>
                m.id === `assistant-${taskId}`
                  ? { ...m, content: err?.message || 'Streaming failed', status: 'failed' }
                  : m
              )
            );
            setCurrentTaskId(null);
            setIsLoading(false);
            streamCleanupRef.current = null;
          }
        });

        streamCleanupRef.current = cleanup;
      } else {
        setIsLoading(false);
        streamCleanupRef.current?.();
        streamCleanupRef.current = null;
      }
    } catch (error) {
      console.error('Failed to load task:', error);
    }
  }, []);

  // Load task from URL if provided
  useEffect(() => {
    const taskId = searchParams.get('task');
    if (!taskId) return;
    if (loadedFromUrlTaskIdRef.current === taskId) return;
    loadedFromUrlTaskIdRef.current = taskId;

    // eslint-disable-next-line react-hooks/set-state-in-effect
    void loadTask(taskId);
  }, [searchParams, loadTask]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!input.trim() || isLoading) return;

    const userMessage: Message = {
      id: `user-${Date.now()}`,
      role: 'user',
      content: input,
      timestamp: new Date(),
    };

    setMessages((prev) => [...prev, userMessage]);
    setInput('');
    setIsLoading(true);

    try {
      const response = await createTask({ task: input });
      setCurrentTaskId(response.id);

      const assistantMessage: Message = {
        id: `assistant-${response.id}`,
        role: 'assistant',
        content: 'Processing your request...',
        timestamp: new Date(),
        status: 'running',
        logs: [],
      };

      setMessages((prev) => [...prev, assistantMessage]);

      // Start streaming
      // Abort any previous stream.
      streamCleanupRef.current?.();

      const cleanup = streamTask(response.id, (event) => {
        if (event.type === 'log') {
          const logEntry = event.data as TaskLogEntry;
          setMessages((prev) =>
            prev.map((m) =>
              m.id === `assistant-${response.id}`
                ? { ...m, logs: [...(m.logs || []), logEntry] }
                : m
            )
          );
        } else if (event.type === 'done') {
          const doneData = event.data as { status: string; result: string | null };
          setMessages((prev) =>
            prev.map((m) =>
              m.id === `assistant-${response.id}`
                ? {
                    ...m,
                    content: doneData.result || 'Task completed',
                    status: doneData.status as Message['status'],
                  }
                : m
            )
          );
          setCurrentTaskId(null);
          setIsLoading(false);
          streamCleanupRef.current = null;
        } else if (event.type === 'error') {
          const err = event.data as { message?: string; status?: number };
          setMessages((prev) =>
            prev.map((m) =>
              m.id === `assistant-${response.id}`
                ? {
                    ...m,
                    content: err?.message || 'Streaming failed',
                    status: 'failed',
                  }
                : m
            )
          );
          setCurrentTaskId(null);
          setIsLoading(false);
          streamCleanupRef.current = null;
        }
      });
      streamCleanupRef.current = cleanup;
    } catch (error) {
      console.error('Failed to create task:', error);
      setMessages((prev) => [
        ...prev,
        {
          id: `error-${Date.now()}`,
          role: 'system',
          content: 'Failed to create task. Please try again.',
          timestamp: new Date(),
        },
      ]);
      setIsLoading(false);
    }
  };

  const handleStop = async () => {
    if (!currentTaskId) return;
    const taskId = currentTaskId;
    try {
      // Do NOT disconnect the stream until we know the stop request succeeded.
      await stopTask(taskId);

      // Now stop streaming locally.
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
      setMessages((prev) =>
        prev.map((m) =>
          m.id === `assistant-${taskId}`
            ? { ...m, status: 'cancelled' as const, content: 'Task was cancelled' }
            : m
        )
      );
      setCurrentTaskId(null);
      setIsLoading(false);
    } catch (error) {
      console.error('Failed to stop task:', error);
      setMessages((prev) => [
        ...prev,
        {
          id: `stop-error-${Date.now()}`,
          role: 'system',
          content: 'Failed to stop task. The stream is still connected; try again.',
          timestamp: new Date(),
        },
      ]);
    }
  };

  const toggleLogs = (messageId: string) => {
    setExpandedLogs((prev) => {
      const next = new Set(prev);
      if (next.has(messageId)) {
        next.delete(messageId);
      } else {
        next.add(messageId);
      }
      return next;
    });
  };

  const getLogIcon = (type: string) => {
    switch (type) {
      case 'tool_call':
        return Terminal;
      case 'response':
        return FileText;
      case 'error':
        return XCircle;
      default:
        return Code;
    }
  };

  return (
    <div className="flex h-screen flex-col">
      {/* Header */}
      <div className="border-b border-[var(--border)] bg-[var(--background-secondary)]/70 backdrop-blur px-6 py-4">
        <h1 className="text-xl font-semibold text-[var(--foreground)]">Agent Control</h1>
        <p className="text-sm text-[var(--foreground-muted)]">
          Give tasks to the autonomous agent
        </p>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto p-6">
        {messages.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <div className="text-center">
              <Bot className="mx-auto h-12 w-12 text-[var(--foreground-muted)]" />
              <h2 className="mt-4 text-lg font-medium text-[var(--foreground)]">
                Start a conversation
              </h2>
              <p className="mt-2 text-sm text-[var(--foreground-muted)]">
                Describe a task for the agent to complete
              </p>
            </div>
          </div>
        ) : (
          <div className="mx-auto max-w-3xl space-y-6">
            {messages.map((message) => (
              <div
                key={message.id}
                className={cn(
                  'flex gap-4',
                  message.role === 'user' ? 'justify-end' : 'justify-start'
                )}
              >
                {message.role !== 'user' && (
                  <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br from-[var(--accent)] to-[var(--accent-secondary)]">
                    <Bot className="h-4 w-4 text-white" />
                  </div>
                )}

                <div
                  className={cn(
                    'max-w-[80%] rounded-lg px-4 py-3',
                    message.role === 'user'
                      ? 'bg-[var(--accent)] text-white'
                      : 'bg-[var(--background-secondary)] text-[var(--foreground)]'
                  )}
                >
                  {/* Status badge */}
                  {message.status && message.role === 'assistant' && (
                    <div className="mb-2 flex items-center gap-2">
                      {message.status === 'pending' && (
                        <span className="flex items-center gap-1 text-xs text-[var(--warning)]">
                          <Clock className="h-3 w-3" />
                          Pending
                        </span>
                      )}
                      {message.status === 'running' && (
                        <span className="flex items-center gap-1 text-xs text-[var(--accent)]">
                          <Loader className="h-3 w-3 animate-spin" />
                          Running
                        </span>
                      )}
                      {message.status === 'completed' && (
                        <span className="flex items-center gap-1 text-xs text-[var(--success)]">
                          <CheckCircle className="h-3 w-3" />
                          Completed
                        </span>
                      )}
                      {message.status === 'cancelled' && (
                        <span className="flex items-center gap-1 text-xs text-[var(--foreground-muted)]">
                          <Ban className="h-3 w-3" />
                          Cancelled
                        </span>
                      )}
                      {message.status === 'failed' && (
                        <span className="flex items-center gap-1 text-xs text-[var(--error)]">
                          <XCircle className="h-3 w-3" />
                          Failed
                        </span>
                      )}
                    </div>
                  )}

                  <p className="whitespace-pre-wrap text-sm">{message.content}</p>

                  {/* Logs */}
                  {message.logs && message.logs.length > 0 && (
                    <div className="mt-3 border-t border-[var(--border)] pt-3">
                      <button
                        onClick={() => toggleLogs(message.id)}
                        className="text-xs text-[var(--foreground-muted)] hover:text-[var(--foreground)]"
                      >
                        {expandedLogs.has(message.id)
                          ? `Hide ${message.logs.length} logs`
                          : `Show ${message.logs.length} logs`}
                      </button>

                      {expandedLogs.has(message.id) && (
                        <div className="mt-2 space-y-2">
                          {message.logs.map((log, i) => {
                            const Icon = getLogIcon(log.entry_type);
                            return (
                              <div
                                key={i}
                                className="flex items-start gap-2 rounded-lg bg-[var(--background-tertiary)] p-2 text-xs"
                              >
                                <Icon className="mt-0.5 h-3 w-3 text-[var(--foreground-muted)]" />
                                <span className="font-mono text-[var(--foreground-muted)]">
                                  {log.content.length > 200
                                    ? `${log.content.slice(0, 200)}...`
                                    : log.content}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                      )}
                    </div>
                  )}
                </div>

                {message.role === 'user' && (
                  <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--background-tertiary)]">
                    <User className="h-4 w-4 text-[var(--foreground-muted)]" />
                  </div>
                )}
              </div>
            ))}
            <div ref={messagesEndRef} />
          </div>
        )}
      </div>

      {/* Input */}
      <div className="border-t border-[var(--border)] bg-[var(--background-secondary)]/70 backdrop-blur p-4">
        <form onSubmit={handleSubmit} className="mx-auto flex max-w-3xl gap-3">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Describe a task for the agent..."
            className="flex-1 rounded-lg border border-[var(--border)] bg-[var(--background)]/60 px-4 py-3 text-sm text-[var(--foreground)] placeholder-[var(--foreground-muted)] focus:border-[var(--accent)] focus:outline-none focus-visible:!border-[var(--border)]"
            disabled={isLoading}
          />
          {isLoading ? (
            <button
              type="button"
              onClick={handleStop}
              className="flex items-center gap-2 rounded-lg bg-[var(--error)] px-4 py-3 text-sm font-medium text-white transition-colors hover:bg-[var(--error)]/90"
            >
              <Square className="h-4 w-4" />
              Stop
            </button>
          ) : (
            <button
              type="submit"
              disabled={!input.trim()}
              className="flex items-center gap-2 rounded-lg bg-[var(--accent)] px-4 py-3 text-sm font-medium text-white transition-colors hover:bg-[var(--accent)]/90 disabled:opacity-50"
            >
              <Send className="h-4 w-4" />
              Send
            </button>
          )}
        </form>
      </div>
    </div>
  );
}



