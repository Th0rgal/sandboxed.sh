import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createSignal, createEffect, onMount, onCleanup } from "solid-js";
import { forkMission, shortModelLabel, type HarnessChoice, type Mission } from "./api";
import { effortLabel, supportedEfforts } from "./effort";

export function ForkMission(p: { mission: Mission; choices: HarnessChoice[]; destination: string; position?: { x: number; y: number }; onClose: () => void; onFork: (mission: Mission) => void }) {
  const [backend, setBackend] = createSignal(p.mission.backend ?? p.choices[0]?.backend.id ?? "");
  const choices = () => p.choices.find(c => c.backend.id === backend())?.models ?? [];
  const [model, setModel] = createSignal(choices().find(m => m.value === p.mission.model_override)?.value ?? choices()[0]?.value ?? "");
  const [effortOpen, setEffortOpen] = createSignal(false);
  const unavailable = (id: string) => !!p.mission.remote_node_id && !["grok", "claudecode", "opencode"].includes(id);
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
  createEffect(() => {
    effortOpen(); backend();
    queueMicrotask(() => {
      if (!root?.isConnected) return;
      root.style.transform = "";
      const rect = root.getBoundingClientRect();
      const dx = Math.max(12 - rect.left, Math.min(0, window.innerWidth - 12 - rect.right));
      const dy = p.position ? Math.max(12 - rect.top, Math.min(0, window.innerHeight - 12 - rect.bottom)) : 0;
      root.style.transform = `translate(${dx}px, ${dy}px)`;
    });
  });
  onMount(() => {
    const outside = (e: PointerEvent) => { if (!busy() && !(root.parentElement?.closest(".popup-menu") ?? (p.position ? root : root.parentElement))?.contains(e.target as Node)) p.onClose(); };
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
    if (e.key === "ArrowRight") { e.preventDefault(); root.querySelector<HTMLButtonElement>(menu?.classList.contains('fork-models') ? '.fork-effort-menu > button' : '.fork-models > button')?.focus(); }
    if (e.key === "ArrowLeft") { e.preventDefault(); root.querySelector<HTMLButtonElement>(menu?.classList.contains('fork-effort-menu') ? '.fork-models > button' : '.fork-harnesses > button[aria-expanded="true"]')?.focus(); }
  };
  return <div ref={root} class="fork-cascade" style={p.position ? { position: "fixed", left: `${p.position.x}px`, top: `${p.position.y}px`, bottom: "auto", "align-items": "flex-start" } : undefined} onKeyDown={move}>
    <div class="menu fork-harnesses" role="menu" aria-label="Fork conversation">
      <Show when={!p.position}><div class="menu-group">Fork conversation</div></Show>
      <For each={p.choices}>{c => <button class="menu-item" role="menuitem" aria-haspopup="menu"
        aria-expanded={backend() === c.backend.id} disabled={busy() || unavailable(c.backend.id)} title={unavailable(c.backend.id) ? "Not supported on this remote workspace" : c.backend.name}
        onMouseEnter={() => { if (!busy() && !unavailable(c.backend.id) && backend() !== c.backend.id) { setBackend(c.backend.id); setEffort(""); setEffortOpen(false); } }}
        onClick={() => { setBackend(c.backend.id); setEffort(""); setEffortOpen(false); }}>
        <span>{c.backend.name}</span><span class="fork-chevron" aria-hidden="true">›</span>
      </button>}</For>
    </div>
    <div class="menu fork-models" role="menu" aria-label="Choose a model">
      <For each={choices()}>{m => <button class="menu-item" role="menuitem" disabled={busy()}
        title={`Fork into ${shortModelLabel(m.label)} · same workspace on ${p.destination}`}
        aria-haspopup={supportedEfforts(backend()).length ? "menu" : undefined}
        onMouseEnter={() => { if (!busy()) { setModel(m.value); setEffortOpen(!!supportedEfforts(backend()).length); } }}
        onClick={() => { setModel(m.value); if (supportedEfforts(backend()).length) setEffortOpen(true); else void fork(); }}>
        <span>{shortModelLabel(m.label)}</span><Show when={supportedEfforts(backend()).length}><span aria-hidden="true">›</span></Show>
        <Show when={backend() === p.mission.backend && m.value === p.mission.model_override}><span aria-hidden="true">✓</span></Show>
      </button>}</For>
      <Show when={!choices().length}><div class="menu-group">No models available</div></Show>
    </div>
    <Show when={effortOpen() && supportedEfforts(backend()).length}>
      <div class="menu fork-effort-menu" role="menu" aria-label="Choose effort">
        <For each={["", ...supportedEfforts(backend())]}>{e => <button class="menu-item" role="menuitem" disabled={busy()}
          onClick={() => { setEffort(e); void fork(); }}>{e ? effortLabel(e) : "Default"}</button>}</For>
      </div>
    </Show>
    <div class="fork-feedback">
      <Show when={busy()}><div class="menu-group" role="status">Forking…</div></Show>
      <Show when={error()}><ErrorNotice error={error()!} /></Show>
    </div>
  </div>;
}
