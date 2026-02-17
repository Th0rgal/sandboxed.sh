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

export function getRuntimeApiBase(): string {
  const envBase = process.env.NEXT_PUBLIC_API_URL;
  if (typeof window === 'undefined') {
    return normalizeBaseUrl(envBase || 'http://127.0.0.1:3000');
  }
  const saved = readSavedSettings().apiUrl;
  if (saved) return normalizeBaseUrl(saved);
  if (envBase) return normalizeBaseUrl(envBase);
  return normalizeBaseUrl(window.location.origin);
}
