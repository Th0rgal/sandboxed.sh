import {For,Show,createSignal} from 'solid-js';
import {queuedLocalMessages,takeQueuedMessage,removeQueuedMessage,retryQueuedMessage,sendQueuedNow} from './localMessageQueue';
import * as Ic from './icons';
export function QueuedMessages(p:{mission:string;onEdit?:(text:string)=>void}){
 const rows=()=>queuedLocalMessages(p.mission).filter(row=>row.waiting&&row.state!=='accepted'&&row.state!=='dispatching'||!!row.error);
 const [working,setWorking]=createSignal(false);
 const [menu,setMenu]=createSignal(false),[error,setError]=createSignal('');
 const act=async(fn:()=>Promise<unknown>)=>{if(working())return;try{setWorking(true);setError('');await fn();}catch(e){setError(String(e));}finally{setWorking(false);}};
 return <Show when={rows().length}><section class="followup-queue" aria-label="Queued messages" aria-live="polite">
  <header><span>{rows().length} Queued</span><span class="queue-hint">After this turn</span><div class="queue-options"><button class="queue-action" disabled={working()||!rows().some(row=>row.state==='queued')} onClick={()=>void act(()=>sendQueuedNow(p.mission))}>Send now</button><button class="queue-menu-toggle" aria-label="Queue options" aria-expanded={menu()} onClick={()=>setMenu(v=>!v)}><Ic.ChevronDown/></button><Show when={menu()}><div class="queue-menu">Messages send one at a time after the current turn. Send now stops the current turn first.</div></Show></div></header>
  <ol><For each={rows()}>{row=><li><span>{row.text}</span><Show when={p.onEdit}><button class="queue-remove queue-edit" title="Edit message" disabled={working()||row.state==='dispatching'||row.state==='accepted'} aria-label={`Edit queued message: ${row.text}`} onClick={()=>void act(async()=>{const text=await takeQueuedMessage(row.id);p.onEdit?.(text);})}><Ic.PencilIcon size={12}/></button></Show><button class="queue-remove" disabled={working()||row.state==='dispatching'||row.state==='accepted'} aria-label={`Remove queued message: ${row.text}`} onClick={()=>void act(()=>removeQueuedMessage(row.id))}><Ic.CloseIcon size={12}/></button><Show when={row.state==='dispatching'}><small>{row.error?'Needs review':'Sending…'}</small></Show><Show when={row.error}><small role="alert">{row.error}</small><Show when={row.state==='error'||row.state==='dispatching'}><button onClick={()=>void act(()=>retryQueuedMessage(row.id))}>Retry</button></Show></Show></li>}</For></ol>
  <Show when={error()}><p role="alert">{error()}</p></Show>
 </section></Show>;
}
