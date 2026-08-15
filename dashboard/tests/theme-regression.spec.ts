import { expect, test } from "@playwright/test";

async function mountThemeFixture(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    const fixture = document.createElement("div");
    fixture.id = "theme-regression-fixture";
    fixture.innerHTML = `
      <div class="panel" data-testid="panel">
        <p class="muted-text" data-testid="muted">Muted text</p>
        <button class="icon-button" data-testid="icon-button">Button</button>
        <div class="panel" data-testid="toast">
          <strong>Toast</strong>
          <p class="muted-text">Notification body</p>
        </div>
        <div class="panel" data-testid="provider-card">
          <strong>Provider settings</strong>
          <button class="icon-button" data-testid="provider-test-button">Test connection</button>
        </div>
        <div class="panel" data-testid="config-editor">
          <pre class="code-block"><code>{ "backend": "opencode" }</code></pre>
        </div>
        <div class="prose-glass panel" data-testid="rich-preview">
          <p>Rich markdown preview <code class="code-inline text-xs font-mono">inline_token</code></p>
          <pre class="code-block"><code>fenced block</code></pre>
        </div>
        <p class="prose-glass">
          <code class="code-inline text-xs font-mono" data-testid="inline-code">sell_fee_split_spec</code>
        </p>
        <pre class="code-block" data-testid="code-block"><code>seller_amount + protocol_fee = total_amount</code></pre>
        <div class="user-message-bubble user-message-bubble-solid" data-testid="user-message">
          User message
        </div>
        <button class="mission-selector-trigger" data-testid="mission-trigger">
          Mission selector
        </button>
        <div class="mission-switcher-row-selected" data-testid="mission-row">
          Selected mission row
        </div>
        <span class="text-[10px] uppercase text-indigo-300/70" data-testid="mode-chip-active">active</span>
        <span class="rounded-full bg-indigo-500/20 text-[10px] font-semibold tabular-nums text-indigo-200" data-testid="unread-badge">9+</span>
      </div>
    `;
    document.body.appendChild(fixture);
  });
}

async function forceTheme(page: import("@playwright/test").Page, theme: "dark" | "light") {
  await page.addInitScript((nextTheme) => {
    localStorage.setItem("sandboxed-theme", nextTheme);
  }, theme);
  await page.emulateMedia({ colorScheme: theme });
  await page.goto("/");
  await page.evaluate((nextTheme) => {
    document.documentElement.dataset.theme = nextTheme;
  }, theme);
  await mountThemeFixture(page);
}

async function forceThemeAgainstSystemPreference(
  page: import("@playwright/test").Page,
  theme: "dark" | "light"
) {
  const opposite = theme === "light" ? "dark" : "light";
  await page.addInitScript((nextTheme) => {
    localStorage.setItem("sandboxed-theme", nextTheme);
  }, theme);
  await page.emulateMedia({ colorScheme: opposite });
  await page.goto("/");
  await page.evaluate((nextTheme) => {
    document.documentElement.dataset.theme = nextTheme;
  }, theme);
  await mountThemeFixture(page);
}

function parseRgb(input: string): [number, number, number] {
  const match = input.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (!match) throw new Error(`Expected rgb color, got ${input}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

test("markdown code uses readable semantic colors in dark theme", async ({ page }) => {
  await forceTheme(page, "dark");

  const inline = page.getByTestId("inline-code");
  const inlineBg = parseRgb(await inline.evaluate((el) => getComputedStyle(el).backgroundColor));
  const inlineText = parseRgb(await inline.evaluate((el) => getComputedStyle(el).color));

  expect(inlineBg[0]).toBeLessThan(90);
  expect(inlineBg[1]).toBeLessThan(90);
  expect(inlineBg[2]).toBeLessThan(110);
  expect(inlineText[0]).toBeGreaterThan(150);
  expect(inlineText[1]).toBeGreaterThan(150);
  expect(inlineText[2]).toBeGreaterThan(180);

  const blockBg = parseRgb(
    await page.getByTestId("code-block").evaluate((el) => getComputedStyle(el).backgroundColor)
  );
  expect(blockBg[0]).toBeLessThan(60);
  expect(blockBg[1]).toBeLessThan(70);
  expect(blockBg[2]).toBeLessThan(80);
});

test("semantic components switch to light theme via data-theme", async ({ page }) => {
  await forceThemeAgainstSystemPreference(page, "light");

  const inline = page.getByTestId("inline-code");
  const inlineBg = parseRgb(await inline.evaluate((el) => getComputedStyle(el).backgroundColor));
  const inlineText = parseRgb(await inline.evaluate((el) => getComputedStyle(el).color));
  const userText = parseRgb(
    await page.getByTestId("user-message").evaluate((el) => getComputedStyle(el).color)
  );
  const triggerText = parseRgb(
    await page.getByTestId("mission-trigger").evaluate((el) => getComputedStyle(el).color)
  );
  const rowText = parseRgb(
    await page.getByTestId("mission-row").evaluate((el) => getComputedStyle(el).color)
  );
  const toastBg = parseRgb(
    await page.getByTestId("toast").evaluate((el) => getComputedStyle(el).backgroundColor)
  );
  const providerBg = parseRgb(
    await page.getByTestId("provider-card").evaluate((el) => getComputedStyle(el).backgroundColor)
  );
  const configBg = parseRgb(
    await page
      .getByTestId("config-editor")
      .locator(".code-block")
      .evaluate((el) => getComputedStyle(el).backgroundColor)
  );
  const richInlineBg = parseRgb(
    await page
      .getByTestId("rich-preview")
      .locator(".code-inline")
      .evaluate((el) => getComputedStyle(el).backgroundColor)
  );

  expect(inlineBg[0]).toBeGreaterThan(200);
  expect(inlineBg[1]).toBeGreaterThan(210);
  expect(inlineText[2]).toBeLessThan(160);
  for (const color of [toastBg, providerBg, configBg, richInlineBg]) {
    expect(color[0]).toBeGreaterThan(200);
    expect(color[1]).toBeGreaterThan(210);
  }
  for (const color of [userText, triggerText, rowText]) {
    expect(color[0]).toBeLessThan(90);
    expect(color[1]).toBeLessThan(90);
    expect(color[2]).toBeLessThan(160);
  }

  const modeChip = parseRgb(
    await page.getByTestId("mode-chip-active").evaluate((el) => getComputedStyle(el).color),
  );
  const unread = parseRgb(
    await page.getByTestId("unread-badge").evaluate((el) => getComputedStyle(el).color),
  );
  // Pale indigo-200 / indigo-300/70 are unreadable on white; remaps must land
  // on a dark indigo (indigo-700-ish) so 10px labels stay above ~4.5:1.
  for (const color of [modeChip, unread]) {
    expect(color[0]).toBeLessThan(90);
    expect(color[1]).toBeLessThan(80);
    expect(color[2]).toBeGreaterThan(180);
  }
});

test("semantic components stay dark when data-theme overrides light system preference", async ({
  page,
}) => {
  await forceThemeAgainstSystemPreference(page, "dark");

  const inline = page.getByTestId("inline-code");
  const inlineBg = parseRgb(await inline.evaluate((el) => getComputedStyle(el).backgroundColor));
  const inlineText = parseRgb(await inline.evaluate((el) => getComputedStyle(el).color));
  const userBg = parseRgb(
    await page.getByTestId("user-message").evaluate((el) => getComputedStyle(el).backgroundColor)
  );
  const userText = parseRgb(
    await page.getByTestId("user-message").evaluate((el) => getComputedStyle(el).color)
  );

  expect(inlineBg[0]).toBeLessThan(90);
  expect(inlineBg[1]).toBeLessThan(90);
  expect(inlineBg[2]).toBeLessThan(110);
  expect(inlineText[0]).toBeGreaterThan(150);
  expect(inlineText[1]).toBeGreaterThan(150);
  expect(inlineText[2]).toBeGreaterThan(180);
  expect(userBg[2]).toBeGreaterThan(userBg[0]);
  for (const color of [userText]) {
    expect(color[0]).toBeGreaterThan(220);
    expect(color[1]).toBeGreaterThan(220);
    expect(color[2]).toBeGreaterThan(220);
  }
});
