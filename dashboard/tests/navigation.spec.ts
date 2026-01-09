import { test, expect } from '@playwright/test';

test.describe('Navigation', () => {
  test('should navigate to all main pages', async ({ page }) => {
    await page.goto('/');

    // Check Overview page loads (title is "Global Monitor")
    await expect(page.getByRole('heading', { name: /Global Monitor/i })).toBeVisible();

    // Navigate directly to each page to test route accessibility
    await page.goto('/control');
    await expect(page).toHaveURL(/\/control/);

    await page.goto('/agents');
    await expect(page).toHaveURL(/\/agents/);
    await expect(page.locator('button[title="New Agent"]')).toBeVisible();

    await page.goto('/workspaces');
    await expect(page).toHaveURL(/\/workspaces/);
    await expect(page.getByRole('heading', { name: 'Workspaces' })).toBeVisible();

    await page.goto('/console');
    await expect(page).toHaveURL(/\/console/);

    await page.goto('/settings');
    await expect(page).toHaveURL(/\/settings/);
  });

  test('should navigate via sidebar links', async ({ page }) => {
    await page.goto('/');

    // Use sidebar to navigate to Mission
    const sidebar = page.locator('aside');
    await sidebar.getByRole('link', { name: 'Mission', exact: true }).click();
    await expect(page).toHaveURL(/\/control/);

    // Navigate to Agents via sidebar
    await sidebar.getByRole('button', { name: /Config/i }).click();
    await sidebar.getByRole('link', { name: /Agents/i }).click();
    await expect(page).toHaveURL(/\/agents/);

    // Navigate to Overview via sidebar
    await sidebar.getByRole('link', { name: /Overview/i }).click();
    await expect(page).toHaveURL('/');
  });

  test('should expand Config submenu', async ({ page }) => {
    await page.goto('/');

    // Click Config button to expand (it's a button, not a link)
    await page.getByRole('button', { name: /Config/i }).click();

    // Should show submenu items
    await expect(page.getByRole('link', { name: /Agents/i })).toBeVisible();
    await expect(page.getByRole('link', { name: /Skills/i })).toBeVisible();
    await expect(page.getByRole('link', { name: /Commands/i })).toBeVisible();
    await expect(page.getByRole('link', { name: /Rules/i })).toBeVisible();

    // Click on Skills to navigate
    await page.getByRole('link', { name: /Skills/i }).click();
    await expect(page).toHaveURL(/\/config\/skills/);
  });

  test('should expand Extensions submenu', async ({ page }) => {
    await page.goto('/');

    // Click Extensions button to expand (it's a button, not a link)
    await page.getByRole('button', { name: /Extensions/i }).click();

    // Should show submenu items
    await expect(page.getByRole('link', { name: /MCP Servers/i })).toBeVisible();
    await expect(page.getByRole('link', { name: /Plugins/i })).toBeVisible();
    await expect(page.getByRole('link', { name: /Tools/i })).toBeVisible();
  });

  test('sidebar should be visible on all pages', async ({ page }) => {
    const pages = ['/', '/agents', '/workspaces', '/control', '/settings'];

    for (const pagePath of pages) {
      await page.goto(pagePath);

      // Sidebar should contain navigation links
      await expect(page.getByRole('link', { name: /Overview/i })).toBeVisible();
      await expect(page.getByRole('link', { name: 'Mission', exact: true })).toBeVisible();
      await expect(page.getByRole('button', { name: /Config/i })).toBeVisible();
    }
  });

  test('should navigate to Config and Extensions subpages', async ({ page }) => {
    // Navigate to MCP Servers
    await page.goto('/extensions/mcps');
    // Wait for page to load (either shows MCP content or "Library unavailable" message)
    await expect(page.getByText(/MCP Servers|Library unavailable|Add MCP/i).first()).toBeVisible();

    // Navigate to Skills
    await page.goto('/config/skills');
    // Wait for page to load (either shows Skills content or "Library unavailable" message)
    await expect(page.getByText(/Skills|Library unavailable|Select a skill/i).first()).toBeVisible();

    // Navigate to Commands
    await page.goto('/config/commands');
    // Wait for page to load (either shows Commands content or "Library unavailable" message)
    await expect(page.getByText(/Commands|Library unavailable|Select a command/i).first()).toBeVisible();
  });
});
