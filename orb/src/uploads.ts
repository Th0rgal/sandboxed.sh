import { api, connectionVersion, getApiUrl } from "./api";
import { mentionText, scanMentions } from "./attach";

export interface UploadSource { name: string; localPath?: string; file?: File }
export interface UploadedFile { source: UploadSource; path: string; destination: string; connection: number; endpoint?: string; dataBase64?: string }
export interface UploadReceipt { name: string; path: string; size: number; sha256: string }
const MAX = 20 * 1024 * 1024;
type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
function nativeInvoke(): Invoke | undefined {
  return (window as unknown as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__?.core?.invoke;
}
export function hasNativePicker() { return !!nativeInvoke(); }
export async function pickNativeFiles(): Promise<UploadSource[]> {
  const invoke = nativeInvoke();
  if (!invoke) throw new Error("Use the desktop app to attach a local file path.");
  const files = await invoke<Array<{ name: string; path: string }>>("pick_upload_files");
  return files.map(file => ({ name: file.name, localPath: file.path }));
}
export async function encoded(source: UploadSource): Promise<string> {
  if (source.localPath) {
    const invoke = nativeInvoke();
    if (!invoke) throw new Error("Reopen this file in the desktop app.");
    return invoke<string>("read_upload_file", { path: source.localPath });
  }
  const file = source.file;
  if (!file) throw new Error("Choose the file again.");
  if (file.size > MAX) throw new Error("Files must be 20 MiB or smaller.");
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
  return btoa(binary);
}
export const uploadToken = (path: string) => mentionText({ kind: "file", path });
export async function transferFile(source: UploadSource, destination: string): Promise<UploadedFile> {
  const connection = connectionVersion();
  if (destination === "side") {
    return { source, path: `side-attachment/${crypto.randomUUID()}/${source.name}`, destination, connection, endpoint:getApiUrl(), dataBase64:await encoded(source) };
  }
  if (destination === "local") {
    if (!source.localPath) {
      const invoke=nativeInvoke();
      if(!invoke)throw new Error("Local file attachments require the desktop app.");
      const path=await invoke<string>("stage_upload_file",{name:source.name,dataBase64:await encoded(source)});
      source={...source,localPath:path};
    }
    return { source, path: source.localPath!, destination, connection, endpoint: getApiUrl() };
  }
  const data = await encoded(source);
  if (connection !== connectionVersion()) throw new Error("The backend changed. Choose the file again.");
  const receipt = await api<UploadReceipt>("/api/uploads", {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ node_id: destination, name: source.name, data_base64: data }),
  });
  if (connection !== connectionVersion()) throw new Error("The backend changed during the upload. Choose the file again.");
  return { source, path: receipt.path, destination, connection, endpoint: getApiUrl() };
}
/** Resolve only references still in the draft, and never reuse another machine's path. */
export async function prepareUploads(text: string, files: UploadedFile[], destination: string,
  transfer = transferFile, connection = connectionVersion()): Promise<{ text: string; files: UploadedFile[] }> {
  const next: UploadedFile[] = [];
  for (const file of files) {
    if (!scanMentions(text).some(mention => mention.value === file.path)) continue;
    const resolved = file.destination === destination && file.connection === connection
      ? file : await transfer(file.source, destination);
    if (resolved !== file) {
      for (const mention of scanMentions(text).filter(mention => mention.value === file.path).reverse()) {
        text = text.slice(0, mention.index) + uploadToken(resolved.path) + text.slice(mention.index + mention.raw.length);
      }
    }
    next.push(resolved);
  }
  return { text, files: next };
}
