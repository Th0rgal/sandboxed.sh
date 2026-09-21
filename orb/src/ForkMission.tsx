import { For, Show, createSignal, onMount, onCleanup } from "solid-js";
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
  let root!: HTMLDivElement;
  onMount(() => {
    const outside = (e: PointerEvent) => { if (!busy() && !root.parentElement?.contains(e.target as Node)) p.onClose(); };
    const escape = (e: KeyboardEvent) => { if (e.key === "Escape" && !busy()) { e.preventDefault(); p.onClose(); } };
    window.addEventListener("pointerdown", outside);
    window.addEventListener("keydown", escape);
    onCleanup(() => { window.removeEventListener("pointerdown", outside); window.removeEventListener("keydown", escape); });
  });
  const move = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement;
    const menu = target.closest('[role="menu"]');
    const items = Array.from(menu?.querySelectorAll<HTMLButtonElement>(':scope > button:not(:disabled)') ?? []);
    const index = items.indexOf(target as HTMLButtonElement);
    if (["ArrowDown", "ArrowUp"].includes(e.key) && items.length) {
      e.preventDefault(); items[(index + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
    }
    if (e.key === "ArrowRight") { e.preventDefault(); root.querySelector<HTMLButtonElement>('.fork-models > button')?.focus(); }
    if (e.key === "ArrowLeft") { e.preventDefault(); root.querySelector<HTMLButtonElement>('.fork-harnesses > button[aria-expanded="true"]')?.focus(); }
  };
  return <div ref={root} class="fork-cascade" onKeyDown={move}>
    <div class="menu fork-harnesses" role="menu" aria-label="Fork conversation">
      <div class="menu-group">Fork conversation</div>
      <For each={p.choices}>{c => <button class="menu-item" role="menuitem" aria-haspopup="menu"
        aria-expanded={backend() === c.backend.id} disabled={busy()}
        onMouseEnter={() => { if (!busy() && backend() !== c.backend.id) { setBackend(c.backend.id); setEffort(""); } }}
        onClick={() => { setBackend(c.backend.id); setEffort(""); }}>
        <span>{c.backend.name}</span><span class="fork-chevron" aria-hidden="true">›</span>
      </button>}</For>
    </div>
    <div class="menu fork-models" role="menu" aria-label="Choose a model">
      <For each={choices()}>{m => <button class="menu-item" role="menuitem" disabled={busy()}
        title={`Fork into ${shortModelLabel(m.label)} · same workspace on ${p.destination}`}
        onClick={() => { setModel(m.value); void fork(); }}>
        <span>{shortModelLabel(m.label)}</span>
        <Show when={backend() === p.mission.backend && m.value === p.mission.model_override}><span aria-hidden="true">✓</span></Show>
      </button>}</For>
      <Show when={!choices().length}><div class="menu-group">No models available</div></Show>
      <Show when={supportedEfforts(backend()).length}>
        <div class="fork-efforts" role="group" aria-label="Reasoning effort"><span>Effort</span>
          <For each={["", ...supportedEfforts(backend())]}>{e => <button type="button" disabled={busy()}
            aria-pressed={effort() === e} onClick={() => setEffort(e)}>{e ? effortLabel(e) : "Default"}</button>}</For>
        </div>
      </Show>
      <Show when={busy()}><div class="menu-group" role="status">Forking…</div></Show>
      <Show when={error()}><p class="st-error" role="alert">{error()}</p></Show>
    </div>
  </div>;
}
