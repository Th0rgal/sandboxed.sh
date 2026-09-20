import { describe, expect, it } from "vitest";
import { parseMarkdown } from "../src/Markdown";

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
});
