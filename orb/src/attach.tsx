import { getProjectController, listProjectFiles, type MissionAttachment } from "./api";

export type AttachKind = "file" | "folder" | "controller";

export interface AttachChip {
  id: string;
  kind: AttachKind;
  path?: string;
  label: string;
}

export interface AttachItem {
  id: string;
  kind: AttachKind;
  section: "Files" | "Folders" | "Controller";
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
  if (!query) return items;
  const q = query.replace(/^@/, "");
  return items.filter((it) => it.label.toLowerCase().includes(q) || (it.path ?? "").toLowerCase().includes(q));
}

export function chipToAttachment(chip: AttachChip): MissionAttachment {
  return chip.kind === "controller" ? { kind: "controller" } : { kind: chip.kind, path: chip.path };
}

export function consumeAtToken(text: string, caret: number): string {
  const q = atQuery(text, caret);
  if (!q.open || q.start < 0) return text;
  return `${text.slice(0, q.start)}${text.slice(caret)}`.trim();
}

export async function loadAttachItems(slug: string): Promise<AttachItem[]> {
  const items: AttachItem[] = [];
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
  return items;
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
