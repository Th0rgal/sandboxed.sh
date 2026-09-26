import { encoded, type UploadedFile } from "./uploads";
import type { DraftImage } from "./imageAttachments";
import { connectionVersion, getApiUrl, getJwt } from "./api";
import type { StreamItem } from "./transcriptModel";
export type SideAttachment = { name:string; data_base64:string; media_type:string };
export type SideExchange = { question: string; answer: string; attachments?:SideAttachment[] };
export async function sideAttachments(images:DraftImage[], files:UploadedFile[]):Promise<SideAttachment[]> {
 const result:SideAttachment[]=images.map(image=>({name:image.name,data_base64:image.dataUrl.split(',')[1],media_type:image.type}));
 for(const file of files)result.push({name:file.source.name,data_base64:file.dataBase64??await encoded(file.source),media_type:file.source.file?.type||'application/octet-stream'});
 if(result.length>8)throw new Error('Attach up to 8 files or images to a side question.');
 if(result.reduce((n,f)=>n+f.data_base64.length,0)>24*1024*1024)throw new Error('Side question attachments must total at most 18 MiB.');
 return result;
}
export type SideEvent = {type:"snapshot";text:string}| {type:"start";model:string}|{type:"delta";text:string}|{type:"done";answer:string}|{type:"error";message:string};
function byteTail(text:string,limit:number):string {
  const bytes=new TextEncoder().encode(text);
  if(bytes.length<=limit)return text;
  let start=bytes.length-limit;
  while(start<bytes.length&&(bytes[start]&0xc0)===0x80)start++;
  return new TextDecoder().decode(bytes.slice(start));
}
export function boundedHistory(history:SideExchange[],attachmentBudget=24*1024*1024):SideExchange[] {
  let budget=65000;const result:SideExchange[]=[];
  for(const exchange of history.slice(-20).reverse()){
    const bytes=new TextEncoder().encode(exchange.question+exchange.answer).length;
    const attached=(exchange.attachments??[]).reduce((n,file)=>n+file.data_base64.length,0);
    if(bytes>budget||attached>attachmentBudget)break;budget-=bytes;attachmentBudget-=attached;result.unshift(exchange);
  }
  return result;
}
// No thinking, unsent queued messages, or in-progress assistant reply is sent.
export function sideContext(items: StreamItem[]): string {
  return byteTail(items.flatMap(item => {
    if(item.kind==='user') return item.queued ? [] : [`User: ${item.text}`];
    if(item.kind==='text') return item.live ? [] : [`Agent: ${item.text}`];
    if(item.kind==='tool' && item.done) return [`Tool ${item.name}: ${JSON.stringify({args:item.args,result:item.result}).slice(0,12000)}`];
    if(item.kind==='error') return [`Recorded error: ${item.text}`];
    return [];
  }).join('\n\n'),80000);
}
export async function askSide(mission:string, question:string, context:string, history:SideExchange[], signal:AbortSignal, receive:(event:SideEvent)=>void, attachments:SideAttachment[] = []) {
  const version=connectionVersion();
  const response=await fetch(`${getApiUrl()}/api/control/missions/${encodeURIComponent(mission)}/btw`,{
    method:'POST', headers:{'Content-Type':'application/json',Authorization:`Bearer ${getJwt()??''}`},
    body:JSON.stringify({question,context,history:boundedHistory(history,24*1024*1024-attachments.reduce((n,file)=>n+file.data_base64.length,0)).map(exchange=>({...exchange,attachments:exchange.attachments?.length?exchange.attachments:undefined})),...(attachments.length?{attachments}:{})}),signal,
  });
  if(!response.ok) { const detail=await response.text().catch(()=>''); throw new Error(response.status===400||response.status===413?detail.slice(0,500):response.status===422?'Core needs an update to accept these attachments. Your working agent is unaffected.':response.status===404?'Side questions are unavailable on this Core, or this conversation has not synced yet.':`Could not ask the side question (${response.status}). Check the connection and Ask model configuration.`); }
  if(!response.body) throw new Error('No answer stream received.');
  const reader=response.body.getReader(),decoder=new TextDecoder();let buffer='',complete=false;
  try {
    while(true) {
      const {done,value}=await reader.read();
      if(connectionVersion()!==version) throw new Error('Connection changed. Ask again in the current conversation.');
      buffer+=decoder.decode(value,{stream:!done});
      buffer=buffer.replace(/\r\n/g,'\n');
      let end:number;
      while((end=buffer.indexOf('\n\n'))>=0) {
        const frame=buffer.slice(0,end);buffer=buffer.slice(end+2);
        const data=frame.split('\n').filter(line=>line.startsWith('data:')).map(line=>line.slice(5).trimStart()).join('\n');
        if(!data)continue;
        const event=JSON.parse(data) as SideEvent;
        if(event.type==='error')throw new Error(event.message);
        if(event.type==='done')complete=true;
        receive(event);
      }
      if(done)break;
    }
    if(!complete)throw new Error('The answer was interrupted. Try again; the working agent is unaffected.');
  } finally {await reader.cancel().catch(()=>{});reader.releaseLock();}
}
