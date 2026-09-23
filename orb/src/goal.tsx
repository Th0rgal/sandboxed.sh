import * as Ic from "./icons";

/**
 * Client-side mirror of the server's `parse_goal_objective` (control/mod.rs):
 * `/goal` followed by whitespace, then a non-empty objective. `/goals …` and
 * plain text are not goals. The backend derives `goal_mode` + `goal_objective`
 * from the prompt itself; there is no dedicated create field, so the composer
 * must send the canonical `/goal <objective>` prompt to enter goal mode.
 */
export type GoalDraft = { kind: "none" } | { kind: "empty" } | { kind: "goal"; objective: string };

export function goalDraft(text: string): GoalDraft {
  const rest = text.trimStart();
  if (!rest.startsWith("/goal")) return { kind: "none" };
  const after = rest.slice("/goal".length);
  if (after.length > 0 && !/^\s/.test(after)) return { kind: "none" };
  const objective = after.trim();
  return objective ? { kind: "goal", objective } : { kind: "empty" };
}

/** The objective of a goal message, or null when the text is not a goal. */
export function goalObjective(text: string | null | undefined): string | null {
  const draft = goalDraft(text ?? "");
  return draft.kind === "goal" ? draft.objective : null;
}

/** Canonical goal prompt, identical to the server's `canonical_goal_message`. */
export function planObjective(text: string | null | undefined): string | null {
  const match = /^\/plan(?:\s+([\s\S]*))?$/.exec((text ?? "").trim());
  return match ? (match[1] ?? "").trim() : null;
}

export const goalPrompt = (objective: string) => `/goal ${objective}`;

export const EMPTY_GOAL_ERROR = "Add an objective after /goal, for example “/goal Make the test suite pass”. Your draft is kept.";

const TITLE_MAX = 42;

/** First line, at most 42 characters, with an ellipsis when cut. */
function shortTitle(text: string): string {
  const line = text.trim().split(/\r?\n/, 1)[0]?.trim() ?? "";
  return line.length > TITLE_MAX ? `${line.slice(0, TITLE_MAX - 1).trimEnd()}…` : line;
}

/** Mission title for a composer draft: the objective for goals, never the raw `/goal` command. */
export function missionTitle(text: string): string {
  return shortTitle(goalObjective(text) ?? planObjective(text) ?? text);
}

/** Stored titles from older clients can be the raw `/goal …` prompt; show the objective instead. */
export function displayTitle(title: string | null | undefined): string | null {
  if (!title) return null;
  return goalObjective(title) ?? planObjective(title) ?? title;
}

/** Compact goal indicator shared by the composer, the launch preview and transcript turns. */
export function GoalTag(p: { detail?: string; class?: string }) {
  return (
    <span class={`goal-tag ${p.class ?? ""}`} title="Keep iterating until the objective is met">
      <Ic.TargetIcon size={12} />
      <span class="goal-tag-label">Goal</span>
      {p.detail ? <span class="goal-tag-detail">{p.detail}</span> : null}
    </span>
  );
}

/** Native `/goal` loop — same harness ids as `native_loops.rs`. */
export const GOAL_HARNESSES = new Set(["claudecode", "codex", "grok", "opencode"]);

export type ComposerMode = "goal" | "plan";

export type SlashItem = {
  id: ComposerMode;
  section: "Modes";
  label: string;
  title: string;
};

export function composerModes(backend?: string | null, planSupported = false): SlashItem[] {
  if (backend && !GOAL_HARNESSES.has(backend)) return [];
  return [{ id: "goal", section: "Modes", label: "Goal", title: "Keep iterating until this objective is met" }, ...(planSupported ? [{id:"plan" as const,section:"Modes" as const,label:"Plan",title:"Plan before making changes"}] : [])];
}

/** `/` plus a query with no whitespace — the Cursor slash palette trigger. */
export function slashQuery(text: string): { open: boolean; query: string } {
  const m = /^\/([^\s]*)$/.exec(text);
  return m ? { open: true, query: m[1].toLowerCase() } : { open: false, query: "" };
}

export function filterSlash(items: SlashItem[], query: string): SlashItem[] {
  if (!query) return items;
  return items.filter((it) => it.id.startsWith(query) || it.label.toLowerCase().startsWith(query));
}

/** Turn a typed `/goal …` draft into the visible objective, or null if it is not a goal. */
export function absorbGoalPrefix(text: string): string | null {
  const draft = goalDraft(text);
  if (draft.kind === "goal") return draft.objective;
  if (draft.kind === "empty") return "";
  return null;
}

export function modePrompt(mode: ComposerMode | null, visible: string): string {
  const body = visible.trim();
  if (mode === "goal") return body ? goalPrompt(body) : "/goal";
  if (mode === "plan") return body ? `/plan ${body}` : "/plan";
  return body;
}

/** In-input Cursor-style mode chip: icon, name, dismiss. */
export function ModeChip(p: { mode: ComposerMode; onClear: () => void }) {
  return (
    <span
      class={`mode-chip ${p.mode}-mode`}
      role="status"
      aria-live="polite"
      aria-label={p.mode === "plan" ? "Plan mode" : "Goal mode"}
      title={p.mode === "plan" ? "Plan before making changes" : "Keep iterating until the objective is met"}
    >
      {p.mode === "plan" ? <Ic.PlanIcon size={12} /> : <Ic.TargetIcon size={12} />}
      <span class="mode-chip-label">{p.mode === "plan" ? "Plan" : "Goal"}</span>
      <button type="button" class="mode-chip-x" tabIndex={-1} title={`Remove ${p.mode === "plan" ? "Plan" : "Goal"}`} aria-label={`Remove ${p.mode === "plan" ? "Plan" : "Goal"}`} onClick={(e) => { e.preventDefault(); e.stopPropagation(); p.onClear(); }}>
        <Ic.CloseIcon size={10} />
      </button>
    </span>
  );
}
