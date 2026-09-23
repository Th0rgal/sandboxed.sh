/**
 * Local harnesses: the CLIs installed on this computer, not a sandboxed.sh
 * runner. Detection and process control go through Tauri. Mention rewriting
 * is pure and tested without a desktop shell.
 */
import { bufferedOutput, type OutputEvent } from "./localStream";
import { createSignal } from "solid-js";
import { mentionText, scanMentions, type AttachChip } from "./attach";
import { listProjectFiles, readProjectFile, getProjectController } from "./api";

function savedLocalFailures(): Record<string,string> {
  try { return JSON.parse(localStorage.getItem("orb.localFailures") ?? "{}"); } catch { return {}; }
}
const [localFailures, setLocalFailures] = createSignal<Record<string,string>>(savedLocalFailures());
export const localFailure = (id:string) => localFailures()[id];
export function recordLocalFailure(id:string, error:unknown) {
  const message = error == null ? "" : error instanceof Error ? error.message : String(error);
  setLocalFailures(previous => { const next={...previous}; if(message)next[id]=message;else delete next[id];
    try {localStorage.setItem("orb.localFailures",JSON.stringify(next));} catch {} return next; });
}
export function missingStreamCommand(error:unknown) {
  const message=String(error);
  return /unknown command/i.test(message) || (/local_agents_subscribe/i.test(message) && /command not found|not allowed|not found/i.test(message));
}

export const LOCAL_HARNESSES = ["claudecode", "codex", "grok", "opencode"] as const;
export type LocalHarnessId = (typeof LOCAL_HARNESSES)[number];

const FILE_CAP = 512 * 1024;
const FOLDER_CAP = 256 * 1024;
const PATH_KEY = "orb.localAgentPaths";
const BIND_KEY = "orb.localBindings";

export interface ScanRow {
  id: string;
  bin: string;
  path?: string | null;
  version?: string | null;
  installed: boolean;
}

export interface LocalBinding {
  harness: string;
  bin: string;
  cwd: string;
  model?: string;
  sessionId?: string;
}

export interface LocalFile {
  encoding?: "base64";
  rel: string;
  content: string;
}

const [installed, setInstalled] = createSignal<ScanRow[]>([]);
const runVersions = new Map<string,number>();
const [running, setRunning] = createSignal<Record<string, boolean>>({});
const [liveText, setLiveText] = createSignal<Record<string, string>>({});

export const localInstalled = installed;
export const localRunActive = (id: string) => !!running()[id];
export const localLiveText = (id: string) => liveText()[id] ?? "";

export function pathOverrides(): Record<string, string> {
  try {
    const raw = localStorage.getItem(PATH_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export function setPathOverride(id: string, path: string) {
  const next = pathOverrides();
  const trimmed = path.trim();
  if (trimmed) next[id] = trimmed;
  else delete next[id];
  localStorage.setItem(PATH_KEY, JSON.stringify(next));
}

export function localBinding(id: string): LocalBinding | undefined {
  try {
    const all = JSON.parse(localStorage.getItem(BIND_KEY) || "{}") as Record<string, LocalBinding>;
    return all[id];
  } catch {
    return undefined;
  }
}

export function rememberBinding(id: string, binding: LocalBinding) {
  const all = (() => {
    try {
      return JSON.parse(localStorage.getItem(BIND_KEY) || "{}") as Record<string, LocalBinding>;
    } catch {
      return {};
    }
  })();
  all[id] = binding;
  localStorage.setItem(BIND_KEY, JSON.stringify(all));
  void tauriInvoke()?.("local_bindings", {id, binding}).catch(console.error);
}

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function tauriInvoke(): Invoke | null {
  const g = window as unknown as {
    __TAURI__?: { core?: { invoke?: Invoke } };
    __TAURI_INTERNALS__?: { invoke?: Invoke };
  };
  return g.__TAURI__?.core?.invoke ?? g.__TAURI_INTERNALS__?.invoke ?? null;
}

export async function restoreLocalBindings() {
  const invoke = tauriInvoke();
  if (!invoke) return;
  const stored = await invoke("local_bindings") as Record<string, LocalBinding>;
  const cached = JSON.parse(localStorage.getItem(BIND_KEY) || "{}") as Record<string, LocalBinding>;
  // Older installations only had web storage. Migrate those entries once.
  for (const [id, binding] of Object.entries(cached)) {
    if (!stored[id]) await invoke("local_bindings", {id, binding});
  }
  localStorage.setItem(BIND_KEY, JSON.stringify({...cached, ...stored}));
}

export async function refreshLocalAgents(): Promise<ScanRow[]> {
  await restoreLocalBindings().catch(console.error);
  const invoke = tauriInvoke();
  if (!invoke) {
    setInstalled([]);
    return [];
  }
  try {
    const rows = (await invoke("local_agents_scan", { request: { overrides: pathOverrides() } })) as ScanRow[];
    setInstalled(Array.isArray(rows) ? rows : []);
    return installed();
  } catch {
    setInstalled([]);
    return [];
  }
}

export function installedIds(): string[] {
  return installed().filter((row) => row.installed && row.path).map((row) => row.id);
}

/** Same secret names the backend refuses to materialize. */
export function isSecretPath(rel: string): boolean {
  const lower = rel.replace(/\\/g, "/").toLowerCase();
  const name = lower.split("/").pop() ?? lower;
  if (lower.split("/").some((part) => [".git", ".ssh", ".aws", ".codex", ".claude"].includes(part) || part === ".env" || part.startsWith(".env."))) {
    return true;
  }
  return (
    name === ".env" ||
    name.startsWith(".env.") ||
    name.endsWith(".pem") ||
    name.endsWith(".key") ||
    name === "id_rsa" ||
    name.startsWith("id_rsa.") ||
    name.startsWith("id_ed25519") ||
    name.includes("credentials") ||
    name === "auth.json" ||
    name === "secrets" ||
    name === "secrets.yaml" ||
    name === "secrets.yml" ||
    name === "secrets.json" ||
    name.endsWith(".p12") ||
    name.endsWith(".pfx")
  );
}

export function quotePath(path: string): string {
  return /[\s"]/.test(path) ? `"${path.replace(/"/g, '\\"')}"` : path;
}

/** Replace each written mention with the absolute path that was copied. */
export function rewritePrompt(text: string, replacements: Array<{ raw: string; absolute: string }>): string {
  let out = text;
  for (const row of replacements) {
    if (!row.raw) continue;
    out = out.replace(row.raw, quotePath(row.absolute));
  }
  return out;
}

export interface MaterializeResult {
  prompt: string;
  files: LocalFile[];
  note: string;
}

/**
 * Copy plan for the mentions in `text`. Unknown `@words` stay prose.
 * A secret or unreadable mentioned file refuses the send.
 */
export async function materializeMentions(
  slug: string,
  text: string,
  chips: AttachChip[],
  readFile: (path: string) => Promise<string> = (path) => readProjectFile(slug, path),
  listDir: (path: string) => Promise<Array<{ name: string; kind: string }>> = async (path) => listProjectFiles(slug, path),
): Promise<MaterializeResult> {
  const mentions = scanMentions(text);
  const files: LocalFile[] = [];
  const replacements: Array<{ raw: string; absolute: string }> = [];
  let folderBytes = 0;
  for (const mention of mentions) {
    const bare = mention.value.replace(/\/$/, "");
    const chip = chips.find((item) => {
      if (item.kind === "controller") return bare.toLowerCase() === "controller";
      return item.path?.replace(/\/$/, "") === bare;
    });
    if (!chip) continue;
    if (chip.kind === "controller") {
      let body = "# Controller\n\nNo controller snapshot was available.\n";
      try {
        const view = await getProjectController(slug, 1);
        body = `# Controller\n\n${view.job?.name || slug}\n`;
      } catch {
        /* snapshot stays the fallback line */
      }
      const rel = ".paloma/controller.md";
      files.push({ rel, content: body });
      replacements.push({ raw: mention.raw, absolute: rel });
      continue;
    }
    const path = chip.path ?? "";
    if (isSecretPath(path)) throw new Error(`${path} is not copied. Your draft is kept.`);
    if (chip.kind === "file") {
      let content = "";
      try {
        content = await readFile(path);
      } catch (e) {
        throw new Error(`${path} could not be read. Your draft is kept. ${e instanceof Error ? e.message : ""}`.trim());
      }
      if (content.length > FILE_CAP) throw new Error(`${path} is over the ${FILE_CAP} byte cap. Your draft is kept.`);
      const rel = `.paloma/attach/${path}`;
      files.push({ rel, content });
      replacements.push({ raw: mention.raw, absolute: rel });
      continue;
    }
    const pending: string[] = [path];
    while (pending.length && files.length < 200) {
      const dir = pending.pop()!;
      let entries: Array<{ name: string; kind: string }> = [];
      try {
        entries = await listDir(dir);
      } catch (e) {
        throw new Error(`${dir}/ could not be read. Your draft is kept. ${e instanceof Error ? e.message : ""}`.trim());
      }
      for (const entry of entries) {
        const relPath = `${dir}/${entry.name}`.replace(/^\//, "");
        if (isSecretPath(relPath)) continue;
        if (entry.kind === "dir") pending.push(relPath);
        else {
          if (folderBytes >= FOLDER_CAP) break;
          const content = await readFile(relPath);
          const take = content.slice(0, FOLDER_CAP - folderBytes);
          folderBytes += take.length;
          files.push({ rel: `.paloma/attach/${relPath}`, content: take });
        }
      }
    }
    replacements.push({ raw: mention.raw, absolute: `.paloma/attach/${path}` });
  }
  const prompt = rewritePrompt(text, replacements.map((row) => ({ ...row, absolute: `__ROOT__/${row.absolute}` })));
  return {
    prompt,
    files,
    note: replacements.map((row) => row.absolute).join("\n"),
  };
}

/** Swap the placeholder root for the workspace Tauri created. */
export function bindWorkspace(prompt: string, root: string): string {
  return prompt.replaceAll("__ROOT__", root.replace(/\/$/, ""));
}

export async function localWorkspace(slug: string): Promise<string> {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error("Local agents run in the Orb desktop app.");
  const path = await invoke("local_agents_workspace", { request: { slug } });
  if (typeof path !== "string" || !path) throw new Error("Could not create the local workspace.");
  return path;
}

export async function writeLocalFiles(root: string, files: LocalFile[]): Promise<void> {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error("Local agents run in the Orb desktop app.");
  const result = await invoke("local_agents_write", { request: { root, files } }) as {skipped?: string[]; binary_supported?: boolean};
  if (files.some(file => file.encoding === "base64") && !result?.binary_supported) throw new Error("Restart Orb to enable image attachments. Your draft is kept.");
  if (result?.skipped?.length) throw new Error(`Some files could not be attached: ${result.skipped.join(", ")}`);
}

export interface StartLocal {
  imagePaths?: string[];
  id: string;
  harness: string;
  bin: string;
  cwd: string;
  prompt: string;
  model?: string;
  sessionId?: string;
}

export async function startLocal(req: StartLocal): Promise<void> {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error("Local agents run in the Orb desktop app.");
  runVersions.set(req.id,(runVersions.get(req.id) ?? 0)+1);
  recordLocalFailure(req.id, null);
  setRunning((prev) => ({ ...prev, [req.id]: true }));
  setLiveText((prev) => ({ ...prev, [req.id]: "" }));
  try {
    await invoke("local_agents_start", {
      request: {
        id: req.id,
        harness: req.harness,
        bin: req.bin,
        cwd: req.cwd,
        prompt: req.prompt,
        model: req.model,
        session_id: req.sessionId,
        image_paths: req.imagePaths,
      },
    });
  } catch (e) {
    await reconcileLocalRun(req.id);
    recordLocalFailure(req.id, e);
    throw e;
  }
}

export interface PollLocal {
  text: string;
  done: boolean;
  exit_code?: number | null;
  session_id?: string | null;
  error?: string | null;
  resumed: boolean;
}

export async function pollLocal(id: string): Promise<PollLocal> {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error("Local agents run in the Orb desktop app.");
  return (await invoke("local_agents_poll", { id })) as PollLocal;
}

/** The native runner survives webview reloads; frontend flags do not. */
export async function reconcileLocalRun(id: string): Promise<void> {
  const version=runVersions.get(id);
  try {
    const state = await pollLocal(id);
    if (runVersions.get(id)!==version) return;
    setRunning(prev => ({ ...prev, [id]: !state.done }));
    setLiveText(prev => ({ ...prev, [id]: state.text }));
    if (state.session_id) {
      const binding = localBinding(id);
      if (binding) rememberBinding(id, { ...binding, sessionId: state.session_id });
    }
  } catch (error) {
    // A transport error does not mean the process stopped.
    if (runVersions.get(id)===version && /no local run/i.test(String(error))) setRunning(prev => ({ ...prev, [id]: false }));
  }
}

export async function stopLocal(id: string): Promise<void> {
  runVersions.set(id,(runVersions.get(id) ?? 0)+1);
  const invoke = tauriInvoke();
  if (invoke) {
    await invoke("local_agents_stop", { id });
  }
  setRunning((prev) => ({ ...prev, [id]: false }));
}

/** New native builds push ordered deltas; old binaries retain compatibility. */
export async function followLocal(id: string, onText: (text: string) => void): Promise<PollLocal> {
  const publish = (text:string) => {
    setLiveText(prev=>({...prev,[id]:text}));
    onText(text);
  };
  const core=(window as unknown as {__TAURI__?:{core?:{Channel?:new()=>{onmessage:(event:OutputEvent<PollLocal>)=>void}}}}).__TAURI__?.core;
  const pollUntilDone = async ():Promise<PollLocal> => {
    let last="";
    for(;;){
      const state=await pollLocal(id);
      if(state.text!==last){last=state.text;publish(last)}
      if(state.done)return state;
      await new Promise(resolve=>setTimeout(resolve,400));
    }
  };
  let state:PollLocal;
  try {
    if(core?.Channel){
      try {
        state=await new Promise<PollLocal>((resolve,reject)=>{
          const buffer=bufferedOutput<PollLocal>(publish,resolve);
          const channel=new core.Channel!();
          channel.onmessage=event=>buffer.receive(event);
          void tauriInvoke()!("local_agents_subscribe",{id,onEvent:channel}).catch(error=>{buffer.dispose();reject(error)});
        });
      } catch(error) {
        if(!missingStreamCommand(error))throw error;
        state=await pollUntilDone();
      }
    } else {state=await pollUntilDone();}
    if(state.session_id){const binding=localBinding(id);if(binding)rememberBinding(id,{...binding,sessionId:state.session_id});}
    setRunning(prev=>({...prev,[id]:false}));
    return state;
  } catch(error) {await reconcileLocalRun(id);recordLocalFailure(id,error);throw error;}
}

export async function localSessionGit(cwd: string): Promise<{ repository: string; branch?: string | null } | null> {
  const invoke = tauriInvoke();
  return invoke ? await invoke("local_session_git", { cwd }) as { repository: string; branch?: string | null } | null : null;
}
