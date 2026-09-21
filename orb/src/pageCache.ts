/** In-memory LRU for page payloads. First paint reads here; network
 * refreshes in the background and joins in-flight work so open + prefetch
 * share one request. */
const MAX = 32;
const recentsKey = "orb.recentPages";

type Entry<T> = { value: T; at: number };
const store = new Map<string, Entry<unknown>>();
const inflight = new Map<string, Promise<unknown>>();
let recents: string[] = loadRecents();

function loadRecents(): string[] {
  try {
    const raw = sessionStorage.getItem(recentsKey);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function saveRecents() {
  try {
    sessionStorage.setItem(recentsKey, JSON.stringify(recents.slice(0, 12)));
  } catch {
    /* quota */
  }
}

export function cachePeek<T>(key: string): T | undefined {
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) return undefined;
  store.delete(key);
  store.set(key, hit);
  return hit.value;
}

export function cacheAge(key: string): number | undefined {
  const hit = store.get(key);
  return hit ? Date.now() - hit.at : undefined;
}

export function cachePut<T>(key: string, value: T): T {
  if (store.has(key)) store.delete(key);
  store.set(key, { value, at: Date.now() });
  while (store.size > MAX) {
    const oldest = store.keys().next().value;
    if (oldest === undefined) break;
    store.delete(oldest);
  }
  return value;
}

/** Deduped fetch that fills the cache. On failure, keep a prior hit. */
export function cacheLoad<T>(key: string, load: () => Promise<T>): Promise<T> {
  const pending = inflight.get(key) as Promise<T> | undefined;
  if (pending) return pending;
  const p = load()
    .then((value) => cachePut(key, value))
    .catch((err) => {
      const hit = cachePeek<T>(key);
      if (hit !== undefined) return hit;
      throw err;
    })
    .finally(() => inflight.delete(key));
  inflight.set(key, p);
  return p;
}

export function cacheBusy(key: string): boolean {
  return inflight.has(key);
}

export function cacheRemember(key: string) {
  recents = [key, ...recents.filter((k) => k !== key)].slice(0, 12);
  saveRecents();
}

export function cacheRecents(): string[] {
  return recents.slice();
}

export function cacheCanPrefetch(): boolean {
  if (typeof document !== "undefined" && document.visibilityState === "hidden") return false;
  const mem = (navigator as { deviceMemory?: number }).deviceMemory;
  if (mem != null && mem < 2) return false;
  return true;
}

/** How many project trees to warm on connect. 0 when the machine is tight. */
export function prefetchProjectLimit(): number {
  if (!cacheCanPrefetch()) return 0;
  const mem = (navigator as { deviceMemory?: number }).deviceMemory;
  if (mem != null && mem < 4) return 8;
  if (mem != null && mem < 8) return 16;
  return 25;
}

type Job = { key: string; run: () => Promise<unknown> };
const queue: Job[] = [];
let active = 0;

function pump() {
  if (active > 0 || queue.length === 0 || !cacheCanPrefetch()) return;
  const job = queue.shift();
  if (!job) return;
  if (store.has(job.key) || inflight.has(job.key)) {
    pump();
    return;
  }
  active = 1;
  void job.run().finally(() => {
    active = 0;
    pump();
  });
}

function idle(cb: () => void) {
  const w = window as Window & { requestIdleCallback?: (fn: () => void, opts?: { timeout: number }) => number };
  if (typeof w.requestIdleCallback === "function") w.requestIdleCallback(cb, { timeout: 2500 });
  else window.setTimeout(cb, 160);
}

/** Low-priority fill. No-ops when the key is warm, in flight, or the machine is tight. */
export function cachePrefetch(key: string, run: () => Promise<unknown>) {
  if (!cacheCanPrefetch() || store.has(key) || inflight.has(key)) return;
  if (queue.some((j) => j.key === key)) return;
  queue.push({ key, run });
  if (queue.length > 8) queue.shift();
  idle(pump);
}

/** Test hook. */
export function cacheReset() {
  store.clear();
  inflight.clear();
  queue.length = 0;
  active = 0;
  recents = [];
}
