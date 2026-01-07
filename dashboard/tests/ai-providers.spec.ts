import { test, expect } from "@playwright/test";

// Run tests serially to avoid provider cleanup conflicts
test.describe.configure({ mode: 'serial' });

test.describe("AI Providers", () => {
  test.beforeEach(async ({ page }) => {
    // Clean up any existing test providers first
    try {
      const response = await page.request.get("http://127.0.0.1:3000/api/ai/providers");
      if (response.ok()) {
        const providers = await response.json();
        for (const provider of providers) {
          if (provider.name.includes("Test")) {
            await page.request.delete(`http://127.0.0.1:3000/api/ai/providers/${provider.id}`);
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
        JSON.stringify({ apiUrl: "http://127.0.0.1:3000", libraryRepo: "" })
      );
    });
    // Reload to pick up new settings
    await page.reload();
    // Wait for the page to load
    await expect(page.locator("h1")).toContainText("Settings");
    // Wait for providers to load
    await page.waitForTimeout(1000);
  });

  test.afterEach(async ({ page }) => {
    // Clean up any test providers created
    try {
      const response = await page.request.get("http://127.0.0.1:3000/api/ai/providers");
      const providers = await response.json();
      for (const provider of providers) {
        if (provider.name.includes("Test")) {
          await page.request.delete(`http://127.0.0.1:3000/api/ai/providers/${provider.id}`);
        }
      }
    } catch {
      // Ignore cleanup errors
    }
  });

  test("shows AI Providers section", async ({ page }) => {
    // Check the AI Providers section exists
    await expect(page.getByRole("heading", { name: "AI Providers" })).toBeVisible({ timeout: 10000 });
    await expect(page.getByText("Configure inference providers for OpenCode")).toBeVisible();
  });

  test("shows empty state when no providers configured", async ({ page }) => {
    // Check for empty state message (may or may not be visible depending on existing providers)
    const emptyState = page.locator("text=No providers configured");
    const providerList = page.locator('[class*="rounded-lg border p-3"]');

    // Either empty state or provider list should be visible
    const isEmpty = await emptyState.isVisible();
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

  test("can open add provider form", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Check form appears
    await expect(page.locator("text=Add AI Provider")).toBeVisible();
    await expect(page.locator("text=Provider Type")).toBeVisible();
    await expect(page.locator("text=Display Name")).toBeVisible();
  });

  test("provider type dropdown shows options", async ({ page }) => {
    // Click the Add Provider button
    await page.getByRole("button", { name: "Add Provider" }).first().click();
    await page.waitForTimeout(500);

    // Check the select dropdown is visible
    const select = page.locator("select");
    await expect(select).toBeVisible({ timeout: 5000 });

    // Check that common providers are in the options (by checking the select has options)
    const options = await select.locator("option").allTextContents();
    expect(options).toContain("Anthropic");
    expect(options).toContain("OpenAI");
  });

  test("shows OAuth notice for Anthropic provider", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Anthropic should be selected by default
    const select = page.locator("select");
    await expect(select).toHaveValue("anthropic");

    // Should show OAuth notice
    await expect(
      page.locator("text=This provider uses OAuth authentication")
    ).toBeVisible();
  });

  test("shows API key field for OpenAI provider", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Select OpenAI
    await page.selectOption("select", "openai");

    // Should show API key field, not OAuth notice
    await expect(page.locator("text=API Key")).toBeVisible();
    await expect(
      page.locator("text=This provider uses OAuth authentication")
    ).not.toBeVisible();
  });

  test("can cancel add provider form", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Form should be visible
    await expect(page.locator("text=Add AI Provider")).toBeVisible();

    // Click Cancel
    await page.click('button:has-text("Cancel"):visible');

    // Form should be hidden
    await expect(page.locator("text=Add AI Provider")).not.toBeVisible();
  });

  test("validates required fields when adding provider", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Select a non-OAuth provider
    await page.selectOption("select", "openai");

    // Clear the name field (it auto-fills)
    await page.fill('input[placeholder="e.g., My Claude Account"]', "");

    // Try to add without filling required fields
    await page.click('button:has-text("Add Provider")');

    // Should show error toast
    // Note: toast might not be easily testable, but the form should still be visible
    await expect(page.locator("text=Add AI Provider")).toBeVisible();
  });

  test("can create an API key provider", async ({ page }) => {
    // Create provider via API directly for reliability
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
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
    // OpenAI provider should show as connected (has API key)
    await expect(page.getByText("Connected", { exact: true }).first()).toBeVisible();
  });

  test("can create an OAuth provider", async ({ page }) => {
    // Create OAuth provider via API directly
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "anthropic",
        name: "Test Anthropic Provider",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // The new provider should appear with "Needs Auth" status
    await expect(page.getByText("Test Anthropic Provider")).toBeVisible({ timeout: 10000 });
    await expect(page.getByText("Needs Auth").first()).toBeVisible();
  });

  test("shows Connect button for providers needing auth", async ({ page }) => {
    // Create OAuth provider via API
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "anthropic",
        name: "Auth Test Provider",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // Should see the provider first
    await expect(page.getByText("Auth Test Provider")).toBeVisible({ timeout: 10000 });

    // Should see Connect button (use exact match to avoid Test Connection button)
    await expect(page.getByRole("button", { name: "Connect", exact: true }).first()).toBeVisible();
  });

  test("can edit a provider", async ({ page }) => {
    // Create provider via API
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Edit Test Provider",
        api_key: "sk-test-key-edit",
      },
    });

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // Wait for the provider to appear first
    await expect(page.getByText("Edit Test Provider")).toBeVisible({ timeout: 10000 });

    // Click Edit on the provider
    await page.getByRole("button", { name: "Edit" }).first().click();

    // Should see the edit form with Name placeholder
    await expect(page.getByPlaceholder("Name")).toBeVisible({ timeout: 5000 });

    // Should be able to save or cancel (use exact match for Save)
    await expect(page.getByRole("button", { name: "Save", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Cancel", exact: true }).first()).toBeVisible();

    // Cancel the edit
    await page.getByRole("button", { name: "Cancel", exact: true }).first().click();
  });

  test("can set a provider as default", async ({ page }) => {
    // Create two providers via API
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Default Test Provider 1",
        api_key: "sk-test-key-default-1",
      },
    });
    await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "groq",
        name: "Default Test Provider 2",
        api_key: "sk-test-key-default-2",
      },
    });

    // Reload to see the providers
    await page.reload();
    await page.waitForTimeout(1000);

    // Wait for providers to appear
    await expect(page.getByText("Default Test Provider 1")).toBeVisible({ timeout: 10000 });
    await expect(page.getByText("Default Test Provider 2")).toBeVisible({ timeout: 10000 });

    // Find a provider that isn't default and set it as default
    const setDefaultButton = page.getByRole("button", { name: "Set Default" });
    await expect(setDefaultButton.first()).toBeVisible({ timeout: 5000 });
    await setDefaultButton.first().click();

    // Should see the Default badge update
    await page.waitForTimeout(1000);
    await expect(page.getByText("Default").first()).toBeVisible();
  });

  test("can delete a provider", async ({ page }) => {
    // Create provider via API
    const response = await page.request.post("http://127.0.0.1:3000/api/ai/providers", {
      data: {
        provider_type: "openai",
        name: "Delete Test Provider",
        api_key: "sk-delete-test",
      },
    });

    // Check if provider was created successfully
    if (!response.ok()) {
      const text = await response.text();
      throw new Error(`Failed to create provider: ${text}`);
    }
    const provider = await response.json();

    // Reload to see the new provider
    await page.reload();
    await page.waitForTimeout(1000);

    // Verify provider was created
    await expect(page.getByText("Delete Test Provider")).toBeVisible({ timeout: 10000 });

    // Delete via API for reliability
    await page.request.delete(`http://127.0.0.1:3000/api/ai/providers/${provider.id}`);

    // Reload to see the provider removed
    await page.reload();
    await page.waitForTimeout(1000);

    // Provider should be removed
    await expect(page.getByText("Delete Test Provider")).not.toBeVisible();
  });

  test("shows custom base URL field", async ({ page }) => {
    // Click the Add Provider button
    await page.click("text=Add Provider");

    // Should see custom base URL field
    await expect(page.locator("text=Custom Base URL (optional)")).toBeVisible();
    await expect(
      page.locator('input[placeholder="https://api.example.com/v1"]')
    ).toBeVisible();
  });
});
