import { getApiUrl, getJwt } from "./api";
import type { ResourceSample } from "./ResourceHistory";
const memory = new Map<string, ResourceSample[]>();
const written = new Map<string, number>();
export function historyScope() {
  let hash = 2166136261;
  for (const c of getJwt() ?? "") hash = Math.imul(hash ^ c.charCodeAt(0), 16777619);
  return `${getApiUrl()}:${hash >>> 0}`;
}
function key(machine: string, scope: string) {
  return `orb.metrics.v1:${machine === "local" ? "local" : scope}:${machine}`;
}
export function freshSamples(samples: ResourceSample[], now = Date.now()) {
  return [...new Map(samples.filter(s => Number.isFinite(s.time) && s.time >= now - 60000 && s.time <= now + 1000).map(s => [s.time, s])).values()].sort((a, b) => a.time - b.time).slice(-120);
}
export function readHistory(machine: string, scope = historyScope()): ResourceSample[] {
  const id = key(machine, scope);
  try {
    const parsed: unknown = memory.get(id) ?? JSON.parse(sessionStorage.getItem(id) ?? "[]");
    return Array.isArray(parsed) ? freshSamples(parsed.filter(s => s && typeof s === "object")) : [];
  } catch { return []; }
}
export function saveHistory(machine: string, samples: ResourceSample[], flush = false, scope = historyScope()) {
  const id = key(machine, scope), now = Date.now();
  const fresh = freshSamples(samples, now);
  memory.set(id, fresh);
  // Keep recent navigation immediate; batch synchronous storage writes.
  if (!flush && now - (written.get(id) ?? 0) < 5000) return;
  written.set(id, now);
  try { sessionStorage.setItem(id, JSON.stringify(fresh)); } catch { /* Memory cache still works. */ }
}
