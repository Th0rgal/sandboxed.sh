import { FileReference, FileReferenceText } from "./fileReferenceContext";
import { For, createMemo, createSignal, type JSX } from "solid-js";
import { openExternalUrl } from "./api";

/**
 * Source-vs-preview for Markdown file views, shared by every one of them so ⌘/
 * works the same on a local demo file and on a core-hosted reference file. It
 * is module-level rather than per-view because the shortcut is handled once, at
 * the window, and the mode is a user preference that should survive switching
 * between files.
 */
const [mdSource, setMdSource] = createSignal(false);
export { mdSource, setMdSource };
export const toggleMdSource = () => setMdSource((on) => !on);

/** Only http(s)/mailto links are rendered as real links; anything else
 * (javascript:, data:, file:) is neutralised so markdown from a mission
 * transcript cannot run script in the webview. */
export function safeHref(raw: string): string | null {
  return /^(https?:|mailto:)/i.test(raw.trim()) ? raw.trim() : null;
}

function inline(text: string): JSX.Element[] {
  const out: JSX.Element[] = [];
  const re = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)]+)\)/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(<FileReferenceText text={text.slice(last, m.index)} />);
    if (m[1] !== undefined) out.push(<strong>{inline(m[1])}</strong>);
    else if (m[2] !== undefined) out.push(<FileReference raw={m[2]}><code>{m[2]}</code></FileReference>);
    else if (!safeHref(m[4])) out.push(<FileReference raw={m[4]}>{m[3]}</FileReference>);
    else
      out.push(
        <a
          href={safeHref(m[4]) ?? "#"}
          onClick={(e) => {
            e.preventDefault();
            const href = safeHref(m[4]);
            if (href) void openExternalUrl(href);
          }}
        >
          {inline(m[3])}
        </a>,
      );
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(<FileReferenceText text={text.slice(last)} />);
  return out;
}

type Align = "left" | "center" | "right";
type Block =
  | { t: "h"; n: number; text: string }
  | { t: "p"; text: string }
  | { t: "ul"; items: string[] }
  | { t: "pre"; lang: string; text: string }
  | { t: "quote"; text: string }
  | { t: "table"; heads: string[]; rows: string[][]; aligns: Align[] };

function tableCells(line: string): string[] {
  let t = line.trim();
  if (t.startsWith("|")) t = t.slice(1);
  if (t.endsWith("|")) t = t.slice(0, -1);
  return t.split("|").map((c) => c.trim());
}
function sepAlign(cell: string): Align | null {
  const n = cell.replace(/\s/g, "").replace(/[−–—]/g, "-");
  if (!/^:?-+:?$/.test(n)) return null;
  const left = n.startsWith(":");
  const right = n.endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  return "left";
}
function isTableSep(line: string): boolean {
  const cells = tableCells(line);
  return cells.length > 0 && cells.every((c) => sepAlign(c) != null);
}
function isPipeRow(line: string): boolean {
  const t = line.trim();
  return t.startsWith("|") && tableCells(t).length >= 2;
}
function readTable(lines: string[], start: number): { block: Extract<Block, { t: "table" }>; next: number } | null {
  const header = lines[start];
  if (!isPipeRow(header) || start + 1 >= lines.length) return null;
  const sep = lines[start + 1];
  const hasSep = isTableSep(sep);
  if (!hasSep && !isPipeRow(sep)) return null;
  const heads = tableCells(header);
  const aligns = hasSep ? tableCells(sep).map((c) => sepAlign(c) ?? "left") : heads.map(() => "left" as Align);
  let i = start + (hasSep ? 2 : 1);
  const rows: string[][] = [];
  while (i < lines.length && isPipeRow(lines[i]) && !isTableSep(lines[i])) {
    rows.push(tableCells(lines[i++]));
  }
  if (!hasSep && rows.length === 0) return null;
  return { block: { t: "table", heads, rows, aligns }, next: i };
}

export function parseMarkdown(src: string): Block[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("```")) {
      const lang = line.slice(3).trim();
      const buf: string[] = [];
      i++;
      while (i < lines.length && !lines[i].startsWith("```")) buf.push(lines[i++]);
      if (i < lines.length) i++;
      out.push({ t: "pre", lang, text: buf.join("\n") });
      continue;
    }
    const hm = /^(#{1,6})\s+(.*)$/.exec(line);
    if (hm) {
      out.push({ t: "h", n: hm[1].length, text: hm[2] });
      i++;
      continue;
    }
    if (/^[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^[-*]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^[-*]\s+/, ""));
        i++;
      }
      out.push({ t: "ul", items });
      continue;
    }
    if (line.startsWith("> ")) {
      out.push({ t: "quote", text: line.slice(2) });
      i++;
      continue;
    }
    if (!line.trim()) {
      i++;
      continue;
    }
    const table = readTable(lines, i);
    if (table) {
      out.push(table.block);
      i = table.next;
      continue;
    }
    const buf = [line];
    i++;
    while (
      i < lines.length &&
      lines[i].trim() &&
      !/^#{1,6}\s/.test(lines[i]) &&
      !/^[-*]\s+/.test(lines[i]) &&
      !lines[i].startsWith("```") &&
      !lines[i].startsWith("> ") &&
      !readTable(lines, i)
    ) {
      buf.push(lines[i++]);
    }
    out.push({ t: "p", text: buf.join(" ") });
  }
  return out;
}

/** Freeze only completed blocks; fences keep blank lines inside the active tail. */
export function incrementalMarkdown() {
  let previous = "", boundary = 0, stable: Block[] = [];
  return (text: string): Block[] => {
    if (!text.startsWith(previous)) { boundary = 0; stable = []; }
    previous = text;
    const tail = text.slice(boundary);
    let fenced = false, end = 0, offset = 0;
    for (const line of tail.split("\n").slice(0, -1)) {
      offset += line.length + 1;
      if (line.startsWith("```")) fenced = !fenced;
      if (!fenced && !line.trim()) end = offset;
    }
    if (end) {
      stable = [...stable, ...parseMarkdown(tail.slice(0, end))];
      boundary += end;
    }
    return [...stable, ...parseMarkdown(text.slice(boundary))];
  };
}

export function MdView(p: { text: string; compact?: boolean }) {
  const parse = incrementalMarkdown();
  const blocks = createMemo(() => parse(p.text));
  return (
    <div class={`md ${p.compact ? "md-compact" : ""}`}>
      <For each={blocks()}>
        {(b) =>
          b.t === "h" && b.n === 1 ? (
            <h1>{inline(b.text)}</h1>
          ) : b.t === "h" && b.n === 2 ? (
            <h2>{inline(b.text)}</h2>
          ) : b.t === "h" ? (
            <h3>{inline(b.text)}</h3>
          ) : b.t === "ul" ? (
            <ul>
              {b.items.map((it) => (
                <li>{inline(it)}</li>
              ))}
            </ul>
          ) : b.t === "pre" ? (
            <pre>
              <code>{b.text}</code>
            </pre>
          ) : b.t === "quote" ? (
            <blockquote>{inline(b.text)}</blockquote>
          ) : b.t === "table" ? (
            <div class="md-table-wrap">
              <table>
                <thead>
                  <tr>
                    {b.heads.map((h, i) => (
                      <th style={{ "text-align": b.aligns[i] ?? "left" }}>{inline(h)}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {b.rows.map((row) => (
                    <tr>
                      {row.map((c, i) => (
                        <td style={{ "text-align": b.aligns[i] ?? "left" }}>{inline(c)}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <p>{inline(b.text)}</p>
          )
        }
      </For>
    </div>
  );
}

function hlInline(text: string): JSX.Element[] {
  const out: JSX.Element[] = [];
  const re = /(\*\*)(.+?)(\*\*)|(`)([^`]+)(`)|(\[)([^\]]+)(\]\()([^)]+)(\))/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1])
      out.push(
        <>
          <span class="md-p">{m[1]}</span>
          <strong>{m[2]}</strong>
          <span class="md-p">{m[3]}</span>
        </>,
      );
    else if (m[4])
      out.push(
        <>
          <span class="md-p">{m[4]}</span>
          <span class="md-code">{m[5]}</span>
          <span class="md-p">{m[6]}</span>
        </>,
      );
    else
      out.push(
        <>
          <span class="md-p">{m[7]}</span>
          <span class="md-link">{m[8]}</span>
          <span class="md-p">{m[9]}</span>
          <span class="md-link">{m[10]}</span>
          <span class="md-p">{m[11]}</span>
        </>,
      );
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function hlLine(line: string): JSX.Element {
  const h = /^(#{1,6})(\s)(.*)$/.exec(line);
  if (h)
    return (
      <>
        <span class="md-p">{h[1]}{h[2]}</span>
        <span class="md-h">{h[3]}</span>
      </>
    );
  if (line.startsWith("```")) return <span class="md-fence">{line}</span>;
  if (/^[-*]\s/.test(line))
    return (
      <>
        <span class="md-p">{line.slice(0, 2)}</span>
        {hlInline(line.slice(2))}
      </>
    );
  if (line.startsWith("> "))
    return (
      <>
        <span class="md-p">{"> "}</span>
        {hlInline(line.slice(2))}
      </>
    );
  return <>{hlInline(line)}</>;
}

export function MdSource(p: { text: string; onInput: (t: string) => void; readOnly?: boolean }) {
  let pre!: HTMLPreElement;
  let ta!: HTMLTextAreaElement;
  const lines = createMemo(() => p.text.split("\n"));
  // The highlighted layer has no scrollbar of its own: it follows the textarea
  // on both axes. Horizontal matters even though both layers wrap, because a
  // single unbreakable run (a long URL, a wide table row) still overflows.
  const sync = () => {
    if (!pre || !ta) return;
    pre.scrollTop = ta.scrollTop;
    pre.scrollLeft = ta.scrollLeft;
  };
  return (
    <div class="md-src">
      <pre ref={pre} class="md-hl" aria-hidden>
        <For each={lines()}>
          {(ln, i) => (
            <>
              {hlLine(ln)}
              {i() < lines().length - 1 ? "\n" : p.text.endsWith("\n") ? "\n" : null}
            </>
          )}
        </For>
      </pre>
      <textarea
        ref={ta}
        class="md-ta"
        readOnly={p.readOnly}
        value={p.text}
        spellcheck={false}
        onScroll={sync}
        onInput={(e) => {
          p.onInput(e.currentTarget.value);
          // Typing at the bottom scrolls the textarea to keep the caret in
          // view; re-sync so the layer beneath follows even if that happened
          // without a scroll event.
          sync();
        }}
      />
    </div>
  );
}

/** The same syntax presentation as the editor, without a writable textarea. */
export function ReadOnlySource(p: { text: string; line?: number }) {
  return <div class="file-source-code"><For each={p.text.split("\n")}>{(line,i)=><div data-line={i()+1} class={p.line===i()+1?"highlight":""}><span class="file-line-number">{i()+1}</span><code>{hlLine(line)||" "}</code></div>}</For></div>;
}
