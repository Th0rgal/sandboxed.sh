import { expect, test, type Page, type Route } from "@playwright/test";

const PERF_MISSION_ID = "77777777-7777-4777-8777-777777777777";
const WORKSPACE_ID = "88888888-8888-4888-8888-888888888888";

type StoredEvent = {
  id: number;
  mission_id: string;
  sequence: number;
  event_type: string;
  timestamp: string;
  event_id?: string;
  tool_call_id?: string;
  tool_name?: string;
  content: string;
  metadata: Record<string, unknown>;
};

async function fulfillJson(
  route: Route,
  body: unknown,
  status = 200,
  headers: Record<string, string> = {},
) {
  await route.fulfill({
    status,
    contentType: "application/json",
    headers: { "Access-Control-Allow-Origin": "*", ...headers },
    body: JSON.stringify(body),
  });
}

function buildLargeMissionEvents(count = 500): StoredEvent[] {
  const base = Date.parse("2026-05-19T00:00:00.000Z");
  const events: StoredEvent[] = [];
  let id = 1;

  for (let i = 0; i < count; i++) {
    const timestamp = new Date(base + i * 2_000).toISOString();
    events.push({
      id,
      mission_id: PERF_MISSION_ID,
      sequence: id,
      event_type: "user_message",
      timestamp,
      event_id: `perf-user-${i}`,
      content: `User message ${i}: inspect the fixture and keep the response concise.`,
      metadata: {},
    });
    id += 1;

    events.push({
      id,
      mission_id: PERF_MISSION_ID,
      sequence: id,
      event_type: "assistant_message",
      timestamp: new Date(base + i * 2_000 + 750).toISOString(),
      event_id: `perf-assistant-${i}`,
      content:
        `Assistant response ${i}: ` +
        "This fixture row intentionally has enough text to exercise markdown layout without requiring a live backend.",
      metadata: { cost_cents: 0, cost_source: "unknown" },
    });
    id += 1;
  }

  return events;
}

async function mockLargeControlMission(page: Page, events: StoredEvent[]) {
  const now = new Date().toISOString();
  const mission = {
    id: PERF_MISSION_ID,
    title: "Large perf fixture mission",
    status: "completed",
    workspace_id: WORKSPACE_ID,
    workspace_name: "perf-workspace",
    backend: "codex",
    created_at: now,
    updated_at: now,
    history: [],
    resumable: true,
  };

  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;

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

    if (path === "/api/control/missions/current") {
      await fulfillJson(route, mission);
      return;
    }
    if (path === "/api/control/missions") {
      await fulfillJson(route, [mission]);
      return;
    }
    if (path === `/api/control/missions/${PERF_MISSION_ID}`) {
      await fulfillJson(route, mission);
      return;
    }
    if (path === `/api/control/missions/${PERF_MISSION_ID}/load`) {
      await fulfillJson(route, mission);
      return;
    }
    if (
      path === `/api/control/missions/${PERF_MISSION_ID}/events` ||
      path === `/api/control/missions/${PERF_MISSION_ID}/trace`
    ) {
      const sinceSeq = Number(url.searchParams.get("since_seq") ?? "0");
      const beforeSeq = Number(url.searchParams.get("before_seq") ?? "0");
      const latest = url.searchParams.get("latest") === "true";
      const limit = Number(url.searchParams.get("limit") ?? "0");
      let selected = events;

      if (sinceSeq > 0)
        selected = events.filter((event) => event.sequence > sinceSeq);
      if (beforeSeq > 0)
        selected = events.filter((event) => event.sequence < beforeSeq);
      if (latest && limit > 0) selected = selected.slice(-limit);
      else if (limit > 0) selected = selected.slice(0, limit);

      await fulfillJson(route, selected, 200, {
        "X-Total-Events": String(events.length),
        "X-Max-Sequence": String(events.at(-1)?.sequence ?? 0),
      });
      return;
    }
    if (path === "/api/control/running") {
      await fulfillJson(route, []);
      return;
    }
    if (path === "/api/control/progress") {
      await fulfillJson(route, {
        run_state: "idle",
        queue_len: 0,
        mission_id: PERF_MISSION_ID,
      });
      return;
    }
    if (path === "/api/control/queue") {
      await fulfillJson(route, []);
      return;
    }
    if (path === "/api/control/stream") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      });
      return;
    }
    if (path === "/api/workspaces") {
      await fulfillJson(route, [
        {
          id: WORKSPACE_ID,
          name: "perf-workspace",
          path: "/tmp/perf-workspace",
        },
      ]);
      return;
    }
    if (path === "/api/desktop/sessions") {
      await fulfillJson(route, []);
      return;
    }
    if (
      path === "/api/backends" ||
      path === "/api/providers" ||
      path === "/api/providers/backend-models"
    ) {
      await fulfillJson(route, []);
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
    if (path.startsWith("/api/library/")) {
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

async function mockControlWebSocket(
  page: Page,
  messages: Array<Record<string, unknown>>,
) {
  await page.addInitScript(
    ({ messages }) => {
      class ControlWebSocket extends EventTarget {
        static CONNECTING = 0;
        static OPEN = 1;
        static CLOSING = 2;
        static CLOSED = 3;
        readyState = ControlWebSocket.CONNECTING;
        onopen: ((event: Event) => void) | null = null;
        onmessage: ((event: MessageEvent) => void) | null = null;
        onerror: ((event: Event) => void) | null = null;
        onclose: ((event: CloseEvent) => void) | null = null;

        constructor(url: string | URL) {
          super();
          if (!String(url).includes("/api/control/ws")) {
            setTimeout(() => this.onerror?.(new Event("error")), 0);
            return;
          }
          setTimeout(() => {
            this.readyState = ControlWebSocket.OPEN;
            this.onopen?.(new Event("open"));
            messages.forEach((message, index) => {
              setTimeout(
                () => {
                  this.onmessage?.(
                    new MessageEvent("message", {
                      data: JSON.stringify(message),
                    }),
                  );
                },
                2000 * (index + 1),
              );
            });
          }, 0);
        }

        close() {
          this.readyState = ControlWebSocket.CLOSED;
        }

        send(_data?: unknown) {}
      }
      window.WebSocket = ControlWebSocket as unknown as typeof WebSocket;
    },
    { messages },
  );
}

async function expectPinnedToBottom(page: Page, testId: string) {
  await expect
    .poll(async () =>
      page.getByTestId(testId).evaluate((el) => {
        const node = el as HTMLElement;
        return Math.round(
          node.scrollHeight - node.scrollTop - node.clientHeight,
        );
      }),
    )
    .toBeLessThan(120);
}

async function getScrollTop(page: Page, testId: string) {
  return page
    .getByTestId(testId)
    .evaluate((el) => (el as HTMLElement).scrollTop);
}

test("control @perf keeps large mission within browser budgets", async ({
  page,
}) => {
  test.setTimeout(60_000);

  await page.addInitScript(() => {
    const win = window as Window &
      typeof globalThis & {
        __controlPerfLongtasks?: { max: number };
      };
    win.__controlPerfLongtasks = { max: 0 };
    try {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          win.__controlPerfLongtasks!.max = Math.max(
            win.__controlPerfLongtasks!.max,
            entry.duration,
          );
        }
      });
      observer.observe({ type: "longtask", buffered: true });
    } catch {
      // Browser does not support longtask entries in this environment.
    }
  });

  const events = buildLargeMissionEvents();
  await mockLargeControlMission(page, events);
  await page.goto(`/control?mission=${PERF_MISSION_ID}&debug=perf`);

  await expect(page.getByTestId("perf-overlay")).toBeVisible();
  await expect(page.getByText("Assistant response 499")).toBeVisible({
    timeout: 15_000,
  });

  await page.waitForTimeout(30_000);

  const budgets = await page.evaluate(() => {
    const memory = (
      performance as Performance & {
        memory?: { usedJSHeapSize?: number };
      }
    ).memory;
    const longtasks = (
      window as Window & {
        __controlPerfLongtasks?: { max: number };
      }
    ).__controlPerfLongtasks;

    return {
      heapMB: memory?.usedJSHeapSize
        ? memory.usedJSHeapSize / 1024 / 1024
        : null,
      maxLongtaskMs: longtasks?.max ?? 0,
      domNodes: document.getElementsByTagName("*").length,
    };
  });

  if (budgets.heapMB !== null) {
    expect(budgets.heapMB).toBeLessThan(300);
  }
  expect(budgets.maxLongtaskMs).toBeLessThan(500);
  expect(budgets.domNodes).toBeLessThan(5_000);
});

test("control lets users scroll down without anchor bounce", async ({
  page,
}) => {
  const events = buildLargeMissionEvents(80);
  await mockLargeControlMission(page, events);
  await page.goto(`/control?mission=${PERF_MISSION_ID}`);

  const chat = page.getByTestId("chat-scroll-container");
  await expect(page.getByText("Assistant response 79")).toBeVisible({
    timeout: 15_000,
  });
  await chat.evaluate((el) => {
    const node = el as HTMLElement;
    node.scrollTop = 0;
  });
  await page.waitForTimeout(100);

  await chat.evaluate((el) => {
    const node = el as HTMLElement;
    node.scrollTop = 900;
  });
  const afterScroll = await getScrollTop(page, "chat-scroll-container");
  await page.waitForTimeout(600);
  const afterSettled = await getScrollTop(page, "chat-scroll-container");

  expect(afterScroll).toBeGreaterThan(500);
  expect(afterSettled).toBeGreaterThan(afterScroll - 80);
});

test("control does not tug scroll while streamed rows update off bottom", async ({
  page,
}) => {
  const draftOne =
    Array.from({ length: 30 }, (_, i) => `draft update one ${i + 1}`).join(
      "\n\n",
    ) + "\n\nDRAFT_ONE_TAIL";
  const draftTwo =
    Array.from({ length: 45 }, (_, i) => `draft update two ${i + 1}`).join(
      "\n\n",
    ) + "\n\nDRAFT_TWO_TAIL";
  const events = buildLargeMissionEvents(80);

  await mockControlWebSocket(page, [
    {
      type: "text_delta",
      mission_id: PERF_MISSION_ID,
      content: draftOne,
    },
    {
      type: "text_delta",
      mission_id: PERF_MISSION_ID,
      content: draftTwo,
    },
  ]);
  await mockLargeControlMission(page, events);
  await page.goto(`/control?mission=${PERF_MISSION_ID}`);

  const chat = page.getByTestId("chat-scroll-container");
  await expect(page.getByText("Assistant response 79")).toBeVisible({
    timeout: 15_000,
  });
  await chat.evaluate((el) => {
    const node = el as HTMLElement;
    node.scrollTop = 900;
  });
  const beforeUpdate = await getScrollTop(page, "chat-scroll-container");
  await page.waitForTimeout(4600);
  const afterUpdate = await getScrollTop(page, "chat-scroll-container");

  expect(afterUpdate).toBeGreaterThan(beforeUpdate - 80);
  expect(afterUpdate).toBeLessThan(beforeUpdate + 80);
});

test("control keeps thoughts pinned while streamed thought finalizes", async ({
  page,
}) => {
  const longThought =
    Array.from({ length: 42 }, (_, i) => `reasoning line ${i + 1}`).join(
      "\n\n",
    ) + "\n\nTHOUGHT_TAIL_MARKER";
  const events: StoredEvent[] = [
    {
      id: 1,
      mission_id: PERF_MISSION_ID,
      sequence: 1,
      event_type: "user_message",
      timestamp: new Date().toISOString(),
      event_id: "scroll-user",
      content: "think out loud",
      metadata: {},
    },
  ];

  await mockControlWebSocket(page, [
    {
      type: "thinking",
      mission_id: PERF_MISSION_ID,
      content: longThought,
      done: false,
    },
    {
      type: "thinking",
      mission_id: PERF_MISSION_ID,
      content: longThought,
      done: true,
    },
  ]);
  await mockLargeControlMission(page, events);
  await page.goto(`/control?mission=${PERF_MISSION_ID}`);

  await expect(page.getByText("THOUGHT_TAIL_MARKER")).toBeVisible({
    timeout: 10_000,
  });
  await expectPinnedToBottom(page, "thoughts-scroll-container");
  await expect(page.getByText(/Thought for|Draft for/)).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.getByText("THOUGHT_TAIL_MARKER")).toBeVisible();
  await expectPinnedToBottom(page, "thoughts-scroll-container");
});

test("control keeps chat pinned when streaming draft becomes assistant message", async ({
  page,
}) => {
  const draft =
    Array.from({ length: 48 }, (_, i) => `draft line ${i + 1}`).join("\n\n") +
    "\n\nCHAT_TAIL_MARKER";
  const finalAnswer =
    Array.from({ length: 48 }, (_, i) => `final line ${i + 1}`).join("\n\n") +
    "\n\nASSISTANT_TAIL_MARKER";
  const events: StoredEvent[] = [
    {
      id: 1,
      mission_id: PERF_MISSION_ID,
      sequence: 1,
      event_type: "user_message",
      timestamp: new Date().toISOString(),
      event_id: "chat-scroll-user",
      content: "stream a long answer",
      metadata: {},
    },
  ];

  await mockControlWebSocket(page, [
    {
      type: "text_delta",
      mission_id: PERF_MISSION_ID,
      content: draft,
    },
    {
      type: "assistant_message",
      id: "chat-scroll-assistant",
      mission_id: PERF_MISSION_ID,
      content: finalAnswer,
      success: true,
      cost_cents: 0,
      cost_source: "unknown",
    },
  ]);
  await mockLargeControlMission(page, events);
  await page.goto(`/control?mission=${PERF_MISSION_ID}`);

  const chat = page.getByTestId("chat-scroll-container");
  await expect(chat.getByText("ASSISTANT_TAIL_MARKER")).toBeVisible({
    timeout: 10_000,
  });
  await expectPinnedToBottom(page, "chat-scroll-container");
});
