"use client";

// Small shared building blocks for the control page, extracted mechanically
// from control-client.tsx so larger view components can live in their own
// modules without importing the monolith.

import { memo } from "react";
import { cn } from "@/lib/utils";
import { useNow } from "@/lib/now-tick";
import { getMissionDotColor } from "@/lib/mission-status";
import type { AwaitingKind, MissionStatus } from "@/lib/api";
import type { ChatItem } from "../events-reducer";

/** Thinking/stream chat items routed to the side panel. */
export type SidePanelItem = Extract<ChatItem, { kind: "thinking" | "stream" }>;

// Module-level so all duration consumers share the same implementation.
export function formatDuration(seconds: number): string {
  if (seconds <= 0) return "<1s";
  if (seconds < 60) return `${seconds}s`;
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m${secs > 0 ? ` ${secs}s` : ""}`;
}

// Renders a live-updating duration string anchored at `startTime`. ONLY this
// component subscribes to `useNow()`, so the 1 Hz tick re-renders just the
// active duration cell — not every visible done item/tool card. Wrapping a
// parent in this child instead of calling `useNow()` directly avoids the
// per-second commit storm we used to get on the thoughts panel (which can
// hold hundreds of done items, each one of which was subscribing for a value
// it never read).
export const LiveDuration = memo(function LiveDuration({
  startTime,
}: {
  startTime: number;
}) {
  const nowMs = useNow();
  const seconds = Math.max(0, Math.floor((nowMs - startTime) / 1000));
  return <>{formatDuration(seconds)}</>;
});

// Shimmer loading effect
export function Shimmer({ className }: { className?: string }) {
  return (
    <div className={cn("animate-pulse", className)}>
      <div className="h-4 bg-white/[0.06] rounded w-3/4 mb-2" />
      <div className="h-4 bg-white/[0.06] rounded w-1/2 mb-2" />
      <div className="h-4 bg-white/[0.06] rounded w-5/6" />
    </div>
  );
}

export function missionStatusLabel(
  status: MissionStatus,
  isRunning = false,
  awaitingKind?: AwaitingKind | null,
): {
  label: string;
  className: string;
} {
  if (isRunning) {
    return { label: "Running", className: "bg-indigo-500/20 text-indigo-400" };
  }

  switch (status) {
    case "pending":
      return { label: "Pending", className: "bg-zinc-500/20 text-zinc-400" };
    case "active":
      return { label: "Active", className: "bg-indigo-500/20 text-indigo-400" };
    case "awaiting_user":
      // Distinguish "agent asked a question" (decision) from "agent finished,
      // waiting to be acked/merged" (ack). The old single "Needs You" label
      // was ambiguous.
      if (awaitingKind === "decision") {
        return {
          label: "Needs Decision",
          className: "bg-amber-500/20 text-amber-400",
        };
      }
      if (awaitingKind === "ack") {
        return {
          label: "Awaiting Review",
          className: "bg-sky-500/20 text-sky-400",
        };
      }
      return {
        label: "Needs You",
        className: "bg-amber-500/20 text-amber-400",
      };
    case "acknowledged":
      return {
        label: "Acknowledged",
        className: "bg-emerald-500/20 text-emerald-400",
      };
    case "completed":
      return {
        label: "Completed",
        className: "bg-emerald-500/20 text-emerald-400",
      };
    case "failed":
      return { label: "Failed", className: "bg-red-500/20 text-red-400" };
    case "interrupted":
      return {
        label: "Interrupted",
        className: "bg-amber-500/20 text-amber-400",
      };
    case "blocked":
      return {
        label: "Blocked",
        className: "bg-orange-500/20 text-orange-400",
      };
    case "not_feasible":
      return {
        label: "Not Feasible",
        className: "bg-rose-500/20 text-rose-400",
      };
  }
}

export function missionStatusDotClass(
  status: MissionStatus,
  isRunning = false,
): string {
  return getMissionDotColor(status, isRunning);
}
