import { test, expect, type Page, type Route } from '@playwright/test';

// Tests for the collapsible boss-row feature in Cmd+K. Bosses with grouped
// workers are collapsed by default; a chevron toggles them; a worker-count
// pill announces hidden depth; an active search auto-expands any boss whose
// workers match the query.

const BOSS_ID = '11111111-1111-4111-8111-111111111111';
const WORKER_IDS = [
  '22222222-2222-4222-8222-222222222221',
  '22222222-2222-4222-8222-222222222222',
  '22222222-2222-4222-8222-222222222223',
];

async function fulfillJson(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: 'application/json',
    headers: { 'Access-Control-Allow-Origin': '*' },
    body: JSON.stringify(body),
  });
}

function buildBoss(status: 'active' | 'completed' = 'active') {
  return {
    id: BOSS_ID,
    title: 'Concrete Audit Model and Proofs',
    short_description: 'Coordinating projection proofs across worker missions',
    status,
    workspace_id: 'ws-1',
    workspace_name: 'dumbcontracts',
    backend: 'codex',
    created_at: '2026-05-22T00:00:00Z',
    updated_at: '2026-05-22T00:00:00Z',
    history: [],
    resumable: false,
  };
}

function buildWorker(i: number) {
  return {
    id: WORKER_IDS[i],
    title: `Worker ${i + 1} — token/Permit2 reduction`,
    short_description: `Reduce Bridge axioms in worker ${i + 1}`,
    status: 'active' as const,
    workspace_id: 'ws-1',
    workspace_name: 'dumbcontracts',
    backend: 'codex',
    created_at: '2026-05-22T00:00:00Z',
    updated_at: '2026-05-22T00:00:00Z',
    history: [],
    resumable: false,
    parent_mission_id: BOSS_ID,
  };
}

async function mockBossWithWorkers(
  page: Page,
  options: { bossStatus?: 'active' | 'completed'; running?: Array<{ mission_id: string; state: string; queue_len: number }> } = {}
) {
  const bossStatus = options.bossStatus ?? 'active';
  const running =
    options.running ??
    [
      { mission_id: BOSS_ID, state: 'running', queue_len: 0 },
      ...WORKER_IDS.map((id) => ({ mission_id: id, state: 'running' as const, queue_len: 0 })),
    ];

  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;

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

    if (path === '/api/health') {
      await fulfillJson(route, { auth_required: false, auth_mode: 'disabled', max_iterations: 50 });
      return;
    }
    if (path === '/api/control/missions') {
      await fulfillJson(route, [buildBoss(bossStatus), ...WORKER_IDS.map((_, i) => buildWorker(i))]);
      return;
    }
    if (path === '/api/control/missions/current') {
      await fulfillJson(route, null);
      return;
    }
    if (path === '/api/control/running') {
      await fulfillJson(route, running);
      return;
    }
    if (path === '/api/control/missions/search') {
      const q = (url.searchParams.get('q') ?? '').toLowerCase();
      const all = [buildBoss(bossStatus), ...WORKER_IDS.map((_, i) => buildWorker(i))];
      const matches = all
        .filter((m) => m.title.toLowerCase().includes(q) || m.short_description.toLowerCase().includes(q))
        .map((mission, idx) => ({ mission, relevance_score: 100 - idx }));
      await fulfillJson(route, matches);
      return;
    }
    if (path === '/api/workspaces') {
      await fulfillJson(route, []);
      return;
    }
    if (path === '/api/control/queue' || path === '/api/desktop/sessions') {
      await fulfillJson(route, []);
      return;
    }
    if (path === '/api/control/progress') {
      await fulfillJson(route, { run_state: 'running', queue_len: 0 });
      return;
    }

    await fulfillJson(route, {});
  });
}

async function openCmdK(page: Page) {
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  await page.keyboard.press('Meta+k');
  await page.waitForSelector('input[placeholder="Search missions..."]');
}

async function countVisibleWorkers(page: Page) {
  return page.evaluate((bossId) => {
    const dialog = document.querySelector('div.z-50');
    if (!dialog) return 0;
    let count = 0;
    for (const id of [
      '22222222-2222-4222-8222-222222222221',
      '22222222-2222-4222-8222-222222222222',
      '22222222-2222-4222-8222-222222222223',
    ]) {
      if (id === bossId) continue;
      if (dialog.querySelector(`a[href*="${id}"]`)) count += 1;
    }
    return count;
  }, BOSS_ID);
}

test.describe('Cmd+K mission switcher collapse', () => {
  test('boss rows collapsed by default with chevron and count pill', async ({ page }) => {
    await mockBossWithWorkers(page);
    await openCmdK(page);

    // Boss row visible
    await expect(page.locator(`a[href*="${BOSS_ID}"]`).first()).toBeVisible();

    // Workers hidden by default
    expect(await countVisibleWorkers(page)).toBe(0);

    // Chevron is present on the boss row, in collapsed state
    const chevron = page.locator(`button[aria-label="Expand workers"]`).first();
    await expect(chevron).toBeVisible();
    await expect(chevron).toHaveAttribute('aria-expanded', 'false');

    // Worker count pill announces hidden depth
    const pill = page.locator('span[title*="hidden worker"]').first();
    await expect(pill).toBeVisible();
    await expect(pill).toContainText('3');
  });

  test('groups workers under a recent boss that is not running', async ({ page }) => {
    await mockBossWithWorkers(page, { bossStatus: 'completed', running: [] });
    await openCmdK(page);

    await expect(page.locator(`a[href*="${BOSS_ID}"]`).first()).toBeVisible();
    expect(await countVisibleWorkers(page)).toBe(0);

    const chevron = page.locator(`button[aria-label="Expand workers"]`).first();
    await expect(chevron).toBeVisible();
    await expect(chevron).toHaveAttribute('aria-expanded', 'false');
    await expect(page.locator('span[title*="hidden worker"]').first()).toContainText('3');

    await chevron.click();
    expect(await countVisibleWorkers(page)).toBe(3);
  });

  test('clicking chevron toggles worker visibility without opening the mission', async ({ page }) => {
    await mockBossWithWorkers(page);
    await openCmdK(page);

    await page.locator(`button[aria-label="Expand workers"]`).first().click();

    // Workers now visible
    expect(await countVisibleWorkers(page)).toBe(3);
    // Pill goes away once expanded
    await expect(page.locator('span[title*="hidden worker"]')).toHaveCount(0);
    // Chevron flipped state
    await expect(page.locator(`button[aria-label="Collapse workers"]`).first()).toHaveAttribute(
      'aria-expanded',
      'true'
    );
    // The URL did NOT change to the boss page — chevron click must not bubble
    // to the row's <a> handler.
    await expect(page).toHaveURL(/\/$/);

    // Collapse again
    await page.locator(`button[aria-label="Collapse workers"]`).first().click();
    expect(await countVisibleWorkers(page)).toBe(0);
  });

  test('search auto-expands bosses whose workers match', async ({ page }) => {
    await mockBossWithWorkers(page);
    await openCmdK(page);

    // Workers hidden initially
    expect(await countVisibleWorkers(page)).toBe(0);

    // Search for text that ONLY appears in workers, not in the boss title.
    await page.getByPlaceholder('Search missions...').fill('Permit2');
    await page.waitForTimeout(300);

    // The boss may or may not still show (no match on its own text), but
    // matching workers must be visible.
    expect(await countVisibleWorkers(page)).toBeGreaterThan(0);

    // Clear the search — collapse state should return to default (hidden).
    await page.getByPlaceholder('Search missions...').fill('');
    await page.waitForTimeout(150);
    expect(await countVisibleWorkers(page)).toBe(0);
  });

  test('keyboard: ArrowRight expands, ArrowLeft collapses', async ({ page }) => {
    await mockBossWithWorkers(page);
    await openCmdK(page);

    // Boss is selectedIndex=0 by default (it's the first row).
    expect(await countVisibleWorkers(page)).toBe(0);

    await page.keyboard.press('ArrowRight');
    await expect(page.locator(`button[aria-label="Collapse workers"]`).first()).toBeVisible();
    expect(await countVisibleWorkers(page)).toBe(3);

    await page.keyboard.press('ArrowLeft');
    await expect(page.locator(`button[aria-label="Expand workers"]`).first()).toBeVisible();
    expect(await countVisibleWorkers(page)).toBe(0);
  });
});
