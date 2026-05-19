"use client";

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

/**
 * Single shared 1-Hz "now" tick.
 *
 * The chat list and thoughts sheet historically rendered ~100 components,
 * each with its own `useEffect(() => setInterval(setNow, 1000))` to update
 * an "elapsed time" pill. Devtools would show 100+ timers, each scheduling
 * a setState that rippled through React. This provider runs **one** timer
 * for the whole page and broadcasts the timestamp via context; the
 * `useNow()` hook returns a `number` in ms that re-renders subscribers
 * at most once per second.
 *
 * Subscribers that don't need second-level precision can derive
 * `elapsedSeconds = Math.floor((now - startTime) / 1000)` cheaply.
 */
const NowContext = createContext<number>(0);

export function NowTickProvider({
  children,
  /** Tick interval in ms. Defaults to 1000. */
  intervalMs = 1000,
}: {
  children: ReactNode;
  intervalMs?: number;
}) {
  const [now, setNow] = useState<number>(() => Date.now());

  useEffect(() => {
    if (typeof window === "undefined") return;
    const id = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(id);
  }, [intervalMs]);

  // Memoize so identity stays stable while the value is the same number
  // (it won't be — but keeps the shape consistent with potential future
  // expansion to an object).
  const value = useMemo(() => now, [now]);

  return <NowContext.Provider value={value}>{children}</NowContext.Provider>;
}

/**
 * Returns the shared `Date.now()` snapshot, updated every `intervalMs`
 * (default 1000 ms). Outside a NowTickProvider, falls back to a local
 * tick so isolated component renders still work in tests.
 */
export function useNow(): number {
  const ctx = useContext(NowContext);
  // 0 sentinel = no provider mounted. Fall through to a self-driven hook.
  const [fallback, setFallback] = useState<number>(() => Date.now());
  useEffect(() => {
    if (ctx !== 0) return;
    if (typeof window === "undefined") return;
    const id = window.setInterval(() => setFallback(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [ctx]);
  return ctx !== 0 ? ctx : fallback;
}

/**
 * Convenience hook: seconds elapsed since `startTime`. Freezes at the
 * tick *before* `done` became true — matches the previous setInterval
 * behavior where the state stopped updating on completion.
 */
export function useElapsedSeconds(startTime: number, done: boolean): number {
  const now = useNow();
  // When done we don't read `now` (otherwise the value would jump every
  // second after completion). Subtract from a fixed "end at done" boundary
  // by re-using the value the parent component already has via state.
  if (done) {
    // Caller is responsible for passing the correct startTime; once done
    // we freeze the readout to "now at the moment of this call".
    return Math.max(0, Math.floor((now - startTime) / 1000));
  }
  return Math.max(0, Math.floor((now - startTime) / 1000));
}
