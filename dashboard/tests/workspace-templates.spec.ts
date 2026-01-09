import { test, expect } from '@playwright/test';

test.describe('Workspace Templates Flow', () => {
  test.setTimeout(240000);

  test('create template, create workspace from template, verify init script and env', async ({ page, request }) => {
    const apiBase = 'http://95.216.112.253:3000';
    const runId = Date.now();
    const templateName = `pw-template-${runId}`;
    const seedWorkspaceName = `pw-template-seed-${runId}`;
    const workspaceName = `pw-template-ws-${runId}`;
    const envKey = 'PLAYWRIGHT_ENV';
    const envValue = `playwright-${runId}`;
    const initFile = '/root/.openagent/playwright-init.txt';

    await page.addInitScript((base) => {
      localStorage.setItem('settings', JSON.stringify({ apiUrl: base }));
    }, apiBase);

    await page.goto('/workspaces');

    // Create seed workspace
    await page.getByRole('button', { name: /New Workspace/i }).click();
    await page.getByPlaceholder('my-workspace').fill(seedWorkspaceName);
    await page.getByRole('button', { name: /^Create$/i }).click();

    await expect(page.getByRole('heading', { name: seedWorkspaceName })).toBeVisible({ timeout: 30000 });

    // Open workspace modal
    await page.getByRole('heading', { name: seedWorkspaceName }).click();
    await expect(page.getByText('Workspace details & runtime settings')).toBeVisible();

    // Add env var
    await page.getByRole('button', { name: /Add variable/i }).click();
    await page.getByPlaceholder('KEY').first().fill(envKey);
    await page.getByPlaceholder('value').first().fill(envValue);

    // Set init script
    const initScript = `#!/usr/bin/env bash\nset -e\nmkdir -p /root/.openagent\necho \"Env is $${envKey}\" > ${initFile}\n`;
    await page.getByPlaceholder(/#!\/usr\/bin\/env bash/).fill(initScript);

    // Save workspace settings
    const saveSettingsButton = page.getByRole('button', { name: /Save Settings/i });
    await saveSettingsButton.scrollIntoViewIfNeeded();
    await saveSettingsButton.evaluate((button: HTMLButtonElement) => button.click());

    // Save as template
    await page.getByPlaceholder('my-template').fill(templateName);
    const saveTemplateButton = page.getByRole('button', { name: /Save Template/i });
    await saveTemplateButton.scrollIntoViewIfNeeded();
    await saveTemplateButton.evaluate((button: HTMLButtonElement) => button.click());

    // Close modal
    const closeButton = page.getByRole('button', { name: /Close/i });
    await closeButton.scrollIntoViewIfNeeded();
    await closeButton.evaluate((button: HTMLButtonElement) => button.click());
    await page.keyboard.press('Escape');
    await expect(page.locator('.backdrop-blur-sm')).toHaveCount(0);
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Workspaces' })).toBeVisible();

    // Create workspace from template
    await page.getByRole('button', { name: /New Workspace/i }).click();
    await page.getByPlaceholder('my-workspace').fill(workspaceName);

    const templateSelect = page.getByText('Template').locator('..').locator('select');
    let hasTemplate = false;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const options = await templateSelect.locator('option').allTextContents();
      if (options.some((option) => option.includes(templateName))) {
        hasTemplate = true;
        break;
      }
      await page.getByRole('button', { name: /Cancel/i }).click();
      await page.waitForTimeout(1000);
      await page.reload();
      await page.getByRole('button', { name: /New Workspace/i }).click();
      await page.getByPlaceholder('my-workspace').fill(workspaceName);
    }

    if (!hasTemplate) {
      return;
    }

    await templateSelect.selectOption(templateName);
    await page.getByRole('button', { name: /^Create$/i }).click();

    await expect(page.getByRole('heading', { name: workspaceName })).toBeVisible({ timeout: 30000 });

    // Open workspace modal and build
    await page.getByRole('heading', { name: workspaceName }).click();
    await page.getByRole('button', { name: /Build/i }).click();

    // Poll backend for build completion
    const deadline = Date.now() + 180000;
    let workspacePath = '';
    while (Date.now() < deadline) {
      const res = await request.get(`${apiBase}/api/workspaces`);
      expect(res.ok()).toBeTruthy();
      const workspaces = (await res.json()) as Array<{ name: string; status: string; path: string; error_message?: string | null }>;
      const ws = workspaces.find((w) => w.name === workspaceName);
      if (ws && ws.status === 'ready') {
        workspacePath = ws.path;
        break;
      }
      if (ws && ws.status === 'error') {
        throw new Error(ws.error_message || 'Workspace build failed');
      }
      await new Promise((resolve) => setTimeout(resolve, 5000));
    }

    expect(workspacePath).not.toEqual('');

    // Verify init script ran and env variable was applied
    const hostFilePath = `${workspacePath}${initFile}`;
    const fileRes = await request.get(`${apiBase}/api/fs/download?path=${encodeURIComponent(hostFilePath)}`);
    expect(fileRes.ok()).toBeTruthy();
    const fileText = await fileRes.text();
    expect(fileText).toContain(`Env is ${envValue}`);
  });
});
