import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { SWRConfig } from 'swr';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import BackendsPage from './page';
import { updateBackendConfig } from '@/lib/api';

const { refreshBackendConfigs, getChatgptUiProfilePoolMock, backendConfigsFixture } = vi.hoisted(() => ({
  refreshBackendConfigs: vi.fn(),
  getChatgptUiProfilePoolMock: vi.fn(),
  backendConfigsFixture: {
    opencode: {
      enabled: true,
      settings: { base_url: 'http://127.0.0.1:4096' },
    },
    claudecode: { enabled: true, settings: {} },
    grok: { enabled: false, settings: {} },
    chatgpt_ui: {
      enabled: false,
      settings: {
        profile_dir: null,
        profile_dirs: [],
        driver_path: null,
        python_path: null,
        proxy_server: null,
        display: null,
        model: null,
        timeout_secs: 900,
        headless: true,
      },
    },
  },
}));

vi.mock('@/components/server-connection-card', () => ({
  ServerConnectionCard: () => <div>Server connection</div>,
}));

vi.mock('@/components/model-routing-debug', () => ({
  ModelRoutingDebug: () => <div>Model routing</div>,
}));

vi.mock('@/components/toast', () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}));

vi.mock('@/lib/api', () => ({
  listBackends: vi.fn().mockResolvedValue([
    { id: 'opencode', name: 'OpenCode' },
    { id: 'codex', name: 'Codex' },
  ]),
  updateBackendConfig: vi.fn().mockResolvedValue({ message: 'saved' }),
  getProviderForBackend: vi.fn().mockResolvedValue({ configured: false }),
  getChatgptUiProfilePool: getChatgptUiProfilePoolMock,
  getHealth: vi.fn().mockResolvedValue({ version: '1.3.0' }),
  getSettings: vi.fn().mockResolvedValue({
    max_parallel_missions: 1,
    max_concurrent_tasks: 5,
  }),
  updateSettings: vi.fn().mockResolvedValue({}),
}));

vi.mock('@/lib/use-backend-configs', () => ({
  useBackendConfigs: () => ({
    configs: backendConfigsFixture,
    refresh: refreshBackendConfigs,
  }),
}));

const mockedUpdateBackendConfig = vi.mocked(updateBackendConfig);

function renderPage() {
  return render(
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      <BackendsPage />
    </SWRConfig>
  );
}

describe('BackendsPage ChatGPT UI settings', () => {
  beforeEach(() => {
    mockedUpdateBackendConfig.mockClear();
    refreshBackendConfigs.mockClear();
    getChatgptUiProfilePoolMock.mockReset();
    getChatgptUiProfilePoolMock.mockResolvedValue({ slots: [] });
  });

  afterEach(() => {
    cleanup();
  });

  it('keeps the ChatGPT UI tab available and fills production defaults', async () => {
    renderPage();

    fireEvent.click(screen.getByRole('button', { name: 'ChatGPT UI (experimental)' }));

    expect(screen.getByText('Dedicated ChatGPT web login')).toBeVisible();
    expect(screen.getByText(/does not reuse the OpenAI or Codex OAuth/i)).toBeVisible();
    expect(screen.getByText('Runtime paths required')).toBeVisible();

    fireEvent.click(screen.getByRole('button', { name: 'Use production defaults' }));
    fireEvent.click(screen.getByRole('checkbox', { name: 'Enable harness' }));

    expect(screen.getByLabelText('Browser profile directory')).toHaveValue(
      '/var/lib/sandboxed-sh/chatgpt-profile'
    );
    expect(screen.getByLabelText('Driver path')).toHaveValue(
      '/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py'
    );
    expect(screen.getByLabelText('Python executable')).toHaveValue(
      '/opt/sandboxed-sh/chatgpt-ui-venv/bin/python'
    );
    expect(screen.getByLabelText('X11 display')).toHaveValue(':93');
    expect(screen.getByLabelText('Browser proxy server')).toHaveValue(
      'socks5://127.0.0.1:10880'
    );
    expect(screen.getByLabelText('Canonical model ID')).toHaveValue('gpt-5.6-pro');
    expect(screen.getByText('Runtime paths configured')).toBeVisible();

    fireEvent.click(screen.getByRole('button', { name: 'Save ChatGPT UI' }));

    await waitFor(() => {
      expect(mockedUpdateBackendConfig).toHaveBeenCalledWith(
        'chatgpt_ui',
        expect.objectContaining({
          profile_dir: '/var/lib/sandboxed-sh/chatgpt-profile',
          profile_dirs: [],
          driver_path: '/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py',
          python_path: '/opt/sandboxed-sh/chatgpt-ui-venv/bin/python',
          proxy_server: 'socks5://127.0.0.1:10880',
          display: ':93',
          model: 'gpt-5.6-pro',
          browser: 'chromium',
          headless: false,
          timeout_secs: 14400,
        }),
        { enabled: true }
      );
    });
    expect(refreshBackendConfigs).toHaveBeenCalled();
  });

  it('shows profile pool slot health once runtime paths are configured', async () => {
    getChatgptUiProfilePoolMock.mockResolvedValue({
      slots: [
        {
          slot: 1,
          profile_name: 'chatgpt-profile',
          state: 'in_use',
          consecutive_failures: 0,
        },
        {
          slot: 2,
          profile_name: 'chatgpt-profile-2',
          state: 'quarantined',
          consecutive_failures: 1,
          quarantine_remaining_secs: 1700,
          last_failure: 'auth',
        },
      ],
    });

    renderPage();

    fireEvent.click(screen.getByRole('button', { name: 'ChatGPT UI (experimental)' }));

    expect(screen.queryByTestId('chatgpt-ui-pool-status')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Use production defaults' }));

    await waitFor(() => {
      expect(screen.getByTestId('chatgpt-ui-pool-status')).toBeVisible();
    });
    expect(screen.getByText('chatgpt-profile')).toBeVisible();
    expect(screen.getByText('in use')).toBeVisible();
    expect(screen.getByText('chatgpt-profile-2')).toBeVisible();
    expect(screen.getByText('quarantined')).toBeVisible();
    expect(screen.getByText(/auth failure/)).toBeVisible();
    expect(screen.getByText(/retry in 29 min/)).toBeVisible();
  });
});
