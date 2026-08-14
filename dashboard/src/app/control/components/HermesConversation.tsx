"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { Loader, Plus, Sparkles, Square } from "lucide-react";

import {
  getHermesSessionMessages,
  hermesChatStream,
  listHermesSessions,
  listMissions,
  type HermesMessage,
  type HermesSession,
  type Mission,
} from "@/lib/api";
import { getMissionTitle, STATUS_DOT_COLORS } from "@/lib/mission-status";
import {
  HermesGatewayClient,
  type HermesGatewayEvent,
} from "@/lib/hermes-gateway";
import {
  createHermesDeliveryId,
  getHermesOutbox,
  hermesDeliveryObserved,
  putHermesOutbox,
  removeHermesOutbox,
  type HermesOutboxEntry,
} from "@/lib/hermes-outbox";
import { EnhancedInput } from "@/components/enhanced-input";
import { stateSignatureKeyFromMessages } from "@/lib/hermes-state-signature";
import { cn } from "@/lib/utils";

import { SessionProjectRail } from "./session-project-rail";
import {
  ChatItemRow,
  deriveItemViews,
  getGroupedItemKey,
} from "../control-client";
import type { ChatItem } from "../events-reducer";
import {
  HermesLiveTranscript,
  hermesHistoryToItems,
} from "../hermes-session-adapter";

let nextLocalId = 0;
function localId(prefix: string): string {
  nextLocalId += 1;
  return `${prefix}-${nextLocalId}`;
}

interface ResumeResult {
  session_id?: string;
  messages?: HermesMessage[];
  running?: boolean;
  inflight?: { user?: string | null } | null;
  info?: { model?: string };
}

type Transport = "connecting" | "ws" | "rest" | "offline";

/**
 * A Hermes session rendered through the control-page transcript pipeline
 * (ChatItem → deriveItemViews → ChatItemRow), so sessions and missions share
 * one visual grammar. Transport is WS-first (the Hermes gateway JSON-RPC
 * bridge, live events for turns started from any platform) with a REST+SSE
 * fallback (history poll + per-turn chat stream) when the bridge is not
 * provisioned.
 */
export function HermesConversation({ sessionId }: { sessionId: string }) {
  const [items, setItems] = useState<ChatItem[]>([]);
  const [running, setRunning] = useState(false);
  const [transport, setTransport] = useState<Transport>("connecting");
  const [error, setError] = useState<string | null>(null);
  const [historyLoaded, setHistoryLoaded] = useState(false);
  const [session, setSession] = useState<HermesSession | null>(null);
  // Routing key from the transcript's last [STATE_SIGNATURE: …] trailer —
  // the display name of last resort for sessions that never got a title
  // (cron controllers), matching what the desktop sidebar shows.
  const [signatureKey, setSignatureKey] = useState<string | null>(null);
  const [workerMissions, setWorkerMissions] = useState<Mission[]>([]);
  const [input, setInput] = useState("");
  const [expandedToolGroups, setExpandedToolGroups] = useState<Set<string>>(
    () => new Set(),
  );

  const clientRef = useRef<HermesGatewayClient | null>(null);
  const transcriptRef = useRef(new HermesLiveTranscript());
  // The live RPC handle for this session (session.resume may return an
  // ephemeral id distinct from the stored one).
  const handleRef = useRef<string>(sessionId);
  const restAbortRef = useRef<AbortController | null>(null);
  // Open REST tool rows by name (REST tool events carry no tool_id).
  const restToolIdsByName = useRef(new Map<string, string[]>());
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const generationRef = useRef(0);
  const reconnectingRef = useRef(false);
  const deliverRef = useRef<
    ((entry: HermesOutboxEntry) => Promise<void>) | null
  >(null);

  const syncFromTranscript = useCallback(() => {
    setItems([...transcriptRef.current.items]);
    setRunning(transcriptRef.current.running);
  }, []);

  const markDelivery = useCallback(
    (
      entry: HermesOutboxEntry,
      status: "sending" | "sent" | "failed",
      reason?: "network" | "rejected",
    ) => {
      const transcript = transcriptRef.current;
      const index = transcript.items.findIndex(
        (item) => item.kind === "user" && item.id === entry.id,
      );
      if (index >= 0) {
        transcript.items = transcript.items.map((item, itemIndex) =>
          itemIndex === index && item.kind === "user"
            ? { ...item, sendStatus: status, failedReason: reason }
            : item,
        );
      } else {
        transcript.items = [
          ...transcript.items,
          {
            kind: "user",
            id: entry.id,
            content: entry.content,
            timestamp: entry.createdAt,
            sendStatus: status,
            failedReason: reason,
          },
        ];
      }
      syncFromTranscript();
    },
    [syncFromTranscript],
  );

  // ── Connection lifecycle ──────────────────────────────────────────────────
  useEffect(() => {
    const generation = ++generationRef.current;
    const transcript = transcriptRef.current;
    handleRef.current = sessionId;

    const client = new HermesGatewayClient();
    clientRef.current = client;
    let disposed = false;
    let hasConnected = false;

    const applyEvent = (event: HermesGatewayEvent) => {
      if (disposed || generationRef.current !== generation) return;
      if (event.session_id && event.session_id !== handleRef.current) return;
      if (transcript.apply(event)) syncFromTranscript();
    };

    const loadRestHistory = async () => {
      const messages = await getHermesSessionMessages(sessionId);
      if (disposed || generationRef.current !== generation) return;
      transcript.reset(hermesHistoryToItems(messages));
      setSignatureKey(stateSignatureKeyFromMessages(messages));
      syncFromTranscript();
      setHistoryLoaded(true);
    };

    const deliver = async (
      entry: HermesOutboxEntry,
      activeClient: HermesGatewayClient,
    ) => {
      markDelivery(entry, "sending");
      try {
        await activeClient.request("prompt.submit", {
          session_id: handleRef.current,
          text: entry.content,
          // Current Hermes ignores unknown prompt fields. The bridge/client
          // retain this stable id for observability and future server dedupe.
          client_message_id: entry.id,
        });
        removeHermesOutbox(entry.id);
        markDelivery(entry, "sent");
      } catch (sendError) {
        const disconnected = activeClient.getState() !== "open";
        if (!disconnected) removeHermesOutbox(entry.id);
        transcript.running = false;
        markDelivery(entry, "failed", disconnected ? "network" : "rejected");
        throw sendError;
      }
    };
    deliverRef.current = (entry) => deliver(entry, client);

    const resume = async () => {
      await client.connect();
      const result = await client.request<ResumeResult>("session.resume", {
        session_id: sessionId,
      });
      if (disposed || generationRef.current !== generation) return;
      if (result?.session_id) handleRef.current = result.session_id;
      const messages = Array.isArray(result?.messages)
        ? result.messages
        : await getHermesSessionMessages(sessionId);
      if (disposed || generationRef.current !== generation) return;
      transcript.reset(hermesHistoryToItems(messages));
      setSignatureKey(stateSignatureKeyFromMessages(messages));
      transcript.running = Boolean(result?.running);

      const pending = getHermesOutbox(sessionId);
      for (const entry of pending) {
        if (
          hermesDeliveryObserved(entry, { messages, inflight: result.inflight })
        ) {
          removeHermesOutbox(entry.id);
          if (!hermesDeliveryObserved(entry, { messages })) {
            markDelivery(entry, "sent");
          }
          continue;
        }
        markDelivery(entry, "failed", "network");
      }

      syncFromTranscript();
      setHistoryLoaded(true);
      setTransport("ws");
      hasConnected = true;

      // Only redeliver after the authoritative resume snapshot proved that
      // Hermes neither persisted nor started this prompt.
      if (!result?.running) {
        for (const entry of getHermesOutbox(sessionId)) {
          if (disposed || generationRef.current !== generation) return;
          try {
            await deliver(entry, client);
          } catch {
            break;
          }
        }
      }
    };

    const reconnect = async () => {
      if (reconnectingRef.current || disposed) return;
      reconnectingRef.current = true;
      setTransport("connecting");
      let delay = 500;
      try {
        while (!disposed && generationRef.current === generation) {
          await new Promise((resolve) => setTimeout(resolve, delay));
          try {
            await resume();
            return;
          } catch {
            delay = Math.min(delay * 2, 5_000);
          }
        }
      } finally {
        reconnectingRef.current = false;
      }
    };

    const unsubscribeState = client.onState((state) => {
      if (disposed || generationRef.current !== generation) return;
      if (state === "error" && hasConnected) void reconnect();
    });

    const unsubscribeEvent = client.onEvent(applyEvent);
    setHistoryLoaded(false);
    setTransport("connecting");
    setError(null);
    void (async () => {
      try {
        await resume();
      } catch {
        if (disposed || generationRef.current !== generation) return;
        client.close();
        try {
          await loadRestHistory();
          setTransport("rest");
        } catch (restErr) {
          if (disposed || generationRef.current !== generation) return;
          setTransport("offline");
          setError(
            restErr instanceof Error
              ? restErr.message
              : "Hermes is unreachable",
          );
        }
      }
    })();

    return () => {
      disposed = true;
      unsubscribeState();
      unsubscribeEvent();
      if (clientRef.current === client) {
        clientRef.current = null;
        deliverRef.current = null;
      }
      client.close();
      restAbortRef.current?.abort();
      restAbortRef.current = null;
    };
  }, [markDelivery, sessionId, syncFromTranscript]);

  // Session metadata (title) + worker missions spawned by this session.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const sessions = await listHermesSessions(200);
        if (cancelled) return;
        setSession(sessions.find((s) => s.id === sessionId) ?? null);
      } catch {
        // Title stays generic; not fatal.
      }
      try {
        // Server-side filter: fetching the newest 50 and filtering here used
        // to show zero workers for any conversation whose missions had
        // scrolled out of that page.
        const missions = await listMissions({
          originSessionId: sessionId,
          limit: 200,
        });
        if (cancelled) return;
        setWorkerMissions(missions);
      } catch {
        // Worker strip stays empty; not fatal.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sessionId, running]);
  // REST transport: re-poll the transcript while idle so turns driven from
  // other platforms (Telegram, cron) eventually appear.
  useEffect(() => {
    if (transport !== "rest" || running) return;
    const timer = setInterval(() => {
      void (async () => {
        try {
          const messages = await getHermesSessionMessages(sessionId);
          const fresh = hermesHistoryToItems(messages);
          const transcript = transcriptRef.current;
          if (!transcript.running && fresh.length !== transcript.items.length) {
            transcript.reset(fresh);
            setSignatureKey(stateSignatureKeyFromMessages(messages));
            syncFromTranscript();
          }
        } catch {
          // Transient poll failure; next tick retries.
        }
      })();
    }, 5000);
    return () => clearInterval(timer);
  }, [transport, running, sessionId, syncFromTranscript]);

  // Pin to bottom as content streams in.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items]);

  const pushUserItem = useCallback(
    (entry: HermesOutboxEntry) => {
      const transcript = transcriptRef.current;
      transcript.items = [
        ...transcript.items,
        {
          kind: "user",
          id: entry.id,
          content: entry.content,
          timestamp: entry.createdAt,
          sendStatus: "sending",
        },
      ];
      transcript.running = true;
      syncFromTranscript();
    },
    [syncFromTranscript],
  );

  const sendViaRest = useCallback(
    (entry: HermesOutboxEntry) => {
      const transcript = transcriptRef.current;
      const abort = new AbortController();
      restAbortRef.current = abort;
      const synthesize = (event: HermesGatewayEvent) => {
        if (transcript.apply(event)) syncFromTranscript();
      };
      const openRestTool = (name: string, args: unknown) => {
        const toolId = localId("rest-tool");
        const stack = restToolIdsByName.current.get(name) ?? [];
        stack.push(toolId);
        restToolIdsByName.current.set(name, stack);
        synthesize({
          type: "tool.start",
          payload: { tool_id: toolId, name, args },
        });
      };
      const closeRestTool = (name: string, result: unknown) => {
        const stack = restToolIdsByName.current.get(name);
        const toolId = stack?.shift();
        if (toolId) {
          synthesize({
            type: "tool.complete",
            payload: { tool_id: toolId, result },
          });
        }
      };
      void (async () => {
        let streamFailure: string | null = null;
        try {
          await hermesChatStream(
            sessionId,
            entry.content,
            {
              onDelta: (text) =>
                synthesize({ type: "message.delta", payload: { text } }),
              onThinking: (text) =>
                synthesize({ type: "thinking.delta", payload: { text } }),
              onToolStart: (t) => openRestTool(t.tool_name ?? "tool", t.args),
              onToolComplete: (t) =>
                closeRestTool(t.tool_name ?? "tool", t.preview),
              onToolFailed: (t) =>
                closeRestTool(t.tool_name ?? "tool", t.preview ?? "failed"),
              onCompleted: (text) =>
                synthesize({ type: "message.complete", payload: { text } }),
              onError: (message) => {
                streamFailure = message;
                synthesize({ type: "error", payload: { message } });
              },
            },
            abort.signal,
          );
          if (streamFailure) throw new Error(streamFailure);
          removeHermesOutbox(entry.id);
          markDelivery(entry, "sent");
        } catch (err: unknown) {
          if (abort.signal.aborted) return;
          markDelivery(entry, "failed", "network");
          synthesize({
            type: "error",
            payload: {
              message: err instanceof Error ? err.message : "Send failed",
            },
          });
        } finally {
          if (restAbortRef.current === abort) restAbortRef.current = null;
          synthesize({ type: "turn.end" });
        }
      })();
    },
    [markDelivery, sessionId, syncFromTranscript],
  );

  const handleSubmit = useCallback(
    ({ content }: { content: string }) => {
      const text = content.trim();
      if (!text || running) return;
      const entry: HermesOutboxEntry = {
        id: createHermesDeliveryId(),
        sessionId,
        content: text,
        createdAt: Date.now(),
        userOrdinal: transcriptRef.current.items.filter(
          (item) => item.kind === "user",
        ).length,
      };
      putHermesOutbox(entry);
      setInput("");
      setError(null);
      pushUserItem(entry);
      if (transport === "ws" && deliverRef.current) {
        deliverRef.current(entry).catch((err: unknown) => {
          setError(
            err instanceof Error ? err.message : "Message not delivered",
          );
        });
      } else {
        sendViaRest(entry);
      }
    },
    [pushUserItem, running, sendViaRest, sessionId, transport],
  );

  const retryUserMessage = useCallback(
    (message: { id: string; content: string }) => {
      if (running) return;
      const existing = getHermesOutbox(sessionId).find(
        (entry) => entry.id === message.id,
      );
      const entry: HermesOutboxEntry = existing ?? {
        id: message.id,
        sessionId,
        content: message.content,
        createdAt: Date.now(),
        userOrdinal: Math.max(
          0,
          transcriptRef.current.items.filter((item) => item.kind === "user")
            .length - 1,
        ),
      };
      putHermesOutbox(entry);
      transcriptRef.current.running = true;
      markDelivery(entry, "sending");
      setError(null);
      if (transport === "ws" && deliverRef.current) {
        deliverRef.current(entry).catch((err: unknown) => {
          setError(
            err instanceof Error ? err.message : "Message not delivered",
          );
        });
      } else {
        sendViaRest(entry);
      }
    },
    [markDelivery, running, sendViaRest, sessionId, transport],
  );

  const handleStop = useCallback(() => {
    if (transport === "ws" && clientRef.current) {
      clientRef.current
        .request("session.interrupt", { session_id: handleRef.current })
        .catch(() => {
          // Interrupt is best-effort; the turn may already be over.
        });
    } else {
      restAbortRef.current?.abort();
      restAbortRef.current = null;
      const transcript = transcriptRef.current;
      transcript.apply({ type: "turn.end" });
      syncFromTranscript();
    }
  }, [syncFromTranscript, transport]);

  const toggleToolGroup = useCallback((groupId: string) => {
    setExpandedToolGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) next.delete(groupId);
      else next.add(groupId);
      return next;
    });
  }, []);

  const views = useMemo(
    () => deriveItemViews(items, false, running),
    [items, running],
  );
  const rows = views.groupedItems;

  const title =
    session?.title?.trim() ||
    signatureKey ||
    session?.preview?.trim() ||
    `Session ${sessionId.slice(0, 8)}`;

  const noopToolResult = useCallback(
    async () => ({ ok: false, delivered: false }),
    [],
  );
  const noop = useCallback(() => {}, []);

  return (
    <div className="flex h-full min-h-0">
    <div className="flex min-w-0 flex-1 flex-col">
      {/* Header */}
      <div className="flex items-center gap-2 border-b border-white/5 px-4 py-2.5">
        <Sparkles className="h-4 w-4 shrink-0 text-indigo-400" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-medium text-white">{title}</div>
          <div className="text-[11px] text-white/40">
            Hermes session
            {transport === "rest" && " · live events unavailable (polling)"}
            {transport === "connecting" && " · connecting…"}
          </div>
        </div>
        {running && (
          <span className="flex items-center gap-1.5 text-[11px] text-white/50">
            <Loader className="h-3 w-3 animate-spin" />
            working
          </span>
        )}
      </div>

      {/* Worker missions spawned by this session */}
      {workerMissions.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 border-b border-white/5 px-4 py-1.5">
          <span className="text-[11px] text-white/40">Workers</span>
          {workerMissions.map((mission) => (
            <Link
              key={mission.id}
              href={`/control?mission=${mission.id}`}
              className="flex items-center gap-1.5 rounded-full border border-white/10 px-2 py-0.5 text-[11px] text-white/70 transition-colors hover:border-white/25 hover:text-white"
            >
              <span
                className={cn(
                  "h-1.5 w-1.5 rounded-full",
                  STATUS_DOT_COLORS[mission.status] ?? "bg-gray-400",
                )}
              />
              <span className="max-w-48 truncate">
                {getMissionTitle(mission)}
              </span>
            </Link>
          ))}
        </div>
      )}

      {/* Transcript */}
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        {!historyLoaded && transport === "connecting" && (
          <div className="flex items-center justify-center gap-2 py-12 text-sm text-white/40">
            <Loader className="h-4 w-4 animate-spin" />
            Connecting to Hermes…
          </div>
        )}
        {transport === "offline" && (
          <div className="mx-auto max-w-md py-12 text-center text-sm text-white/40">
            <p className="text-white/70">Hermes is unreachable.</p>
            {error && <p className="mt-2 text-xs">{error}</p>}
          </div>
        )}
        {historyLoaded && rows.length === 0 && transport !== "offline" && (
          <div className="flex flex-col items-center gap-2 py-12 text-sm text-white/40">
            <Plus className="h-4 w-4" />
            Send the first message to start this session.
          </div>
        )}
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-1">
          {rows.map((item, index) => (
            <ChatItemRow
              key={getGroupedItemKey(item)}
              item={item}
              highlighted={false}
              workspaceId={undefined}
              missionId={undefined}
              basePath={undefined}
              isLast={index === rows.length - 1}
              isToolGroupExpanded={expandedToolGroups.has(
                getGroupedItemKey(item),
              )}
              onToggleToolGroup={toggleToolGroup}
              onResume={noop}
              onToolResult={noopToolResult}
              onOptimisticToolResult={noop}
              onRetryUserMessage={retryUserMessage}
            />
          ))}
        </div>
      </div>

      {/* Composer */}
      <div className="border-t border-white/5 px-4 py-3">
        <div className="mx-auto w-full max-w-3xl">
          {running && (
            <div className="mb-2 flex justify-end">
              <button
                type="button"
                onClick={handleStop}
                className="flex items-center gap-1.5 rounded-md border border-white/10 px-2.5 py-1 text-xs text-white/70 transition-colors hover:border-white/25 hover:text-white"
              >
                <Square className="h-3 w-3" />
                Stop
              </button>
            </div>
          )}
          <EnhancedInput
            value={input}
            onChange={setInput}
            onSubmit={handleSubmit}
            placeholder="Message Hermes…"
            disabled={transport === "offline" || transport === "connecting"}
          />
        </div>
      </div>
    </div>
    <SessionProjectRail sessionId={sessionId} />
    </div>
  );
}
