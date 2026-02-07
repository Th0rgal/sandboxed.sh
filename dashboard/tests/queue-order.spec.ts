import { test, expect } from '@playwright/test';

const missionId = '11111111-1111-1111-1111-111111111111';

test.describe('Queue Ordering', () => {
  test('assistant response renders before queued messages', async ({ page }) => {
    const sse = [
      'event: status',
      `data: ${JSON.stringify({ state: 'running', queue_len: 1, mission_id: missionId })}`,
      '',
      'event: user_message',
      `data: ${JSON.stringify({ id: 'u1', content: 'First message', queued: false, mission_id: missionId })}`,
      '',
      'event: user_message',
      `data: ${JSON.stringify({ id: 'u2', content: 'Second message', queued: true, mission_id: missionId })}`,
      '',
      'event: assistant_message',
      `data: ${JSON.stringify({ id: 'a1', content: 'Reply to first', success: true, mission_id: missionId })}`,
      '',
      'event: status',
      `data: ${JSON.stringify({ state: 'waiting_for_tool', queue_len: 0, mission_id: missionId })}`,
      '',
    ].join('\n');

    await page.addInitScript((payload: { sse: string }) => {
      localStorage.setItem('settings', JSON.stringify({ apiUrl: 'http://localhost:3099' }));
      const originalError = console.error.bind(console);
      console.error = (...args: unknown[]) => {
        if (typeof args[0] === 'string' && args[0].includes('[control:sse]')) {
          return;
        }
        originalError(...args);
      };
      const originalFetch = window.fetch.bind(window);
      window.fetch = async (input, init) => {
        const url = typeof input === 'string' ? input : input.url;
        if (url.includes('/api/control/stream')) {
          const encoder = new TextEncoder();
          const stream = new ReadableStream({
            start(controller) {
              setTimeout(() => {
                controller.enqueue(encoder.encode(payload.sse));
              }, 250);
            },
          });
          return new Response(stream, {
            status: 200,
            headers: {
              'Content-Type': 'text/event-stream',
              'Cache-Control': 'no-cache',
            },
          });
        }
        return originalFetch(input, init);
      };
    }, { sse });
    await page.route('**/api/**', async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;

      if (path.endsWith('/api/control/missions/current')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            id: missionId,
            status: 'active',
            title: 'Test Mission',
            history: [],
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          }),
        });
        return;
      }

      if (path.endsWith('/api/control/missions')) {
        await route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
        return;
      }

      if (path.includes('/api/control/missions/') && path.endsWith('/events')) {
        await route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
        return;
      }

      if (path.includes('/api/control/missions/') && !path.endsWith('/events')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            id: missionId,
            status: 'active',
            title: 'Test Mission',
            history: [],
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          }),
        });
        return;
      }

      if (path.endsWith('/api/control/running_missions')) {
        await route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
        return;
      }

      if (path.endsWith('/api/control/queue')) {
        await route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
        return;
      }

      if (path.endsWith('/api/control/progress')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            total_subtasks: 0,
            completed_subtasks: 0,
            current_subtask: null,
            current_depth: 0,
          }),
        });
        return;
      }

      if (path.endsWith('/api/workspaces')) {
        await route.fulfill({ status: 200, contentType: 'application/json', body: '[]' });
        return;
      }

      if (path.endsWith('/api/health')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ status: 'ok' }),
        });
        return;
      }

      if (path.endsWith('/api/settings')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({}),
        });
        return;
      }

      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: '[]',
      });
    });

    await page.goto('/control');

    const assistant = page.getByText('Reply to first');
    const queued = page.locator('p.whitespace-pre-wrap', { hasText: 'Second message' }).first();
    await expect(assistant).toBeVisible();
    await expect(queued).toBeVisible();

    const assistantBox = await assistant.boundingBox();
    const queuedBox = await queued.boundingBox();
    expect(assistantBox).not.toBeNull();
    expect(queuedBox).not.toBeNull();
    expect(assistantBox!.y).toBeLessThan(queuedBox!.y);

  });
});
