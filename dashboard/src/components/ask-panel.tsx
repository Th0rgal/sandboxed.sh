"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  Sparkles,
  X,
  Plus,
  Trash2,
  Send,
  Loader,
  ChevronDown,
  CornerUpLeft,
  Terminal,
  User,
  FlaskConical,
} from "lucide-react";

import {
  askSend,
  listAskThreads,
  getAskThread,
  deleteAskThread,
  type AskThread,
  type AskMessage,
} from "@/lib/api";
import { LazyMarkdownContent } from "@/components/markdown-content";
import { cn } from "@/lib/utils";

interface AskPanelProps {
  missionId: string;
  onClose: () => void;
  /** Drop a piece of an Ask answer into the real mission composer. */
  onSendToAgent?: (text: string) => void;
  /** Optional text to prefill the composer with (from the "ask about this" spark). */
  seed?: string | null;
  /** Called once the seed has been consumed into the composer. */
  onSeedConsumed?: () => void;
}

/**
 * Ask panel — the web surface for the non-interrupting sidecar co-pilot.
 *
 * Runs in its own lane: it never touches the mission's queue or the working
 * agent. Its conversation lives in a separate store (`ask_threads`/`ask_messages`)
 * and is rendered here with a distinct "co-pilot" identity (cyan/sky), separate
 * from the mission's indigo agent bubbles.
 */
export function AskPanel({
  missionId,
  onClose,
  onSendToAgent,
  seed,
  onSeedConsumed,
}: AskPanelProps) {
  const [threads, setThreads] = useState<AskThread[]>([]);
  const [threadId, setThreadId] = useState<string | null>(null);
  const [messages, setMessages] = useState<AskMessage[]>([]);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showThreadList, setShowThreadList] = useState(false);
  const [sandbox, setSandbox] = useState(false);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  // Auto-grow the composer with input, capped (lighter than the main composer's
  // 10-line cap). Runs on every input change, including seed prefill and reset.
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 120)}px`;
  }, [input]);

  const refreshThreads = useCallback(async () => {
    try {
      const t = await listAskThreads(missionId);
      setThreads(t);
      return t;
    } catch {
      return [];
    }
  }, [missionId]);

  // On mission change: load threads and open the most recent (if any).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      const t = await refreshThreads();
      if (cancelled) return;
      if (t.length > 0) {
        setThreadId(t[0].id);
        try {
          const detail = await getAskThread(missionId, t[0].id);
          if (!cancelled) setMessages(detail.messages ?? []);
        } catch {
          /* ignore */
        }
      } else {
        setThreadId(null);
        setMessages([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [missionId, refreshThreads]);

  // Auto-scroll on new messages.
  useEffect(() => {
    const node = scrollRef.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [messages, loading]);

  // "Ask about this" spark: prefill the composer with the quoted item.
  useEffect(() => {
    if (seed && seed.trim()) {
      const snippet = seed.length > 280 ? `${seed.slice(0, 280)}…` : seed;
      setInput(`About this:\n"""\n${snippet}\n"""\n\n`);
      onSeedConsumed?.();
    }
    // Only react to a new seed; onSeedConsumed clears it so this won't loop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed]);

  const selectThread = useCallback(
    async (id: string) => {
      setShowThreadList(false);
      setThreadId(id);
      setMessages([]);
      try {
        const detail = await getAskThread(missionId, id);
        setMessages(detail.messages ?? []);
      } catch {
        setMessages([]);
      }
    },
    [missionId],
  );

  const newThread = useCallback(() => {
    setShowThreadList(false);
    setThreadId(null);
    setMessages([]);
    setInput("");
  }, []);

  const send = useCallback(async () => {
    const content = input.trim();
    if (!content || loading) return;
    setInput("");
    setError(null);
    setLoading(true);

    // Optimistic user bubble.
    const tempId = `temp-${Date.now()}`;
    setMessages((prev) => [
      ...prev,
      {
        id: tempId,
        thread_id: threadId ?? "",
        seq: prev.length + 1,
        role: "user",
        content,
        created_at: new Date().toISOString(),
      },
    ]);

    try {
      const res = await askSend(
        missionId,
        content,
        threadId ?? undefined,
        sandbox,
      );
      setThreadId(res.thread_id);
      setMessages(res.messages);
      void refreshThreads();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Ask failed");
      // Roll back the optimistic bubble.
      setMessages((prev) => prev.filter((m) => m.id !== tempId));
      setInput(content);
    } finally {
      setLoading(false);
    }
  }, [input, loading, missionId, threadId, sandbox, refreshThreads]);

  const clearActive = useCallback(async () => {
    if (!threadId) {
      newThread();
      return;
    }
    try {
      await deleteAskThread(missionId, threadId);
    } catch {
      /* ignore */
    }
    await refreshThreads();
    newThread();
  }, [missionId, threadId, refreshThreads, newThread]);

  // Theme-aware tokens (the app theme is driven by data-theme, so we key off the
  // CSS variables rather than Tailwind's media-based dark: variant, which can
  // diverge from a manually-stored theme).
  const copilot = "text-[rgb(var(--copilot))]";
  const ctrl =
    "border border-[rgb(var(--foreground)/0.1)] bg-[rgb(var(--foreground)/0.04)] text-[rgb(var(--foreground)/0.6)] hover:bg-[rgb(var(--foreground)/0.07)] hover:text-[rgb(var(--foreground)/0.85)]";

  return (
    <div className="flex h-full w-[380px] shrink-0 flex-col rounded-2xl border border-[rgb(var(--copilot)/0.25)] bg-[rgb(var(--background-elevated)/0.72)] backdrop-blur-xl">
      {/* Header */}
      <div className="flex items-center justify-between gap-2 border-b border-[rgb(var(--foreground)/0.1)] px-3 py-2.5">
        <div className="flex items-center gap-2">
          <div className="flex h-6 w-6 items-center justify-center rounded-full bg-[rgb(var(--copilot)/0.15)]">
            <Sparkles className={cn("h-3.5 w-3.5", copilot)} />
          </div>
          <span className={cn("text-sm font-semibold", copilot)}>Ask</span>
          <span className="rounded bg-[rgb(var(--foreground)/0.06)] px-1.5 py-0.5 text-[10px] text-[rgb(var(--foreground)/0.45)]">
            co-pilot
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={() => setSandbox((v) => !v)}
            title={
              sandbox
                ? "Isolated copy: writes go to a throwaway git worktree"
                : "Run in an isolated copy of the workspace (git only)"
            }
            className={cn(
              "rounded-lg border p-1.5 transition-all active:scale-95",
              sandbox
                ? "border-amber-500/40 bg-amber-500/15 text-amber-600 dark:text-amber-300"
                : ctrl,
            )}
          >
            <FlaskConical className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={() => setShowThreadList((v) => !v)}
            title="Threads"
            className={cn(
              "flex items-center gap-1 rounded-lg border px-2 py-1 text-[11px] tabular-nums transition-all active:scale-95",
              showThreadList
                ? cn(
                    "border-[rgb(var(--copilot)/0.4)] bg-[rgb(var(--copilot)/0.12)]",
                    copilot,
                  )
                : ctrl,
            )}
          >
            {threads.length}
            <ChevronDown
              className={cn(
                "h-3 w-3 transition-transform",
                showThreadList && "rotate-180",
              )}
            />
          </button>
          <button
            type="button"
            onClick={newThread}
            title="New thread"
            className={cn("rounded-lg p-1.5 transition-all active:scale-95", ctrl)}
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={clearActive}
            title="Clear / delete thread"
            className="rounded-lg border border-[rgb(var(--foreground)/0.1)] bg-[rgb(var(--foreground)/0.04)] p-1.5 text-[rgb(var(--foreground)/0.6)] transition-all hover:bg-red-500/10 hover:text-[rgb(var(--error))] active:scale-95"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={onClose}
            title="Close"
            className={cn("rounded-lg p-1.5 transition-all active:scale-95", ctrl)}
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      {/* Thread switcher */}
      {showThreadList && (
        <div className="max-h-48 overflow-y-auto border-b border-[rgb(var(--foreground)/0.1)] bg-[rgb(var(--foreground)/0.03)] p-1.5">
          {threads.length === 0 && (
            <p className="px-2 py-1.5 text-[11px] text-[rgb(var(--foreground)/0.4)]">
              No threads yet.
            </p>
          )}
          {threads.map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => selectThread(t.id)}
              className={cn(
                "block w-full truncate rounded-md px-2 py-1.5 text-left text-[11px] transition-colors",
                t.id === threadId
                  ? cn("bg-[rgb(var(--copilot)/0.12)]", copilot)
                  : "text-[rgb(var(--foreground)/0.6)] hover:bg-[rgb(var(--foreground)/0.05)]",
              )}
            >
              {t.title || "Untitled thread"}
              <span className="ml-1 text-[rgb(var(--foreground)/0.35)]">
                {new Date(t.updated_at).toLocaleTimeString()}
              </span>
            </button>
          ))}
        </div>
      )}

      {/* Messages */}
      <div ref={scrollRef} className="flex-1 space-y-3 overflow-y-auto p-3">
        {messages.length === 0 && !loading && (
          <div className="mx-auto mt-10 flex max-w-[15rem] flex-col items-center text-center">
            <div className="mb-3 flex h-9 w-9 items-center justify-center rounded-xl bg-[rgb(var(--copilot)/0.12)] ring-1 ring-inset ring-[rgb(var(--copilot)/0.25)]">
              <Sparkles className={cn("h-4 w-4", copilot)} />
            </div>
            <p className="text-[13px] font-medium text-[rgb(var(--foreground)/0.75)]">
              Ask about this mission
            </p>
            <p className="mt-1 text-[11.5px] leading-relaxed text-[rgb(var(--foreground)/0.45)]">
              What it&apos;s doing, why, or inspect the workspace. The working
              agent is never interrupted.
            </p>
          </div>
        )}
        {messages.map((m) => (
          <AskBubble key={m.id} message={m} onSendToAgent={onSendToAgent} />
        ))}
        {loading && (
          <div className="flex animate-fade-in justify-start">
            <div className="inline-flex items-center gap-1.5 rounded-full border border-[rgb(var(--copilot)/0.25)] bg-[rgb(var(--copilot)/0.1)] px-2.5 py-1">
              <Loader className="h-3 w-3 animate-spin text-[rgb(var(--copilot))]" />
              <span className="text-xs font-medium text-[rgb(var(--copilot))]">
                Thinking
              </span>
            </div>
          </div>
        )}
        {error && (
          <div className="rounded-lg border border-red-500/30 bg-red-500/10 px-2.5 py-1.5 text-[11px] text-[rgb(var(--error))]">
            {error}
          </div>
        )}
      </div>

      {/* Composer — mirrors the main mission composer's treatment */}
      <div className="border-t border-[rgb(var(--foreground)/0.1)] p-2.5">
        <div className="flex items-end gap-2 rounded-xl border border-[rgb(var(--foreground)/0.08)] bg-[rgb(var(--foreground)/0.03)] px-3.5 py-2.5 transition-[border-color] duration-150 ease-out focus-within:border-[rgb(var(--copilot)/0.5)]">
          <textarea
            ref={textareaRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder="Ask the co-pilot…"
            className="min-h-[24px] flex-1 resize-none overflow-y-auto bg-transparent text-sm leading-5 text-[rgb(var(--foreground)/0.9)] placeholder:text-[rgb(var(--foreground)/0.4)] focus:outline-none"
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={loading || !input.trim()}
            title="Send"
            className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-sky-600 text-white transition-all hover:bg-sky-500 active:scale-95 disabled:opacity-40 disabled:active:scale-100"
          >
            <Send className="h-4 w-4" />
          </button>
        </div>
      </div>
    </div>
  );
}

function AskBubble({
  message,
  onSendToAgent,
}: {
  message: AskMessage;
  onSendToAgent?: (text: string) => void;
}) {
  const { role, content } = message;

  if (role === "user") {
    return (
      <div className="flex justify-end gap-2">
        <div className="max-w-[85%] rounded-2xl rounded-tr-md bg-[rgb(var(--foreground)/0.07)] px-3 py-2">
          <p className="whitespace-pre-wrap break-words text-sm text-[rgb(var(--foreground)/0.9)]">
            {content}
          </p>
        </div>
        <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[rgb(var(--foreground)/0.07)]">
          <User className="h-3.5 w-3.5 text-[rgb(var(--foreground)/0.5)]" />
        </div>
      </div>
    );
  }

  if (role === "tool_call" || role === "tool_result") {
    const isCall = role === "tool_call";
    return (
      <div className="ml-8 flex items-start gap-1.5 text-[11px] text-[rgb(var(--foreground)/0.45)]">
        <Terminal className="mt-0.5 h-3 w-3 shrink-0 text-[rgb(var(--foreground)/0.35)]" />
        <div className="min-w-0 flex-1">
          <span className="text-[rgb(var(--foreground)/0.35)]">
            {isCall ? `${message.tool_name ?? "tool"} →` : "↳"}
          </span>{" "}
          <span className="break-words font-mono">
            {truncate(isCall ? extractCommand(content) : content, 240)}
          </span>
        </div>
      </div>
    );
  }

  // assistant
  return (
    <div className="flex justify-start gap-2">
      <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[rgb(var(--copilot)/0.15)] ring-1 ring-inset ring-[rgb(var(--copilot)/0.25)]">
        <Sparkles className="h-3.5 w-3.5 text-[rgb(var(--copilot))]" />
      </div>
      <div className="group max-w-[85%] rounded-2xl rounded-tl-md border border-[rgb(var(--copilot)/0.18)] bg-[rgb(var(--copilot)/0.07)] px-3 py-2">
        <LazyMarkdownContent content={content} className="text-sm" />
        {onSendToAgent && (
          <button
            type="button"
            onClick={() => onSendToAgent(content)}
            title="Send to the working agent's composer"
            className="mt-1.5 inline-flex items-center gap-1 text-[10px] text-[rgb(var(--foreground)/0.4)] opacity-0 transition-all hover:text-[rgb(var(--copilot))] active:scale-95 group-hover:opacity-100"
          >
            <CornerUpLeft className="h-3 w-3" /> Send to agent
          </button>
        )}
      </div>
    </div>
  );
}

function extractCommand(toolCallJson: string): string {
  try {
    const parsed = JSON.parse(toolCallJson);
    return parsed.command ?? parsed.path ?? toolCallJson;
  } catch {
    return toolCallJson;
  }
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}
