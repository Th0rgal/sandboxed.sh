"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  Brain,
  ChevronDown,
  ChevronRight,
  Clock,
  Loader,
  Plus,
  Send,
  Square,
  Terminal,
  Trash2,
  User,
} from "lucide-react";

import {
  createHermesSession,
  deleteHermesSession,
  getHermesSessionMessages,
  hermesChatStream,
  listHermesSessions,
  type HermesMessage,
  type HermesSession,
} from "@/lib/api";
import { LazyMarkdownContent } from "@/components/markdown-content";
import { cn } from "@/lib/utils";

/** One rendered row in the Hermes transcript. Tool/thinking rows are built
 * from stream events; user/assistant rows mirror the persisted messages. */
interface ChatItem {
  id: string;
  role: "user" | "assistant" | "thinking" | "tool";
  content: string;
  toolName?: string;
  toolStatus?: "running" | "done" | "failed";
}

const TRANSCRIPT_POLL_MS = 5_000;

let nextLocalId = 0;
function localId(prefix: string): string {
  nextLocalId += 1;
  return `${prefix}-${nextLocalId}`;
}

function persistedRows(messages: HermesMessage[]): ChatItem[] {
  const rows: ChatItem[] = [];
  for (const message of messages) {
    const id =
      message.id == null
        ? localId("hist")
        : `persisted-${String(message.id)}`;
    if (message.role === "user" || message.role === "assistant") {
      const content = (message.content ?? "").trim();
      if (content) rows.push({ id, role: message.role, content });
    } else if (message.role === "tool") {
      rows.push({
        id,
        role: "tool",
        content: (message.content ?? "").trim(),
        toolName: message.tool_name ?? undefined,
        toolStatus: "done",
      });
    }
  }
  return rows;
}

function sameTranscript(left: ChatItem[], right: ChatItem[]): boolean {
  return (
    left.length === right.length &&
    left.every((item, index) => {
      const other = right[index];
      return (
        item.role === other?.role &&
        item.content === other.content &&
        item.toolName === other.toolName &&
        item.toolStatus === other.toolStatus
      );
    })
  );
}

export function HermesThread({ className }: { className?: string }) {
  const [sessions, setSessions] = useState<HermesSession[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [items, setItems] = useState<ChatItem[]>([]);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showSessions, setShowSessions] = useState(false);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  // Bumped on send / session switch so stale async work can detect it.
  const genRef = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  // Ids of the rows the current stream writes into.
  const streamAssistantIdRef = useRef<string | null>(null);
  const streamThinkingIdRef = useRef<string | null>(null);

  // Auto-grow composer, capped. The wrapper height is frozen across the
  // height:auto measurement so the transcript scroller never sees the
  // momentary collapse (browser would clamp its scrollTop — see
  // enhanced-input.tsx).
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const wrapper = ta.parentElement;
    const prevWrapperHeight = wrapper ? wrapper.style.height : "";
    if (wrapper) wrapper.style.height = `${wrapper.offsetHeight}px`;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 160)}px`;
    if (wrapper) wrapper.style.height = prevWrapperHeight;
  }, [input]);

  // Pin to bottom as content streams in.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items, loading]);

  const refreshSessions = useCallback(async () => {
    try {
      setSessions(await listHermesSessions());
    } catch {
      /* session list is decoration; chat still works */
    }
  }, []);

  useEffect(() => {
    void refreshSessions();
  }, [refreshSessions]);

  const loadSession = useCallback(async (id: string) => {
    genRef.current += 1;
    const gen = genRef.current;
    abortRef.current?.abort();
    setSessionId(id);
    setItems([]);
    setError(null);
    setHistoryLoading(true);
    try {
      const messages = await getHermesSessionMessages(id);
      if (genRef.current !== gen) return;
      setItems(persistedRows(messages));
    } catch (e) {
      if (genRef.current === gen) {
        setError(e instanceof Error ? e.message : "Failed to load session");
      }
    } finally {
      if (genRef.current === gen) setHistoryLoading(false);
    }
  }, []);

  // Cron/callback delivery for Desktop sessions is persisted after the
  // originating HTTP stream has ended. Refresh the active transcript so those
  // durable assistant turns appear in the same conversation without requiring
  // a manual session switch or page reload.
  useEffect(() => {
    if (!sessionId || loading || historyLoading) return;

    let cancelled = false;
    const refreshTranscript = async () => {
      if (document.visibilityState === "hidden") return;
      try {
        const messages = await getHermesSessionMessages(sessionId);
        if (cancelled) return;
        const rows = persistedRows(messages);
        setItems((current) => {
          if (sameTranscript(current, rows)) return current;
          return rows;
        });
      } catch {
        // Background refresh is best-effort. Explicit session loads and sends
        // still surface errors through the normal UI.
      }
    };

    const timer = window.setInterval(
      () => void refreshTranscript(),
      TRANSCRIPT_POLL_MS,
    );
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [historyLoading, loading, sessionId]);

  const newSession = useCallback(() => {
    genRef.current += 1;
    abortRef.current?.abort();
    setSessionId(null);
    setItems([]);
    setError(null);
    setShowSessions(false);
  }, []);

  const removeSession = useCallback(
    async (id: string) => {
      try {
        await deleteHermesSession(id);
        setSessions((prev) => prev.filter((s) => s.id !== id));
        if (sessionId === id) newSession();
      } catch (e) {
        setError(e instanceof Error ? e.message : "Failed to delete session");
      }
    },
    [sessionId, newSession],
  );

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const send = useCallback(async () => {
    const content = input.trim();
    if (!content || loading) return;

    genRef.current += 1;
    const gen = genRef.current;
    const stale = () => genRef.current !== gen;

    setInput("");
    setError(null);
    setLoading(true);

    const userRow: ChatItem = { id: localId("user"), role: "user", content };
    // Every row this turn adds, so an error can roll the whole turn back.
    const turnIds = new Set<string>([userRow.id]);
    setItems((prev) => [...prev, userRow]);
    streamAssistantIdRef.current = null;
    streamThinkingIdRef.current = null;

    const appendRow = (row: ChatItem) => {
      turnIds.add(row.id);
      setItems((prev) => [...prev, row]);
    };
    const patchRow = (id: string, patch: Partial<ChatItem>) => {
      setItems((prev) =>
        prev.map((r) => (r.id === id ? { ...r, ...patch } : r)),
      );
    };

    try {
      let sid = sessionId;
      if (!sid) {
        const session = await createHermesSession();
        if (stale()) return;
        sid = session.id;
        setSessionId(sid);
        void refreshSessions();
      }

      const controller = new AbortController();
      abortRef.current = controller;

      await hermesChatStream(
        sid,
        content,
        {
          onDelta: (text) => {
            if (stale()) return;
            const id = streamAssistantIdRef.current;
            if (id) {
              setItems((prev) =>
                prev.map((r) =>
                  r.id === id ? { ...r, content: r.content + text } : r,
                ),
              );
            } else {
              const row: ChatItem = {
                id: localId("assistant"),
                role: "assistant",
                content: text,
              };
              streamAssistantIdRef.current = row.id;
              appendRow(row);
            }
          },
          onThinking: (text) => {
            if (stale()) return;
            const id = streamThinkingIdRef.current;
            if (id) {
              setItems((prev) =>
                prev.map((r) =>
                  r.id === id ? { ...r, content: r.content + text } : r,
                ),
              );
            } else {
              const row: ChatItem = {
                id: localId("think"),
                role: "thinking",
                content: text,
              };
              streamThinkingIdRef.current = row.id;
              appendRow(row);
            }
          },
          onToolStart: (t) => {
            if (stale()) return;
            // A tool call closes the current text/thinking segments.
            streamAssistantIdRef.current = null;
            streamThinkingIdRef.current = null;
            appendRow({
              id: localId("tool"),
              role: "tool",
              content: t.preview ?? "",
              toolName: t.tool_name,
              toolStatus: "running",
            });
          },
          onToolComplete: (t) => {
            if (stale()) return;
            setItems((prev) => {
              const idx = findLastRunningTool(prev, t.tool_name);
              if (idx < 0) return prev;
              const next = [...prev];
              next[idx] = {
                ...next[idx],
                toolStatus: "done",
                content: t.preview ?? next[idx].content,
              };
              return next;
            });
          },
          onToolFailed: (t) => {
            if (stale()) return;
            setItems((prev) => {
              const idx = findLastRunningTool(prev, t.tool_name);
              if (idx < 0) return prev;
              const next = [...prev];
              next[idx] = {
                ...next[idx],
                toolStatus: "failed",
                content: t.preview ?? next[idx].content,
              };
              return next;
            });
          },
          onCompleted: (finalContent) => {
            if (stale() || !finalContent.trim()) return;
            // Authoritative turn text: replace the streamed bubble (deltas can
            // be lossy) or append if the turn produced no deltas.
            const id = streamAssistantIdRef.current;
            if (id) {
              patchRow(id, { content: finalContent });
            } else {
              appendRow({
                id: localId("assistant"),
                role: "assistant",
                content: finalContent,
              });
            }
          },
          onError: (message) => {
            if (stale()) return;
            setError(message);
          },
        },
        controller.signal,
      );
      if (!stale()) void refreshSessions();
    } catch (e) {
      if (!stale()) {
        if (e instanceof DOMException && e.name === "AbortError") {
          // User stop: keep whatever streamed in.
        } else {
          setError(e instanceof Error ? e.message : "Send failed");
          setItems((prev) => prev.filter((r) => !turnIds.has(r.id)));
          setInput(content);
        }
      }
    } finally {
      if (!stale()) setLoading(false);
    }
  }, [input, loading, sessionId, refreshSessions]);

  const activeSession = sessions.find((s) => s.id === sessionId);

  return (
    <div className={cn("flex min-h-0 flex-col", className)}>
      {/* Session bar — left edge aligned with the page header (px-6) */}
      <div className="relative flex h-11 shrink-0 items-center gap-2 border-b border-white/[0.06] px-6">
        <button
          type="button"
          onClick={() => setShowSessions((v) => !v)}
          className="-ml-2 flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-[rgb(var(--foreground)/0.7)] transition-colors hover:bg-[rgb(var(--foreground)/0.05)] hover:text-[rgb(var(--foreground)/0.9)]"
        >
          <span className="truncate">
            {activeSession?.title?.trim() ||
              (sessionId ? sessionId : "New conversation")}
          </span>
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[rgb(var(--foreground)/0.4)]" />
        </button>
        <div className="flex-1" />
        <button
          type="button"
          onClick={newSession}
          title="New conversation"
          className="inline-flex h-7 w-7 items-center justify-center rounded-lg text-[rgb(var(--foreground)/0.5)] transition-colors hover:bg-[rgb(var(--foreground)/0.06)] hover:text-[rgb(var(--foreground)/0.8)]"
        >
          <Plus className="h-4 w-4" />
        </button>

        {showSessions && (
          <div className="absolute left-6 top-full z-20 mt-1 max-h-80 w-80 overflow-y-auto rounded-xl border border-white/[0.08] bg-[rgb(var(--background-elevated))] p-1 shadow-xl">
            {sessions.length === 0 && (
              <p className="px-3 py-2 text-xs text-white/40">
                No conversations yet.
              </p>
            )}
            {sessions.map((s) => (
              <div
                key={s.id}
                className={cn(
                  "group flex items-center gap-2 rounded-lg px-2.5 py-1.5",
                  s.id === sessionId ? "bg-white/[0.07]" : "hover:bg-white/[0.04]",
                )}
              >
                <button
                  type="button"
                  onClick={() => {
                    setShowSessions(false);
                    void loadSession(s.id);
                  }}
                  className="min-w-0 flex-1 text-left"
                >
                  <span className="block truncate text-sm text-white/80">
                    {s.title?.trim() || s.preview?.trim() || s.id}
                  </span>
                  {s.last_active && (
                    <span className="block truncate text-[10px] text-white/35">
                      {s.last_active}
                    </span>
                  )}
                </button>
                <button
                  type="button"
                  onClick={() => void removeSession(s.id)}
                  title="Delete conversation"
                  className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded text-white/30 opacity-0 transition-all hover:text-red-400 group-hover:opacity-100"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Transcript */}
      <div ref={scrollRef} className="flex-1 space-y-3 overflow-y-auto px-6 py-4">
        {historyLoading && (
          <div className="flex items-center gap-2 text-xs text-white/40">
            <Loader className="h-3.5 w-3.5 animate-spin" /> Loading conversation…
          </div>
        )}
        {!historyLoading && items.length === 0 && (
          <div className="mt-16 text-center">
            <p className="text-sm text-white/60">What should Hermes handle?</p>
            <p className="mx-auto mt-1 max-w-sm text-xs text-white/35">
              Your assistant can start missions, check on running ones and
              answer questions about your fleet. Mission IDs it mentions become
              clickable.
            </p>
          </div>
        )}
        {items.map((item) => (
          <HermesRow key={item.id} item={item} />
        ))}
        {loading && (
          <div className="flex items-center gap-2 text-xs text-white/40">
            <Loader className="h-3.5 w-3.5 animate-spin" /> Hermes is working…
          </div>
        )}
        {error && (
          <p className="rounded-lg border border-red-500/25 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
          </p>
        )}
      </div>

      {/* Composer — same row grammar as the Ask panel composer */}
      <div className="border-t border-white/[0.06] px-6 py-3">
        <div className="flex items-center gap-2 rounded-xl border border-[rgb(var(--foreground)/0.08)] bg-[rgb(var(--foreground)/0.03)] px-3.5 py-2 transition-[border-color] duration-150 ease-out focus-within:border-indigo-400/50">
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
            placeholder="Message Hermes…"
            className="min-h-[20px] flex-1 resize-none overflow-y-auto bg-transparent text-sm leading-5 text-[rgb(var(--foreground)/0.9)] placeholder:text-[rgb(var(--foreground)/0.4)] focus:outline-none"
          />
          {loading ? (
            <button
              type="button"
              onClick={stop}
              title="Stop"
              className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-white/[0.08] text-white/70 transition-all hover:bg-white/[0.12] active:scale-95"
            >
              <Square className="h-3.5 w-3.5" />
            </button>
          ) : (
            <button
              type="button"
              onClick={() => void send()}
              disabled={!input.trim()}
              title="Send"
              className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-indigo-500 text-white transition-all hover:bg-indigo-600 active:scale-95 disabled:opacity-40 disabled:active:scale-100"
            >
              <Send className="h-4 w-4" />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/** Hermes annotates durable deliveries (cron, callbacks) with a bracketed
 * prefix on the message text, e.g. "[Cron delivery: Beal roadmap — current
 * Desktop session] …". The prefix comes from upstream Hermes, so we lift it
 * into a visual divider here instead of showing it inline. */
const DELIVERY_PREFIX = /^\[([A-Za-z]+) delivery:\s*([^\]]*)\]\s*/;

function parseDelivery(
  content: string,
): { kind: string; label: string; body: string } | null {
  const match = DELIVERY_PREFIX.exec(content);
  if (!match) return null;
  // "— current Desktop session" is routing boilerplate, not a title.
  const label = match[2].replace(/\s*—\s*current\s+.*?session\s*$/i, "").trim();
  return { kind: match[1], label, body: content.slice(match[0].length) };
}

function DeliveryDivider({ kind, label }: { kind: string; label: string }) {
  return (
    <div className="flex items-center gap-3 pt-2">
      <div className="h-px flex-1 bg-[rgb(var(--foreground)/0.08)]" />
      <span className="flex min-w-0 items-center gap-1.5 rounded-full border border-[rgb(var(--foreground)/0.08)] bg-[rgb(var(--foreground)/0.03)] px-2.5 py-0.5 text-[11px] text-[rgb(var(--foreground)/0.5)]">
        <Clock className="h-3 w-3 shrink-0 text-[rgb(var(--foreground)/0.4)]" />
        <span className="shrink-0">{kind}</span>
        {label && (
          <>
            <span className="shrink-0 text-[rgb(var(--foreground)/0.35)]">·</span>
            <span className="truncate">{label}</span>
          </>
        )}
      </span>
      <div className="h-px flex-1 bg-[rgb(var(--foreground)/0.08)]" />
    </div>
  );
}

function findLastRunningTool(rows: ChatItem[], toolName?: string): number {
  for (let i = rows.length - 1; i >= 0; i -= 1) {
    const r = rows[i];
    if (
      r.role === "tool" &&
      r.toolStatus === "running" &&
      (!toolName || r.toolName === toolName)
    ) {
      return i;
    }
  }
  return -1;
}

function HermesRow({ item }: { item: ChatItem }) {
  if (item.role === "user") {
    return (
      <div className="flex justify-end gap-2">
        <div className="max-w-[85%] rounded-2xl rounded-tr-md bg-[rgb(var(--foreground)/0.07)] px-3 py-2">
          <p className="whitespace-pre-wrap break-words text-sm text-[rgb(var(--foreground)/0.9)]">
            {item.content}
          </p>
        </div>
        <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[rgb(var(--foreground)/0.07)]">
          <User className="h-3.5 w-3.5 text-[rgb(var(--foreground)/0.5)]" />
        </div>
      </div>
    );
  }

  if (item.role === "thinking") {
    return <ThinkingRow content={item.content} />;
  }

  if (item.role === "tool") {
    return (
      <div className="ml-8 flex items-start gap-1.5 text-[11px] text-[rgb(var(--foreground)/0.45)]">
        {item.toolStatus === "running" ? (
          <Loader className="mt-0.5 h-3 w-3 shrink-0 animate-spin text-[rgb(var(--foreground)/0.35)]" />
        ) : (
          <Terminal
            className={cn(
              "mt-0.5 h-3 w-3 shrink-0",
              item.toolStatus === "failed"
                ? "text-red-400/70"
                : "text-[rgb(var(--foreground)/0.35)]",
            )}
          />
        )}
        <div className="min-w-0 flex-1">
          <span className="text-[rgb(var(--foreground)/0.35)]">
            {item.toolName ?? "tool"}
          </span>
          {item.content && item.content !== item.toolName && (
            <>
              {" "}
              <span className="break-words font-mono">
                {truncate(item.content, 240)}
              </span>
            </>
          )}
        </div>
      </div>
    );
  }

  // assistant
  const delivery = parseDelivery(item.content);
  return (
    <>
      {delivery && (
        <DeliveryDivider kind={delivery.kind} label={delivery.label} />
      )}
      <div className="flex justify-start gap-2">
        <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-indigo-500/15 ring-1 ring-inset ring-indigo-400/25">
          <Brain className="h-3.5 w-3.5 text-indigo-300" />
        </div>
        <div className="max-w-[85%] rounded-2xl rounded-tl-md border border-[rgb(var(--foreground)/0.08)] bg-[rgb(var(--foreground)/0.03)] px-3 py-2">
          <LazyMarkdownContent
            content={delivery ? delivery.body : item.content}
            className="text-sm"
            missionLinks
          />
        </div>
      </div>
    </>
  );
}

/** Collapsible reasoning block, collapsed by default once the turn moves on. */
function ThinkingRow({ content }: { content: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="ml-8">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 text-[11px] text-[rgb(var(--foreground)/0.35)] transition-colors hover:text-[rgb(var(--foreground)/0.6)]"
      >
        {open ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
        <Brain className="h-3 w-3" /> thinking
      </button>
      {open && (
        <p className="mt-1 whitespace-pre-wrap break-words border-l border-[rgb(var(--foreground)/0.08)] pl-3 text-[11px] leading-relaxed text-[rgb(var(--foreground)/0.4)]">
          {content}
        </p>
      )}
    </div>
  );
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}
