import { Select } from "./Select";
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { ErrorNotice } from "./ErrorNotice";
import { cancelMission, getMission, type HarnessChoice, type Mission } from "./api";
import { machineIdentity, nativeInvoke } from "./clientRuns";
import { localBinding, localRunActive, pollLocal, refreshLocalAgents, stopLocal } from "./localAgents";
import { appendClientTranscript, setClientMissionStatus } from "./api";
import { activeTransfer, activateTransfer, copyTransfer, inspectTransfer, machineLabel, sameMachine, snapshotTransfer, transferRequest, verifyTransfer, type Destination, type Machine, type TransferAction } from "./machineTransfer";

export function ChangeMachine(p: { mission: Mission; choices: HarnessChoice[]; onClose: () => void; onMoved: (mission: Mission) => void }) {
  const [destinations, setDestinations] = createSignal<Destination[]>([]);
  const [selected, setSelected] = createSignal<Destination>();
  const [action, setAction] = createSignal<TransferAction>();
  const [backend, setBackend] = createSignal(p.mission.backend ?? "");
  const [model, setModel] = createSignal(p.mission.model_override ?? "");
  const [loading, setLoading] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [stage, setStage] = createSignal("");
  const [progress, setProgress] = createSignal(0);
  const [error, setError] = createSignal("");
  let cancelled = false;
  let alive = true;
  let root!: HTMLDivElement;
  let client: string | undefined;
  const requestKey = crypto.randomUUID();
  const current = (): Machine => p.mission.machine_transfer?.destination ?? (p.mission.tags?.includes("placement:client") ? { kind: "client", id: client ?? "unknown" } : p.mission.remote_node_id || p.mission.remote_job?.node_id ? { kind: "node", id: (p.mission.remote_node_id ?? p.mission.remote_job?.node_id)! } : { kind: "core" });
  const running = () => localRunActive(p.mission.id) || ["active", "pending", "running", "starting"].includes(p.mission.status);
  const models = () => p.choices.find(c => c.backend.id === backend())?.models ?? [];
  const availableHarnesses = () => p.choices.filter(c => !selected()?.harnesses || selected()!.harnesses!.includes(c.backend.id));
  const compatible = () => !selected()?.harnesses || selected()!.harnesses!.includes(backend());
  const fail = (e: unknown) => setError(e instanceof Error ? e.message : String(e));
  const load = async () => {
    setLoading(true); setError("");
    try {
      const view = await inspectTransfer(p.mission.id);
      if (view.version !== 1) throw new Error("Update the connected backend to enable machine transfer.");
      const rows = [...view.destinations];
      if (nativeInvoke()) {
        client = await machineIdentity();
        const installed = await refreshLocalAgents();
        rows.unshift({ machine: { kind: "client", id: client }, label: "This computer", available: true, harnesses: installed.filter(c => c.installed).map(c => c.id) });
      } else rows.unshift({ machine: { kind: "client", id: "unavailable" }, label: "This computer", available: false, reason: "Open Orb desktop to use this computer" });
      if (!alive) return;
      setDestinations(rows);
      queueMicrotask(() => root?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus());
      const pending = [...view.actions].reverse().find(activeTransfer);
      if (pending) {
        setAction(pending); setBackend(pending.backend); setModel(pending.model ?? "");
        setSelected(rows.find(r => sameMachine(r.machine, pending.destination)) ?? { machine: pending.destination, label: machineLabel(pending.destination), available: true });
      }
    } catch (e) { if (alive) fail(e); }
    finally { if (alive) setLoading(false); }
  };
  onMount(() => {
    void load();
    const close = (e: PointerEvent) => { if (!busy() && !action() && !root.parentElement?.contains(e.target as Node)) p.onClose(); };
    window.addEventListener("pointerdown", close);
    onCleanup(() => window.removeEventListener("pointerdown", close));
  });
  onCleanup(() => { alive = false; cancelled = true; });
  const stopSource = async () => {
    if (current().kind === "client") {
      await stopLocal(p.mission.id);
      const result = await pollLocal(p.mission.id);
      if (!result.done) throw new Error("The local agent has not stopped yet.");
      // A normal completion handler may already have saved/settled this run.
      const latest = await getMission(p.mission.id);
      if (["active", "pending"].includes(latest.status)) {
        try {
          if (result.text.trim()) await appendClientTranscript(p.mission.id, "assistant", result.text);
          await setClientMissionStatus(p.mission.id, "interrupted");
        } catch (error) {
          if (["active", "pending"].includes((await getMission(p.mission.id)).status)) throw error;
        }
      }
    } else {
      await cancelMission(p.mission.id);
      for (let i = 0; i < 40; i++) {
        const latest = await getMission(p.mission.id);
        if (!["active", "pending"].includes(latest.status) && (!latest.execution?.state || latest.execution.state === "terminal")) return;
        await new Promise(resolve => setTimeout(resolve, 250));
      }
      throw new Error("The source has not confirmed termination. Wait, then retry.");
    }
  };
  const prepare = async () => {
    const target = selected(); if (!target || busy()) return;
    cancelled = false; setBusy(true); setError(""); setStage("Preparing workspace…");
    try {
      if (current().kind === "client" && !localBinding(p.mission.id)) throw new Error("Open this conversation on its source computer before moving it.");
      if (running()) await stopSource();
      let a: TransferAction = action() ?? await transferRequest<TransferAction>(p.mission.id, { op: "prepare", destination: target.machine, client_id: client, client_root: localBinding(p.mission.id)?.cwd, idempotency_key: requestKey, backend: backend(), model: model(), effort: backend() === p.mission.backend ? p.mission.model_effort : "" });
      setAction(a); a = await snapshotTransfer(a); setAction(a); setStage("");
    } catch (e) { fail(e); } finally { if (cancelled && action()) await cancel(); setBusy(false); }
  };
  const move = async () => {
    let a = action(); if (!a || busy()) return;
    cancelled = false; setBusy(true); setError("");
    try {
      setStage("Copying workspace…");
      a = await copyTransfer(a, (done, total) => setProgress(total ? done / total : 1), () => cancelled);
      if (cancelled) throw new Error("Transfer cancelled before activation.");
      setStage("Verifying destination…"); a = await verifyTransfer(a); setAction(a);
      if (cancelled) throw new Error("Transfer cancelled before activation.");
      setStage("Activating destination…"); const mission = await activateTransfer(a);
      p.onMoved(mission); p.onClose();
    } catch (e) {
      fail(e);
      if (cancelled) await cancel();
    } finally { setBusy(false); }
  };
  const cancel = async () => {
    const a = action();
    try { if (a) await transferRequest(p.mission.id, { op: "cancel", transfer_id: a.id }); p.onClose(); }
    catch (e) { fail(e); }
  };
  const keys = (e: KeyboardEvent) => {
    if (e.key === "Escape" && !busy()) { e.preventDefault(); if (action()) void cancel(); else p.onClose(); }
    if (["ArrowUp", "ArrowDown"].includes(e.key) && !selected()) {
      const items = Array.from(root.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)'));
      const index = items.indexOf(document.activeElement as HTMLButtonElement);
      e.preventDefault(); items[(index + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
    }
  };
  return <div ref={root} class="menu machine-transfer-menu" role={selected() ? "dialog" : "menu"} aria-label="Change machine" onKeyDown={keys}>
    <div class="menu-group">{selected() ? `Continue on ${selected()!.label}` : "Change machine…"}</div>
    <Show when={loading()}><div class="menu-group" role="status">Checking machines…</div></Show>
    <Show when={!selected()}>
      <For each={destinations()}>{d => <button class="menu-item" role="menuitem" disabled={!d.available || sameMachine(d.machine, current())} title={d.reason ?? d.label} onClick={() => { setSelected(d); setError(""); queueMicrotask(() => root.querySelector<HTMLSelectElement>("select")?.focus()); }}>
        <span>{d.label}<Show when={d.reason}><small>{d.reason}</small></Show></span><span>{sameMachine(d.machine, current()) ? "✓" : "›"}</span>
      </button>}</For>
    </Show>
    <Show when={selected()}>
      <div class="transfer-body">
        <p>Move this conversation and its workspace files. The agent will wait for your next message.</p>
        <Show when={!action()}>
          <label>Harness<Select aria-label="Transfer harness" value={backend()} disabled={busy()} onChange={e => { setBackend(e.currentTarget.value); setModel(p.choices.find(c => c.backend.id === e.currentTarget.value)?.models[0]?.value ?? ""); }}>
            <Show when={!compatible()}><option value={backend()} disabled>{backend()} — unavailable</option></Show>
            <For each={availableHarnesses()}>{c => <option value={c.backend.id}>{c.backend.name}</option>}</For>
          </Select></label>
          <label>Model<Select aria-label="Transfer model" value={model()} disabled={busy()} onChange={e => setModel(e.currentTarget.value)}><For each={models()}>{m => <option value={m.value}>{m.label}</option>}</For></Select></label>
          <Show when={!compatible()}><p>Choose a harness available on this machine.</p></Show>
        </Show>
        <Show when={action()?.manifest}>{m => <details><summary>{m().files.length} files · {(m().bytes / 1024 / 1024).toFixed(1)} MiB</summary><ul><For each={m().files}>{f => <li>{f.path}</li>}</For></ul><Show when={m().excluded.length}><p>Excluded credentials, generated configuration and caches:</p><ul><For each={m().excluded}>{f => <li>{f}</li>}</For></ul></Show></details>}</Show>
        <Show when={busy()}><div role="status">{stage()}</div><Show when={stage().startsWith("Copying")}><progress max="1" value={progress()} /></Show></Show>
        <div class="transfer-actions">
          <button class="pill" disabled={stage() === "Activating destination…" && busy()} onClick={() => busy() ? cancelled = true : void cancel()}>Cancel</button>
          <Show when={action()?.manifest} fallback={<button class="pill on" disabled={busy() || !compatible() || !model()} onClick={() => void prepare()}>{running() ? "Stop and prepare" : "Prepare transfer"}</button>}>
            <button class="pill on" disabled={busy()} onClick={() => void move()}>Move to {selected()!.label}</button>
          </Show>
        </div>
      </div>
    </Show>
    <Show when={error()}><ErrorNotice error={error()} /><Show when={!selected()}><button class="menu-item" onClick={() => void load()}>Retry</button></Show></Show>
  </div>;
}
