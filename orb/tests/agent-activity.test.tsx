import {createSignal} from 'solid-js';
import {describe,it,expect,afterEach} from 'vitest';
import {render,screen,cleanup,fireEvent} from '@solidjs/testing-library';
import {AgentActivity,activityDuration,activityState} from '../src/AgentActivity';
import type {LocalActivity} from '../src/localAgents';
afterEach(cleanup);
const task:LocalActivity={id:'task:a',label:'Inspect the importer',kind:'agent',background:true,tool_use_id:'spawn',done:false,failed:false,started_at:1000,updated_at:4000};
describe('agent activity',()=>{
 it('keeps live tasks visible and hides the tool call that spawned them',()=>{
  render(()=><AgentActivity running items={[{id:'spawn',label:'Agent',done:true,failed:false},task,{...task,id:'task:b',label:'Inventory docs',done:true,status:'completed'}]}/>);
  expect(screen.getByText('Inspect the importer')).toBeTruthy();
  expect(screen.queryByText('Agent')).toBeNull();
  expect(screen.getByText('1 completed task')).toBeTruthy();
  expect(screen.getByText('Inventory docs').closest('.agent-activity-history')?.hasAttribute('open')).toBe(false);
 });
 it('keeps failures visible and exposes their reported details',()=>{
  render(()=><AgentActivity running={false} items={[{...task,done:true,failed:true,status:'failed',detail:'Build exited with code 1'}]}/>);
  fireEvent.click(screen.getByText('Inspect the importer'));
  expect(screen.getByText('Failed')).toBeTruthy();
  expect(screen.getByText('Build exited with code 1').closest('details')?.open).toBe(true);
 });
 it('keeps expanded details open across native polling snapshots',()=>{
  const [items,setItems]=createSignal<LocalActivity[]>([{...task,detail:'Reading files'}]);
  render(()=><AgentActivity running items={items()}/>);
  fireEvent.click(screen.getByText('Inspect the importer'));
  const details=screen.getByText('Inspect the importer').closest('details');
  expect(details?.open).toBe(true);
  setItems([{...task,detail:'Inspecting tests'}]);
  expect(screen.getByText('Inspecting tests').closest('details')).toBe(details);
  expect(details?.open).toBe(true);
  setItems([]);
  expect(screen.queryByRole('region',{name:'Agent activity'})).toBeNull();
 });
 it('does not invent durations or success for old or incomplete results',()=>{
  expect(activityDuration({...task,started_at:undefined},5000,true)).toBe('');
  expect(activityDuration({...task,done:true,finished_at:62000},100000,true)).toBe('1m 1s');
  expect(activityState(task,false)).toBe('No result recorded');
  expect(activityState({...task,done:true,status:'finished'},false)).toBe('Finished');
 });
});
