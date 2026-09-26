import { describe, it, expect } from "vitest";
import { buildTranscript, applyStreamEvent } from "../src/transcriptModel";
import { heldAfterHistory, storedToStream, type StreamEvent } from "../src/stream";
const message = (id: string, queued: boolean, content = "same text"): StreamEvent => ({ type:"user_message", eventId:id, data:{id,queued,content} });
const delta = (content:string):StreamEvent => ({type:"text_delta",data:{content}});
const users = (events:StreamEvent[]) => buildTranscript(events).filter(i=>i.kind==="user");
describe("message identity and queue lifecycle",()=>{
  it("keeps identical sends distinct, in order, and does not close the current assistant",()=>{
    const items=buildTranscript([delta("Working"),message("a",true),message("b",true),delta("Still working")]);
    expect(items.filter(i=>i.kind==="user")).toMatchObject([{messageId:"a",queued:true},{messageId:"b",queued:true}]);
    expect(items.filter(i=>i.kind==="text")).toMatchObject([{text:"Still working",live:true}]);
  });
  it("a delivered event replaces the queued row after its preceding response",()=>{
    let items=buildTranscript([message("a",true),delta("Response before delivery"),message("b",true)]);
    items=applyStreamEvent(items,message("a",false));
    expect(items.filter(i=>i.kind!=="user"||!i.queued).map(i=>i.kind)).toEqual(["text","user"]);
    expect(items.filter(i=>i.kind==="user")).toHaveLength(2);
    expect(items.filter(i=>i.kind==="text")).toMatchObject([{live:false}]);
    items=applyStreamEvent(items,delta("Next response"));
    expect(items.filter(i=>i.kind==="text")).toHaveLength(2);
  });
  it.each([
    [true,false,true,false], [false,true,false,true], [true,true,false,false],
  ])("receipt / SSE / history order %j is monotonic",(...states)=>{
    expect(users(states.map(state=>message("id",state)))).toMatchObject([{messageId:"id",queued:false}]);
  });
  it("applies an older queued-to-delivered transition even after newer user sequences",()=>{
    expect(users([{...message("a",true),sequence:1},{...message("b",true),sequence:3},{...message("a",false),sequence:2}])).toMatchObject([{messageId:"b",queued:true},{messageId:"a",queued:false}]);
  });
  it("keeps attachment references through delivery and stale receipts",()=>{
    const content="same text\n\n<!-- paloma:attachment:id -->\nAttached context: read snapshot";
    expect(users([message("id",true,content),message("id",false,content),message("id",true,"same text")])).toMatchObject([{text:content,queued:false}]);
  });
  it("replay keeps server identity and queued metadata",()=>{
    const replay=storedToStream({id:7,event_id:"msg",sequence:20,event_type:"user_message",timestamp:"",content:"hello",metadata:{queued:false}})!;
    expect(users([message("msg",true,"hello"),replay])).toMatchObject([{messageId:"msg",queued:false}]);
  });
  it("a later overlapping event cannot discard held queued messages",()=>{
    const final:StreamEvent={type:"assistant_message",eventId:"final",data:{content:"Done"}};
    const held=[message("a",true),final,message("a",false)];
    const kept=heldAfterHistory([final],held);
    expect(kept).toEqual([held[0],held[2]]);
    expect(users(kept)).toMatchObject([{messageId:"a",queued:false}]);
  });
  it("scheduler coalescing delivers original IDs, not the scheduler's replacement ID",()=>{
    const a="00000000-0000-4000-8000-000000000001",b="00000000-0000-4000-8000-000000000002";
    const text="same 🦀 text", encoded=btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify({messages:[[a,text],[b,text]]}))));
    const delivered:StreamEvent={type:"user_message",eventId:"scheduler-id",data:{id:"scheduler-id",source:"scheduler",queued:false,content:`${text}\n${text}\n<!-- sandboxed:messages:v1:${encoded} -->`}};
    expect(users([message(a,true,text),message(b,true,text),delivered,delivered,message(a,true,text)])).toMatchObject([{messageId:a,text,queued:false},{messageId:b,text,queued:false}]);
    expect(users([{...delivered,data:{...delivered.data,source:"api:user"}}])).toMatchObject([{messageId:"scheduler-id"}]);
  });
});

import { messagePresentation } from "../src/messagePresentation";
it("upgrades a receipt to the authoritative attachment content without duplicating its row",()=>{
  const id="00000000-0000-4000-8000-000000000001";
  const content=`same text\n\n<!-- paloma:attachment:${id} -->\nAttached context: read \`.paloma/messages/${id}/.paloma/attach.md\` (paths in that manifest are relative to \`.paloma/messages/${id}\`).`;
  const initial=message(id,true);initial.data.receipt=true;initial.data.attached=true;
  const rows=users([initial,message(id,true,content),message(id,false,content),message(id,true)]);
  expect(rows).toMatchObject([{text:content,queued:false,attached:true}]);
  expect(messagePresentation(content)).toEqual({text:"same text",attached:true});
  expect(messagePresentation("Please read .paloma/attach.md")).toEqual({text:"Please read .paloma/attach.md",attached:false});
});
it("stored structured scheduler metadata reconciles its original message IDs",()=>{
  const id="00000000-0000-4000-8000-000000000001";
  const replay=storedToStream({id:1,event_id:"scheduler",sequence:1,event_type:"user_message",timestamp:"",content:"combined prompt",metadata:{source:"scheduler",queued:false,messages:[[id,"actual user text"]]}})!;
  expect(users([message(id,true,"actual user text"),replay])).toMatchObject([{messageId:id,text:"actual user text",queued:false}]);
});
it('preserves automatic origin through delivery and late receipts',()=>{
 const automatic:StreamEvent={type:'user_message',eventId:'auto',data:{id:'auto',content:'Continue',source:'idle-worker-watchdog'}};
 expect(users([message('auto',true,'Continue'),automatic,message('auto',true,'Continue')])).toMatchObject([{source:'idle-worker-watchdog',queued:false}]);
 expect(users([storedToStream({id:1,event_id:'auto',sequence:1,event_type:'user_message',timestamp:'',content:'Continue',metadata:{source:'transport_auto_resume'}})!])).toMatchObject([{source:'transport_auto_resume'}]);
 const combined:StreamEvent={type:'user_message',data:{id:'combined',content:'Continue',source:'scheduler',messages:[['00000000-0000-4000-8000-000000000001','Continue']]}};
 expect(users([combined])[0]).not.toHaveProperty('source','scheduler');
});
