import {rememberApprovedPlan} from "./PlanProgress";
import {For,Show,createEffect,createSignal,onCleanup,untrack} from 'solid-js';
import {MdView} from './Markdown';
import {api,connectionVersion} from './api';
import {observeMissionInteraction} from './missionAttention';
import type {StreamItem} from './transcriptModel';

type Request={id:string;method:string;params:{plan?:string;tool?:string;input?:{command?:string;file_path?:string;description?:string};questions?:Array<{id?:string;question:string;header?:string;multiSelect?:boolean;options?:Array<{label:string;description?:string}>}>}};
const invoke=(command:string,args:Record<string,unknown>) => {
 const host=window as unknown as {__TAURI_INTERNALS__?:{invoke:(cmd:string,args:Record<string,unknown>)=>Promise<unknown>}};
 if(!host.__TAURI_INTERNALS__) return Promise.reject(new Error('Open this session in Orb.'));
 return host.__TAURI_INTERNALS__.invoke(command,args);
};
export function NativeInteraction(p:{mission:string;active:boolean;remote?:boolean;items?:StreamItem[]}) {
 const [request,setRequest]=createSignal<Request|null>(null);
 const [answers,setAnswers]=createSignal<Record<string,string[]>>({});
 const [feedback,setFeedback]=createSignal('');
 const [sending,setSending]=createSignal(false);
 const [error,setError]=createSignal('');
 const answered=new Set<string>();
 let currentMission=p.mission;
 let currentConnection=connectionVersion();
 createEffect(()=>{
  connectionVersion();
  const current=request();
  if(p.active && current) onCleanup(observeMissionInteraction(p.mission,current));
 });
 createEffect(()=>{
  const id=p.mission;
  const version=connectionVersion();
  if(version!==currentConnection){answered.clear();setRequest(null);currentConnection=version;}
  if(id!==currentMission){answered.clear();setRequest(null);currentMission=id;}
  if(p.remote){
   if(!p.active){setRequest(null);return;}
   const item=p.items?.find(i=>i.kind==='tool'&&!i.done&&!answered.has(i.callId)&&['ui_native_request','AskUserQuestion','question'].includes(i.name));
   if(item?.kind==='tool') {
    const args=item.args as {method?:string;params?:Request['params'];questions?:Request['params']['questions']};
    const next:Request={id:item.callId,method:args.method??'claude_questions',params:args.params??args};
    if(next.id!==untrack(request)?.id){setAnswers({});setFeedback('');setError('');}
    setRequest(next);
   }else setRequest(null);
   return;
  }
  if(!p.active){setRequest(null);return;}
  let live=true;
  const refresh=async()=>{try{
   const next=await invoke('local_interaction',{id}) as Request|null;
   if(!live)return;
   if(next?.id!==request()?.id){setAnswers({});setFeedback('');setError('');}
   setRequest(next && !answered.has(next.id) ? next : null);
  }catch{/* Old native builds do not advertise this capability. */}};
  void refresh();const timer=setInterval(refresh,350);
  onCleanup(()=>{live=false;clearInterval(timer);});
 });
 const reply=async(action?:string)=>{
  const current=request();if(!current||sending())return;
  const selectedMission=p.mission;
  const boundary=p.items?.at(-1)?.key;
  setSending(true);setError('');
  const mapped=Object.fromEntries((current.params.questions??[]).map((q,i)=>{
   const key=q.id??String(i),values=answers()[key]??[];
   return current.method==='claude_questions'?[q.question,values.join(', ')]:[key,{answers:values}];
  }));
  try{
   const answer=action?{action,feedback:feedback()}:{answers:mapped};
   if(p.remote){
    const item=p.items?.find(i=>i.kind==='tool'&&i.callId===current.id);
    const result=await api<{delivered:boolean}>('/api/control/tool_result',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({tool_call_id:current.id,name:item?.kind==='tool'?item.name:'ui_native_request',result:answer})});
    if(!result.delivered)throw new Error('This request has expired. Resume the session to continue.');
   }else await invoke('local_interaction_answer',{id:p.mission,requestId:current.id,answer});
   if(current.method==='plan'&&action==='accept') {
    await rememberApprovedPlan(selectedMission,{requestId:current.id,text:current.params.plan??'Plan text unavailable',approvedAt:new Date().toISOString(),boundary}).catch(()=>setError('Plan accepted, but its local tracking could not be saved.'));
   }
   answered.add(current.id);
   setRequest(null);
  }
  catch(e){setError(String(e));}finally{setSending(false);}
 };
 return <Show when={request()}>{r=><section class="native-question" aria-label="Waiting for your response">
  <div class="native-question-heading">{r().method==='plan'?'Review the plan':r().method==='permission'?'Permission requested':'Questions'}</div>
  <Show when={r().method==='plan'||r().method==='permission'} fallback={<For each={r().params.questions}>{(q,i)=>{
   const key=()=>q.id??String(i());
   return <fieldset disabled={sending()}><legend>{q.question}</legend>
    <For each={q.options}>{(option,index)=><label class="native-choice"><input type={q.multiSelect?'checkbox':'radio'} name={`question-${key()}`} checked={answers()[key()]?.includes(option.label)??false} onChange={e=>setAnswers(prev=>({...prev,[key()]:q.multiSelect?(e.currentTarget.checked?[...(prev[key()]??[]),option.label]:(prev[key()]??[]).filter(v=>v!==option.label)):[option.label]}))}/><span class="native-choice-key" aria-hidden="true">{String.fromCharCode(65+index())}</span><span class="native-choice-copy"><span class="native-choice-title">{option.label}</span><Show when={option.description}><small>{option.description}</small></Show></span><span class="native-choice-check" aria-hidden="true">✓</span></label>}</For>
    <input class="s-input" aria-label={`Other answer: ${q.question}`} placeholder="Other…" value={answers()[key()]?.filter(v=>!q.options?.some(o=>o.label===v)).join(", ")??""} onInput={e=>setAnswers(prev=>({...prev,[key()]:[e.currentTarget.value]}))}/>
   </fieldset>;
  }}</For>}>
   <Show when={r().params.plan}><div class="native-plan"><MdView compact text={r().params.plan!}/></div></Show>
   <Show when={r().method==='permission'}>
    <div class="native-permission-title"><span class="p-chip">{r().params.tool ?? 'Tool action'}</span><span>{r().params.input?.description ?? 'Allow this action?'}</span></div>
    <Show when={r().params.input?.command || r().params.input?.file_path}><pre>{r().params.input?.command ?? r().params.input?.file_path}</pre></Show>
   </Show>
   <details class="native-feedback" open={r().method==='plan'}><summary>{r().method==='plan'?"Request changes":"Add feedback"}</summary><textarea aria-label="Requested changes" placeholder={r().method==='plan'?"Changes to the plan…":"Optional feedback…"} value={feedback()} onInput={e=>setFeedback(e.currentTarget.value)}/></details>
  </Show>
  <div class="native-question-actions"><Show when={r().method==='plan'||r().method==='permission'} fallback={<button class="s-btn native-primary" disabled={sending()||(r().params.questions??[]).some((q,i)=>!(answers()[q.id??String(i)]??[]).some(v=>v.trim()))} onClick={()=>void reply()}>Continue</button>}>
   <button class="s-btn native-secondary" disabled={sending()||(r().method==='plan'&&!feedback().trim())} onClick={()=>void reply('revise')}>{r().method==='plan'?'Request changes':'Decline'}</button>
   <button class="s-btn native-primary" disabled={sending()} onClick={()=>void reply('accept')}>{r().method==='plan'?'Implement plan':'Allow once'}</button>
  </Show><Show when={sending()}><span role="status">Sending…</span></Show></div>
  <Show when={error()}><div role="alert">{error()}</div></Show>
 </section>}</Show>;
}
