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
