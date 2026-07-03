"use client";

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  Sparkles,
  X,
  Plus,
  Trash2,
  Send,
  Loader,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  CornerUpLeft,
  Terminal,
  User,
  FlaskConical,
} from "lucide-react";

import {
  askSendStream,
  listAskThreads,
  getAskThread,
  deleteAskThread,
  type AskThread,
  type AskMessage,
} from "@/lib/api";
import { LazyMarkdownContent } from "@/components/markdown-content";
import { useMediaQuery } from "@/hooks/use-media-query";
import { LG_MEDIA_QUERY } from "@/lib/responsive-layout";
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
  // Resizable: persisted so the chat/co-pilot split survives reloads.
  // Lazy initializer reads the saved width on first render so the panel never
  // mounts at the default and then jumps to the stored value.
  const [panelWidth, setPanelWidthRaw] = useState(() => {
    if (typeof window === "undefined") return 380;
    const saved = Number(window.localStorage.getItem("ask-panel-width"));
    return Number.isFinite(saved) && saved >= 300 && saved <= 760 ? saved : 380;
  });
  const setPanelWidth = useCallback((w: number) => {
    setPanelWidthRaw(w);
  }, []);
  const persistPanelWidth = useCallback((w: number) => {
    localStorage.setItem("ask-panel-width", String(Math.round(w)));
  }, []);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showThreadList, setShowThreadList] = useState(false);
  const [sandbox, setSandbox] = useState(false);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  // Id of the assistant bubble currently being streamed into (null between segments).
  const streamIdRef = useRef<string | null>(null);
  // Bumped on every send / thread switch so an in-flight post-stream reconcile
  // fetch can detect it's stale and skip overwriting the current view.
  const genRef = useRef(0);
  // Aborts the in-flight stream when a new send / thread switch supersedes it.
  const abortRef = useRef<AbortController | null>(null);

  // Auto-grow the composer with input, capped (lighter than the main composer's
  // 10-line cap). Runs on every input change, including seed prefill and reset.
  // The wrapper height is frozen across the height:auto measurement so the
  // messages scroller above never sees the momentary collapse (which would
  // make the browser clamp its scrollTop — see enhanced-input.tsx).
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const wrapper = ta.parentElement;
    const prevWrapperHeight = wrapper ? wrapper.style.height : "";
    if (wrapper) wrapper.style.height = `${wrapper.offsetHeight}px`;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 120)}px`;
    if (wrapper) wrapper.style.height = prevWrapperHeight;
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
    // A mission switch supersedes any in-flight stream from the old mission, and
    // we clear the old mission's view immediately rather than showing it until
    // the refetch lands.
    genRef.current += 1;
    abortRef.current?.abort();
    setLoading(false);
    streamIdRef.current = null;
    setThreadId(null);
    setMessages([]);
    setThreads([]);
    setError(null);
    const myGen = genRef.current;
    const live = () => !cancelled && genRef.current === myGen;
    (async () => {
      const t = await refreshThreads();
      if (!live()) return;
      if (t.length > 0) {
        setThreadId(t[0].id);
        try {
          const detail = await getAskThread(missionId, t[0].id);
          if (live()) setMessages(detail.messages ?? []);
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
      genRef.current += 1;
      const myGen = genRef.current;
      abortRef.current?.abort();
      setLoading(false);
      streamIdRef.current = null;
      setShowThreadList(false);
      setThreadId(id);
      setMessages([]);
      try {
        const detail = await getAskThread(missionId, id);
        // A later switch / send may have superseded this fetch.
        if (genRef.current === myGen) setMessages(detail.messages ?? []);
      } catch {
        if (genRef.current === myGen) setMessages([]);
      }
    },
    [missionId],
  );

  const newThread = useCallback(() => {
    genRef.current += 1;
    abortRef.current?.abort();
    setLoading(false);
    streamIdRef.current = null;
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
    streamIdRef.current = null;
    genRef.current += 1;
    const myGen = genRef.current;
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;

    // Ids of every bubble this turn added, so a failed turn can roll them back
    // (the backend may not have persisted these fragments).
    const turnIds: string[] = [];
    const rollbackTurn = () =>
      setMessages((prev) => prev.filter((m) => !turnIds.includes(m.id)));

    const now = () => new Date().toISOString();
    const userId = `u-${Date.now()}`;
    turnIds.push(userId);
    setMessages((prev) => [
      ...prev,
      {
        id: userId,
        thread_id: threadId ?? "",
        seq: prev.length + 1,
        role: "user",
        content,
        created_at: now(),
      },
    ]);

    // Append a streamed delta to the current assistant bubble, creating one on
    // the first fragment (so it lands after any preceding tool rows).
    // True once a newer send / thread switch has superseded this turn; its
    // late-arriving stream events must not mutate the current view.
    const stale = () => genRef.current !== myGen;

    const appendDelta = (text: string) => {
      if (stale()) return;
      setMessages((prev) => {
        const id = streamIdRef.current;
        if (id) {
          return prev.map((m) =>
            m.id === id ? { ...m, content: m.content + text } : m,
          );
        }
        const newId = `a-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
        streamIdRef.current = newId;
        turnIds.push(newId);
        return [
          ...prev,
          {
            id: newId,
            thread_id: threadId ?? "",
            seq: prev.length + 1,
            role: "assistant",
            content: text,
            created_at: now(),
          },
        ];
      });
    };

    try {
      await askSendStream(
        missionId,
        content,
        { threadId: threadId ?? undefined, sandbox },
        {
          onDelta: appendDelta,
          onToolCall: (t) => {
            if (stale()) return;
            streamIdRef.current = null; // close the current assistant segment
            turnIds.push(`tc-${t.tool_call_id}`);
            setMessages((prev) => [
              ...prev,
              {
                id: `tc-${t.tool_call_id}`,
                thread_id: threadId ?? "",
                seq: prev.length + 1,
                role: "tool_call",
                content: t.args,
                tool_name: t.name,
                tool_call_id: t.tool_call_id,
                created_at: now(),
              },
            ]);
          },
          onToolResult: (t) => {
            if (stale()) return;
            turnIds.push(`tr-${t.tool_call_id}`);
            setMessages((prev) => [
              ...prev,
              {
                id: `tr-${t.tool_call_id}`,
                thread_id: threadId ?? "",
                seq: prev.length + 1,
                role: "tool_result",
                content: t.result,
                tool_name: t.name,
                tool_call_id: t.tool_call_id,
                created_at: now(),
              },
            ]);
          },
          onDone: (d) => {
            if (stale()) return;
            streamIdRef.current = null;
            setThreadId(d.thread_id);
            // Reconcile the locally-streamed bubbles with the canonical
            // persisted messages (the backend stores tool steps + the final
            // answer, but not pre-tool interim text).
            void (async () => {
              try {
                const detail = await getAskThread(missionId, d.thread_id);
                // Skip if a newer send / thread switch / mission change happened.
                if (genRef.current === myGen) setMessages(detail.messages ?? []);
              } catch {
                /* keep the streamed bubbles on failure */
              }
              if (genRef.current === myGen) void refreshThreads();
            })();
          },
          onError: (msg) => {
            if (stale()) return;
            setError(msg);
            // Drop this turn's in-progress bubbles and restore the question so
            // it isn't lost (unless the user already started a new draft).
            rollbackTurn();
            setInput((cur) => cur || content);
          },
        },
        ac.signal,
      );
    } catch (e) {
      // Superseded turns (thread switch / new send aborted this one) must not
      // surface an error, roll back, or restore input for the new turn.
      if (genRef.current === myGen) {
        setError(e instanceof Error ? e.message : "Ask failed");
        rollbackTurn();
        setInput((cur) => cur || content);
      }
    } finally {
      if (genRef.current === myGen) {
        setLoading(false);
        streamIdRef.current = null;
      }
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
  const isDesktop = useMediaQuery(LG_MEDIA_QUERY);

  return (
    <div
      className={cn(
        "@container relative flex h-full shrink-0 flex-col rounded-2xl border border-[rgb(var(--copilot)/0.25)] bg-[rgb(var(--background-elevated)/0.72)] backdrop-blur-xl",
        !isDesktop && "w-full max-lg:h-[60vh]",
      )}
      style={isDesktop ? { width: panelWidth } : undefined}
    >
      {/* Drag the left edge to trade width between the chat and the co-pilot. */}
      <div
        role="separator"
        aria-orientation="vertical"
        className="absolute -left-1 top-0 z-10 hidden h-full w-2 cursor-col-resize hover:bg-[rgb(var(--copilot)/0.25)] lg:block"
        onPointerDown={(e) => {
          e.preventDefault();
          const startX = e.clientX;
          const startWidth = panelWidth;
          const onMove = (ev: PointerEvent) => {
            const next = Math.min(
              760,
              Math.max(300, startWidth + (startX - ev.clientX)),
            );
            setPanelWidth(next);
          };
          const onUp = (ev: PointerEvent) => {
            persistPanelWidth(
              Math.min(760, Math.max(300, startWidth + (startX - ev.clientX))),
            );
            window.removeEventListener("pointermove", onMove);
            window.removeEventListener("pointerup", onUp);
            window.removeEventListener("pointercancel", onUp);
          };
          window.addEventListener("pointermove", onMove);
          window.addEventListener("pointerup", onUp);
          window.addEventListener("pointercancel", onUp);
        }}
      />
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
        {groupToolRuns(messages).map((item) =>
          item.kind === "msg" ? (
            <AskBubble
              key={item.msg.id}
              message={item.msg}
              onSendToAgent={onSendToAgent}
            />
          ) : (
            <AskToolRun key={item.msgs[0].id} messages={item.msgs} />
          ),
        )}
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
        <div className="flex items-center gap-2 rounded-xl border border-[rgb(var(--foreground)/0.08)] bg-[rgb(var(--foreground)/0.03)] px-3.5 py-2 transition-[border-color] duration-150 ease-out focus-within:border-[rgb(var(--copilot)/0.5)]">
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
            className="min-h-[20px] flex-1 resize-none overflow-y-auto bg-transparent text-sm leading-5 text-[rgb(var(--foreground)/0.9)] placeholder:text-[rgb(var(--foreground)/0.4)] focus:outline-none"
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

type RenderedItem =
  | { kind: "msg"; msg: AskMessage }
  | { kind: "run"; msgs: AskMessage[] };

/** Group consecutive tool_call/tool_result rows into runs so each run gets a
    single collapse affordance (mirrors the main chat's previous-tools fold). */
function groupToolRuns(messages: AskMessage[]): RenderedItem[] {
  const out: RenderedItem[] = [];
  for (const m of messages) {
    if (m.role === "tool_call" || m.role === "tool_result") {
      const last = out[out.length - 1];
      if (last?.kind === "run") last.msgs.push(m);
      else out.push({ kind: "run", msgs: [m] });
    } else {
      out.push({ kind: "msg", msg: m });
    }
  }
  return out;
}

/** A run of consecutive tool steps with a subtle fold toggle underneath.
    Expanded by default (the steps are useful context); one quiet click hides
    the run behind a single summary row. */
function AskToolRun({ messages }: { messages: AskMessage[] }) {
  const [collapsed, setCollapsed] = useState(false);
  const callCount = messages.filter((m) => m.role === "tool_call").length;

  // Keep the toggle pinned under the cursor across fold/unfold (mirrors the
  // main chat's previous-tools fold): steps reveal *upward* into history and
  // the content below the toggle never moves. The toggle's viewport position
  // is captured on click and the scroller re-aligned pre-paint.
  const toggleRef = useRef<HTMLButtonElement | null>(null);
  const anchorTopRef = useRef<number | null>(null);

  const setFolded = (next: boolean) => {
    anchorTopRef.current =
      toggleRef.current?.getBoundingClientRect().top ?? null;
    setCollapsed(next);
  };

  useLayoutEffect(() => {
    const anchorTop = anchorTopRef.current;
    if (anchorTop == null) return;
    anchorTopRef.current = null;
    const el = toggleRef.current;
    if (!el) return;
    const scroller = el.closest(".overflow-y-auto");
    if (!scroller) return;
    const delta = el.getBoundingClientRect().top - anchorTop;
    if (Math.abs(delta) > 0.5) scroller.scrollTop += delta;
  }, [collapsed]);

  if (collapsed) {
    return (
      <button
        ref={toggleRef}
        type="button"
        onClick={() => setFolded(false)}
        className="ml-8 flex items-center gap-1 text-[11px] text-[rgb(var(--foreground)/0.35)] transition-colors hover:text-[rgb(var(--foreground)/0.6)]"
        title="Show tool steps (they reveal above)"
      >
        <ChevronUp className="h-3 w-3" />
        {callCount} tool step{callCount === 1 ? "" : "s"} hidden
      </button>
    );
  }

  return (
    <div className="space-y-3">
      {messages.map((m) => (
        <AskBubble key={m.id} message={m} />
      ))}
      <button
        ref={toggleRef}
        type="button"
        onClick={() => setFolded(true)}
        className="ml-8 flex items-center gap-1 text-[10px] text-[rgb(var(--foreground)/0.3)] transition-colors hover:text-[rgb(var(--foreground)/0.55)]"
        title="Hide tool steps"
      >
        <ChevronRight className="h-3 w-3" />
        Hide tool steps
      </button>
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
        <div className="@max-[24rem]:hidden flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[rgb(var(--foreground)/0.07)]">
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
      <div className="@max-[24rem]:hidden flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[rgb(var(--copilot)/0.15)] ring-1 ring-inset ring-[rgb(var(--copilot)/0.25)]">
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
