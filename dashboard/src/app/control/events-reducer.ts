import type { Mission, SharedFile, StoredEvent } from "@/lib/api";
import { isStreamContinuation } from "@/lib/stream-continuation";

export type CostSource = "actual" | "estimated" | "unknown";

export type ChatItem =
  | {
      kind: "user";
      id: string;
      content: string;
      timestamp: number;
      queued?: boolean;
    }
  | {
      kind: "assistant";
      id: string;
      content: string;
      success: boolean;
      costCents: number;
      costSource: CostSource;
      model: string | null;
      timestamp: number;
      sharedFiles?: SharedFile[];
      resumable?: boolean;
      goalIteration?: number;
    }
  | {
      kind: "thinking";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      kind: "stream";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      kind: "tool";
      id: string;
      toolCallId: string;
      name: string;
      args: unknown;
      result?: unknown;
      isUiTool: boolean;
      startTime: number;
      endTime?: number;
    }
  | {
      kind: "system";
      id: string;
      content: string;
      timestamp: number;
      resumable?: boolean;
      missionId?: string;
    }
  | {
      kind: "phase";
      id: string;
      phase: string;
      detail: string | null;
      agent: string | null;
    };

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function parseCostSource(raw: unknown): CostSource {
  if (raw === "actual" || raw === "estimated" || raw === "unknown") {
    return raw;
  }
  return "unknown";
}

export function parseCostAmount(raw: unknown): number | undefined {
  if (typeof raw === "number" && Number.isFinite(raw)) {
    return raw;
  }
  if (typeof raw === "string") {
    const parsed = Number(raw);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }
  return undefined;
}

export function parseCostMetadata(
  meta: Record<string, unknown>,
  fallback?: { costCents: number; costSource: CostSource },
): { costCents: number; costSource: CostSource } {
  const cost = meta["cost"];
  if (isRecord(cost)) {
    const parsedAmount = parseCostAmount(cost["amount_cents"]);
    const hasSource = cost["source"] !== undefined;
    return {
      costCents: parsedAmount ?? fallback?.costCents ?? 0,
      costSource: hasSource
        ? parseCostSource(cost["source"])
        : (fallback?.costSource ?? "unknown"),
    };
  }

  const parsedAmount = parseCostAmount(meta["cost_cents"]);
  const hasSource = meta["cost_source"] !== undefined;
  if (parsedAmount !== undefined || hasSource) {
    return {
      costCents: parsedAmount ?? fallback?.costCents ?? 0,
      costSource: hasSource
        ? parseCostSource(meta["cost_source"])
        : (fallback?.costSource ?? "unknown"),
    };
  }

  return {
    costCents: fallback?.costCents ?? 0,
    costSource: fallback?.costSource ?? "unknown",
  };
}

export function eventsToItemsImpl(
  events: StoredEvent[],
  mission?: Mission | null,
): ChatItem[] {
  const items: ChatItem[] = [];
  const toolCallMap = new Map<string, number>();
  const seenEventIds = new Set<string>();
  let currentThinkingIdx: number | null = null;
  let lastTextDelta: {
    id: string;
    content: string;
    timestamp: number;
  } | null = null;
  let lastAssistantTimestamp = 0;
  const missionActive = mission?.status === "active";

  const finalizePendingThinking = (endTime: number) => {
    if (currentThinkingIdx !== null) {
      const pending = items[currentThinkingIdx] as Extract<
        ChatItem,
        { kind: "thinking" }
      >;
      if (!pending.done) {
        items[currentThinkingIdx] = {
          ...pending,
          done: true,
          endTime,
        };
      }
      currentThinkingIdx = null;
    }
  };

  for (const event of events) {
    const timestamp = new Date(event.timestamp).getTime();

    switch (event.event_type) {
      case "user_message": {
        finalizePendingThinking(timestamp);
        const itemId = event.event_id ?? `event-${event.id}`;
        if (seenEventIds.has(itemId)) break;
        seenEventIds.add(itemId);
        items.push({
          kind: "user",
          id: itemId,
          content: event.content,
          timestamp,
        });
        break;
      }

      case "assistant_message":
      case "assistant_message_canonical": {
        finalizePendingThinking(timestamp);
        const meta = event.metadata || {};
        const isFailure = meta.success === false;
        const { costCents, costSource } = parseCostMetadata(meta);

        if (isFailure) {
          const errorMessage = event.content || "Mission failed";
          for (let i = 0; i < items.length; i++) {
            const it = items[i];
            if (it.kind === "tool" && it.result === undefined) {
              items[i] = {
                ...it,
                result: { error: errorMessage, status: "failed" },
                endTime: timestamp,
              };
            }
          }
        }

        const assistantId = event.event_id ?? `event-${event.id}`;
        if (seenEventIds.has(assistantId)) break;
        seenEventIds.add(assistantId);
        items.push({
          kind: "assistant",
          id: assistantId,
          content: event.content,
          success: !isFailure,
          costCents,
          costSource,
          model: typeof meta.model === "string" ? meta.model : null,
          timestamp,
        });
        lastAssistantTimestamp = timestamp;
        break;
      }

      case "text_delta": {
        const content = event.content || "";
        if (content.trim().length === 0) break;
        lastTextDelta = {
          id: event.event_id ?? `text-delta-${event.id}`,
          content,
          timestamp,
        };
        break;
      }

      case "text_op": {
        const bubbleId =
          typeof event.metadata?.bubble_id === "string"
            ? event.metadata.bubble_id
            : (event.event_id ?? `text-op-${event.id}`);
        let stream = items.find(
          (item): item is Extract<ChatItem, { kind: "stream" }> =>
            item.kind === "stream" && item.id === bubbleId,
        );
        if (!stream) {
          stream = {
            kind: "stream",
            id: bubbleId,
            content: "",
            done: false,
            startTime: timestamp,
          };
          items.push(stream);
        }
        let content = stream.content;
        let finalized = false;
        let ops: unknown = [];
        try {
          ops = JSON.parse(event.content || "[]") as unknown;
        } catch {
          ops = [];
        }
        if (Array.isArray(ops)) {
          for (const op of ops) {
            if (!isRecord(op)) continue;
            if (op.type === "insert") {
              const pos =
                typeof op.pos === "number"
                  ? Math.max(0, Math.min(op.pos, content.length))
                  : content.length;
              content =
                content.slice(0, pos) + String(op.text ?? "") + content.slice(pos);
            } else if (op.type === "replace") {
              const range = Array.isArray(op.range) ? op.range : [];
              const start =
                typeof range[0] === "number"
                  ? Math.max(0, Math.min(range[0], content.length))
                  : 0;
              const end =
                typeof range[1] === "number"
                  ? Math.max(start, Math.min(range[1], content.length))
                  : content.length;
              content =
                content.slice(0, start) + String(op.text ?? "") + content.slice(end);
            } else if (op.type === "finalize") {
              finalized = true;
            }
          }
        }
        stream.content = content;
        stream.done = finalized;
        stream.endTime = finalized ? timestamp : undefined;
        break;
      }

      case "thinking": {
        const meta = event.metadata || {};
        const isDone = meta.done === true;
        const content = event.content || "";

        if (currentThinkingIdx !== null) {
          const existing = items[currentThinkingIdx] as Extract<
            ChatItem,
            { kind: "thinking" }
          >;
          const existingContent = existing.content || "";
          const isContinuation = isStreamContinuation(content, existingContent);

          if (!isContinuation) {
            items[currentThinkingIdx] = {
              ...existing,
              done: true,
              endTime: timestamp,
            };
            const newIdx = items.length;
            items.push({
              kind: "thinking",
              id: `event-${event.id}`,
              content,
              done: isDone,
              startTime: timestamp,
              endTime: isDone ? timestamp : undefined,
            });
            currentThinkingIdx = isDone ? null : newIdx;
          } else {
            const newContent =
              content.length > existingContent.length
                ? content
                : existingContent;
            items[currentThinkingIdx] = {
              ...existing,
              content: newContent,
              done: isDone,
              endTime: isDone ? timestamp : existing.endTime,
            };
            if (isDone) {
              currentThinkingIdx = null;
            }
          }
        } else {
          const newIdx = items.length;
          items.push({
            kind: "thinking",
            id: `event-${event.id}`,
            content,
            done: isDone,
            startTime: timestamp,
            endTime: isDone ? timestamp : undefined,
          });
          if (!isDone) {
            currentThinkingIdx = newIdx;
          }
        }
        break;
      }

      case "tool_call": {
        finalizePendingThinking(timestamp);
        lastTextDelta = null;
        const toolCallId = event.tool_call_id || `unknown-${event.id}`;
        const name = event.tool_name || "unknown";
        const isUiTool =
          name.startsWith("ui_") ||
          name === "question" ||
          name === "AskUserQuestion";
        let args: unknown = undefined;
        try {
          args = event.content ? JSON.parse(event.content) : undefined;
        } catch {
          args = event.content;
        }
        const toolItem: ChatItem = {
          kind: "tool",
          id: `tool-${toolCallId}`,
          toolCallId,
          name,
          args,
          isUiTool,
          startTime: timestamp,
          result: undefined,
          endTime: undefined,
        };
        toolCallMap.set(toolCallId, items.length);
        items.push(toolItem);
        break;
      }

      case "tool_result": {
        const toolCallId = event.tool_call_id || "";
        const idx = toolCallMap.get(toolCallId);
        if (idx !== undefined) {
          const toolItem = items[idx] as Extract<ChatItem, { kind: "tool" }>;
          let result: unknown = event.content;
          try {
            result = event.content ? JSON.parse(event.content) : event.content;
          } catch {
            // Keep as string if not valid JSON.
          }
          items[idx] = {
            ...toolItem,
            result,
            endTime: timestamp,
          };
        }
        break;
      }
    }
  }

  if (!missionActive) {
    finalizePendingThinking(Date.now());
  }

  if (lastTextDelta && lastTextDelta.timestamp > lastAssistantTimestamp) {
    const staleThresholdMs = 5 * 60 * 1000;
    const isStale =
      missionActive && Date.now() - lastTextDelta.timestamp > staleThresholdMs;
    const isDone = !missionActive || isStale;
    items.push({
      kind: "stream",
      id: lastTextDelta.id,
      content: lastTextDelta.content,
      done: isDone,
      startTime: lastTextDelta.timestamp,
      endTime: isDone ? lastTextDelta.timestamp : undefined,
    });
  }

  return items;
}
