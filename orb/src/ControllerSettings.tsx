import { ErrorNotice } from "./ErrorNotice";
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { getApiUrl, updateController, type ControllerPatch, type ControllerView } from "./api";
import { ignoredCronFields, scheduleExpression } from "./cronSchema";
import { SchedulePicker } from "./SchedulePicker";


export type CronDraft = {
  name: string;
  schedule: string;
  prompt: string;
  skills: string[];
  deliver: string;
  failure_deliver: string;
  repeat: string; // "" = forever
  workdir: string;
  model: string;
  provider: string;
  reasoning_effort: string;
  continuity: boolean;
};

export function draftOf(view: ControllerView): CronDraft {
  const j = view.job;
  const s = view.settings;
  return {
    name: j?.name ?? "",
    schedule: scheduleExpression(j?.schedule).trim(),
    prompt: s?.prompt ?? "",
    skills: [...(s?.skills ?? [])],
    deliver: s?.deliver ?? "",
    failure_deliver: s?.failure_deliver ?? "",
    repeat: s?.repeat_times != null ? String(s.repeat_times) : "",
    workdir: s?.workdir ?? "",
    model: s?.model ?? "",
    provider: s?.provider ?? "",
    reasoning_effort: s?.reasoning_effort ?? "",
    continuity: s?.continuity ?? false,
  };
}

function Section(p: { title: string; hint?: string; children: JSX.Element }) {
  return (
    <section class="s-sec cs-sec">
      <h3>{p.title}</h3>
      <Show when={p.hint}>
        <p class="cs-hint">{p.hint}</p>
      </Show>
      <div class="s-card">{p.children}</div>
    </section>
  );
}

function Row(p: { title: string; desc?: string; stack?: boolean; children: JSX.Element }) {
  return (
    <div class={`s-row ${p.stack ? "cs-stack" : ""}`}>
      <div class="s-row-text">
        <div class="s-row-title">{p.title}</div>
        <Show when={p.desc}>
          <div class="s-row-desc">{p.desc}</div>
        </Show>
      </div>
      <div class="s-row-ctrl">{p.children}</div>
    </div>
  );
}

/** Shared creation and editing form. Drafts survive tabs, navigation and unmount. */
export function CronForm(p: {
  draftKey: string;
  view: ControllerView;
  creating?: boolean;
  deliveryRoute?: { ready: boolean; loading: boolean; error: string | null };
  save: (patch: ControllerPatch) => Promise<ControllerView>;
  onSaved: (view: ControllerView, warning?: string) => void;
  onBusyChange?: (busy: boolean) => void;
  onClose?: () => void;
}) {
  const draftPrefix = `orb.cronDraft:${getApiUrl()}:`;
  const storageKey = `${draftPrefix}${p.draftKey}`;
  let restored: { draft: CronDraft; base: CronDraft; skillInput?: string } | null = null;
  try { restored = JSON.parse(sessionStorage.getItem(storageKey) ?? "null"); } catch { /* unavailable storage */ }
  const initialDraft = () => ({ ...draftOf(p.view), ...(p.creating ? { deliver: p.view.settings?.deliver ?? `project:${p.view.slug}` } : {}) });
  const [draft, setDraft] = createStore<CronDraft>(restored?.draft ?? initialDraft());
  const [base, setBase] = createSignal<CronDraft>(restored?.base ?? initialDraft());
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [skillInput, setSkillInput] = createSignal(restored?.skillInput ?? "");
  const persist = () => {
    try {
      if (dirtyCount() || skillInput().trim()) sessionStorage.setItem(storageKey, JSON.stringify({ draft, base: base(), skillInput: skillInput() }));
      else sessionStorage.removeItem(storageKey);
    } catch { /* beforeunload still protects edits when storage is unavailable */ }
  };
  createEffect(persist);
  const beforeUnload = (e: BeforeUnloadEvent) => {
    if (dirtyCount() || skillInput().trim() || saving()) { e.preventDefault(); e.returnValue = ""; }
  };
  window.addEventListener("beforeunload", beforeUnload);
  onCleanup(() => { persist(); window.removeEventListener("beforeunload", beforeUnload); });

  // A fresh server view resets the form only while nothing is being edited.
  createEffect(
    on(
      () => p.view,
      (view) => {
        if (dirtyCount() > 0 || saving()) return;
        const next = draftOf(view);
        setBase(next);
        setDraft(next);
      },
      { defer: true },
    ),
  );

  const patch = createMemo<ControllerPatch>(() => {
    const b = base();
    const out: ControllerPatch = {};
    if (draft.name.trim() !== b.name) out.name = draft.name.trim();
    if (draft.schedule.trim() !== b.schedule) out.schedule = draft.schedule.trim();
    if (draft.prompt !== b.prompt) out.prompt = draft.prompt;
    if (draft.skills.join("\n") !== b.skills.join("\n")) out.skills = [...draft.skills];
    if (draft.deliver.trim() !== b.deliver) out.deliver = draft.deliver.trim();
    if (draft.failure_deliver.trim() !== b.failure_deliver) out.failure_deliver = draft.failure_deliver.trim();
    if (draft.repeat.trim() !== b.repeat) out.repeat = Number(draft.repeat.trim()) || 0;
    if (draft.workdir.trim() !== b.workdir) out.workdir = draft.workdir.trim();
    if (draft.model.trim() !== b.model) out.model = draft.model.trim();
    if (draft.provider.trim() !== b.provider) out.provider = draft.provider.trim();
    if (draft.reasoning_effort !== b.reasoning_effort) out.reasoning_effort = draft.reasoning_effort;
    if (draft.continuity !== b.continuity) out.continuity = draft.continuity;
    return out;
  });
  const dirtyCount = () => Object.keys(patch()).length;

  const usesProjectRoute = () => !draft.deliver.trim() || draft.deliver.split(",").some((target) => target.trim() === `project:${p.view.slug}`);
  const deliveryHint = () => {
    if (draft.deliver.trim() === "local") return "Save output on the job only; no conversation copy.";
    if (!usesProjectRoute()) return "Send run output to the destination specified here.";
    if (p.deliveryRoute?.error) return `Project delivery unavailable: ${p.deliveryRoute.error}`;
    if (p.deliveryRoute?.loading) return "Checking the project's delivery route…";
    if (p.deliveryRoute && !p.deliveryRoute.ready) return "No delivery route is bound yet. That route is plumbing, not a chat to open. Bind one first, or choose local to keep output on the job only.";
    return "Run output is stored on this job. A copy may also land on the project's delivery route — look here, not in Hermes chat.";
  };
  const save = async () => {
    if (saving()) return;
    addSkill();
    if (!draft.name.trim() || !draft.prompt.trim() || !draft.schedule.trim()) {
      setError("Name, instruction, and schedule are required."); return;
    }
    if (draft.name.trim().length > 200) { setError("Name must be 200 characters or fewer."); return; }
    if (draft.repeat.trim() && (!/^\d+$/.test(draft.repeat) || !Number.isSafeInteger(Number(draft.repeat)) || Number(draft.repeat) < 1)) {
      setError("Stops after must be a positive whole number, or empty for Never."); return;
    }
    if ((p.creating || patch().prompt !== undefined) && draft.prompt.length > (p.view.settings?.prompt_budget ?? 5000)) { setError(`Instruction is ${draft.prompt.length.toLocaleString()} characters; the limit is ${(p.view.settings?.prompt_budget ?? 5000).toLocaleString()}. Shorten it before saving. Your edits are kept.`); return; }
    if (p.creating && usesProjectRoute() && p.deliveryRoute && !p.deliveryRoute.ready) {
      setError(p.deliveryRoute.error ?? (p.deliveryRoute.loading ? "Checking project delivery route…" : "No delivery route is bound yet. Bind one first, or choose local to keep output on the job only.")); return;
    }
    if (!p.creating && dirtyCount() === 0) return;
    setSaving(true);
    p.onBusyChange?.(true);
    setError(null);
    try {
      const changes: ControllerPatch = p.creating ? {
        name: draft.name.trim(), schedule: draft.schedule.trim(), prompt: draft.prompt,
        skills: [...draft.skills], deliver: draft.deliver.trim() || `project:${p.view.slug}`,
        ...(draft.repeat ? { repeat: Number(draft.repeat) } : {}),
        ...Object.fromEntries((["failure_deliver", "workdir", "model", "provider", "reasoning_effort"] as const).filter((key) => draft[key].trim()).map((key) => [key, draft[key].trim()])),
        ...(draft.continuity ? { continuity: true } : {}),
      } : patch();
      const view = await p.save(changes);
      const ignored = ignoredCronFields(changes, view);
      const warning = ignored.length ? `Hermes did not retain: ${ignored.join(", ")}. Its cron API needs support for these settings.` : undefined;
      const next = draftOf(view);
      if (warning && p.creating && view.job) {
        const pending = { ...next, ...Object.fromEntries(ignored.map((key) => [key, draft[key as keyof CronDraft]])) };
        try { sessionStorage.setItem(`${draftPrefix}edit:${view.slug}:${view.job.id}`, JSON.stringify({ base: next, draft: pending })); } catch { /* warning still identifies dropped values */ }
      }
      setBase(next);
      if (warning && !p.creating) { setError(warning); return; }
      setDraft(next);
      setSkillInput("");
      try { sessionStorage.removeItem(storageKey); } catch { /* storage may be unavailable */ }
      p.onSaved(view, warning);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
      p.onBusyChange?.(false);
    }
  };
  const discard = () => {
    setDraft(base());
    setSkillInput("");
    setError(null);
  };

  const addSkill = () => {
    const name = skillInput().trim();
    if (!name || draft.skills.includes(name)) return setSkillInput("");
    setDraft("skills", [...draft.skills, name]);
    setSkillInput("");
  };

  const settings = () => p.view.settings;
  const budget = () => settings()?.prompt_budget ?? null;
  const overBudgetHint = () =>
    budget() != null && (p.view.job?.last_error ?? "").includes("initial prompt");

  return (
    <div class="cs">
      <fieldset class="cs-fieldset" disabled={saving()}>
      <Section title="Schedule">
        <Row title="Name">
          <input aria-label="Name" class="s-input cs-input" value={draft.name} onInput={(e) => setDraft("name", e.currentTarget.value)} />
        </Row>
        <Row title="Runs">
          <SchedulePicker value={draft.schedule} onChange={(v) => setDraft("schedule", v)} />
        </Row>
        <Row title="Stops after" desc={`${settings()?.repeat_completed ?? 0} runs so far`}>
          <input
            aria-label="Stops after" class="s-input cs-input cs-narrow"
            inputmode="numeric"
            placeholder="Never"
            value={draft.repeat}
            onInput={(e) => setDraft("repeat", e.currentTarget.value)}
          />
        </Row>
        <Row title="Deliver to" desc={deliveryHint()}>
          <input class="s-input cs-input" placeholder={`project:${p.view.slug}`} aria-label="Delivery" value={draft.deliver} onInput={(e) => setDraft("deliver", e.currentTarget.value)} />
        </Row>
      </Section>

      <Section title="Instruction">
        <div class="cs-prompt-wrap">
          <textarea
            aria-label="Instruction" class="cs-prompt"
            spellcheck={false}
            value={draft.prompt}
            onInput={(e) => setDraft("prompt", e.currentTarget.value)}
          />
          <div class="cs-prompt-foot">
            <span class={draft.prompt.length > (budget() ?? 5000) ? "cs-warn" : ""}>{draft.prompt.length.toLocaleString()} / {(budget() ?? 5000).toLocaleString()} characters</span>
            <Show when={budget()}>
              <span class={overBudgetHint() ? "cs-warn" : ""}>
                Bound controller: prompt plus preloaded skills must stay under {budget()!.toLocaleString()} characters
              </span>
            </Show>
          </div>
        </div>
      </Section>

      <Section title="Skills" hint="Inserted before the instruction on every run.">
        <div class="cs-skills">
          <For each={draft.skills}>
            {(skill) => (
              <span class="cs-chip">
                {skill}
                <button title="Remove" onClick={() => setDraft("skills", draft.skills.filter((s) => s !== skill))}>
                  <Ic.CloseIcon size={11} />
                </button>
              </span>
            )}
          </For>
          <input
            class="cs-skill-input"
            placeholder={draft.skills.length ? "Add skill" : "No skills attached. Add one"}
            value={skillInput()}
            onInput={(e) => setSkillInput(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === ",") {
                e.preventDefault();
                addSkill();
              } else if (e.key === "Backspace" && !skillInput() && draft.skills.length) {
                setDraft("skills", draft.skills.slice(0, -1));
              }
            }}
            onBlur={addSkill}
          />
        </div>
      </Section>

      <details class="cs-advanced"><summary>Advanced</summary>
      <Section title="Execution overrides" hint="Optional Hermes execution and delivery overrides.">

        <Row title="Model" desc={settings()?.model_snapshot ? `Saved default: ${settings()!.model_snapshot}` : undefined}>
          <input class="s-input cs-input" placeholder={settings()?.model_snapshot ? "No override" : "Hermes default"} aria-label="Model" value={draft.model} onInput={(e) => setDraft("model", e.currentTarget.value)} />
        </Row>
        <Row title="Provider" desc={settings()?.provider_snapshot ? `Saved default: ${settings()!.provider_snapshot}` : undefined}>
          <input class="s-input cs-input" placeholder={settings()?.provider_snapshot ? "No override" : "Hermes default"} aria-label="Provider" value={draft.provider} onInput={(e) => setDraft("provider", e.currentTarget.value)} />
        </Row>
        <Row title="Failure delivery"><input class="s-input cs-input" placeholder="Same as delivery" aria-label="Failure delivery" value={draft.failure_deliver} onInput={(e) => setDraft("failure_deliver", e.currentTarget.value)} /></Row>
        <Row title="Working directory"><input class="s-input cs-input" placeholder="Hermes default" aria-label="Working directory" value={draft.workdir} onInput={(e) => setDraft("workdir", e.currentTarget.value)} /></Row>
        <Row title="Reasoning"><select aria-label="Reasoning" class="s-input cs-input" value={draft.reasoning_effort} onChange={(e) => setDraft("reasoning_effort", e.currentTarget.value)}>
          <option value="">Hermes default</option><For each={["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"]}>{(effort) => <option value={effort}>{effort}</option>}</For>
        </select></Row>
        <Row title="Continuity" desc="Keep context across runs when supported by Hermes."><input aria-label="Continuity" type="checkbox" checked={draft.continuity} onChange={(e) => setDraft("continuity", e.currentTarget.checked)} /></Row>
      </Section>
      </details>

      </fieldset>
      <Show when={p.creating || dirtyCount() > 0 || skillInput().trim() || error()}>
        <div class="cs-save-area">
          <Show when={error()}><ErrorNotice error={error()!} title="Couldn’t save the controller" /></Show>
          <div class="cs-savebar">
          <span>{`${dirtyCount()} unsaved change${dirtyCount() === 1 ? "" : "s"}`}</span>
          <span class="dlg-spacer" />
          <Show when={p.onClose}><button class="s-btn sm quiet" disabled={saving()} onClick={() => {
            if (!(dirtyCount() || skillInput().trim()) || window.confirm("Discard this cron draft?")) { discard(); p.onClose?.(); }
          }}>Cancel</button></Show>
          <button class="s-btn sm quiet" disabled={saving()} onClick={discard}>
            Discard
          </button>
          <button class="s-btn sm primary" disabled={saving() || (p.creating && usesProjectRoute() && !!p.deliveryRoute && !p.deliveryRoute.ready) || (!p.creating && dirtyCount() === 0 && !skillInput().trim())} onClick={save}>
            {saving() ? (p.creating ? "Creating…" : "Saving…") : (p.creating ? "Create" : "Save")}
          </button>
          </div>
        </div>
      </Show>
    </div>
  );
}

export function ControllerSettingsPanel(p: { slug: string; id?: string; view: ControllerView; onSaved: (v: ControllerView) => void; save?: (patch: ControllerPatch) => Promise<ControllerView> }) {
  return <CronForm draftKey={`edit:${p.slug}:${p.id ?? "controller"}`} view={p.view} save={(patch) => p.save ? p.save(patch) : updateController(p.slug, patch)} onSaved={p.onSaved} />;
}
