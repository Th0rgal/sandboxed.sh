import { expect, test } from "@playwright/test";

const missionControl = {
  generated_at: "2026-07-10T10:00:00Z",
  runtime: {
    service_name: "hermes-assistant",
    service_active: true,
    model: "builtin/smart",
    base_url: "http://localhost:3000/v1",
    expected_base_url: "http://localhost:3000/v1",
    uses_sandboxed_proxy: true,
    env_present: true,
    config_present: true,
    token_present: true,
    notes: [],
  },
  sessions: {
    since: "2026-07-07T10:00:00Z",
    total: 8,
    by_source: { api_server: 5, telegram: 3 },
    messages: 42,
    tool_calls: 12,
    open: 2,
  },
  active: [
    {
      id: "11111111-1111-4111-8111-111111111111",
      title: "Review sandbox isolation",
      status: "active",
      workspace_name: "sandboxed-sh",
      backend: "codex",
      model_override: null,
      model_effort: "high",
      terminal_reason: null,
      updated_at: "2026-07-10T09:58:00Z",
      last_agent_event_at: "2026-07-10T09:58:00Z",
      last_activity_at: "2026-07-10T09:58:00Z",
      attention: null,
    },
  ],
  needs_attention: [],
  handled_recently: [],
  failures: [],
  mission_status_counts: { active: 1 },
  remote_nodes: {
    enabled: false,
    configured_nodes: 0,
    status: "disabled",
    notes: [],
  },
};

test.beforeEach(async ({ page }) => {
  await page.route("**/api/health", (route) =>
    route.fulfill({ json: { auth_required: false, version: "test" } }),
  );
  await page.route("**/api/system/hermes-assistant/status", (route) =>
    route.fulfill({
      json: {
        service_name: "hermes-assistant",
        service_active: true,
        model: "builtin/smart",
        env_path: "",
        config_path: "",
        env_present: true,
        config_present: true,
        token_present: true,
        telegram_ok: true,
        telegram_bot_username: "hermes",
        telegram_webhook_configured: false,
        telegram_pending_update_count: 0,
        telegram_last_error: null,
        notes: [],
      },
    }),
  );
  await page.route("**/api/system/hermes-assistant/mission-control", (route) =>
    route.fulfill({ json: missionControl }),
  );
  await page.route("**/api/assistant/hermes/api/sessions**", (route) =>
    route.fulfill({ json: { data: [] } }),
  );
  await page.route("**/api/control/alerts**", (route) =>
    route.fulfill({ json: { alerts: [], next_cursor: null } }),
  );
});

test("keeps Hermes chat primary and mission control closed", async ({ page }, testInfo) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Hermes" })).toBeVisible();
  await expect(page.getByText("What should Hermes handle?")).toBeVisible();
  await expect(page.getByRole("dialog", { name: "Mission control" })).toHaveCount(0);

  const viewport = page.viewportSize();
  const thread = await page.getByPlaceholder("Message Hermes…").boundingBox();
  expect(viewport).not.toBeNull();
  expect(thread).not.toBeNull();
  expect(thread!.width).toBeGreaterThan(500);
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
  ).toBe(true);

  if (process.env.CAPTURE_HERMES_UI) {
    await page.screenshot({ path: testInfo.outputPath("hermes-default.png"), fullPage: true });
  }

  await page.getByRole("button", { name: "Missions" }).click();
  const drawer = page.getByRole("dialog", { name: "Mission control" });
  await expect(drawer).toBeVisible();
  expect((await drawer.boundingBox())!.width).toBeLessThanOrEqual(580);

  await page.getByRole("button", { name: "Missions" }).click();
  await expect(drawer).toHaveCount(0);

  await page.getByRole("button", { name: "Missions" }).click();
  await expect(drawer).toBeVisible();

  if (process.env.CAPTURE_HERMES_UI) {
    await page.screenshot({ path: testInfo.outputPath("hermes-desktop.png"), fullPage: true });
  }
});

test("uses drawers without overflow on a compact viewport", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto("/");

  await expect(page.getByText("What should Hermes handle?")).toBeVisible();
  await expect(page.getByRole("button", { name: "Open mission updates" })).toBeVisible();
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
  ).toBe(true);

  await page.getByRole("button", { name: "Open mission updates" }).click();
  await expect(page.getByRole("dialog", { name: "Mission updates" })).toBeVisible();
});
