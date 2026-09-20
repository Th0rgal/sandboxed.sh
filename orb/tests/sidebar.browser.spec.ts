import { test, expect } from "@playwright/test";

test("375px actual app: sidebar toggle opens, closes and dismisses the drawer", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 800 });
  await page.route("**/api/**", (route) => route.fulfill({ json: {} }));
  await page.goto("/");
  const toggle = page.getByRole("button", { name: "Toggle sidebar", exact: true });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator("#orb-sidebar")).toBeHidden();
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator("#orb-sidebar")).toBeVisible();
  await toggle.click();
  await expect(page.locator("#orb-sidebar")).toBeHidden();
  await toggle.click();
  await page.getByRole("button", { name: "Close sidebar", exact: true }).click({ position: { x: 340, y: 200 } });
  await expect(page.locator("#orb-sidebar")).toBeHidden();
  await toggle.click(); await page.keyboard.press("Escape");
  await expect(page.locator("#orb-sidebar")).toBeHidden();
  await toggle.click();
  await page.screenshot({ path: "test-results/orb-narrow-drawer.png" });
});

test("sidebar rows stay compact with distinct hover/selected and delayed real metadata", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  const missions = Array.from({ length: 36 }, (_, i) => ({
    id: `m${i}`,
    title: i === 0 ? "Orb DGX launch without losing this draft" : i < 4 ? `Live mission ${i}` : `Finished mission ${i} with a long truncated title`,
    status: i < 4 ? "active" : "completed",
    history: [],
    workspace_name: i === 0 ? "host" : null,
    remote_node_id: i === 0 ? "dgx-spark" : null,
    created_at: "",
    updated_at: "",
  }));
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = path === "/api/projects" ? { projects: [{ slug: "test", title: "test" }] }
      : path === "/api/control/missions" && new URL(route.request().url()).searchParams.get("project") === "test" ? missions
      : path.endsWith("/files") ? { entries: [] }
      : path.endsWith("/crons") ? { jobs: [] }
      : path.endsWith("/controller") ? { job: null, runs: [] }
      : [];
    await route.fulfill({ json });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "test", exact: true }).click();
  const live = page.getByRole("button", { name: /Orb DGX launch/ });
  const finished = page.getByRole("button", { name: "32 finished" });
  await expect(live).toBeVisible();
  await expect(finished).toBeVisible();
  expect((await live.boundingBox())!.height).toBe(30);
  expect((await page.getByRole("button", { name: "New Agent" }).boundingBox())!.height).toBe(30);
  const liveBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  await live.hover();
  const hoverBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(hoverBg).not.toBe(liveBg);
  await live.click();
  const selectedBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(selectedBg).not.toBe(hoverBg);
  const newAgent = page.getByRole("button", { name: "New Agent" });
  await newAgent.hover();
  const idleHover = await newAgent.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(idleHover).not.toBe(selectedBg);
  await page.locator("#orb-sidebar").screenshot({ path: "test-results/orb-sidebar-hover-selected.png" });
  await finished.click();
  const done = page.getByRole("button", { name: /Finished mission 5/ });
  await expect(done).toBeVisible();
  const livePad = await live.evaluate((el) => getComputedStyle(el).paddingLeft);
  const donePad = await done.evaluate((el) => getComputedStyle(el).paddingLeft);
  expect(donePad).toBe(livePad);
  const otherLive = page.getByRole("button", { name: "Live mission 1" });
  const doneColor = await done.evaluate((el) => getComputedStyle(el).color);
  const liveColor = await otherLive.evaluate((el) => getComputedStyle(el).color);
  expect(doneColor).toBe(liveColor);
  const dim = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--fg-3").trim());
  expect(doneColor).not.toBe(dim);
  await live.hover();
  await page.waitForTimeout(560);
  await expect(page.locator(".row-tip")).toContainText("Orb DGX launch without losing this draft");
  await expect(page.locator(".row-tip")).toContainText("DGX Spark");
  await expect(page.locator(".row-tip")).not.toContainText("host");
  await page.locator("#orb-sidebar").screenshot({ path: "test-results/orb-sidebar-finished.png" });
  const measure = await page.evaluate(() => {
    const rows = [...document.querySelectorAll("#orb-sidebar .row")];
    const start = performance.now();
    rows.forEach((row) => { (row as HTMLElement).offsetHeight; });
    return { count: rows.length, layoutMs: performance.now() - start, height: rows[0]?.getBoundingClientRect().height };
  });
  expect(measure.count).toBeGreaterThan(30);
  expect(measure.layoutMs).toBeLessThan(40);
  expect(measure.height).toBe(30);
  await page.evaluate(() => { localStorage.setItem("orb-theme", "light"); document.documentElement.dataset.theme = "light"; });
  await page.locator("#orb-sidebar").screenshot({ path: "test-results/orb-sidebar-light.png" });
});

test("project action menu is compact, pointer hover has no focus ring, keyboard still focuses", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = path === "/api/projects" ? { projects: [{ slug: "test", title: "test" }] }
      : path.endsWith("/files") ? { entries: [] }
      : path.endsWith("/missions") ? []
      : path.endsWith("/crons") ? { jobs: [] }
      : path.endsWith("/controller") ? { job: null, runs: [] }
      : {};
    return route.fulfill({ json });
  });
  await page.goto("/");
  const action = page.getByRole("button", { name: "Project actions for test" });
  await action.hover();
  await action.click();
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  const box = await menu.boundingBox();
  expect(box!.width).toBeLessThanOrEqual(220);
  const first = page.getByRole("menuitem", { name: "New folder" });
  await expect(first).not.toBeFocused();
  const outline = await first.evaluate((el) => getComputedStyle(el).outlineStyle);
  expect(outline === "none" || outline === "").toBeTruthy();
  await page.screenshot({ path: "test-results/orb-project-menu.png" });
  await page.keyboard.press("ArrowDown");
  await expect(first).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(action).toBeFocused();
  await action.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem", { name: "New folder" })).toBeFocused();
  await page.evaluate(() => { document.documentElement.dataset.theme = "light"; });
  await page.screenshot({ path: "test-results/orb-project-menu-light.png" });
});
