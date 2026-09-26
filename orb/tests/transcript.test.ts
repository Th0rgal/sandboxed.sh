import { describe,it,expect } from "vitest";
import { buildTranscript,applyStreamEvent } from "../src/Transcript";
import { storedToStream,heldAfterHistory,type StreamEvent } from "../src/stream";
const ev=(type:string,data:Record<string,unknown>):StreamEvent=>({type,data});
const snap=(content:string)=>ev("text_delta",{content});
const tool=ev("tool_call",{tool_call_id:"a",name:"read"});
const result=ev("tool_result",{tool_call_id:"a",result:"done"});
const final=(content:string,id="final")=>ev("assistant_message",{content,id});
const texts=(items:ReturnType<typeof buildTranscript>)=>items.filter(x=>x.kind==="text");
describe("producer transcript contract",()=>{
 it("updates one cumulative response across tools in replay and live",()=>{
  const events=[snap("Inspect file."),tool,result,snap("Inspect file. Fix guard.")];
  const replay=buildTranscript(events);
  let live=buildTranscript([]);for(const e of events)live=applyStreamEvent(live,e);
  expect(live).toEqual(replay);expect(texts(live)).toHaveLength(1);expect(texts(live)[0].text).toBe("Inspect file. Fix guard.");
  live=applyStreamEvent(live,final("Done."));expect(texts(live)).toMatchObject([{text:"Done.",live:false}]);
 });
 it("keys independent bubbles and uses Unicode scalar offsets for true operations",()=>{
  let items=buildTranscript([ev("text_op",{bubble_id:"one",ops:[{type:"insert",pos:0,text:"A😀B"}]}),tool,ev("text_op",{bubble_id:"two",ops:[{type:"insert",pos:0,text:"Other"}]})]);
  items=applyStreamEvent(items,ev("text_op",{bubble_id:"one",ops:[{type:"replace",range:[1,2],text:"🌙"},{type:"insert",pos:3,text:"!"},{type:"finalize"}]}));
  expect(texts(items)).toMatchObject([{text:"A🌙B!",live:false},{text:"Other",live:true}]);
 });
 it("handles finalization followed by final history without losing genuine repeats",()=>{
  const items=buildTranscript([snap("Same"),tool,ev("text_op",{ops:[{type:"finalize"}]}),final("Same","first"),final("Same","first"),final("Same","second")]);
  expect(texts(items)).toHaveLength(2);expect(texts(items).every(x=>!x.live)).toBe(true);
 });
 it("keeps true deltas, and reconnect snapshot insert replaces the same bubble",()=>{
  const items=buildTranscript([snap("A"),ev("text_delta",{content:"B",mode:"delta"}),tool,ev("text_op",{bubble_id:"text_delta_latest",ops:[{type:"insert",pos:0,text:"AB updated"}]})]);
  expect(texts(items)).toMatchObject([{text:"AB updated"}]);
 });
  it("hides generated remote-job status rows and keeps ordinary Remote job prose",()=>{
    const row=(content:string,metadata?:Record<string,unknown>)=>({id:1,sequence:1,event_type:"assistant_message",content,timestamp:"",metadata});
    expect(storedToStream(row("Remote job e14b14a3-bce7-4d80-8d81-ad7ad239fd82 on node 'dgx-spark' is now running"))).toBeNull();
    expect(storedToStream(row("Dispatched job e14b14a3-bce7-4d80-8d81-ad7ad239fd82 to remote node 'dgx-spark' (grok/grok-4.6; node state: queued)"))).toBeNull();
    expect(storedToStream(row("Remote job scheduling is now fixed on the node."))).toMatchObject({type:"assistant_message",data:{content:"Remote job scheduling is now fixed on the node."}});
    expect(storedToStream(row("Remote job e14b14a3-bce7-4d80-8d81-ad7ad239fd82 on node 'dgx-spark' is now running\nAlso explain the queue."))).toMatchObject({type:"assistant_message"});
    expect(storedToStream(row("Dispatched job scheduling is now documented."))).toMatchObject({type:"assistant_message"});
    expect(storedToStream(row("Remote job scheduling is now fixed.",{kind:"remote_job_status"}))).toBeNull();
  });
  it("preserves stored identities and rejects stale sequenced snapshots",()=>{
  const stored=storedToStream({id:7,event_id:"uuid",sequence:12,event_type:"text_delta",content:"Latest",timestamp:""})!;
  expect(stored).toMatchObject({storedId:7,eventId:"uuid",sequence:12});
  const items=buildTranscript([stored,{...snap("Stale"),sequence:11}]);expect(texts(items)[0].text).toBe("Latest");
 });
 it("reconciles held overlap through the final event identity, not text",()=>{
  const history=[snap("Done"),tool,result,final("Done")];
  const held=[snap("Earlier"),result,final("Done"),ev("user_message",{id:"new",content:"Again"}),final("Done","new-final")];
  expect(heldAfterHistory(history,held)).toEqual(held.slice(3));
  let items=buildTranscript(history);for(const e of heldAfterHistory(history,held))items=applyStreamEvent(items,e);
  expect(texts(items)).toHaveLength(2);
 });
 it("coalesces explicitly canonical rows only within the current response",()=>{
  expect(texts(buildTranscript([final("First"),ev("assistant_message",{content:"Canonical",canonical:true})]))).toMatchObject([{text:"Canonical"}]);
  expect(texts(buildTranscript([final("Same","a"),ev("user_message",{content:"Again"}),final("Same","b")]))).toHaveLength(2);
 });
});

import importer from "./fixtures/importer-events.json";
it("replays the supplied 328-event redacted production fixture and updates its live response",()=>{
 const stream=importer.map(row=>storedToStream(row)).filter((x):x is StreamEvent=>!!x);
 let items=buildTranscript(stream);
 const before=items.filter(x=>x.kind==="text").length;
 items=applyStreamEvent(items,ev("tool_call",{tool_call_id:"next",name:"read"}));
 items=applyStreamEvent(items,ev("text_op",{bubble_id:"text_delta_latest",ops:[{type:"replace",range:[0,9999],text:"Updated response after next tool"}]}));
 expect(items.filter(x=>x.kind==="tool")).toHaveLength(163);
 expect(items.filter(x=>x.kind==="text")).toHaveLength(before);
 expect(items.filter(x=>x.kind==="text"&&x.live)).toHaveLength(1);
});

it("keeps distinct canonical bubble IDs and bridges canonical-before-final ordering",()=>{
  const canonical=(bubble_id:string,content:string)=>ev("assistant_message",{canonical:true,bubble_id,content});
  const items=buildTranscript([canonical("one","First"),canonical("two","Second")]);
  expect(texts(items)).toMatchObject([{text:"First"},{text:"Second"}]);
  expect(texts(buildTranscript([canonical("text_delta_latest","Draft"),final("Final")]))).toMatchObject([{text:"Final"}]);
});

import canary from "./fixtures/native-canary-events.json";
it("replays native Grok canary without duplicating final assistant_message + text_delta",()=>{
  const stream=canary.map(row=>storedToStream(row as Parameters<typeof storedToStream>[0])).filter((x):x is StreamEvent=>!!x);
  const replay=buildTranscript(stream);
  let live=buildTranscript([]);for(const e of stream)live=applyStreamEvent(live,e);
  const converse=[...stream].sort((a,b)=>(a.sequence??0)-(b.sequence??0));
  const ordered=buildTranscript(converse);
  const finalText="I'll run `hostname` once and report the result with `ORB_PROD_NATIVE_GROK_OK`.ORB_PROD_NATIVE_GROK_OK\n\n`spark-de79`";
  for(const items of [replay,live,ordered]){
    expect(items.filter(x=>x.kind==="user")).toHaveLength(1);
    expect(items.filter(x=>x.kind==="tool")).toHaveLength(1);
    expect(texts(items).filter(x=>x.text===finalText)).toHaveLength(1);
    expect(texts(items).at(-1)).toMatchObject({text:finalText,live:false});
  }
  expect(texts(buildTranscript([final("Same","a"),ev("user_message",{content:"Again"}),final("Same","b")]))).toHaveLength(2);
});

it("does not reattach a distinct later snapshot to lastFinal in the same user turn",()=>{
  expect(texts(buildTranscript([final("First"),tool,result,snap("Second")]))).toMatchObject([{text:"First",live:false},{text:"Second"}]);
  expect(texts(buildTranscript([final("First"),snap("Second")]))).toMatchObject([{text:"First",live:false},{text:"Second"}]);
  expect(texts(buildTranscript([final("First"),ev("text_delta",{content:" more",mode:"delta"})]))).toMatchObject([{text:"First",live:false},{text:" more"}]);
  expect(texts(buildTranscript([final("Same"),snap("Same")]))).toMatchObject([{text:"Same",live:false}]);
});

it('settles missing tool results at turn end without claiming success',()=>{
 const items=buildTranscript([
  {type:'tool_call',data:{tool_call_id:'fetch',name:'webfetch',args:{url:'https://example.test'}}},
  {type:'assistant_message',data:{content:'Done',success:true}},
 ]);
 const tool=items.find(item=>item.kind==='tool');
 expect(tool).toMatchObject({done:true,unresolved:true});
 const updated=applyStreamEvent(items,{type:'tool_result',data:{tool_call_id:'fetch',result:{error:'403'}}});
 expect(updated.find(item=>item.kind==='tool')).toMatchObject({done:true,unresolved:false,result:{error:'403'}});
});
