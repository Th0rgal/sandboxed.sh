"use client";

// Mission workbench side panel: mission metadata, status controls, child
// missions, and quick actions. Extracted mechanically from
// control-client.tsx.

import { useEffect, useRef, useState } from "react";
import {
  BriefcaseBusiness,
  CheckCircle,
  ChevronRight,
  Clipboard,
  Clock,
  Flag,
  Inbox,
  Layers,
  RotateCcw,
  Square,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { AsyncButton } from "@/components/ui/async-button";
import { RelativeTime } from "@/components/ui/relative-time";
import { getMissionShortName } from "@/lib/mission-display";
import type { inferMissionRole } from "@/lib/mission-role";
import type { Mission, MissionStatus } from "@/lib/api";
import type { MissionStateSummary } from "../events-reducer";
import { missionStatusDotClass, missionStatusLabel } from "./common";
import { MissionTaskBoard } from "./MissionTaskBoard";

export function MissionWorkbenchPanel({
  mission,
  workspaceLabel,
  role,
  isRunning,
  childMissions,
  queueLen,
  missionState,
  onClose,
  onResume,
  onCancel,
  onOpenAutomations,
  onOpenSwitcher,
  onViewMission,
  onSetStatus,
  onCopyDebug,
  runSettingsSlot,
  className,
}: {
  mission: Mission | null;
  workspaceLabel?: string;
  role: ReturnType<typeof inferMissionRole>;
  isRunning: boolean;
  childMissions: Mission[];
  /** Pending message count, surfaced inline alongside status. */
  queueLen?: number;
  /** Agent task board + next-wakeup marker derived from chat items. */
  missionState?: MissionStateSummary;
  onClose: () => void;
  onResume: () => void;
  onCancel: (missionId: string) => void;
  onOpenAutomations: () => void;
  onOpenSwitcher: () => void;
  onViewMission: (missionId: string) => void;
  onSetStatus: (status: MissionStatus) => void | Promise<void>;
  /** Copy a JSON debug snapshot (mission + stream phase) to the clipboard. */
  onCopyDebug: () => void | Promise<void>;
  /**
   * Optional slot for the mission's run-settings editor (the
   * `<NewMissionDialog mode="edit">` trigger). Rendered on its own row below
   * the action grid so the dialog's larger button doesn't break the 2-col
   * rhythm of the other actions.
   */
  runSettingsSlot?: React.ReactNode;
  className?: string;
}) {
  const title =
    mission?.title?.trim() ||
    (mission ? getMissionShortName(mission.id) : "No mission selected");
  const status = mission
    ? missionStatusLabel(mission.status, isRunning, mission.awaiting_kind)
    : null;
  const canResume =
    mission &&
    !isRunning &&
    mission.resumable &&
    (mission.status === "interrupted" ||
      mission.status === "blocked" ||
      mission.status === "failed");

  // Effective model: an explicit per-mission override wins, otherwise fall
  // back to the model recorded from the last run's metadata. Strip any
  // `provider/` prefix for display (matching the assistant message badge) but
  // keep the full value in the tooltip.
  const modelOverride = mission?.model_override?.trim() || undefined;
  const modelRecorded = mission?.metadata_model?.trim() || undefined;
  const modelRaw = modelOverride || modelRecorded || null;
  const modelEffort = mission?.model_effort?.trim() || undefined;
  const modelDisplay = modelRaw
    ? modelRaw.includes("/")
      ? modelRaw.split("/").pop()
      : modelRaw
    : null;

  const [markAsOpen, setMarkAsOpen] = useState(false);
  const markAsRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!markAsOpen) return;
    function handlePointerDown(event: MouseEvent) {
      if (
        markAsRef.current &&
        !markAsRef.current.contains(event.target as Node)
      ) {
        setMarkAsOpen(false);
      }
    }
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") setMarkAsOpen(false);
    }
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKey);
    };
  }, [markAsOpen]);

  useEffect(() => {
    setMarkAsOpen(false);
  }, [mission?.id]);

  return (
    <aside
      className={cn(
        "w-full h-full flex flex-col rounded-2xl glass-panel border border-white/[0.06] overflow-hidden animate-slide-in-right",
        className,
      )}
      aria-label="Mission workbench"
    >
      <div className="flex items-center justify-between border-b border-white/[0.06] px-3 py-2">
        <div className="flex min-w-0 items-center gap-2">
          <BriefcaseBusiness className="h-3.5 w-3.5 shrink-0 text-indigo-400" />
          <span className="truncate text-xs font-medium text-white/90">
            Workbench
          </span>
        </div>
        <button
          onClick={onClose}
          className="flex h-5 w-5 items-center justify-center rounded text-white/40 hover:bg-white/[0.04] hover:text-white transition-colors"
          title="Close workbench"
        >
          <X className="h-3 w-3" />
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-2.5 text-xs">
        {!mission ? (
          <div className="flex h-full flex-col items-center justify-center text-center">
            <Inbox className="mb-3 h-8 w-8 text-white/20" />
            <p className="text-sm text-white/40">
              Select a mission to inspect.
            </p>
            <button
              onClick={onOpenSwitcher}
              className="mt-4 rounded-md border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-xs text-white/70 hover:bg-white/[0.04]"
            >
              Open mission switcher
            </button>
          </div>
        ) : (
          <>
            <p
              className="line-clamp-2 text-xs font-medium leading-snug text-white/85"
              title={title}
            >
              {title}
            </p>

            <dl className="mt-2 space-y-0.5 text-[11px]">
              <Row label="Status">
                <span className="flex items-center gap-1.5">
                  <span
                    className={cn(
                      "h-1.5 w-1.5 rounded-full",
                      missionStatusDotClass(mission.status, isRunning),
                    )}
                  />
                  <span className={cn("font-medium", status?.className)}>
                    {status?.label}
                  </span>
                </span>
              </Row>
              {queueLen !== undefined && queueLen > 0 && (
                <Row label="Queue">
                  <span
                    className={cn(
                      "font-mono tabular-nums",
                      queueLen >= 3 ? "text-orange-300" : "text-amber-300",
                    )}
                  >
                    {queueLen}
                  </span>
                </Row>
              )}
              <Row label="Role">
                <span className="capitalize font-mono text-white/70">
                  {role ?? "mission"}
                </span>
              </Row>
              <Row label="Model">
                <span className="flex min-w-0 items-center justify-end gap-1.5">
                  {modelOverride && (
                    <span className="shrink-0 text-[10px] font-medium uppercase tracking-wide text-indigo-300/80">
                      override
                    </span>
                  )}
                  <span
                    className={cn(
                      "max-w-[130px] truncate font-mono",
                      modelDisplay ? "text-white/70" : "text-white/40",
                    )}
                    title={
                      modelRaw
                        ? modelEffort
                          ? `${modelRaw} (${modelEffort} effort)`
                          : modelRaw
                        : undefined
                    }
                  >
                    {modelDisplay ?? "Default"}
                  </span>
                </span>
              </Row>
              <Row label="Workspace">
                <span
                  className="truncate font-mono text-white/70 max-w-[160px]"
                  title={workspaceLabel}
                >
                  {workspaceLabel ?? "Unassigned"}
                </span>
              </Row>
              <Row label="Updated">
                <RelativeTime
                  date={mission.updated_at}
                  className="font-mono text-white/70"
                />
              </Row>
              {mission.project && (
                <Row label="Project">
                  <span
                    className="truncate font-mono text-white/70 max-w-[160px]"
                    title={[mission.project, mission.track, mission.intent]
                      .filter(Boolean)
                      .join(" / ")}
                  >
                    {[mission.project, mission.track, mission.intent]
                      .filter(Boolean)
                      .join(" / ")}
                  </span>
                </Row>
              )}
              {mission.github_pr != null && (
                <Row label="PR">
                  <span className="font-mono text-white/70">
                    #{mission.github_pr}
                  </span>
                </Row>
              )}
            </dl>

            {mission.tags && mission.tags.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1">
                {mission.tags.map((tag) => (
                  <span
                    key={tag}
                    className="rounded-full border border-white/[0.08] bg-white/[0.04] px-1.5 py-0.5 text-[10px] font-medium text-white/55"
                  >
                    {tag}
                  </span>
                ))}
              </div>
            )}

            {mission.short_description && (
              <p className="workbench-mission-description mt-2 rounded-md border border-white/[0.05] bg-white/[0.02] px-2 py-1.5 text-[11px] leading-relaxed text-white/50">
                {mission.short_description}
              </p>
            )}

            <div className="mt-3 border-t border-white/[0.06] pt-2.5">
              <p className="mb-1.5 text-[10px] uppercase tracking-wide text-white/30">
                Actions
              </p>
              <div className="grid grid-cols-2 gap-1.5">
                {canResume && (
                  <WorkbenchActionButton
                    onClick={onResume}
                    tone="emerald"
                    icon={RotateCcw}
                    label="Resume"
                  />
                )}
                {isRunning && (
                  <WorkbenchActionButton
                    onClick={() => onCancel(mission.id)}
                    tone="red"
                    icon={Square}
                    label="Stop"
                  />
                )}
                <WorkbenchActionButton
                  onClick={onOpenAutomations}
                  icon={Clock}
                  label="Automations"
                />
                <WorkbenchActionButton
                  onClick={onOpenSwitcher}
                  icon={Layers}
                  label="Switch"
                />
                <div ref={markAsRef} className="relative">
                  <button
                    onClick={() => setMarkAsOpen((prev) => !prev)}
                    aria-haspopup="menu"
                    aria-expanded={markAsOpen}
                    className={cn(
                      "inline-flex h-7 w-full items-center justify-center gap-1 rounded-md border border-white/[0.06] bg-white/[0.02] px-2 text-[11px] font-medium text-white/70 hover:bg-white/[0.04]",
                      markAsOpen && "bg-white/[0.06] text-white",
                    )}
                  >
                    <Flag className="h-3 w-3" />
                    Mark as
                  </button>
                  {markAsOpen && (
                    <div
                      role="menu"
                      className="absolute right-0 top-full z-20 mt-1 w-36 overflow-hidden rounded-md border border-white/[0.08] bg-[#1a1a1a] shadow-xl"
                    >
                      {(
                        ["completed", "blocked", "failed"] as MissionStatus[]
                      ).map((nextStatus) => (
                        <AsyncButton
                          key={nextStatus}
                          role="menuitem"
                          onClick={async () => {
                            try {
                              await onSetStatus(nextStatus);
                            } finally {
                              setMarkAsOpen(false);
                            }
                          }}
                          disabled={mission.status === nextStatus}
                          spinnerClassName="h-3 w-3"
                          className="flex w-full items-center justify-between gap-2 px-2.5 py-1.5 text-[11px] capitalize text-white/70 transition-colors hover:bg-white/[0.06] disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent"
                        >
                          <span>{nextStatus.replace("_", " ")}</span>
                          {mission.status === nextStatus && (
                            <CheckCircle className="h-3 w-3 text-white/40" />
                          )}
                        </AsyncButton>
                      ))}
                    </div>
                  )}
                </div>
                <WorkbenchActionButton
                  onClick={onCopyDebug}
                  icon={Clipboard}
                  label="Copy debug"
                  title="Copy mission + stream debug info as JSON"
                />
              </div>
              {runSettingsSlot && (
                <div className="mt-1.5 [&>div]:w-full [&>div>button]:w-full [&>div>button]:justify-center [&>div>button]:h-7 [&>div>button]:px-2 [&>div>button]:py-0 [&>div>button]:text-[11px] [&>div>button]:gap-1 [&>div>button>svg]:h-3 [&>div>button>svg]:w-3 [&>div>button>span]:!inline">
                  {runSettingsSlot}
                </div>
              )}
            </div>

            {missionState?.upNext && (
              <div className="mt-3 border-t border-white/[0.06] pt-2.5">
                <div className="mb-1 flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-white/40">
                  <Clock className="h-3 w-3" />
                  Up next
                </div>
                <div className="text-[11px] leading-snug text-white/70">
                  {missionState.upNext.reason.length > 160
                    ? missionState.upNext.reason.slice(0, 160) + "…"
                    : missionState.upNext.reason}
                </div>
                <div className="mt-0.5 text-[10px] text-white/35">
                  scheduled{" "}
                  <RelativeTime date={new Date(missionState.upNext.timestamp)} />
                  {missionState.upNext.delaySeconds != null &&
                    ` · fires ~${Math.round(missionState.upNext.delaySeconds / 60)}min later`}
                </div>
              </div>
            )}

            {missionState?.plan && missionState.plan.items.length > 0 && (
              <div className="mt-3 border-t border-white/[0.06] pt-2.5">
                <div className="mb-1.5 flex items-center justify-between">
                  <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-white/40">
                    <Flag className="h-3 w-3" />
                    Plan
                  </div>
                  <span className="text-[10px] text-white/35">
                    {missionState.plan.items.filter((t) => t.status === "completed").length}
                    /{missionState.plan.items.length} done
                  </span>
                </div>
                <ul className="space-y-1">
                  {missionState.plan.items.map((task, i) => (
                    <li
                      key={i}
                      className={cn(
                        "flex items-start gap-1.5 text-[11px] leading-snug",
                        task.status === "completed"
                          ? "text-white/30 line-through"
                          : task.status === "in_progress"
                            ? "text-amber-200/90"
                            : "text-white/60",
                      )}
                    >
                      <span className="mt-px shrink-0">
                        {task.status === "completed"
                          ? "✓"
                          : task.status === "in_progress"
                            ? "●"
                            : "○"}
                      </span>
                      <span>{task.content}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            {mission && (
              <MissionTaskBoard
                missionId={mission.id}
                onViewMission={onViewMission}
              />
            )}

            {childMissions.length > 0 && (
              <div className="mt-3 border-t border-white/[0.06] pt-2.5">
                <div className="mb-1.5 flex items-center justify-between">
                  <p className="text-[10px] uppercase tracking-wide text-white/30">
                    Workers
                  </p>
                  <span className="text-[10px] tabular-nums text-white/30">
                    {childMissions.length}
                  </span>
                </div>
                <div className="space-y-0.5">
                  {childMissions.slice(0, 8).map((child) => (
                    <button
                      key={child.id}
                      onClick={() => onViewMission(child.id)}
                      className="flex w-full items-center gap-2 rounded px-1.5 py-1 text-left hover:bg-white/[0.04]"
                    >
                      <span
                        className={cn(
                          "h-1.5 w-1.5 rounded-full shrink-0",
                          missionStatusDotClass(child.status),
                        )}
                      />
                      <span className="min-w-0 flex-1 truncate text-[11px] text-white/70">
                        {child.title?.trim() || getMissionShortName(child.id)}
                      </span>
                      <ChevronRight className="h-3 w-3 shrink-0 text-white/30" />
                    </button>
                  ))}
                </div>
              </div>
            )}
          </>
        )}
      </div>
    </aside>
  );
}

function Row({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-2 py-0.5">
      <dt className="text-white/40">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

function WorkbenchActionButton({
  onClick,
  icon: Icon,
  label,
  tone,
  title,
}: {
  onClick: () => void | Promise<void>;
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  tone?: "emerald" | "red";
  title?: string;
}) {
  const toneClasses =
    tone === "emerald"
      ? "border-emerald-500/25 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/15"
      : tone === "red"
        ? "border-red-500/25 bg-red-500/10 text-red-400 hover:bg-red-500/15"
        : "border-white/[0.06] bg-white/[0.02] text-white/70 hover:bg-white/[0.04]";
  return (
    <button
      type="button"
      onClick={() => void onClick()}
      title={title}
      className={cn(
        "inline-flex h-7 w-full items-center justify-center gap-1 rounded-md border px-2 text-[11px] font-medium transition-colors",
        toneClasses,
      )}
    >
      <Icon className="h-3 w-3" />
      {label}
    </button>
  );
}
