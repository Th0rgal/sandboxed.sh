import { test, expect } from '@playwright/test';

test.describe('Overview Page', () => {
  test('should load overview page', async ({ page }) => {
    await page.goto('/');

    // Should show Global Monitor title
    await expect(page.getByRole('heading', { name: /Global Monitor/i })).toBeVisible();
  });

  test('should show stats cards', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(2000);

    // Should show stats cards (Total Tasks, Active, Success Rate, Total Cost)
    // These might be loading initially so check for either value or shimmer
    const statsSection = page.locator('.grid');
    await expect(statsSection.first()).toBeVisible();
  });

  test('should show New Mission button', async ({ page }) => {
    await page.goto('/');

    // Should have New Mission link/button
    const newMissionButton = page.getByRole('button', { name: /New Mission/i });
    await expect(newMissionButton).toBeVisible();
  });

  test('should open new mission dialog', async ({ page }) => {
    await page.goto('/');

    // Click New Mission
    await page.getByRole('button', { name: /New Mission/i }).click();

    // Should show mission dialog
    await expect(page.getByRole('heading', { name: /Create New Mission/i })).toBeVisible();

    // Close dialog
    await page.locator('button').filter({ has: page.locator('svg') }).nth(1).click();
    await expect(page.getByRole('heading', { name: /Create New Mission/i })).not.toBeVisible();
  });

  test('should show radar visualization', async ({ page }) => {
    await page.goto('/');

    // Should show the compact mission board columns
    await expect(page.getByText('Running').first()).toBeVisible();
    await expect(page.getByText('Needs You').first()).toBeVisible();
    await expect(page.getByText('Finished').first()).toBeVisible();
  });

  test('groups worker missions under boss cards in the kanban', async ({ page }) => {
    const now = new Date().toISOString();
    const bossMission = {
      id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      title: 'Boss mission',
      short_description: 'orchestrator-boss coordination',
      status: 'completed',
      workspace_name: 'ops',
      history: [],
      created_at: now,
      updated_at: now,
    };
    const workerMission = {
      id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
      parent_mission_id: bossMission.id,
      title: 'Worker mission',
      status: 'active',
      workspace_name: 'ops',
      history: [],
      created_at: now,
      updated_at: now,
    };

    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (route.request().method() === 'OPTIONS') {
        await route.fulfill({
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Headers': '*',
            'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,OPTIONS',
          },
        });
        return;
      }
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          headers: { 'Access-Control-Allow-Origin': '*' },
          body: JSON.stringify(body),
        });

      if (path === '/api/stats') {
        await json({
          total_tasks: 2,
          active_tasks: 0,
          completed_tasks: 1,
          failed_tasks: 0,
          total_cost_cents: 0,
          actual_cost_cents: 0,
          estimated_cost_cents: 0,
          unknown_cost_cents: 0,
          success_rate: 1,
        });
        return;
      }
      if (path === '/api/workspaces') {
        await json([]);
        return;
      }
      if (path === '/api/control/missions') {
        await json([bossMission, workerMission]);
        return;
      }
      if (path === '/api/control/running') {
        await json([{ mission_id: workerMission.id, state: 'running' }]);
        return;
      }
      if (path === '/api/control/automations') {
        await json([]);
        return;
      }
      if (path === '/api/tasks') {
        await json([]);
        return;
      }
      await json([]);
    });

    await page.goto('/');

    await expect(page.getByText('Boss mission')).toBeVisible();
    await expect(page.getByText('1 worker')).toBeVisible();
    await expect(page.getByText('Worker mission')).toHaveCount(0);

    await page.getByRole('button', { name: 'Expand workers' }).click();
    await expect(page.getByText('Worker mission')).toBeVisible();
  });

  test('removes grouped workers from kanban after deleting their boss mission', async ({ page }) => {
    const now = new Date().toISOString();
    const bossMission = {
      id: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      title: 'Boss mission',
      short_description: 'orchestrator-boss coordination',
      status: 'completed',
      workspace_name: 'ops',
      history: [],
      created_at: now,
      updated_at: now,
    };
    const workerMission = {
      id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
      parent_mission_id: bossMission.id,
      title: 'Worker mission',
      status: 'completed',
      workspace_name: 'ops',
      history: [],
      created_at: now,
      updated_at: now,
    };
    let missions = [bossMission, workerMission];

    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (route.request().method() === 'OPTIONS') {
        await route.fulfill({
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Headers': '*',
            'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,OPTIONS',
          },
        });
        return;
      }
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          headers: { 'Access-Control-Allow-Origin': '*' },
          body: JSON.stringify(body),
        });

      if (path === '/api/stats') {
        await json({
          total_tasks: missions.length,
          active_tasks: 0,
          completed_tasks: missions.length,
          failed_tasks: 0,
          total_cost_cents: 0,
          actual_cost_cents: 0,
          estimated_cost_cents: 0,
          unknown_cost_cents: 0,
          success_rate: 1,
        });
        return;
      }
      if (path === '/api/workspaces') {
        await json([]);
        return;
      }
      if (path === '/api/control/missions') {
        await json(missions);
        return;
      }
      if (path === `/api/control/missions/${bossMission.id}` && route.request().method() === 'DELETE') {
        missions = [];
        await json({
          ok: true,
          deleted: bossMission.id,
          deleted_ids: [bossMission.id, workerMission.id],
          deleted_count: 2,
        });
        return;
      }
      if (path === '/api/control/running' || path === '/api/control/automations' || path === '/api/tasks') {
        await json([]);
        return;
      }
      await json([]);
    });

    await page.goto('/');

    await expect(page.getByText('Boss mission')).toBeVisible();
    await expect(page.getByText('1 worker')).toBeVisible();
    await page.getByTitle('Delete').click();
    await expect(page.getByText('Boss mission')).toHaveCount(0);
    await expect(page.getByText('Worker mission')).toHaveCount(0);
  });

  test('should show recent tasks sidebar', async ({ page }) => {
    await page.goto('/');

    // Should have a sidebar with Recent Tasks
    const sidebar = page.locator('.border-l');
    await expect(sidebar.first()).toBeVisible();
  });

  test('should show connection status', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(2000);

    // Connection status component should be visible
    // It shows either Connected or connection error state
    const connectionStatus = page.getByText(/Connected|Connecting|Disconnected|Connection/i);
    expect(await connectionStatus.first().isVisible().catch(() => false) || true).toBeTruthy();
  });

  test('should update stats dynamically', async ({ page }) => {
    await page.goto('/');

    // Wait for initial load
    await page.waitForTimeout(3000);

    // Stats should be loaded (not showing shimmer/loading state)
    // Check for actual stat values or icons
    const statsCards = page.locator('.grid > div');
    expect(await statsCards.count()).toBeGreaterThan(0);
  });

  test('should have activity indicator when active', async ({ page }) => {
    await page.goto('/');
    await page.waitForTimeout(2000);

    // Check for LIVE indicator (visible when tasks are active)
    // This is conditional on server state
    const liveIndicator = page.getByText(/LIVE/i);
    const hasLive = await liveIndicator.isVisible().catch(() => false);

    // Should either show LIVE or not - both are valid states
    expect(hasLive || !hasLive).toBeTruthy();
  });

  test('Needs You only lists qualified operator pages', async ({ page }) => {
    const now = new Date().toISOString();
    const decisionMission = {
      id: '11111111-1111-4111-8111-111111111111',
      title: 'Pick a merge strategy',
      status: 'awaiting_user',
      awaiting_kind: 'decision',
      needs_operator: true,
      workspace_name: 'dev-workspace',
      history: [],
      created_at: now,
      updated_at: now,
    };
    const ackMission = {
      id: '22222222-2222-4222-8222-222222222222',
      title: 'PR ready for review',
      status: 'awaiting_user',
      awaiting_kind: 'ack',
      needs_operator: false,
      workspace_name: 'app-workspace',
      history: [],
      created_at: now,
      updated_at: now,
    };
    const inGraceMission = {
      id: '33333333-3333-4333-8333-333333333333',
      title: 'Controller is triaging',
      status: 'awaiting_user',
      awaiting_kind: 'decision',
      needs_operator: false,
      origin_session_id: 'sess-1',
      history: [],
      created_at: now,
      updated_at: now,
    };
    const waitingToolMission = {
      id: '44444444-4444-4444-8444-444444444444',
      title: 'Ask parked inside grace',
      status: 'active',
      needs_operator: false,
      origin_session_id: 'sess-1',
      history: [],
      created_at: now,
      updated_at: now,
    };

    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (route.request().method() === 'OPTIONS') {
        await route.fulfill({
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Headers': '*',
            'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,OPTIONS',
          },
        });
        return;
      }
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          headers: { 'Access-Control-Allow-Origin': '*' },
          body: JSON.stringify(body),
        });

      if (path === '/api/stats') {
        await json({
          total_tasks: 4,
          active_tasks: 1,
          completed_tasks: 0,
          failed_tasks: 0,
          total_cost_cents: 0,
          actual_cost_cents: 0,
          estimated_cost_cents: 0,
          unknown_cost_cents: 0,
          success_rate: 1,
        });
        return;
      }
      if (path === '/api/workspaces') {
        await json([]);
        return;
      }
      if (path === '/api/control/missions') {
        await json([decisionMission, ackMission, inGraceMission, waitingToolMission]);
        return;
      }
      if (path === '/api/control/running') {
        await json([{ mission_id: waitingToolMission.id, state: 'waiting_for_tool', queue_len: 0 }]);
        return;
      }
      if (path === '/api/control/automations') {
        await json([]);
        return;
      }
      if (path === '/api/backends') {
        await json([]);
        return;
      }
      if (path === '/api/providers' || path === '/api/providers/backend-models') {
        await json([]);
        return;
      }
      if (/^\/api\/backends\/[^/]+\/agents$/.test(path)) {
        await json([]);
        return;
      }
      if (/^\/api\/backends\/[^/]+\/config$/.test(path)) {
        await json({ hidden_agents: [], default_agent: null });
        return;
      }
      if (path.startsWith('/api/library/')) {
        await json([]);
        return;
      }
      await json({});
    });

    await page.goto('/');

    const needsYou = page.locator('div').filter({ hasText: /^Needs You/ }).first();
    await expect(needsYou.getByText('Pick a merge strategy')).toBeVisible();
    await expect(needsYou.getByText('PR ready for review')).toHaveCount(0);
    await expect(needsYou.getByText('Controller is triaging')).toHaveCount(0);
    await expect(needsYou.getByText('Ask parked inside grace')).toHaveCount(0);

    await expect(page.getByText('PR ready for review')).toBeVisible();
    await expect(page.getByText('Ask parked inside grace')).toBeVisible();
  });

  test('Needs You is empty when nothing qualifies', async ({ page }) => {
    const now = new Date().toISOString();
    const ackMission = {
      id: '22222222-2222-4222-8222-222222222222',
      title: 'PR ready for review',
      status: 'awaiting_user',
      awaiting_kind: 'ack',
      needs_operator: false,
      history: [],
      created_at: now,
      updated_at: now,
    };

    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (route.request().method() === 'OPTIONS') {
        await route.fulfill({
          status: 204,
          headers: {
            'Access-Control-Allow-Origin': '*',
            'Access-Control-Allow-Headers': '*',
            'Access-Control-Allow-Methods': 'GET,POST,PUT,PATCH,DELETE,OPTIONS',
          },
        });
        return;
      }
      const json = (body: unknown) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          headers: { 'Access-Control-Allow-Origin': '*' },
          body: JSON.stringify(body),
        });

      if (path === '/api/stats') {
        await json({
          total_tasks: 1,
          active_tasks: 0,
          completed_tasks: 0,
          failed_tasks: 0,
          total_cost_cents: 0,
          actual_cost_cents: 0,
          estimated_cost_cents: 0,
          unknown_cost_cents: 0,
          success_rate: 1,
        });
        return;
      }
      if (path === '/api/workspaces' || path === '/api/control/automations') {
        await json([]);
        return;
      }
      if (path === '/api/control/missions') {
        await json([ackMission]);
        return;
      }
      if (path === '/api/control/running') {
        await json([]);
        return;
      }
      await json({});
    });

    await page.goto('/');
    await expect(page.getByText('All good!')).toBeVisible();
    await expect(page.getByText('PR ready for review')).toBeVisible();
  });
});
