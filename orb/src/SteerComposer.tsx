import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createSignal } from "solid-js";
import { ArrowUpIcon, Spinner } from "./icons";
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
  const [requested, setRequested] = createSignal<Set<string>>(new Set());
  const [error, setError] = createSignal<string | null>(null);
  let ta!: HTMLTextAreaElement;

  const resize = () => {
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 220) + "px";
  };

  const send = async () => {
    const body = text().trim();
    if (!body || busy()) return;
    setBusy(true);
    setError(null);
    try {
      const previous = new Set((p.steers?.pending ?? []).map(s => s.id));
      const next = await addProjectSteer(p.slug, body, "orb");
      p.onSteers(next);
      setText("");
      ta.value = "";
      resize();
      if (runNow() && !p.running) {
        try {
          await controllerAction(p.slug, "run");
          setRequested(ids => new Set([...ids, ...next.pending.filter(s => !previous.has(s.id)).map(s => s.id)]));
          p.onRan?.();
        } catch (e) {
          setError(`Steer saved, but the controller could not start: ${e instanceof Error ? e.message : String(e)}. Use Run now to retry.`);
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="steer-box">
      <Show when={(p.steers?.pending.length ?? 0) > 0}>
        <div class="steer-list">
          <For each={p.steers?.pending ?? []}>
            {(s) => <SteerChip steer={s} pending label={requested().has(s.id) ? "Run requested" : p.running ? "Awaiting pickup" : "Next tick"} />}
          </For>
        </div>
      </Show>
      <div class="composer steer-composer">
        <div class="composer-field">
          <textarea
            ref={ta}
            disabled={busy()}
            aria-label="Steer the next tick"
            rows={1}
            placeholder="Steer the next tick…"
            onInput={(e) => { setText(e.currentTarget.value); resize(); }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />
        </div>
        <button
          type="button"
          class="steer-timing"
          aria-label="Run now"
          aria-pressed={runNow()}
          title={runNow() ? "Run the controller after sending (unless already running). Click to wait for the next tick." : "Wait for the next scheduled tick. Click to run after sending."}
          disabled={busy()}
          onClick={() => setRunNow(!runNow())}
        >
          {runNow() ? "Now" : "Next tick"}
        </button>
        <div class="send-slot">
          <button class="send" aria-label="Steer" title="Send steer" disabled={busy() || !text().trim()} onClick={() => void send()}>
            <Show when={busy()} fallback={<ArrowUpIcon size={14} />}><Spinner size={14} /></Show>
          </button>
        </div>
      </div>
      <Show when={error()}>
        <ErrorNotice error={error()!} />
      </Show>
    </div>
  );
}

function SteerChip(p: { steer: ProjectSteer; pending?: boolean; label?: string }) {
  return (
    <div class={`steer-chip ${p.pending ? "pending" : "consumed"}`} title={p.steer.body}>
      <span class="steer-chip-kind">{p.pending ? p.label ?? "Next tick" : "Consumed"}</span>
      <span class="steer-chip-body">{p.steer.body}</span>
    </div>
  );
}
