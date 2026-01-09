import { test, expect } from '@playwright/test';

test.describe('Agents Page', () => {
  test('should load agents page', async ({ page }) => {
    await page.goto('/agents');

    // Check for page title
    await expect(page.getByText(/^Agents/).first()).toBeVisible();

    // Check for "New Agent" button
    await expect(page.locator('button[title="New Agent"]')).toBeVisible();
  });

  test('should show empty state or agents list', async ({ page }) => {
    await page.goto('/agents');

    // Wait for loading to complete (loader disappears)
    await page.waitForTimeout(2000);

    // Check for empty state or agents list or selection prompt
    const emptyText = page.getByText(/No agents yet/i);
    const selectPrompt = page.getByText(/Select an agent to edit or create a new one/i);
    const agentsList = page.locator('button').filter({ hasText: /^[^New]/ }); // Buttons that aren't "New Agent"

    const hasEmpty = await emptyText.isVisible().catch(() => false);
    const hasSelectPrompt = await selectPrompt.isVisible().catch(() => false);
    const hasAgents = await agentsList.count() > 0;

    // Should show either empty state, selection prompt, or agent list
    expect(hasEmpty || hasSelectPrompt || hasAgents).toBeTruthy();
  });

  test('should open new agent dialog', async ({ page }) => {
    await page.goto('/agents');

    // Wait for page to load
    await page.waitForTimeout(1000);

    // Click "New Agent" button
    await page.locator('button[title="New Agent"]').click();

    // Check dialog appears
    await expect(page.getByRole('heading', { name: 'New Agent' })).toBeVisible();

    // Check for name input
    await expect(page.getByPlaceholder(/code-reviewer/i)).toBeVisible();
  });

  test('should validate agent creation form', async ({ page }) => {
    await page.goto('/agents');

    // Wait for page to load
    await page.waitForTimeout(1000);

    // Open new agent dialog
    await page.locator('button[title="New Agent"]').click();

    // Wait for dialog to appear
    await expect(page.getByRole('heading', { name: 'New Agent' })).toBeVisible();

    // Create button should be disabled initially (no name)
    const createButton = page.getByRole('button', { name: 'Create', exact: true });
    await expect(createButton).toBeDisabled();

    // Fill in name
    await page.getByPlaceholder(/code-reviewer/i).fill('test-agent');

    // Button should be enabled once name is provided
    await expect(createButton).toBeEnabled();
  });

  test('should close new agent dialog', async ({ page }) => {
    await page.goto('/agents');

    // Wait for page to load
    await page.waitForTimeout(1000);

    // Open dialog
    await page.locator('button[title="New Agent"]').click();
    await expect(page.getByRole('heading', { name: 'New Agent' })).toBeVisible();

    // Click cancel
    await page.getByRole('button', { name: /Cancel/i }).click();

    // Dialog should close
    await expect(page.getByRole('heading', { name: 'New Agent' })).not.toBeVisible();
  });
});
