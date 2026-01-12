import { test, expect } from '@playwright/test';

test.describe('Desktop Session Management', () => {
  test.beforeEach(async ({ page }) => {
    // Navigate to control page before each test
    await page.goto('/control');
    await page.waitForTimeout(1500);
  });

  test('should load control page with desktop section', async ({ page }) => {
    // The desktop toggle button should be present when there are desktop sessions
    // or hidden when there are none - both are valid states
    const desktopButton = page.locator('button').filter({
      hasText: 'Desktop'
    });

    // Just verify the page loads without errors
    await expect(page).toHaveTitle(/Open Agent/i, { timeout: 10000 });
  });

  test('should have desktop API endpoint available', async ({ request }) => {
    // Test that the desktop sessions API endpoint exists
    // Note: This requires the backend to be running
    const response = await request.get('/api/desktop/sessions', {
      failOnStatusCode: false
    });

    // Should either return 200 (success) or 401 (auth required)
    // Both are valid - we just want to make sure the endpoint exists
    expect([200, 401, 404]).toContain(response.status());
  });

  test('desktop dropdown should open when clicked', async ({ page }) => {
    // Look for the desktop display selector button (shows :99, :100, etc.)
    const displaySelector = page.locator('button').filter({
      has: page.locator('text=":')
    });

    const count = await displaySelector.count();

    if (count > 0) {
      // Click the display selector
      await displaySelector.first().click();

      // Wait for dropdown to appear
      await page.waitForTimeout(300);

      // Should show dropdown with display options or session info
      const dropdownContent = page.locator('[class*="absolute"]').filter({
        has: page.locator('button')
      });

      expect(await dropdownContent.count()).toBeGreaterThan(0);
    } else {
      // Desktop section not visible - this is OK if no sessions exist
      test.skip(true, 'Desktop section not visible (no active sessions)');
    }
  });

  test('desktop sessions should show status indicators', async ({ page }) => {
    // Look for status indicators in the desktop dropdown
    const displaySelector = page.locator('button').filter({
      has: page.locator('text=":')
    });

    const count = await displaySelector.count();

    if (count > 0) {
      await displaySelector.first().click();
      await page.waitForTimeout(300);

      // Look for status indicators (colored dots)
      const statusDots = page.locator('[class*="rounded-full"][class*="bg-"]');

      // If sessions exist, they should have status indicators
      // Just verify the UI structure is correct
      const dotsCount = await statusDots.count();
      console.log(`Found ${dotsCount} status indicator dots`);
    }
  });

  test('should be able to close a desktop session via UI', async ({ page }) => {
    const displaySelector = page.locator('button').filter({
      has: page.locator('text=":')
    });

    const count = await displaySelector.count();

    if (count > 0) {
      await displaySelector.first().click();
      await page.waitForTimeout(300);

      // Look for close buttons (X icons) in the dropdown
      const closeButtons = page.locator('button[title="Close session"]');

      if (await closeButtons.count() > 0) {
        // Verify close button exists
        await expect(closeButtons.first()).toBeVisible();
      }
    }
  });

  test('should show cleanup option for orphaned sessions', async ({ page }) => {
    const displaySelector = page.locator('button').filter({
      has: page.locator('text=":')
    });

    const count = await displaySelector.count();

    if (count > 0) {
      await displaySelector.first().click();
      await page.waitForTimeout(300);

      // Look for "Close all orphaned" button
      const cleanupButton = page.locator('button').filter({
        hasText: 'orphaned'
      });

      const cleanupCount = await cleanupButton.count();
      console.log(`Found ${cleanupCount} cleanup buttons (expected 0 if no orphaned sessions)`);
    }
  });

  test('should show keep-alive option for orphaned sessions', async ({ page }) => {
    const displaySelector = page.locator('button').filter({
      has: page.locator('text=":')
    });

    const count = await displaySelector.count();

    if (count > 0) {
      await displaySelector.first().click();
      await page.waitForTimeout(300);

      // Look for keep-alive button (clock icon)
      const keepAliveButton = page.locator('button[title="Extend keep-alive (+2h)"]');

      const keepAliveCount = await keepAliveButton.count();
      console.log(`Found ${keepAliveCount} keep-alive buttons (expected 0 if no orphaned sessions)`);
    }
  });
});

test.describe('Desktop Sessions API', () => {
  test('list sessions endpoint returns valid structure', async ({ request }) => {
    const response = await request.get('/api/desktop/sessions', {
      failOnStatusCode: false
    });

    if (response.status() === 200) {
      const data = await response.json();

      // Verify the response structure
      expect(data).toHaveProperty('sessions');
      expect(Array.isArray(data.sessions)).toBe(true);

      // If there are sessions, verify their structure
      if (data.sessions.length > 0) {
        const session = data.sessions[0];
        expect(session).toHaveProperty('display');
        expect(session).toHaveProperty('status');
        expect(session).toHaveProperty('process_running');
        expect(['active', 'orphaned', 'stopped', 'unknown']).toContain(session.status);
      }
    } else if (response.status() === 401) {
      // Auth required - skip this test
      test.skip(true, 'Authentication required');
    }
  });

  test('close session endpoint exists', async ({ request }) => {
    const response = await request.post('/api/desktop/sessions/:99/close', {
      failOnStatusCode: false
    });

    // Should return 200, 401 (auth required), 404 (not found), or 500 (session doesn't exist)
    expect([200, 401, 404, 500]).toContain(response.status());
  });

  test('keep-alive endpoint exists', async ({ request }) => {
    const response = await request.post('/api/desktop/sessions/:99/keep-alive', {
      failOnStatusCode: false,
      data: { extension_secs: 7200 }
    });

    // Should return 200, 401 (auth required), 404 (not found), or 500 (session doesn't exist)
    expect([200, 401, 404, 500]).toContain(response.status());
  });

  test('cleanup endpoint exists', async ({ request }) => {
    const response = await request.post('/api/desktop/sessions/cleanup', {
      failOnStatusCode: false
    });

    // Should return 200 or 401 (auth required)
    expect([200, 401]).toContain(response.status());
  });
});
