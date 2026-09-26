import {anchoredDisclosure} from "./anchoredDisclosure";
import { messageImages } from "./messageImages";
import { imagePrompt } from "./imageAttachments";
import { Dialog } from "./Dialog";
import { FileReferenceContext } from "./fileReferenceContext";
import { copyText } from "./clipboard";
import { remoteLog } from "./remoteLog";
import { ErrorNotice } from "./ErrorNotice";
import { forkContext } from "./forkContext";
import { For, Show, createSignal, createEffect, createMemo, useContext, onCleanup } from "solid-js";
import * as Ic from "./icons";
import { MdView } from "./Markdown";
import { createStore, reconcile } from "solid-js/store";
import { goalDraft, planObjective } from "./goal";

import { messagePresentation } from "./messagePresentation";
import { latestChecklist, toolArgs, toolName, workSummary } from "./workModel";
import { visibleTranscript, type StreamItem } from "./transcriptModel";
export { buildTranscript, applyStreamEvent } from "./transcriptModel";
export type { StreamItem } from "./transcriptModel";

/** Short, human-readable target for a tool call row (Cursor-style). */
function toolTarget(name: string, args: unknown): string {
  const a = toolArgs(args);
  if (!a) return "";
  const pick = (...keys: string[]): string => {
    for (const k of keys) {
      const v = a[k];
      if (typeof v === "string" && v.trim()) return v.trim();
    }
    return "";
  };
  let t = "";
  switch (toolName(name)) {
    case "exec_command":
    case "run_terminal_command":
    case "shell_command":
    case "bash":
      t = pick("command", "cmd");
      break;
    case "read_file":
    case "write_file":
    case "edit_file":
    case "read":
    case "write":
    case "edit":
      t = pick("file_path", "filePath", "path", "file");
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
function MessageImage(p: {path:string; index:number}) {
  const resolver=useContext(FileReferenceContext);
  const [url,setUrl]=createSignal<string | null>(null);
  const [expanded,setExpanded]=createSignal(false);
  createEffect(() => {
    const path=p.path;
    let cancelled=false;
    let loaded:string | null=null;
    setUrl(null);
    void resolver?.loadImage?.(path).then(value => {
      if(cancelled) { if(value)URL.revokeObjectURL(value); return; }
      loaded=value;setUrl(value);
    }).catch(() => {});
    onCleanup(() => {cancelled=true;if(loaded)URL.revokeObjectURL(loaded);});
  });
  return <>
    <button class="message-image" aria-label={`Image #${p.index}`} title={url()?`Open image #${p.index}`:`Image #${p.index} — preview unavailable`} disabled={!url()} onDblClick={e=>e.stopPropagation()} onClick={e=>{e.stopPropagation();setExpanded(true);}}>
      <Show when={url()} fallback={<Ic.FileIcon size={22}/>}>{src=><img src={src()} alt={`Image #${p.index}`} onError={()=>setUrl(null)}/>}</Show>
      <span>#{p.index}</span>
    </button>
    <Show when={expanded() && url()}><Dialog title={`Image #${p.index}`} size="wide" onClose={()=>setExpanded(false)}><img class="message-image-preview" src={url()!} alt={`Image #${p.index}`}/></Dialog></Show>
  </>;
}

const AUTOMATIC_SOURCES = new Set(["scheduler", "idle-worker-watchdog", "transport_auto_resume", "remote-build-terminal", "task-board"]);

export function UserTurn(p: { text: string; source?: string; attached?: boolean; pending?: boolean; onSend?: (text: string) => boolean | Promise<boolean> }) {
  const fork = createMemo(() => forkContext(p.text));
  const presentation = createMemo(() => messagePresentation(p.text));
  const images = createMemo(() => messageImages(presentation().text));
  const goal = createMemo(() => goalDraft(images().text));
  const plan = createMemo(() => planObjective(images().text));
  let bubble!: HTMLDivElement;
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [copyState, setCopyState] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [sendError, setSendError] = createSignal("");
  const submit = async () => {
    if (sending() || !draft().trim() || !p.onSend) return;
    setSending(true); setSendError("");
    try {
      const accepted = await p.onSend(imagePrompt(draft(), images().paths));
      if (accepted) setEditing(false);
      else setSendError("The message was not sent. Your draft is kept; try again.");
    } catch (e) { setSendError(e instanceof Error ? e.message : String(e)); }
    finally { setSending(false); }
  };
  const resizeEditor = (el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  };
  const edit = () => { if (fork()) return; bubble.style.setProperty("--editing-width", `${bubble.getBoundingClientRect().width}px`); setDraft(images().text); setCopyState(""); setSendError(""); setEditing(true); };
  return (
    <div ref={bubble} onDblClick={() => { if (!editing()) edit(); }} class={`user ${editing() ? "editing" : ""} ${goal().kind === "goal" ? "goal" : ""} ${plan() !== null ? "plan" : ""} ${p.pending ? "pending" : ""}`}>
      <Show when={images().paths.length}><div class="message-images"><For each={images().paths}>{(path,index)=><MessageImage path={path} index={index()+1}/>}</For></div></Show>
      <Show when={editing()} fallback={<>
      <Show when={p.source && AUTOMATIC_SOURCES.has(p.source)}><small class="user-origin" title="This message was generated by the agent coordinator">↻ Automatic follow-up</small></Show>
      <Show when={plan() !== null}><small class="user-plan"><Ic.PlanIcon size={12}/>Plan</small></Show>
      <Show when={fork()} fallback={<span>{goal().kind === "goal" ? (goal() as { objective: string }).objective : plan() ?? images().text}</span>}>
        {context => <details class="fork-context"><summary>Forked from {context().source_title || "conversation"} · {context().messages.length} messages</summary>
          <For each={context().messages}>{m => <div class="fork-context-message"><small>{m.role === "user" ? "You" : "Assistant"}</small><p>{m.content}</p></div>}</For>
        </details>}
      </Show>
      <Show when={p.attached || presentation().attached}><small class="user-context">Attached context</small></Show>
      <Show when={!fork()}><button class="icon-btn prompt-edit" aria-label="Edit prompt" onClick={edit}><Ic.PencilIcon size={14} /></button></Show>
      </>}>
        <textarea class="prompt-editor" rows={1} aria-label="Edit prompt text" disabled={sending()} value={draft()} onInput={e => { setDraft(e.currentTarget.value); resizeEditor(e.currentTarget); }} onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); if (!sending()) setEditing(false); } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.isComposing) { e.preventDefault(); void submit(); } }} ref={el => queueMicrotask(() => { resizeEditor(el); el.focus({ preventScroll: true }); })} />
        <div class="prompt-editor-actions">
          <button class="icon-btn" aria-label="Cancel" title="Cancel (Esc)" disabled={sending()} onClick={() => setEditing(false)}><Ic.CloseIcon size={16} /></button>
          <button class="icon-btn" aria-label="Copy prompt" title="Copy prompt" onClick={() => { void copyText(draft()).then(() => setCopyState("Copied"), e => setCopyState(String(e))); }}><Ic.CopyIcon size={15} /></button>
          <span role="status">{copyState()}</span>
          <Show when={p.onSend}><button class="send" aria-label={sending() ? "Sending follow-up" : "Send follow-up"} title="Send as follow-up (⌘/Ctrl+Enter)" disabled={sending() || !draft().trim()} onClick={() => void submit()}><Ic.ArrowUpIcon size={16} /></button></Show>
        </div>
        <Show when={sendError()}><ErrorNotice error={sendError()} /></Show>
      </Show>
    </div>
  );
}

function ToolRow(p: { item: Extract<StreamItem, { kind: "tool" }> }) {
  const [open, setOpen] = createSignal(false);
  const anchored=anchoredDisclosure();
  let toggle!:HTMLButtonElement;
  const target = () => toolTarget(p.item.name, p.item.args);
  const failed=()=>{const r=toolArgs(p.item.result);return !!r&&(!!r.error||r.status==="failed"||r.is_error===true);};
  const detail = () => {
    const parts: string[] = [];
    if (p.item.args != null) parts.push(typeof p.item.args === "string" ? p.item.args : JSON.stringify(p.item.args, null, 2));
    const r = resultText(p.item.result);
    if (r) parts.push(r);
    if(p.item.unresolved)parts.push("This turn ended without a recorded result for this action.");
    return parts.join("\n\n");
  };
  return (
    <div class={`st-tool ${open() ? "open" : ""}`}>
      <Show when={open()}>
        <pre class="st-tool-detail">{detail() || "(no details)"}</pre>
      </Show>
      <button class="st-tool-head" ref={toggle} aria-expanded={open()} onClick={() => anchored(toggle,()=>setOpen(!open()))}>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        <span class="st-tool-name">{p.item.name}</span>
        <Show when={target()}>
          <span class="st-tool-target">{target()}</span>
        </Show>
        <span class="st-tool-state">
          <Show when={p.item.done} fallback={<Ic.Spinner size={12} />}>
            <span class="st-tool-check" classList={{"is-failed":failed()}} title={p.item.unresolved?"No result recorded":failed()?"Failed":undefined}>{p.item.unresolved?"—":failed()?"!":"✓"}</span>
          </Show>
        </span>
      </button>

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
  const items = visibleTranscript(input);
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
  const anchored=anchoredDisclosure();
  let toggle!:HTMLButtonElement;
  const current = () => {
    const cur = [...p.items].reverse().find((t) => (t.kind === "tool" ? !t.done : !t.done));
    if (!cur) return "Working…";
    if (cur.kind === "think") return "Thinking";
    return `${cur.name} ${toolTarget(cur.name, cur.args) ?? ""}`.trim();
  };
  const summary = () => workSummary(p.items);
  return (
    <div class={`st-work ${open() ? "open" : ""}`}>
      <Show when={open()}>
        <div class="st-work-body">
          <For each={p.items}>
            {(t) => (t.kind === "tool" ? <ToolRow item={t} /> : (
              <Show when={t.text}><div class="st-think-body">{t.text}</div></Show>
            ))}
          </For>
        </div>
      </Show>
      <button class="st-work-head" ref={toggle} aria-expanded={open()} onClick={() => anchored(toggle,()=>setOpen(!open()))}>
        <Ic.ChevronRight size={12} class={`chev ${open() ? "open" : ""}`} />
        <Show when={running()} fallback={<span class="st-work-label">{summary()}</span>}>
          <span class="st-work-label shimmer">{current()}</span>
        </Show>
      </button>

    </div>
  );
}

export function Transcript(p: { items: StreamItem[]; pending?: boolean; onSend?: (text: string) => boolean | Promise<boolean> }) {
  // Reconcile by stable keys: existing WorkFold/ToolRow instances and parsed
  // historical Markdown survive token updates and history resynchronization.
  const [grouped, setGrouped] = createStore<Grouped[]>([]);
  const checklist = createMemo(() => latestChecklist(p.items));
  const groups = createMemo<Grouped[]>((previous) => groupWork(p.items.filter(item => item.kind !== "user" || !item.queued), previous), []);
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
              return <UserTurn text={item.text} source={item.source} attached={item.attached} onSend={p.onSend} pending={p.pending && item.key === lastUserKey()} />;
            case "think":
              return <ThinkBlock item={item} />;
            case "text":
              return (
                <div class={`st-text ${item.live ? "live" : ""}`}>
                  <AssistantText text={item.text} live={item.live} />
                </div>
              );
            case "tool":
              return <ToolRow item={item} />;
            case "error":
              return <ErrorNotice error={item.text} title={item.cancelled ? "Mission cancelled" : "Mission failed"} />;
          }
        }}
      </For>
      <Show when={checklist()?.tasks.length}>
        <section class="mission-tasks" id="mission-tasks" aria-label="Tasks" tabIndex={-1}>
          <div class="tasks-heading"><strong>Tasks</strong><span>{checklist()!.tasks.filter(task => task.status === "completed").length}/{checklist()!.tasks.length} completed</span></div>
          <progress aria-label="Task progress" max={checklist()!.tasks.length} value={checklist()!.tasks.filter(task => task.status === "completed").length} />
          <ol><For each={checklist()!.tasks}>{task => <li data-status={task.status}>
            <span class={`task-state ${task.status === "in_progress" ? "shimmer" : ""}`} aria-label={task.status.replaceAll("_", " ")}>{task.status === "completed" ? "✓" : task.status === "cancelled" ? "−" : task.status === "in_progress" ? "◉" : "○"}</span><span>{task.text}</span>
          </li>}</For></ol>
        </section>
      </Show>
    </>
  );
}

function AssistantText(p: { text: string; live?: boolean }) {
  const references = useContext(FileReferenceContext);
  const content = createMemo(() => remoteLog(p.text));
  const [copied, setCopied] = createSignal(false);
  const [copyError, setCopyError] = createSignal("");
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => { p.text; setCopied(false); setCopyError(""); clearTimeout(timer); });
  onCleanup(() => clearTimeout(timer));
  const copy = async () => {
    const text = content().text;
    try {
      await copyText(text);
      if (text !== content().text) return;
      setCopied(true); setCopyError(""); clearTimeout(timer);
      timer = setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      if (text === content().text) setCopyError(error instanceof Error ? error.message : String(error));
    }
  };
  return <>
    <Show when={!p.live} fallback={<FileReferenceContext.Provider value={undefined}><MdView text={content().text} compact /></FileReferenceContext.Provider>}>
      <FileReferenceContext.Provider value={references}><MdView text={content().text} compact /></FileReferenceContext.Provider>
    </Show>
    <Show when={content().details}><details class="legacy-log"><summary>Original execution log</summary><pre>{content().details}</pre></details></Show>
    <Show when={content().text.trim()}>
      <div class="response-actions">
        <button class="icon-btn response-copy" aria-label={copied() ? "Response copied" : "Copy response"} title={copied() ? "Copied" : "Copy response"} onClick={() => void copy()}>
          <Show when={copied()} fallback={<Ic.CopyIcon size={14} />}><Ic.CheckIcon size={14} /></Show>
        </button>
        <span class="sr-only" role="status">{copied() ? "Response copied" : ""}</span>
        <Show when={copyError()}><span class="response-copy-error" role="status">{copyError()}</span></Show>
      </div>
    </Show>
  </>;
}
