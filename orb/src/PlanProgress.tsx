import {createEffect,createSignal,For,Show,onCleanup} from 'solid-js';
import {connectionVersion} from './api';
import {sideQuestionKey} from './sideQuestionStorage';
import {readSideThread,saveSideThread} from './composerDrafts';
import {MdView} from './Markdown';
import type {StreamItem} from './transcriptModel';
export type ApprovedPlan={requestId:string;text:string;approvedAt:string;boundary?:string};
const key=(mission:string)=>'plan:'+sideQuestionKey(mission);
const [revision,setRevision]=createSignal(0);
const sessionPlans=new Map<string,ApprovedPlan>();
const failedSaves=new Set<string>();
export async function rememberApprovedPlan(mission:string,plan:ApprovedPlan){
 const id=key(mission);sessionPlans.set(id,plan);
 try{await saveSideThread(id,plan);failedSaves.delete(id);}catch{failedSaves.add(id);}
 setRevision(v=>v+1);
}
export function planSteps(items:StreamItem[],boundary?:string){
 const start=boundary?items.findIndex(i=>i.key===boundary):-1;
 if(boundary&&start<0)return [];
 for(const item of items.slice(start+1).reverse()){
  if(item.kind!=='tool'||!['TodoWrite','todowrite','update_plan'].includes(item.name))continue;
  let args:any=item.args;
  try{if(typeof args==='string')args=JSON.parse(args);}catch{continue;}
  const rows=args?.todos??args?.plan;
  if(Array.isArray(rows))return rows.filter(r=>r&&typeof(r.content??r.step)==='string').map(r=>({text:r.content??r.step,status:String(r.status??'pending')}));
 }
 return [];
}
export function recoverPlan(items:StreamItem[]):ApprovedPlan|undefined {
 let marker:Extract<StreamItem,{kind:'user'}>|undefined;
 for(const item of items)if(item.kind==='user'&&!item.queued&&/^\s*\/plan(?:\s|$)/.test(item.text))marker=item;
 if(!marker)return;
 const later=items.slice(items.indexOf(marker)+1);
 for(const item of [...later].reverse()){
  if(item.kind!=='tool')continue;
  let args:any=item.args;
  try{if(typeof args==='string')args=JSON.parse(args);}catch{continue;}
  if(item.name==='ExitPlanMode'||(item.name==='ui_native_request'&&args?.method==='plan')){
   const text=args?.plan??args?.params?.plan;
   if(typeof text==='string'&&text.trim())return {requestId:item.callId,text,approvedAt:'',boundary:item.key};
  }
 }
 return {requestId:marker.key,text:marker.text.replace(/^\s*\/plan\s*/,''),approvedAt:'',boundary:marker.key};
}
// Plan tracking belongs to its conversation turn. A historical /plan marker
// must not label every later agent run as plan execution.
export function planBelongsToCurrentTurn(plan:ApprovedPlan,items:StreamItem[]):boolean {
 if(!plan.boundary)return true;
 const boundary=items.findIndex(item=>item.key===plan.boundary);
 if(boundary<0)return true; // A partial transcript cannot establish a new turn.
 return !items.slice(boundary+1).some(item=>item.kind==='user'&&!item.queued);
}
export function createPlanProgress(p:{mission:string;items:StreamItem[];active:boolean}){
 const [plan,setPlan]=createSignal<ApprovedPlan>();
 createEffect(()=>{
  connectionVersion();revision();const current=key(p.mission);let live=true;setPlan(undefined);
  void readSideThread<ApprovedPlan>(current).then(value=>{if(live)setPlan(sessionPlans.get(current)??value);}).catch(()=>{if(live)setPlan(sessionPlans.get(current));});
  onCleanup(()=>{live=false;});
 });
 const current=()=>{const recovered=recoverPlan(p.items);const saved=plan();
  if(recovered&&saved?.boundary){const old=p.items.findIndex(i=>i.key===saved.boundary);const latest=p.items.findIndex(i=>i.key===recovered.boundary);if(latest>old&&old>=0)return recovered;}
  return saved??recovered;};
 const steps=()=>planSteps(p.items,current()?.boundary);
 const completed=()=>steps().filter(s=>s.status==='completed').length;
 const label=()=>steps().length&&completed()===steps().length?'Steps reported complete':p.active?'In progress':'Execution stopped';
 return () => { const saved=current(); return saved && planBelongsToCurrentTurn(saved,p.items) ? {plan:saved,steps:steps(),completed:completed(),label:label(),saveFailed:failedSaves.has(key(p.mission))} : undefined; };
}
export type PlanProgressData = NonNullable<ReturnType<ReturnType<typeof createPlanProgress>>>;
export function PlanDetails(p:{data:PlanProgressData}){
 return <section class="plan-progress">
  <div class="plan-progress-heading">Plan · {p.data.label}<Show when={p.data.steps.length}> · {p.data.completed}/{p.data.steps.length}</Show></div>
  <div class="plan-progress-body">
   <Show when={p.data.saveFailed}><p role="alert">Plan accepted, but local storage failed. Tracking may be lost on refresh.</p></Show>
   <small>{p.data.plan.approvedAt?`Approved in Orb · ${new Date(p.data.plan.approvedAt).toLocaleString()}`:'Recovered from conversation · approval not recorded'}</small>
   <Show when={p.data.steps.length} fallback={<p class="dim">Progress not reported by the agent.</p>}>
    <ol><For each={p.data.steps}>{step=><li><span>{step.status==='completed'?'✓':step.status==='in_progress'?'◐':step.status==='blocked'?'!':'○'}</span> {step.text} <small>{step.status.replaceAll('_',' ')}</small></li>}</For></ol>
   </Show>
   <details><summary>{p.data.plan.approvedAt?'Approved plan':'Plan / original request'}</summary><MdView text={p.data.plan.text} compact/></details>
  </div>
 </section>;
}
export function PlanProgress(p:{mission:string;items:StreamItem[];active:boolean}){
 const data=createPlanProgress(p);
 return <Show when={data()}>{value=><PlanDetails data={value()}/>}</Show>;
}
