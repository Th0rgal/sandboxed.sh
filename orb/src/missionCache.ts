import { buildTranscript, type StreamItem } from "./transcriptModel";
import { getMissionEvents, storedToStream, type StreamEvent } from "./stream";
import { cacheLoad, cachePeek, cachePrefetch, cachePut } from "./pageCache";

export type TranscriptSnap = { items: StreamItem[]; stream: StreamEvent[] };

const key = (id: string) => `m:${id}:tx`;
const heightKey = (id: string) => `m:${id}:h`;

export function peekTranscript(id: string): TranscriptSnap | undefined {
  return cachePeek<TranscriptSnap>(key(id));
}

export function peekTranscriptHeight(id: string): number | undefined {
  return cachePeek<number>(heightKey(id));
}

export function putTranscript(id: string, snap: TranscriptSnap) {
  cachePut(key(id), snap);
}

export function putTranscriptItems(id: string, items: StreamItem[]) {
  const prev = peekTranscript(id);
  cachePut(key(id), { items, stream: prev?.stream ?? [] });
}

export function putTranscriptHeight(id: string, height: number) {
  if (height > 0) cachePut(heightKey(id), Math.round(height));
}

async function fetchTranscript(id: string): Promise<TranscriptSnap> {
  const events = await getMissionEvents(id);
  const stream: StreamEvent[] = [];
  for (const row of events) {
    const ev = storedToStream(row);
    if (ev) stream.push(ev);
  }
  return { items: buildTranscript(stream), stream };
}

export function loadTranscript(id: string): Promise<TranscriptSnap> {
  return cacheLoad(key(id), () => fetchTranscript(id));
}

export function prefetchTranscript(id: string) {
  cachePrefetch(key(id), () => loadTranscript(id));
}
