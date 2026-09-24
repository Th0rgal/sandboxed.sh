import { createEffect, createSignal, For, onCleanup, Show } from 'solid-js';
import { connectionVersion } from './api';
import { MdView } from './Markdown';
import { askSide, sideContext, type SideExchange } from './sideQuestionClient';
import type { StreamItem } from './transcriptModel';
import * as Ic from './icons';
export type SideQuestionsHandle={ask:(question:string)=>boolean;open:()=>void};
// Separate, session-only history. Never enters mission events or the main draft.
const threads=new Map<string,SideExchange[]>();
let threadConnection=-1;
export function SideQuestions(p:{mission:string;items:StreamItem[];ref:(handle:SideQuestionsHandle)=>void;onTransfer:(text:string)=>void}) {
 const key=()=>`${connectionVersion()}:${p.mission}`;
 const [history,setHistory]=createSignal<SideExchange[]>([]),[open,setOpen]=createSignal(false),[busy,setBusy]=createSignal(false);
 const [question,setQuestion]=createSignal(''),[answer,setAnswer]=createSignal(''),[draft,setDraft]=createSignal(''),[error,setError]=createSignal(''),[model,setModel]=createSignal('');
 let abort:AbortController|undefined;
 let scroll:HTMLDivElement|undefined;
 createEffect(()=>{answer();history();if(scroll&&scroll.scrollHeight-scroll.scrollTop-scroll.clientHeight<120)queueMicrotask(()=>{if(scroll)scroll.scrollTop=scroll.scrollHeight;});});
 createEffect(()=>{const current=key();if(threadConnection!==connectionVersion()){threads.clear();threadConnection=connectionVersion();}abort?.abort();setHistory(threads.get(current)??[]);setBusy(false);setOpen(false);setQuestion('');setAnswer('');setError('');setDraft('');setModel('');});
 onCleanup(()=>abort?.abort());
 const ask=(text:string)=>{
  text=text.trim();if(!text||busy())return false;
  if(text.length>2000){setOpen(true);setError('Keep side questions under 2,000 characters.');return false;}
  const current=key(),context=sideContext(p.items),controller=new AbortController();abort=controller;
  setOpen(true);setBusy(true);setQuestion(text);setAnswer('');setError('');setDraft('');
  void askSide(p.mission,text,context,history(),controller.signal,event=>{
   if(current!==key()||controller.signal.aborted)return;
   if(event.type==='start')setModel(event.model);
   if(event.type==='delta')setAnswer(value=>value+event.text);
   if(event.type==='done'){
    setAnswer(event.answer);
    const next=[...history(),{question:text,answer:event.answer}].slice(-20);
    setHistory(next);threads.set(current,next);
   }
  }).catch(e=>{if(current===key()&&!controller.signal.aborted)setError(e instanceof Error?e.message:String(e));})
  .finally(()=>{if(current===key()&&abort===controller)setBusy(false);});
  return true;
 };
 p.ref({ask,open:()=>setOpen(true)});
 const cancel=()=>{abort?.abort();setBusy(false);setError('Side question cancelled. The agent continues working.');};
 return <>
  <Show when={!open()&&(history().length||busy()||error())}><button class="btw-reopen" onClick={()=>setOpen(true)}>Side questions {busy()?'· Answering…':`· ${history().length}`}</button></Show>
  <Show when={open()}><section class="btw-panel" aria-label="Side questions">
   <header><div><strong>Side question</strong><span>Conversation snapshot · No tools{model()?` · ${model()}`:''}</span></div><button class="icon-btn" aria-label="Close side questions" onClick={()=>setOpen(false)}><Ic.CloseIcon size={16}/></button></header>
   <div class="btw-thread" ref={scroll}>
    <For each={history()}>{exchange=><article><p class="btw-question">{exchange.question}</p><MdView compact text={exchange.answer}/><button class="btw-transfer" onClick={()=>p.onTransfer(`About this side question: ${exchange.question}\n\n${exchange.answer}`)}>Use in agent draft ↗</button></article>}</For>
    <Show when={busy()||error()}><article><p class="btw-question">{question()}</p><Show when={answer()}><MdView compact text={answer()}/></Show><Show when={busy()}><p class="dim" role="status">Answering from the conversation…</p></Show><Show when={error()}><p role="alert" class="error">{error()}</p><button onClick={()=>ask(question())} disabled={busy()}>Retry</button></Show></article></Show>
    <Show when={!history().length&&!question()}><p class="dim">Ask about what the agent has already read or done. Its work continues uninterrupted.</p></Show>
   </div>
   <form onSubmit={e=>{e.preventDefault();ask(draft());}}><textarea aria-label="Follow-up side question" placeholder="Ask a side question…" value={draft()} onInput={e=>setDraft(e.currentTarget.value)} rows={1} maxLength={2000} onKeyDown={e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing){e.preventDefault();ask(draft());}}}/><Show when={busy()} fallback={<button type="submit" disabled={!draft().trim()} aria-label="Send side question"><Ic.ArrowUpIcon size={16}/></button>}><button type="button" onClick={cancel} aria-label="Cancel side question"><Ic.StopIcon size={14}/></button></Show></form>
  </section></Show>
 </>;
}
