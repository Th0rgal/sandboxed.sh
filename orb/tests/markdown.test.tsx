import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { parseMarkdown, MdView } from "../src/Markdown";

const HEX_ZEROS = `On this DGX Spark the generator runs at about **1.29 billion addresses/s** (mean of three 20 s GB10 runs: 1299 / 1287 / 1281 M addr/s).

This tool's \`--until-score S\` counts **leading zero bytes** (\`0x00…\`), not hex characters. **S bytes = 2S hex zeros.** Expected wait is geometric: \`16^n / 1.29e9\` seconds for **n hex zeros**, or \`256^S / 1.29e9\` for **S zero bytes**. Median is ~0.69× that; ~95% of runs finish by ~3×.

| Hex zeros \`n\` | Address looks like | Byte score \`S\` | Expected keys | Expected time | 95% time |
|---:|---|---:|---:|---|---|
| 4 | \`0x0000…\` | 2 | 6.6×10⁴ | **50 µs** | 0.2 ms |
| 6 | \`0x000000…\` | 3 | 1.7×10⁷ | **13 ms** | 40 ms |
| 8 | \`0x00000000…\` | 4 | 4.3×10⁹ | **3.3 s** | 10 s |
| 10 | \`0x0000000000…\` | 5 | 1.1×10¹² | **14 min** | 42 min |
| 12 | \`0x000000000000…\` | 6 | 2.8×10¹⁴ | **2.5 days** | 7.6 days |
| 14 | \`0x00000000000000…\` | 7 | 7.2×10¹⁶ | **1.8 years** | 5.3 years |
| 16 | \`0x0000000000000000…\` | 8 | 1.8×10¹⁹ | **450 years** | 1.4 millennia |
| 20 | 10 zero bytes | 10 | 1.2×10²⁴ | **3×10⁷ years** | — |

That matches what we already saw: score 3 (\`0x000000…\`) in the first kernel, score 4 (\`0x00000000…\`) in a few seconds.
`;

describe("parseMarkdown", () => {
  it("keeps GFM tables as tables instead of one mashed paragraph", () => {
    const src = [
      "## GB10 results",
      "| Item | Result |",
      "|---|---|",
      "| Device | NVIDIA GB10 |",
      "| Throughput | **1.29B** addr/s |",
      "",
      "After the table.",
    ].join("\n");
    const blocks = parseMarkdown(src);
    expect(blocks).toMatchObject([
      { t: "h", n: 2, text: "GB10 results" },
      { t: "table", heads: ["Item", "Result"], rows: [["Device", "NVIDIA GB10"], ["Throughput", "**1.29B** addr/s"]] },
      { t: "p", text: "After the table." },
    ]);
  });

  it("parses GFM alignment separators and inline code in headers", () => {
    const src = [
      "Median is ~0.69× that; ~95% of runs finish by ~3×.",
      "",
      "| Hex zeros `n` | Address looks like | Byte score `S` | Expected keys | Expected time | 95% time |",
      "|---:|---|---:|---:|---|---|",
      "| 4 | `0x0000…` | 2 | 6.6×10⁴ | **50 µs** | 0.2 ms |",
      "| 6 | `0x000000…` | 3 | 1.7×10⁷ | **13 ms** | 40 ms |",
      "",
      "That matches what we already saw.",
    ].join("\n");
    const blocks = parseMarkdown(src);
    expect(blocks.map((b) => b.t)).toEqual(["p", "table", "p"]);
    expect(blocks[1]).toMatchObject({
      t: "table",
      heads: ["Hex zeros `n`", "Address looks like", "Byte score `S`", "Expected keys", "Expected time", "95% time"],
      aligns: ["right", "left", "right", "right", "left", "left"],
    });
    expect((blocks[1] as { rows: string[][] }).rows).toHaveLength(2);
  });

  it("renders the vanity Hex zeros estimate as an HTML table", () => {
    const blocks = parseMarkdown(HEX_ZEROS);
    expect(blocks.some((b) => b.t === "table")).toBe(true);
    const { container } = render(() => <MdView text={HEX_ZEROS} compact />);
    expect(container.querySelectorAll("table")).toHaveLength(1);
    expect(container.querySelectorAll("th").length).toBe(6);
    expect(container.querySelectorAll("tr").length).toBe(9); // header + 8 data
    expect(container.textContent).not.toContain("|---|");
  });
});

it("keeps quote separators and paragraphs in one blockquote",()=>{
 const text="> Salut **équipe**.\n>\n> Le rapport est prêt.\n>\n> Merci.";
 const {container}=render(()=><MdView text={text}/>);
 expect(container.querySelectorAll('blockquote')).toHaveLength(1);
 expect(container.querySelectorAll('blockquote p')).toHaveLength(3);
 expect(container.textContent).not.toContain('>');
 expect(container.querySelector('blockquote strong')?.textContent).toBe('équipe');
});
it("renders nested quotes, lists and fenced code within a quote",()=>{
 const {container}=render(()=><MdView text={'> Outer\n>\n> > Inner\n>\n> - one\n> - two\n>\n> ```txt\n> code\n> ```\n\nOutside'}/>);
 expect(container.querySelectorAll('blockquote')).toHaveLength(2);
 expect(container.querySelectorAll('blockquote li')).toHaveLength(2);
 expect(container.querySelector('blockquote pre')?.textContent).toBe('code');
 expect(container.querySelector(':scope > .md > p')?.textContent).toBe('Outside');
});
