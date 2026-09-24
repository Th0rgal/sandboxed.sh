import { contextManifest } from "./projectContext";
import { getProjectController, listProjectFiles, type MissionAttachment } from "./api";

export type AttachKind = "file" | "folder" | "controller" | "context";

export interface AttachChip {
  id: string;
  kind: AttachKind;
  path?: string;
  label: string;
}

export interface AttachItem {
  id: string;
  kind: AttachKind;
  section: "Files" | "Folders" | "Controller" | "Context";
  path?: string;
  label: string;
}

/** `@` plus a query at the start of the current token. */
export function atQuery(text: string, caret: number): { open: boolean; query: string; start: number } {
  const before = text.slice(0, caret);
  const m = /(^|[\s])@([^\s]*)$/.exec(before);
  if (!m) return { open: false, query: "", start: -1 };
  const start = before.length - m[2].length - 1;
  return { open: true, query: m[2].toLowerCase(), start };
}

export function filterAttach(items: AttachItem[], query: string): AttachItem[] {
  if (!query) return items.slice(0,100);
  const q = query.replace(/^@/, "");
  return items.filter((it) => it.label.toLowerCase().includes(q) || (it.path ?? "").toLowerCase().includes(q)).slice(0,100);
}

export function chipToAttachment(chip: AttachChip): MissionAttachment {
  return chip.kind === "controller" ? { kind: "controller" } : { kind: chip.kind, path: chip.path };
}

export function consumeAtToken(text: string, caret: number): string {
  const q = atQuery(text, caret);
  if (!q.open || q.start < 0) return text;
  return `${text.slice(0, q.start)}${text.slice(caret)}`;
}

/** The literal the controller is written as; it has no path to name. */
export const CONTROLLER_MENTION = "controller";

/**
 * The text a mention is written as, at the point the user typed `@`.
 *
 * Mentions live in the draft as plain text rather than as widgets over the
 * textarea. That keeps every editing gesture native — arrow keys, shift-select,
 * backspace, undo, dictation, select-all — and means what the user sees is
 * exactly what the agent receives. A quoted form covers paths containing
 * spaces; full paths (never basenames) keep two files of the same name in
 * different folders apart.
 */
export function mentionText(item: { kind: AttachKind; path?: string }): string {
  if (item.kind === "controller") return `@${CONTROLLER_MENTION}`;
  const path = item.path ?? "";
  // A folder keeps its trailing slash so it reads as a folder in the sentence.
  const written = item.kind === "folder" && !path.endsWith("/") ? `${path}/` : path;
  return /[\s"]/.test(written) ? `@"${written.replace(/"/g, '\\"')}"` : `@${written}`;
}

/** Every mention written in a draft, in the order they appear. */
export function scanMentions(text: string): Array<{ raw: string; value: string; index: number }> {
  const out: Array<{ raw: string; value: string; index: number }> = [];
  // `@"..."` (spaces allowed, `\"` escapes a quote) or a bare run up to
  // whitespace. Must start the token, so an email address never matches.
  const re = /(^|[\s(\[{])@(?:"((?:[^"\\]|\\.)*)"|([^\s)\]},;]+))/g;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const quoted = m[2] !== undefined;
    const value = quoted ? m[2].replace(/\\(.)/g, "$1") : m[3];
    const at = m.index + m[1].length;
    out.push({ raw: text.slice(at, m.index + m[0].length), value, index: at });
  }
  return out;
}

/** Trailing punctuation a bare mention should not swallow: "see @a/b.md." */
function trimBare(value: string): string {
  return value.replace(/[.,;:!?]+$/, "");
}

/**
 * The attachments a draft actually refers to, resolved against what the project
 * offers. Derived from the text on every send rather than tracked beside it, so
 * deleting a mention detaches it — nothing can ride along invisibly — and
 * re-typing one attaches it again.
 *
 * Order follows the sentence, and a file mentioned twice is sent once.
 */
export function mentionedChips(text: string, items: AttachItem[]): AttachChip[] {
  const byPath = new Map<string, AttachItem>();
  for (const item of items) {
    if (item.path) byPath.set(item.path.replace(/\/$/, ""), item);
  }
  const controller = items.find((it) => it.kind === "controller");
  const chips: AttachChip[] = [];
  const seen = new Set<string>();
  for (const mention of scanMentions(text)) {
    const bare = (mention.raw.startsWith('@"') ? mention.value : trimBare(mention.value)).replace(/\/$/, "");
    const item =
      bare.toLowerCase() === CONTROLLER_MENTION && controller
        ? controller
        : byPath.get(bare) ?? byPath.get(trimBare(bare).replace(/\/$/, ""));
    if (bare === "context" || bare.startsWith("context/")) {
      if (!seen.has(bare)) { seen.add(bare); chips.push({id:`context:${bare}`,kind:"context",path:bare,label:bare}); }
      continue;
    }
    // An unknown `@word` is ordinary prose, not a silent attachment.
    if (!item || seen.has(item.id)) continue;
    seen.add(item.id);
    chips.push({ id: item.id, kind: item.kind, path: item.path, label: item.label });
  }
  return chips;
}

/**
 * Replace the `@query` being typed with the chosen mention and return the new
 * draft plus where the caret belongs. A trailing space lets the sentence carry
 * on without the next word joining the path.
 */
export function insertMention(
  text: string,
  caret: number,
  item: { kind: AttachKind; path?: string },
): { text: string; caret: number } {
  const q = atQuery(text, caret);
  const start = q.open && q.start >= 0 ? q.start : caret;
  const token = `${mentionText(item)} `;
  return {
    text: `${text.slice(0, start)}${token}${text.slice(caret)}`,
    caret: start + token.length,
  };
}

export async function loadAttachItems(slug: string): Promise<AttachItem[]> {
  const items: AttachItem[] = [];
  let context: AttachItem[] = [];
  try {
    const manifest=await contextManifest(slug);
    context=[{id:"context:root",kind:"context",path:"context",label:"context/",section:"Context"},...Object.entries(manifest.entries).map(([path,entry])=>({id:`context:${path}`,kind:"context" as const,path:`context/${path}`,label:`context/${path}${entry.directory?"/":""}`,section:"Context" as const}))];
  } catch { /* Older servers do not advertise synchronized context. */ }
  try {
    const controller = await getProjectController(slug, 1);
    if (controller.job) {
      items.push({
        id: `controller:${slug}`,
        kind: "controller",
        section: "Controller",
        label: controller.job.name || `${slug} controller`,
      });
    }
  } catch {
    /* no cron */
  }
  await walkFiles(slug, "", items, 0);
  return [...context,...items];
}

async function walkFiles(slug: string, path: string, items: AttachItem[], depth: number) {
  if (depth > 3 || items.length >= 200) return;
  let entries: Awaited<ReturnType<typeof listProjectFiles>> = [];
  try {
    entries = await listProjectFiles(slug, path);
  } catch {
    return;
  }
  for (const entry of entries) {
    if (items.length >= 200) break;
    const rel = path ? `${path}/${entry.name}` : entry.name;
    if (entry.kind === "dir") {
      items.push({ id: `folder:${rel}`, kind: "folder", section: "Folders", path: rel, label: `${rel}/` });
      await walkFiles(slug, rel, items, depth + 1);
    } else {
      items.push({ id: `file:${rel}`, kind: "file", section: "Files", path: rel, label: rel });
    }
  }
}
