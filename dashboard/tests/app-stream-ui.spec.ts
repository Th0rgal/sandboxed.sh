import { mkdir } from "node:fs/promises";
import { readFileSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page, type Route } from "@playwright/test";

const MISSION_ID = "77777777-7777-4777-8777-777777777777";
const WORKSPACE_ID = "88888888-8888-4888-8888-888888888888";
const SCREENSHOT_DIR = path.resolve(process.cwd(), "../reports/screenshots");

async function fulfillJson(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    headers: { "Access-Control-Allow-Origin": "*" },
    body: JSON.stringify(body),
  });
}

async function mockAppStreamControl(page: Page, frameBase64: string) {
  const now = new Date().toISOString();
  const session = {
    display: ":42",
    display_server: "wayland",
    compositor: "sway",
    status: "active",
    mission_id: MISSION_ID,
    mission_title: "Chrome checkout flow",
    mission_status: "running",
    started_at: now,
    process_running: true,
  };
  const mission = {
    id: MISSION_ID,
    title: "Chrome checkout flow",
    status: "running",
    workspace_id: WORKSPACE_ID,
    workspace_name: "app-stream-lab",
    backend: "codex",
    created_at: now,
    updated_at: now,
    history: [],
    desktop_sessions: [session],
  };

  await page.addInitScript((base64Frame) => {
    (
      window as unknown as { __appStreamSentCommands: string[] }
    ).__appStreamSentCommands = [];
    const frameBytes = Uint8Array.from(atob(base64Frame), (char) =>
      char.charCodeAt(0)
    );

    class MockWebSocket {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSING = 2;
      static CLOSED = 3;

      binaryType = "arraybuffer";
      readyState = MockWebSocket.OPEN;
      onopen: (() => void) | null = null;
      onmessage: ((event: { data: unknown }) => void) | null = null;
      onerror: (() => void) | null = null;
      onclose: (() => void) | null = null;

      constructor() {
        window.setTimeout(() => {
          this.onopen?.();
          window.setTimeout(() => {
            const frame = frameBytes.slice().buffer;
            this.onmessage?.({ data: frame });
          }, 20);
        }, 20);
      }

      send(data: string) {
        (
          window as unknown as { __appStreamSentCommands: string[] }
        ).__appStreamSentCommands.push(data);
      }

      close() {
        this.readyState = MockWebSocket.CLOSED;
        this.onclose?.();
      }
    }

    Object.defineProperty(window, "WebSocket", {
      configurable: true,
      writable: true,
      value: MockWebSocket,
    });
  }, frameBase64);

  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const pathName = new URL(request.url()).pathname;

    if (request.method() === "OPTIONS") {
      await route.fulfill({
        status: 204,
        headers: {
          "Access-Control-Allow-Origin": "*",
          "Access-Control-Allow-Headers": "*",
          "Access-Control-Allow-Methods": "GET,POST,PUT,PATCH,DELETE,OPTIONS",
        },
      });
      return;
    }

    if (pathName === "/api/health") {
      await fulfillJson(route, { auth_required: false, max_iterations: 50 });
      return;
    }
    if (pathName === "/api/control/missions/current") {
      await fulfillJson(route, mission);
      return;
    }
    if (pathName === "/api/control/missions") {
      await fulfillJson(route, [mission]);
      return;
    }
    if (pathName === `/api/control/missions/${MISSION_ID}`) {
      await fulfillJson(route, mission);
      return;
    }
    if (pathName === `/api/control/missions/${MISSION_ID}/events`) {
      await fulfillJson(route, { events: [], next_cursor: null, has_more: false });
      return;
    }
    if (pathName === "/api/control/running") {
      await fulfillJson(route, [
        { mission_id: MISSION_ID, state: "running", queue_len: 0 },
      ]);
      return;
    }
    if (pathName === "/api/control/progress") {
      await fulfillJson(route, {
        run_state: "running",
        queue_len: 0,
        mission_id: MISSION_ID,
      });
      return;
    }
    if (pathName === "/api/control/queue") {
      await fulfillJson(route, []);
      return;
    }
    if (pathName === "/api/control/stream") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      });
      return;
    }
    if (pathName === "/api/desktop/sessions") {
      await fulfillJson(route, { sessions: [session] });
      return;
    }
    if (pathName === "/api/workspaces") {
      await fulfillJson(route, [
        { id: WORKSPACE_ID, name: "app-stream-lab", path: "/workspaces/app-stream-lab" },
      ]);
      return;
    }
    if (
      pathName === "/api/backends" ||
      pathName === "/api/providers" ||
      pathName === "/api/providers/backend-models"
    ) {
      await fulfillJson(route, []);
      return;
    }
    if (/^\/api\/backends\/[^/]+\/agents$/.test(pathName)) {
      await fulfillJson(route, []);
      return;
    }
    if (/^\/api\/backends\/[^/]+\/config$/.test(pathName)) {
      await fulfillJson(route, { hidden_agents: [], default_agent: null });
      return;
    }
    if (pathName.startsWith("/api/library/")) {
      await fulfillJson(route, []);
      return;
    }

    await fulfillJson(route, {});
  });
}

test("control exposes a focused app-stream surface", async ({ page }) => {
  await mkdir(SCREENSHOT_DIR, { recursive: true });
  const frameBase64 = readFileSync(
    path.join(process.cwd(), "tests/fixtures/wayland-real-app-foot.jpg")
  ).toString("base64");
  await mockAppStreamControl(page, frameBase64);
  await page.addInitScript(() => {
    localStorage.setItem("sandboxed-theme", "dark");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/control");

  await expect(page.getByTestId("app-stream-panel")).toBeVisible();
  const panelResize = await page.getByTestId("right-side-panel").evaluate((panel) => {
    const styles = window.getComputedStyle(panel);
    return {
      maxHeight: Number.parseFloat(styles.maxHeight),
      minHeight: Number.parseFloat(styles.minHeight),
      resize: styles.resize,
    };
  });
  expect(panelResize.resize).toBe("both");
  expect(panelResize.minHeight).toBe(420);
  expect(panelResize.maxHeight).toBeGreaterThan(panelResize.minHeight);
  await expect(page.getByText("Interactive app surface")).toBeVisible();
  await expect(page.getByText("Pointer")).toBeVisible();
  await expect(page.getByText("Keyboard")).toBeVisible();
  await expect
    .poll(async () =>
      page.getByTestId("app-stream-canvas").evaluate((canvas) => {
        const element = canvas as HTMLCanvasElement;
        return `${element.width}x${element.height}`;
      })
    )
    .toBe("1280x720");
  const visiblePixels = await page
    .getByTestId("app-stream-canvas")
    .evaluate((canvas) => {
      const element = canvas as HTMLCanvasElement;
      const context = element.getContext("2d");
      if (!context) return 0;
      const { data } = context.getImageData(0, 0, element.width, element.height);
      let visible = 0;
      for (let i = 0; i < data.length; i += 400) {
        if (data[i] > 40 || data[i + 1] > 40 || data[i + 2] > 40) {
          visible += 1;
        }
      }
      return visible;
    });
  expect(visiblePixels).toBeGreaterThan(1000);

  const canvas = page.getByTestId("app-stream-canvas");
  const canvasBox = await canvas.boundingBox();
  expect(canvasBox).not.toBeNull();
  if (!canvasBox) return;
  await page.mouse.move(
    canvasBox.x + canvasBox.width / 2,
    canvasBox.y + canvasBox.height / 2
  );
  await page.mouse.down();
  await page.mouse.up();
  await page.waitForTimeout(300);
  await canvas.click({
    position: {
      x: Math.round(canvasBox.width / 3),
      y: Math.round(canvasBox.height / 3),
    },
  });
  await page.keyboard.type("ok");
  await canvas.dispatchEvent("wheel", { deltaY: 140, deltaX: 0 });

  const sentCommands = await page.evaluate(() =>
    (
      window as unknown as { __appStreamSentCommands: string[] }
    ).__appStreamSentCommands.map((raw) => JSON.parse(raw))
  );
  expect(sentCommands.some((cmd: { t: string }) => cmd.t === "move")).toBe(true);
  expect(
    sentCommands.some(
      (cmd: { t: string; double?: boolean }) => cmd.t === "click" && !cmd.double
    )
  ).toBe(true);
  expect(
    sentCommands.some(
      (cmd: { t: string; text?: string }) => cmd.t === "type" && cmd.text === "o"
    )
  ).toBe(true);
  expect(
    sentCommands.some(
      (cmd: { t: string; text?: string }) => cmd.t === "type" && cmd.text === "k"
    )
  ).toBe(true);
  expect(
    sentCommands.some(
      (cmd: { t: string; delta_y?: number }) =>
        cmd.t === "scroll" && (cmd.delta_y ?? 0) > 0
    )
  ).toBe(true);

  const panel = page.getByTestId("right-side-panel");
  const beforeResize = await panel.boundingBox();
  expect(beforeResize).not.toBeNull();
  if (!beforeResize) return;
  await page.mouse.move(
    beforeResize.x + beforeResize.width - 4,
    beforeResize.y + beforeResize.height - 4
  );
  await page.mouse.down();
  await page.mouse.move(
    beforeResize.x + beforeResize.width + 90,
    beforeResize.y + beforeResize.height + 90,
    { steps: 8 }
  );
  await page.mouse.up();
  const afterResize = await panel.boundingBox();
  expect(afterResize).not.toBeNull();
  if (!afterResize) return;
  expect(afterResize.width).toBeGreaterThan(beforeResize.width + 20);
  expect(afterResize.height).toBeGreaterThan(beforeResize.height + 20);

  await page
    .getByTestId("app-stream-panel")
    .screenshot({ path: path.join(SCREENSHOT_DIR, "app-stream-surface-dark.png") });
  await page.screenshot({
    path: path.join(SCREENSHOT_DIR, "app-stream-control-dark.png"),
    fullPage: true,
  });

  await page.evaluate(() => {
    localStorage.setItem("sandboxed-theme", "light");
    document.documentElement.dataset.theme = "light";
  });
  await page
    .getByTestId("app-stream-panel")
    .screenshot({ path: path.join(SCREENSHOT_DIR, "app-stream-surface-light.png") });
  await page.screenshot({
    path: path.join(SCREENSHOT_DIR, "app-stream-control-light.png"),
    fullPage: true,
  });

  await page.setViewportSize({ width: 390, height: 844 });
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "light";
  });
  await page.screenshot({
    path: path.join(SCREENSHOT_DIR, "app-stream-control-mobile-light.png"),
    fullPage: true,
  });
});
