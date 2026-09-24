import { api, type Mission } from "./api";
import { writeLocalFiles } from "./localAgents";

export interface DraftImage { id: string; name: string; type: string; dataUrl: string; reference?: number; }
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
export function imagePrompt(text: string, paths: string[], images: DraftImage[] = []): string {
  return [text, ...paths.map((path, index) => `${images[index]?.reference ? `[Image #${images[index].reference}] ` : ""}[Uploaded: ${path}]`)].filter(Boolean).join("\n\n");
}
export async function stageLocalImages(root: string, images: DraftImage[]): Promise<string[]> {
  if (!images.length) return [];
  const files = images.map(image => ({rel: `.paloma/images/${image.name}`, content:image.dataUrl.split(",")[1], encoding:"base64" as const}));
  await writeLocalFiles(root, files);
  return files.map(file => `${root}/${file.rel}`);
}
export async function stageRemoteImages(images: DraftImage[], mission?: Mission | null, destination?: string): Promise<string[]> {
  const node = destination ?? mission?.remote_node_id ?? mission?.remote_job?.node_id ?? "core";
  const paths: string[] = [];
  for (const image of images) {
    if (node !== "core") {
      const result = await api<{path:string}>("/api/uploads", {method:"POST", headers:{"Content-Type":"application/json"}, body:JSON.stringify({node_id:node,name:image.name,data_base64:image.dataUrl.split(",")[1]})});
      if (!result?.path) throw new Error("Image upload did not return a file path. Your draft is kept.");
      paths.push(result.path);
      continue;
    }
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

// Parse inert clipboard HTML: never mount pasted markup or execute its scripts.
export async function readImagePaste(html: string, plain: string, files: File[], firstReference: number) {
  const doc = new DOMParser().parseFromString(html, "text/html");
  doc.querySelectorAll("script,style,template").forEach(node => node.remove());
  const embedded = Array.from(doc.querySelectorAll("img, [data-proton-embedded]"));
  const images: DraftImage[] = [];
  const remaining = [...files];
  const add = async (file: File) => {
    if (images.length >= IMAGE_COUNT) throw new Error(`Attach up to ${IMAGE_COUNT} images at a time.`);
    const image = await readImage(file);
    image.reference = firstReference + images.length;
    images.push(image);
    return `[Image #${image.reference}]`;
  };
  for (const element of embedded) {
    const src = element.getAttribute("src") ?? "";
    let file: File | undefined;
    // Clipboard files accompany cid:/file: images; prefer those bytes when available.
    if (remaining.length) file = remaining.shift();
    else if (/^(data:image\/|https?:\/\/|blob:)/i.test(src)) {
      const response = await fetch(src, {credentials: "omit", signal: AbortSignal.timeout(10000)});
      if (!response.ok) throw new Error("Couldn’t read an embedded image. Copy the image itself or attach it separately.");
      const blob = await response.blob();
      file = new File([blob], "pasted-image", {type: blob.type});
    }
    if (!file) throw new Error(element.hasAttribute("data-proton-embedded")
      ? "Proton Mail copied a placeholder instead of the image. Copy the image itself or attach it separately."
      : "The clipboard didn’t include the embedded image’s bytes. Copy the image itself or attach it separately.");
    element.replaceWith(doc.createTextNode(await add(file)));
  }
  const render = (node: Node): string => {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent ?? "";
    if (!(node instanceof Element)) return "";
    if (node.tagName === "BR") return "\n";
    const content = Array.from(node.childNodes).map(render).join("");
    return /^(P|DIV|LI|TR|H[1-6]|BLOCKQUOTE|PRE)$/.test(node.tagName) ? content + "\n" : content;
  };
  let text = embedded.length ? render(doc.body).replace(/\n{3,}/g, "\n\n").trim() : plain;
  for (const file of remaining) text += `${text ? "\n" : ""}${await add(file)}`;
  return {text, images};
}
