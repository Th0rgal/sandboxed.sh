import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { SWRConfig } from 'swr';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  getBackendConfig,
  getClaudeCodeConfig,
  getSandboxedConfig,
  getVisibleAgents,
  listBackendAgents,
  listBackendModelOptions,
  listBackends,
  listProviders,
} from '@/lib/api';
import type { Workspace } from '@/lib/api';

import { NewMissionDialog } from './new-mission-dialog';

vi.mock('next/navigation', () => ({
  useRouter: () => ({
    push: vi.fn(),
  }),
}));

vi.mock('@/lib/api', () => ({
  getVisibleAgents: vi.fn().mockResolvedValue([]),
  getSandboxedConfig: vi.fn().mockResolvedValue({ hidden_agents: [] }),
  listBackends: vi.fn().mockResolvedValue([{ id: 'codex', name: 'Codex' }]),
  listBackendAgents: vi.fn().mockResolvedValue([{ id: 'default', name: 'Default' }]),
  getBackendConfig: vi.fn().mockResolvedValue({ enabled: true, cli_available: true }),
  getClaudeCodeConfig: vi.fn().mockResolvedValue({ hidden_agents: [] }),
  listBackendModelOptions: vi.fn().mockResolvedValue({ backends: {} }),
  listProviders: vi.fn().mockResolvedValue({ providers: [] }),
}));

function renderDialog(
  onCreate: Parameters<typeof NewMissionDialog>[0]['onCreate'],
  workspaces: Workspace[] = [],
) {
  return render(
    <SWRConfig value={{ provider: () => new Map() }}>
      <NewMissionDialog workspaces={workspaces} onCreate={onCreate} />
    </SWRConfig>
  );
}

describe('NewMissionDialog', () => {
  beforeEach(() => {
    vi.mocked(getVisibleAgents).mockResolvedValue([]);
    vi.mocked(getSandboxedConfig).mockResolvedValue({ hidden_agents: [] });
    vi.mocked(listBackends).mockResolvedValue([{ id: 'codex', name: 'Codex' }]);
    vi.mocked(listBackendAgents).mockResolvedValue([{ id: 'default', name: 'Default' }]);
    vi.mocked(getBackendConfig).mockResolvedValue({
      id: 'test',
      name: 'Test backend',
      enabled: true,
      settings: {},
      cli_available: true,
    });
    vi.mocked(getClaudeCodeConfig).mockResolvedValue({ hidden_agents: [] });
    vi.mocked(listBackendModelOptions).mockResolvedValue({ backends: {} });
    vi.mocked(listProviders).mockResolvedValue({ providers: [] });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('reserves a new tab synchronously before async mission creation finishes', async () => {
    let resolveCreate: (mission: { id: string }) => void = () => {};
    const createPromise = new Promise<{ id: string }>((resolve) => {
      resolveCreate = resolve;
    });
    const onCreate = vi.fn(() => createPromise);
    const reservedTab = {
      opener: {},
      location: { href: 'about:blank' },
      closed: false,
      close: vi.fn(),
    };
    const openSpy = vi
      .spyOn(window, 'open')
      .mockReturnValue(reservedTab as unknown as Window);

    renderDialog(onCreate);

    fireEvent.click(screen.getByRole('button', { name: /new mission/i }));
    fireEvent.click(await screen.findByRole('button', { name: /new tab/i }));

    expect(openSpy).toHaveBeenCalledTimes(1);
    expect(openSpy).toHaveBeenCalledWith('about:blank', '_blank');
    expect(onCreate).toHaveBeenCalledTimes(1);
    expect(reservedTab.location.href).toBe('about:blank');

    resolveCreate({ id: 'mission-1' });

    await waitFor(() => {
      expect(reservedTab.location.href).toBe('/control?mission=mission-1');
    });
    expect(openSpy).toHaveBeenCalledTimes(1);
  });

  it('uses one host-only canonical Pro option for ChatGPT UI missions', async () => {
    vi.mocked(listBackends).mockResolvedValue([
      { id: 'chatgpt_ui', name: 'ChatGPT UI (experimental)' },
    ]);
    const onCreate = vi.fn().mockResolvedValue({ id: 'chatgpt-mission' });
    const unavailableWorkspace: Workspace = {
      id: 'project-workspace',
      name: 'Project container',
      workspace_type: 'container',
      path: '/containers/project',
      status: 'building',
      error_message: null,
      created_at: '2026-07-25T00:00:00Z',
      skills: [],
      plugins: [],
      env_vars: {},
      config_profile: 'project',
    };

    renderDialog(onCreate, [unavailableWorkspace]);

    fireEvent.click(screen.getByRole('button', { name: /new mission/i }));
    const chatGptOption = await screen.findByRole('option', {
      name: 'ChatGPT Pro web conversation',
    });
    expect(chatGptOption).toHaveValue('chatgpt_ui:');
    expect(
      screen.queryByRole('option', {
        name: 'ChatGPT UI (experimental) default',
      })
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('option', {
        name: 'ChatGPT web conversation',
      })
    ).not.toBeInTheDocument();

    const agentSelect = chatGptOption.closest('select');
    expect(agentSelect).not.toBeNull();
    fireEvent.change(agentSelect!, { target: { value: 'chatgpt_ui:' } });

    await waitFor(() => {
      expect(agentSelect).toHaveValue('chatgpt_ui:');
    });
    const [workspaceSelect] = screen.getAllByRole('combobox');
    expect(workspaceSelect).toBeDisabled();
    expect(workspaceSelect).toHaveValue('');
    expect(within(workspaceSelect).getAllByRole('option')).toHaveLength(1);
    expect(
      within(workspaceSelect).getByRole('option', {
        name: 'Host artifact storage (required)',
      })
    ).toBeVisible();
    expect(
      screen.queryByText(/workspace\\(s\\) unavailable/i)
    ).not.toBeInTheDocument();
    expect(screen.getByText('ChatGPT Pro')).toBeVisible();
    expect(screen.getByText('gpt-5.6-pro')).toBeVisible();
    expect(
      screen.queryByText('Model override (optional)')
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Create here' }));

    await waitFor(() => {
      expect(onCreate).toHaveBeenCalledWith({
        workspaceId: undefined,
        agent: undefined,
        backend: 'chatgpt_ui',
        modelOverride: 'gpt-5.6-pro',
        modelEffort: undefined,
        configProfile: undefined,
        openInNewTab: false,
      });
    });
  });
});
