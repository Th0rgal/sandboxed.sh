import { requestHighlight } from "./codeHighlightClient";
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { copyText } from "./clipboard";
import { CopyIcon, CheckIcon } from "./icons";

export function CodeBlock(p: {text:string;lang:string}) {
  const [html,setHtml]=createSignal<string|null>(null);
  const [feedback,setFeedback]=createSignal("");
  let feedbackTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(()=>clearTimeout(feedbackTimer));
  createEffect(()=>{
    const text=p.text, lang=p.lang;
    setHtml(null);
    if (!lang || text.length>100_000 || typeof Worker === "undefined") return;
    let cancel: (()=>void) | undefined;
    // Coalesce streaming updates; completed blocks retain their highlighted DOM.
    const timer=setTimeout(()=>{
      cancel=requestHighlight(text,lang,setHtml);
    },120);
    onCleanup(()=>{clearTimeout(timer);cancel?.();});
  });
  const copy=async()=>{
    try {await copyText(p.text);setFeedback("Copied");}
    catch(e) {setFeedback(e instanceof Error?e.message:String(e));}
    clearTimeout(feedbackTimer);
    feedbackTimer=setTimeout(()=>setFeedback(""),2500);
  };
  return <div class="md-code-block">
    <pre><Show when={html()} fallback={<code>{p.text}</code>}>{value=><code innerHTML={value()}/>}</Show></pre>
    <button class="icon-btn code-copy" aria-label="Copy code" title={feedback()||"Copy code"} onClick={()=>void copy()}>
      <Show when={feedback()==="Copied"} fallback={<CopyIcon size={14}/>}><CheckIcon size={14}/></Show>
    </button>
    <Show when={feedback()}><span class="code-copy-feedback" role="status">{feedback()}</span></Show>
  </div>;
}
