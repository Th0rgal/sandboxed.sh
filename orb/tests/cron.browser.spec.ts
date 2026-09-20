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
    await expect(page.getByRole("menuitem", { name: "New folder" })).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(page.getByRole("menuitem", { name: "New agent" })).toBeFocused();
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
    await action.click(); await page.getByRole("menuitem", { name: "New cron" }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("Project notes");
    await page.getByLabel("Instruction", { exact: true }).fill(fixtures.hourly.prompt);
    const schedule = page.getByRole("button", { name: "Schedule", exact: true });
    await expect(schedule).toHaveText("Every hour⌄");
    const widths = await Promise.all([schedule, page.getByLabel("Name", { exact: true }), page.getByLabel("Stops after")].map(async (el) => (await el.boundingBox())!.width));
    expect(Math.max(...widths) - Math.min(...widths)).toBeLessThan(1);
    await schedule.click();
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
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await expect(page.getByRole("button", { name: "Settings", exact: true })).toBeVisible();
    await expect(project).toHaveAttribute("aria-expanded", "true");
    expect(requests.some((r) => r.method === "POST" && r.path.endsWith("/crons"))).toBe(true);
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
