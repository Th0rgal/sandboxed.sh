import { For, Show, createSignal } from "solid-js";
import { Dialog } from "./Dialog";
import { forkMission, shortModelLabel, type HarnessChoice, type Mission } from "./api";
import { effortLabel, supportedEfforts } from "./effort";

export function ForkMission(p: { mission: Mission; choices: HarnessChoice[]; destination: string; onClose: () => void; onFork: (mission: Mission) => void }) {
  const [backend, setBackend] = createSignal(p.mission.backend ?? p.choices[0]?.backend.id ?? "");
  const choices = () => p.choices.find(c => c.backend.id === backend())?.models ?? [];
  const [model, setModel] = createSignal(choices().find(m => m.value === p.mission.model_override)?.value ?? choices()[0]?.value ?? "");
  const [effort, setEffort] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const key = crypto.randomUUID();
  const fork = async () => {
    if (busy() || !backend() || !model()) return;
    setBusy(true); setError("");
    try {
      p.onFork(await forkMission(p.mission.id, { backend: backend(), model_override: model(), model_effort: effort(), idempotency_key: key }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally { setBusy(false); }
  };
  return <Dialog title="Fork conversation" onClose={() => { if (!busy()) p.onClose(); }} footer={<>
    <button class="s-btn quiet" disabled={busy()} onClick={p.onClose}>Cancel</button>
    <button class="s-btn" disabled={busy() || !model()} onClick={() => void fork()}>{busy() ? "Forking…" : "Fork and continue"}</button>
  </>}>
    <p class="fork-summary">Continue in a new mission with the original prompt and conversation history.</p>
    <div class="fork-fields">
      <label>Harness<select aria-label="Harness" class="s-input" value={backend()} disabled={busy()} onChange={e => { setBackend(e.currentTarget.value); setModel(choices()[0]?.value ?? ""); setEffort(""); }}>
        <For each={p.choices}>{c => <option value={c.backend.id}>{c.backend.name}</option>}</For>
      </select></label>
      <label>Model<select aria-label="Model" class="s-input" value={model()} disabled={busy()} onChange={e => setModel(e.currentTarget.value)}>
        <For each={choices()}>{m => <option value={m.value}>{shortModelLabel(m.label)}</option>}</For>
      </select></label>
      <Show when={supportedEfforts(backend()).length}><label>Effort<select aria-label="Effort" class="s-input" value={effort()} disabled={busy()} onChange={e => setEffort(e.currentTarget.value)}>
        <option value="">Default</option><For each={supportedEfforts(backend())}>{e => <option value={e}>{effortLabel(e)}</option>}</For>
      </select></label></Show>
    </div>
    <p class="fork-note">Same workspace on {p.destination}. Files are shared. The original mission stays unchanged{["active", "pending", "waiting_background"].includes(p.mission.status) ? " and continues running" : ""}.</p>
    <Show when={error()}><p class="st-error" role="alert">{error()}</p></Show>
  </Dialog>;
}
