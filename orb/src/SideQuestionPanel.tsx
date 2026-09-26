import {NativeInteraction} from "./NativeInteraction";
import {askBtwAgent,watchBtw,stopBtw,btwSession,btwActivities,btwItems} from "./btwAgent";
import {btwConfig} from "./btwSettings";
import {AgentActivity} from "./AgentActivity";
import { Composer } from "./App";
import type { DraftImage } from "./imageAttachments";
import { uploadToken, type UploadedFile } from "./uploads";
import { Portal } from 'solid-js/web';
import { useSidePanel } from './FilePanel';
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { connectionVersion } from './api';
import { readSideQuestion, writeSideQuestion, sideQuestionKey } from './sideQuestionStorage';
import { UserTurn } from "./Transcript";
import { MdView } from './Markdown';
import { askSide, sideContext, sideAttachments, type SideAttachment, type SideExchange } from './sideQuestionClient';
import type { StreamItem } from './transcriptModel';
import * as Ic from './icons';
export type SideQuestionsHandle={ask:(question:string,images?:DraftImage[],files?:UploadedFile[])=>Promise<boolean>;open:()=>void};
// Local side history stays separate from mission events and the main draft.
export function SideQuestions(p:{mission:string;items:StreamItem[];ref:(handle:SideQuestionsHandle)=>void;onTransfer:(text:string)=>void}) {
 const side=useSidePanel();
 const [docked,setDocked]=createSignal(false);
 const key=()=>{connectionVersion();return sideQuestionKey(p.mission);};
 const [history,setHistory]=createSignal<SideExchange[]>([]),[open,setOpen]=createSignal(false),[busy,setBusy]=createSignal(false);
 const [question,setQuestion]=createSignal(''),[answer,setAnswer]=createSignal(''),[draft,setDraft]=createSignal(''),[error,setError]=createSignal(''),[model,setModel]=createSignal('');
 let abort:AbortController|undefined;
 let scroll:HTMLDivElement|undefined;
 createEffect(()=>{answer();history();if(scroll&&scroll.scrollHeight-scroll.scrollTop-scroll.clientHeight<120)queueMicrotask(()=>{if(scroll)scroll.scrollTop=scroll.scrollHeight;});});
 const [storageError,setStorageError]=createSignal(false);
 const [pendingAttachments,setPendingAttachments]=createSignal<SideAttachment[]>([]);
 const [loadedKey,setLoadedKey]=createSignal('');
 let ready:Promise<void>=Promise.resolve();
 createEffect(on(key,current=>{
  abort?.abort();setLoadedKey('');setBusy(false);setHistory([]);setError('');
  let stale=false;onCleanup(()=>{stale=true;});
  ready=readSideQuestion(current).then(saved=>{
  if(stale)return;
  setHistory(saved?.history??[]);setBusy(false);setOpen(saved?.open??false);
  setDocked(saved?.docked??false);setQuestion(saved?.pending?.question??'');
  setAnswer(saved?.pending?.answer??'');
  setError(saved?.pending ? saved.pending.error || 'Side question interrupted. Retry to request a complete answer.' : '');
  setPendingAttachments(saved?.pending?.attachments??[]);setDraft(saved?.draft??'');setModel(saved?.model??'');setStorageError(false);
  setLoadedKey(current);
  const agent=btwSession(p.mission);
  const lastAnswer=saved?.history.at(-1)?.answer;
  const emptyReply=lastAnswer!==undefined&&(lastAnswer==='The side agent finished without a text response.'||!lastAnswer.replace(/[.\s…]/g,''));
  if(emptyReply){setHistory(rows=>rows.slice(0,-1));setQuestion(saved!.history.at(-1)!.question);setAnswer('');}
  if(agent&&(agent.active||emptyReply||saved?.pending)){
   const controller=new AbortController();abort=controller;setBusy(true);setQuestion(agent.question);setError('');
   void watchBtw(p.mission,controller.signal,event=>{
    if(stale||controller.signal.aborted)return;
    if(event.type==='start')setModel(event.model);
    if(event.type==='snapshot')setAnswer(event.text);
    if(event.type==='done'){setHistory(rows=>[...rows,{question:agent.question,answer:event.answer}].slice(-20));setBusy(false);}
   }).catch(e=>{if(!stale&&!controller.signal.aborted){setError(String(e));setBusy(false);}});
  }
  if(saved?.open&&saved.docked)queueMicrotask(()=>{if(loadedKey()===current)side?.show();});
  });
 }));
 createEffect(()=>{
  const current=key();
  const snapshot={history:history(),draft:draft(),model:model(),open:docked()&&side?side.visible():open(),docked:docked(),
    pending:(busy()||error())?{question:question(),answer:answer(),error:error(),attachments:pendingAttachments()}:undefined};
  if(loadedKey()===current)void writeSideQuestion(current,snapshot).then(saved=>{if(key()===current)setStorageError(!saved);});
 });
 side?.register(()=>{setDocked(true);setOpen(true);});
 onCleanup(()=>{abort?.abort();side?.register(undefined);});
 const ask=async(text:string,images:DraftImage[]=[],files:UploadedFile[]=[],retryAttachments?:SideAttachment[])=>{
  text=text.trim();if(!text||busy())return false;
  const selected=key();await ready;if(selected!==key()||busy())return false;
  const current=key(),context=sideContext(p.items),controller=new AbortController();abort=controller;
  let attachments:SideAttachment[];
  setBusy(true);
  try { attachments=retryAttachments??await sideAttachments(images,files); }
  catch(e){setBusy(false);throw e;}
  if(controller.signal.aborted||current!==key())return false;
  for(const file of files)text=text.replaceAll(uploadToken(file.path),`[File: ${file.source.name}]`);
  setPendingAttachments(attachments);
  setOpen(true);if(docked())side?.show();setBusy(true);setQuestion(text);setAnswer('');setError('');setDraft('');
  void askBtwAgent(p.mission,text,context,history(),controller.signal,event=>{
   if(current!==key()||controller.signal.aborted)return;
   if(event.type==='start')setModel(event.model);
   if(event.type==='snapshot')setAnswer(event.text);
   if(event.type==='delta')setAnswer(value=>value+event.text);
   if(event.type==='done'){
    setAnswer(event.answer);
    const next=[...history(),{question:text,answer:event.answer,attachments}].slice(-20);
    setHistory(next);setBusy(false);
   }
  },attachments).catch(e=>{if(current===key()&&!controller.signal.aborted)setError(e instanceof Error?e.message:String(e));})
  .finally(()=>{if(current===key()&&abort===controller)setBusy(false);});
  return true;
 };
 const reveal=()=>{setOpen(true);if(docked())side?.show();};
 p.ref({ask,open:reveal});
 const escape=(event:KeyboardEvent)=>{
  if(event.key==='Escape'&&!event.defaultPrevented&&open()&&!docked()){
   event.preventDefault();setOpen(false);
  }
 };
 onMount(()=>window.addEventListener('keydown',escape));
 onCleanup(()=>window.removeEventListener('keydown',escape));
 const cancel=()=>{void stopBtw(p.mission).then(()=>{abort?.abort();setBusy(false);setError('Side agent stopped.');}).catch(e=>setError(String(e)));};
 let inline!:HTMLDivElement;
 return <>
  <div ref={inline}/>
  <Show when={!side&&!open()&&(history().length||busy()||error())}><button class="btw-reopen" onClick={reveal}>Side questions {busy()?'· Answering…':`· ${history().length}`}</button></Show>
  <Show when={open()}><Portal mount={docked() ? side?.target() : inline}><section class="btw-panel" aria-label="Side questions">
   <header><div><strong>Side question</strong></div><div class="btw-actions"><Show when={side}><button class="icon-btn" aria-label={docked()?"Move side question below conversation":"Move side question to right panel"} title={docked()?"Move below conversation":"Move to right panel"} onClick={()=>{if(docked()){setDocked(false);side?.hide();}else{setDocked(true);side?.show();}}}><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/><path d={docked()?"m11 9-3 3 3 3":"m8 9 3 3-3 3"}/></svg></button></Show><button class="icon-btn" aria-label="Close side questions" onClick={()=>{setOpen(false);if(docked())side?.hide();}}><Ic.CloseIcon size={16}/></button></div></header>
   <div class="btw-thread" ref={scroll}>
    <For each={history()}>{exchange=><article><UserTurn text={exchange.question} onSend={text=>ask(text,[],[],exchange.attachments??[])}/><MdView compact text={exchange.answer}/><button class="btw-transfer" onClick={()=>p.onTransfer(`About this side question: ${exchange.question}\n\n${exchange.answer}`)}>Use in agent draft ↗</button></article>}</For>
    <Show when={busy()||error()}><article><UserTurn text={question()} onSend={text=>ask(text,[],[],pendingAttachments())}/><Show when={answer()}><MdView compact text={answer()}/></Show><Show when={busy()}><p class="dim" role="status">Side agent is working…</p></Show><Show when={error()}><p role="alert" class="error">{error()}</p><button onClick={()=>void ask(question(),[],[],pendingAttachments())} disabled={busy()}>Retry</button></Show></article></Show>
    <Show when={!history().length&&!question()}><p class="dim">Ask a question or give the side agent a task. It shares the main agent’s workspace.</p></Show>
    <AgentActivity items={btwActivities(p.mission)} running={busy()}/>
    <Show when={btwSession(p.mission) && busy()}><NativeInteraction mission={btwSession(p.mission)!.id} active={busy()} remote={!btwSession(p.mission)!.local} items={btwItems(p.mission)}/></Show>
   </div>
   <Show when={storageError()}><p class="dim" role="status">Local storage is unavailable. This side conversation may be lost on refresh.</p></Show>
   <div class="btw-composer-dock"><Composer sideQuestion picker={false} placeholder="Ask a side question…" busy={busy()} scope={key()} uploadTarget="side" onDraft={setDraft} onSend={(text,images,files)=>ask(text,images,files)} onStop={cancel}/><div class="btw-footer"><span>/btw</span><span aria-hidden="true">·</span><span title="Independent agent sharing the main workspace">{model()||`${btwConfig().harness} · ${btwConfig().model}`}</span></div></div>
  </section></Portal></Show>
 </>;
}
