import {createSignal,createEffect,onCleanup,For,Show} from 'solid-js';
import {contextBlob,contextManifest,contextHistory,contextConflicts,restoreContext,type ContextChange,type ContextManifest,type ContextOperation} from './projectContext';
import {ErrorNotice} from './ErrorNotice';
export function ContextHistory(p:{slug:string;path:string;onRestore:()=>void}){
 const[open,setOpen]=createSignal(false),[busy,setBusy]=createSignal(false),[error,setError]=createSignal<string|null>(null);
 const[history,setHistory]=createSignal<ContextChange[]>([]),[manifest,setManifest]=createSignal<ContextManifest|null>(null),[conflicts,setConflicts]=createSignal<Record<string,ContextOperation>>({});
 let panel!: HTMLDivElement;
 createEffect(()=>{if(!open())return;const close=(event:PointerEvent)=>{if(panel&&!panel.contains(event.target as Node))setOpen(false);};const escape=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.stopPropagation();setOpen(false);}};document.addEventListener('pointerdown',close);document.addEventListener('keydown',escape,true);onCleanup(()=>{document.removeEventListener('pointerdown',close);document.removeEventListener('keydown',escape,true);});});
 const[comparison,setComparison]=createSignal<{left:string;right:string;images:boolean}|null>(null);
 const urls:string[]=[];onCleanup(()=>urls.forEach(url=>URL.revokeObjectURL(url)));
 const compare=async(op:ContextOperation)=>{setBusy(true);try{
   const hashes=[manifest()?.entries[p.path]?.hash,op.hash];
   const blobs=await Promise.all(hashes.map(hash=>hash?contextBlob(p.slug,hash):Promise.resolve(new Blob([]))));
   const images=/\.(png|jpe?g|gif|webp)$/i.test(p.path);
   const values=await Promise.all(blobs.map(async blob=>{if(images){const url=URL.createObjectURL(blob);urls.push(url);return url;}if(blob.size>512*1024)return 'Preview limited to 512 KiB';return blob.text();}));
   setComparison({left:values[0],right:values[1],images});
 }catch(e){setError(String(e));}finally{setBusy(false);}};
 const both=async(id:string,op:ContextOperation)=>{setBusy(true);try{if(op.delete)throw new Error('A deletion has no second file to keep.');const dot=p.path.lastIndexOf('.');const slash=p.path.lastIndexOf('/');const position=dot>slash?dot:p.path.length;const path=`${p.path.slice(0,position)}.conflict-${id.slice(0,8)}${p.path.slice(position)}`;await restoreContext(p.slug,path,{hash:op.hash,directory:op.directory,size:0,revision:0},null,id);await load();p.onRestore();}catch(e){setError(String(e));}finally{setBusy(false);}};
 const load=async()=>{setBusy(true);setError(null);try{const[m,h,c]=await Promise.all([contextManifest(p.slug),contextHistory(p.slug),contextConflicts(p.slug)]);setManifest(m);setHistory(h.filter(row=>row.path===p.path).reverse());setConflicts(Object.fromEntries(Object.entries(c).filter(([,op])=>op.path===p.path)));}catch(e){setError(String(e));}finally{setBusy(false);}};
 const restore=async(entry:ContextChange['entry'],id?:string)=>{setBusy(true);try{await restoreContext(p.slug,p.path,entry,manifest()?.entries[p.path]?.revision??null,id);await load();p.onRestore();}catch(e){setError(String(e));}finally{setBusy(false);}};
 return <div class="context-history" ref={panel} onKeyDown={event=>{if(event.key==="Escape"){event.stopPropagation();setOpen(false);}}}><button class="s-btn" aria-expanded={open()} onClick={()=>{setOpen(!open());if(open())void load();}}>History</button><Show when={open()}><div class="context-history-panel" role="region" aria-label="Context file history">
 <div class="context-history-heading"><span>File history</span><button class="s-btn" disabled={busy()} onClick={()=>void load()}>Refresh</button></div>
 <Show when={error()}>{message=><ErrorNotice error={message()}/>}</Show>
 <For each={Object.entries(conflicts())}>{([id,op])=><div class="context-history-row"><span>Conflict · {op.source}<small>{op.delete?'Deleted on this machine':'Unpublished variant preserved'}</small></span><button class="s-btn" disabled={busy()} onClick={()=>void compare(op)}>Compare</button><button class="s-btn" disabled={busy()} onClick={()=>void restore(manifest()?.entries[p.path]??null,id)}>Keep shared</button><button class="s-btn" disabled={busy()} onClick={()=>void restore(op.delete?null:{hash:op.hash,directory:op.directory,size:0,revision:0},id)}>Use variant</button><Show when={!op.delete}><button class="s-btn" disabled={busy()} onClick={()=>void both(id,op)}>Keep both</button></Show></div>}</For>
 <Show when={comparison()}>{value=><div class="context-comparison"><section><small>Shared version</small><Show when={value().images} fallback={<pre>{value().left}</pre>}><img src={value().left} alt="Shared version"/></Show></section><section><small>Variant</small><Show when={value().images} fallback={<pre>{value().right}</pre>}><img src={value().right} alt="Conflicting variant"/></Show></section></div>}</Show>
 <For each={history()}>{row=><div class="context-history-row"><span>Version {row.revision}<small>{row.source} · {row.entry?`${row.entry.size} bytes`:'Deleted'}</small></span><button class="s-btn" disabled={busy()||manifest()?.entries[p.path]?.revision===row.revision} onClick={()=>void restore(row.entry)}>Restore</button></div>}</For>
 <Show when={!busy()&&!history().length&&!error()}><p class="dim">No changes recorded.</p></Show>
 </div></Show></div>;
}
