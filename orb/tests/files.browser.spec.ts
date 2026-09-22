import { test, expect } from "@playwright/test";
test.use({
  browserName:
    process.env.ORB_TEST_BROWSER === "webkit" ? "webkit" : "chromium",
});
test("Cursor-style files preserve chat, resolve references and navigate Markdown/source", async ({
  page,
}) => {
  const entries = [
    { name: "guarantees.yaml", path: "audit/guarantees.yaml", kind: "file" },
    {
      name: "IMPLEMENTATION-BRIEF.md",
      path: "audit/IMPLEMENTATION-BRIEF.md",
      kind: "file",
    },
  ];
  await page.route("**/api/**", (route) => {
    if (!route.request().url().endsWith("/file-resources"))
      return route.fulfill({
        json: {
          settings: {
            prompt: "# Pareto controller\n\nVerify the seven guarantees.",
          },
        },
      });
    const q = route.request().postDataJSON();
    let body: unknown = {};
    if (q.action === "roots")
      body = {
        sources: [
          { id: "workspace", label: "Workspace · Core", available: true },
        ],
      };
    if (q.action === "list")
      body = {
        entries: q.path
          ? entries
          : [{ name: "audit", path: "audit", kind: "dir" }],
      };
    if (q.action === "resolve")
      body = {
        results: q.paths.map((reference: string) => ({
          reference,
          matches: entries.filter((e) => e.path === reference),
        })),
      };
    if (q.action === "search")
      body = { entries: entries.filter((e) => e.path.includes(q.query)) };
    if (q.action === "read")
      body = {
        content: q.path.endsWith(".md")
          ? "# Pareto — Implementation brief\n\nSeven mandatory properties establish the audit scope.\n\n## Guarantees\n\n- Price preservation\n- Epoch interest settlement\n- Single-use claims"
          : "guarantees:\n  - id: PRICE-1\n    title: Price preservation\n  - id: CLAIM-1\n    title: Single-use claims",
        size: 160,
        binary: false,
        truncated: false,
      };
    return route.fulfill({ json: body });
  });
  await page.setViewportSize({ width: 1700, height: 1000 });
  await page.goto("/tests/file-panel.html");
  const initialChat = await page.locator(".main").boundingBox();
  await page.getByRole("textbox", { name: "Draft" }).fill("Keep my draft");
  await page
    .getByRole("button", { name: "IMPLEMENTATION-BRIEF.md", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Pareto — Implementation brief" }),
  ).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Draft" })).toHaveValue(
    "Keep my draft",
  );
  await expect(page.locator(".file-panel")).toBeVisible();
  const tabBar = await page.locator(".file-tabs").boundingBox();
  const titleBar = await page.locator(".titlebar").boundingBox();
  expect(Math.abs(tabBar!.height - titleBar!.height)).toBeLessThanOrEqual(1);
  const tab = await page.locator(".file-tab").first().boundingBox();
  expect(tab!.y - tabBar!.y).toBeGreaterThanOrEqual(5);
  await page.screenshot({ path: "test-results/files-dark.png" });
  await page.locator(".file-preview").click();
  await page.keyboard.press("Meta+/");
  await expect(page.locator(".file-source-code")).toBeVisible();
  await page.getByRole("button", { name: "Preview", exact: true }).click();
  const handle = await page.getByRole("separator", { name: "Resize file panel", exact: true }).boundingBox();
  await page.mouse.move(handle!.x + handle!.width / 2, handle!.y + 100);
  await page.mouse.down();
  await page.mouse.move(handle!.x - 70, handle!.y + 100);
  await page.mouse.up();
  await page.getByRole("button", { name: "Close files", exact: true }).click();
  await expect(page.locator(".file-panel")).toHaveCount(0);
  await expect
    .poll(async () =>
      Math.round((await page.locator(".main").boundingBox())!.width),
    )
    .toBe(Math.round(initialChat!.width));
  await expect(page.locator(".app")).toHaveAttribute(
    "data-files-open",
    "false",
  );
  await expect(page.getByRole("textbox", { name: "Draft" })).toHaveValue(
    "Keep my draft",
  );
  await page.keyboard.press("Meta+p");
  await page
    .getByRole("textbox", { name: "Find file by name or path" })
    .fill("guarantees");
  await page.locator(".file-search-results button").first().click();
  await expect(page.locator(".file-source-code")).toContainText("PRICE-1");
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));
  await page.screenshot({ path: "test-results/files-light.png" });
  await page.setViewportSize({ width: 900, height: 800 });
  await expect(page.locator(".file-panel")).toBeVisible();
});
