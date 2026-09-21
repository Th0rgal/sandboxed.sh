import { For, Show, createSignal } from "solid-js";
import { addProjectSteer, controllerAction, type ProjectSteer, type ProjectSteers } from "./api";

export function SteerComposer(p: {
  slug: string;
  steers: ProjectSteers | null;
  running: boolean;
  onSteers: (next: ProjectSteers) => void;
  onRan?: () => void;
}) {
  const [text, setText] = createSignal("");
  const [runNow, setRunNow] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let ta!: HTMLTextAreaElement;

  const send = async () => {
    const body = text().trim();
    if (!body || busy()) return;
    setBusy(true);
    setError(null);
    try {
      const next = await addProjectSteer(p.slug, body, "orb");
      p.onSteers(next);
      setText("");
      ta.value = "";
      if (runNow() && !p.running) {
        await controllerAction(p.slug, "run");
        p.onRan?.();
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="steer-box">
      <Show when={(p.steers?.pending.length ?? 0) > 0 || (p.steers?.recent.length ?? 0) > 0}>
        <div class="steer-list">
          <For each={p.steers?.pending ?? []}>
            {(s) => <SteerChip steer={s} pending />}
          </For>
          <For each={p.steers?.recent ?? []}>
            {(s) => <SteerChip steer={s} />}
          </For>
        </div>
      </Show>
      <div class="composer tall steer-composer">
        <div class="composer-field">
          <textarea
            ref={ta}
            rows={2}
            placeholder="Steer the next tick…"
            onInput={(e) => setText(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />
        </div>
        <label class="steer-run">
          <input type="checkbox" checked={runNow()} onChange={(e) => setRunNow(e.currentTarget.checked)} />
          Run now
        </label>
        <div class="send-slot">
          <button class="s-btn sm" disabled={busy() || !text().trim()} onClick={() => void send()}>
            {busy() ? "Sending…" : "Steer"}
          </button>
        </div>
      </div>
      <Show when={error()}>
        <p class="st-error cr-error">{error()}</p>
      </Show>
    </div>
  );
}

function SteerChip(p: { steer: ProjectSteer; pending?: boolean }) {
  return (
    <div class={`steer-chip ${p.pending ? "pending" : "consumed"}`} title={p.steer.body}>
      <span class="steer-chip-kind">{p.pending ? "Pending" : "Consumed"}</span>
      <span class="steer-chip-body">{p.steer.body}</span>
    </div>
  );
}
