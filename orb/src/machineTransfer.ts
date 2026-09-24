import { api, ApiError, getMission, type Mission } from "./api";
import { machineIdentity, nativeInvoke } from "./clientRuns";
import { localBinding, refreshLocalAgents, rememberBinding } from "./localAgents";
export type Machine = { kind: "core" } | { kind: "node" | "client"; id: string };
export interface TransferFile { path: string; bytes: number; sha256: string; executable: boolean }
export interface Manifest { files: TransferFile[]; excluded: string[]; bytes: number }
export interface TransferAction {
  id: string; mission_id: string; phase: string; source: Machine; destination: Machine;
  backend: string; model?: string | null; effort?: string | null;
  source_root?: string | null; destination_root?: string | null;
  manifest?: Manifest | null; receipt?: { root: string; bytes: number; files: number; digest: string } | null;
  created_at: string;
}
export interface Destination { machine: Machine; label: string; available: boolean; reason?: string; harnesses?: string[] }
export interface TransferView { version: number; actions: TransferAction[]; destinations: Destination[] }
export const machineLabel = (m: Machine) => m.kind === "core" ? "Core" : m.kind === "client" ? "This computer" : m.id;
export const sameMachine = (a: Machine, b: Machine) => a.kind === b.kind && (a.kind === "core" || (b.kind !== "core" && a.id === b.id));
export const activeTransfer = (a: TransferAction) => !["activated", "cancelled"].includes(a.phase);
export async function inspectTransfer(id: string): Promise<TransferView> {
  try { return await api<TransferView>(`/api/control/missions/${id}/machine-transfer`); }
  catch (e) { if (e instanceof ApiError && [404, 405].includes(e.status)) throw new Error("Update the connected backend to enable machine transfer. The conversation has not moved."); throw e; }
}
export const transferRequest = <T = TransferAction>(id: string, body: Record<string, unknown>) => api<T>(`/api/control/missions/${id}/machine-transfer`, {
  method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
});
export async function transferFiles<T>(action: TransferAction, side: "source" | "destination", operation: Record<string, unknown>): Promise<T> {
  if (action[side].kind === "client") {
    if ((action[side] as { id: string }).id !== await machineIdentity()) throw new Error("Open Orb on the computer participating in this transfer.");
    const invoke = nativeInvoke();
    if (!invoke) throw new Error("The native transfer adapter is unavailable. Update Orb desktop.");
    return await invoke("local_machine_transfer", { id: action.mission_id, transferId: action.id, side, operation }) as T;
  }
  return transferRequest<T>(action.mission_id, { op: "files", transfer_id: action.id, side, operation });
}
export async function snapshotTransfer(action: TransferAction): Promise<TransferAction> {
  if (action.manifest) return action;
  if (action.source.kind !== "client") return transferFiles(action, "source", { op: "snapshot" });
  const binding = localBinding(action.mission_id);
  if (!binding) throw new Error("The source workspace is unavailable on this computer.");
  const manifest = await transferFiles<Manifest>(action, "source", { op: "snapshot" });
  return transferRequest(action.mission_id, { op: "client_snapshot", transfer_id: action.id, client_id: await machineIdentity(), root: binding.cwd, manifest });
}
export async function copyTransfer(action: TransferAction, progress: (done: number, total: number) => void, cancelled: () => boolean = () => false): Promise<TransferAction> {
  if (action.phase === "verified" || action.phase === "activated") return action;
  const manifest = action.manifest;
  if (!manifest) throw new Error("Prepare a workspace snapshot first.");
  const staged = await transferFiles<{ sealed: boolean; received: Record<string, number> }>(action, "destination", { op: "stage", manifest });
  if (staged.sealed) return action;
  let done = 0;
  for (const file of manifest.files) {
    const received = staged.received?.[file.path] ?? 0;
    done += received; progress(done, manifest.bytes);
    for (let offset = received; offset < file.bytes || (offset === 0 && file.bytes === 0); offset += 1024 * 1024) {
      if (cancelled()) throw new Error("Transfer cancelled before activation.");
      const { data } = await transferFiles<{ data: string }>(action, "source", { op: "read", path: file.path, offset });
      await transferFiles(action, "destination", { op: "write", path: file.path, offset, data });
      done += Math.min(1024 * 1024, file.bytes - offset); progress(done, manifest.bytes);
    }
  }
  return action;
}
export async function verifyTransfer(action: TransferAction): Promise<TransferAction> {
  if (action.phase === "verified" || action.phase === "activated") return action;
  if (action.destination.kind !== "client") return transferFiles(action, "destination", { op: "verify" });
  const receipt = await transferFiles(action, "destination", { op: "verify" });
  return transferRequest(action.mission_id, { op: "client_verified", transfer_id: action.id, client_id: await machineIdentity(), receipt });
}
export async function adoptTransferredWorkspace(action: TransferAction) {
  if (action.phase !== "activated" || action.destination.kind !== "client") return;
  if (action.destination.id !== await machineIdentity()) return;
  if (!action.destination_root) throw new Error("Destination receipt has no workspace.");
  const old = localBinding(action.mission_id);
  if (old?.cwd === action.destination_root) return; // retain the new native session on subsequent reads
  const rows = await refreshLocalAgents();
  const cli = rows.find(r => r.id === action.backend && r.installed && r.path);
  if (!cli?.path) throw new Error("Install the selected harness on this computer before continuing.");
  rememberBinding(action.mission_id, { harness: action.backend, bin: cli.path, cwd: action.destination_root, model: action.model ?? undefined });
}
export async function activateTransfer(action: TransferAction): Promise<Mission> {
  let clientSourceVerified: string | undefined;
  if (action.source.kind === "client") {
    await transferFiles(action, "source", { op: "check_source" });
    clientSourceVerified = await machineIdentity();
  }
  const receipt = await transferRequest(action.mission_id, { op: "activate", transfer_id: action.id, client_source_verified: clientSourceVerified });
  await adoptTransferredWorkspace(receipt);
  return getMission(action.mission_id);
}
