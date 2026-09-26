import { onCleanup, onMount } from 'solid-js';
import type { UploadSource } from './uploads';

/** Native drops have OS paths; only the composer under the pointer consumes them. */
export function nativeComposerDrop(element:()=>HTMLElement|undefined, attach:(sources:UploadSource[])=>Promise<void>) {
 let dispose:(()=>void)|undefined, stopped=false;
 onMount(()=>{
  const event=(window as any).__TAURI__?.event;
  if(!event)return;
  void event.listen('orb-upload-drop',(event:{payload:{paths:string[];x:number;y:number}})=>{
   const el=element();if(!el||!el.getClientRects().length)return;
   const {paths,x,y}=event.payload, bounds=el.getBoundingClientRect(),scale=window.devicePixelRatio||1;
   if(x/scale<bounds.left||x/scale>bounds.right||y/scale<bounds.top||y/scale>bounds.bottom)return;
   void attach(paths.map(path=>({name:path.split(/[\\/]/).at(-1)??'file',localPath:path})));
  }).then((off:()=>void)=>{if(stopped)off();else dispose=off;});
 });
 onCleanup(()=>{stopped=true;dispose?.();});
}
