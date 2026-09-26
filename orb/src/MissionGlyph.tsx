import { Dynamic } from "solid-js/web";
import { Show } from "solid-js";
import * as Icon from "./sidebarIcons";
import "./sidebar-status.css";
import { pendingMissionInteraction, type PendingInteraction } from "./missionAttention";

/** Preserve the backend status: idle is not success, and blocked is not failure. */
export function missionStatusPresentation(status: string, request?: PendingInteraction) {
  if (request && !["completed", "failed", "not_feasible", "interrupted", "cancelled", "canceled", "acknowledged"].includes(status)) {
    return { label: request.method === "permission" || request.method === "plan" ? "Approval requested" : "Waiting for your reply", tone: "attention", icon: Icon.MessageCircle };
  }
  switch (status) {
    case "active": case "running": case "resuming": case "starting":
      return { label: "Running", tone: "running", icon: Icon.LoaderCircle };
    case "pending": case "queued":
      return { label: "Queued", tone: "quiet", icon: Icon.Clock };
    case "waiting_background":
      return { label: "Background work running", tone: "running", icon: Icon.Clock };
    case "awaiting_user": case "waiting_user": case "acknowledged":
      return { label: "Ready for a follow-up", tone: "quiet", icon: null };
    case "blocked":
      return { label: "Blocked", tone: "attention", icon: Icon.CircleAlert };
    case "paused":
      return { label: "Paused", tone: "quiet", icon: Icon.Pause };
    case "completed":
      return { label: "Completed", tone: "success", icon: Icon.CircleCheck };
    case "failed": case "not_feasible":
      return { label: status === "failed" ? "Failed" : "Not feasible", tone: "attention", icon: Icon.CircleAlert };
    case "interrupted": case "cancelled":
      return { label: "Interrupted", tone: "quiet", icon: Icon.Pause };
    case "idle":
      return { label: "Idle", tone: "quiet", icon: null };
    default:
      return { label: status || "Unknown status", tone: "quiet", icon: null };
  }
}

export function MissionGlyph(p: { status: string; missionId?: string }) {
  const state = () => missionStatusPresentation(p.status, pendingMissionInteraction(p.missionId));
  return <span class={`mission-glyph ${state().tone}`} data-mission-status={p.status} title={state().label}>
    <Icon.Bot />
    <Show when={state().icon}>{Glyph => <span class="mission-status-mark" aria-hidden="true"><Dynamic component={Glyph()} size={12} class={state().icon === Icon.LoaderCircle ? "mission-status-spin" : undefined} /></span>}</Show>
  </span>;
}
