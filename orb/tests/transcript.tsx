import { render } from "solid-js/web";
import { createSignal } from "solid-js";
const source = new URLSearchParams(location.search).has("baseline") ? "/src/_benchmarkBefore.tsx" : "/src/Transcript.tsx";
const { Transcript, buildTranscript, applyStreamEvent } = await import(/* @vite-ignore */ source);
import type { StreamEvent } from "../src/stream";
import "../src/styles.css";
const events: StreamEvent[] = [];
for (let i=0;i<300;i++) events.push(
 {type:"user_message",data:{content:`Question ${i}`,id:`u${i}`}},
 {type:"text_delta",data:{content:`Inspect file ${i}.`}},
 {type:"tool_call",data:{tool_call_id:`t${i}`,name:"read",args:{path:`src/file${i}.ts`}}},
 {type:"tool_result",data:{tool_call_id:`t${i}`,result:"Read complete"}},
 {type:"assistant_message",data:{content:`Inspect file ${i}. Done.`,id:`a${i}`}}
);
const [items,setItems]=createSignal(buildTranscript(events));
render(()=><main id="transcript"><Transcript items={items()}/></main>,document.getElementById("root")!);
Object.assign(window,{transcriptHarness:{
 reset:(ev:StreamEvent[])=>setItems(buildTranscript(ev)),
 apply:(ev:StreamEvent)=>setItems(v=>applyStreamEvent(v,ev)),
 benchmark:async()=>{
 const renderTimes:number[]=[], replayTimes:number[]=[];
 for(let i=0;i<20;i++){
   const replayStart=performance.now(); const replay=buildTranscript(events); replayTimes.push(performance.now()-replayStart);
   const mount=document.createElement("div");document.body.append(mount);
   const start=performance.now();const dispose=render(()=><Transcript items={replay}/>,mount);void mount.offsetHeight;
   renderTimes.push(performance.now()-start);dispose();mount.remove();await new Promise(requestAnimationFrame);
 }
 renderTimes.sort((a,b)=>a-b);replayTimes.sort((a,b)=>a-b);
 const host=document.getElementById("transcript")!;
 const fold=host.querySelector(".st-work")!;
 (fold.querySelector("button") as HTMLButtonElement).click();
 let added=0,removed=0;
 const observer=new MutationObserver(records=>{for(const r of records){added+=r.addedNodes.length;removed+=r.removedNodes.length;}});
 observer.observe(host,{childList:true,subtree:true});
 const timings:number[]=[];
 for(let i=0;i<60;i++){
  const start=performance.now();
  setItems(v=>applyStreamEvent(v,{type:"text_delta",data:{content:`Live update ${i}`}}));
  void host.offsetHeight;
  timings.push(performance.now()-start);
  await new Promise(requestAnimationFrame);
 }
 observer.disconnect(); timings.sort((a,b)=>a-b);
 return {events:1500,updates:60,renderMedian:renderTimes[10],renderP95:renderTimes[19],replayMedian:replayTimes[10],replayP95:replayTimes[19],median:timings[30],p95:timings[57],added,removed,foldRetained:host.contains(fold),foldOpen:fold.classList.contains("open"),elements:host.querySelectorAll("*").length};
 }
}});
