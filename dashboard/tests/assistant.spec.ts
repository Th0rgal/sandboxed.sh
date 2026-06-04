import { test, expect } from '@playwright/test';

test.describe('Assistant page', () => {
  test.beforeEach(async ({ page }) => {
    await page.route('**/api/health', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ auth_required: false, auth_mode: 'disabled' }),
      });
    });
    await page.route('**/api/system/components', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          components: [
            {
              name: 'assistant_mcp',
              version: '0.1.0',
              installed: true,
              update_available: null,
              path: '/usr/local/bin/assistant-mcp',
              status: 'ok',
            },
            {
              name: 'hermes_assistant',
              version: null,
              installed: true,
              update_available: null,
              path: '/etc/systemd/system/hermes-assistant-dev.service',
              status: 'ok',
            },
          ],
        }),
      });
    });
  });

  test('is a top-level navigation destination', async ({ page }) => {
    await page.goto('/');

    const sidebar = page.locator('aside');
    await sidebar.getByRole('link', { name: 'Assistant', exact: true }).click();

    await expect(page).toHaveURL(/\/assistant/);
    await expect(page.getByRole('heading', { name: 'Assistant', exact: true })).toBeVisible();
    await expect(page.getByText('assistant-mcp 0.1.0')).toBeVisible();
    await expect(page.getByText('Hermes runtime active')).toBeVisible();
    await expect(page.getByRole('button', { name: /Add Gateway/i }).first()).toBeVisible();

    await page.getByRole('button', { name: /Add Gateway/i }).first().click();
    await expect(page.getByRole('dialog', { name: 'Add Assistant Gateway' })).toBeVisible();
    await expect(page.getByText('Hermes runtime is active. Do not add a compatibility gateway for a bot token Hermes already owns.')).toBeVisible();
  });

  test('shows handoff warnings when Hermes bridge and runtime are unavailable', async ({ page }) => {
    await page.route('**/api/system/components', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          components: [
            {
              name: 'assistant_mcp',
              version: null,
              installed: false,
              update_available: null,
              path: null,
              status: 'missing',
            },
            {
              name: 'hermes_assistant',
              version: null,
              installed: false,
              update_available: null,
              path: null,
              status: 'not_installed',
            },
          ],
        }),
      });
    });

    await page.goto('/assistant');

    await expect(page.getByText('assistant-mcp not ready')).toBeVisible();
    await expect(page.getByText('Install assistant-mcp before handing mission control to Hermes.')).toBeVisible();
    await expect(page.getByText('Assistant bridge is not ready')).toBeVisible();
    await expect(page.getByText('Hermes runtime not installed')).toBeVisible();
    await expect(page.getByText('Install hermes-assistant-dev.service before moving webhook ownership.')).toBeVisible();

    await page.getByRole('button', { name: /Add Gateway/i }).first().click();
    await expect(page.getByRole('dialog', { name: 'Add Assistant Gateway' })).toBeVisible();
    await expect(page.getByText('This compatibility gateway registers the Telegram webhook until Hermes owns the bot.')).toBeVisible();
  });

  test('keeps the old Telegram settings route as a redirect', async ({ page }) => {
    await page.goto('/settings/telegram');

    await expect(page).toHaveURL(/\/assistant/);
    await expect(page.getByRole('heading', { name: 'Assistant', exact: true })).toBeVisible();
  });

  test('labels gateway controls and details with Assistant wording', async ({ page }) => {
    await page.route('**/api/**', async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify(body),
        });

      if (path === '/api/health') {
        return json({ auth_required: false, auth_mode: 'disabled' });
      }
      if (path === '/api/system/components') {
        return json({
          components: [
            {
              name: 'assistant_mcp',
              version: '0.1.0',
              installed: true,
              update_available: null,
              path: '/usr/local/bin/assistant-mcp',
              status: 'ok',
            },
            {
              name: 'hermes_assistant',
              version: null,
              installed: true,
              update_available: null,
              path: '/etc/systemd/system/hermes-assistant-dev.service',
              status: 'ok',
            },
          ],
        });
      }
      if (path === '/api/control/assistant/gateways') {
        return json([
          {
            id: 'bot-1',
            mission_id: 'mission-1',
            bot_username: 'hermes_devbot',
            allowed_chat_ids: [],
            trigger_mode: 'mention_or_dm',
            active: true,
            instructions: null,
            auto_create_missions: true,
            default_backend: 'claudecode',
            default_model_override: null,
            default_model_effort: null,
            default_workspace_id: null,
            default_config_profile: null,
            default_agent: null,
            created_at: '2026-05-28T12:00:00Z',
            updated_at: '2026-05-28T12:00:00Z',
          },
        ]);
      }
      if (path === '/api/backends') {
        return json([{ id: 'claudecode', name: 'Claude Code', enabled: true, settings: {} }]);
      }
      if (path === '/api/workspaces' || path === '/api/control/missions' || path === '/api/library/config-profile') {
        return json([]);
      }
      if (path === '/api/providers') {
        return json({ providers: [] });
      }
      if (path === '/api/providers/backend-models') {
        return json({ backends: {} });
      }
      if (path.startsWith('/api/control/assistant/gateways/bot-1/')) {
        return json([]);
      }

      return route.continue();
    });

    await page.goto('/assistant');

    await expect(page.getByLabel('Assistant gateways').getByText('@hermes_devbot')).toBeVisible();
    await expect(page.getByLabel('Recent assistant activity').getByText('@hermes_devbot')).toBeVisible();
    await expect(page.getByText('Compatibility gateway still active')).toBeVisible();
    await expect(page.getByText('Hermes runtime is active while 1 compatibility gateway remains active. Use Adopt on the matching gateway to copy the existing token into Hermes and stop the legacy webhook.')).toBeVisible();
    await expect(page.getByRole('link', { name: 'Review gateways' })).toHaveAttribute('href', '#assistant-gateways');
    await expect(page.getByText('Compatibility webhook')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Edit @hermes_devbot' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Deactivate @hermes_devbot' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Delete @hermes_devbot' })).toBeVisible();

    await page.getByRole('button', { name: 'Edit @hermes_devbot' }).click();
    await expect(page.getByRole('dialog', { name: 'Edit @hermes_devbot' })).toBeVisible();
    await expect(page.getByText('Hermes runtime is active. Keep this compatibility gateway inactive for bot tokens already moved to Hermes.')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();

    await page.getByRole('button', { name: 'Expand @hermes_devbot details' }).click();

    await expect(page.getByText('No conversations yet. Message the connected gateway to start one.')).toBeVisible();
    await page.getByRole('button', { name: 'Actions' }).click();
    await expect(page.getByText('No gateway actions recorded yet.')).toBeVisible();
    await page.getByRole('button', { name: 'Scheduled' }).click();
    await expect(page.getByText('No scheduled messages for this gateway.')).toBeVisible();
    await page.getByRole('button', { name: 'Memory' }).click();
    await expect(page.getByPlaceholder('Search structured memory...')).toBeVisible();
  });
});
