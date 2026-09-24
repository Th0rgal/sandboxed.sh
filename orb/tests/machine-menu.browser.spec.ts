import { test, expect, type Page } from "@playwright/test";

/**
 * The machine picker's two-line entries (name + status/host caption) used to be
 * squeezed into a fixed 28px `.menu-item`, so the caption rendered on top of the
 * name. These tests measure the real boxes rather than eyeballing a screenshot.
 */
const nodes = [
  { id: "dgx-spark", status: "online", cordoned: false },
  { id: "paloma-frankfurt-01", status: "degraded", cordoned: true },
  { id: "hermes-worker-eu-west-1b", status: "offline", cordoned: false },
];

async function setup(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json =
      path === "/api/remote-nodes"
        ? { enabled: true, nodes, remote_launch: { typed: true, harnesses: ["claudecode", "codex"], proxy_url_configured: true } }
        : path === "/api/projects"
          ? { projects: [{ slug: "test", title: "Test" }] }
          : path === "/api/backends"
            ? [{ id: "claudecode", name: "Claude Code" }, { id: "codex", name: "Codex" }]
            : path === "/api/providers/backend-models"
              ? { backends: { claudecode: [{ value: "claude-opus-5", label: "Anthropic — Claude Opus 5" }], codex: [{ value: "gpt-6-astra", label: "OpenAI — GPT-6 Astra" }] } }
              : path === "/api/control/missions"
                ? []
                : path.endsWith("/files")
                  ? { entries: [] }
                  : path.endsWith("/crons")
                    ? { jobs: [] }
                    : { job: null, runs: [] };
    await route.fulfill({ json });
  });
  await page.goto("/");
  await expect(page.getByRole("button", { name: /Core \(agent-core\)/ })).toBeVisible();
}

/** Every two-line entry: caption strictly below the title, both inside the row. */
async function expectNoOverlap(page: Page) {
  await page.locator(".na-menu").evaluate(async el => { await Promise.all(el.getAnimations().map(animation => animation.finished)); });
  const items = page.locator(".na-menu .menu-item:has(.menu-col)");
  const count = await items.count();
  expect(count).toBeGreaterThan(3); // core + the three nodes
  for (let i = 0; i < count; i++) {
    const item = items.nth(i);
    const row = (await item.boundingBox())!;
    const title = (await item.locator(".menu-title").boundingBox())!;
    expect(title.y).toBeGreaterThanOrEqual(row.y);
    expect(title.y + title.height).toBeLessThanOrEqual(row.y + row.height + 0.5);
    const caption = item.locator(".menu-sub");
    if (await caption.count()) {
      const sub = (await caption.boundingBox())!;
      expect(sub.y).toBeGreaterThanOrEqual(title.y + title.height - 0.5);
      expect(sub.y + sub.height).toBeLessThanOrEqual(row.y + row.height + 0.5);
    }
    const state = item.locator(".machine-node-state");
    if (await state.count()) {
      const box = (await state.boundingBox())!;
      expect(title.x + title.width).toBeLessThanOrEqual(box.x + 0.5);
      expect(box.x + box.width).toBeLessThanOrEqual(row.x + row.width + 0.5);
    }
    if (i + 1 < count) {
      const next = (await items.nth(i + 1).boundingBox())!;
      expect(row.y + row.height).toBeLessThanOrEqual(next.y + 0.5);
    }
  }
}

test("machine picker: two-line entries never overlap, and the footer stays reachable", async ({ page }) => {
  await setup(page);
  await page.getByRole("button", { name: /Core \(agent-core\)/ }).click();
  const menu = page.locator(".na-menu");
  await expect(menu).toBeVisible();

  await expectNoOverlap(page);

  // Single-line entries keep the 32px rhythm. Measured loosely: a
  // bounding box is reported in device pixels, so a row that is exactly 32 CSS
  // pixels can come back as 31.99993896484375 depending on the display scale.
  const manage = page.getByRole("button", { name: "Manage machines" });
  expect((await manage.boundingBox())!.height).toBeCloseTo(32, 2);

  // Footer is outside the scroll area, so it is visible without scrolling.
  await expect(manage).toBeInViewport();
  const scroller = menu.locator(".na-menu-list");
  await expect(scroller).toHaveCSS("overflow-y", "auto");

  // Selection still works and closes the menu. The trigger then shows the
  // node's display label ("dgx-spark" → "DGX Spark").
  await page.getByRole("button", { name: /dgx-spark/ }).click();
  await expect(menu).toBeHidden();
  const trigger = page.getByRole("button", { name: /DGX Spark/ });
  await expect(trigger).toBeVisible();

  await trigger.click();
  await expect(page.locator(".na-menu .menu-item.on")).toHaveCount(1);
  await expect(page.locator(".na-menu .menu-item.on .menu-title")).toHaveText("dgx-spark");
  await page.locator(".na-menu").screenshot({ path: "artifacts/orb-machine-menu-wide.png" });
});

test("machine picker at 375px: wraps inside the viewport with no overlap", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 800 });
  await setup(page);
  await page.getByRole("button", { name: /Core \(agent-core\)/ }).click();
  const menu = page.locator(".na-menu");
  await expect(menu).toBeVisible();

  await expectNoOverlap(page);

  const box = (await menu.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(375);

  await expect(page.getByRole("button", { name: "Manage machines" })).toBeInViewport();
  await page.screenshot({ path: "artifacts/orb-machine-menu-narrow.png" });
});
