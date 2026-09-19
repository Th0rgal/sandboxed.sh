import { For, Show, createEffect, createMemo, createSignal, on, type JSX } from "solid-js";
import { createStore } from "solid-js/store";
import * as Ic from "./icons";
import { updateController, type ControllerPatch, type ControllerView } from "./api";

const EFFORTS = ["", "low", "medium", "high", "xhigh", "max"] as const;
const SCHEDULE_EXAMPLES = ["every 45m", "every 2h", "weekdays at 9am", "0 9 * * 1-5"];

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
      <Section title="Schedule" hint="When Hermes wakes this cron. Each tick starts a fresh agent with the instruction below.">
        <Row title="Name">
          <input class="s-input cs-input" value={draft.name} onInput={(e) => setDraft("name", e.currentTarget.value)} />
        </Row>
        <Row title="Runs" desc="An interval, a weekday phrase, a cron expression or an ISO date for a one-off.">
          <div class="cs-col">
            <input class="s-input cs-input" spellcheck={false} value={draft.schedule} onInput={(e) => setDraft("schedule", e.currentTarget.value)} />
            <div class="cs-examples">
              <For each={SCHEDULE_EXAMPLES}>
                {(ex) => (
                  <button class="cs-example" onClick={() => setDraft("schedule", ex)}>
                    {ex}
                  </button>
                )}
              </For>
            </div>
          </div>
        </Row>
        <Row title="Repeat" desc={`Ran ${settings()?.repeat_completed ?? 0} times so far. Leave empty to repeat forever.`}>
          <input
            class="s-input cs-input cs-narrow"
            inputmode="numeric"
            placeholder="forever"
            value={draft.repeat}
            onInput={(e) => setDraft("repeat", e.currentTarget.value.replace(/[^0-9]/g, ""))}
          />
        </Row>
      </Section>

      <Section title="Instruction" hint="The prompt the agent receives on every tick. Attached skills are inserted before it.">
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

      <Section
        title="Skills"
        hint="Preloaded into the prompt on every tick. Large skills count against a bound controller's budget; the instruction can ask the agent to load a skill on demand instead."
      >
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

      <Section title="Model" hint="Leave empty to follow Hermes' default for crons.">
        <Row title="Model">
          <input class="s-input cs-input" spellcheck={false} placeholder="default" value={draft.model} onInput={(e) => setDraft("model", e.currentTarget.value)} />
        </Row>
        <Row title="Provider">
          <input class="s-input cs-input" spellcheck={false} placeholder="default" value={draft.provider} onInput={(e) => setDraft("provider", e.currentTarget.value)} />
        </Row>
        <Row title="Reasoning effort">
          <div class="cs-seg">
            <For each={EFFORTS}>
              {(e) => (
                <button class={draft.reasoning_effort === e ? "on" : ""} onClick={() => setDraft("reasoning_effort", e)}>
                  {e || "default"}
                </button>
              )}
            </For>
          </div>
        </Row>
      </Section>

      <Section title="Delivery" hint="Where the agent's answer goes after each tick.">
        <Row title="Deliver to" desc="origin (the chat that created it), local (save only), or project:<slug>.">
          <input class="s-input cs-input" spellcheck={false} value={draft.deliver} onInput={(e) => setDraft("deliver", e.currentTarget.value)} />
        </Row>
        <Row title="On failure" desc="Optional separate target for failure notices. Empty uses the target above.">
          <input class="s-input cs-input" spellcheck={false} placeholder="same as above" value={draft.failure_deliver} onInput={(e) => setDraft("failure_deliver", e.currentTarget.value)} />
        </Row>
        <Row title="Continuity" desc="Each tick sees this cron's previous output, so it can continue instead of starting over.">
          <button class={`toggle ${draft.continuity ? "on" : ""}`} role="switch" aria-checked={draft.continuity} onClick={() => setDraft("continuity", !draft.continuity)} />
        </Row>
      </Section>

      <Section title="Advanced">
        <Row title="Working directory" desc="Absolute path the agent runs from. Empty clears it.">
          <input class="s-input cs-input" spellcheck={false} placeholder="none" value={draft.workdir} onInput={(e) => setDraft("workdir", e.currentTarget.value)} />
        </Row>
        <Show when={settings()?.script}>
          <Row title={settings()?.no_agent ? "Script (no agent)" : "Prelude script"} desc="Managed in Hermes.">
            <code class="cs-ro">{settings()?.script}</code>
          </Row>
        </Show>
        <Show when={settings()?.monitor_url || settings()?.monitor_script}>
          <Row title="Monitor source" desc="Managed in Hermes.">
            <code class="cs-ro">{settings()?.monitor_url ?? settings()?.monitor_script}</code>
          </Row>
        </Show>
        <Show when={(settings()?.enabled_toolsets.length ?? 0) > 0}>
          <Row title="Toolsets" desc="Managed in Hermes.">
            <code class="cs-ro">{settings()?.enabled_toolsets.join(", ")}</code>
          </Row>
        </Show>
        <Show when={settings()?.binding}>
          <Row title="Scope binding" desc="Project scope and permissions enforced by Hermes. Read-only." stack>
            <pre class="cs-binding">{JSON.stringify(settings()?.binding, null, 2)}</pre>
          </Row>
        </Show>
        <Row title="Job id">
          <code class="cs-ro">{p.view.job?.id}</code>
        </Row>
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
