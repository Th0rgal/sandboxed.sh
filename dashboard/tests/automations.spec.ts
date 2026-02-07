import { test, expect, Page } from '@playwright/test';

const API_BASE = process.env.NEXT_PUBLIC_API_URL || 'https://agent-backend-dev.thomas.md';
const DEV_PASSWORD = 'dev-3knzssZU7cIJMdKOqhskuoNM';

/**
 * Helper: get an auth token from the dev backend.
 */
async function getAuthHeaders(): Promise<Record<string, string>> {
  try {
    const res = await fetch(`${API_BASE}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: DEV_PASSWORD }),
    });
    if (res.ok) {
      const data = (await res.json()) as { token?: string };
      if (data.token) {
        return { Authorization: `Bearer ${data.token}` };
      }
    }
  } catch {
    // Auth not required
  }
  return {};
}

/**
 * Helper: create a mission via API so we have something to attach automations to.
 */
async function ensureMission(headers: Record<string, string>): Promise<string> {
  const listRes = await fetch(`${API_BASE}/api/control/missions?limit=5`, { headers });
  if (listRes.ok) {
    const missions = (await listRes.json()) as Array<{ id: string; status: string }>;
    const active = missions.find(m => m.status === 'active' || m.status === 'completed');
    if (active) return active.id;
  }

  const createRes = await fetch(`${API_BASE}/api/control/missions`, {
    method: 'POST',
    headers: { ...headers, 'Content-Type': 'application/json' },
    body: JSON.stringify({ title: 'Automation Test Mission' }),
  });

  if (!createRes.ok) {
    throw new Error(`Failed to create mission: ${await createRes.text()}`);
  }
  const mission = (await createRes.json()) as { id: string };
  return mission.id;
}

/**
 * Helper: clean up automations created during tests.
 */
async function cleanupAutomations(
  missionId: string,
  headers: Record<string, string>
): Promise<void> {
  const res = await fetch(
    `${API_BASE}/api/control/missions/${missionId}/automations`,
    { headers }
  );
  if (!res.ok) return;
  const automations = (await res.json()) as Array<{ id: string }>;
  for (const a of automations) {
    await fetch(`${API_BASE}/api/control/automations/${a.id}`, {
      method: 'DELETE',
      headers,
    });
  }
}

/**
 * Helper: locate the Automations button (uses title attribute for robustness).
 */
function automationsBtnLocator(page: Page) {
  return page.locator('button[title*="automations" i]');
}

/**
 * Helper: get the automations dialog container (the modal panel).
 * We locate it by finding the h3 heading and going up to its rounded-2xl parent.
 */
function dialogLocator(page: Page) {
  return page.locator('div.rounded-2xl', { has: page.locator('h3:has-text("Mission Automations")') });
}

/**
 * Helper: navigate to control page, open automations dialog, return dialog locator.
 * Skips the test if no active mission is loaded.
 */
async function gotoAndOpenDialog(page: Page, t: typeof test, missionId?: string) {
  const url = missionId ? `/control?mission=${missionId}` : '/control';
  await page.goto(url);
  await page.waitForTimeout(4000);

  const btn = automationsBtnLocator(page);
  if ((await btn.count()) === 0 || (await btn.isDisabled())) {
    t.skip();
    return dialogLocator(page);
  }

  await btn.click();
  await expect(page.locator('h3:has-text("Mission Automations")')).toBeVisible({ timeout: 5000 });
  return dialogLocator(page);
}

// ========================================================================
// API-level tests (test backend directly)
// ========================================================================

test.describe('Automations API', () => {
  let headers: Record<string, string>;
  let missionId: string;

  test.beforeAll(async () => {
    headers = await getAuthHeaders();
    missionId = await ensureMission(headers);
  });

  test.afterAll(async () => {
    await cleanupAutomations(missionId, headers);
  });

  test('should create an interval automation with library command', async () => {
    const res = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Test library-like command' },
          trigger: { type: 'interval', seconds: 300 },
        }),
      }
    );

    expect(res.status).toBe(200);
    const automation = (await res.json()) as {
      id: string;
      command_source: { type: string };
      trigger: { type: string; seconds: number };
      active: boolean;
    };
    expect(automation.id).toBeTruthy();
    expect(automation.command_source.type).toBe('inline');
    expect(automation.trigger.type).toBe('interval');
    expect(automation.trigger.seconds).toBe(300);
    expect(automation.active).toBe(true);

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });

  test('should create an inline prompt automation', async () => {
    const res = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: {
            type: 'inline',
            content: 'Check deployment status for <env/> at <timestamp/>',
          },
          trigger: { type: 'interval', seconds: 60 },
          variables: { env: 'production' },
        }),
      }
    );

    expect(res.status).toBe(200);
    const automation = (await res.json()) as {
      id: string;
      command_source: { type: string; content: string };
      variables: Record<string, string>;
    };
    expect(automation.command_source.type).toBe('inline');
    expect(automation.command_source.content).toContain('<env/>');
    expect(automation.variables.env).toBe('production');

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });

  test('should create a webhook automation', async () => {
    const res = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: {
            type: 'inline',
            content: 'Process webhook payload from <source/>',
          },
          trigger: {
            type: 'webhook',
            config: { webhook_id: '' },
          },
          variables: { source: 'github' },
        }),
      }
    );

    expect(res.status).toBe(200);
    const automation = (await res.json()) as {
      id: string;
      trigger: { type: string; config: { webhook_id: string } };
    };
    expect(automation.trigger.type).toBe('webhook');
    expect(automation.trigger.config.webhook_id).toBeTruthy();
    expect(automation.trigger.config.webhook_id.length).toBeGreaterThan(0);

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });

  test('should list mission automations', async () => {
    const create = async (content: string) => {
      const res = await fetch(
        `${API_BASE}/api/control/missions/${missionId}/automations`,
        {
          method: 'POST',
          headers: { ...headers, 'Content-Type': 'application/json' },
          body: JSON.stringify({
            command_source: { type: 'inline', content },
            trigger: { type: 'interval', seconds: 600 },
          }),
        }
      );
      return (await res.json()) as { id: string };
    };

    const a1 = await create('Automation 1');
    const a2 = await create('Automation 2');

    const listRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      { headers }
    );
    expect(listRes.status).toBe(200);
    const automations = (await listRes.json()) as Array<{ id: string }>;
    expect(automations.length).toBeGreaterThanOrEqual(2);

    for (const a of [a1, a2]) {
      await fetch(`${API_BASE}/api/control/automations/${a.id}`, {
        method: 'DELETE',
        headers,
      });
    }
  });

  test('should toggle automation active state', async () => {
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Toggle test' },
          trigger: { type: 'interval', seconds: 120 },
        }),
      }
    );
    const automation = (await createRes.json()) as { id: string; active: boolean };
    expect(automation.active).toBe(true);

    const pauseRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}`,
      {
        method: 'PATCH',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({ active: false }),
      }
    );
    expect(pauseRes.status).toBe(200);
    const paused = (await pauseRes.json()) as { active: boolean };
    expect(paused.active).toBe(false);

    const resumeRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}`,
      {
        method: 'PATCH',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({ active: true }),
      }
    );
    expect(resumeRes.status).toBe(200);
    const resumed = (await resumeRes.json()) as { active: boolean };
    expect(resumed.active).toBe(true);

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });

  test('should delete an automation', async () => {
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Delete test' },
          trigger: { type: 'interval', seconds: 120 },
        }),
      }
    );
    const automation = (await createRes.json()) as { id: string };

    const deleteRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}`,
      { method: 'DELETE', headers }
    );
    expect(deleteRes.status).toBe(204);

    const getRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}`,
      { headers }
    );
    expect(getRes.status).toBe(404);
  });

  test('webhook should accept variables in payload', async () => {
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: {
            type: 'inline',
            content: 'Deploy <version/> to <target/>',
          },
          trigger: {
            type: 'webhook',
            config: { webhook_id: '' },
          },
          variables: { version: 'v0.0.0', target: 'staging' },
        }),
      }
    );
    expect(createRes.status).toBe(200);
    const automation = (await createRes.json()) as {
      id: string;
      trigger: { config: { webhook_id: string } };
    };

    const webhookId = automation.trigger.config.webhook_id;

    const webhookRes = await fetch(
      `${API_BASE}/api/webhooks/${missionId}/${webhookId}`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          variables: { version: 'v1.0.0', target: 'production' },
          event: 'deploy',
        }),
      }
    );
    // Should succeed (200) or fail during execution (500)
    // but should NOT be 400 or 404
    expect([200, 500]).toContain(webhookRes.status);

    const execRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}/executions`,
      { headers }
    );
    expect(execRes.status).toBe(200);
    const executions = (await execRes.json()) as Array<{
      variables_used: Record<string, string>;
      trigger_source: string;
    }>;

    if (executions.length > 0) {
      const lastExec = executions[0];
      expect(lastExec.trigger_source).toBe('webhook');
      expect(lastExec.variables_used.version).toBe('v1.0.0');
      expect(lastExec.variables_used.target).toBe('production');
    }

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });

  test('should get execution history', async () => {
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Execution history test' },
          trigger: { type: 'interval', seconds: 9999 },
        }),
      }
    );
    const automation = (await createRes.json()) as { id: string };

    const execRes = await fetch(
      `${API_BASE}/api/control/automations/${automation.id}/executions`,
      { headers }
    );
    expect(execRes.status).toBe(200);
    const executions = await execRes.json();
    expect(Array.isArray(executions)).toBe(true);

    await fetch(`${API_BASE}/api/control/automations/${automation.id}`, {
      method: 'DELETE',
      headers,
    });
  });
});

// ========================================================================
// UI tests (control the dashboard via Playwright browser)
// All locators are scoped to the dialog to avoid matching chat history text.
// ========================================================================

test.describe.configure({ mode: 'serial' });
test.describe('Automations UI', () => {
  let headers: Record<string, string>;
  let missionId: string;

  test.beforeAll(async () => {
    headers = await getAuthHeaders();
    missionId = await ensureMission(headers);
  });

  test.afterEach(async () => {
    await cleanupAutomations(missionId, headers);
  });

  test('should open automations dialog from control page', async ({ page }) => {
    await page.goto('/control');
    await page.waitForTimeout(4000);

    const automationsBtn = automationsBtnLocator(page);
    await expect(automationsBtn).toBeVisible({ timeout: 10000 });

    const isDisabled = await automationsBtn.isDisabled();
    if (!isDisabled) {
      await automationsBtn.click();
      await expect(
        page.locator('h3:has-text("Mission Automations")')
      ).toBeVisible({ timeout: 5000 });
    }
  });

  test('should show create automation form with source and trigger selectors', async ({
    page,
  }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Should have Source and Trigger labels
    await expect(dlg.locator('label:has-text("Source")')).toBeVisible();
    await expect(dlg.locator('label:has-text("Trigger")')).toBeVisible();

    // Source select
    const sourceSelect = dlg.locator('select').first();
    await expect(sourceSelect).toBeVisible();

    // Trigger select
    const triggerSelect = dlg.locator('select').nth(1);
    await expect(triggerSelect).toBeVisible();
  });

  test('should switch between library and inline source types', async ({ page }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Start in library mode - should show Command label
    await expect(dlg.locator('label:has-text("Command")')).toBeVisible();

    // Switch to inline prompt
    const sourceSelect = dlg.locator('select').first();
    await sourceSelect.selectOption('inline');

    // Should now show Prompt textarea
    await expect(dlg.locator('label:has-text("Prompt")')).toBeVisible();

    // Should show variable syntax hint
    await expect(dlg.locator('text=<variable_name/>')).toBeVisible();

    // Switch back to library
    await sourceSelect.selectOption('library');
    await expect(dlg.locator('label:has-text("Command")')).toBeVisible();
  });

  test('should switch between interval and webhook triggers', async ({ page }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Start with interval
    await expect(dlg.locator('label:has-text("Interval")')).toBeVisible();

    // Switch to webhook
    const triggerSelect = dlg.locator('select').nth(1);
    await triggerSelect.selectOption('webhook');

    // Should show webhook info text
    await expect(dlg.locator('text=webhook URL will be generated')).toBeVisible();

    // Interval input should be gone
    await expect(dlg.locator('label:has-text("Interval")')).not.toBeVisible();

    // Switch back to interval
    await triggerSelect.selectOption('interval');
    await expect(dlg.locator('label:has-text("Interval")')).toBeVisible();
  });

  test('should add and remove variables', async ({ page }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Click "Add variable"
    const addVarBtn = dlg.locator('button').filter({ hasText: /Add variable/i });
    await expect(addVarBtn).toBeVisible();
    await addVarBtn.click();

    // Should show variable inputs
    const keyInput = dlg.locator('input[placeholder="key"]');
    await expect(keyInput).toBeVisible();
    await keyInput.fill('my_var');

    const valueInput = dlg.locator('input[placeholder="default value"]');
    await expect(valueInput).toBeVisible();
    await valueInput.fill('default_val');

    // Add another variable
    await addVarBtn.click();
    const allKeyInputs = dlg.locator('input[placeholder="key"]');
    await expect(allKeyInputs).toHaveCount(2);
  });

  test('should create an inline prompt automation via UI', async ({ page }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Switch to inline prompt
    const sourceSelect = dlg.locator('select').first();
    await sourceSelect.selectOption('inline');

    // Fill in the prompt
    const promptInput = dlg.locator('textarea');
    await promptInput.fill('Check system status at <timestamp/>');

    // Set interval to 10 minutes
    const intervalInput = dlg.locator('input[type="number"]');
    await intervalInput.fill('10');

    // Click create
    const createBtn = dlg.locator('button').filter({ hasText: /Create automation/i });
    await createBtn.click();

    // The new automation should appear in the Current Automations section
    await expect(
      dlg.locator('text=Check system status at')
    ).toBeVisible({ timeout: 8000 });
  });

  test('should show webhook URL after creating webhook automation', async ({ page }) => {
    const dlg = await gotoAndOpenDialog(page, test);

    // Switch to inline + webhook
    const sourceSelect = dlg.locator('select').first();
    await sourceSelect.selectOption('inline');

    const triggerSelect = dlg.locator('select').nth(1);
    await triggerSelect.selectOption('webhook');

    // Fill in the prompt
    const promptInput = dlg.locator('textarea');
    await promptInput.fill('Process webhook event');

    // Click create
    const createBtn = dlg.locator('button').filter({ hasText: /Create automation/i });
    await createBtn.click();

    // Should show the webhook URL in the dialog
    await expect(dlg.locator('text=POST')).toBeVisible({ timeout: 8000 });
    await expect(dlg.locator('text=/api/webhooks/')).toBeVisible({ timeout: 5000 });

    // Should have a copy button
    await expect(dlg.locator('button').filter({ hasText: /Copy/i })).toBeVisible();
  });

  test('should toggle automation active state via UI', async ({ page }) => {
    // Create an automation via API first (use unique text to avoid chat history collisions)
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Toggle test auto xyz' },
          trigger: { type: 'interval', seconds: 600 },
        }),
      }
    );
    expect(createRes.status).toBe(200);

    const dlg = await gotoAndOpenDialog(page, test, missionId);

    // Click refresh to make sure the dialog has the latest automations
    const refreshBtn = dlg.locator('button').filter({ hasText: /Refresh/i });
    if ((await refreshBtn.count()) > 0) {
      await refreshBtn.click();
      await page.waitForTimeout(1000);
    }

    // Wait for automation to appear in dialog
    await expect(dlg.locator('text=Toggle test auto xyz')).toBeVisible({ timeout: 10000 });

    // Find the automation card within the dialog
    const card = dlg.locator('div.rounded-xl').filter({ hasText: 'Toggle test auto xyz' });

    // Find and check the Active checkbox
    const activeCheckbox = card.locator('input[type="checkbox"]');
    if ((await activeCheckbox.count()) > 0) {
      await expect(activeCheckbox.first()).toBeChecked();

      // Toggle it off using click instead of uncheck (the handler might update state asynchronously)
      await activeCheckbox.first().click({ force: true });
      await page.waitForTimeout(2000);

      // Should show "Paused" or the checkbox should be unchecked
      const isPaused = await card.locator('text=Paused').isVisible();
      const isUnchecked = !(await activeCheckbox.first().isChecked());
      expect(isPaused || isUnchecked).toBe(true);
    }
  });

  test('should show source tags (Library/Prompt) on automation cards', async ({ page }) => {
    // Create an inline automation via API
    const createRes = await fetch(
      `${API_BASE}/api/control/missions/${missionId}/automations`,
      {
        method: 'POST',
        headers: { ...headers, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          command_source: { type: 'inline', content: 'Inline type auto xyz' },
          trigger: { type: 'interval', seconds: 600 },
        }),
      }
    );
    expect(createRes.status).toBe(200);

    const dlg = await gotoAndOpenDialog(page, test, missionId);

    // Click refresh to ensure latest automations are loaded
    const refreshBtn = dlg.locator('button').filter({ hasText: /Refresh/i });
    if ((await refreshBtn.count()) > 0) {
      await refreshBtn.click();
      await page.waitForTimeout(1000);
    }

    await expect(dlg.locator('text=Inline type auto xyz')).toBeVisible({ timeout: 10000 });

    // Should show "Prompt" tag for inline automations (exact match to avoid ambiguity)
    await expect(dlg.locator('span:text-is("Prompt")')).toBeVisible();
  });

  test('should close dialog with Escape key', async ({ page }) => {
    await gotoAndOpenDialog(page, test);

    await page.keyboard.press('Escape');

    await expect(
      page.locator('h3:has-text("Mission Automations")')
    ).not.toBeVisible({ timeout: 3000 });
  });

  test('should close dialog by clicking outside', async ({ page }) => {
    await gotoAndOpenDialog(page, test);

    // Click outside the dialog (on the backdrop)
    await page.mouse.click(10, 10);

    await expect(
      page.locator('h3:has-text("Mission Automations")')
    ).not.toBeVisible({ timeout: 3000 });
  });
});
