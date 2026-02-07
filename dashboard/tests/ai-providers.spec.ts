import { test, expect } from "@playwright/test";

// Run tests serially to avoid provider cleanup conflicts
test.describe.configure({ mode: 'serial' });

let apiAvailable = false;

test.describe("AI Providers", () => {
  test.beforeEach(async ({ page, request }) => {
    apiAvailable = false;

    // Clean up any existing test providers first
    try {
      const response = await request.get("http://127.0.0.1:3000/api/ai/providers");
      if (response.ok()) {
        apiAvailable = true;
        const providers = await response.json();
        for (const provider of providers) {
          if (provider.name.includes("Test")) {
            await request.delete(`http://127.0.0.1:3000/api/ai/providers/${provider.id}`);
          }
        }
      }
    } catch {
      // Ignore cleanup errors
    }

    // Clear localStorage and set local API URL
    await page.goto("/settings");
    await page.evaluate(() => {
      localStorage.clear();
      localStorage.setItem(
        "settings",
        JSON.stringify({ apiUrl: "http://127.0.0.1:3000" })
      );
    });
    // Reload to pick up new settings
    await page.reload();
    // Wait for the page to load
    await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
    // Wait for providers to load
    await page.waitForTimeout(1000);
  });

  test.afterEach(async ({ request }) => {
    if (!apiAvailable) return;

    // Clean up any test providers created
    try {
      const response = await request.get("http://127.0.0.1:3000/api/ai/providers");
      if (response.ok()) {
        const providers = await response.json();
        for (const provider of providers) {
          if (provider.name.includes("Test")) {
            await request.delete(`http://127.0.0.1:3000/api/ai/providers/${provider.id}`);
          }
        }
      }
    } catch {
      // Ignore cleanup errors
    }
  });

  test("shows AI Providers section", async ({ page }) => {
    // Check the AI Providers section exists
    await expect(page.getByRole("heading", { name: "AI Providers" })).toBeVisible({ timeout: 10000 });
    await expect(page.getByText("Configure inference providers for OpenCode, Claude Code, and Codex")).toBeVisible();
  });

  test("shows empty state when no providers configured", async ({ page }) => {
    // Check for empty state message (may or may not be visible depending on existing providers)
    const emptyState = page.locator("text=No providers configured");
    const providerList = page.locator('[class*="rounded-lg border p-3"]');

    // Either empty state or provider list should be visible
    const isEmpty = await emptyState.isVisible().catch(() => false);
    if (isEmpty) {
      await expect(emptyState).toBeVisible();
      await expect(
        page.locator("text=Add an AI provider to enable inference capabilities")
      ).toBeVisible();
    } else {
      // Providers exist
      await expect(providerList.first()).toBeVisible();
    }
  });

  test("can open add provider modal", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Check modal appears
    await expect(page.getByRole("heading", { name: "Add Provider" })).toBeVisible();
  });

  test("provider list shows common providers", async ({ page }) => {
    test.skip(!apiAvailable, 'API not available');

    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Should list common providers
    await expect(page.getByRole("button", { name: "Anthropic" })).toBeVisible();
    await expect(page.getByRole("button", { name: "OpenAI" })).toBeVisible();
  });

  test("shows OAuth options for Anthropic provider", async ({ page }) => {
    test.skip(!apiAvailable, 'API not available');

    // Open modal
    await page.click("text=Add Provider");

    // Select Anthropic
    await page.getByRole("button", { name: "Anthropic" }).click();

    // Should show auth methods
    await expect(page.getByRole("heading", { name: /Connect Anthropic/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Claude Pro\/Max/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /Enter API Key/i })).toBeVisible();
  });

  test("shows OAuth options for OpenAI provider", async ({ page }) => {
    test.skip(!apiAvailable, 'API not available');

    // Open modal
    await page.click("text=Add Provider");

    // Select OpenAI
    await page.getByRole("button", { name: "OpenAI" }).click();

    // Should show auth methods
    await expect(page.getByRole("heading", { name: /Connect OpenAI/i })).toBeVisible();
    await expect(
      page.getByRole("button", { name: /ChatGPT Plus\/Pro/i })
    ).toBeVisible();
    await expect(page.getByRole("button", { name: /Enter API Key/i })).toBeVisible();
  });

  test("can cancel add provider modal", async ({ page }) => {
    // Open modal
    await page.click("text=Add Provider");
    await expect(page.getByRole("heading", { name: "Add Provider" })).toBeVisible();

    // Close with Escape
    await page.keyboard.press('Escape');
    await expect(page.getByRole("heading", { name: "Add Provider" })).not.toBeVisible();
  });

  test("validates required fields when adding provider", async ({ page }) => {
    test.skip(!apiAvailable, 'API not available');

    // Open modal and pick OpenAI
    await page.click("text=Add Provider");
    await page.getByRole("button", { name: "OpenAI" }).click();

    // Select API key method
    await page.getByRole("button", { name: /Enter API Key/i }).click();

    // OpenAI supports backend targeting. Continue to API key entry.
    await expect(page.getByRole("heading", { name: /Select Backends/i })).toBeVisible();
    await page.getByRole("button", { name: "Continue" }).click();

    // Should show API key field
    await expect(page.getByPlaceholder("sk-...")).toBeVisible();

    const addButton = page.getByRole("button", { name: "Add Provider" });
    await expect(addButton).toBeDisabled();

    // Fill API key
    await page.getByPlaceholder("sk-...").fill("sk-test-key");
    await expect(addButton).toBeEnabled();
  });

  test("can create an API key provider", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create provider via API directly for reliability
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Test OpenAI Provider",
        api_key: "sk-test-key-12345",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // The new provider should appear in the list
    await expect(page.getByText("Test OpenAI Provider")).toBeVisible({ timeout: 10000 });
  });

  test("can create an OAuth provider", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create OAuth provider via API directly
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "anthropic",
        name: "Test Anthropic Provider",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // The new provider should appear in the list
    await expect(page.getByText("Test Anthropic Provider")).toBeVisible({ timeout: 10000 });
  });

  test("shows Connect button for providers needing auth", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create OAuth provider via API
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "anthropic",
        name: "Auth Test Provider",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // Should see the provider first
    const providerRow = page.locator('div').filter({ hasText: "Auth Test Provider" }).filter({
      has: page.locator('button[title="Connect"]'),
    }).first();
    await expect(providerRow).toBeVisible({ timeout: 10000 });

    await providerRow.hover();
    await expect(providerRow.locator('button[title="Connect"]')).toBeVisible();
  });

  test("can edit a provider", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create provider via API
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Edit Test Provider",
        api_key: "sk-test-key-edit",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    const providerRow = page.locator('div').filter({ hasText: "Edit Test Provider" }).filter({
      has: page.locator('button[title="Edit"]'),
    }).first();
    await expect(providerRow).toBeVisible({ timeout: 10000 });

    await providerRow.hover();
    await providerRow.locator('button[title="Edit"]').click();

    // Should see the edit form with Name placeholder
    await expect(page.getByPlaceholder("Name")).toBeVisible({ timeout: 5000 });

    // Should be able to save or cancel
    await expect(page.getByRole("button", { name: "Save" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Cancel" }).first()).toBeVisible();

    // Cancel the edit
    await page.getByRole("button", { name: "Cancel" }).first().click();
  });

  test("can set a provider as default", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create two providers via API
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Default Test Provider 1",
        api_key: "sk-test-key-default-1",
      },
    });
    await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "groq",
        name: "Default Test Provider 2",
        api_key: "sk-test-key-default-2",
      },
    });

    const listResponse = await request.get("http://127.0.0.1:3000/api/ai/providers");
    const providers = await listResponse.json();
    const candidates = providers.filter((provider: { name: string }) =>
      provider.name.startsWith("Default Test Provider")
    );
    const target = candidates.find((provider: { is_default: boolean }) => !provider.is_default);

    test.skip(!target, 'No non-default provider to update');

    // Reload to see the providers
    await page.reload();
    await page.waitForTimeout(1000);

    const providerRow = page.locator('div').filter({ hasText: target.name }).filter({
      has: page.locator('button[title="Set as default"]'),
    }).first();
    await expect(providerRow).toBeVisible({ timeout: 10000 });

    await providerRow.hover();
    await providerRow.locator('button[title="Set as default"]').click();

    // Should see the Default star indicator
    await expect(providerRow.locator('svg.text-indigo-400')).toBeVisible({ timeout: 10000 });
  });

  test("can delete a provider", async ({ page, request }) => {
    test.skip(!apiAvailable, 'API not available');

    // Create provider via API
    const response = await request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Delete Test Provider",
        api_key: "sk-delete-test",
      },
    });

    if (!response.ok()) {
      const text = await response.text();
      throw new Error(`Failed to create provider: ${text}`);
    }

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    const providerRow = page.locator('div').filter({ hasText: "Delete Test Provider" }).filter({
      has: page.locator('button[title="Delete"]'),
    }).first();
    await expect(providerRow).toBeVisible({ timeout: 10000 });

    await providerRow.hover();
    await providerRow.locator('button[title="Delete"]').click();
    await page.waitForTimeout(1000);

    // Provider should be removed
    await expect(page.getByText("Delete Test Provider")).not.toBeVisible();
  });
});
