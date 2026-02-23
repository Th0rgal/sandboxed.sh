import { test, expect, type Page, type Route } from "@playwright/test";

const MISSION_ID = "910aba49-6e34-457e-bcde-26ad65a456a5";
const AUTOMATION_ID = "a1a1a1a1-1111-4111-8111-111111111111";

async function fulfillJson(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function mockControlApi(page: Page) {
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    const now = new Date().toISOString();

    const mission = {
      id: MISSION_ID,
      title: "Mock mission",
      status: "running",
      workspace_id: "11111111-1111-1111-1111-111111111111",
      backend: "codex",
      created_at: now,
      updated_at: now,
      history: [],
    };

    if (path === "/api/control/missions/current") {
      await fulfillJson(route, mission);
      return;
    }
    if (path === "/api/control/missions") {
      await fulfillJson(route, [mission]);
      return;
    }
    if (path === "/api/control/running") {
      await fulfillJson(route, [{ mission_id: MISSION_ID, state: "running", queue_len: 0 }]);
      return;
    }
    if (path === "/api/control/progress") {
      await fulfillJson(route, { run_state: "running", queue_len: 0, mission_id: MISSION_ID });
      return;
    }
    if (path === "/api/control/stream") {
      await route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
      return;
    }
    if (path === `/api/control/missions/${MISSION_ID}/automations`) {
      await fulfillJson(route, [
        {
          id: AUTOMATION_ID,
          mission_id: MISSION_ID,
          command_source: { type: "inline", content: "Do thing" },
          trigger: { type: "webhook", config: { webhook_id: "wh_1" } },
          variables: {},
          active: true,
          stop_policy: { type: "never" },
          fresh_session: "keep",
          created_at: new Date(Date.now() - 86_400_000).toISOString(),
          last_triggered_at: null,
        },
      ]);
      return;
    }
    if (path === `/api/control/missions/${MISSION_ID}/automation-executions`) {
      await fulfillJson(route, [
        {
          id: "e1e1e1e1-1111-4111-8111-111111111111",
          automation_id: AUTOMATION_ID,
          mission_id: MISSION_ID,
          triggered_at: new Date(Date.now() - 120_000).toISOString(),
          trigger_source: "webhook",
          status: "success",
          variables_used: {},
          completed_at: new Date(Date.now() - 60_000).toISOString(),
          retry_count: 0,
        },
      ]);
      return;
    }
    if (/^\/api\/backends\/[^/]+\/agents$/.test(path)) {
      await fulfillJson(route, []);
      return;
    }
    if (/^\/api\/backends\/[^/]+\/config$/.test(path)) {
      await fulfillJson(route, { hidden_agents: [], default_agent: null });
      return;
    }
    if (path === "/api/backends") {
      await fulfillJson(route, []);
      return;
    }
    if (path.startsWith("/api/library/")) {
      await fulfillJson(route, []);
      return;
    }
    if (path === "/api/providers" || path === "/api/providers/backend-models") {
      await fulfillJson(route, []);
      return;
    }
    if (path === "/api/workspaces" || path === "/api/desktop/sessions") {
      await fulfillJson(route, []);
      return;
    }
    if (path === "/api/health") {
      await fulfillJson(route, { max_iterations: 50 });
      return;
    }

    await fulfillJson(route, {});
  });
}

test.describe("Automation Modal Behavior", () => {
  test("shows recent last run using execution fallback when automation timestamp is missing", async ({
    page,
  }) => {
    await mockControlApi(page);
    await page.goto("/control");

    await page.getByRole("button", { name: /automations/i }).click();
    await expect(page.getByRole("heading", { name: /mission automations/i })).toBeVisible();

    const lastRunLabel = page.getByText(/Last run/i).first();
    await expect(lastRunLabel).toBeVisible();
    await expect(lastRunLabel).not.toContainText(/never/i);
  });

  test("escape closes modal first when thinking panel is open", async ({ page }) => {
    await mockControlApi(page);
    await page.goto("/control");

    await page.getByRole("button", { name: /^Thinking$/i }).click();
    await expect(page.getByText(/No thoughts yet/i)).toBeVisible();

    await page.getByRole("button", { name: /automations/i }).click();
    await expect(page.getByRole("heading", { name: /mission automations/i })).toBeVisible();

    await page.getByRole("heading", { name: /mission automations/i }).click();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("heading", { name: /mission automations/i })).not.toBeVisible();
    await expect(page.getByText(/No thoughts yet/i)).toBeVisible();
  });
});
