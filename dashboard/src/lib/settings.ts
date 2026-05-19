export type SavedSettings = Partial<{
  apiUrl: string;
}>;

const STORAGE_KEY = 'settings';

export function readSavedSettings(): SavedSettings {
  if (typeof window === 'undefined') return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: SavedSettings = {};
    if (typeof parsed.apiUrl === 'string') out.apiUrl = parsed.apiUrl;
    return out;
  } catch {
    return {};
  }
}

export function writeSavedSettings(next: SavedSettings): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
}

function normalizeBaseUrl(url: string): string {
  const trimmed = url.trim();
  if (!trimmed) return trimmed;
  return trimmed.endsWith('/') ? trimmed.slice(0, -1) : trimmed;
}

const HOSTED_API_BASE_BY_HOSTNAME: Record<string, string> = {
  'agent.thomas.md': 'https://agent-backend.thomas.md',
};

const LOCAL_BACKEND_PORT = '3000';

export function inferHostedApiBase(hostname: string): string | null {
  return HOSTED_API_BASE_BY_HOSTNAME[hostname] ?? null;
}

export function inferLocalApiBase(location: Location): string | null {
  if (!['localhost', '127.0.0.1', '::1'].includes(location.hostname)) {
    return null;
  }
  if (location.port === LOCAL_BACKEND_PORT) {
    return null;
  }
  const host = location.hostname === '::1' ? '[::1]' : location.hostname;
  return `${location.protocol}//${host}:${LOCAL_BACKEND_PORT}`;
}

export function getRuntimeApiBase(): string {
  const envBase = process.env.NEXT_PUBLIC_API_URL;
  if (typeof window === 'undefined') {
    return normalizeBaseUrl(envBase || 'http://127.0.0.1:3000');
  }
  const saved = readSavedSettings().apiUrl;
  const localBase = inferLocalApiBase(window.location);
  if (saved) {
    const normalizedSaved = normalizeBaseUrl(saved);
    if (localBase && normalizedSaved === normalizeBaseUrl(window.location.origin)) {
      return normalizeBaseUrl(localBase);
    }
    return normalizedSaved;
  }
  if (envBase) return normalizeBaseUrl(envBase);
  const hostedBase = inferHostedApiBase(window.location.hostname);
  if (hostedBase) return normalizeBaseUrl(hostedBase);
  if (localBase) return normalizeBaseUrl(localBase);
  return normalizeBaseUrl(window.location.origin);
}
