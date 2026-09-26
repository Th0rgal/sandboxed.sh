import { test, expect, type Page } from "@playwright/test";

/**
 * The Markdown editor stacks a highlighted `<pre>` under a transparent
 * textarea. If the two layers are not the same box, or wrap at different
 * points, a selection made near the top of the document is painted over a
 * paragraph further down — which is what a user reported.
 *
 * The cause was the global `textarea` rule capping height at 220px: the
 * textarea scrolled inside a short box while the `<pre>` stayed full height.
 * These tests assert the geometry that makes such a mismatch impossible, so
 * the same leak cannot come back from another global rule.
 */

const TOP = "TOP PARAGRAPH — the first words of the document.";
const BOTTOM = "BOTTOM PARAGRAPH — the last words of the document.";
const WRAPPING =
  "A deliberately long line that has to wrap several times so the two layers " +
  "must agree on where every soft break falls, otherwise the highlighted text " +
  "beneath the caret drifts further out of step with every wrapped row.";

/** Distinct top/bottom paragraphs, wrapping lines, headings, blanks, trailing newline. */
const DOC = [
  "# Pareto Credit Vault",
  "",
  TOP,
  "",
  WRAPPING,
  "",
  "## Section two",
  "",
  ...Array.from({ length: 40 }, (_, i) => `- filler item ${i + 1} with enough text on it to occupy a full row`),
  "",
  "### Deep heading",
  "",
  WRAPPING,
  "",
  "```",
  "const fenced = 'code block';",
  "```",
  "",
  BOTTOM,
  "",
].join("\n");

async function openEditor(page: Page) {
  const writes: Array<{ path: string; content: string }> = [];
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path.endsWith("/file") && request.method() === "PUT") {
      writes.push(request.postDataJSON());
      return route.fulfill({ json: { path: request.postDataJSON().path, bytes: 0 } });
    }
    if (path.endsWith("/file")) return route.fulfill({ json: { content: DOC } });
    const json =
      path === "/api/projects" ? { projects: [{ slug: "test", title: "Test" }] }
      : path === "/api/control/missions" ? []
      : path.endsWith("/files") ? { entries: [{ name: "notes.md", kind: "file" }] }
      : path.endsWith("/crons") ? { jobs: [] }
      : { job: null, runs: [] };
    await route.fulfill({ json });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Test", exact: true }).click();
  await page.getByRole("button", { name: "notes.md" }).click();
  await expect(page.locator(".pf-path")).toHaveText("test/notes.md");
  // ⌘/ opens the source layer; Meta is Control on non-macOS builds.
  await page.getByRole("button", { name: "Edit" }).click();
  const ta = page.locator(".md-ta");
  const pre = page.locator(".md-hl");
  await expect(ta).toBeVisible();
  await expect(ta).toHaveValue(DOC);
  return { ta, pre, writes };
}

const box = (locator: ReturnType<Page["locator"]>) =>
  locator.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  });

test("editor layers share one box and wrap identically", async ({ page }) => {
  const { ta, pre } = await openEditor(page);

  // Same box. Before the fix the textarea was capped at 220px tall while the
  // <pre> filled the pane.
  const taBox = await box(ta);
  const preBox = await box(pre);
  expect(taBox.x).toBeCloseTo(preBox.x, 1);
  expect(taBox.y).toBeCloseTo(preBox.y, 1);
  expect(taBox.width).toBeCloseTo(preBox.width, 1);
  expect(taBox.height).toBeCloseTo(preBox.height, 1);
  expect(taBox.height).toBeGreaterThan(260);

  // No global rule may re-cap it.
  expect(await ta.evaluate((el) => getComputedStyle(el).maxHeight)).toBe("none");

  // Identical wrapping: same content height, same padding, same font metrics.
  const metrics = await page.evaluate(() => {
    const t = document.querySelector<HTMLTextAreaElement>(".md-ta")!;
    const p = document.querySelector<HTMLPreElement>(".md-hl")!;
    const cs = (el: Element) => {
      const s = getComputedStyle(el);
      return { font: s.font, lineHeight: s.lineHeight, padding: s.padding, whiteSpace: s.whiteSpace, boxSizing: s.boxSizing };
    };
    return {
      taScrollHeight: t.scrollHeight,
      preScrollHeight: p.scrollHeight,
      taClientWidth: t.clientWidth,
      preClientWidth: p.clientWidth,
      ta: cs(t),
      pre: cs(p),
    };
  });
  // A scrollbar taking layout space in one layer only would break wrapping.
  expect(metrics.taClientWidth).toBe(metrics.preClientWidth);
  expect(metrics.taScrollHeight).toBe(metrics.preScrollHeight);
  expect(metrics.ta.lineHeight).toBe(metrics.pre.lineHeight);
  expect(metrics.ta.padding).toBe(metrics.pre.padding);
  expect(metrics.ta.whiteSpace).toBe(metrics.pre.whiteSpace);
  expect(metrics.ta.boxSizing).toBe(metrics.pre.boxSizing);
  // The document is genuinely taller than the pane, so scrolling is exercised.
  expect(metrics.taScrollHeight).toBeGreaterThan(taBox.height);
});

test("a selection near the top stays over the top text after visiting the bottom", async ({ page }, testInfo) => {
  const { ta, pre } = await openEditor(page);

  // Go to the bottom, then back to the top — the reported sequence.
  await ta.evaluate((el: HTMLTextAreaElement) => { el.scrollTop = el.scrollHeight; el.dispatchEvent(new Event("scroll")); });
  await expect
    .poll(() => page.evaluate(() => document.querySelector(".md-hl")!.scrollTop))
    .toBeGreaterThan(0);
  const atBottom = await page.evaluate(() => ({
    ta: document.querySelector(".md-ta")!.scrollTop,
    pre: document.querySelector(".md-hl")!.scrollTop,
  }));
  expect(atBottom.pre).toBeCloseTo(atBottom.ta, 1);

  await ta.evaluate((el: HTMLTextAreaElement) => { el.scrollTop = 0; el.dispatchEvent(new Event("scroll")); });
  await expect
    .poll(() => page.evaluate(() => document.querySelector(".md-hl")!.scrollTop))
    .toBe(0);

  // Select the top paragraph, as the user did. Focusing can scroll the textarea
  // to reveal the caret, so settle back at the top and let the layers sync
  // before measuring — a scroll event is delivered asynchronously.
  const selected = await page.evaluate((top: string) => {
    const t = document.querySelector<HTMLTextAreaElement>(".md-ta")!;
    const start = t.value.indexOf(top);
    t.focus();
    t.setSelectionRange(start, start + top.length);
    t.scrollTop = 0;
    t.dispatchEvent(new Event("scroll"));
    return t.value.slice(t.selectionStart, t.selectionEnd);
  }, TOP);
  expect(selected).toBe(TOP);
  await expect
    .poll(() => page.evaluate(() => {
      const t = document.querySelector<HTMLTextAreaElement>(".md-ta")!;
      const p = document.querySelector<HTMLPreElement>(".md-hl")!;
      return p.scrollTop - t.scrollTop;
    }))
    .toBe(0);

  // Both layers share one box and one scroll position, so the rows the <pre>
  // paints for that text are the rows the selection highlight covers.
  const geometry = await page.evaluate((top: string) => {
    const t = document.querySelector<HTMLTextAreaElement>(".md-ta")!;
    const p = document.querySelector<HTMLPreElement>(".md-hl")!;
    const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
    let node: Node | null;
    let painted: DOMRect | null = null;
    while ((node = walker.nextNode())) {
      const idx = (node.textContent ?? "").indexOf(top.slice(0, 20));
      if (idx >= 0) {
        const range = document.createRange();
        range.setStart(node, idx);
        range.setEnd(node, idx + 20);
        painted = range.getBoundingClientRect();
        break;
      }
    }
    return {
      taScrollTop: t.scrollTop,
      preScrollTop: p.scrollTop,
      paintedTop: painted ? painted.top : null,
      paneTop: t.getBoundingClientRect().top,
      paneBottom: t.getBoundingClientRect().bottom,
    };
  }, TOP);

  expect(geometry.preScrollTop).toBe(geometry.taScrollTop);
  // The text the user selected is painted inside the visible pane, near its
  // top — not scrolled away below, which is what the 220px cap produced.
  expect(geometry.paintedTop).not.toBeNull();
  expect(geometry.paintedTop!).toBeGreaterThan(geometry.paneTop);
  expect(geometry.paintedTop!).toBeLessThan(geometry.paneTop + 200);

  await page.locator(".file-view").screenshot({
    path: `artifacts/orb-md-selection-${testInfo.project.name || "chromium"}.png`,
  });
});

test("editing still highlights, autosaves and toggles back to preview", async ({ page }) => {
  const { ta, pre, writes } = await openEditor(page);

  // Syntax highlighting survives the layout fix.
  await expect(pre.locator(".md-h").first()).toBeVisible();
  await expect(pre.locator(".md-fence").first()).toBeVisible();

  const edited = DOC.replace(TOP, `${TOP} EDITED`);
  await ta.fill(edited);
  await expect.poll(() => writes.length, { timeout: 5000 }).toBe(1);
  expect(writes[0].content).toBe(edited);
  // Nothing was trimmed: blank lines and the trailing newline round-trip.
  expect(writes[0].content.endsWith("\n")).toBe(true);
  expect(writes[0].content).toContain(BOTTOM);
  await expect(ta).toHaveValue(edited);

  // Layers still agree after the edit.
  const after = await page.evaluate(() => ({
    ta: document.querySelector<HTMLTextAreaElement>(".md-ta")!.scrollHeight,
    pre: document.querySelector<HTMLPreElement>(".md-hl")!.scrollHeight,
  }));
  expect(after.ta).toBe(after.pre);

  await page.keyboard.press("Meta+/");
  await expect(page.locator(".md-ta")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Edit" })).toBeVisible();
});
