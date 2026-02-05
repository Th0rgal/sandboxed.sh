import { test, expect } from '@playwright/test';
import fs from 'fs/promises';
import path from 'path';

async function waitForFile(filePath: string, timeoutMs = 10000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      await fs.access(filePath);
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
  throw new Error(`Timed out waiting for file: ${filePath}`);
}

test('claude code mission generates correct config', async ({ page }) => {
  const workspacesRes = await page.request.get('/api/workspaces');
  if (!workspacesRes.ok()) {
    test.skip(true, 'Backend API not available');
  }
  const workspaces = await workspacesRes.json();
  const hostWorkspace =
    workspaces.find((ws: { id: string }) => ws.id === '00000000-0000-0000-0000-000000000000') ||
    workspaces.find((ws: { workspace_type: string }) => ws.workspace_type === 'host');

  if (!hostWorkspace) {
    test.skip(true, 'No host workspace found');
  }

  const missionRes = await page.request.post('/api/control/missions', {
    data: {
      title: `pw-claude-${Date.now()}`,
      workspace_id: hostWorkspace.id,
      backend: 'claudecode',
    },
  });
  if (!missionRes.ok()) {
    test.skip(true, `Mission create failed (${missionRes.status()})`);
  }
  const mission = await missionRes.json();

  const messageRes = await page.request.post('/api/control/message', {
    data: { content: 'Generate Claude config', mission_id: mission.id },
  });
  if (!messageRes.ok()) {
    test.skip(true, 'Mission execution not available');
  }

  const missionDir = hostWorkspace.path;
  const claudeSettings = path.join(missionDir, '.claude', 'settings.local.json');

  await waitForFile(claudeSettings);
  const settingsContent = await fs.readFile(claudeSettings, 'utf8');
  expect(settingsContent).toContain('mcpServers');

  const skillsRes = await page.request.get('/api/library/skills');
  if (skillsRes.ok()) {
    const skills = await skillsRes.json();
    if (skills.length > 0) {
      const claudeMd = path.join(missionDir, 'CLAUDE.md');
      await waitForFile(claudeMd);
      const claudeContent = await fs.readFile(claudeMd, 'utf8');
      expect(claudeContent).toContain('Project Context');
    }
  }
});
