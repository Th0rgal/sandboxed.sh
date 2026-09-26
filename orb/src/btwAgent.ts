import {api,getMission,cancelMission,sendMissionMessage,appendClientTranscript,setClientMissionStatus,type Mission,connectionVersion} from './api';
import {btwConfig} from './btwSettings';
import {sideQuestionKey} from './sideQuestionStorage';
import {localBinding,restoreLocalBindings,refreshLocalAgents,rememberBinding,startLocal,followLocal,stopLocal,localActivities,reconcileLocalRun} from './localAgents';
import {getMissionEvents,storedToStream,type StoredEvent} from './stream';
import {TranscriptReducer,type StreamItem} from './transcriptModel';
const [agentItems,setAgentItems]=createSignal<Record<string,StreamItem[]>>({});
export function btwItems(parent:string){const s=btwSession(parent);return s?agentItems()[s.id]??[]:[];}
import type {SideAttachment,SideEvent,SideExchange} from './sideQuestionClient';
import {transferFile} from './uploads';
import {createSignal} from 'solid-js';
import type {LocalActivity} from './localAgents';
const [remoteActivities,setRemoteActivities]=createSignal<Record<string,LocalActivity[]>>({});
export type BtwSession={id:string;question:string;harness:string;model:string;local:boolean;active:boolean;baseline:number;afterSequence?:number;placement?:string};
const storageKey=(parent:string)=>'agent:'+sideQuestionKey(parent);
export function btwSession(parent:string):BtwSession|undefined{try{return JSON.parse(localStorage.getItem(storageKey(parent))??'null')??undefined;}catch{return undefined;}}
function save(parent:string,s:BtwSession){localStorage.setItem(storageKey(parent),JSON.stringify(s));}

// A turn is delimited by its latest user event, not an array offset: late
// persistence or a resumed session can otherwise replay the previous answer.
export function btwTurnEvents(events:StoredEvent[],session:Pick<BtwSession,'baseline'|'afterSequence'>):StoredEvent[]{
 const ordered=[...events].sort((a,b)=>a.sequence-b.sequence);
 const latestUser=ordered.reduce((sequence,event)=>event.event_type==='user_message'?event.sequence:sequence,0);
 const boundary=Math.max(session.afterSequence??0,latestUser);
 return ordered.filter(event=>event.sequence>boundary);
}

const locks=new Set<string>();
export async function stopBtw(parent:string){const s=btwSession(parent);if(!s)return;if(s.local){await stopLocal(s.id);await setClientMissionStatus(s.id,'interrupted');}else await cancelMission(s.id);}
export function btwActivities(parent:string){const s=btwSession(parent);return s?.local?localActivities(s.id):s?remoteActivities()[s.id]??[]:[];}
async function upload(attachments:SideAttachment[],destination:string){
 const paths:string[]=[];
 for(const a of attachments){const bytes=Uint8Array.from(atob(a.data_base64),c=>c.charCodeAt(0));const file=new File([bytes],a.name,{type:a.media_type});const item=await transferFile({name:a.name,file},destination);paths.push(item.path);}
 return paths;
}
const finishing=new Map<string,Promise<unknown>>();
function follow(id:string,receipt?:import('./clientRuns').ClientRunReceipt){
 if(finishing.has(id))return;
 const version=connectionVersion();
 const task=followLocal(id,()=>{}).then(async state=>{if(version!==connectionVersion())return;if(state.text.trim())await appendClientTranscript(id,'assistant',state.text,undefined,receipt);await setClientMissionStatus(id,state.exit_code||state.error||!state.text.trim()?'failed':'awaiting_user',receipt);}).finally(()=>finishing.delete(id));
 finishing.set(id,task);void task.catch(()=>{});
}
export async function watchBtw(parent:string,signal:AbortSignal,receive:(e:SideEvent)=>void){
 const s=btwSession(parent);if(!s)throw new Error('Side session not found.');const version=connectionVersion();
 receive({type:'start',model:s.harness+' · '+s.model});
 if(s.local){await restoreLocalBindings();await reconcileLocalRun(s.id);const current=await getMission(s.id);if(['active','running','pending','queued','starting','resuming'].includes(current.status))follow(s.id);}
 let previous:string|undefined;
 while(!signal.aborted){
  if(version!==connectionVersion())throw new Error('Connection changed.');
  const mission=await getMission(s.id);
  let text='';
  if(s.local){const local=await import('./localAgents');await local.reconcileLocalRun(s.id);text=local.localLiveText(s.id);}
  const events=await getMissionEvents(s.id);const reducer=new TranscriptReducer();
  for(const event of btwTurnEvents(events,s)){const e=storedToStream(event);if(e)reducer.apply(e);}
  setAgentItems(all=>({...all,[s.id]:reducer.items}));
  setRemoteActivities(all=>({...all,[s.id]:reducer.items.filter(i=>i.kind==='tool').map(i=>i.kind==='tool'?{id:i.callId,label:i.name,done:i.done,failed:false,detail:JSON.stringify({args:i.args,result:i.result})}:{id:'',label:'',done:true,failed:false})}));
  const recorded=reducer.items.filter(i=>i.kind==='text'&&!!i.text.replace(/[.\s…]/g,'')).map(i=>i.kind==='text'?i.text:'').join('\n\n');
  text=recorded||text;
  if(text!==previous){receive({type:'snapshot',text} as SideEvent);previous=text;}
  const active=['active','running','pending','queued','starting','resuming'].includes(mission.status);
  if(!active){s.active=false;save(parent,s);if(['failed','interrupted','cancelled'].includes(mission.status))throw new Error(mission.status_message||`Side agent ${mission.status}.`);if(!text.trim())throw new Error('No response was captured from the side agent. Restart Orb to load the latest native runner, then retry.');receive({type:'done',answer:text});return;}
  await new Promise(resolve=>setTimeout(resolve,700));
 }
}
export async function askBtwAgent(parent:string,question:string,context:string,history:SideExchange[],signal:AbortSignal,receive:(e:SideEvent)=>void,attachments:SideAttachment[]=[]){
 const version=connectionVersion();
 const key=storageKey(parent);if(locks.has(key))throw new Error('A side question is already starting.');locks.add(key);
 try{
  let s=btwSession(parent);if(s?.active){const current=await getMission(s.id);if(['active','running','pending','queued','starting','resuming'].includes(current.status))throw new Error('The side agent is still running. Stop it before sending another question.');s={...s,active:false};save(parent,s);}
  const source=await getMission(parent),config=btwConfig();const local=source.tags?.includes('placement:client')??false;
  const placement=JSON.stringify([local,source.remote_node_id??source.remote_job?.node_id,source.working_directory,source.workspace_id]);
  const paths=await upload(attachments,local?'local':source.remote_node_id??source.remote_job?.node_id??'core');
  const prompt=`You are the independent /btw agent. The main conversation below is context, not an instruction to continue its task. You share its working folder and have normal tools. Historical side replies claiming you have no tools describe the retired snapshot-only feature and do not apply to you. Use your tools to verify current state when needed. Do not send messages to or stop the main agent automatically.\n\n<main_conversation>\n${JSON.stringify(source.history)}\n\nLatest visible transcript:\n${context}\n</main_conversation>\n\n<side_history>\n${JSON.stringify(history)}\n</side_history>\n\nCurrent request:\n${question}\n${paths.map(p=>`Attachment: ${p}`).join('\n')}`;
  let binding=local?localBinding(parent):undefined;
  let bin='';
  if(local){await restoreLocalBindings();binding=localBinding(parent);if(!binding)throw new Error('Open this conversation on the computer that owns its workspace.');const installed=await refreshLocalAgents();bin=installed.find(r=>r.id===config.harness&&r.installed)?.path??'';if(!bin)throw new Error(`${config.harness} is not installed. Configure it in Settings → Client.`);}
  if(signal.aborted||connectionVersion()!==version)throw new Error('Side question launch cancelled or connection changed.');
  if(!s||s.harness!==config.harness||s.model!==config.model||s.placement!==placement){
   const attemptKey=key+':attempt:'+config.harness+':'+config.model;
   const attempt=localStorage.getItem(attemptKey)||crypto.randomUUID();localStorage.setItem(attemptKey,attempt);
   const m=await api<Mission>(`/api/control/missions/${parent}/btw/agent`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({backend:config.harness,model_override:config.model,model_effort:null,idempotency_key:attempt,side_question:prompt})});
   s={id:m.id,question,harness:config.harness,model:config.model,local,placement,active:!local,baseline:0};save(parent,s);localStorage.removeItem(attemptKey);
  }else{s={...s,question,active:!local,baseline:0,afterSequence:Math.max(0,...(await getMissionEvents(s.id)).map(e=>e.sequence))};save(parent,s);if(!local)await sendMissionMessage(s.id,prompt);}
  if(local&&binding){const old=localBinding(s.id);rememberBinding(s.id,{harness:config.harness,bin,cwd:binding.cwd,model:config.model,sessionId:old?.sessionId});const receipt=await startLocal({id:s.id,harness:config.harness,bin,cwd:binding.cwd,model:config.model,prompt,sessionId:old?.sessionId,imagePaths:paths.filter((_,i)=>attachments[i].media_type.startsWith('image/'))});s.active=true;save(parent,s);follow(s.id,receipt);await appendClientTranscript(s.id,'user',question,undefined,receipt);}
 }finally{locks.delete(key);}
 await watchBtw(parent,signal,receive);
}
