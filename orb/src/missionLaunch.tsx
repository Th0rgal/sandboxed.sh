import { Show } from "solid-js";
import { ApiError, getApiUrl, type Mission } from "./api";
import type { StreamItem } from "./Transcript";

export type LaunchReceipt = { prompt: string; nodeId: string; destination: string };
const receiptKey = (id: string) => `orb.launch:${getApiUrl()}:${id}`;
const receipts = new Map<string, LaunchReceipt>();
export function rememberLaunch(id: string, receipt: LaunchReceipt) {
  receipts.set(receiptKey(id), receipt);
  try { sessionStorage.setItem(receiptKey(id), JSON.stringify(receipt)); } catch { /* memory receipt still works */ }
}
export function recalledLaunch(id: string): LaunchReceipt | undefined {
  const cached = receipts.get(receiptKey(id));
  if (cached) return cached;
  try { return JSON.parse(sessionStorage.getItem(receiptKey(id)) ?? "null") ?? undefined; } catch { return undefined; }
}
export const nodeLabel = (id: string) => id === "core" ? "Core" : id === "dgx-spark" ? "DGX Spark" : id;
export function missionDestination(mission: Mission | null, receipt?: LaunchReceipt) {
  // Server-owned remote placement or the accepted selection takes precedence
  // over workspace_name, which can still name the host's bookkeeping workspace.
  return nodeLabel(mission?.remote_node_id ?? receipt?.nodeId ?? mission?.workspace_name ?? "selected machine");
}
export function initialPrompt(mission: Mission | null, receipt?: LaunchReceipt): string | undefined {
  if (receipt?.prompt) return receipt.prompt;
  const user = mission?.history?.find(entry => entry.role === "user");
  if (user?.content) return user.content;
  if (mission?.goal_mode && mission.goal_objective) return `/goal ${mission.goal_objective.replace(/^\/goal\s+/, "")}`;
}
export function withInitialPrompt(items: StreamItem[], mission: Mission | null, receipt?: LaunchReceipt): StreamItem[] {
  // The placeholder represents the initial user turn only. The first real user
  // event replaces it, even if backend normalization changed the stored text.
  // Subsequent real repeated messages are never deduplicated by text.
  const prompt = initialPrompt(mission, receipt);
  return prompt && !items.some(item => item.kind === "user") ? [{kind:"user",key:`initial:${mission?.id ?? "launch"}`,text:prompt}, ...items] : items;
}
export function launchError(error: unknown): string {
  if (error instanceof ApiError && /remote_command.*required/i.test(error.detail)) return "This backend needs an update for structured remote launches. Your draft is kept; no fallback machine was selected.";
  if (error instanceof ApiError) return error.detail || "The launch request was rejected. Your draft is kept.";
  return error instanceof Error ? error.message : String(error);
}
export function missionPhase(mission: Mission | null, activity: boolean) {
  const status = mission?.status;
  if (!status) return { label: "Loading mission", moving: true, detail: "Checking the accepted mission status." };
  if (["failed","interrupted","cancelled","canceled","not_feasible"].includes(status)) {
    const reason = mission?.terminal_reason ?? mission?.execution?.terminal_reason;
    return { label: status === "interrupted" ? "Interrupted" : status.startsWith("cancel") ? "Cancelled" : "Failed", moving: false, failed: true,
      detail: reason === "orphan_no_runner" ? "The backend could not find an active runner." : mission?.status_message ?? reason?.replaceAll("_", " ") ?? "The mission stopped before completion." };
  }
  if (["completed","done"].includes(status)) return { label:"Completed", moving:false, detail:activity ? "" : "The mission completed without transcript output." };
  if (["paused","blocked","awaiting_user","waiting_user"].includes(status)) return {label:status==="paused"?"Paused":"Waiting for input",moving:false,detail:"The mission is not currently running."};
  if (status === "resuming") return {label:"Resuming",moving:true,detail:"Waiting for the runner to resume."};
  if (["pending","queued"].includes(status)) return {label:"Queued",moving:true,detail:"Request accepted. Waiting for the runner to start."};
  if (["active","running","starting"].includes(status)) return {label:activity?"Running":"Starting",moving:true,detail:activity?"":"Request accepted. Waiting for the first output."};
  return {label:status.replaceAll("_"," "),moving:false,detail:""};
}
export function LaunchStatus(p: { destination: string; mission?: Mission | null; activity?: boolean; submitting?: boolean }) {
  const phase = () => p.submitting ? {label:"Starting",moving:true,detail:"Submitting your request…",failed:false} : missionPhase(p.mission ?? null, !!p.activity);
  return <div class={`launch-status ${phase().failed ? "failed" : ""}`} role="status" aria-live="polite">
    <div><Show when={phase().moving}><span class="launch-pulse" aria-hidden="true" /></Show><span>{phase().label} on {p.destination}</span></div>
    <Show when={phase().detail}><p>{phase().detail}</p></Show>
  </div>;
}
