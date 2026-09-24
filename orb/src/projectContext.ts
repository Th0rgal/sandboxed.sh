import {api,getApiUrl,getJwt} from './api';
import {nativeInvoke} from './clientRuns';
export interface ContextEntry {hash:string|null; directory:boolean; revision:number; size:number}
export interface ContextManifest {revision:number; entries:Record<string,ContextEntry>}
export interface ContextOperation {id:string;path:string;base:number|null;hash:string|null;directory:boolean;delete:boolean;source:string}
export interface ContextChange {revision:number;path:string;entry:ContextEntry|null;source:string}
const route=(slug:string)=>`/api/projects/${encodeURIComponent(slug)}/context`;
export const contextManifest=(slug:string)=>api<ContextManifest>(`${route(slug)}/manifest`);
export const contextHistory=(slug:string)=>api<ContextChange[]>(`${route(slug)}/history`);
export const contextConflicts=(slug:string)=>api<Record<string,ContextOperation>>(`${route(slug)}/conflicts`);
export async function restoreContext(slug:string,path:string,entry:ContextEntry|null,base:number|null,conflict?:string){
 const operation:ContextOperation={id:crypto.randomUUID(),path,base,hash:entry?.hash??null,directory:entry?.directory??false,delete:!entry,source:'Orb'};
 const result=await api<{revision:number;conflict:boolean}>(`${route(slug)}/${conflict?`conflicts/${encodeURIComponent(conflict)}`:'operations'}`,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(operation)});
 if(result.conflict)throw new Error('This file changed again. Refresh before resolving it. Your variant is preserved.');
}
export async function readProjectFileVersion(slug:string,path:string){
 const local=await localContextFile<{content:string;revision?:number}>(slug,"read",path);if(local)return local;
 return api<{content:string;revision?:number}>(`/api/projects/${encodeURIComponent(slug)}/file?path=${encodeURIComponent(path)}`);
}
export async function contextBlob(slug:string,hash:string):Promise<Blob>{
 const {getApiUrl,getJwt}=await import('./api');
 const response=await fetch(`${getApiUrl()}${route(slug)}/blobs/${encodeURIComponent(hash)}`,{headers:{Authorization:`Bearer ${getJwt()??''}`}});
 if(!response.ok)throw new Error(`Cannot load context version (${response.status})`);
 return response.blob();
}

export async function localContextFile<T>(slug:string,operation:string,path:string,content?:string,revision?:number):Promise<T|undefined>{
 const invoke=nativeInvoke();if(!invoke)return undefined;
 try{return await invoke('project_context_file',{request:{endpoint:getApiUrl(),token:getJwt()??'',project:slug},operation,path,content:content??null,revision:revision??null}) as T;}
 catch(error){if(/unknown command|command .*not found|HTTP 404/i.test(String(error)))return undefined;throw error;}
}
