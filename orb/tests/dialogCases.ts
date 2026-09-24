import { test, expect } from "@playwright/test";

export function dialogCases(browserName: "chromium" | "webkit") {
    for (const theme of ["dark", "light"]) {
      test(`modal layout and nested confirmation — ${theme}`, async ({ page }) => {
        const errors: string[] = [];
        page.on("pageerror", error => errors.push(error.message));
        await page.goto(`/tests/dialog.html?theme=${theme}`);
        const opener = page.getByRole("button", { name: "Rename project" });
        await opener.focus(); await opener.click();
        const dialog = page.getByRole("dialog", { name: "Rename", exact: true });
        await expect(page.getByRole("textbox", { name: "Project name" })).toBeFocused();
        const box = (await dialog.boundingBox())!;
        const viewport = page.viewportSize()!;
        expect(Math.abs(box.x + box.width / 2 - viewport.width / 2)).toBeLessThan(2);
        expect(Math.abs(box.y + box.height / 2 - viewport.height / 2)).toBeLessThan(2);
        await expect(page.locator(".dlg-back-dim")).toHaveCount(1);
        expect(await page.locator(".dlg-back-dim").evaluate(el => getComputedStyle(el).backgroundColor)).not.toBe("rgba(0, 0, 0, 0)");
        await page.screenshot({ path: `test-results/modal-rename-${browserName}-${theme}.png` });
        await page.keyboard.press("Escape"); await expect(opener).toBeFocused();
        await page.getByRole("button", { name: "Delete chain", exact: true }).click();
        await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeFocused();
        await page.screenshot({ path: `test-results/modal-confirm-${browserName}-${theme}.png` });
        await page.keyboard.press("Escape");
        await page.getByRole("button", { name: "New cron", exact: true }).click();
        await page.getByRole("textbox", { name: "Name", exact: true }).fill("Changed draft");
        await page.getByRole("button", { name: "Schedule", exact: true }).click();
        await page.keyboard.press("Escape");
        await expect(page.getByRole("dialog", { name: "New cron", exact: true })).toBeVisible();
        await page.screenshot({ path: `test-results/modal-cron-${browserName}-${theme}.png` });
        await page.getByRole("button", { name: "Cancel", exact: true }).focus();
        await page.getByRole("button", { name: "Cancel", exact: true }).click();
        await expect(page.getByRole("button", { name: "Keep editing" })).toBeFocused();
        await expect(page.locator(".dlg[inert]")).toHaveCount(1);
        await expect(page.locator(".dlg-back-dim")).toHaveCount(1);
        await page.screenshot({ path: `test-results/modal-nested-${browserName}-${theme}.png` });
        await page.keyboard.press("Escape");
        await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeFocused();
        await expect(page.getByRole("textbox", { name: "Name", exact: true })).toHaveValue("Changed draft");
        await page.setViewportSize({ width: 390, height: 500 });
        const panel = page.getByRole("dialog", { name: "New cron", exact: true });
        const small = (await panel.boundingBox())!;
        expect(small.x).toBeGreaterThanOrEqual(19); expect(small.y).toBeGreaterThanOrEqual(19);
        expect(small.y + small.height).toBeLessThanOrEqual(481);
        expect(await panel.locator(".dlg-body").evaluate(el => el.scrollHeight > el.clientHeight)).toBe(true);
        await page.screenshot({ path: `test-results/modal-narrow-${browserName}-${theme}.png` });
        await panel.getByRole("button", { name: "Close", exact: true }).click();
        await expect(page.getByRole("dialog")).toHaveCount(0);
        expect(errors).toEqual([]);
      });
    }
}
