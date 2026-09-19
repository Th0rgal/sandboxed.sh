import { For, Show, createEffect, createMemo, createSignal, on, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { updateController, type ControllerPatch, type ControllerView } from "./api";
import { SchedulePicker, scheduleSummary } from "./SchedulePicker";


type Draft = {
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

function draftOf(view: ControllerView): Draft {
  const j = view.job;
  const s = view.settings;
  return {
    name: j?.name ?? "",
    schedule: (j?.schedule ?? "").trim(),
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

/** Settings for a project's controller: every field Hermes lets you edit. */
export function ControllerSettingsPanel(p: { slug: string; view: ControllerView; onSaved: (v: ControllerView) => void }) {
  const [draft, setDraft] = createStore<Draft>(draftOf(p.view));
  const [base, setBase] = createSignal<Draft>(draftOf(p.view));
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [skillInput, setSkillInput] = createSignal("");

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

  const save = async () => {
    if (saving() || dirtyCount() === 0) return;
    setSaving(true);
    setError(null);
    try {
      const view = await updateController(p.slug, patch());
      const next = draftOf(view);
      setBase(next);
      setDraft(next);
      p.onSaved(view);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };
  const discard = () => {
    setDraft(base());
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
      <Section title="Schedule">
        <Row title="Name">
          <input class="s-input cs-input" value={draft.name} onInput={(e) => setDraft("name", e.currentTarget.value)} />
        </Row>
        <Row title="Runs" desc={scheduleSummary(draft.schedule)}>
          <SchedulePicker value={draft.schedule} onChange={(v) => setDraft("schedule", v)} />
        </Row>
        <Row title="Stops after" desc={`${settings()?.repeat_completed ?? 0} runs so far`}>
          <input
            class="s-input cs-input cs-narrow"
            inputmode="numeric"
            placeholder="Never"
            value={draft.repeat}
            onInput={(e) => setDraft("repeat", e.currentTarget.value.replace(/[^0-9]/g, ""))}
          />
        </Row>
      </Section>

      <Section title="Instruction">
        <div class="cs-prompt-wrap">
          <textarea
            class="cs-prompt"
            spellcheck={false}
            value={draft.prompt}
            onInput={(e) => setDraft("prompt", e.currentTarget.value)}
          />
          <div class="cs-prompt-foot">
            <span>{draft.prompt.length.toLocaleString()} characters</span>
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

      <Show when={dirtyCount() > 0 || error()}>
        <div class="cs-savebar">
          <span class={error() ? "cs-warn" : ""}>
            {error() ?? `${dirtyCount()} unsaved change${dirtyCount() === 1 ? "" : "s"}`}
          </span>
          <span class="dlg-spacer" />
          <button class="s-btn sm quiet" disabled={saving()} onClick={discard}>
            Discard
          </button>
          <button class="s-btn sm primary" disabled={saving() || dirtyCount() === 0} onClick={save}>
            {saving() ? "Saving…" : "Save"}
          </button>
        </div>
      </Show>
    </div>
  );
}
