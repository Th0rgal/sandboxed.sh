"use client";

import { useCallback, useState } from "react";
import Link from "next/link";
import { Loader } from "lucide-react";

import { getMission, type Mission } from "@/lib/api";
import { getMissionShortName } from "@/lib/mission-display";
import { getMissionDotColor, getMissionTitle, statusLabel } from "@/lib/mission-status";
import { cn } from "@/lib/utils";

/**
 * Inline chip for a mission id mentioned in prose (Hermes chat). Lazily
 * fetches the mission on first hover for the tooltip card; clicking always
 * navigates to /control, even if the fetch failed.
 */
export function MissionChip({ missionId }: { missionId: string }) {
  const [mission, setMission] = useState<Mission | null>(null);
  const [fetchState, setFetchState] = useState<
    "idle" | "loading" | "loaded" | "failed"
  >("idle");

  const prefetch = useCallback(() => {
    if (fetchState !== "idle") return;
    setFetchState("loading");
    getMission(missionId)
      .then((m) => {
        setMission(m);
        setFetchState("loaded");
      })
      .catch(() => setFetchState("failed"));
  }, [fetchState, missionId]);

  const dot = mission
    ? getMissionDotColor(mission.status, false)
    : "bg-white/30";

  return (
    <span className="group/chip relative inline-block align-baseline">
      <Link
        href={`/control?mission=${missionId}`}
        onMouseEnter={prefetch}
        onFocus={prefetch}
        className="inline-flex items-center gap-1.5 rounded-full border border-white/[0.1] bg-white/[0.05] px-2 py-0.5 align-baseline text-xs text-white/80 no-underline transition-colors hover:border-indigo-400/40 hover:bg-indigo-500/10"
      >
        <span className={cn("h-1.5 w-1.5 rounded-full", dot)} />
        {mission
          ? getMissionTitle(mission, { maxLength: 32 })
          : getMissionShortName(missionId)}
      </Link>

      {/* Hover card */}
      <span className="pointer-events-none absolute bottom-full left-0 z-30 mb-1.5 hidden w-64 rounded-xl border border-white/[0.1] bg-[rgb(var(--background-elevated))] p-3 shadow-xl group-hover/chip:block">
        {fetchState === "loading" && (
          <span className="flex items-center gap-2 text-xs text-white/50">
            <Loader className="h-3 w-3 animate-spin" /> Loading mission…
          </span>
        )}
        {fetchState === "failed" && (
          <span className="block text-xs text-white/50">
            Mission not found — it may have been deleted.
            <span className="mt-1 block font-mono text-[10px] text-white/30">
              {missionId}
            </span>
          </span>
        )}
        {mission && (
          <>
            <span className="block truncate text-sm text-white/90">
              {getMissionTitle(mission, { maxLength: 60 })}
            </span>
            <span className="mt-1 flex items-center gap-1.5 text-xs text-white/55">
              <span className={cn("h-1.5 w-1.5 rounded-full", dot)} />
              {statusLabel(mission.status, mission.awaiting_kind ?? null)}
              {mission.workspace_name && (
                <span className="truncate text-white/35">
                  · {mission.workspace_name}
                </span>
              )}
            </span>
            {mission.short_description && (
              <span className="mt-1 line-clamp-3 block text-[11px] leading-snug text-white/45">
                {mission.short_description}
              </span>
            )}
          </>
        )}
      </span>
    </span>
  );
}
