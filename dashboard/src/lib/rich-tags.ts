/**
 * Parser and transformer for rich `<image>` and `<file>` component tags.
 *
 * Agents can write `<image path="./chart.png" alt="Chart" />` or
 * `<file path="./report.pdf" name="Report" />` in their markdown output.
 * These are pre-processed into markdown-compatible syntax before being
 * rendered by react-markdown.
 */

// Matches self-closing <image .../>/<file .../> tags and paired
// <file ...>inner text</file> tags. The paired form captures any inner text
// (group 3) so a display name written between the tags — e.g.
// `<file path="r.pdf">My Report</file>` — is recognized instead of rendered
// raw. Agents vary the exact XML-ish spelling while streaming, so keep this
// parser deliberately small but permissive.
const TAG_RE = /<(image|file)\s+([^>]*?)(?:\/\s*>|>([\s\S]*?)<\/\1\s*>)/gi;

// Matches a partial (unclosed) tag at the end of streaming content: either a
// half-written opening tag (`<file path="bar` — no `>` yet) or a fully opened
// paired tag whose closing `</file>` hasn't streamed in yet
// (`<file path="x">partial nam`). Excludes self-closing `/>` tags.
const PARTIAL_TAG_RE =
  /<(?:image|file)(?:\s+[^>]*)?$|<(?:image|file)\s+[^>]*[^/>]>(?:(?!<\/(?:image|file)\s*>)[\s\S])*$/i;

/**
 * Trim and collapse whitespace; returns undefined when the value is empty or
 * whitespace-only. Used so an explicit empty/blank attribute (`name=""`,
 * `alt="  "`) is treated as absent consistently across parse/transform/strip —
 * falling back to inner text, then the path basename.
 */
function presentText(value: string | undefined): string | undefined {
  const collapsed = value?.trim().replace(/\s+/g, " ");
  return collapsed ? collapsed : undefined;
}

/** Escape markdown link/image label delimiters so freeform text stays inert. */
function escapeLinkText(value: string): string {
  return value.replace(/[[\]]/g, "\\$&");
}

interface RichTag {
  type: "image" | "file";
  path: string;
  alt?: string;
  name?: string;
}

/** Extract attribute values from a tag's attribute string. */
function parseAttrs(attrStr: string): Record<string, string> {
  const attrs: Record<string, string> = {};
  const attrRe = /(\w+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  let m: RegExpExecArray | null;
  while ((m = attrRe.exec(attrStr)) !== null) {
    attrs[m[1]] = m[2] ?? m[3] ?? "";
  }
  return attrs;
}

/** Parse all rich tags from content, returning structured metadata. */
export function parseRichTags(content: string): RichTag[] {
  const tags: RichTag[] = [];
  let m: RegExpExecArray | null;
  const re = new RegExp(TAG_RE.source, TAG_RE.flags);
  while ((m = re.exec(content)) !== null) {
    const tagType = m[1].toLowerCase() as "image" | "file";
    const attrs = parseAttrs(m[2]);
    if (!attrs.path) continue;
    const innerText = presentText(m[3]);
    tags.push({
      type: tagType,
      path: attrs.path,
      alt: presentText(attrs.alt) ?? (tagType === "image" ? innerText : undefined),
      name: presentText(attrs.name) ?? (tagType === "file" ? innerText : undefined),
    });
  }
  return tags;
}

/**
 * Replace rich tags with markdown-compatible syntax:
 * - `<image path="p" alt="a" />` → `![a](sandboxed-image://p)`
 * - `<file path="p" name="n" />`  → `[n](sandboxed-file://p)`
 *
 * Paths are URI-encoded to handle spaces and special characters.
 */
export function transformRichTags(content: string): string {
  return content.replace(
    TAG_RE,
    (_match, tagType: string, attrStr: string, inner?: string) => {
      const attrs = parseAttrs(attrStr);
      if (!attrs.path) return _match; // leave malformed tags as-is
      const innerText = presentText(inner);
      const basename = attrs.path.split("/").pop();
      const encodedPath = encodeURIComponent(attrs.path);
      if (tagType.toLowerCase() === "image") {
        const alt = presentText(attrs.alt) ?? innerText ?? basename ?? "image";
        return `![${escapeLinkText(alt)}](sandboxed-image://${encodedPath})`;
      } else {
        const name = presentText(attrs.name) ?? innerText ?? basename ?? "file";
        return `[${escapeLinkText(name)}](sandboxed-file://${encodedPath})`;
      }
    },
  );
}

function fileStem(value: string): string {
  return value.replace(/\.[a-z0-9]+$/i, "");
}

function normalizeFileKey(value: string | undefined): string {
  return (value ?? "")
    .trim()
    .toLowerCase()
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ");
}

function basename(value: string | undefined): string {
  return (value ?? "").split(/[\\/]/).pop() ?? "";
}

/**
 * Remove file rich tags whose display name, path, basename, or stem is already
 * rendered elsewhere as a shared-file card.
 */
export function stripRichFileTagsByName(
  content: string,
  names: Iterable<string>,
): string {
  const nameSet = new Set<string>();
  for (const name of names) {
    const raw = String(name ?? "");
    const base = basename(raw);
    for (const key of [raw, base, fileStem(raw), fileStem(base)]) {
      const normalized = normalizeFileKey(key);
      if (normalized) nameSet.add(normalized);
    }
  }
  if (nameSet.size === 0) return content;

  return content.replace(TAG_RE, (match, tagType: string, attrStr: string, inner?: string) => {
    if (tagType.toLowerCase() !== "file") return match;
    const attrs = parseAttrs(attrStr);
    const base = basename(attrs.path);
    const displayName = presentText(attrs.name) ?? presentText(inner) ?? base;
    const candidates = [
      displayName,
      base,
      attrs.path,
      fileStem(displayName),
      fileStem(base),
    ];
    return candidates.some((candidate) =>
      nameSet.has(normalizeFileKey(candidate)),
    )
      ? ""
      : match;
  });
}

/**
 * Detect an incomplete rich tag at the end of streaming content.
 * Returns true if the content ends with something like `<image path="foo`
 * (no closing `/>` yet).
 */
export function hasPartialRichTag(content: string): boolean {
  return PARTIAL_TAG_RE.test(content);
}
