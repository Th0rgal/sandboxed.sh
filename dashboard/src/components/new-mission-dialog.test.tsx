import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { SWRConfig } from 'swr';
import { afterEach, describe, expect, it, vi } from 'vitest';

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
  getLibraryOpenCodeSettingsForProfile: vi.fn().mockResolvedValue({ agents: [] }),
  listBackendModelOptions: vi.fn().mockResolvedValue({ backends: {} }),
  listProviders: vi.fn().mockResolvedValue({ providers: [] }),
}));

function renderDialog(onCreate: Parameters<typeof NewMissionDialog>[0]['onCreate']) {
  return render(
    <SWRConfig value={{ provider: () => new Map() }}>
      <NewMissionDialog workspaces={[]} onCreate={onCreate} />
    </SWRConfig>
  );
}

describe('NewMissionDialog', () => {
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
});
