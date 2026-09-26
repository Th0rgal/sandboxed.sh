import {expect,it,vi,afterEach} from 'vitest';
import {render,screen,cleanup} from '@solidjs/testing-library';
import {PlanProgress,planSteps,rememberApprovedPlan} from '../src/PlanProgress';
const store=vi.hoisted(()=>new Map());
vi.mock('../src/composerDrafts',()=>({readSideThread:async(k:string)=>store.get(k),saveSideThread:async(k:string,v:unknown)=>{store.set(k,v);}}));
afterEach(()=>{cleanup();store.clear();});
it('ignores tasks before approval and preserves unknown status',()=>{
 expect(planSteps([{kind:'tool',key:'old',callId:'old',name:'TodoWrite',args:{todos:[{content:'old',status:'completed'}]},done:true}], 'old')).toEqual([]);
 expect(planSteps([{kind:'tool',key:'new',callId:'new',name:'update_plan',args:{plan:[{step:'Build',status:'in_progress'}]},done:false}])).toEqual([{text:'Build',status:'in_progress'}]);
});
it('restores an approved plan without declaring an idle run complete',async()=>{
 await rememberApprovedPlan('test',{requestId:'r',text:'Review this plan',approvedAt:new Date().toISOString()});
 const mount=()=>render(()=><PlanProgress mission="test" items={[]} active={false}/>);
 mount();await screen.findByText(/Execution stopped/);cleanup();mount();
 await screen.findByText(/Execution stopped/);
 expect(screen.getByText('Progress not reported by the agent.')).toBeTruthy();
});
it('shows a pre-existing plan even without a saved approval receipt',async()=>{
 render(()=><PlanProgress mission="legacy" active items={[
 {kind:'user',key:'request',text:'/plan Improve the spec'},
 {kind:'tool',key:'tasks',callId:'tasks',name:'TodoWrite',args:{todos:[{content:'Typed accessors',status:'in_progress'}]},done:true}
 ]}/>);
 await screen.findByText(/approval not recorded/);
 expect(screen.getByText('Typed accessors',{exact:false})).toBeTruthy();
 expect(screen.getByText('Improve the spec')).toBeTruthy();
});

it('hides a historical plan when an ordinary follow-up starts, including after reload',async()=>{
 const items:any[]=[{kind:'user',key:'plan',text:'/plan Improve the spec'},
 {kind:'text',key:'result',text:'The plan has been implemented.',live:false},
 {kind:'user',key:'question',text:'Why is there a Proof folder?'},
 {kind:'text',key:'answer',text:'The folder contains lemmas.',live:true}];
 const mount=()=>render(()=><PlanProgress mission="normal-followup" items={items} active/>);
 mount();await new Promise(resolve=>setTimeout(resolve,0));
 expect(screen.queryByText(/Plan ·/)).toBeNull();cleanup();mount();
 await new Promise(resolve=>setTimeout(resolve,0));expect(screen.queryByText(/Plan ·/)).toBeNull();
});
it('keeps tracking while a follow-up is only queued and starts fresh for a new plan',async()=>{
 const items:any[]=[{kind:'user',key:'plan',text:'/plan First plan'},
 {kind:'user',key:'queued',text:'A later question',queued:true}];
 const view=render(()=><PlanProgress mission="queued-plan" items={items} active/>);
 await screen.findByText('First plan');view.unmount();
 render(()=><PlanProgress mission="new-plan" items={[...items,{kind:'user',key:'next',text:'/plan Second plan'}]} active/>);
 await screen.findByText('Second plan');expect(screen.queryByText('First plan')).toBeNull();
});
it('does not resurrect a saved approved plan after a normal question',async()=>{
 await rememberApprovedPlan('approved-followup',{requestId:'approval',text:'Saved plan',approvedAt:new Date().toISOString(),boundary:'approval'});
 render(()=><PlanProgress mission="approved-followup" active items={[
 {kind:'tool',key:'approval',callId:'approval',name:'ExitPlanMode',args:{plan:'Saved plan'},done:true},
 {kind:'user',key:'ordinary',text:'Explain the result'}]}/>);
 await new Promise(resolve=>setTimeout(resolve,0));expect(screen.queryByText(/Plan ·/)).toBeNull();
});
