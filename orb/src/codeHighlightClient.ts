let worker: Worker | undefined;
let sequence=0;
const pending=new Map<number,(html:string|null)=>void>();
/** One lazy worker shared by all blocks. Cancelled requests cannot update stale DOM. */
export function requestHighlight(text:string,lang:string,receive:(html:string|null)=>void):()=>void {
  if (!worker) {
    worker=new Worker(new URL("./codeHighlight.worker.ts",import.meta.url),{type:"module"});
    worker.onmessage=event=>{
      const {id,html}=event.data;
      pending.get(id)?.(html);pending.delete(id);
    };
    worker.onerror=()=>{worker?.terminate();worker=undefined;pending.forEach(cb=>cb(null));pending.clear();};
  }
  const id=++sequence;
  pending.set(id,receive);worker.postMessage({id,text,lang});
  return ()=>{pending.delete(id);};
}
