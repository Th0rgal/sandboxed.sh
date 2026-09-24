import {For, Show, createMemo, createSignal, onCleanup} from 'solid-js';
import {BranchIcon, DotsIcon} from './icons';
import type {LocalActivity} from './localAgents';

export function activityState(item: LocalActivity, running: boolean) {
  if (item.status === 'stopped') return 'Stopped';
  if (item.failed) return 'Failed';
  if (item.done) return item.status === 'finished' ? 'Finished' : 'Done';
  return running ? 'Running' : 'No result recorded';
}
export function activityDuration(item: LocalActivity, now: number, running: boolean) {
  if (!item.started_at) return '';
  const end = item.finished_at ?? (running && !item.done ? now : item.updated_at);
  if (!end) return '';
  const seconds = Math.max(0, Math.floor((end - item.started_at) / 1000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}
export function AgentActivity(p: {items: LocalActivity[]; running: boolean}) {
  const [now, setNow] = createSignal(Date.now());
  const timer = setInterval(() => { if (p.running) setNow(Date.now()); }, 1000);
  onCleanup(() => clearInterval(timer));
  const tasks = createMemo(() => p.items.filter(a => a.background || a.id.startsWith('task:')));
  const tools = createMemo(() => {
    const spawned = new Set(tasks().map(a => a.tool_use_id).filter(Boolean));
    return p.items.filter(a => !a.background && !a.id.startsWith('task:') && !spawned.has(a.id));
  });
  const activeTasks = () => tasks().filter(a => !a.done || a.failed);
  const completedTasks = () => tasks().filter(a => a.done && !a.failed);
  const activeTools = () => tools().filter(a => !a.done);
  const completedTools = () => tools().filter(a => a.done);
  const row = (id: string) => <ActivityRow item={p.items.find(a=>a.id===id)!} running={p.running} now={now()}/>;
  return <Show when={p.items.length}><section class="agent-activity" aria-label="Agent activity">
    <Show when={tasks().length}><div class="agent-activity-heading">Background work <span>{tasks().filter(a=>!a.done).length} active</span></div></Show>
    <For each={activeTasks().map(a=>a.id)}>{row}</For>
    <Show when={completedTasks().length}><details class="agent-activity-history"><summary>{completedTasks().length} completed {completedTasks().length === 1 ? 'task' : 'tasks'}</summary><For each={completedTasks().map(a=>a.id)}>{row}</For></details></Show>
    <For each={activeTools().map(a=>a.id)}>{row}</For>
    <Show when={completedTools().length}><details class="agent-activity-history"><summary>{completedTools().length} earlier {completedTools().length === 1 ? 'action' : 'actions'}<Show when={completedTools().some(a=>a.failed)}><span class="agent-activity-failed"> · {completedTools().filter(a=>a.failed).length} failed</span></Show></summary><For each={completedTools().map(a=>a.id)}>{row}</For></details></Show>
  </section></Show>;
}

function ActivityRow(p: {item: LocalActivity; running: boolean; now: number}) {
  return <details class="agent-task" classList={{'is-running': !p.item.done && p.running, 'is-failed': p.item.failed}}>
    <summary>
      <span class="agent-task-icon" aria-hidden="true">{p.item.kind === 'agent' || p.item.label === 'Agent' ? <BranchIcon size={15}/> : p.item.kind === 'thinking' || p.item.label === 'Thinking' ? <DotsIcon size={15}/> : '›_'}</span>
      <span class="agent-task-name" title={p.item.label}><span>{p.item.label}</span><Show when={p.item.background || p.item.id.startsWith('task:')}><small>{p.item.kind === 'agent' ? 'Subagent' : 'Background task'}</small></Show></span>
      <span class="agent-task-state">{activityState(p.item, p.running)}</span>
      <time class="agent-task-time">{activityDuration(p.item, p.now, p.running)}</time>
      <span class="agent-task-chevron" aria-hidden="true">›</span>
    </summary>
    <div class="agent-task-detail"><Show when={p.item.detail} fallback={<span class="agent-task-empty">{p.item.done ? 'No additional details were reported.' : 'Waiting for the next update…'}</span>}><pre>{p.item.detail}</pre></Show></div>
  </details>;
}
