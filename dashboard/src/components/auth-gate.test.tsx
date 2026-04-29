import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { AuthGate } from './auth-gate';
import { getHealth, login } from '@/lib/api';

vi.mock('@/lib/api', () => ({
  getHealth: vi.fn(),
  login: vi.fn(),
}));

const mockedGetHealth = vi.mocked(getHealth);
const mockedLogin = vi.mocked(login);
const authRequiredHealth = {
  status: 'ok',
  version: 'test',
  dev_mode: false,
  auth_required: true,
  auth_mode: 'single_tenant' as const,
  max_iterations: 50,
};

describe('AuthGate', () => {
  afterEach(() => {
    cleanup();
  });

  beforeEach(() => {
    localStorage.clear();
    mockedGetHealth.mockReset();
    mockedLogin.mockReset();
  });

  test('does not mount children while auth status is still loading', () => {
    mockedGetHealth.mockReturnValue(new Promise<never>(() => undefined));

    render(
      <AuthGate>
        <div>Dashboard content</div>
      </AuthGate>
    );

    expect(screen.getByLabelText('Checking authentication')).toBeInTheDocument();
    expect(screen.queryByText('Dashboard content')).not.toBeInTheDocument();
  });

  test('shows only the login form when auth is required and no token is stored', async () => {
    mockedGetHealth.mockResolvedValue(authRequiredHealth);

    render(
      <AuthGate>
        <div>Dashboard content</div>
      </AuthGate>
    );

    expect(await screen.findByRole('heading', { name: 'Authenticate' })).toBeVisible();
    expect(screen.queryByText('Dashboard content')).not.toBeInTheDocument();
  });

  test('mounts children after a successful login', async () => {
    mockedGetHealth.mockResolvedValue(authRequiredHealth);
    mockedLogin.mockResolvedValue({
      token: 'jwt-token',
      exp: Math.floor(Date.now() / 1000) + 3600,
    });

    render(
      <AuthGate>
        <div>Dashboard content</div>
      </AuthGate>
    );

    fireEvent.change(await screen.findByPlaceholderText('Password'), {
      target: { value: 'password' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));

    await waitFor(() => expect(screen.getByText('Dashboard content')).toBeVisible());
  });
});
