import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createSignal, onCleanup } from "solid-js";
import * as Ic from "./icons";
import { pollWhileVisible } from "./poll";
import { displayTitle } from "./goal";
import {
  NO_PROJECT_LIMIT,
  getProjectGrant,
  holdsCapSlot,
  isConnected,
  listProjectMissions,
  listProjects,
  projectLimitOf,
  setProjectLimit,
  type Mission,
  type ProjectGrant,
} from "./api";
import { ControllerSkeleton } from "./Skeleton";

/** Shown for context, changed elsewhere. Saving the limit above leaves every
 * one of these exactly as it is. */
const READ_ONLY_FIELDS: Array<{ key: keyof ProjectGrant; label: string; desc: string }> = [
  { key: "autonomy_level", label: "Autonomy level", desc: "How much this project's agents may do on their own." },
  { key: "merge_authority", label: "Merge authority", desc: "Who may merge this project's work." },
  { key: "budget_per_tick", label: "Budget per run", desc: "What a scheduled run may spend." },
  { key: "material_bar", label: "Material bar", desc: "What counts as a material change here." },
  { key: "pause_reason", label: "Pause reason", desc: "Why this project was paused." },
  { key: "resume_condition", label: "Resume condition", desc: "What has to be true to resume it." },
];

/**
 * A project's own settings, opened in the main panel from the sidebar's
 * right-click menu — the same navigation a cron uses, not a modal.
 *
 * The one writable field is this project's limit on unfinished agents, which
 * `create_mission` enforces by counting the project's own unfinished missions
 * (`campaign_slot_held_by` in `src/api/control/mod.rs`). It is a different
 * limit from the backend-wide one in Execution settings, which this page only
 * links to.
 */
export function ProjectSettings(p: { slug: string; onOpenPage: (id: string) => void; onOpenMission: (id: string) => void }) {
  const [grant, setGrant] = createSignal<ProjectGrant | null>(null);
  const [title, setTitle] = createSignal<string | null>(null);
  const [missions, setMissions] = createSignal<Mission[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [saveError, setSaveError] = createSignal<string | null>(null);
  const [saved, setSaved] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [draft, setDraft] = createSignal<string>("");
  const [dirty, setDirty] = createSignal(false);

  /** The project's unfinished missions — the ones the limit is counted against. */
  const unfinished = () => missions().filter((m) => holdsCapSlot(m.status));
  /** The enforced limit, or null when there is none. A stored 0 is "no limit". */
  const limit = () => projectLimitOf(grant());
  /** What the input shows for a grant: blank when no limit is enforced. */
  const fieldFor = (g: ProjectGrant | null) => {
    const value = projectLimitOf(g);
    return value == null ? "" : String(value);
  };

  const load = async () => {
    if (!isConnected()) return;
    try {
      const [g, ms] = await Promise.all([
        getProjectGrant(p.slug),
        listProjectMissions(p.slug).catch(() => [] as Mission[]),
      ]);
      setGrant(g);
      setMissions(ms);
      // Never stomp on what the user is typing.
      if (!dirty()) setDraft(fieldFor(g));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  };
  void load();
  void listProjects()
    .then((list) => setTitle(list.find((x) => x.slug === p.slug)?.title ?? null))
    .catch(() => {});
  onCleanup(pollWhileVisible(load, 15000));

  const parsed = (): { value: number } | { error: string } => {
    const raw = draft().trim();
    // Empty means no limit, which the backend stores as 0 — a JSON null would
    // be merged away and silently leave the old limit in place.
    if (!raw) return { value: NO_PROJECT_LIMIT };
    if (!/^\d+$/.test(raw)) return { error: "Enter a whole number, or leave it empty for no limit." };
    const value = Number(raw);
    if (value < 1) return { error: "Enter 1 or more, or leave it empty for no limit." };
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
      // Only this field is sent; the backend preserves the rest of the project's
      // settings in the same write, so a change made elsewhere meanwhile is not
      // reverted by anything this page does.
      const updated = await setProjectLimit(p.slug, next.value);
      setGrant(updated);
      setDirty(false);
      setDraft(fieldFor(updated));
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
          <h2>{title() || p.slug}</h2>
        </div>
        <p class="s-lead">
          Settings for the project <code>{p.slug}</code> on the connected backend.
        </p>
        <Show when={error()}>
          <ErrorNotice error={error()!} />
        </Show>
        <Show when={loaded()} fallback={<ControllerSkeleton />}>
          <section class="s-sec">
            <h3>Concurrency</h3>
            <div class="s-card">
              <div class="s-row">
                <div class="s-row-text">
                  <div class="s-row-title">Maximum unfinished agents for this project</div>
                  <div class="s-row-desc">
                    Starting another agent here is refused once this many are still unfinished. Pending, paused and
                    awaiting-user agents count too. Empty means no project limit.
                  </div>
                </div>
                <div class="s-row-ctrl ps-cap-ctrl">
                  <input
                    class="s-input ps-cap-input"
                    inputmode="numeric"
                    aria-label="Maximum unfinished agents for this project"
                    placeholder="No limit"
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
              <div class="s-row">
                <div class="s-row-text">
                  <div class="s-row-title">Unfinished now</div>
                  <div class="s-row-desc">This project's agents that have not finished yet.</div>
                </div>
                <div class="s-row-ctrl">
                  <span class="ps-usage" classList={{ full: limit() != null && unfinished().length >= limit()! }}>
                    {unfinished().length}
                    <Show when={limit() != null} fallback={" running"}>{` / ${limit()}`}</Show>
                  </span>
                </div>
              </div>
            </div>
            <Show when={saveError()}>
              <ErrorNotice error={saveError()!} />
            </Show>
            <Show when={saved()}>
              <p class="ps-saved" role="status">Saved. Only this project's limit changed.</p>
            </Show>
            <Show when={unfinished().length > 0}>
              <div class="s-card ps-occupants">
                <For each={unfinished()}>
                  {(m) => (
                    <button class="ps-occupant" onClick={() => p.onOpenMission(m.id)}>
                      <span class="ps-occupant-title">{displayTitle(m.title) || m.id}</span>
                      <span class="ps-occupant-status">{m.status.replaceAll("_", " ")}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </section>

          <section class="s-sec">
            <h3>Backend-wide limit</h3>
            <div class="s-card">
              <div class="s-row">
                <div class="s-row-text">
                  <div class="s-row-title">Maximum agents across all projects</div>
                  <div class="s-row-desc">
                    A separate limit that applies to the whole backend. Changing it does not affect this project's
                    limit above.
                  </div>
                </div>
                <div class="s-row-ctrl">
                  <button class="s-btn" onClick={() => p.onOpenPage("execution")}>
                    Execution settings
                  </button>
                </div>
              </div>
            </div>
          </section>

          <section class="s-sec">
            <h3>Permissions</h3>
            <p class="s-lead">
              Shown here, changed elsewhere. Saving the limit above leaves all of these untouched.
            </p>
            <div class="s-card">
              <For each={READ_ONLY_FIELDS}>
                {(field) => (
                  <div class="s-row">
                    <div class="s-row-text">
                      <div class="s-row-title">{field.label}</div>
                      <div class="s-row-desc">{field.desc}</div>
                    </div>
                    <div class="s-row-ctrl">
                      <span class="ps-readonly">{String(grant()?.[field.key] ?? "") || "—"}</span>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </section>
        </Show>
      </div>
    </div>
  );
}

/** Sidebar/menu glyph for the project settings entry. */
export const ProjectSettingsIcon = Ic.SlidersIcon;
