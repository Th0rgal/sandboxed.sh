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
  return nodeLabel(mission?.remote_job?.node_id ?? mission?.remote_node_id ?? receipt?.nodeId ?? mission?.workspace_name ?? "selected machine");
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
  if (error instanceof ApiError && /remote_command.*required/i.test(error.detail)) return "This remote launch requires a supported harness command. Your draft is kept; no fallback machine was selected.";
  if (error instanceof ApiError) return error.detail || "The launch request was rejected. Your draft is kept.";
  return error instanceof Error ? error.message : String(error);
}
export function missionPhase(mission: Mission | null, activity: boolean) {
  const status = mission?.status;
  if (!status) return { label: "Loading mission", moving: true, detail: "Checking the accepted mission status." };
  if (["failed","interrupted","cancelled","canceled","not_feasible"].includes(status)) {
    const reason = mission?.terminal_reason ?? mission?.remote_job?.terminal_reason ?? mission?.execution?.terminal_reason;
    return { label: status === "interrupted" ? "Interrupted" : status.startsWith("cancel") ? "Cancelled" : "Failed", moving: false, failed: true,
      detail: reason === "orphan_no_runner" ? "The backend could not find an active runner." : mission?.status_message ?? reason?.replaceAll("_", " ") ?? "The mission stopped before completion." };
  }
  if (["completed","done"].includes(status)) return { label:"Completed", moving:false, detail:activity ? "" : "The mission completed without transcript output." };
  if (["paused","blocked","awaiting_user","waiting_user"].includes(status)) return {label:status==="paused"?"Paused":"Waiting for input",moving:false,detail:"The mission is not currently running."};
  const job = mission?.remote_job;
  if (job || mission?.execution?.state === "waiting_remote_job") {
    // Active means durable acceptance, not that the selected harness is running.
    if (job?.phase === "submit_ambiguous") return {label:"Checking submission",moving:true,detail:"The backend is checking whether the remote node accepted the job."};
    if (job?.phase === "unobserved" || job?.phase === "lease_only") return {label:"Checking remote job",moving:true,detail:"Waiting for a current remote job status. Execution is not confirmed."};
    if (["failed", "lost", "cancelled", "canceled"].includes(job?.node_state ?? "") || (job?.exit_code != null && job.exit_code !== 0)) return {label:"Remote job stopped",moving:false,failed:true,detail:job?.error ?? job?.terminal_reason ?? `Remote job ${job?.node_state ?? "exited"}${job?.exit_code != null ? ` (exit ${job.exit_code})` : ""}.`};
    if (job?.phase === "finished" || job?.finished_at) return {label:"Remote job finished",moving:false,detail:"Waiting for the backend to finalize the mission."};
    if (job?.node_state === "running") return {label:"Running",moving:true,detail:"The remote node reports the job is running."};
    if (job?.node_state === "queued") return {label:"Queued",moving:true,detail:"The remote node is waiting for a runner slot."};
    return {label:"Remote job accepted",moving:true,detail:"Waiting for the remote node to confirm execution."};
  }
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
