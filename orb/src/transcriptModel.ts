import type { DraftImage } from "./imageAttachments";
import { deferredMessages } from "./deferredMessages";
import type { StreamEvent } from "./stream";

export type StreamItem =
  | { kind: "user"; key: string; text: string; images?: DraftImage[]; messageId?: string; source?: string; queued?: boolean; attached?: boolean; receipt?: boolean }
  | { kind: "think"; key: string; text: string; done: boolean }
  | { kind: "text"; key: string; text: string; live: boolean }
  | { kind: "error"; key: string; text: string; terminal?: boolean; cancelled?: boolean }
  | {
      kind: "tool";
      key: string;
      callId: string;
      name: string;
      args: unknown;
      result?: unknown;
      done: boolean;
      unresolved?: boolean;
    };


const str = (v: unknown): string => typeof v === "string" ? v : v == null ? "" : String(v);
const states = new WeakMap<StreamItem[], TranscriptReducer>();

/** The producer owns text_delta_latest for an entire assistant response,
 * including tool boundaries. Other text_op bubbles have independent identities.
 * Offsets are Rust Unicode scalar indices, not JavaScript UTF-16 offsets. */
export class TranscriptReducer {
  items: StreamItem[] = [];
  private users = new Map<string, number>();
  private tools = new Map<string, number>();
  private bubbles = new Map<string, number>();
  private seen = new Set<string>();
  private seq = new Map<string, number>();
  private next = 0;
  private lastFinal: number | undefined;
  private key(kind: string) { return `${kind}-${++this.next}`; }
  private put(index: number, item: StreamItem) { this.items[index] = item; }
  private close() {
    for (const index of this.bubbles.values()) {
      const item = this.items[index];
      if (item?.kind === "text" && item.live) this.put(index, { ...item, live: false });
    }
    for (let i=0;i<this.items.length;i++) {
      const item=this.items[i];
      if(item.kind==='tool'&&!item.done)this.put(i,{...item,done:true,unresolved:true});
      if(item.kind==='think'&&!item.done)this.put(i,{...item,done:true});
    }
    this.bubbles.clear();
  }
  apply(ev: StreamEvent) {
    const d = ev.data;
    // A message has one identity and a monotonic queued -> delivered lifecycle.
    // Process it before event dedupe and channel sequence checks: receipt, queue
    // snapshot, stored history and SSE may arrive in any order.
    if (ev.type === "user_message") {
      const parts = deferredMessages(str(d.content), d.source, d.messages);
      if (parts) {
        for (const part of parts) this.apply({ type: "user_message", data: { ...part, queued: d.queued === true } });
        return;
      }
      const messageId = typeof d.id === "string" ? d.id : ev.eventId;
      const queued = d.queued === true;
      const source = typeof d.source === "string" ? d.source : undefined;
      if (!messageId && ev.sequence != null) {
        const legacyIdentity = `user:${ev.sequence}:${queued}`;
        if (this.seen.has(legacyIdentity)) return;
        this.seen.add(legacyIdentity);
      }
      const index = messageId ? this.users.get(messageId) : undefined;
      const previous = index == null ? undefined : this.items[index];
      if (previous?.kind === "user" && index != null) {
        if (source && source !== previous.source) this.put(index, { ...previous, source });
        if (!previous.queued || queued) {
          if (previous.receipt && d.receipt !== true) this.put(index, { ...previous, source: source ?? previous.source, text: str(d.content) || previous.text, receipt: false });
          return;
        }
        this.close(); this.lastFinal = undefined;
        // A queued row may precede text that arrived while it was waiting.
        // Delivery places it after that response without changing its identity.
        this.items.splice(index, 1);
        for (const map of [this.users, this.tools, this.bubbles]) {
          for (const [key, position] of map) if (position > index) map.set(key, position - 1);
        }
        this.users.set(messageId!, this.items.length);
        this.items.push({ ...previous, source: source ?? previous.source, text: str(d.content) || previous.text, queued: false, receipt: d.receipt === true });
        return;
      }
      if (!queued) { this.close(); this.lastFinal = undefined; }
      if (messageId) this.users.set(messageId, this.items.length);
      this.items.push({ kind: "user", key: messageId ? `user:${messageId}` : this.key("user"), text: str(d.content), messageId, source, queued, attached: d.attached === true, receipt: d.receipt === true });
      return;
    }
    const id = ev.eventId ?? (typeof d.id === "string" ? d.id : undefined);
    const identity = d.canonical === true && ev.sequence != null ? `canonical:${ev.sequence}` : id ? `${ev.type}:${id}` : ev.sequence != null ? `stored:${ev.sequence}:${ev.type}` : undefined;
    if (identity && this.seen.has(identity)) return;
    if (identity) this.seen.add(identity);
    const bubble = str(d.bubble_id) || "text_delta_latest";
    const channel = `${ev.type}:${bubble}`;
    if (ev.sequence != null) {
      if (ev.sequence < (this.seq.get(channel) ?? -1)) return;
      this.seq.set(channel, ev.sequence);
    }
    const last = this.items.at(-1);
    switch (ev.type) {
      case "text_delta":
      case "text_op": {
        let index = this.bubbles.get(bubble);
        const previous = index == null ? undefined : this.items[index];
        let text = previous?.kind === "text" ? previous.text : "";
        let live = previous?.kind === "text" ? previous.live : true;
        let snapshot: string | undefined;
        if (ev.type === "text_delta") {
          const next = str(d.content);
          if (d.mode === "delta") {
            text += next;
            live = true;
          } else {
            snapshot = next;
            text = next;
            live = true;
          }
        } else {
          for (const op of (Array.isArray(d.ops) ? d.ops : []) as Record<string, unknown>[]) {
            const chars = Array.from(text);
            if (op.type === "insert") {
              // This synthetic bubble carries a full snapshot even on reconnect,
              // when a fresh producer buffer emits insert(0, accumulated_text).
              if (bubble === "text_delta_latest" && op.pos === 0) { snapshot = str(op.text); text = snapshot; }
              else { const pos = typeof op.pos === "number" ? op.pos : chars.length; chars.splice(Math.max(0,pos),0,str(op.text)); text=chars.join(""); }
            } else if (op.type === "replace") {
              const range = Array.isArray(op.range) ? op.range as number[] : [0,chars.length];
              if (bubble === "text_delta_latest" && range[0] === 0) { snapshot = str(op.text); text = snapshot; }
              else { chars.splice(Math.max(0,range[0]),Math.max(0,range[1]-range[0]),str(op.text)); text=chars.join(""); }
            } else if (op.type === "finalize") live = false;
          }
        }
        if (index == null && bubble === "text_delta_latest" && this.lastFinal != null) {
          const finalized = this.items[this.lastFinal];
          if (finalized?.kind === "text" && !finalized.live && snapshot != null && snapshot === finalized.text) {
            index = this.lastFinal;
            this.bubbles.set(bubble, index);
            live = false;
          }
        }
        if (index == null) {
          if (!text) return;
          index=this.items.length; this.bubbles.set(bubble,index);
          this.items.push({kind:"text",key:this.key("text"),text,live});
        } else if (previous?.kind === "text" && (previous.text !== text || previous.live !== live)) this.put(index,{...previous,text,live});
        return;
      }
      case "assistant_message": {
        const text=str(d.content);
        const finalBubble=str(d.bubble_id)||"text_delta_latest";
        if(d.canonical===true){
          // Canonical rows finalize one identified bubble; other native bubbles
          // must remain distinct. Keep it available for the turn's final event.
          const index=this.bubbles.get(finalBubble) ?? (finalBubble==="text_delta_latest" ? this.lastFinal : undefined);
          const previous=index==null ? undefined : this.items[index];
          if(previous?.kind==="text"&&index!=null){this.put(index,{...previous,text:text||previous.text,live:false});this.bubbles.set(finalBubble,index);}
          else if(text){this.bubbles.set(finalBubble,this.items.length);this.items.push({kind:"text",key:this.key("text"),text,live:false});}
          return;
        }
        const index=this.bubbles.get(finalBubble);
        const previous=index == null ? undefined : this.items[index];
        this.close();
        if(d.success===false){this.items.push({kind:"error",key:this.key("error"),text:text||"Mission failed",terminal:true,cancelled:text.trim().toLowerCase()==="cancelled"});return;}
        if(previous?.kind === "text" && index != null){this.put(index,{...previous,text:text||previous.text,live:false});this.lastFinal=index;}
 else if(text){this.lastFinal=this.items.length;this.items.push({kind:"text",key:this.key("text"),text,live:false});}
        return;
      }
      case "thinking": {
        const text=str(d.content),done=d.done===true;
        if(last?.kind==="think"&&!last.done)this.put(this.items.length-1,{...last,text,done});
        else if(text)this.items.push({kind:"think",key:this.key("think"),text,done});
        return;
      }
      case "tool_call":
      case "tool_result": {
        const callId=str(d.tool_call_id), index=this.tools.get(callId);
        if(index!=null){
          const old=this.items[index];
          if(old.kind==="tool"&&ev.type==="tool_result")this.put(index,{...old,result:d.result,done:true,unresolved:false});
          else if(old.kind==="tool"&&ev.type==="tool_call")this.put(index,{...old,name:str(d.name)||old.name,args:d.args??old.args});
          return;
        }
        this.tools.set(callId,this.items.length);
        this.items.push({kind:"tool",key:`tool:${callId}`,callId,name:str(d.name)||"tool",args:d.args??null,...(ev.type==="tool_result"?{result:d.result}:{}),done:ev.type==="tool_result"});return;
      }
      case "error": this.close(); this.items.push({kind:"error",key:this.key("error"),text:str(d.message)});return;
    }
  }
}

/**
 * Punctuation that can never be an answer on its own. Deliberately narrow:
 * `---`, `***`, `|`, backticks and brackets are all meaningful Markdown and are
 * not listed, so real content is never at risk.
 */
const FILLER_ONLY = /^[.\u2026,;]+$/;

/**
 * A finalized assistant bubble holding nothing but filler punctuation, with
 * real output still to come.
 *
 * Asking OpenCode "What's the status of the Pareto audit?" produced a bare `.`
 * between two batches of tool calls, before the real answer — rendered as its
 * own paragraph, and splitting what was one stretch of work into "Worked 5
 * tools" and "Worked 1 tool". The source of the reported live-only emission was not retained in the
 * persisted events. Only finalized bubbles are candidates here; a live
 * prefix must remain free to grow into text.
 *
 * Three conditions keep this from eating anything real:
 *  - `live` bubbles are never hidden, so a `.` that is the first token of a
 *    sentence still being streamed stays and grows normally;
 *  - something must come after it, so if `.` is all the agent ever said the
 *    user sees it rather than an empty transcript;
 *  - only sentence punctuation counts, so code, rules and tables are untouched.
 *
 * Presentation only — the underlying items, and the events behind them, are
 * unchanged.
 */
export function isFillerBubble(item: StreamItem, index: number, items: StreamItem[]): boolean {
  if (item.kind !== "text" || item.live) return false;
  if (!FILLER_ONLY.test(item.text.trim())) return false;
  // An error is not an answer: it must not license hiding the only output there
  // was. Only real text that followed does.
  for (const next of items.slice(index + 1)) {
    if (next.kind === "user" || next.kind === "error") return false;
    if (next.kind === "text" && next.text.trim() && !FILLER_ONLY.test(next.text.trim())) return true;
  }
  return false;
}

/** The transcript as shown: filler bubbles dropped, everything else intact. */
export function withoutFiller(items: StreamItem[]): StreamItem[] {
  const keep = items.filter((item, index) => !isFillerBubble(item, index, items));
  return keep.length === items.length ? items : keep;
}

/** A delivered follow-up supersedes the previous attempt's terminal notice.
 * Keep the raw history intact, and do not dismiss failures for queued input. */
export function visibleTranscript(items: StreamItem[]): StreamItem[] {
  let lastDelivered = -1;
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.kind === "user" && !item.queued && !item.attached) {
      lastDelivered = i;
      break;
    }
  }
  return withoutFiller(items.filter((item, index) =>
    !(item.kind === "error" && item.terminal && index < lastDelivered),
  ));
}

export function buildTranscript(events: StreamEvent[]): StreamItem[] {
  const reducer=new TranscriptReducer();
  for(const event of events)reducer.apply(event);
  states.set(reducer.items,reducer);
  return reducer.items;
}

export function applyStreamEvent(items: StreamItem[], event: StreamEvent): StreamItem[] {
  let reducer=states.get(items);
  if(!reducer){
    if(items.length)throw new Error("Transcript must originate from buildTranscript");
    reducer=new TranscriptReducer();
  }
  reducer.items=items.slice();
  reducer.apply(event);
  states.set(reducer.items,reducer);
  return reducer.items;
}
