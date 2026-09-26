import { test, expect } from "@playwright/test";
import fixtures from "./fixtures/hermes-jobs.json" with { type: "json" };

for (const theme of ["light", "dark"]) {
  test(`${theme}: create, keyboard menu, root refresh, shared edit, drafts and schedule fit`, async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    let folderCreated = false;
    let cronCreated = false;
    const requests: { method: string; path: string; body: any }[] = [];
    await page.route("**/api/**", async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const method = request.method();
      const body = request.postDataJSON();
      requests.push({ method, path, body });
      let result: unknown = {};
      if (path === "/api/projects") result = { projects: [{ slug: "notes", title: "Project notes" }] };
      else if (path.endsWith("/file/mkdir")) { folderCreated = true; result = {}; }
      else if (path.endsWith("/files")) result = { entries: folderCreated ? [{ name: "Notes", kind: "dir" }] : [] };
      else if (path.endsWith("/missions")) result = [];
      else if (path.endsWith("/controller")) result = { slug: "notes", job: null, runs: [] };
      else if (path.endsWith("/crons/defaults")) result = { deliver: "project:notes", route_ready: true };
      else if (path.endsWith("/crons")) {
        if (method === "POST") { cronCreated = true; result = { job: fixtures.hourly }; }
        else result = { jobs: cronCreated ? [fixtures.hourly] : [] };
      } else if (path.includes("/crons/")) result = { job: fixtures.hourly };
      await route.fulfill({ json: result });
    });
    await page.goto(`/tests/browser.html?theme=${theme}`);
    const project = page.getByRole("button", { name: "Project notes", exact: true });
    await expect(project).toBeVisible();
    await project.click();
    await expect(page.getByText("No missions or files yet.")).toBeVisible();
    await project.click();
    await project.focus();
    const action = page.getByRole("button", { name: "Project actions for Project notes" });
    await expect(action).toHaveCSS("opacity", "1");
    await action.focus(); await page.keyboard.press("Enter");
    // Agent and cron creation lead; files are only created inside folders.
    await expect(page.getByRole("menuitem", { name: "New agent" })).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(page.getByRole("menuitem", { name: "New cron" })).toBeFocused();
    await page.keyboard.press("Escape"); await expect(action).toBeFocused();
    await action.click(); await page.getByRole("menuitem", { name: "New folder" }).click();
    await page.getByPlaceholder("Folder name").fill("Notes");
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await expect(project).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByRole("button", { name: "Notes", exact: true })).toBeVisible();
    await project.click();
    await action.focus(); await page.keyboard.press("Enter");
    await page.getByRole("menuitem", { name: "New cron" }).click();
    await page.getByLabel("Name", { exact: true }).fill("Project notes");
    await page.keyboard.press("Escape");
    await expect(action).toBeFocused();
    await action.click(); await page.getByRole("menuitem", { name: "New cron" }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Project notes");
    await page.getByLabel("Instruction", { exact: true }).fill(fixtures.hourly.prompt);
    const schedule = page.getByRole("button", { name: "Schedule", exact: true });
    await expect(schedule).toHaveText("Every hour⌄");
    const widths = await Promise.all([schedule, page.getByLabel("Name", { exact: true }), page.getByLabel("Stops after")].map(async (el) => (await el.boundingBox())!.width));
    expect(Math.max(...widths) - Math.min(...widths)).toBeLessThan(1);
    await schedule.click();
    await page.keyboard.press("Shift+Tab");
    await expect(page.getByRole("button", { name: "Done", exact: true })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(page.getByLabel("Schedule type")).toBeFocused();
    await page.getByLabel("Schedule type").selectOption("days");
    await expect(schedule).toContainText("Weekdays at 09:00");
    const panel = page.getByRole("dialog", { name: "Schedule editor" });
    expect((await panel.boundingBox())!.width).toBe(240);
    expect(await panel.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    await page.screenshot({ path: `test-results/cron-create-${theme}.png`, fullPage: true, style: ".harness-controls { visibility: hidden; }" });
    await page.getByLabel("Schedule type").selectOption("once");
    expect(await panel.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    await page.getByLabel("Date and time").focus(); await page.keyboard.press("Escape");
    await expect(schedule).toBeFocused();
    await expect(page.getByRole("dialog", { name: "New cron", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Close", exact: true }).focus();
    await page.keyboard.press("Shift+Tab");
    await expect(page.getByRole("button", { name: "Create", exact: true })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(page.getByRole("button", { name: "Close", exact: true })).toBeFocused();
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await expect(page.getByRole("button", { name: "Settings", exact: true })).toBeVisible();
    await expect(project).toHaveAttribute("aria-expanded", "true");
    expect(requests.find((r) => r.method === "POST" && r.path.endsWith("/crons"))?.body.deliver).toBe("project:notes");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("Draft retained");
    await page.getByRole("button", { name: "Runs", exact: true }).click();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Draft retained");
    await page.getByRole("button", { name: "Toggle view" }).click();
    await page.getByRole("button", { name: "Toggle view" }).click();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Draft retained");
    await page.setViewportSize({ width: 1100, height: 1450 });
    await page.screenshot({ path: `test-results/cron-edit-${theme}.png`, fullPage: true, style: ".harness-controls { visibility: hidden; }" });
    await expect(page.getByRole("link")).toHaveCSS("cursor", "pointer");
    await expect(page.getByLabel("Disabled field")).toHaveCSS("cursor", "default");
    await page.getByText("Advanced", { exact: true }).click();
    await expect(page.getByRole("combobox")).toHaveCSS("cursor", "pointer");
    expect(errors).toEqual([]);
  });
}

test("compact window: days and datetime stay inside the schedule popover", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 800 });
  await page.route("**/api/**", (route) => route.fulfill({ json: { projects: [{ slug: "notes", title: "Project notes" }] } }));
  await page.goto("/tests/browser.html?theme=light");
  await page.getByRole("button", { name: "Project actions for Project notes" }).click();
  await page.getByRole("menuitem", { name: "New cron" }).click();
  await page.getByRole("button", { name: "Schedule", exact: true }).click();
  const panel = page.getByRole("dialog", { name: "Schedule editor" });
  for (const mode of ["days", "once"]) {
    await page.getByLabel("Schedule type").selectOption(mode);
    const box = (await panel.boundingBox())!;
    expect(box.width).toBe(240); expect(box.x).toBeGreaterThanOrEqual(8); expect(box.x + box.width).toBeLessThanOrEqual(367);
    expect(await panel.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  }
  expect(await page.locator(".dlg-wide").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

for (const theme of ["light", "dark"]) {
  test(`${theme}: native snapshot defaults remain unpinned`, async ({ page }) => {
    await page.route("**/api/**", (route) => {
      const path = new URL(route.request().url()).pathname;
      const json = path === "/api/projects" ? { projects: [{ slug: "notes", title: "Project notes" }] }
        : path.endsWith("/missions") ? []
        : path.endsWith("/files") ? { entries: [] }
        : path.endsWith("/controller") ? { slug: "notes", job: null, runs: [] }
        : path.endsWith("/crons") ? { jobs: [fixtures.snapshot] }
        : { job: fixtures.snapshot };
      return route.fulfill({ json });
    });
    await page.goto(`/tests/browser.html?theme=${theme}`);
    await page.getByRole("button", { name: "Project notes", exact: true }).click();
    await page.getByRole("button", { name: /Saved local defaults/ }).click();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByText("Advanced", { exact: true }).click();
    await expect(page.getByText("Saved default: fixture-local-model")).toBeVisible();
    await expect(page.getByText("Saved default: custom")).toBeVisible();
    await expect(page.getByLabel("Model", { exact: true })).toHaveValue("");
    await expect(page.getByLabel("Provider", { exact: true })).toHaveValue("");
    await expect(page.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
    await page.setViewportSize({ width: 1100, height: 1450 });
    await page.screenshot({ path: `test-results/cron-snapshots-${theme}.png`, fullPage: true, style: ".harness-controls { visibility: hidden; }" });
  });
}

test("cron outage retains cached rows; Retry restores availability without logging out", async ({ page }) => {
  let fail = false;
  await page.route("**/api/**", (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/crons") && fail) return route.fulfill({ status: 502, body: "Hermes scheduler returned 401: Unauthorized" });
    const json = path === "/api/projects" ? { projects: [{ slug: "notes", title: "Project notes" }] }
      : path.endsWith("/crons") ? { jobs: [fixtures.hourly] }
      : path.endsWith("/missions") ? [] : path.endsWith("/files") ? { entries: [] }
      : { slug: "notes", job: null, runs: [] };
    return route.fulfill({ json });
  });
  await page.goto("/tests/browser.html?theme=dark");
  const folder = page.getByRole("button", { name: "Project notes", exact: true });
  await folder.click();
  await expect(page.locator(".row.cron")).toHaveCount(1);
  fail = true;
  await folder.click(); await folder.click();
  await expect(page.getByRole("status")).toContainText("Crons temporarily unavailable");
  await expect(page.getByRole("status")).toHaveAttribute("title", /Cached jobs are retained/);
  await expect(page.locator(".row.cron")).toHaveCount(1);
  expect(await page.evaluate(() => localStorage.getItem("orb.jwt"))).toBe("local-browser-test");
  fail = false; await page.getByRole("button", { name: "Retry crons" }).click();
  await expect(page.getByRole("status")).toHaveCount(0);
  await expect(page.locator(".row.cron")).toHaveCount(1);
});

test("creation defaults to the project conversation; missing route requires explicit local choice", async ({ page }) => {
  await page.route("**/api/**", (route) => route.fulfill({ json: new URL(route.request().url()).pathname.endsWith("/crons/defaults") ? { deliver: "project:notes", route_ready: false } : { projects: [{ slug: "notes", title: "Project notes" }] } }));
  await page.goto("/tests/browser.html?theme=light");
  await page.getByRole("button", { name: "Project actions for Project notes" }).click();
  await page.getByRole("menuitem", { name: "New cron" }).click();
  await expect(page.getByLabel("Delivery", { exact: true })).toHaveValue("project:notes");
  await expect(page.getByText(/No delivery route is bound yet/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Create", exact: true })).toBeDisabled();
  await page.getByLabel("Delivery", { exact: true }).fill("local");
  await expect(page.getByText(/no conversation copy/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Create", exact: true })).toBeEnabled();
});

for (const status of [404, 500]) test(`cron ${status}: compact status preserves content and capability polling is bounded`, async ({page}) => {
 let fail=false, requests=0;
 await page.clock.install();
 await page.route("**/api/**",route=>{
  const path=new URL(route.request().url()).pathname;
  if(path.endsWith("/crons")){requests++;if(fail)return route.fulfill({status,body:"upstream detail"});return route.fulfill({json:{jobs:[fixtures.hourly]}});}
  const json=path==="/api/projects"?{projects:[{slug:"notes",title:"Project notes"}]}:path.endsWith("/crons/defaults")?{deliver:"project:notes",route_ready:true}:path.endsWith("/missions")?[]:path.endsWith("/files")?{entries:[]}: {slug:"notes",job:fixtures.weekdays,runs:[]};
  return route.fulfill({json});
 });
 await page.goto("/tests/browser.html?theme=dark");
 const project=page.getByRole("button",{name:"Project notes",exact:true});await project.click();await expect(page.locator(".row.cron")).toHaveCount(2);
 fail=true;await project.click();await project.click();
 const row=page.getByRole("status");await expect(row).toContainText(status===404?"Crons need backend update":"Crons temporarily unavailable");
 await expect(row).not.toContainText(String(status));expect((await row.boundingBox())!.height).toBeLessThanOrEqual(32);
 await expect(page.locator(".row.cron")).toHaveCount(2);
 await page.bringToFront();
 expect(await page.evaluate(() => document.visibilityState)).toBe("visible");
 const failedCount=requests;await page.clock.fastForward(31000);
 if(status===404){
  expect(requests).toBe(failedCount);await expect(page.getByRole("button",{name:"Retry crons"})).toHaveCount(0);
  await page.getByRole("button",{name:"Project actions for Project notes"}).click();await page.getByRole("menuitem",{name:"New cron",exact:true}).click();
  await expect(page.getByRole("dialog",{name:"Project crons"})).toBeVisible();await expect(page.getByRole("dialog",{name:"New cron"})).toHaveCount(0);
  await expect(page.getByText(/Update the connected backend/)).toBeVisible();
  await page.getByRole("button",{name:"Close",exact:true}).click();
  await page.screenshot({path:"test-results/orb-cron-unsupported.png"});
  fail=false;await page.getByRole("button",{name:"Reconnect backend"}).click();await expect(row).toHaveCount(0);expect(requests).toBeGreaterThan(failedCount);
 } else {await expect.poll(()=>requests).toBeGreaterThan(failedCount);fail=false;await page.getByRole("button",{name:"Retry crons"}).click();await expect(row).toHaveCount(0);}
});

for (const disconnect of ["401", "logout"]) test(`cron ${disconnect} stops fetching until reconnect`, async ({ page }) => {
  let unauthorized = false, requests = 0;
  await page.clock.install();
  await page.route("**/api/**", route => {
    requests++;
    const path = new URL(route.request().url()).pathname;
    if (unauthorized) return route.fulfill({ status: 401 });
    return route.fulfill({ json: path === "/api/projects" ? { projects: [{ slug: "notes", title: "Project notes" }] } : path.endsWith("/crons") ? { jobs: [fixtures.hourly] } : path.endsWith("/missions") ? [] : path.endsWith("/files") ? { entries: [] } : { slug: "notes", job: fixtures.weekdays, runs: [] } });
  });
  await page.goto("/tests/browser.html");
  await page.getByRole("button", { name: "Project notes", exact: true }).click();
  await expect(page.locator(".row.cron")).toHaveCount(2);
  if (disconnect === "logout") await page.getByRole("button", { name: "Disconnect backend" }).click();
  else {
    unauthorized = true;
    await page.clock.fastForward(11000);
    await expect.poll(() => page.evaluate(() => localStorage.getItem("orb.jwt"))).toBeNull();
  }
  // Let all requests from the same polling tick settle before checking silence.
  await page.waitForLoadState("networkidle");
  const stopped = requests;
  await page.bringToFront();
  expect(await page.evaluate(() => document.visibilityState)).toBe("visible");
  await page.clock.fastForward(61000);
  await page.waitForLoadState("networkidle");
  expect(requests).toBe(stopped);
  unauthorized = false;
  await page.getByRole("button", { name: "Reconnect backend" }).click();
  await expect.poll(() => requests).toBeGreaterThan(stopped);
  await expect(page.locator(".row.cron")).toHaveCount(2);
});

for (const staleStatus of [200, 401, 404, 500]) test(`old connection cron ${staleStatus} response cannot change reconnected cache`, async ({ page }) => {
  let cronRequests = 0;
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/**", async route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/crons")) {
      cronRequests++;
      if (cronRequests === 1) {
        await held;
        return route.fulfill({ status: staleStatus, json: staleStatus === 200 ? { jobs: [] } : { error: "Old backend response" } });
      }
      return route.fulfill({ json: { jobs: [fixtures.hourly] } });
    }
    return route.fulfill({ json: path === "/api/projects" ? { projects: [{ slug: "notes", title: "Project notes" }] } : path.endsWith("/missions") ? [] : path.endsWith("/files") ? { entries: [] } : { slug: "notes", job: fixtures.weekdays, runs: [] } });
  });
  await page.goto("/tests/browser.html");
  await page.getByRole("button", { name: "Project notes", exact: true }).click();
  await expect.poll(() => cronRequests).toBe(1);
  await page.getByRole("button", { name: "Reconnect backend" }).click();
  await expect(page.locator(".row.cron")).toHaveCount(2);
  release();
  await page.waitForLoadState("networkidle");
  await expect(page.locator(".row.cron")).toHaveCount(2);
  await expect(page.getByRole("status")).toHaveCount(0);
  expect(await page.evaluate(() => localStorage.getItem("orb.jwt"))).toBe("local-browser-test");
});

test("primary controller steers the next tick and Run now hits controller/action", async ({ page }) => {
  const requests: { method: string; path: string; body: any }[] = [];
  let pending: { id: string; body: string; created_at: string; origin: string }[] = [];
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    const body = request.postDataJSON();
    requests.push({ method, path, body });
    if (path === "/api/projects") return route.fulfill({ json: { projects: [{ slug: "notes", title: "Project notes" }] } });
    if (path.endsWith("/steers")) {
      if (method === "POST") {
        pending = [{ id: "s1", body: body.body, created_at: "2026-09-21T11:00:00Z", origin: "orb" }];
      }
      return route.fulfill({ json: { pending, recent: [] } });
    }
    if (path.endsWith("/controller/action")) return route.fulfill({ json: { slug: "notes", job: fixtures.hourly, runs: [] } });
    if (path.endsWith("/controller")) return route.fulfill({ json: { slug: "notes", job: fixtures.hourly, runs: [] } });
    if (path.endsWith("/missions")) return route.fulfill({ json: [] });
    if (path.endsWith("/files")) return route.fulfill({ json: { entries: [] } });
    if (path.endsWith("/crons")) return route.fulfill({ json: { jobs: [] } });
    return route.fulfill({ json: {} });
  });
  await page.goto("/tests/browser.html?theme=light");
  await page.getByRole("button", { name: "Project notes", exact: true }).click();
  await page.locator(".row.cron").first().click();
  const input = page.getByPlaceholder("Steer the next tick…");
  await expect(input).toBeVisible();
  const timing = page.locator(".steer-composer").getByRole("button", { name: "Run now", exact: true, pressed: true });
  await expect(timing).toHaveText("Now");
  await timing.click();
  await expect(page.locator(".steer-composer").getByRole("button", { name: "Run now", exact: true, pressed: false })).toHaveText("Next tick");
  await page.locator(".steer-composer").getByRole("button", { name: "Run now", exact: true, pressed: false }).click();
  await expect(page.locator(".steer-options")).toHaveCount(0);
  expect((await page.locator(".steer-composer").boundingBox())!.height).toBeLessThan(65);
  await expect(page.locator(".steer-composer .send svg")).toBeVisible();
  await input.fill("review the open PRs");
  await page.getByRole("button", { name: "Steer", exact: true }).click();
  await expect.poll(() => requests.some((r) => r.method === "POST" && r.path.endsWith("/steers"))).toBe(true);
  const steer = requests.find((r) => r.method === "POST" && r.path.endsWith("/steers"));
  expect(steer?.body).toMatchObject({ body: "review the open PRs", origin: "orb" });
  await expect.poll(() => requests.some((r) => r.method === "POST" && r.path.endsWith("/controller/action"))).toBe(true);
  const action = requests.find((r) => r.method === "POST" && r.path.endsWith("/controller/action"));
  expect(action?.body).toMatchObject({ action: "run" });
  await expect(page.locator(".steer-chip-kind")).toHaveText("Run requested");
  await expect(page.getByText("review the open PRs")).toBeVisible();
  await page.screenshot({path:"test-results/orb-steer-light.png", fullPage:true});
  await page.evaluate(() => document.documentElement.dataset.theme = "dark");
  await page.screenshot({path:"test-results/orb-steer-dark.png", fullPage:true});
});
