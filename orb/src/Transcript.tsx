import { For, Show, createSignal, createEffect, createMemo } from "solid-js";
import * as Ic from "./icons";
import { MdView } from "./Markdown";
import { createStore, reconcile } from "solid-js/store";
import { goalDraft, GoalTag } from "./goal";

import { withoutFiller, type StreamItem } from "./transcriptModel";
export { buildTranscript, applyStreamEvent } from "./transcriptModel";
export type { StreamItem } from "./transcriptModel";

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

/** A user turn. A `/goal <objective>` message is shown as a Goal turn with the
 * exact objective, not the raw slash command; the text itself is untouched. */
/**
 * `pending` marks the prompt whose answer is still coming. It is the only
 * signal that a healthy mission is working: a slow shimmer along the turn, no
 * text and no extra height, replacing the banner that used to sit above the
 * transcript announcing what the footer already says.
 */
export function UserTurn(p: { text: string; pending?: boolean }) {
  const goal = createMemo(() => goalDraft(p.text));
  return (
    <div class={`user ${goal().kind === "goal" ? "goal" : ""} ${p.pending ? "pending" : ""}`}>
      <Show when={goal().kind === "goal"}><GoalTag /></Show>
      <span>{goal().kind === "goal" ? (goal() as { objective: string }).objective : p.text}</span>
    </div>
  );
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
  const [open, setOpen] = createSignal(true);
  return (
    <div class={`st-think ${open() ? "open" : ""}`}>
      <button class="st-think-head" onClick={() => setOpen(!open())}>
        <Show when={p.item.done} fallback={<span class="shimmer">Thinking</span>}>
          <span>Thinking</span>
        </Show>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
      </button>
      <Show when={open() && p.item.text}>
        <div class="st-think-body">{p.item.text}</div>
      </Show>
    </div>
  );
}

/** Everything an agent does between two pieces of visible text (thoughts
 * and tool calls, interleaved) folds into one "Worked" line, Cursor-style:
 * the header shows the current tool while running; the body stays closed
 * until the user opens it, so a 150-tool run does not dump the full list. */
type WorkItem = Extract<StreamItem, { kind: "tool" | "think" }>;
type Grouped = StreamItem | { kind: "work"; key: string; items: WorkItem[] };

function groupWork(input: StreamItem[], previous: Grouped[] = []): Grouped[] {
  const cached = new Map(previous.filter(x => x.kind === "work").map(x => [x.key, x]));
  const out: Grouped[] = [];
  // Dropping a filler bubble also rejoins the work around it, so one stretch of
  // tool calls reads as one fold instead of being split in two by a stray ".".
  const items = withoutFiller(input);
  for (const it of items) {
    const last = out[out.length - 1];
    if (it.kind === "tool" || it.kind === "think") {
      if (last && last.kind === "work") last.items.push(it);
      else out.push({ kind: "work", key: it.key, items: [it] });
    } else {
      out.push(it);
    }
  }
  return out.map(item => {
    if (item.kind !== "work") return item;
    const old = cached.get(item.key);
    return old?.kind === "work" && old.items.length === item.items.length && old.items.every((entry, i) => entry === item.items[i]) ? old : item;
  });
}

function WorkFold(p: { items: WorkItem[] }) {
  const running = () => p.items.some((t) => (t.kind === "tool" ? !t.done : !t.done));
  const [open, setOpen] = createSignal(false);
  const tools = () => p.items.filter((t) => t.kind === "tool").length;
  const current = () => {
    const cur = [...p.items].reverse().find((t) => (t.kind === "tool" ? !t.done : !t.done));
    if (!cur) return "Working…";
    if (cur.kind === "think") return "Thinking";
    return `${cur.name} ${toolTarget(cur.name, cur.args) ?? ""}`.trim();
  };
  const summary = () => {
    const n = tools();
    return n === 0 ? "Thought" : `Worked · ${n} tool${n === 1 ? "" : "s"}`;
  };
  return (
    <div class={`st-work ${open() ? "open" : ""}`}>
      <button class="st-work-head" onClick={() => setOpen(!open())}>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        <Show when={running()} fallback={<span class="st-work-label">{summary()}</span>}>
          <span class="st-work-label shimmer">{current()}</span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="st-work-body">
          <For each={p.items}>
            {(t) => (t.kind === "tool" ? <ToolRow item={t} /> : (
              <Show when={t.text}><div class="st-think-body">{t.text}</div></Show>
            ))}
          </For>
        </div>
      </Show>
    </div>
  );
}

export function Transcript(p: { items: StreamItem[]; pending?: boolean }) {
  // Reconcile by stable keys: existing WorkFold/ToolRow instances and parsed
  // historical Markdown survive token updates and history resynchronization.
  const [grouped, setGrouped] = createStore<Grouped[]>([]);
  const groups = createMemo<Grouped[]>((previous) => groupWork(p.items, previous), []);
  /** Key of the last user turn, when nothing follows it yet. Compared by key
   * rather than identity: `reconcile` hands the loop store proxies, not the
   * original objects. */
  const lastUserKey = createMemo(() => {
    const last = p.items[p.items.length - 1];
    return last?.kind === "user" ? last.key : null;
  });
  createEffect(() => setGrouped(reconcile(groups(), { key: "key" })));
  return (
    <>
      <For each={grouped}>
        {(item) => {
          switch (item.kind) {
            case "work":
              if (!item.items.some((t) => t.kind === "tool")) {
                return (
                  <For each={item.items}>
                    {(t) => (t.kind === "think" ? <ThinkBlock item={t} /> : null)}
                  </For>
                );
              }
              return <WorkFold items={item.items} />;
            case "user":
              // Only the turn still waiting for a reply animates: once anything
              // has been said or done after it, the work is visible on its own.
              return <UserTurn text={item.text} pending={p.pending && item.key === lastUserKey()} />;
            case "think":
              return <ThinkBlock item={item} />;
            case "text":
              return (
                <div class={`st-text ${item.live ? "live" : ""}`}>
                  <MdView text={item.text} compact />
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
