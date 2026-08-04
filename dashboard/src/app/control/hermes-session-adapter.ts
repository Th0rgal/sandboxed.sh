import type { HermesMessage } from "@/lib/api";
import type { HermesGatewayEvent } from "@/lib/hermes-gateway";
import type { ChatItem } from "./events-reducer";

/**
 * Adapter from the Hermes session protocol to the control-page `ChatItem`
 * union, so Hermes conversations render through the exact same transcript
 * pipeline as missions (deriveItemViews + ChatItemRow).
 *
 * Two sources feed it:
 *  - persisted history (`HermesMessage[]` from session.resume / REST)
 *  - live gateway events (message.delta / thinking.delta / tool.* / …)
 */

let nextLocalId = 0;
function localId(prefix: string): string {
  nextLocalId += 1;
  return `${prefix}-${nextLocalId}`;
}

function parseTimestamp(raw: unknown): number {
  if (typeof raw === "number" && Number.isFinite(raw)) {
    // Hermes timestamps are seconds; anything past ~2001 in ms is already ms.
    return raw > 1e12 ? raw : raw * 1000;
  }
  if (typeof raw === "string") {
    const parsed = Date.parse(raw);
    if (Number.isFinite(parsed)) return parsed;
    const numeric = Number(raw);
    if (Number.isFinite(numeric)) return numeric > 1e12 ? numeric : numeric * 1000;
  }
  return Date.now();
}

function parseToolCallArguments(raw: unknown): unknown {
  if (typeof raw !== "string") return raw;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

interface RawToolCall {
  id?: unknown;
  function?: { name?: unknown; arguments?: unknown };
  name?: unknown;
  arguments?: unknown;
}

/** Map persisted Hermes messages onto ChatItems. Tool results (role "tool")
 * are folded into the tool item created by the assistant's tool_calls. */
export function hermesHistoryToItems(messages: HermesMessage[]): ChatItem[] {
  const items: ChatItem[] = [];
  const toolItemIndexByCallId = new Map<string, number>();

  for (const message of messages) {
    const id =
      message.id == null ? localId("hh") : `hermes-msg-${String(message.id)}`;
    const timestamp = parseTimestamp(message.timestamp);
    const content = (message.content ?? "").trim();

    if (message.role === "user") {
      if (content) {
        items.push({ kind: "user", id, content, timestamp });
      }
      continue;
    }

    if (message.role === "assistant") {
      const reasoning = (
        message.reasoning_content ??
        message.reasoning ??
        ""
      ).trim();
      if (reasoning) {
        items.push({
          kind: "thinking",
          id: `${id}-thinking`,
          content: reasoning,
          done: true,
          startTime: timestamp,
          endTime: timestamp,
        });
      }
      if (content) {
        items.push({
          kind: "assistant",
          id,
          content,
          success: true,
          costCents: 0,
          costSource: "unknown",
          model: null,
          timestamp,
        });
      }
      const toolCalls = Array.isArray(message.tool_calls)
        ? (message.tool_calls as RawToolCall[])
        : [];
      for (const call of toolCalls) {
        const callId =
          typeof call.id === "string" && call.id
            ? call.id
            : localId("hh-call");
        const name =
          (typeof call.function?.name === "string" && call.function.name) ||
          (typeof call.name === "string" && call.name) ||
          "tool";
        toolItemIndexByCallId.set(callId, items.length);
        items.push({
          kind: "tool",
          id: `hermes-tool-${callId}`,
          toolCallId: callId,
          name,
          args: parseToolCallArguments(
            call.function?.arguments ?? call.arguments,
          ),
          isUiTool: false,
          startTime: timestamp,
          endTime: timestamp,
          hasResult: false,
        });
      }
      continue;
    }

    if (message.role === "tool") {
      const callId = message.tool_call_id ?? undefined;
      const index =
        callId != null ? toolItemIndexByCallId.get(callId) : undefined;
      if (index !== undefined) {
        const existing = items[index];
        if (existing.kind === "tool") {
          items[index] = {
            ...existing,
            result: content,
            hasResult: true,
            endTime: timestamp,
          };
        }
      } else if (content) {
        items.push({
          kind: "tool",
          id,
          toolCallId: callId ?? id,
          name: message.tool_name ?? "tool",
          args: undefined,
          result: content,
          isUiTool: false,
          startTime: timestamp,
          endTime: timestamp,
          hasResult: true,
        });
      }
    }
  }
  // Historical tool calls whose result row was never persisted would render
  // as "running for N days" (the row treats result === undefined as live).
  // They are history — close them with an empty result.
  return items.map((item) =>
    item.kind === "tool" && item.result === undefined
      ? { ...item, result: "", hasResult: true }
      : item,
  );
}

function payloadString(payload: unknown, key: string): string | undefined {
  if (typeof payload !== "object" || payload === null) return undefined;
  const value = (payload as Record<string, unknown>)[key];
  return typeof value === "string" ? value : undefined;
}

function payloadValue(payload: unknown, key: string): unknown {
  if (typeof payload !== "object" || payload === null) return undefined;
  return (payload as Record<string, unknown>)[key];
}

/**
 * Streaming state machine for one live Hermes session. `apply` folds a
 * gateway event into the item list and returns true when the list changed.
 * The caller owns re-rendering (typically by cloning `items` into state).
 */
export class HermesLiveTranscript {
  items: ChatItem[] = [];
  running = false;

  private streamItemId: string | null = null;
  private thinkingItemId: string | null = null;
  private streamText = "";
  private toolItemIdByToolId = new Map<string, string>();

  reset(items: ChatItem[]) {
    this.items = items;
    this.streamItemId = null;
    this.thinkingItemId = null;
    this.streamText = "";
    this.toolItemIdByToolId.clear();
  }

  private updateItem(id: string, update: (item: ChatItem) => ChatItem) {
    const index = this.items.findIndex((item) => item.id === id);
    if (index >= 0) {
      this.items = [
        ...this.items.slice(0, index),
        update(this.items[index]),
        ...this.items.slice(index + 1),
      ];
    }
  }

  private appendStreamText(text: string) {
    if (!text) return;
    this.streamText += text;
    if (this.streamItemId == null) {
      this.streamItemId = localId("hs-stream");
      this.items = [
        ...this.items,
        {
          kind: "stream",
          id: this.streamItemId,
          content: this.streamText,
          done: false,
          startTime: Date.now(),
        },
      ];
    } else {
      const id = this.streamItemId;
      const content = this.streamText;
      this.updateItem(id, (item) =>
        item.kind === "stream" ? { ...item, content } : item,
      );
    }
  }

  private appendThinkingText(text: string) {
    if (!text) return;
    if (this.thinkingItemId == null) {
      this.thinkingItemId = localId("hs-think");
      this.items = [
        ...this.items,
        {
          kind: "thinking",
          id: this.thinkingItemId,
          content: text,
          done: false,
          startTime: Date.now(),
        },
      ];
    } else {
      const id = this.thinkingItemId;
      this.updateItem(id, (item) =>
        item.kind === "thinking"
          ? { ...item, content: item.content + text }
          : item,
      );
    }
  }

  private finalizeThinking() {
    if (this.thinkingItemId == null) return;
    const id = this.thinkingItemId;
    this.thinkingItemId = null;
    this.updateItem(id, (item) =>
      item.kind === "thinking"
        ? { ...item, done: true, endTime: Date.now() }
        : item,
    );
  }

  private finalizeAssistant(finalText?: string) {
    this.finalizeThinking();
    const text = (finalText ?? this.streamText).trim();
    const streamId = this.streamItemId;
    this.streamItemId = null;
    this.streamText = "";
    if (streamId != null) {
      this.items = this.items.filter((item) => item.id !== streamId);
    }
    if (text) {
      this.items = [
        ...this.items,
        {
          kind: "assistant",
          id: localId("hs-assistant"),
          content: text,
          success: true,
          costCents: 0,
          costSource: "unknown",
          model: null,
          timestamp: Date.now(),
        },
      ];
    }
  }

  private startTool(toolId: string, name: string, args: unknown) {
    const itemId = `hs-tool-${toolId}`;
    this.toolItemIdByToolId.set(toolId, itemId);
    this.items = [
      ...this.items,
      {
        kind: "tool",
        id: itemId,
        toolCallId: toolId,
        name,
        args,
        isUiTool: false,
        startTime: Date.now(),
        hasResult: false,
      },
    ];
  }

  private completeTool(toolId: string, result: unknown) {
    const itemId = this.toolItemIdByToolId.get(toolId);
    if (itemId == null) return;
    this.toolItemIdByToolId.delete(toolId);
    this.updateItem(itemId, (item) =>
      item.kind === "tool"
        ? { ...item, result, hasResult: true, endTime: Date.now() }
        : item,
    );
  }

  apply(event: HermesGatewayEvent): boolean {
    const before = this.items;
    const runningBefore = this.running;
    const payload = event.payload;

    switch (event.type) {
      case "turn.start":
      case "turn.started":
        this.running = true;
        break;
      case "message.start":
        this.running = true;
        break;
      case "message.delta":
        this.appendStreamText(payloadString(payload, "text") ?? "");
        break;
      case "message.interim":
        // A full interim assistant message: flush it as its own bubble.
        this.finalizeAssistant(payloadString(payload, "text"));
        break;
      case "message.complete":
        this.finalizeAssistant(payloadString(payload, "text"));
        break;
      case "thinking.delta":
      case "reasoning.delta":
        this.appendThinkingText(payloadString(payload, "text") ?? "");
        break;
      case "reasoning.available": {
        this.appendThinkingText(payloadString(payload, "text") ?? "");
        this.finalizeThinking();
        break;
      }
      case "tool.start": {
        const toolId =
          payloadString(payload, "tool_id") ?? localId("hs-toolid");
        const name = payloadString(payload, "name") ?? "tool";
        this.startTool(
          toolId,
          name,
          payloadValue(payload, "args") ??
            payloadString(payload, "args_text"),
        );
        break;
      }
      case "tool.complete": {
        const toolId = payloadString(payload, "tool_id");
        if (toolId != null) {
          this.completeTool(
            toolId,
            payloadValue(payload, "result") ??
              payloadString(payload, "summary"),
          );
        }
        break;
      }
      case "subagent.start": {
        const subagentId =
          payloadString(payload, "subagent_id") ?? localId("hs-subagent");
        this.startTool(`subagent:${subagentId}`, "subagent", {
          goal: payloadString(payload, "goal"),
          model: payloadString(payload, "model"),
          child_session_id: payloadString(payload, "child_session_id"),
        });
        break;
      }
      case "subagent.complete": {
        const subagentId = payloadString(payload, "subagent_id");
        if (subagentId != null) {
          this.completeTool(
            `subagent:${subagentId}`,
            payloadString(payload, "summary") ??
              payloadValue(payload, "status"),
          );
        }
        break;
      }
      case "approval.request": {
        const command = payloadString(payload, "command");
        this.items = [
          ...this.items,
          {
            kind: "system",
            id: localId("hs-approval"),
            content: command
              ? `Hermes is asking for approval:\n\`\`\`\n${command}\n\`\`\`\nRespond from a connected Hermes client.`
              : "Hermes is asking for an approval. Respond from a connected Hermes client.",
            timestamp: Date.now(),
          },
        ];
        break;
      }
      case "error":
      case "turn.error": {
        const message =
          payloadString(payload, "message") ?? "Hermes reported an error.";
        this.finalizeAssistant();
        this.items = [
          ...this.items,
          {
            kind: "system",
            id: localId("hs-error"),
            content: message,
            timestamp: Date.now(),
          },
        ];
        this.running = false;
        break;
      }
      case "turn.end":
        this.finalizeAssistant();
        this.running = false;
        break;
      default:
        break;
    }

    return this.items !== before || this.running !== runningBefore;
  }
}
