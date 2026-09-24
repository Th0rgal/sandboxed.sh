import { test, expect, type Page } from "@playwright/test";

const MISSION = "9f2a91c4-8b7d-4e21-9a0c-5d6e7f801234";

type Options = { status?: string; events?: unknown[]; remoteJob?: Record<string, unknown>; hold?: boolean };

async function setup(page: Page, o: Options = {}) {
  let released = !o.hold;
  const mission = {
    id: MISSION,
    title: "Pareto audit",
    status: o.status ?? "active",
    history: [],
    backend: "codex",
    created_at: "",
    updated_at: "",
    ...(o.remoteJob ? { remote_job: { job_id: "j1", node_id: "dgx-spark", ...o.remoteJob } } : {}),
  };
  await page.addInitScript(() => {
    localStorage.setItem("orb.apiUrl", location.origin);
    localStorage.setItem("orb.jwt", "test");
    localStorage.setItem("orb-theme", "dark");
  });
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/events")) {
      while (!released) await new Promise((r) => setTimeout(r, 50));
      return route.fulfill({ json: o.events ?? [] });
    }
    if (path === "/api/control/stream") return route.fulfill({ contentType: "text/event-stream", body: "" });
    if (path.endsWith("/queue")) return route.fulfill({ json: [] });
    if (path.startsWith("/api/control/missions/")) return route.fulfill({ json: mission });
    const json = path === "/api/projects" ? { projects: [{ slug: "test", title: "Test" }] }
      : path === "/api/control/missions" ? [mission]
      : path === "/api/backends" ? [{ id: "codex", name: "Codex" }]
      : path === "/api/providers/backend-models" ? { backends: { codex: [{ value: "gpt-6-astra", label: "GPT-6 Astra" }] } }
      : path === "/api/remote-nodes" ? { enabled: true, nodes: [] }
      : path.endsWith("/files") ? { entries: [] }
      : path.endsWith("/crons") ? { jobs: [] } : { job: null, runs: [] };
    await route.fulfill({ json });
  });
  await page.goto(`/#`);
  await page.getByRole("button", { name: "Test", exact: true }).click();
  // A terminal mission lives under the collapsed "finished" fold.
  const row = page.getByRole("button", { name: /Pareto audit/ });
  if (!(await row.isVisible().catch(() => false))) {
    await page.getByRole("button", { name: /finished$/ }).click();
  }
  await row.click();
  return { release: () => { released = true; } };
}

const evUser = { id: "e1", event_id: "e1", sequence: 1, event_type: "user_message", content: "What's the status of Pareto audit?", timestamp: "" };
const evText = (t: string, seq: number) => ({ id: `t${seq}`, event_id: `t${seq}`, sequence: seq, event_type: "assistant_message", content: t, timestamp: "" });
const evTool = (seq: number) => ({ id: `c${seq}`, event_id: `c${seq}`, sequence: seq, event_type: "tool_call", tool_call_id: `c${seq}`, name: "read", timestamp: "" });

test("a running mission shows a compact startup status below the optimistic prompt", async ({ page }) => {
  await setup(page, { status: "active", events: [evUser] });
  await expect(page.locator(".user")).toContainText("What's the status of Pareto audit?");
  const status = page.locator(".agent-wait-status");
  await expect(status).toBeVisible();
  await expect(status).toContainText(/^Starting on .+/);
  const destination = (await status.textContent())!.replace(/^Starting on /, "").replace(/…$/, "");
  await expect(page.locator(".under-loc")).toContainText(destination);
  await page.locator(".main").screenshot({ path: "artifacts/orb-pending-quiet.png" });
});

test("the animation stops as soon as output arrives — a tool call counts", async ({ page }) => {
  await setup(page, { status: "active", events: [evUser, evTool(2)] });
  await expect(page.locator(".st-work")).toHaveCount(1);
  await expect(page.locator(".user.pending")).toHaveCount(0);
  await expect(page.locator(".launch-status")).toHaveCount(0);
});

test("text output also stops it, and nothing animates once finished", async ({ page }) => {
  await setup(page, { status: "completed", events: [evUser, evText("The audit is clean.", 2)] });
  await expect(page.locator(".st-text")).toContainText("The audit is clean.");
  await expect(page.locator(".user.pending")).toHaveCount(0);
  await expect(page.locator(".launch-status")).toHaveCount(0);
});

test("the prompt animates during the loading window too", async ({ page }) => {
  const { release } = await setup(page, { status: "active", hold: true });
  // Transcript still loading: the optimistic prompt is already animating.
  await expect(page.locator(".sk-transcript")).toBeVisible();
  await expect(page.locator(".launch-status")).toHaveCount(0);
  release();
});

test("reduced motion keeps the state visible without moving anything", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await setup(page, { status: "active", events: [evUser] });
  const turn = page.locator(".user.pending");
  await expect(turn).toHaveCount(1);
  expect(await turn.evaluate((el) => getComputedStyle(el).animationName)).toBe("none");
  // Still distinguishable from a settled turn.
  expect(Number(await turn.evaluate((el) => getComputedStyle(el).opacity))).toBeLessThan(1);
});

test("states the user must act on still show a compact banner", async ({ page }) => {
  await setup(page, { status: "awaiting_user", events: [evUser] });
  const banner = page.locator(".launch-status");
  await expect(banner).toHaveCount(1);
  await expect(banner).toContainText("Waiting for input");
  await expect(page.locator(".user.pending")).toHaveCount(0);
});

test("a failure still shows", async ({ page }) => {
  await setup(page, { status: "failed", events: [evUser] });
  await expect(page.getByRole("alert")).toContainText("Mission failed");
  await expect(page.locator(".user.pending")).toHaveCount(0);

});

test("an unconfirmed remote job still explains itself", async ({ page }) => {
  await setup(page, { status: "active", events: [evUser], remoteJob: { phase: "submit_ambiguous" } });
  await expect(page.locator(".launch-status")).toContainText("Checking submission");
});
