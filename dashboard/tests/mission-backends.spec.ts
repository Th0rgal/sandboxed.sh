import { test, expect, APIRequestContext } from '@playwright/test';

const API_BASE = process.env.OPEN_AGENT_API_BASE || 'http://95.216.112.253:3000';

async function waitForDownload(
  request: APIRequestContext,
  path: string,
  timeoutMs = 60_000
): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  const url = `${API_BASE}/api/fs/download?path=${encodeURIComponent(path)}`;

  while (Date.now() < deadline) {
    const res = await request.get(url);
    if (res.ok()) {
      return await res.text();
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }

  throw new Error(`Timed out waiting for ${path}`);
}

async function getHostWorkspace(request: APIRequestContext) {
  const workspacesRes = await request.get(`${API_BASE}/api/workspaces`);
  expect(workspacesRes.ok()).toBeTruthy();
  const workspaces = (await workspacesRes.json()) as Array<{
    id: string;
    workspace_type?: string;
    path: string;
  }>;
  return (
    workspaces.find((ws) => ws.id === '00000000-0000-0000-0000-000000000000') ||
    workspaces.find((ws) => ws.workspace_type === 'host') ||
    null
  );
}

test.describe('Mission backend configs', () => {
  test.setTimeout(180_000);

  test('new mission dialog shows spark-local profile agents', async ({ page, request }) => {
    const profilesRes = await request.get(`${API_BASE}/api/library/config-profile`);
    expect(profilesRes.ok()).toBeTruthy();
    const profiles = (await profilesRes.json()) as Array<{ name: string }>;
    const sparkProfile = profiles.find((profile) => profile.name === 'spark-local');
    expect(sparkProfile).toBeTruthy();

    const hostWorkspace = await getHostWorkspace(request);
    expect(hostWorkspace).toBeTruthy();

    const updateRes = await request.put(`${API_BASE}/api/workspaces/${hostWorkspace!.id}`, {
      data: { config_profile: 'spark-local' },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(updateRes.ok()).toBeTruthy();

    const settingsRes = await request.get(
      `${API_BASE}/api/library/config-profile/spark-local/opencode/settings`
    );
    expect(settingsRes.ok()).toBeTruthy();
    const settings = (await settingsRes.json()) as { agents?: Record<string, unknown> | unknown[] };
    const agentNames = Array.isArray(settings.agents)
      ? settings.agents
          .map((entry) => {
            if (typeof entry === 'string') return entry;
            if (entry && typeof entry === 'object') {
              const obj = entry as { name?: unknown; id?: unknown };
              if (typeof obj.name === 'string') return obj.name;
              if (typeof obj.id === 'string') return obj.id;
            }
            return null;
          })
          .filter((name): name is string => Boolean(name))
      : settings.agents && typeof settings.agents === 'object'
      ? Object.keys(settings.agents)
      : [];

    expect(agentNames.length).toBeGreaterThan(0);
    const sampleAgent = agentNames[0];

    await page.addInitScript((base) => {
      localStorage.setItem('settings', JSON.stringify({ apiUrl: base }));
    }, API_BASE);

    await page.goto('/control');
    await page.getByRole('button', { name: 'New Mission' }).click();
    await page.getByText('Create New Mission').waitFor({ timeout: 10_000 });

    const agentSelect = page
      .locator('label:has-text("Agent")')
      .locator('..')
      .locator('select');
    await expect(agentSelect.locator('option', { hasText: new RegExp(sampleAgent, 'i') })).toHaveCount(1, {
      timeout: 10_000,
    });
  });

  test('claude and opencode missions emit configs on remote host workspace', async ({ page, request }) => {
    const hostWorkspace = await getHostWorkspace(request);
    if (!hostWorkspace) {
      test.skip(true, 'No host workspace found on remote backend');
      return;
    }

    await page.addInitScript((base) => {
      localStorage.setItem('settings', JSON.stringify({ apiUrl: base }));
    }, API_BASE);

    await page.goto('/control');
    await page.waitForTimeout(2000);

    const backends: Array<{ backend: 'claudecode' | 'opencode'; prompt: string; check: (dir: string) => Promise<void> }> = [
      {
        backend: 'claudecode',
        prompt: 'Generate Claude config',
        check: async (dir) => {
          const settingsPath = `${dir}/.claude/settings.local.json`;
          const settings = await waitForDownload(request, settingsPath);
          expect(settings).toContain('mcpServers');
        },
      },
      {
        backend: 'opencode',
        prompt: 'Ping',
        check: async (dir) => {
          const opencodePath = `${dir}/.opencode/opencode.json`;
          const rootPath = `${dir}/opencode.json`;
          const opencode = await waitForDownload(request, opencodePath);
          const root = await waitForDownload(request, rootPath);
          expect(opencode).toContain('mcp');
          expect(root).toContain('mcp');
        },
      },
    ];

    for (const { backend, prompt, check } of backends) {
      const missionTitle = `pw-${backend}-${Date.now()}`;
      const missionRes = await request.post(`${API_BASE}/api/control/missions`, {
        data: {
          title: missionTitle,
          workspace_id: hostWorkspace.id,
          backend,
        },
        headers: { 'Content-Type': 'application/json' },
      });
      expect(missionRes.ok()).toBeTruthy();
      const mission = (await missionRes.json()) as { id: string };

      const loadRes = await request.post(`${API_BASE}/api/control/missions/${mission.id}/load`);
      expect(loadRes.ok()).toBeTruthy();

      const messageRes = await request.post(`${API_BASE}/api/control/message`, {
        data: { content: prompt, mission_id: mission.id },
        headers: { 'Content-Type': 'application/json' },
      });
      expect(messageRes.ok()).toBeTruthy();

      await check(hostWorkspace.path);
    }
  });
});
