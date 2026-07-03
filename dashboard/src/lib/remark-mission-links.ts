/**
 * Remark plugin: turn bare mission UUIDs in prose into `mission://<id>`
 * links, which markdown-content renders as MissionChip. Opt-in (Hermes chat
 * only) so regular mission transcripts keep rendering ids as plain text.
 */

const UUID_RE =
  /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi;

interface MdNode {
  type: string;
  value?: string;
  url?: string;
  children?: MdNode[];
}

/** Node types whose text must not be linkified. */
const SKIP_PARENTS = new Set(["link", "linkReference", "code", "inlineCode"]);

function splitTextNode(node: MdNode): MdNode[] | null {
  const value = node.value ?? "";
  UUID_RE.lastIndex = 0;
  if (!UUID_RE.test(value)) return null;
  UUID_RE.lastIndex = 0;

  const out: MdNode[] = [];
  let last = 0;
  for (const match of value.matchAll(UUID_RE)) {
    const idx = match.index ?? 0;
    if (idx > last) out.push({ type: "text", value: value.slice(last, idx) });
    const id = match[0].toLowerCase();
    out.push({
      type: "link",
      url: `mission://${id}`,
      children: [{ type: "text", value: id }],
    });
    last = idx + match[0].length;
  }
  if (last < value.length) out.push({ type: "text", value: value.slice(last) });
  return out;
}

function walk(node: MdNode): void {
  if (!node.children || SKIP_PARENTS.has(node.type)) return;
  const next: MdNode[] = [];
  for (const child of node.children) {
    if (child.type === "text") {
      const replaced = splitTextNode(child);
      if (replaced) {
        next.push(...replaced);
        continue;
      }
    } else {
      walk(child);
    }
    next.push(child);
  }
  node.children = next;
}

export function remarkMissionLinks() {
  return (tree: unknown) => {
    walk(tree as MdNode);
  };
}
