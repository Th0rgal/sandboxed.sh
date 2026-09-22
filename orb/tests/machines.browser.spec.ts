import { test, expect } from "@playwright/test";

test("machine details use live core metrics and heartbeat capacity without inventing missing load", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", route => {
    const path = new URL(route.request().url()).pathname;
    const json = path === "/api/remote-nodes" ? { nodes: [{ id: "spark", status: "online", labels: [], base_url: "http://spark:3088", cpu_total: 20, mem_total_bytes: 128 * 1024 ** 3, mem_available_bytes: 32 * 1024 ** 3, disk_total_bytes: 1024 ** 4, disk_available_bytes: 512 * 1024 ** 3, active_jobs: 2 }] }
      : path === "/api/projects" ? { projects: [] }
      : path === "/api/backends" || path === "/api/control/missions" ? []
      : path === "/api/providers/backend-models" ? { backends: {} } : {};
    return route.fulfill({ json });
  });
  await page.routeWebSocket("**/api/monitoring/ws", socket => {
    socket.send(JSON.stringify({ cpu_percent: 23, memory_used: 16 * 1024 ** 3, memory_total: 64 * 1024 ** 3, disk_used: 100 * 1024 ** 3, disk_total: 200 * 1024 ** 3, timestamp_ms: Date.now() }));
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Machines", exact: true }).click();
  const core = page.getByRole("button", { name: /Core Live/ });
  await core.click();
  await expect(core).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByText("23%", { exact: true })).toBeVisible();
  await expect(page.getByText("16.0 GiB / 64.0 GiB")).toBeVisible();
  const spark = page.getByRole("button", { name: /spark online/ });
  await spark.click();
  await expect(page.getByText("20 cores", { exact: true })).toBeVisible();
  await expect(page.getByText("96.0 GiB / 128.0 GiB")).toBeVisible();
  await expect(page.getByText(/CPU load and GPU metrics are not reported/)).toBeVisible();
  await page.screenshot({ path: "/tmp/orb-machines-details.png" });
  await spark.click();
  await expect(page.getByText("96.0 GiB / 128.0 GiB")).not.toBeVisible();
});

test("collapsed providers show right-aligned used percentages and local monochrome marks", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", route => {
    const path = new URL(route.request().url()).pathname;
    const json = path === "/api/ai/providers" ? [{ id: "anthropic", name: "Anthropic", provider_type: "anthropic", uses_oauth: true, status: { type: "connected" }, account_email: "example@example.com" }]
      : path === "/api/ai/providers/usage" ? { entries: { anthropic: { unified_5h_utilization: 0, unified_7d_utilization: 0.63 } } }
      : path === "/api/projects" ? { projects: [] }
      : path === "/api/remote-nodes" ? { nodes: [] }
      : path === "/api/backends" || path === "/api/control/missions" ? [] : {};
    return route.fulfill({ json });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Providers", exact: true }).click();
  const row = page.locator(".p-acc-btn");
  await expect(row.getByText("0%", { exact: true })).toBeVisible();
  await expect(row.getByText("63%", { exact: true })).toBeVisible();
  await expect(row.locator(".p-bar")).toHaveCount(0);
  const text = await row.locator(".s-row-text").boundingBox();
  const usage = await row.locator(".p-usage").boundingBox();
  expect(usage!.x).toBeGreaterThanOrEqual(text!.x + text!.width);
  await expect(row.locator(".provider-logo > span")).toHaveCSS("mask-image", /anthropic.svg/);
  await page.screenshot({ path: "/tmp/orb-provider-summary.png" });
  await row.click();
  await expect(row).toHaveAttribute("aria-expanded", "true");
});
