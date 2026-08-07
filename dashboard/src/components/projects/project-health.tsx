/**
 * Controller mode and mission-health presentation.
 *
 * Two signals the board used to drop on the floor:
 *
 * 1. `health` — computed and served by the backend rollup since day one, but
 *    absent from the frontend type, so a project whose oldest track had been
 *    failing for days looked identical to a healthy one.
 * 2. `mode` — the controller's own `[CTRL: … mode=… ]` trailer. Before it,
 *    "healthy and quiet", "stuck on the same blocker for sixteen ticks" and
 *    "deliberately paused" all rendered as the same silence.
 *
 * Both degrade to nothing: a controller that has not adopted the trailer, or a
 * backend that predates the rollup, renders exactly as the board did before.
 */

import type { ProjectHealth, ProjectRow, TrackVerdict } from "@/lib/api/projects";
import { cn } from "@/lib/utils";

/** Silence longer than this on an otherwise live project is worth flagging:
 *  it is the cheap client-side proxy for "has this controller stopped
 *  ticking?", which the backend cannot answer (crons live in Hermes). */
const STALE_UPDATE_HOURS = 24;

export type ControllerMode = {
  /** The regime itself. */
  base: "active" | "blocked" | "paused";
  /** Optional cause carried as `blocked:transport-cap`. */
  cause: string | null;
};

/** Read the controller mode off the newest delivery. Returns null when the
 *  controller has not adopted the trailer — callers must render nothing in
 *  that case rather than inventing an "unknown" state. */
export function parseMode(project: ProjectRow): ControllerMode | null {
  const raw = project.latest_update?.mode;
  if (!raw) return null;
  const [base, ...rest] = raw.trim().toLowerCase().split(":");
  if (base !== "active" && base !== "blocked" && base !== "paused") return null;
  const cause = rest.join(":").trim();
  return { base, cause: cause.length > 0 ? cause : null };
}

/** True when a project has gone quiet for long enough that its controller may
 *  have died rather than simply having nothing to say. */
export function isStale(project: ProjectRow): boolean {
  const at = project.latest_update?.at;
  if (!at) return false;
  const ms = Date.now() - new Date(at).getTime();
  return Number.isFinite(ms) && ms > STALE_UPDATE_HOURS * 3600_000;
}

/** One-line health digest, or null when there is nothing worth saying.
 *  Only speaks up when a track actually needs attention — otherwise the row
 *  keeps showing the controller's own headline, which is more informative. */
export function healthDigest(health: ProjectHealth | undefined): string | null {
  if (!health || health.tracks_needing_attention < 1) return null;
  const parts: string[] = [];
  const tracks = health.tracks.length;
  if (tracks > 0) parts.push(`${tracks} track${tracks === 1 ? "" : "s"}`);
  if (health.failed > 0) parts.push(`${health.failed} failing`);
  if (health.overdue > 0) parts.push(`${health.overdue} overdue`);
  if (health.active > 0) parts.push(`${health.active} active`);
  return parts.length > 0 ? parts.join(" · ") : null;
}

const MODE_LABEL: Record<ControllerMode["base"], string> = {
  active: "active",
  blocked: "blocked",
  paused: "paused",
};

/** Compact mode indicator. Amber is reserved for "someone should look at
 *  this"; a paused project is deliberate, so it reads as dimmed, not alarming. */
export function ModeChip({
  mode,
  className,
}: {
  mode: ControllerMode | null;
  className?: string;
}) {
  if (!mode) return null;
  const label = mode.cause
    ? `${MODE_LABEL[mode.base]}: ${mode.cause}`
    : MODE_LABEL[mode.base];
  return (
    <span
      title={label}
      className={cn(
        "inline-flex shrink-0 items-center gap-1 truncate text-[10px] uppercase tracking-wide",
        mode.base === "blocked" && "text-amber-400/80",
        mode.base === "active" && "text-indigo-300/70",
        mode.base === "paused" && "text-white/35",
        className,
      )}
    >
      <span
        className={cn(
          "h-1 w-1 shrink-0 rounded-full",
          mode.base === "blocked" && "bg-amber-400/80",
          mode.base === "active" && "bg-indigo-400/70",
          mode.base === "paused" && "bg-white/30",
        )}
      />
      {label}
    </span>
  );
}

const VERDICT_TONE: Record<TrackVerdict, string> = {
  failing: "text-amber-400/80",
  overdue: "text-amber-400/70",
  active: "text-indigo-300/70",
  done: "text-white/40",
  idle: "text-white/35",
};

/** Per-track breakdown for the detail pane. The backend already sorts
 *  worst-first, so the row that needs attention is the one you read first. */
export function TrackHealthList({ health }: { health: ProjectHealth | undefined }) {
  if (!health || health.tracks.length === 0) return null;
  return (
    <div className="space-y-1">
      {health.tracks.map((track, index) => {
        const desired = Object.entries(track.desired_states ?? {});
        return (
          <div
            key={track.track ?? `untracked-${index}`}
            className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-[11px]"
          >
            <span className="text-white/70">{track.track ?? "untracked"}</span>
            <span className={VERDICT_TONE[track.verdict]}>{track.verdict}</span>
            <span className="text-white/40">
              {[
                track.active > 0 ? `${track.active} active` : null,
                track.failed > 0 ? `${track.failed} failed` : null,
                track.overdue > 0 ? `${track.overdue} overdue` : null,
                track.completed > 0 ? `${track.completed} done` : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
            {desired.length > 0 && (
              <span className="text-white/30">
                {desired.map(([state, count]) => `${state}: ${count}`).join(" · ")}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}
