import type { Mission, SharedFile, StoredEvent } from "@/lib/api";
import {
  isStreamContinuation,
  mergeStreamFragment,
} from "@/lib/stream-continuation";

export type CostSource = "actual" | "estimated" | "unknown";

export type ChatItem =
  | {
      kind: "user";
      id: string;
      content: string;
      timestamp: number;
      queued?: boolean;
      /**
       * Optimistic-send lifecycle (client-only; never set from server
       * events). `sending` = POST in flight, `sent` = backend acked,
       * `failed` = never reached the backend. Drives the bubble's opacity
       * and the inline "Sending…" / "Retry" affordances.
       */
      sendStatus?: "sending" | "sent" | "failed";
      /** When `sendStatus === "failed"`: drives copy + auto-retry eligibility. */
      failedReason?: "network" | "rejected";
      /** Locked agent for this message, preserved so a retry re-applies it. */
      agent?: string;
      /**
       * Mission this row was created for (client-only). Stamped on
       * live/optimistic rows so a row created while viewing mission A can
       * never be reconciled into, or rendered under, mission B during a
       * switch race. Unset for server-history rows (already loaded per
       * mission, so correct by construction).
       */
      missionId?: string;
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
      /**
       * Raw terminal_reason from the backend's completion_evidence, when
       * present. Lets the UI distinguish a real agent failure from a
       * deploy-induced SIGTERM (terminal_reason === "ServerShutdown"),
       * which the mission auto-resumes from. Without this distinction
       * every restart looks like the agent crashed.
       */
      terminalReason?: string;
    }
  | {
      kind: "thinking";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
      missionId?: string;
    }
  | {
      kind: "stream";
      id: string;
      content: string;
      done: boolean;
      startTime: number;
      endTime?: number;
      missionId?: string;
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
      lazy?: boolean;
      loading?: boolean;
      hasResult?: boolean;
      contentBytes?: number;
      resultBytes?: number;
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

function normalizeStreamComparisonText(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

function streamDuplicatesAssistant(stream: string, assistant: string): boolean {
  const streamText = normalizeStreamComparisonText(stream);
  const assistantText = normalizeStreamComparisonText(assistant);
  if (!streamText || !assistantText) return false;
  if (streamText === assistantText) return true;
  const minOverlapLen = 80;
  return (
    (streamText.length >= minOverlapLen &&
      assistantText.startsWith(streamText)) ||
    (assistantText.length >= minOverlapLen &&
      streamText.startsWith(assistantText))
  );
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
  let lastAssistantContent = "";
  const missionActive = mission?.status === "active";
  const isGoalMode = mission?.goal_mode === true;

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

  // Materialize the pending text-delta run as a finished stream bubble.
  // Without this, narration the agent emits between tool calls ("Now
  // regenerating artifacts...") was silently dropped — only the trailing
  // run after the last tool call ever rendered.
  // Stream items flushed since the last assistant message. A flushed run can
  // turn out to duplicate the *next* assistant message of the same turn (the
  // final text often repeats earlier narration); those are removed
  // retroactively when that assistant message arrives.
  let flushedStreamIds: string[] = [];

  const flushTextDelta = (endTime: number) => {
    if (!lastTextDelta) return;
    if (!streamDuplicatesAssistant(lastTextDelta.content, lastAssistantContent)) {
      items.push({
        kind: "stream",
        id: lastTextDelta.id,
        content: lastTextDelta.content,
        done: true,
        startTime: lastTextDelta.timestamp,
        endTime,
      });
      flushedStreamIds.push(lastTextDelta.id);
    }
    lastTextDelta = null;
  };

  const dropStreamsDuplicatedBy = (assistantContent: string) => {
    if (flushedStreamIds.length > 0) {
      const ids = new Set(flushedStreamIds);
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (
          it.kind === "stream" &&
          ids.has(it.id) &&
          streamDuplicatesAssistant(it.content, assistantContent)
        ) {
          items.splice(i, 1);
        }
      }
    }
    flushedStreamIds = [];
  };

  const pushGoalDeliverable = (event: StoredEvent, timestamp: number) => {
    const content = event.content.trim();
    if (!content) return;
    const alreadyHasAssistant = items.some(
      (item) => item.kind === "assistant" && item.content.trim() === content,
    );
    if (alreadyHasAssistant) return;
    const itemId = event.event_id
      ? `goal-deliverable-${event.event_id}`
      : `goal-deliverable-${event.id}`;
    if (seenEventIds.has(itemId)) return;
    seenEventIds.add(itemId);
    items.push({
      kind: "assistant",
      id: itemId,
      content,
      success: true,
      costCents: 0,
      costSource: "unknown",
      model: null,
      timestamp,
    });
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
        dropStreamsDuplicatedBy(event.content);
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
        const ce =
          (meta.completion_evidence as { terminal_reason?: unknown } | undefined) ?? undefined;
        const terminalReason =
          typeof ce?.terminal_reason === "string" ? ce.terminal_reason : undefined;
        const resumable =
          typeof (meta as { resumable?: unknown }).resumable === "boolean"
            ? (meta as { resumable: boolean }).resumable
            : undefined;
        items.push({
          kind: "assistant",
          id: assistantId,
          content: event.content,
          success: !isFailure,
          costCents,
          costSource,
          model: typeof meta.model === "string" ? meta.model : null,
          timestamp,
          terminalReason,
          resumable,
        });
        lastAssistantTimestamp = timestamp;
        lastAssistantContent = event.content;
        break;
      }

      case "text_delta": {
        const content = event.content || "";
        if (content.trim().length === 0) break;
        const mergedContent: string = lastTextDelta
          ? mergeStreamFragment(lastTextDelta.content, content)
          : content;
        lastTextDelta = {
          id: event.event_id ?? `text-delta-${event.id}`,
          content: mergedContent,
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
                content.slice(0, pos) +
                String(op.text ?? "") +
                content.slice(pos);
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
                content.slice(0, start) +
                String(op.text ?? "") +
                content.slice(end);
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
        const isGoalDeliverable =
          isGoalMode && isDone && meta.goal_role === "deliverable";
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
            if (isGoalDeliverable) {
              pushGoalDeliverable(event, timestamp);
            }
          }
        } else {
          // Legacy finalizers were persisted with empty content; with no
          // open block they would render as empty thought bubbles. Drop them.
          if (isDone && content.trim() === "") {
            break;
          }
          const newIdx = items.length;
          if (!isGoalDeliverable) {
            items.push({
              kind: "thinking",
              id: `event-${event.id}`,
              content,
              done: isDone,
              startTime: timestamp,
              endTime: isDone ? timestamp : undefined,
            });
          }
          if (!isDone) {
            currentThinkingIdx = newIdx;
          } else if (isGoalDeliverable) {
            pushGoalDeliverable(event, timestamp);
          }
        }
        break;
      }

      case "tool_call": {
        finalizePendingThinking(timestamp);
        flushTextDelta(timestamp);
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

      case "tool_stub": {
        finalizePendingThinking(timestamp);
        flushTextDelta(timestamp);
        const toolCallId = event.tool_call_id || `unknown-${event.id}`;
        const name = event.tool_name || "unknown";
        const isUiTool =
          name.startsWith("ui_") ||
          name === "question" ||
          name === "AskUserQuestion";
        const resultTimestamp =
          typeof event.metadata?.result_timestamp === "string"
            ? new Date(event.metadata.result_timestamp).getTime()
            : undefined;
        const toolItem: ChatItem = {
          kind: "tool",
          id: `tool-${toolCallId}`,
          toolCallId,
          name,
          args: undefined,
          isUiTool,
          startTime: timestamp,
          result: undefined,
          endTime:
            typeof resultTimestamp === "number" &&
            Number.isFinite(resultTimestamp)
              ? resultTimestamp
              : undefined,
          lazy: true,
          hasResult: event.metadata?.has_result === true,
          contentBytes:
            typeof event.metadata?.call_content_bytes === "number"
              ? event.metadata.call_content_bytes
              : undefined,
          resultBytes:
            typeof event.metadata?.result_content_bytes === "number"
              ? event.metadata.result_content_bytes
              : undefined,
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
            lazy: false,
            loading: false,
            hasResult: true,
          };
        }
        break;
      }
    }
  }

  if (!missionActive) {
    finalizePendingThinking(Date.now());
  }

  if (
    lastTextDelta &&
    !streamDuplicatesAssistant(lastTextDelta.content, lastAssistantContent)
  ) {
    const staleThresholdMs = 5 * 60 * 1000;
    const isStale =
      missionActive && Date.now() - lastTextDelta.timestamp > staleThresholdMs;
    const assistantArrivedAfterStream =
      lastAssistantTimestamp >= lastTextDelta.timestamp;
    const isDone = assistantArrivedAfterStream || !missionActive || isStale;
    items.push({
      kind: "stream",
      id: lastTextDelta.id,
      content: lastTextDelta.content,
      done: isDone,
      startTime: lastTextDelta.timestamp,
      endTime: isDone
        ? Math.max(lastTextDelta.timestamp, lastAssistantTimestamp)
        : undefined,
    });
  }

  return items;
}

// ── Mission state extraction (Workbench panel) ──────────────────────

export type PlanItem = {
  content: string;
  status: "pending" | "in_progress" | "completed";
};

export type MissionStateSummary = {
  /** Latest task-board snapshot from the harness's native plan tool. */
  plan: { items: PlanItem[]; timestamp: number } | null;
  /** Latest self-scheduled wakeup: what the agent said it will do next. */
  upNext: { reason: string; delaySeconds: number | null; timestamp: number } | null;
};

function normalizePlanStatus(raw: unknown): PlanItem["status"] {
  if (raw === "completed" || raw === "in_progress" || raw === "pending") return raw;
  if (raw === "done" || raw === "complete") return "completed";
  return "pending";
}

/**
 * Derive the agent's task board and "up next" marker from already-rendered
 * chat items. Reads the harness's own plan tools (Claude Code `TodoWrite`,
 * Codex `update_plan`, OpenCode `todowrite`) and the last `ScheduleWakeup`
 * call — no new agent behavior or events required, so it stays truthful.
 */
export function extractMissionState(items: ChatItem[]): MissionStateSummary {
  let plan: MissionStateSummary["plan"] = null;
  let upNext: MissionStateSummary["upNext"] = null;
  // Lazily-loaded history stubs carry no args; if a stub for one of these
  // tools is NEWER than the last parsed snapshot, the parsed one is stale —
  // drop it rather than present outdated state as current.
  let stalePlanAfter = 0;
  let staleUpNextAfter = 0;
  for (const item of items) {
    if (item.kind !== "tool") continue;
    const name = item.name;
    if (!isRecord(item.args)) {
      if (name === "TodoWrite" || name === "todowrite" || name === "update_plan") {
        stalePlanAfter = Math.max(stalePlanAfter, item.startTime);
      } else if (name === "ScheduleWakeup") {
        staleUpNextAfter = Math.max(staleUpNextAfter, item.startTime);
      }
      continue;
    }
    if (name === "TodoWrite" || name === "todowrite" || name === "update_plan") {
      const raw = (item.args as Record<string, unknown>)["todos"] ??
        (item.args as Record<string, unknown>)["plan"];
      if (Array.isArray(raw)) {
        const parsed: PlanItem[] = [];
        for (const entry of raw) {
          if (!isRecord(entry)) continue;
          const content =
            typeof entry["content"] === "string"
              ? entry["content"]
              : typeof entry["step"] === "string"
                ? entry["step"]
                : null;
          if (!content) continue;
          parsed.push({ content, status: normalizePlanStatus(entry["status"]) });
        }
        // An empty/unparseable board still supersedes the previous one —
        // keeping the old snapshot would present stale state as current.
        plan = parsed.length > 0 ? { items: parsed, timestamp: item.startTime } : null;
      }
    } else if (name === "ScheduleWakeup") {
      const args = item.args as Record<string, unknown>;
      const reason =
        typeof args["reason"] === "string" && args["reason"].trim()
          ? args["reason"]
          : typeof args["prompt"] === "string"
            ? args["prompt"]
            : "";
      // A wakeup without usable text still replaces (clears) the previous
      // marker — the old "up next" is no longer what happens next.
      if (!reason) {
        upNext = null;
        continue;
      }
      {
        upNext = {
          reason,
          delaySeconds:
            typeof args["delaySeconds"] === "number"
              ? args["delaySeconds"]
              : typeof args["delay_seconds"] === "number"
                ? args["delay_seconds"]
                : null,
          timestamp: item.startTime,
        };
      }
    }
  }
  if (plan && stalePlanAfter > plan.timestamp) plan = null;
  if (upNext && staleUpNextAfter > upNext.timestamp) upNext = null;
  return { plan, upNext };
}
