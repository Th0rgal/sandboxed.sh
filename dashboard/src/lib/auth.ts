const TOKEN_KEY = 'openagent.jwt';
const EXP_KEY = 'openagent.jwt_exp';

export function getStoredJwt(): { token: string; exp: number } | null {
  if (typeof window === 'undefined') return null;
  // Use localStorage for persistence across browser sessions
  const token = localStorage.getItem(TOKEN_KEY);
  const expRaw = localStorage.getItem(EXP_KEY);
  if (!token || !expRaw) return null;
  const exp = Number(expRaw);
  if (!Number.isFinite(exp)) return null;
  return { token, exp };
}

export function isJwtValid(exp: number, skewSeconds = 15): boolean {
  const now = Math.floor(Date.now() / 1000);
  return exp > now + skewSeconds;
}

export function getValidJwt(): { token: string; exp: number } | null {
  const stored = getStoredJwt();
  if (!stored) return null;
  if (!isJwtValid(stored.exp)) {
    clearJwt();
    return null;
  }
  return stored;
}

export function setJwt(token: string, exp: number): void {
  if (typeof window === 'undefined') return;
  // Use localStorage for persistence across browser sessions
  localStorage.setItem(TOKEN_KEY, token);
  localStorage.setItem(EXP_KEY, String(exp));
}

export function clearJwt(): void {
  if (typeof window === 'undefined') return;
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(EXP_KEY);
}

export function authHeader(): Record<string, string> {
  const jwt = getValidJwt();
  if (!jwt) return {};
  return { Authorization: `Bearer ${jwt.token}` };
}

export function signalAuthRequired(): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent('openagent:auth:required'));
}

export function signalAuthSuccess(): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent('openagent:auth:success'));
}
