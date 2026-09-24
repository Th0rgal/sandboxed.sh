import {createSignal,onCleanup,onMount,Show} from 'solid-js';
import {contextConflicts} from './projectContext';
import {getApiUrl,getJwt,connectionVersion} from './api';
import {nativeInvoke} from './clientRuns';
export function ContextBadge(p:{slug:string}){
 const[label,setLabel]=createSignal(''),[detail,setDetail]=createSignal(''),[expanded,setExpanded]=createSignal(false);
 let disposed=false;
 const refresh=async()=>{
  const version=connectionVersion(),slug=p.slug;
  let label='',detail='';
  try {
   const conflicts=await contextConflicts(p.slug);const count=Object.keys(conflicts).length;
   if(count){label=`${count} context conflict${count===1?'':'s'}`;detail=Object.values(conflicts).map(op=>op.path).join('\n');}
  }catch{/* Old servers have no context status. */}
  const invoke=nativeInvoke();
  if(invoke)try{
   const result=await invoke('project_context_status',{request:{endpoint:getApiUrl(),token:getJwt()??'',project:p.slug}}) as {state:{initialized:boolean;pending:unknown[];error?:string}};
   if(result.state.initialized && result.state.error){label=label||'Context sync pending';detail=[detail,result.state.error,`${result.state.pending.length} local changes queued`].filter(Boolean).join('\n');}
   else if(result.state.pending.length){label=label||'Syncing context…';}
  }catch{/* Native capability may not yet be installed. */}
  if(!disposed && version===connectionVersion() && slug===p.slug){setLabel(label);setDetail(detail);}
 };
 onMount(()=>{void refresh();const timer=setInterval(()=>void refresh(),5000);onCleanup(()=>{disposed=true;clearInterval(timer);});});
 return <Show when={label()}><div class="context-sync-state"><button class="s-btn" aria-expanded={expanded()} onClick={event=>{event.stopPropagation();setExpanded(!expanded());}}>{label()}</button><Show when={expanded()}><div class="context-sync-detail">{detail()||'Changes are being synchronized.'}</div></Show></div></Show>;
}
