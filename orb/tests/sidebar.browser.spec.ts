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
