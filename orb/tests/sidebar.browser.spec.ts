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
  const archived = page.getByRole("button", { name: "Archived", exact: true });
  await expect(live).toBeVisible();
  await expect(archived).toBeVisible();
  expect((await live.boundingBox())!.height).toBe(30);
  expect((await page.getByRole("button", { name: "New Agent" }).boundingBox())!.height).toBe(30);
  const liveBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  await live.hover();
  await expect.poll(() => live.evaluate(el => getComputedStyle(el).backgroundColor)).not.toBe(liveBg);
  await page.waitForTimeout(200);
  const hoverBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(hoverBg).not.toBe(liveBg);
  await live.click();
  await expect.poll(() => live.evaluate(el => getComputedStyle(el).backgroundColor)).not.toBe(hoverBg);
  await page.waitForTimeout(200);
  const selectedBg = await live.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(selectedBg).not.toBe(hoverBg);
  const newAgent = page.getByRole("button", { name: "New Agent" });
  await newAgent.hover();
  const idleHover = await newAgent.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(idleHover).not.toBe(selectedBg);
  await page.locator("#orb-sidebar").screenshot({ path: "test-results/orb-sidebar-hover-selected.png" });
  await expect(archived).toHaveAttribute("aria-expanded", "false");
  const done = page.getByRole("button", { name: /Finished mission 5/ });
  await expect(done).toBeVisible();
  const livePad = await live.evaluate((el) => getComputedStyle(el).paddingLeft);
  const donePad = await done.evaluate((el) => getComputedStyle(el).paddingLeft);
  expect(parseFloat(donePad)).toBe(parseFloat(livePad));
  const otherLive = page.getByRole("button", { name: "Live mission 1" });
  const doneColor = await done.evaluate((el) => getComputedStyle(el).color);
  const liveColor = await otherLive.evaluate((el) => getComputedStyle(el).color);
  expect(doneColor).toBe(liveColor);
  const dim = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--fg-3").trim());
  expect(doneColor).not.toBe(dim);
  expect(await live.getAttribute("title")).toBeNull();
  await live.hover();
  const tip = page.locator(".row-tip");
  await expect(tip).toBeHidden();
  await page.waitForTimeout(560);
  await expect(tip).toBeVisible();
  await expect(tip.locator(".row-tip-title")).toHaveText("Orb DGX launch without losing this draft");
  await expect(tip.locator(".row-tip-meta").first()).toHaveText("DGX Spark");
  expect(await tip.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe("rgb(25, 25, 25)");
  await expect(tip).not.toContainText("host");
  await expect(live).toHaveAttribute("aria-describedby", "orb-row-tip");
  await expect(live).toHaveAccessibleName(/Orb DGX launch without losing this draft/);
  const liveBox = (await live.boundingBox())!;
  const tipBox = (await tip.boundingBox())!;
  expect(tipBox.x).toBeGreaterThan(liveBox.x);
  expect(tipBox.x).toBeLessThan(liveBox.x + liveBox.width);
  await page.screenshot({ path: "test-results/orb-sidebar-tooltip.png" });
  await page.keyboard.press("Escape");
  await expect(tip).toBeHidden();
  await expect(live).not.toHaveAttribute("aria-describedby");
  await page.locator(".titlebar").click();
  await live.hover();
  await expect(tip).toBeHidden();
  await page.waitForTimeout(560);
  await expect(tip).toBeVisible();
  await live.click();
  await expect(tip).toBeHidden();
  await page.waitForTimeout(560);
  await expect(tip).toBeHidden();
  await expect(live).not.toHaveAttribute("aria-describedby");
  await page.locator(".titlebar").click();
  for (const dismiss of ["escape", "scroll", "pointer", "resize"] as const) {
    await live.hover();
    await expect(tip).toBeHidden();
    if (dismiss === "escape") await page.keyboard.press("Escape");
    else if (dismiss === "scroll") await page.locator(".sb-scroll").evaluate((el) => { el.scrollTop += 20; });
    else if (dismiss === "pointer") await live.click();
    else await page.evaluate(() => window.dispatchEvent(new Event("resize")));
    await page.waitForTimeout(560);
    await expect(tip).toBeHidden();
    await page.locator(".titlebar").click();
  }
  await live.focus();
  await page.keyboard.press("ArrowDown");
  await expect(otherLive).toBeFocused();
  await expect(tip).toBeHidden();
  await page.waitForTimeout(560);
  await expect(tip).toBeVisible();
  await expect(tip.locator(".row-tip-title")).toHaveText("Live mission 1");
  await expect(tip.locator(".row-tip-meta")).toHaveText(["m1", "Running"]);
  await page.locator(".sb-scroll").evaluate((el) => { el.scrollTop += 40; });
  await expect(tip).toBeHidden();
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
  await expect.poll(() => menu.evaluate((el) => Number(getComputedStyle(el).opacity))).toBe(1);
  await page.waitForTimeout(120);
  expect(await menu.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe("rgb(25, 25, 25)");
  expect(await menu.evaluate((el) => getComputedStyle(el).backdropFilter === "none" || !getComputedStyle(el).backdropFilter)).toBeTruthy();
  const box = await menu.boundingBox();
  expect(box!.width).toBeLessThanOrEqual(220);
  // Agent and cron creation lead the project menu; files require a folder.
  const first = page.getByRole("menuitem", { name: "New agent" });
  await expect(page.getByRole("menuitem", { name: "New file" })).toHaveCount(0);
  await expect(page.getByRole("menuitem", { name: "Rename" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Archive" })).toBeVisible();
  await expect(first).not.toBeFocused();
  const outline = await first.evaluate((el) => getComputedStyle(el).outlineStyle);
  expect(outline === "none" || outline === "").toBeTruthy();
  await page.screenshot({ path: "artifacts/orb-project-menu.png" });
  await page.keyboard.press("ArrowDown");
  await expect(first).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(action).toBeFocused();
  await action.focus();
  await page.keyboard.press("Enter");
  await expect(first).toBeFocused();
  await page.evaluate(() => { document.documentElement.dataset.theme = "light"; });
  await expect.poll(() => menu.evaluate((el) => Number(getComputedStyle(el).opacity))).toBe(1);
  expect(await menu.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe("rgb(255, 255, 255)");
  await page.screenshot({ path: "test-results/orb-project-menu-light.png" });
});

test("project folders contain agents and crons and offer scoped creation", async ({page}) => {
  await page.addInitScript(() => { localStorage.setItem("orb.apiUrl", location.origin); localStorage.setItem("orb.jwt", "test"); });
  await page.route("**/api/**", async route => {
    const url=new URL(route.request().url()), path=url.pathname;
    const json=path==="/api/projects" ? {projects:[{slug:"test",title:"Test"}]}
      :path==="/api/control/missions" && url.searchParams.has("project") ? [{id:"nested",title:"Audit agent",status:"active",tags:["orb-folder:audit"],history:[],created_at:"",updated_at:""}]
      :path.endsWith("/files") ? {entries:url.searchParams.get("path") ? [] : [{name:"audit",kind:"dir"}]}
      :path.endsWith("/crons") ? {jobs:[{id:"scheduled",name:"Audit cron",folder:"audit",enabled:true,schedule:"every 1h"}]}
      :path.endsWith("/controller") ? {job:null,runs:[]}:[];
    await route.fulfill({json});
  });
  await page.goto("/");await page.getByRole("button",{name:"Test",exact:true}).click();
  await expect(page.getByRole("button",{name:"Audit agent",exact:true})).toHaveCount(0);
  await page.getByRole("button",{name:"audit",exact:true}).click();
  await expect(page.getByRole("button",{name:"Audit agent",exact:true})).toBeVisible();
  await expect(page.getByRole("button",{name:/Audit cron/})).toBeVisible();
  await page.getByRole("button",{name:"Folder actions for audit"}).click();
  const labels=await page.getByRole("menuitem").allTextContents();
  expect(labels.slice(0,2)).toEqual(["New agent","New cron"]);
  await expect(page.getByRole("menuitem",{name:"New file",exact:true})).toBeVisible();
  await page.getByRole("menuitem",{name:"New agent",exact:true}).click();
  await expect(page.getByRole("button",{name:"Choose project"})).toContainText("/ audit");
});
