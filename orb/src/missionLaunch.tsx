import { Show } from "solid-js";
import { ApiError, getApiUrl, type Mission, type RemoteLaunchCapability, type RemoteNodesResponse } from "./api";
import { goalObjective, GoalTag } from "./goal";
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
export const TYPED_LAUNCH_UNSUPPORTED = "This backend does not support structured remote launches. Update the connected backend to enable them. Your draft and selection are kept; no mission was submitted.";
export type RemoteSupport = "supported" | "unsupported" | "unknown";
/** Whether the server has confirmed typed remote launches for a harness id. Nothing is assumed until it does. */
export function remoteHarnessSupport(capability: RemoteLaunchCapability | null | undefined, backend: string): RemoteSupport {
  if (!capability || capability.typed !== true || !Array.isArray(capability.harnesses)) return "unknown";
  return capability.harnesses.includes(backend) ? "supported" : "unsupported";
}
/**
 * Pre-POST check against the fresh `GET /api/remote-nodes` answer. Returns the
 * user-facing refusal, or null when the server has confirmed the launch can be
 * accepted. Only harness support and proxy reachability are checked here;
 * model validation and provisioning stay on the typed server path.
 */
export function remoteLaunchPreflight(fleet: RemoteNodesResponse, nodeId: string, pick: { backend: string; model: string }, harnessName: (id: string) => string = id => id): string | null {
  const destination = nodeLabel(nodeId);
  const node = fleet.nodes?.find(n => n.id === nodeId);
  if (!fleet.enabled || !node || node.cordoned || !["online","degraded"].includes(node.status)) return `${destination} is unavailable. Choose an available machine; your draft is kept.`;
  const capability = fleet.remote_launch;
  if (!capability || capability.typed !== true) return TYPED_LAUNCH_UNSUPPORTED;
  const harnesses = Array.isArray(capability.harnesses) ? capability.harnesses : [];
  if (!harnesses.includes(pick.backend)) {
    const supported = harnesses.length ? `This backend currently runs ${harnesses.map(harnessName).join(", ")} on remote nodes.` : "This backend has not enabled any harness on remote nodes yet.";
    return `Remote launch for ${pick.backend} (${pick.model}) is not supported on ${nodeId}. ${supported} Your draft and selection are kept; no mission was submitted.`;
  }
  if (capability.proxy_url_configured === false && remoteHarnessNeedsProxy(capability, pick.backend)) {
    return `${destination} cannot reach this backend's model proxy. Your draft and selection are kept; no mission was submitted.`;
  }
  return null;
}
/** Native Grok uses managed OAuth and does not need the model proxy. Claude Code and OpenCode do, unless the server lists `requires_proxy_harnesses`. */
export function remoteHarnessNeedsProxy(capability: RemoteLaunchCapability, backend: string): boolean {
  const listed = capability.requires_proxy_harnesses;
  if (Array.isArray(listed)) return listed.includes(backend);
  return backend === "claudecode" || backend === "opencode";
}
/** The capability could not be read at all (network/server error): refuse rather than guess. */
export function remoteLaunchUnconfirmed(nodeId: string, error: unknown): string {
  const detail = error instanceof Error ? error.message : String(error);
  return `Could not confirm remote launch support on ${nodeLabel(nodeId)}: ${detail}. Your draft and selection are kept; no mission was submitted.`;
}
export function launchError(error: unknown): string {
  if (error instanceof ApiError && /remote_command.*required/i.test(error.detail)) return TYPED_LAUNCH_UNSUPPORTED;
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
/** Goal mode from persisted state first, then from the accepted prompt (covers the optimistic window before the server answers). */
export function missionGoal(mission: Mission | null | undefined, receipt?: LaunchReceipt): string | null {
  if (mission?.goal_mode && mission.goal_objective) return mission.goal_objective;
  return goalObjective(receipt?.prompt) ?? goalObjective(mission?.history?.find(entry => entry.role === "user")?.content);
}
export function LaunchStatus(p: { destination: string; mission?: Mission | null; activity?: boolean; submitting?: boolean; goal?: string | null }) {
  const phase = () => p.submitting ? {label:"Starting",moving:true,detail:"Submitting your request…",failed:false} : missionPhase(p.mission ?? null, !!p.activity);
  return <div class={`launch-status ${phase().failed ? "failed" : ""}`} role="status" aria-live="polite">
    <div><Show when={phase().moving}><span class="launch-pulse" aria-hidden="true" /></Show><Show when={p.goal}><GoalTag class="small" /></Show><span>{phase().label} on {p.destination}</span></div>
    <Show when={phase().detail}><p>{phase().detail}</p></Show>
  </div>;
}
