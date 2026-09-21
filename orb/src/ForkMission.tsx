import { For, Show, createSignal } from "solid-js";
import { PromptSheet } from "./Dialog";
import { forkMission, shortModelLabel, type HarnessChoice, type Mission } from "./api";
import { effortLabel, supportedEfforts } from "./effort";

export function ForkMission(p: { mission: Mission; choices: HarnessChoice[]; destination: string; onClose: () => void; onFork: (mission: Mission) => void }) {
  const [backend, setBackend] = createSignal(p.mission.backend ?? p.choices[0]?.backend.id ?? "");
  const choices = () => p.choices.find(c => c.backend.id === backend())?.models ?? [];
  const [model, setModel] = createSignal(choices().find(m => m.value === p.mission.model_override)?.value ?? choices()[0]?.value ?? "");
  const [query, setQuery] = createSignal("");
  const filtered = () => p.choices.map(c => ({ ...c, models: c.models.filter(m => `${c.backend.name} ${m.label}`.toLowerCase().includes(query().toLowerCase().trim())) })).filter(c => c.models.length);
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
  return <PromptSheet title="Fork conversation" hint={p.destination}
    label="Search models" placeholder="Search models or harnesses…" value={query()} onInput={setQuery}
    action="Fork and continue" onAction={() => void fork()} busy={busy()} disabled={!model()}
    onClose={p.onClose} error={error()}
    footer={<span title="The original mission stays unchanged. Both missions use the same workspace files.">Prompt and history copied · Files are shared</span>}>
    <div class="fork-picker" aria-label="Choose a model">
      <For each={filtered()}>{c => <section class="fork-group">
        <div class="fork-group-title">{c.backend.name}</div>
        <For each={c.models}>{m => <button type="button" class="fork-option"
          aria-pressed={backend() === c.backend.id && model() === m.value}
          disabled={busy()} onClick={() => { if (backend() !== c.backend.id) setEffort(""); setBackend(c.backend.id); setModel(m.value); }}>
          <span>{shortModelLabel(m.label)}</span>
          <Show when={backend() === c.backend.id && model() === m.value}><span class="fork-check" aria-hidden="true">✓</span></Show>
        </button>}</For>
      </section>}</For>
      <Show when={!filtered().length}><p class="fork-empty">No matching models</p></Show>
    </div>
    <Show when={supportedEfforts(backend()).length}>
      <div class="fork-efforts" role="group" aria-label="Reasoning effort"><span>Effort</span>
        <For each={["", ...supportedEfforts(backend())]}>{e => <button type="button" disabled={busy()}
          aria-pressed={effort() === e} onClick={() => setEffort(e)}>{e ? effortLabel(e) : "Default"}</button>}</For>
      </div>
    </Show>
  </PromptSheet>;
}
