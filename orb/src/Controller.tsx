import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { MdView } from "./Markdown";
import { pollWhileVisible } from "./poll";
import { controllerAction, getProjectController, isConnected, type ControllerJob, type ControllerRun, type ControllerView as View } from "./api";

/** How a controller is doing, derived from its Hermes job record. */
export type CronState = "running" | "paused" | "attention" | "scheduled";

export function cronState(job: ControllerJob | null | undefined, running = false): CronState {
  if (!job) return "scheduled";
  if (running || job.state === "running") return "running";
  if (!job.enabled || job.state === "paused") return "paused";
  if (job.failure_streak > 0 || (job.last_status && job.last_status !== "ok")) return "attention";
  return "scheduled";
}

/** 0..1 progress from the last run to the next one. */
function tickProgress(job: ControllerJob | null | undefined, now: number): number {
  const next = job?.next_run_at ? Date.parse(job.next_run_at) : NaN;
  const last = job?.last_run_at ? Date.parse(job.last_run_at) : NaN;
  if (!Number.isFinite(next) || !Number.isFinite(last) || next <= last) return 0;
  return Math.max(0, Math.min(1, (now - last) / (next - last)));
}

/** Clock-ring glyph: the arc fills toward the next tick; spins while ticking. */
export function CronGlyph(p: { job: ControllerJob | null | undefined; running?: boolean; size?: number }) {
  const [now, setNow] = createSignal(Date.now());
  const t = window.setInterval(() => setNow(Date.now()), 30000);
  onCleanup(() => clearInterval(t));
  const size = () => p.size ?? 14;
  const r = 5;
  const c = 2 * Math.PI * r;
  const state = () => cronState(p.job, p.running);
  return (
    <svg class={`cron-glyph ${state()}`} width={size()} height={size()} viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="8" r={r} stroke="currentColor" stroke-width="1.5" opacity="0.28" />
      <Show when={state() !== "paused"}>
        <circle
          cx="8"
          cy="8"
          r={r}
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-dasharray={`${c}`}
          stroke-dashoffset={`${state() === "running" ? c * 0.7 : c * (1 - tickProgress(p.job, now()))}`}
          transform="rotate(-90 8 8)"
        />
      </Show>
      <path d="M8 5.2V8l1.8 1.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" opacity={state() === "paused" ? 0.5 : 0.9} />
    </svg>
  );
}

export function untilLabel(iso: string | null | undefined, now: number): string {
  if (!iso) return "";
  const delta = Date.parse(iso) - now;
  if (!Number.isFinite(delta)) return "";
  if (delta <= 0) return "due";
  const mins = Math.round(delta / 60000);
  if (mins < 1) return "in <1m";
  if (mins < 60) return `in ${mins}m`;
  const hours = Math.floor(mins / 60);
  return `in ${hours}h ${mins % 60}m`;
}

const timeOf = (iso?: string | null) =>
  iso ? new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";

function dayOf(iso?: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  const today = new Date();
  const yesterday = new Date(Date.now() - 86400000);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "long", day: "numeric", month: "short" });
}

function durationLabel(secs?: number | null): string {
  if (secs == null) return "";
  if (secs < 60) return `${secs}s`;
  return `${Math.floor(secs / 60)}m ${String(secs % 60).padStart(2, "0")}s`;
}

/** Timeline entries: a real run, or a fold of consecutive silent ticks. */
type Entry =
  | { kind: "day"; key: string; label: string }
  | { kind: "run"; key: string; run: ControllerRun }
  | { kind: "silent"; key: string; runs: ControllerRun[] }
  | { kind: "failed"; key: string; runs: ControllerRun[]; error: string };

const isFailed = (r: ControllerRun) => r.status === "failed" || !!r.error;

function buildEntries(runs: ControllerRun[]): Entry[] {
  const out: Entry[] = [];
  let day = "";
  for (const run of runs) {
    const d = dayOf(run.at);
    if (d && d !== day) {
      day = d;
      out.push({ kind: "day", key: `day:${d}`, label: d });
    }
    const last = out[out.length - 1];
    const running = run.status === "running" || run.status === "claimed";
    if (isFailed(run) && !running && !run.report) {
      // A streak of the same failure (a broken cron fails every minute)
      // reads as one line, not forty identical red cards.
      const error = run.error ?? "failed";
      if (last && last.kind === "failed" && last.error === error) last.runs.push(run);
      else out.push({ kind: "failed", key: `f:${run.id}`, runs: [run], error });
    } else if (run.silent && !running && !run.error) {
      if (last && last.kind === "silent") last.runs.push(run);
      else out.push({ kind: "silent", key: `s:${run.id}`, runs: [run] });
    } else {
      out.push({ kind: "run", key: run.id, run });
    }
  }
  return out;
}

function RunCard(p: { run: ControllerRun }) {
  const running = () => p.run.status === "running" || p.run.status === "claimed";
  const failed = () => p.run.status === "failed" || !!p.run.error;
  return (
    <div class={`cr-run ${failed() ? "failed" : ""}`}>
      <div class="cr-rail">
        <span class="cr-time">{timeOf(p.run.at)}</span>
        <span class="cr-dur">{running() ? "" : durationLabel(p.run.duration_secs)}</span>
      </div>
      <div class="cr-body">
        <Show when={running()}>
          <p class="shimmer cr-ticking">Ticking…</p>
        </Show>
        <Show when={p.run.report}>
          <MdView text={p.run.report} compact />
        </Show>
        <Show when={p.run.error}>
          <p class="st-error cr-error">{p.run.error}</p>
        </Show>
        <Show when={p.run.ctrl || (p.run.source && p.run.source !== "builtin")}>
          <div class="cr-meta">
            <Show when={p.run.source && p.run.source !== "builtin"}>
              <span class="cr-chip">{p.run.source}</span>
            </Show>
            <Show when={p.run.ctrl}>
              <span class="cr-ctrl">{p.run.ctrl}</span>
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}

function SilentFold(p: { runs: ControllerRun[] }) {
  const [open, setOpen] = createSignal(false);
  const span = () => {
    const first = p.runs[p.runs.length - 1];
    const last = p.runs[0];
    return p.runs.length === 1 ? timeOf(last.at) : `${timeOf(first.at)} – ${timeOf(last.at)}`;
  };
  return (
    <div class="cr-silent">
      <button class="cr-silent-head" onClick={() => setOpen(!open())}>
        <span class="cr-silent-dot" />
        {p.runs.length} silent tick{p.runs.length === 1 ? "" : "s"} · {span()}
      </button>
      <Show when={open()}>
        <For each={p.runs}>
          {(r) => (
            <div class="cr-silent-row">
              <span class="cr-time">{timeOf(r.at)}</span>
              <span class="cr-dur">{durationLabel(r.duration_secs)}</span>
              <span class="cr-ctrl">{r.ctrl ?? "nothing new"}</span>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
}

function FailedFold(p: { runs: ControllerRun[]; error: string }) {
  const span = () => {
    const first = p.runs[p.runs.length - 1];
    const last = p.runs[0];
    return p.runs.length === 1 ? timeOf(last.at) : `${timeOf(first.at)} – ${timeOf(last.at)}`;
  };
  return (
    <div class="cr-run failed cr-failed-fold">
      <div class="cr-rail">
        <span class="cr-time">{timeOf(p.runs[0].at)}</span>
        <span class="cr-dur">×{p.runs.length}</span>
      </div>
      <div class="cr-body">
        <p class="cr-failed-title">
          {p.runs.length === 1 ? "Tick failed" : `${p.runs.length} ticks failed`} <span class="cr-failed-span">· {span()}</span>
        </p>
        <p class="st-error cr-error">{p.error}</p>
      </div>
    </div>
  );
}

/** Page for a project's controller: a timeline of ticks, newest first. */
export function ControllerView(p: { slug: string }) {
  const [view, setView] = createSignal<View | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());

  const load = async () => {
    if (!isConnected()) return;
    try {
      setView(await getProjectController(p.slug));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  void load();
  onCleanup(pollWhileVisible(load, 15000));
  const clock = window.setInterval(() => setNow(Date.now()), 15000);
  onCleanup(() => clearInterval(clock));

  const job = () => view()?.job ?? null;
  const running = () => (view()?.runs ?? []).some((r) => r.status === "running" || r.status === "claimed");
  const state = () => cronState(job(), running());
  const entries = createMemo(() => buildEntries(view()?.runs ?? []));

  const act = async (action: "pause" | "resume" | "run") => {
    if (busy()) return;
    setBusy(action);
    try {
      setView(await controllerAction(p.slug, action));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div class="scroll">
      <div class="col cr-page">
        <Show when={view()} fallback={<p class="s-lead">{error() ?? "Loading controller…"}</p>}>
          <Show when={job()} fallback={<p class="s-lead">This project has no controller cron in Hermes.</p>}>
            {(j) => (
              <>
                <div class="cr-head">
                  <CronGlyph job={j()} running={running()} size={18} />
                  <div class="cr-head-text">
                    <div class="cr-name">{j().name}</div>
                    <div class="cr-sub">
                      <span>{j().schedule ?? "scheduled"}</span>
                      <span class="cr-sep">·</span>
                      <span class={`cr-state ${state()}`}>
                        {state() === "running"
                          ? "ticking now"
                          : state() === "paused"
                            ? `paused${j().paused_reason ? ` · ${j().paused_reason}` : ""}`
                            : `next ${untilLabel(j().next_run_at, now())}`}
                      </span>
                      <Show when={state() === "attention"}>
                        <span class="cr-sep">·</span>
                        <span class="cr-state attention">
                          {j().failure_streak > 0 ? `${j().failure_streak} failed in a row` : `last: ${j().last_status}`}
                        </span>
                      </Show>
                    </div>
                  </div>
                  <div class="cr-actions">
                    <Show
                      when={state() === "paused"}
                      fallback={
                        <button class="s-btn sm quiet" disabled={!!busy()} onClick={() => act("pause")}>
                          {busy() === "pause" ? "Pausing…" : "Pause"}
                        </button>
                      }
                    >
                      <button class="s-btn sm quiet" disabled={!!busy()} onClick={() => act("resume")}>
                        {busy() === "resume" ? "Resuming…" : "Resume"}
                      </button>
                    </Show>
                    <button class="s-btn sm" disabled={!!busy() || running()} onClick={() => act("run")}>
                      {busy() === "run" ? "Starting…" : "Run now"}
                    </button>
                  </div>
                </div>
                <Show when={error()}>
                  <p class="st-error cr-error">{error()}</p>
                </Show>
                <Show when={j().last_error && state() === "attention"}>
                  <p class="st-error cr-error">{j().last_error}</p>
                </Show>

                <div class="cr-timeline">
                  <For each={entries()}>
                    {(e) =>
                      e.kind === "day" ? (
                        <div class="cr-day">{e.label}</div>
                      ) : e.kind === "silent" ? (
                        <SilentFold runs={e.runs} />
                      ) : e.kind === "failed" ? (
                        <FailedFold runs={e.runs} error={e.error} />
                      ) : (
                        <RunCard run={e.run} />
                      )
                    }
                  </For>
                  <Show when={entries().length === 0}>
                    <p class="s-lead">No ticks recorded yet.</p>
                  </Show>
                </div>
              </>
            )}
          </Show>
        </Show>
      </div>
    </div>
  );
}
