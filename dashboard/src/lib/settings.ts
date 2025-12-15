export type SavedSettings = Partial<{
  apiUrl: string;
  defaultModel: string;
  defaultBudget: string; // cents, stored as string for form input
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
    if (typeof parsed.defaultModel === 'string') out.defaultModel = parsed.defaultModel;
    if (typeof parsed.defaultBudget === 'string') out.defaultBudget = parsed.defaultBudget;
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
  const envBase = process.env.NEXT_PUBLIC_API_URL || 'http://127.0.0.1:3000';
  if (typeof window === 'undefined') return normalizeBaseUrl(envBase);
  const saved = readSavedSettings().apiUrl;
  return normalizeBaseUrl(saved || envBase);
}

export function getRuntimeTaskDefaults(): { model?: string; budget_cents?: number } {
  if (typeof window === 'undefined') return {};
  const saved = readSavedSettings();
  const out: { model?: string; budget_cents?: number } = {};
  if (saved.defaultModel && saved.defaultModel.trim()) out.model = saved.defaultModel.trim();
  if (saved.defaultBudget && saved.defaultBudget.trim()) {
    const n = Number(saved.defaultBudget);
    if (Number.isFinite(n) && n > 0) out.budget_cents = Math.floor(n);
  }
  return out;
}


