import { api, ApiError, getApiUrl, getJwt } from "./api";
export interface ClientRunReceipt { run_id: string; generation: number; legacy?: boolean }
type Invoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;
export function nativeInvoke(): Invoke | undefined {
  const w = window as unknown as { __TAURI_INTERNALS__?: { invoke: Invoke }; __TAURI__?: { core?: { invoke: Invoke } } };
  return w.__TAURI_INTERNALS__?.invoke ?? w.__TAURI__?.core?.invoke;
}
export async function machineIdentity(): Promise<string> {
  const invoke = nativeInvoke();
  if (!invoke) throw new Error("Open Orb desktop to move a conversation to or from this computer.");
  return await invoke("local_machine_identity") as string;
}
const receipts = new Map<string, ClientRunReceipt>();
const key = (id: string) => `${getApiUrl()}:${id}`;
export function rememberClientRunReceipt(id: string, receipt: ClientRunReceipt) {
  receipts.set(key(id), receipt);
}
async function legacyBackend(id: string): Promise<boolean> {
  try { await api(`/api/control/missions/${id}/machine-transfer`); return false; }
  catch (e) { if (e instanceof ApiError && [404, 405].includes(e.status)) return true; throw e; }
}
const legacyReceipt = (): ClientRunReceipt => ({ run_id: "00000000-0000-0000-0000-000000000000", generation: 0, legacy: true });
export async function clientRunReceipt(id: string): Promise<ClientRunReceipt> {
  const cached = receipts.get(key(id));
  if (cached) return cached;
  let receipt: ClientRunReceipt;
  try {
    receipt = await api<ClientRunReceipt>(`/api/control/missions/${id}/client-run`, {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ op: "inspect", client_id: await machineIdentity() }),
    });
  } catch (error) { if (await legacyBackend(id)) receipt = legacyReceipt(); else throw error; }
  receipts.set(key(id), receipt);
  return receipt;
}

export async function beginClientRun(id: string, prompt: string, cwd: string, sessionId?: string) {
  let client_id: string;
  let nativeLegacy = false;
  try { client_id = await machineIdentity(); }
  catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    if (!/unknown command|command .* not found|not found.*command/i.test(detail)) {
      throw new Error(`Could not identify this computer: ${detail}`);
    }
    if (!await legacyBackend(id)) throw new Error("Update Orb desktop before continuing on this computer.");
    client_id = "00000000-0000-0000-0000-000000000000"; nativeLegacy = true;
  }
  let receipt: ClientRunReceipt & { prompt: string };
  try {
    if (nativeLegacy) receipt = { ...legacyReceipt(), prompt };
    else receipt = await api<ClientRunReceipt & { prompt: string }>(`/api/control/missions/${id}/client-run`, {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ op: "begin", client_id, prompt, cwd, session_id: sessionId }),
    });
  } catch (error) {
    if (!(error instanceof ApiError && [404, 405].includes(error.status)) || !await legacyBackend(id)) throw error;
    receipt = { ...legacyReceipt(), prompt };
  }
  receipts.set(key(id), receipt);
  return { receipt, nativeLegacy, permit: { ...receipt, client_id, api_url: getApiUrl(), token: getJwt() } };
}
