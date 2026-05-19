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

const DB_NAME = "openagent.event-cache";
const DB_VERSION = 1;
const STORE = "missions";

/**
 * Per-mission cap on cached events. Newest entries are kept on overflow.
 * Sized to comfortably exceed the typical `INITIAL_HISTORY_PAGE_SIZE` so a
 * cache hit on reopen surfaces noticeably more context than a cold load,
 * while keeping per-mission storage well under a megabyte for typical
 * `tool_result` payload sizes (~1–2 KB on average).
 */
const MAX_CACHED_EVENTS = 400;

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
  missionId: string
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
  totalEvents: number
): Promise<void> {
  if (!missionId || events.length === 0) return;
  const db = await openDb();
  if (!db) return;

  // Keep only the newest tail. Sort defensively in case callers passed an
  // unsorted set — cheap on small arrays and stops a stale `sequence` row
  // from leaking past the slice.
  const sorted =
    events.length > 1
      ? events.slice().sort((a, b) => a.sequence - b.sequence)
      : events.slice();
  const trimmed =
    sorted.length > MAX_CACHED_EVENTS
      ? sorted.slice(sorted.length - MAX_CACHED_EVENTS)
      : sorted;

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
