import { clearConnection, getApiUrl, getJwt } from "./api";

export interface StoredEvent {
  id: number;
  event_id?: string | null;
  sequence: number;
  event_type: string;
  timestamp: string;
  tool_call_id?: string | null;
  tool_name?: string | null;
  content: string;
  metadata?: Record<string, unknown>;
}

const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const REMOTE_JOB_RUNNING = new RegExp(`^Remote job ${UUID} on node '[^']+' is now (queued|running|finished)$`);
const DISPATCHED_JOB = new RegExp(`^Dispatched job ${UUID} to remote node '[^']+'(?: \\([^\\n]*\\))?$`);

export function isGeneratedRemoteJobStatus(ev: StoredEvent): boolean {
  const meta = ev.metadata ?? {};
  if (meta.kind === "remote_job_status" || meta.remote_job_status === true) return true;
  return REMOTE_JOB_RUNNING.test(ev.content) || DISPATCHED_JOB.test(ev.content);
}

export interface StreamEvent {
  eventId?: string;
  sequence?: number;
  storedId?: number;
  type: string;
  data: Record<string, unknown>;
}

async function apiRaw(path: string): Promise<Response> {
  const jwt = getJwt();
  const res = await fetch(`${getApiUrl()}${path}`, {
    headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
  });
  if (res.status === 401) {
    clearConnection();
    throw new Error("401 Unauthorized — reconnect in Settings → Backend");
  }
  if (!res.ok) throw new Error(`${res.status} ${await res.text().catch(() => "")}`.trim());
  return res;
}

export async function getMissionEvents(id: string): Promise<StoredEvent[]> {
  const res = await apiRaw(`/api/control/missions/${id}/events?limit=4000`);
  const data = (await res.json()) as StoredEvent[] | { events?: StoredEvent[] };
  return Array.isArray(data) ? data : (data.events ?? []);
}

/** Map a stored (replayed) event row onto the live-stream event shape. */
export function storedToStream(ev: StoredEvent): StreamEvent | null {
  const d = (obj: Record<string, unknown>): StreamEvent => ({ type: ev.event_type, data: obj, eventId: ev.event_id ?? undefined, sequence: ev.sequence, storedId: ev.id });
  switch (ev.event_type) {
    case "text_delta":
      return d({ content: ev.content });
    case "thinking":
      return d({ content: ev.content, done: ev.metadata?.done === true });
    case "user_message":
      return d({ id: ev.event_id ?? undefined, content: ev.content, queued: ev.metadata?.queued === true, source: ev.metadata?.source, messages: ev.metadata?.messages });
    case "assistant_message":
    case "assistant_message_canonical":
      // Exact generated remote-job status rows, not ordinary assistant prose.
      if (isGeneratedRemoteJobStatus(ev)) return null;
      // Canonical rows are the finalized text_op bubble; treat both as the
      // turn's final message (the reducer dedupes identical text).
      return { ...d({ content: ev.content, success: ev.metadata?.success !== false, canonical: ev.event_type === "assistant_message_canonical", bubble_id: ev.metadata?.bubble_id }), type: "assistant_message" };
    case "text_op": {
      let ops: unknown = [];
      try {
        ops = JSON.parse(ev.content);
      } catch {
        ops = [];
      }
      return d({ bubble_id: ev.metadata?.bubble_id, ops });
    }
    case "tool_call": {
      let args: unknown = null;
      try {
        args = JSON.parse(ev.content);
      } catch {
        args = ev.content;
      }
      return d({ tool_call_id: ev.tool_call_id ?? `event-${ev.id}`, name: ev.tool_name ?? "tool", args });
    }
    case "tool_result": {
      let result: unknown = ev.content;
      try {
        result = JSON.parse(ev.content);
      } catch {
        /* plain string */
      }
      return d({ tool_call_id: ev.tool_call_id ?? "", name: ev.tool_name ?? "tool", result });
    }
    case "error":
      return d({ message: ev.content, resumable: false });
    default:
      return null;
  }
}

/**
 * Subscribe to the mission's live SSE stream (`/api/control/stream?mission=`).
 * EventSource can't carry the Authorization header, so this parses SSE frames
 * off a fetch ReadableStream. Reconnects with backoff until the returned
 * cleanup is called; `onLagged` fires when the server reports dropped events
 * (the caller should refetch and resync).
 */
export function streamMission(
  missionId: string,
  onEvent: (ev: StreamEvent) => void,
  onLagged: () => void,
): () => void {
  let stopped = false;
  let retry = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let controller: AbortController | undefined;

  const connect = async () => {
    if (stopped) return;
    controller = new AbortController();
    try {
      const jwt = getJwt();
      const res = await fetch(`${getApiUrl()}/api/control/stream?mission=${missionId}`, {
        headers: {
          Accept: "text/event-stream",
          ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
        },
        signal: controller.signal,
      });
      if (res.status === 401) {
        // Token expired/revoked: reconnecting would loop forever.
        clearConnection();
        stopped = true;
        return;
      }
      if (!res.ok || !res.body) throw new Error(`stream ${res.status}`);
      if (retry > 0) {
        // Events emitted while we were disconnected never reached us.
        onLagged();
      }
      retry = 0;
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true }).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
        let idx = buffer.indexOf("\n\n");
        while (idx !== -1) {
          const raw = buffer.slice(0, idx);
          buffer = buffer.slice(idx + 2);
          idx = buffer.indexOf("\n\n");
          let eventType = "message";
          let data = "";
          for (const line of raw.split("\n")) {
            if (line.startsWith("event:")) eventType = line.slice(6).trim();
            else if (line.startsWith("data:")) data += line.slice(5).trim();
          }
          if (!data) continue;
          if (eventType === "stream_lagged") {
            onLagged();
            continue;
          }
          try {
            const parsed = JSON.parse(data);
            onEvent({ type: eventType, data: parsed, eventId: parsed.event_id ?? parsed.id, sequence: parsed.sequence });
          } catch {
            /* malformed frame — skip */
          }
        }
      }
      throw new Error("stream ended");
    } catch (e) {
      if (stopped || (e instanceof DOMException && e.name === "AbortError")) return;
      retry = Math.min(retry + 1, 5);
      timer = setTimeout(() => void connect(), Math.min(1000 * 2 ** retry, 15000));
    }
  };

  void connect();
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
    controller?.abort();
  };
}

/** Trim only overlap proven by event identity/sequence or a tool boundary.
 * Text contents are deliberately never an identity: two real replies may match. */
export function heldAfterHistory(history: StreamEvent[], held: StreamEvent[]): StreamEvent[] {
  const identity = (event: StreamEvent): string | undefined => {
    const id = event.eventId ?? event.data.id;
    if (id != null) return `${event.type}:${id}`;
    if (event.sequence != null) return `sequence:${event.sequence}:${event.type}`;
    if ((event.type === "tool_call" || event.type === "tool_result") && event.data.tool_call_id)
      return `${event.type}:${event.data.tool_call_id}`;
    return undefined;
  };
  const known = new Set(history.map(identity).filter(Boolean));
  // Stored rows also retain tool ids when they have sequence metadata.
  for (const event of history) if (event.type === "tool_call" || event.type === "tool_result") known.add(`${event.type}:${event.data.tool_call_id}`);
  let cut = 0;
  held.forEach((event,index) => { const key=identity(event); if(key && known.has(key)) cut=index+1; });
  return held.filter((event, index) => event.type === "user_message" || index >= cut);
}
