import { For, Show, createSignal } from "solid-js";
import * as Ic from "./icons";
import { MdView } from "./Markdown";
import type { StreamEvent } from "./stream";

export type StreamItem =
  | { kind: "user"; key: string; text: string }
  | { kind: "think"; key: string; text: string; done: boolean }
  | { kind: "text"; key: string; text: string; live: boolean }
  | { kind: "error"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      callId: string;
      name: string;
      args: unknown;
      result?: unknown;
      done: boolean;
    };

let keySeq = 0;
const nextKey = () => `k${++keySeq}`;

/**
 * Build a transcript from a whole event log in one pass. `applyStreamEvent`
 * copies the array per event (fine for a live trickle) and scans for tool
 * call ids, which made a 4000-event replay quadratic. Here the array is
 * mutated in place and tool rows are indexed by call id.
 */
export function buildTranscript(events: StreamEvent[]): StreamItem[] {
  const items: StreamItem[] = [];
  const toolIndex = new Map<string, number>();
  for (const ev of events) {
    if (ev.type === "tool_call") {
      const callId = str(ev.data.tool_call_id);
      if (toolIndex.has(callId)) continue;
      toolIndex.set(callId, items.length);
      items.push({ kind: "tool", key: nextKey(), callId, name: str(ev.data.name) || "tool", args: ev.data.args, done: false });
      continue;
    }
    if (ev.type === "tool_result") {
      const callId = str(ev.data.tool_call_id);
      const idx = toolIndex.get(callId);
      if (idx != null && items[idx]?.kind === "tool") {
        items[idx] = { ...(items[idx] as Extract<StreamItem, { kind: "tool" }>), result: ev.data.result, done: true };
        continue;
      }
      toolIndex.set(callId, items.length);
      items.push({ kind: "tool", key: nextKey(), callId, name: str(ev.data.name) || "tool", args: null, result: ev.data.result, done: true });
      continue;
    }
    // Everything else only touches the tail: reduce a 0/1-element window.
    const last = items[items.length - 1];
    const tail = last ? [last] : [];
    const next = applyStreamEvent(tail, ev);
    if (next === tail) continue;
    if (last) items.pop();
    for (const it of next) items.push(it);
  }
  return items;
}

const str = (v: unknown): string => (typeof v === "string" ? v : v == null ? "" : String(v));

/**
 * Fold one stream event into the transcript. text_delta / thinking contents
 * are cumulative snapshots, so the last one of a run replaces the open block;
 * tool calls are keyed by tool_call_id so results update in place.
 */
export function applyStreamEvent(items: StreamItem[], ev: StreamEvent): StreamItem[] {
  const last = items[items.length - 1];
  switch (ev.type) {
    case "user_message": {
      return [...items, { kind: "user", key: nextKey(), text: str(ev.data.content) }];
    }
    case "text_delta": {
      const text = str(ev.data.content);
      if (!text) return items;
      if (last?.kind === "text" && last.live) {
        return [...items.slice(0, -1), { ...last, text, live: true }];
      }
      return [...items, { kind: "text", key: nextKey(), text, live: true }];
    }
    case "text_op": {
      // Live stream shape: the server rewrites text_delta into CRDT-style
      // ops on a "text_delta_latest" bubble (insert/replace carry the full
      // accumulated text; finalize closes the bubble).
      const ops = Array.isArray(ev.data.ops) ? (ev.data.ops as Array<Record<string, unknown>>) : [];
      let text = last?.kind === "text" && last.live ? last.text : "";
      let finalized = false;
      for (const op of ops) {
        if (!op || typeof op !== "object") continue;
        if (op.type === "insert") {
          const pos = typeof op.pos === "number" ? Math.max(0, Math.min(op.pos, text.length)) : text.length;
          text = text.slice(0, pos) + str(op.text) + text.slice(pos);
        } else if (op.type === "replace") {
          const range = Array.isArray(op.range) ? (op.range as unknown[]) : [];
          const start = typeof range[0] === "number" ? Math.max(0, Math.min(range[0], text.length)) : 0;
          const end = typeof range[1] === "number" ? Math.max(start, Math.min(range[1], text.length)) : text.length;
          text = text.slice(0, start) + str(op.text) + text.slice(end);
        } else if (op.type === "finalize") {
          finalized = true;
        }
      }
      if (last?.kind === "text" && last.live) {
        return [...items.slice(0, -1), { ...last, text: text || last.text, live: !finalized }];
      }
      if (!text) return items;
      return [...items, { kind: "text", key: nextKey(), text, live: !finalized }];
    }
    case "thinking": {
      const done = ev.data.done === true;
      const text = str(ev.data.content);
      if (last?.kind === "think" && !last.done) {
        // Snapshots are cumulative; never let a shorter/late chunk shrink the block.
        return [...items.slice(0, -1), { ...last, text: text.length >= last.text.length ? text : last.text, done }];
      }
      if (!text && done) return items;
      return [...items, { kind: "think", key: nextKey(), text, done }];
    }
    case "tool_call": {
      const callId = str(ev.data.tool_call_id);
      if (items.some((i) => i.kind === "tool" && i.callId === callId)) return items;
      return [
        ...items,
        {
          kind: "tool",
          key: nextKey(),
          callId,
          name: str(ev.data.name) || "tool",
          args: ev.data.args,
          done: false,
        },
      ];
    }
    case "tool_result": {
      const callId = str(ev.data.tool_call_id);
      const idx = items.findIndex((i) => i.kind === "tool" && i.callId === callId);
      if (idx < 0) {
        // Result without a matching call (event window cut) — still show it.
        return [
          ...items,
          {
            kind: "tool",
            key: nextKey(),
            callId,
            name: str(ev.data.name) || "tool",
            args: null,
            result: ev.data.result,
            done: true,
          },
        ];
      }
      const it = items[idx];
      if (it.kind !== "tool") return items;
      return [
        ...items.slice(0, idx),
        { ...it, result: ev.data.result, done: true },
        ...items.slice(idx + 1),
      ];
    }
    case "assistant_message": {
      const text = str(ev.data.content);
      if (ev.data.success === false) {
        // Failed turn: the content is the failure reason, not assistant prose.
        const closed = last?.kind === "text" && last.live ? [...items.slice(0, -1), { ...last, live: false }] : items;
        return [...closed, { kind: "error", key: nextKey(), text: text || "Mission failed" }];
      }
      // Final full text for the turn: close the open live bubble with it.
      if (last?.kind === "text" && last.live) {
        return [...items.slice(0, -1), { ...last, text: text || last.text, live: false }];
      }
      if (!text) return items;
      // A just-finalized text_op bubble, or an assistant_message_canonical
      // row stored next to assistant_message, carries the same turn — fold
      // it into the previous bubble instead of rendering it twice.
      if (last?.kind === "text" && (text.startsWith(last.text) || last.text.startsWith(text))) {
        return text.length > last.text.length ? [...items.slice(0, -1), { ...last, text }] : items;
      }
      return [...items, { kind: "text", key: nextKey(), text, live: false }];
    }
    case "error": {
      return [...items, { kind: "error", key: nextKey(), text: str(ev.data.message) }];
    }
    default:
      return items;
  }
}

/** Short, human-readable target for a tool call row (Cursor-style). */
function toolTarget(name: string, args: unknown): string {
  if (!args || typeof args !== "object") return "";
  const a = args as Record<string, unknown>;
  const pick = (...keys: string[]): string => {
    for (const k of keys) {
      const v = a[k];
      if (typeof v === "string" && v.trim()) return v.trim();
    }
    return "";
  };
  let t = "";
  switch (name.toLowerCase()) {
    case "bash":
      t = pick("command", "cmd");
      break;
    case "read":
    case "write":
    case "edit":
      t = pick("file_path", "path", "file");
      break;
    case "grep":
    case "glob":
      t = pick("pattern", "query");
      break;
    case "task":
      t = pick("description", "prompt");
      break;
    case "webfetch":
    case "web_fetch":
      t = pick("url");
      break;
    default:
      t = pick("file_path", "path", "command", "query", "url", "pattern", "prompt", "description");
  }
  if (t.length > 90) t = `${t.slice(0, 90)}…`;
  return t;
}

function resultText(result: unknown): string {
  if (result == null) return "";
  if (typeof result === "string") return result;
  try {
    return JSON.stringify(result, null, 2);
  } catch {
    return String(result);
  }
}

function ToolRow(p: { item: Extract<StreamItem, { kind: "tool" }> }) {
  const [open, setOpen] = createSignal(false);
  const target = () => toolTarget(p.item.name, p.item.args);
  const detail = () => {
    const parts: string[] = [];
    if (p.item.args != null) parts.push(typeof p.item.args === "string" ? p.item.args : JSON.stringify(p.item.args, null, 2));
    const r = resultText(p.item.result);
    if (r) parts.push(r);
    return parts.join("\n\n");
  };
  return (
    <div class={`st-tool ${open() ? "open" : ""}`}>
      <button class="st-tool-head" onClick={() => setOpen(!open())}>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        <span class="st-tool-name">{p.item.name}</span>
        <Show when={target()}>
          <span class="st-tool-target">{target()}</span>
        </Show>
        <span class="st-tool-state">
          <Show when={p.item.done} fallback={<Ic.Spinner size={12} />}>
            <span class="st-tool-check">✓</span>
          </Show>
        </span>
      </button>
      <Show when={open()}>
        <pre class="st-tool-detail">{detail() || "(no details)"}</pre>
      </Show>
    </div>
  );
}

function ThinkBlock(p: { item: Extract<StreamItem, { kind: "think" }> }) {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="st-think">
      <button class="st-think-head" onClick={() => setOpen(!open())}>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        <Show when={p.item.done} fallback={<span class="shimmer">Thinking…</span>}>
          <span>Thought</span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="st-think-body">{p.item.text}</div>
      </Show>
    </div>
  );
}

export function Transcript(p: { items: StreamItem[] }) {
  return (
    <>
      <For each={p.items}>
        {(item) => {
          switch (item.kind) {
            case "user":
              return (
                <div class="user">
                  <span>{item.text}</span>
                </div>
              );
            case "think":
              return <ThinkBlock item={item} />;
            case "text":
              return (
                <div class={`st-text ${item.live ? "live" : ""}`}>
                  <MdView text={item.text} />
                  <Show when={item.live}>
                    <span class="st-caret" />
                  </Show>
                </div>
              );
            case "tool":
              return <ToolRow item={item} />;
            case "error":
              return <p class="st-error">{item.text}</p>;
          }
        }}
      </For>
    </>
  );
}
