import { api, type Mission } from "./api";
import { writeLocalFiles } from "./localAgents";

export interface DraftImage { id: string; name: string; type: string; dataUrl: string; }
export const IMAGE_LIMIT = 10 * 1024 * 1024;
export const IMAGE_COUNT = 8;
const extensions: Record<string,string> = { "image/png":"png", "image/jpeg":"jpg", "image/webp":"webp", "image/gif":"gif" };
export async function readImage(file: File): Promise<DraftImage> {
  if (!extensions[file.type]) throw new Error("Use a PNG, JPEG, WebP or GIF image.");
  if (file.size > IMAGE_LIMIT) throw new Error("Images can be up to 10 MB each.");
  const dataUrl = await new Promise<string>((resolve,reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error("Couldn’t read the image. Try pasting it again."));
    reader.readAsDataURL(file);
  });
  const id = crypto.randomUUID();
  return { id, name: `image-${id}.${extensions[file.type]}`, type: file.type, dataUrl };
}
export function imagePrompt(text: string, paths: string[]): string {
  return [text, ...paths.map(path => `[Uploaded: ${path}]`)].filter(Boolean).join("\n\n");
}
export async function stageLocalImages(root: string, images: DraftImage[]): Promise<string[]> {
  if (!images.length) return [];
  const files = images.map(image => ({rel: `.paloma/images/${image.name}`, content:image.dataUrl.split(",")[1], encoding:"base64" as const}));
  await writeLocalFiles(root, files);
  return files.map(file => `${root}/${file.rel}`);
}
export async function stageRemoteImages(images: DraftImage[], mission?: Mission | null): Promise<string[]> {
  if (images.length && mission?.remote_node_id) throw new Error("Image upload to this remote machine is not available yet. Your draft is kept.");
  const paths: string[] = [];
  for (const image of images) {
    const form = new FormData();
    const bytes = Uint8Array.from(atob(image.dataUrl.split(",")[1]), char => char.charCodeAt(0));
    form.append("file", new Blob([bytes], {type:image.type}), image.name);
    const query = new URLSearchParams({path:"./context/"});
    if (mission?.workspace_id) query.set("workspace_id", mission.workspace_id);
    if (mission) query.set("mission_id", mission.id);
    const result = await api<{path:string}>(`/api/fs/upload?${query}`, {method:"POST",body:form});
    if (!result?.path) throw new Error("Image upload did not return a file path. Your draft is kept.");
    paths.push(result.path);
  }
  return paths;
}
