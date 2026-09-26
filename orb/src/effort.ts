/**
 * Reasoning effort, mirroring the core's own gate:
 * `normalize_model_effort_for_backend` in `src/api/control/mod.rs`. Only the
 * Codex and Claude Code harnesses accept an effort there, and both accept the
 * same `low..max` ladder; for every other harness `create_mission` forces
 * `model_effort` to null, so Orb shows no control at all rather than offering a
 * value the server would drop or reject.
 *
 * `tests/effort.test.ts` re-derives this table from that Rust function, so the
 * two cannot drift silently. Nothing here is guessed: `ultra` is named in the
 * core's `supported_model_efforts_for_backend` help string but is *not*
 * accepted by the gate, so it is deliberately absent.
 */
export const EFFORT_LADDER = ["low", "medium", "high", "xhigh", "max"] as const;
export type Effort = (typeof EFFORT_LADDER)[number];

export const EFFORT_BY_HARNESS: Readonly<Record<string, readonly Effort[]>> = {
  codex: EFFORT_LADDER,
  claudecode: EFFORT_LADDER,
};

/** The efforts this harness accepts. Empty when it ignores effort entirely. */
export function supportedEfforts(backend?: string | null): readonly Effort[] {
  return (backend && EFFORT_BY_HARNESS[backend]) || [];
}

export function harnessSupportsEffort(backend?: string | null): boolean {
  return supportedEfforts(backend).length > 0;
}

/**
 * The effort actually usable on `backend`, or null for "let the backend pick"
 * (which is sent by omitting `model_effort` on create, and by an explicit empty
 * string on the settings patch — the core trims that to a clear).
 * An effort the harness no longer accepts normalizes away instead of riding
 * along into a request the server would reject.
 */
export function normalizeEffort(effort: string | null | undefined, backend?: string | null): Effort | null {
  const value = (effort ?? "").trim().toLowerCase();
  if (!value) return null;
  const allowed = supportedEfforts(backend);
  return allowed.includes(value as Effort) ? (value as Effort) : null;
}

const LABELS: Record<Effort, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "XHigh",
  max: "Max",
};

/** Menu/chip text. Unset means the backend's own default, never a guessed level. */
export const DEFAULT_EFFORT_LABEL = "Default";

export function effortLabel(effort: string | null | undefined): string {
  const value = (effort ?? "").trim().toLowerCase() as Effort;
  return LABELS[value] ?? DEFAULT_EFFORT_LABEL;
}
