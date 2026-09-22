import { readHistory, saveHistory, freshSamples } from "./resourceCache";
import { ResourceHistory, appendSamples, type ResourceSample } from "./ResourceHistory";
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { LaptopIcon } from "./icons";
import { pollWhileVisible } from "./poll";

type Snapshot = {
  history?: ResourceSample[];
  gpu_percent?: number | null; cpu_percent: number | null; memory_used: number; memory_total: number;
  disk_used: number; disk_total: number;
  consumers: { label: string; memory: number; processes: number }[];
};
const size = (n: number) => n >= 1024 ** 3 ? `${(n / 1024 ** 3).toFixed(1)} GiB` : `${Math.round(n / 1024 ** 2)} MiB`;
const percent = (used: number, total: number) => total > 0 ? `${(used / total * 100).toFixed(1)}%` : "—";

export function LocalMachine() {
  const [history, setHistory] = createSignal<ResourceSample[]>(readHistory("local"));
  const [open, setOpen] = createSignal(false);
  const [sample, setSample] = createSignal<Snapshot>();
  const [error, setError] = createSignal("");
  onMount(() => {
    const invoke = (window as Window & { __TAURI__?: { core?: { invoke: (name: string) => Promise<Snapshot> } } }).__TAURI__?.core?.invoke;
    if (!invoke) { setError("Available in the Orb desktop app"); return; }
    let disposed = false;
    let pending = false;
    const refresh = async () => {
      if (pending) return;
      pending = true;
      try {
        const result = await invoke("local_machine_metrics");
        if (!disposed) { setSample(result); setHistory(old => freshSamples(appendSamples(old, result.history ?? [{ time: Date.now(), cpu: result.cpu_percent, gpu: result.gpu_percent, memory: result.memory_total > 0 ? result.memory_used / result.memory_total * 100 : null }]))); saveHistory("local", history()); setError(""); }
      } catch (e) { if (!disposed) setError(`Couldn’t refresh local metrics: ${String(e)}`); }
      finally { pending = false; }
    };
    void refresh();
    const stop = pollWhileVisible(refresh, 3000);
    onCleanup(() => { disposed = true; stop(); saveHistory("local", history(), true); });
  });
  return <div class="s-card local-machine"><button class="s-row p-acc-btn" aria-expanded={open()} onClick={() => setOpen(!open())}>
    <LaptopIcon size={18} /><div class="s-row-text"><div class="s-row-title">This Mac</div><div class="s-row-desc">{error() || "This computer"}</div></div>
    <Show when={sample()}>{s => <span class="s-row-desc">RAM {percent(s().memory_used, s().memory_total)}</span>}</Show>
    <span class={`chev p-acc-chev ${open() ? "open" : ""}`}>›</span>
  </button><Show when={open()}><div class="p-acc-body machine-expanded">
    <Show when={sample()} fallback={<p class="s-row-desc">{error() || "Reading local metrics…"}</p>}>{s => <>
      <ResourceHistory samples={history()} live={!error()} />
      <div class="machine-resources">
        <div class="machine-resource"><span>CPU</span><strong>{s().cpu_percent == null ? "Sampling…" : `${Math.round(s().cpu_percent!)}%`}</strong></div>
        <div class="machine-resource"><span>Memory</span><strong>{percent(s().memory_used, s().memory_total)}</strong><small>{size(s().memory_used)} / {size(s().memory_total)}</small></div>
        <div class="machine-resource"><span>Disk</span><strong>{percent(s().disk_used, s().disk_total)}</strong><small>{size(s().disk_used)} / {size(s().disk_total)}</small></div>
        <div class="machine-resource"><span>GPU</span><strong>{s().gpu_percent != null ? `${Math.round(s().gpu_percent!)}%` : "Unavailable"}</strong></div>
      </div>
      <Show when={s().consumers.some(c => c.processes > 0)}><div class="local-consumers" title="Resident process memory · % of total RAM. Shared pages may overlap; macOS-managed WebKit processes may be excluded."><For each={s().consumers}>{(c, index) => <Show when={c.processes > 0}><div class="local-consumer"><span class="consumer-label"><i style={{ background: ["#92968d", "#a397bc", "#849ca4"][index()] }} />{c.label}</span><span>{size(c.memory)}</span><span>{percent(c.memory, s().memory_total)}</span></div></Show>}</For></div></Show>
    </>}</Show>
  </div></Show></div>;
}
