import { For, createMemo, type JSX } from "solid-js";
import { openExternalUrl } from "./api";

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
    if (m.index > last) out.push(text.slice(last, m.index));
    if (m[1] !== undefined) out.push(<strong>{inline(m[1])}</strong>);
    else if (m[2] !== undefined) out.push(<code>{m[2]}</code>);
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
  if (last < text.length) out.push(text.slice(last));
  return out;
}

type Block =
  | { t: "h"; n: number; text: string }
  | { t: "p"; text: string }
  | { t: "ul"; items: string[] }
  | { t: "pre"; lang: string; text: string }
  | { t: "quote"; text: string }
  | { t: "table"; heads: string[]; rows: string[][] };

function isTableSep(line: string): boolean {
  const t = line.trim();
  return /^\|?[\s:|-]+\|[\s:|-]*\|?$/.test(t) && t.includes("-");
}
function tableCells(line: string): string[] {
  let t = line.trim();
  if (t.startsWith("|")) t = t.slice(1);
  if (t.endsWith("|")) t = t.slice(0, -1);
  return t.split("|").map((c) => c.trim());
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
    if (line.includes("|") && i + 1 < lines.length && isTableSep(lines[i + 1])) {
      const heads = tableCells(line);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && lines[i].includes("|") && !isTableSep(lines[i])) {
        rows.push(tableCells(lines[i++]));
      }
      out.push({ t: "table", heads, rows });
      continue;
    }
    const buf = [line];
    i++;
    while (i < lines.length && lines[i].trim() && !/^#{1,6}\s/.test(lines[i]) && !/^[-*]\s+/.test(lines[i]) && !lines[i].startsWith("```") && !lines[i].startsWith("> ") && !(lines[i].includes("|") && i + 1 < lines.length && isTableSep(lines[i + 1]))) {
      buf.push(lines[i++]);
    }
    out.push({ t: "p", text: buf.join(" ") });
  }
  return out;
}

export function MdView(p: { text: string; compact?: boolean }) {
  const blocks = createMemo(() => parseMarkdown(p.text));
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
            <table>
              <thead>
                <tr>
                  {b.heads.map((h) => (
                    <th>{inline(h)}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row) => (
                  <tr>
                    {row.map((c) => (
                      <td>{inline(c)}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
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

export function MdSource(p: { text: string; onInput: (t: string) => void }) {
  let pre!: HTMLPreElement;
  let ta!: HTMLTextAreaElement;
  const lines = createMemo(() => p.text.split("\n"));
  const sync = () => {
    if (pre && ta) pre.scrollTop = ta.scrollTop;
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
        value={p.text}
        spellcheck={false}
        onScroll={sync}
        onInput={(e) => p.onInput(e.currentTarget.value)}
      />
    </div>
  );
}
