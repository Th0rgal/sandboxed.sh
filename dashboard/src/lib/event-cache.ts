/**
 * IndexedDB-backed cache for the per-mission event history.
 *
 * Why this exists: the `/api/control/missions/<id>/events` endpoint is the
 * single slowest call on the control page — a mission with a few hundred
 * tool-heavy events can take 20+ seconds to come back and weigh in at over a
 * megabyte. On a fresh load there's nothing to do but wait, but on reopen we
 * already know what most of those events look like.
 *
 * What this stores: a capped tail of the raw `StoredEvent` rows for each
 * mission (newest `MAX_CACHED_EVENTS`), plus the server's last reported
 * `maxSequence` / `totalEvents`. On reopen, the consumer can render the
 * cached events immediately and then issue a small `since_seq` request for
 * just the delta — turning a 20-second wait into a near-instant repaint plus
 * a sub-second tail fetch.
 *
 * Why IDB over localStorage: a long mission's event tail can exceed 1 MB
 * once `tool_result` payloads are included. localStorage's 5–10 MB shared
 * budget runs out fast across multiple missions and writes block the main
 * thread; IDB writes are async and per-mission storage is naturally
 * bounded by the cap.
 */

import type { StoredEvent } from "./api/missions";
import { isStreamContinuation } from "./stream-continuation";

const DB_NAME = "openagent.event-cache";
const DB_VERSION = 1;
const STORE = "missions";

/**
 * Per-mission cap on cached events. Newest entries are kept on overflow.
 * Sized to keep the transcript plus the recent deferred trace together for
 * active missions. If this is too small, reopen renders chat first and then
 * has to refetch thoughts/tools as a second pass, which causes a visible
 * redraw.
 */
const MAX_CACHED_EVENTS = 1_500;

function compactEventsForCache(events: StoredEvent[]): StoredEvent[] {
  const sorted =
    events.length > 1
      ? events.slice().sort((a, b) => a.sequence - b.sequence)
      : events.slice();
  const compacted: StoredEvent[] = [];
  let thinkingRun: {
    first: StoredEvent;
    latest: StoredEvent;
    content: string;
  } | null = null;

  const flushThinking = () => {
    if (!thinkingRun) return;
    compacted.push({
      ...thinkingRun.first,
      content: thinkingRun.content,
      metadata: {
        ...thinkingRun.first.metadata,
        ...thinkingRun.latest.metadata,
      },
      timestamp: thinkingRun.latest.timestamp,
    });
    thinkingRun = null;
  };

  for (const event of sorted) {
    if (event.event_type !== "thinking") {
      flushThinking();
      compacted.push(event);
      continue;
    }

    const content = event.content || "";
    const done = event.metadata?.done === true;
    if (!thinkingRun) {
      thinkingRun = { first: event, latest: event, content };
      if (done) flushThinking();
      continue;
    }

    if (!isStreamContinuation(content, thinkingRun.content)) {
      flushThinking();
      thinkingRun = { first: event, latest: event, content };
      if (done) flushThinking();
      continue;
    }

    thinkingRun.latest = event;
    if (content.length > thinkingRun.content.length) {
      thinkingRun.content = content;
    }
    if (done) flushThinking();
  }

  flushThinking();
  return compacted;
}

function trimEventsForCache(events: StoredEvent[]): StoredEvent[] {
  if (events.length <= MAX_CACHED_EVENTS) return events;

  const mustKeepTypes = new Set(["thinking", "text_delta", "text_op"]);
  const keep = new Set<number>();
  const mustKeep = events.filter((event) =>
    mustKeepTypes.has(event.event_type),
  );

  const protectedEvents =
    mustKeep.length > MAX_CACHED_EVENTS
      ? mustKeep.slice(mustKeep.length - MAX_CACHED_EVENTS)
      : mustKeep;
  for (const event of protectedEvents) {
    keep.add(event.sequence);
  }

  let remaining = MAX_CACHED_EVENTS - keep.size;
  for (let i = events.length - 1; i >= 0 && remaining > 0; i -= 1) {
    const event = events[i];
    if (!keep.has(event.sequence)) {
      keep.add(event.sequence);
      remaining -= 1;
    }
  }

  return events.filter((event) => keep.has(event.sequence));
}

/** Drop entries this old at read time — server state may have diverged
 * (mission deleted, rebuilt, etc.) and we'd rather miss the cache than
 * render bogus history. */
const STALE_AFTER_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

export interface CachedEvents {
  missionId: string;
  events: StoredEvent[];
  /** `X-Max-Sequence` from the most recent network response. Used as the
   * `since_seq` cursor on the next reopen so the delta fetch only carries
   * events that arrived after the cache was written. */
  maxSequence: number;
  /** `X-Total-Events` from the most recent network response, used to drive
   * the "Load older messages" button's `hasMore` heuristic when the cache
   * was last refreshed. */
  totalEvents: number;
  /** Wall-clock time of the most recent write. Used to expire stale rows
   * at read time. */
  updatedAt: number;
}

let dbPromise: Promise<IDBDatabase | null> | null = null;

function openDb(): Promise<IDBDatabase | null> {
  if (typeof window === "undefined" || !("indexedDB" in window)) {
    return Promise.resolve(null);
  }
  if (dbPromise) return dbPromise;
  dbPromise = new Promise<IDBDatabase | null>((resolve) => {
    let req: IDBOpenDBRequest;
    try {
      req = window.indexedDB.open(DB_NAME, DB_VERSION);
    } catch {
      resolve(null);
      return;
    }
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: "missionId" });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => resolve(null);
    req.onblocked = () => resolve(null);
  });
  return dbPromise;
}

export async function readCachedEvents(
  missionId: string,
): Promise<CachedEvents | null> {
  const db = await openDb();
  if (!db) return null;
  return new Promise<CachedEvents | null>((resolve) => {
    let tx: IDBTransaction;
    try {
      tx = db.transaction(STORE, "readonly");
    } catch {
      resolve(null);
      return;
    }
    const req = tx.objectStore(STORE).get(missionId);
    req.onsuccess = () => {
      const value = req.result as CachedEvents | undefined;
      if (!value) {
        resolve(null);
        return;
      }
      // Stale entries are dropped at read time. The next write will
      // overwrite them; we don't bother deleting eagerly because the
      // common case is a cache hit followed by a refresh write.
      if (
        typeof value.updatedAt !== "number" ||
        Date.now() - value.updatedAt > STALE_AFTER_MS
      ) {
        resolve(null);
        return;
      }
      if (!Array.isArray(value.events) || value.events.length === 0) {
        resolve(null);
        return;
      }
      resolve(value);
    };
    req.onerror = () => resolve(null);
  });
}

/**
 * Persist (or update) the cached tail for a mission. `events` may contain
 * more than `MAX_CACHED_EVENTS`; the cache keeps only the newest slice.
 * Existing cached events outside that window are dropped.
 *
 * Best-effort: all errors are swallowed. A failed write is no worse than
 * a cache miss on the next visit.
 */
export async function writeCachedEvents(
  missionId: string,
  events: StoredEvent[],
  maxSequence: number,
  totalEvents: number,
): Promise<void> {
  if (!missionId || events.length === 0) return;
  const db = await openDb();
  if (!db) return;

  // Keep only the newest render-equivalent tail. Thinking streams are
  // cumulative, so hundreds of intermediate rows often render as one final
  // thought; compact them before applying the cache cap.
  const sorted = compactEventsForCache(events);
  const trimmed = trimEventsForCache(sorted);

  const record: CachedEvents = {
    missionId,
    events: trimmed,
    maxSequence,
    totalEvents,
    updatedAt: Date.now(),
  };

  await new Promise<void>((resolve) => {
    let tx: IDBTransaction;
    try {
      tx = db.transaction(STORE, "readwrite");
    } catch {
      resolve();
      return;
    }
    const req = tx.objectStore(STORE).put(record);
    req.onsuccess = () => resolve();
    req.onerror = () => resolve();
    tx.onerror = () => resolve();
    tx.onabort = () => resolve();
  });
}

/**
 * Drop the cached row for a mission. Used when the server reports a state
 * that's inconsistent with our cache (mission deleted, sequence regressed)
 * so the next load can't render bogus history.
 */
export async function deleteCachedEvents(missionId: string): Promise<void> {
  if (!missionId) return;
  const db = await openDb();
  if (!db) return;
  await new Promise<void>((resolve) => {
    let tx: IDBTransaction;
    try {
      tx = db.transaction(STORE, "readwrite");
    } catch {
      resolve();
      return;
    }
    const req = tx.objectStore(STORE).delete(missionId);
    req.onsuccess = () => resolve();
    req.onerror = () => resolve();
    tx.onerror = () => resolve();
    tx.onabort = () => resolve();
  });
}

export const EVENT_CACHE_MAX = MAX_CACHED_EVENTS;
