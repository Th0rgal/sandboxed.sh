import { highlightCode } from "./codeHighlight";
self.onmessage = (event: MessageEvent<{id:number;text:string;lang:string}>) => {
  let html: string|null=null;
  try { html=highlightCode(event.data.text,event.data.lang); } catch { /* keep plaintext */ }
  self.postMessage({id:event.data.id,html});
};
