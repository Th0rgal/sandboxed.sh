import { Show, createMemo } from "solid-js";
export type ResourceSample = { time: number; cpu?: number | null; memory?: number | null };
export function appendSamples(previous: ResourceSample[], incoming: ResourceSample[]) {
  const samples = [...new Map([...previous, ...incoming].filter(s => Number.isFinite(s.time)).map(s => [s.time, s])).values()].sort((a, b) => a.time - b.time);
  const end = samples.at(-1)?.time ?? 0;
  return samples.filter(s => s.time >= end - 120000).slice(-240);
}
export function ResourceHistory(p: { samples: ResourceSample[]; live: boolean }) {
  const end = () => p.samples.at(-1)?.time ?? Date.now();
  const path = (key: "cpu" | "memory") => {
    let previous = 0;
    return p.samples.map(s => {
      const v = s[key];
      if (v == null || !Number.isFinite(v)) { previous = 0; return ""; }
      const command = previous && s.time - previous < 30000 ? "L" : "M";
      previous = s.time;
      return `${command}${((s.time - (end() - 120000)) / 120000 * 600).toFixed(1)},${(72 - Math.min(100, Math.max(0, v)) / 100 * 68).toFixed(1)}`;
    }).join(" ");
  };
  const cpu = createMemo(() => p.samples.some(s => s.cpu != null));
  return <div class="resource-history">
    <div class="history-heading"><span>Usage history</span><span class={`metrics-live ${p.live ? "on" : ""}`} role="img" aria-label={p.live ? "Receiving metrics" : "Metrics disconnected"} title={p.live ? "Receiving metrics" : "Disconnected · last samples"} /></div>
    <svg viewBox="0 0 600 80" preserveAspectRatio="none" role="img" aria-label="CPU and memory usage over the last two minutes">
      <path d="M0 4H600 M0 38H600 M0 72H600" class="history-grid" />
      <Show when={cpu()}><path d={path("cpu")} class="history-cpu" /></Show><path d={path("memory")} class="history-memory" />
      <Show when={p.samples.length === 1}><circle cx="600" cy={72 - (p.samples[0].memory ?? 0) / 100 * 68} r="2" class="history-dot" /></Show>
    </svg>
    <div class="history-legend"><span>−2 min</span><span class="history-keys"><Show when={cpu()}><i class="cpu-key" />CPU</Show><i />RAM</span><span>Latest · 0–100%</span></div>
  </div>;
}
