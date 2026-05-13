/**
 * Dev-only fetch throttle: lets us simulate slow connections without using
 * Chrome's network throttling. Reads `localStorage.slowFetch` (milliseconds)
 * on every request and delays the response by that much.
 *
 * Toggle from the browser console:
 *   __slowFetch.set(2000)   // every fetch waits 2s
 *   __slowFetch.clear()
 *   __slowFetch.get()
 *
 * Disabled in production builds. Returns a no-op there.
 */

const LS_KEY = "slowFetch";

let installed = false;

export function installFetchThrottle(): void {
  if (installed) return;
  if (typeof window === "undefined") return;
  if (process.env.NODE_ENV === "production") return;

  installed = true;

  const original = window.fetch.bind(window);
  window.fetch = async (...args) => {
    const delay = readDelay();
    if (delay > 0) {
      await new Promise<void>((resolve) => window.setTimeout(resolve, delay));
    }
    return original(...args);
  };

  const helpers = {
    set(ms: number) {
      if (!Number.isFinite(ms) || ms < 0) {
        console.warn("[slowFetch] expected a non-negative number");
        return;
      }
      window.localStorage.setItem(LS_KEY, String(Math.floor(ms)));
      console.info(`[slowFetch] every fetch will now wait ${Math.floor(ms)}ms`);
    },
    clear() {
      window.localStorage.removeItem(LS_KEY);
      console.info("[slowFetch] disabled");
    },
    get() {
      return readDelay();
    },
  };

  // Stash on window so it's discoverable from devtools without imports.
  (window as unknown as { __slowFetch: typeof helpers }).__slowFetch = helpers;

  console.info(
    `[slowFetch] installed (current delay: ${readDelay()}ms). ` +
      `Use __slowFetch.set(ms) / __slowFetch.clear() in the console.`
  );
}

function readDelay(): number {
  try {
    const raw = window.localStorage.getItem(LS_KEY);
    if (!raw) return 0;
    const num = Number.parseInt(raw, 10);
    return Number.isFinite(num) && num > 0 ? num : 0;
  } catch {
    return 0;
  }
}
