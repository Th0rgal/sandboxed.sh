'use client';

import { useEffect } from 'react';
import { getRuntimeApiBase } from '@/lib/settings';

/**
 * Injects `<link rel="preconnect">` and `<link rel="dns-prefetch">` for the
 * configured API backend at the very top of the React tree. Browsers handle
 * the warmup (DNS + TCP + TLS) in parallel with React mounting, so the first
 * real fetch finds the connection already established. On a cold visit to a
 * remote backend this typically saves 100–300 ms of handshake before TTFB.
 *
 * No-op when the API base is same-origin (browser already has the connection)
 * or when it matches `window.location.origin`.
 */
export function BackendPreconnect() {
  useEffect(() => {
    if (typeof window === 'undefined' || typeof document === 'undefined') return;
    let base: string;
    try {
      base = getRuntimeApiBase();
    } catch {
      return;
    }
    if (!base || base === window.location.origin) return;

    let origin: string;
    try {
      origin = new URL(base).origin;
    } catch {
      return;
    }

    const already = document.head.querySelector(`link[data-oa-preconnect="${origin}"]`);
    if (already) return;

    const preconnect = document.createElement('link');
    preconnect.rel = 'preconnect';
    preconnect.href = origin;
    preconnect.crossOrigin = 'anonymous';
    preconnect.setAttribute('data-oa-preconnect', origin);
    document.head.appendChild(preconnect);

    const dnsPrefetch = document.createElement('link');
    dnsPrefetch.rel = 'dns-prefetch';
    dnsPrefetch.href = origin;
    dnsPrefetch.setAttribute('data-oa-preconnect-dns', origin);
    document.head.appendChild(dnsPrefetch);
  }, []);

  return null;
}
