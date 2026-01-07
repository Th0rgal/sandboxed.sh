import { test, expect } from '@playwright/test';

test.describe('Workspaces Page', () => {
  test('should load workspaces page', async ({ page }) => {
    await page.goto('/workspaces');

    // Check for page title
    await expect(page.getByRole('heading', { name: 'Workspaces' })).toBeVisible();

    // Check for "New Workspace" button
    await expect(page.getByRole('button', { name: /New Workspace/i })).toBeVisible();
  });

  test('should show empty state or workspace cards', async ({ page }) => {
    await page.goto('/workspaces');

    // Wait for loading
    await page.waitForTimeout(1000);

    // Either shows empty state or workspace cards
    const emptyState = await page.getByText(/No workspaces yet/i).isVisible().catch(() => false);
    const hasCards = await page.locator('[class*="rounded-xl"]').count() > 0;

    expect(emptyState || hasCards).toBeTruthy();
  });

  test('should open new workspace dialog', async ({ page }) => {
    await page.goto('/workspaces');

    // Click "New Workspace" button
    await page.getByRole('button', { name: /New Workspace/i }).click();

    // Check dialog appears
    await expect(page.getByRole('heading', { name: 'New Workspace' })).toBeVisible();

    // Check for name input
    await expect(page.getByPlaceholder(/workspace|name/i)).toBeVisible();

    // Check for type selector
    await expect(page.locator('select')).toBeVisible();
  });

  test('should validate workspace creation form', async ({ page }) => {
    await page.goto('/workspaces');

    // Open new workspace dialog
    await page.getByRole('button', { name: /New Workspace/i }).click();

    // Try to create without name
    const createButton = page.getByRole('button', { name: /Create/i });
    await expect(createButton).toBeDisabled();

    // Fill in name
    await page.getByPlaceholder(/workspace|name/i).fill('test-workspace');

    // Now button should be enabled
    await expect(createButton).toBeEnabled();
  });

  test('should show workspace type options', async ({ page }) => {
    await page.goto('/workspaces');

    // Open new workspace dialog
    await page.getByRole('button', { name: /New Workspace/i }).click();

    // Check type selector has options
    const select = page.locator('select');
    await expect(select).toBeVisible();

    // Should have Host and Chroot options
    const options = await select.locator('option').allTextContents();
    expect(options.some(opt => opt.toLowerCase().includes('host'))).toBeTruthy();
    expect(options.some(opt => opt.toLowerCase().includes('chroot'))).toBeTruthy();
  });
});
