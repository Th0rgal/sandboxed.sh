import { test, expect } from '@playwright/test';
import fs from 'fs';
import path from 'path';

test.describe('Minecraft workspace mission', () => {
  test.setTimeout(900_000);

  test('creates and builds minecraft workspace', async ({ page, request }) => {
    const apiBase = process.env.OPEN_AGENT_API_BASE || 'http://95.216.112.253:3000';
    const runId = Date.now();
    const workspaceName = `mc-ws-${runId}`;
    const missionTitle = `mc-mission-${runId}`;

    const templatePath = path.resolve(
      __dirname,
      '..',
      '..',
      'library-template',
      'workspace-template',
      'minecraft-neoforge.json'
    );
    const skillPath = path.resolve(
      __dirname,
      '..',
      '..',
      'library-template',
      'skill',
      'minecraft-workspace',
      'SKILL.md'
    );

    const template = JSON.parse(fs.readFileSync(templatePath, 'utf-8')) as {
      name: string;
      skills?: string[];
    };
    const skillContent = fs.readFileSync(skillPath, 'utf-8');
    const skillName = template.skills?.[0] || 'minecraft-workspace';

    const skillRes = await request.put(
      `${apiBase}/api/library/skills/${skillName}`,
      {
        data: { content: skillContent },
        headers: { 'Content-Type': 'application/json' },
      }
    );
    expect(skillRes.ok()).toBeTruthy();

    const templateRes = await request.put(
      `${apiBase}/api/library/workspace-template/${template.name}`,
      {
        data: template,
        headers: { 'Content-Type': 'application/json' },
      }
    );
    expect(templateRes.ok()).toBeTruthy();

    await page.addInitScript((base) => {
      localStorage.setItem('settings', JSON.stringify({ apiUrl: base }));
    }, apiBase);

    await page.goto('/workspaces');

    await page.getByRole('button', { name: /New Workspace/i }).click();
    await page.getByPlaceholder('my-workspace').fill(workspaceName);

    const templateSelect = page.getByText('Template').locator('..').locator('select');
    await templateSelect.selectOption(template.name);

    await page.getByRole('button', { name: /^Create$/i }).click();

    await expect(
      page.getByRole('heading', { name: workspaceName })
    ).toBeVisible({ timeout: 30_000 });

    await page.getByRole('heading', { name: workspaceName }).click();
    await page.getByRole('button', { name: /Build/i }).click();

    let workspaceId = '';
    let workspacePath = '';
    const deadline = Date.now() + 15 * 60 * 1000;

    while (Date.now() < deadline) {
      const res = await request.get(`${apiBase}/api/workspaces`);
      expect(res.ok()).toBeTruthy();
      const workspaces: Array<{
        id: string;
        name: string;
        status: string;
        path: string;
        error_message?: string | null;
      }> = await res.json();
      const ws = workspaces.find((w) => w.name === workspaceName);
      if (ws) {
        workspaceId = ws.id;
        workspacePath = ws.path;
        if (ws.status === 'ready') {
          break;
        }
        if (ws.status === 'error') {
          throw new Error(ws.error_message || 'Workspace build failed');
        }
      }
      await page.waitForTimeout(5000);
    }

    expect(workspaceId).not.toEqual('');
    expect(workspacePath).not.toEqual('');

    await page.screenshot({
      path: test.info().outputPath(`${workspaceName}-workspace.png`),
      fullPage: true,
    });

    const missionRes = await request.post(`${apiBase}/api/control/missions`, {
      data: { title: missionTitle, workspace_id: workspaceId, agent: 'build' },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(missionRes.ok()).toBeTruthy();
    const mission = (await missionRes.json()) as { id?: string };
    const missionId = mission.id;
    expect(missionId).toBeTruthy();

    await request.get(`${apiBase}/api/control/missions/${missionId}/load`);

    const prompt = [
      'Start the desktop session and ensure DISPLAY is set.',
      'Run start-mc-demo with MC_DEMO_DETACH=true MC_DEMO_CONNECT=true MC_DEMO_CAPTURE=true MC_SCREENSHOT_PATH="screenshots/mc-demo.png" so it auto-joins demo.oraxen.com and saves a screenshot.',
      'After it finishes, reply with the screenshot path (kept under the mission workspace screenshots folder).',
    ].join(' ');

    const msgRes = await request.post(`${apiBase}/api/control/message`, {
      data: { content: prompt },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(msgRes.ok()).toBeTruthy();

    await page.goto('/control');
    await page.waitForTimeout(5000);
    await page.screenshot({
      path: test.info().outputPath(`${workspaceName}-control.png`),
      fullPage: true,
    });
  });
});
