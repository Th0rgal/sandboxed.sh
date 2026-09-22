import { ErrorNotice } from "./ErrorNotice";
import { Show, createSignal, onCleanup } from "solid-js";
import { pollWhileVisible } from "./poll";
import { getGlobalSettings, isConnected, updateGlobalSettings, type GlobalSettings } from "./api";
import { ControllerSkeleton } from "./Skeleton";

/**
 * Backend-wide execution limits (`GET|PUT /api/settings`). Deliberately kept
 * apart from a project's own limit, which lives on that project's settings
 * page: the two are enforced by different code paths, and raising this one does
 * not clear a project's "too many unfinished agents" refusal.
 */
export function ExecutionSettings(p: { onOpenPage?: (id: string) => void }) {
  const [settings, setSettings] = createSignal<GlobalSettings | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [saveError, setSaveError] = createSignal<string | null>(null);
  const [saved, setSaved] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [dirty, setDirty] = createSignal(false);

  const load = async () => {
    if (!isConnected()) return;
    try {
      const next = await getGlobalSettings();
      setSettings(next);
      if (!dirty()) setDraft(next.max_parallel_missions != null ? String(next.max_parallel_missions) : "");
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  };
  void load();
  onCleanup(pollWhileVisible(load, 30000));

  const parsed = (): { value: number } | { error: string } => {
    const raw = draft().trim();
    if (!raw) return { error: "Enter a number of 1 or more." };
    if (!/^\d+$/.test(raw)) return { error: "Enter a whole number." };
    const value = Number(raw);
    // The core rejects anything below 1; there is no "clear" for this field.
    if (value < 1) return { error: "Enter 1 or more." };
    return { value };
  };

  const save = async () => {
    if (saving()) return;
    const next = parsed();
    if ("error" in next) { setSaveError(next.error); return; }
    setSaving(true);
    setSaveError(null);
    setSaved(false);
    try {
      // PUT /api/settings is a patch on the core's side: it re-reads the stored
      // settings and overwrites only the fields present here.
      const updated = await updateGlobalSettings({ max_parallel_missions: next.value });
      setSettings(updated);
      setDirty(false);
      setDraft(updated.max_parallel_missions != null ? String(updated.max_parallel_missions) : "");
      setSaved(true);
      window.setTimeout(() => setSaved(false), 2400);
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="scroll">
      <div class="col ps-page">
        <div class="page-head">
          <h2>Execution</h2>
        </div>
        <p class="s-lead">Limits that apply to the whole backend, across every project.</p>
        <Show when={error()}>
          <ErrorNotice error={error()!} />
        </Show>
        <Show when={loaded()} fallback={<ControllerSkeleton />}>
          <section class="s-sec">
            <h3>Concurrency</h3>
            <div class="s-card">
              <div class="s-row">
                <div class="s-row-text">
                  <div class="s-row-title">Maximum agents across all projects</div>
                  <div class="s-row-desc">
                    How many agents the backend runs at once in total. Each project can also have its own, lower
                    limit on its settings page — when a project refuses to start another agent, that project's
                    limit is the one to change, not this.
                  </div>
                </div>
                <div class="s-row-ctrl ps-cap-ctrl">
                  <input
                    class="s-input ps-cap-input"
                    inputmode="numeric"
                    aria-label="Maximum agents across all projects"
                    placeholder="—"
                    value={draft()}
                    disabled={saving()}
                    onInput={(e) => { setDirty(true); setSaved(false); setSaveError(null); setDraft(e.currentTarget.value); }}
                    onKeyDown={(e) => e.key === "Enter" && void save()}
                  />
                  <button class="s-btn primary" disabled={saving() || !dirty()} onClick={() => void save()}>
                    {saving() ? "Saving…" : "Save"}
                  </button>
                </div>
              </div>
              <Show when={settings()?.max_concurrent_tasks != null}>
                <div class="s-row">
                  <div class="s-row-text">
                    <div class="s-row-title">Maximum concurrent tasks</div>
                    <div class="s-row-desc">Shown for context; changed elsewhere.</div>
                  </div>
                  <div class="s-row-ctrl">
                    <span class="ps-readonly">{settings()?.max_concurrent_tasks}</span>
                  </div>
                </div>
              </Show>
            </div>
            <Show when={saveError()}>
              <ErrorNotice error={saveError()!} />
            </Show>
            <Show when={saved()}>
              <p class="ps-saved" role="status">Saved. Only the backend-wide limit changed.</p>
            </Show>
          </section>
        </Show>
      </div>
    </div>
  );
}
