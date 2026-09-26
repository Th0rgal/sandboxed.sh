/** Interval polling that respects the window: skipped while the document is
 * hidden, never overlaps an in-flight run, and fires once as soon as the
 * window becomes visible again. Returns a stop function. */
export function pollWhileVisible(run: () => void | Promise<unknown>, ms: number): () => void {
  let busy = false;
  const tick = () => {
    if (busy || document.visibilityState === "hidden") return;
    const r = run();
    if (r && typeof (r as Promise<unknown>).then === "function") {
      busy = true;
      void (r as Promise<unknown>).finally(() => {
        busy = false;
      });
    }
  };
  const timer = window.setInterval(tick, ms);
  const onVisible = () => {
    if (document.visibilityState === "visible") tick();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** Merge a fresh list into the previous one keeping object identity for
 * entries that did not change, so keyed `<For>` rows are not re-rendered on
 * every poll. Order and membership follow `next`. */
export function mergeById<T extends { id: string }>(prev: T[], next: T[]): T[] {
  if (prev.length === 0) return next;
  const byId = new Map(prev.map((p) => [p.id, p]));
  let changed = prev.length !== next.length;
  const out = next.map((n, i) => {
    const old = byId.get(n.id);
    if (old && shallowEqual(old, n)) {
      if (prev[i] !== old) changed = true;
      return old;
    }
    changed = true;
    return n;
  });
  return changed ? out : prev;
}

function shallowEqual(a: object, b: object): boolean {
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    const va = (a as Record<string, unknown>)[k];
    const vb = (b as Record<string, unknown>)[k];
    if (va === vb) continue;
    // Nested objects/arrays: compare by JSON as a cheap structural check.
    if (typeof va === "object" && typeof vb === "object" && va && vb && JSON.stringify(va) === JSON.stringify(vb)) continue;
    return false;
  }
  return true;
}
