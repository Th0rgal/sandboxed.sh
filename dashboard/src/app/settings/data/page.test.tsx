import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent, cleanup } from '@testing-library/react';
import { SWRConfig } from 'swr';

import DataSettingsPage from './page';
import { getSettings, updateRtkEnabled } from '@/lib/api';

vi.mock('@/components/toast', () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}));

vi.mock('@/lib/api', () => ({
  getSettings: vi.fn(),
  updateLibraryRemote: vi.fn(),
  updateSettings: vi.fn(),
  downloadBackup: vi.fn(),
  restoreBackup: vi.fn(),
  updateRtkEnabled: vi.fn(),
}));

const mockedGetSettings = vi.mocked(getSettings);
const mockedUpdateRtkEnabled = vi.mocked(updateRtkEnabled);

function renderPage() {
  return render(
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      <DataSettingsPage />
    </SWRConfig>
  );
}

describe('DataSettingsPage RTK toggle', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedGetSettings.mockResolvedValue({
      library_remote: null,
      sandboxed_repo_path: null,
      rtk_enabled: false,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it('optimistically updates toggle state and persists on success', async () => {
    mockedUpdateRtkEnabled.mockResolvedValue({ rtk_enabled: true, previous_value: false });
    renderPage();

    const toggle = await screen.findByRole('button', { name: 'Toggle RTK compression' });
    expect(toggle).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(toggle);

    await waitFor(() => {
      expect(toggle).toHaveAttribute('aria-pressed', 'true');
    });
    expect(mockedUpdateRtkEnabled).toHaveBeenCalledWith(true);
  });

  it('rolls back toggle state when API update fails', async () => {
    mockedUpdateRtkEnabled.mockRejectedValue(new Error('network failed'));
    renderPage();

    const toggle = await screen.findByRole('button', { name: 'Toggle RTK compression' });
    expect(toggle).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(toggle);

    await waitFor(() => {
      expect(mockedUpdateRtkEnabled).toHaveBeenCalledWith(true);
    });

    await waitFor(() => {
      expect(toggle).toHaveAttribute('aria-pressed', 'false');
    });
  });
});
