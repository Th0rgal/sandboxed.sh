import {anchoredDisclosure} from "./anchoredDisclosure";
import {For, Show, createEffect, createMemo, createSignal, onCleanup, type JSX} from 'solid-js';
import {BranchIcon, DotsIcon, FileIcon, SearchIcon, PencilIcon, GearIcon} from './icons';
import type {LocalActivity} from './localAgents';

/** A successful turn can await a follow-up without completing the conversation.
 * Native work still in flight always wins over a stale server status. */
export function activityShouldCollapse(status: string | undefined, running: boolean) {
  return !running && ["completed", "awaiting_user", "acknowledged"].includes(status ?? "");
}

export function activityState(item: LocalActivity, running: boolean) {
  if (item.status === 'stopped') return 'Stopped';
  if (item.failed) return 'Failed';
  if (item.done) return item.status === 'finished' ? 'Finished' : 'Done';
  return running ? 'Running' : 'No result recorded';
}
/** Only explicit CI wait labels qualify; builds and arbitrary sleeping tools do not. */
export function isCiWait(item: LocalActivity) {
  return /\bwait(?:ing)?\b/i.test(item.label) && /\b(?:CI|checks?|pipelines?|workflows?|verify proofs|foundry)\b/i.test(item.label);
}
export function activityDuration(item: LocalActivity, now: number, running: boolean) {
  if (!item.started_at) return '';
  const end = item.finished_at ?? (running && !item.done ? now : item.updated_at);
  if (!end) return '';
  const seconds = Math.max(0, Math.floor((end - item.started_at) / 1000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}
export function AgentActivity(p: {items: LocalActivity[]; running: boolean; completed?: boolean}) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => { if (p.running) setNow(Date.now()); }, 1000);
  onCleanup(() => clearInterval(timer));
  const tasks = createMemo(() => p.items.filter(a => a.background || a.id.startsWith('task:')));
  const tools = createMemo(() => {
    const spawned = new Set(tasks().map(a => a.tool_use_id).filter(Boolean));
    return p.items.filter(a => !a.background && !a.id.startsWith('task:') && !spawned.has(a.id));
  });
  const all = createMemo(() => [...tasks(), ...tools()]);
  const isActive = (a: LocalActivity) => p.running && !a.done && !a.failed && a.status !== 'stopped';
  const [visible, setVisible] = createSignal<string[]>([]);
  const retiring = new Map<string, ReturnType<typeof setTimeout>>();
  createEffect(() => {
    const active = all().filter(isActive).map(a => a.id);
    setVisible(previous => {
      for (const id of previous) {
        if (active.includes(id)) {
          clearTimeout(retiring.get(id)); retiring.delete(id);
        } else if (!retiring.has(id)) {
          retiring.set(id, setTimeout(() => {
            retiring.delete(id);
            setVisible(ids => ids.filter(value => value !== id));
          }, 180));
        }
      }
      return [...active, ...previous.filter(id => !active.includes(id) && all().some(a => a.id === id))];
    });
  });
  onCleanup(() => { for (const timer of retiring.values()) clearTimeout(timer); });
  const history = () => all().filter(a => !isActive(a) && !visible().includes(a.id));
  const pastTasks = () => history().filter(a => a.background || a.id.startsWith('task:')).length;
  const pastActions = () => history().length - pastTasks();
  const failures = () => history().filter(a => a.failed && a.status !== 'stopped').length;
  const historyLabel = () => [
    pastTasks() ? `${pastTasks()} previous ${pastTasks() === 1 ? 'task' : 'tasks'}` : '',
    pastActions() ? `${pastActions()} ${pastActions() === 1 ? 'action' : 'actions'}` : '',
  ].filter(Boolean).join(' · ');
  const row = (id: string) => {
    const item = createMemo<LocalActivity | undefined>(previous => all().find(a=>a.id===id) ?? previous);
    return <Show when={item()}>{value => <ActivityRow item={value()} running={p.running} now={now()} />}</Show>;
  };
  const current = () => all().find(a => visible().includes(a.id) && isActive(a));
  return <Show when={p.items.length}><section class="agent-activity" classList={{'is-completed': !!p.completed}} aria-label="Agent activity" aria-hidden={p.completed || undefined} inert={!!p.completed}>
    <div class="agent-activity-collapse"><div class="agent-activity-content">
    <ActivityHistory ids={[...all().filter(a=>!isActive(a)), ...all().filter(isActive)].map(a=>a.id)} label={historyLabel() || 'Activity'} failures={failures()} row={id => row(id)} active={!!current()} lead={<Show when={current()} keyed>{item => <>
      <span class="activity-live-icon" aria-hidden="true"><CurrentActivityIcon item={item} /></span>
      <span class="agent-task-name"><span>{isCiWait(item) ? "Waiting for CI" : item.label}</span></span>
      <time class="agent-task-time">{activityDuration(item, now(), p.running)}</time>
      <Show when={all().filter(isActive).length > 1}><span class="agent-task-state">+{all().filter(isActive).length - 1} running</span></Show>
      <Show when={history().length}><span class="agent-task-state">{historyLabel()}</span></Show>
    </>}</Show>} />
    </div></div>
  </section></Show>;
}

function ActivityHistory(p: {ids: string[]; label: string; failures: number; row: (id: string) => JSX.Element; lead?: JSX.Element; active?: boolean}) {
  const [open,setOpen] = createSignal(false);
  const [limit,setLimit] = createSignal(20);
  let toggle!: HTMLButtonElement;
  let earlier: HTMLButtonElement | undefined;
  const remaining = () => Math.max(0,p.ids.length-limit());
  const scroller = () => {
    let node: HTMLElement | null = toggle.parentElement;
    while (node) {
      if (/(auto|scroll)/.test(getComputedStyle(node).overflowY)) return node;
      node=node.parentElement;
    }
    return document.scrollingElement as HTMLElement;
  };
  let frame = 0;
  function anchored(change: () => void) {
    const container=scroller(), before=toggle.getBoundingClientRect().top;
    change();
    cancelAnimationFrame(frame);
    frame=requestAnimationFrame(()=> { container.scrollTop += toggle.getBoundingClientRect().top-before; });
  }
  function loadEarlier() { anchored(()=>setLimit(n=>n+20)); }
  createEffect(()=> {
    if (!open() || !remaining() || !earlier || typeof IntersectionObserver === 'undefined') return;
    const container=scroller();
    const observer=new IntersectionObserver(entries=> {
      if (entries.some(entry=>entry.isIntersecting)) { observer.disconnect(); loadEarlier(); }
    },{root: container===document.scrollingElement ? null : container,rootMargin:'200px 0px 0px'});
    observer.observe(earlier);
    onCleanup(()=>observer.disconnect());
  });
  onCleanup(()=>cancelAnimationFrame(frame));
  return <div class="agent-activity-history" data-open={open()}>
    <Show when={open()}><div class="agent-history-entries">
      <Show when={remaining()}><button ref={earlier} class="agent-history-earlier" onClick={loadEarlier}>Show earlier actions ({remaining()})</button></Show>
      <For each={p.ids.slice(-limit())}>{p.row}</For>
    </div></Show>
    <button ref={toggle} class="agent-history-toggle" classList={{"has-current": !!p.active}} aria-expanded={open()} onClick={()=>anchored(()=> {setLimit(20);setOpen(v=>!v);})}>
      <Show when={p.active} fallback={p.label}>{p.lead}</Show><span class="history-chevron" aria-hidden="true">›</span><Show when={p.failures}><span class="agent-activity-failed"> · {p.failures} {p.failures===1?'error':'errors'}</span></Show>
    </button>
  </div>;
}

function ActivityRow(p: {item: LocalActivity; running: boolean; now: number; retiring?: boolean}) {
  const [open,setOpen]=createSignal(false);
  const anchored=anchoredDisclosure();
  let toggle!:HTMLButtonElement;
  return <div class="agent-task" data-open={open()} classList={{'is-running': !p.item.done && !p.item.failed && p.item.status !== 'stopped' && p.running, 'is-failed': p.item.failed && p.item.status !== 'stopped', 'is-retiring': p.retiring}}>
    <Show when={open()}>
    <div class="agent-task-detail"><Show when={p.item.detail} fallback={<span class="agent-task-empty">{p.item.done ? 'No additional details were reported.' : 'Waiting for the next update…'}</span>}><pre>{p.item.detail}</pre></Show></div>
    </Show>
    <button class="agent-task-toggle" ref={toggle} aria-expanded={open()} onClick={()=>anchored(toggle,()=>setOpen(v=>!v))}>
      <span class="agent-task-icon" aria-hidden="true">{p.item.kind === 'agent' || p.item.label === 'Agent' ? <BranchIcon size={15}/> : p.item.kind === 'thinking' || p.item.label === 'Thinking' ? <DotsIcon size={15}/> : '›_'}</span>
      <span class="agent-task-name" title={p.item.label}><span>{p.item.label}</span></span>
      <Show when={p.item.done || !p.running || p.item.failed || p.item.status === 'stopped'}><span class="agent-task-state">{activityState(p.item, p.running)}</span></Show>
      <time class="agent-task-time">{activityDuration(p.item, p.now, p.running)}</time>
      <span class="agent-task-chevron" aria-hidden="true">›</span>
    </button>

  </div>;
}

function CurrentActivityIcon(p: {item: LocalActivity}) {
  const type = () => {
    if (p.item.kind === 'thinking' || /^thinking$/i.test(p.item.label)) return 'thinking';
    if (isCiWait(p.item)) return 'waiting';
    if (p.item.kind === 'agent') return 'agent';
    if (/^(read|open|inspect)\b/i.test(p.item.label)) return 'read';
    if (/^(search|grep|glob|find)\b/i.test(p.item.label)) return 'search';
    if (/^(edit|write|patch)\b/i.test(p.item.label)) return 'edit';
    return 'tool';
  };
  return <span data-activity={type()}>
    {type() === 'thinking' ? <DotsIcon size={16}/> : type() === 'agent' ? <BranchIcon size={16}/> : type() === 'read' ? <FileIcon size={16}/> : type() === 'search' ? <SearchIcon size={16}/> : type() === 'edit' ? <PencilIcon size={16}/> : type() === 'waiting' ? <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.1"><circle cx="8" cy="8" r="5.5"/><path d="M8 4.5V8l2.5 1.5"/></svg> : <GearIcon size={16}/>}
  </span>;
}
