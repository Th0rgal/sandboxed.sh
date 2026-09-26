import type { ControllerJob, ControllerSettings, ControllerView } from "./api";

/** Native Hermes records differ from the core controller projection. */
export type HermesJob = Omit<ControllerJob, "schedule"> & Partial<Omit<ControllerSettings, "skills" | "enabled_toolsets">> & {
  skills?: string[] | null;
  enabled_toolsets?: string[] | null;
  context_from?: string[] | string | null;
  schedule?: string | { kind: string; minutes?: number; expr?: string; run_at?: string; display?: string } | null;
  schedule_display?: string;
  repeat?: number | { times?: number | null; completed?: number } | null;
  skill?: string | null;
};

export function scheduleExpression(schedule: HermesJob["schedule"]): string {
  if (typeof schedule === "string") return schedule;
  if (!schedule) return "";
  if (schedule.kind === "interval" && schedule.minutes) {
    const n = schedule.minutes;
    return n % 1440 === 0 ? `every ${n / 1440}d` : n % 60 === 0 ? `every ${n / 60}h` : `every ${n}m`;
  }
  if (schedule.kind === "cron") return schedule.expr ?? "";
  // Preserve timezone and seconds in the custom editor.
  if (schedule.kind === "once") return schedule.run_at ?? "";
  return schedule.display ?? "";
}

export function getProjectCronFromJob(slug: string, raw: HermesJob): ControllerView {
  if (!raw || typeof raw !== "object" || !raw.id) throw new Error("Hermes did not return a job record");
  const repeat = raw.repeat;
  const prompt = raw.prompt ?? "";
  return {
    slug,
    job: { ...raw, schedule: scheduleExpression(raw.schedule), failure_streak: raw.failure_streak ?? 0 },
    settings: {
      ...raw, prompt, prompt_chars: prompt.length,
      skills: raw.skills ?? (raw.skill ? [raw.skill] : []),
      repeat_times: typeof repeat === "number" ? repeat : repeat?.times ?? raw.repeat_times ?? null,
      repeat_completed: typeof repeat === "object" && repeat ? repeat.completed ?? 0 : raw.repeat_completed ?? 0,
      no_agent: raw.no_agent ?? false, continuity: raw.continuity ?? (Array.isArray(raw.context_from) ? raw.context_from.some((ref) => ref.toLowerCase() === "self") : raw.context_from?.toLowerCase() === "self"),
      enabled_toolsets: raw.enabled_toolsets ?? [],
    },
    runs: [],
  };
}

/** Translate UI continuity without dropping other jobs used as context. */
export function hermesPatch(patch: import("./api").ControllerPatch, job?: HermesJob): Record<string, unknown> {
  const { continuity, ...out } = patch;
  if (continuity === undefined) return out;
  const refs = job?.context_from;
  const context = (Array.isArray(refs) ? refs : refs ? [refs] : []).filter((id) => id.toLowerCase() !== "self");
  if (continuity) context.push("self");
  return { ...out, context_from: context };
}

/** Older Hermes REST handlers silently filter execution overrides. Surface that mismatch. */
export function ignoredCronFields(patch: import("./api").ControllerPatch, view: ControllerView): string[] {
  const settings = view.settings;
  return (["model", "provider", "reasoning_effort", "workdir", "failure_deliver", "continuity"] as const).filter((key) => {
    if (patch[key] === undefined) return false;
    return key === "continuity" ? !!patch[key] !== !!settings?.[key] : (patch[key] || "") !== (settings?.[key] || "");
  });
}

/** Both primary controllers and additional jobs may contain native schedule objects. */
export type HermesControllerView = Omit<ControllerView, "job"> & { job: HermesJob | null };
export function normalizeControllerView(view: HermesControllerView): ControllerView {
  if (!view.job) return { ...view, job: null };
  const normalized = getProjectCronFromJob(view.slug, view.job);
  return { ...view, job: normalized.job, settings: { ...normalized.settings!, ...view.settings } };
}
