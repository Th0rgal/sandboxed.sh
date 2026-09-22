import type {DraftImage} from "./imageAttachments";
export interface ComposerDraft { text:string; images:DraftImage[]; }
let database: Promise<IDBDatabase> | undefined;
function db(): Promise<IDBDatabase> {
 return database ??= new Promise((resolve,reject)=>{
  const request=indexedDB.open('orb-composer-drafts',1);
  request.onupgradeneeded=()=>request.result.createObjectStore('drafts');
  request.onsuccess=()=>resolve(request.result);
  request.onerror=()=>reject(request.error);
 });
}
export async function readComposerDraft(scope:string):Promise<ComposerDraft | undefined> {
 const database=await db();
 return new Promise((resolve,reject)=>{
  const request=database.transaction('drafts').objectStore('drafts').get(scope);
  request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
 });
}
export async function saveComposerDraft(scope:string,draft:ComposerDraft):Promise<void> {
 const database=await db();
 return new Promise((resolve,reject)=>{
  const transaction=database.transaction('drafts','readwrite');
  const store=transaction.objectStore('drafts');
  if (!draft.text && !draft.images.length) store.delete(scope);else store.put(draft,scope);
  transaction.oncomplete=()=>resolve();transaction.onerror=()=>reject(transaction.error);
 });
}
