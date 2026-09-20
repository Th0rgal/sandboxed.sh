import type { StreamEvent } from "./stream";

export type StreamItem =
  | { kind: "user"; key: string; text: string }
  | { kind: "think"; key: string; text: string; done: boolean }
  | { kind: "text"; key: string; text: string; live: boolean }
  | { kind: "error"; key: string; text: string }
  | {
      kind: "tool";
      key: string;
      callId: string;
      name: string;
      args: unknown;
      result?: unknown;
      done: boolean;
    };


const str = (v: unknown): string => typeof v === "string" ? v : v == null ? "" : String(v);
const states = new WeakMap<StreamItem[], TranscriptReducer>();

/** The producer owns text_delta_latest for an entire assistant response,
 * including tool boundaries. Other text_op bubbles have independent identities.
 * Offsets are Rust Unicode scalar indices, not JavaScript UTF-16 offsets. */
export class TranscriptReducer {
  items: StreamItem[] = [];
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
    this.bubbles.clear();
  }
  apply(ev: StreamEvent) {
    const d = ev.data;
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
      case "user_message":
        this.close(); this.lastFinal = undefined;
        this.items.push({kind:"user",key:this.key("user"),text:str(d.content)}); return;
      case "text_delta":
      case "text_op": {
        let index = this.bubbles.get(bubble);
        if (index == null && bubble === "text_delta_latest" && this.lastFinal != null && this.items[this.lastFinal]?.kind === "text") {
          index = this.lastFinal;
          this.bubbles.set(bubble, index);
        }
        const previous = index == null ? undefined : this.items[index];
        let text = previous?.kind === "text" ? previous.text : "";
        let live = previous?.kind === "text" ? previous.live : true;
        if (ev.type === "text_delta") {
          const next = str(d.content);
          if (d.mode === "delta") {
            text += next;
            live = true;
          } else {
            text = next;
            if (!(previous?.kind === "text" && !previous.live && previous.text === next)) live = true;
          }
        } else {
          for (const op of (Array.isArray(d.ops) ? d.ops : []) as Record<string, unknown>[]) {
            const chars = Array.from(text);
            if (op.type === "insert") {
              // This synthetic bubble carries a full snapshot even on reconnect,
              // when a fresh producer buffer emits insert(0, accumulated_text).
              if (bubble === "text_delta_latest" && op.pos === 0) text = str(op.text);
              else { const pos = typeof op.pos === "number" ? op.pos : chars.length; chars.splice(Math.max(0,pos),0,str(op.text)); text=chars.join(""); }
            } else if (op.type === "replace") {
              const range = Array.isArray(op.range) ? op.range as number[] : [0,chars.length];
              if (bubble === "text_delta_latest" && range[0] === 0) text = str(op.text);
              else { chars.splice(Math.max(0,range[0]),Math.max(0,range[1]-range[0]),str(op.text)); text=chars.join(""); }
            } else if (op.type === "finalize") live = false;
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
        if(d.success===false){this.items.push({kind:"error",key:this.key("error"),text:text||"Mission failed"});return;}
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
          if(old.kind==="tool"&&ev.type==="tool_result")this.put(index,{...old,result:d.result,done:true});
          return;
        }
        this.tools.set(callId,this.items.length);
        this.items.push({kind:"tool",key:`tool:${callId}`,callId,name:str(d.name)||"tool",args:d.args??null,...(ev.type==="tool_result"?{result:d.result}:{}),done:ev.type==="tool_result"});return;
      }
      case "error": this.close(); this.items.push({kind:"error",key:this.key("error"),text:str(d.message)});return;
    }
  }
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
