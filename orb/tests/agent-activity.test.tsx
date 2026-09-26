import {createSignal} from 'solid-js';
import {describe,it,expect,afterEach} from 'vitest';
import {render,screen,cleanup,fireEvent,waitFor} from '@solidjs/testing-library';
import {AgentActivity,activityDuration,activityState,activityShouldCollapse,isCiWait} from '../src/AgentActivity';
import type {LocalActivity} from '../src/localAgents';
afterEach(cleanup);
const task:LocalActivity={id:'task:a',label:'Inspect the importer',kind:'agent',background:true,tool_use_id:'spawn',done:false,failed:false,started_at:1000,updated_at:4000};
describe('agent activity',()=>{
 it('keeps live tasks visible and hides the tool call that spawned them',()=>{
  render(()=><AgentActivity running items={[{id:'spawn',label:'Agent',done:true,failed:false},task,{...task,id:'task:b',label:'Inventory docs',done:true,status:'completed'}]}/>);
  expect(screen.getByText('Inspect the importer')).toBeTruthy();
  expect(screen.queryByText('Agent')).toBeNull();
  expect(screen.getByText('1 previous task')).toBeTruthy();
  expect(screen.queryByText('Inventory docs')).toBeNull();
 });
 it('keeps failures visible and exposes their reported details',()=>{
  render(()=><AgentActivity running={false} items={[{...task,done:true,failed:true,status:'failed',detail:'Build exited with code 1'}]}/>);
  fireEvent.click(screen.getByText('1 previous task'));
  fireEvent.click(screen.getByText('Inspect the importer'));
  expect(screen.getByText('Failed')).toBeTruthy();
  expect(screen.getByText('Build exited with code 1').closest('.agent-task')?.getAttribute('data-open')).toBe('true');
 });
 it('keeps expanded details open across native polling snapshots',()=>{
  const [items,setItems]=createSignal<LocalActivity[]>([{...task,detail:'Reading files'}]);
  render(()=><AgentActivity running items={items()}/>);
  fireEvent.click(screen.getByText('Inspect the importer'));
  const details=document.querySelector('.agent-history-entries .agent-task') as HTMLElement;
  fireEvent.click(details.querySelector('button')!);
  expect(details?.getAttribute("data-open")).toBe("true");
  setItems([{...task,detail:'Inspecting tests'}]);
  expect(screen.getByText('Inspecting tests').closest('.agent-task')).toBe(details);
  expect(details?.getAttribute("data-open")).toBe("true");
  setItems([]);
  expect(screen.queryByRole('region',{name:'Agent activity'})).toBeNull();
 });
 it('moves settled tasks into one history and treats stopped tasks as neutral',async()=>{
  const [items,setItems]=createSignal<LocalActivity[]>([task]);
  const {container}=render(()=><AgentActivity running items={items()}/>);
  expect(container.querySelectorAll('.agent-history-toggle.has-current')).toHaveLength(1);
  setItems([{...task,done:true,failed:true,status:'stopped'}]);
  expect(container.querySelector('.agent-history-toggle.has-current')).toBeNull();
  await waitFor(()=>expect(screen.getByText('1 previous task')).toBeTruthy());
  expect(container.querySelectorAll('.is-running,.is-failed,.agent-activity-failed')).toHaveLength(0);
  expect(screen.queryByText('Stopped')).toBeNull();
 });
 it('does not invent durations or success for old or incomplete results',()=>{
  expect(activityDuration({...task,started_at:undefined},5000,true)).toBe('');
  expect(activityDuration({...task,done:true,finished_at:62000},100000,true)).toBe('1m 1s');
  expect(activityState(task,false)).toBe('No result recorded');
  expect(activityState({...task,done:true,status:'finished'},false)).toBe('Finished');
 });
});

 describe('activity turn completion',()=>{
  it.each(['completed','awaiting_user','acknowledged'])('collapses a settled %s turn but preserves native work still running',status=>{
   expect(activityShouldCollapse(status,false)).toBe(true);
   expect(activityShouldCollapse(status,true)).toBe(false);
  });
  it.each(['failed','interrupted','paused','active','blocked',undefined])('does not hide unresolved activity for %s',status=>{
   expect(activityShouldCollapse(status,false)).toBe(false);
  });
  it('closes stale task rows after a successful local turn and reveals them on resume',()=>{
   const [running,setRunning]=createSignal(true);
   const [status,setStatus]=createSignal('active');
   const {container}=render(()=><AgentActivity items={[task]} running={running()} completed={activityShouldCollapse(status(),running())}/>);
   const panel=container.querySelector('.agent-activity')!;
   setStatus('awaiting_user');
   expect(panel.getAttribute('aria-hidden')).toBeNull();
   setRunning(false);
   expect(panel.getAttribute('aria-hidden')).toBe('true');
   expect(panel.classList.contains('is-completed')).toBe(true);
   setRunning(true);
   expect(panel.getAttribute('aria-hidden')).toBeNull();
  });
 });

it('groups CI waits and preserves unrelated work and action details',()=>{
 const waits=[{...task,id:'task:ci1',label:'Wait for Verity Verify proofs run'}, {...task,id:'task:ci2',label:'Wait for Verity CI result file'}, {...task,id:'task:rerun',label:'Wait for rerun trigger'}, {...task,id:'task:run-end',label:'Wait for run end and rerun failed jobs'}];
 const {container}=render(()=><AgentActivity running items={[...waits,{...task,id:'task:build',label:'Build compiler'}]}/>);
 expect(screen.getByText('Waiting for CI')).toBeTruthy();
 expect(screen.queryByText('Build compiler')).toBeNull();
 fireEvent.click(screen.getByText('Waiting for CI'));
 expect(screen.getByText('Build compiler')).toBeTruthy();
 expect(container.querySelectorAll('.agent-history-entries .agent-task')).toHaveLength(5);
 expect(isCiWait({...task,label:'Wait for user input'})).toBe(false);
 expect(isCiWait({...task,label:'Build CI binaries'})).toBe(false);
});
