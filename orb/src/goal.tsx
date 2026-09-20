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
  return shortTitle(goalObjective(text) ?? text);
}

/** Stored titles from older clients can be the raw `/goal …` prompt; show the objective instead. */
export function displayTitle(title: string | null | undefined): string | null {
  if (!title) return null;
  return goalObjective(title) ?? title;
}

/** Compact goal indicator shared by the composer, the launch preview and transcript turns. */
export function GoalTag(p: { detail?: string; class?: string }) {
  return (
    <span class={`goal-tag ${p.class ?? ""}`} title="Goal mode: the agent keeps iterating until this objective is met.">
      <Ic.TargetIcon size={12} />
      <span class="goal-tag-label">Goal</span>
      {p.detail ? <span class="goal-tag-detail">{p.detail}</span> : null}
    </span>
  );
}
